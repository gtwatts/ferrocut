//! Typed, agent-facing timeline edit operations.
//!
//! An edit script is a JSON list of [`EditOp`]s applied in order to a
//! [`Timeline`] (`ferrocut edit <timeline.json> <ops.json> -o <new.json>`).
//! Each op is checked as it is applied: its own preconditions (the clip
//! exists, the time is inside the clip, the neighbor exists...), source-media
//! bounds (no slipping or trimming past either end of the media; J/L offsets
//! need source handles), and then the whole timeline is re-validated, so a
//! failing op reports `op N (kind clip): reason` and nothing is written.
//!
//! Semantics (NLE conventions; times are exact rationals in seconds):
//! - `split`: cut a clip at timeline time `at` into two clips (`new_id` for
//!   the right part, default `<id>.2`); the left keeps the incoming
//!   transition, audio in-offset and fade-in, the right keeps the out-offset
//!   and fade-out.
//! - `trim` (`edge` `in`/`out`, `delta`): move one edge; positive `delta`
//!   moves it later. Trimming the in-edge moves `start` and `source_in`
//!   together (content stays in place on the timeline); neighbors don't move.
//! - `ripple_delete`: remove a clip and pull later clips on its track left to
//!   close the space it occupied (up to where the next clip began; a dissolve
//!   from the deleted clip into the next is dropped). `all_tracks` also
//!   pulls the other tracks' clips that start after it (sync lock).
//! - `ripple_insert`: insert a clip at `at`, pushing clips that start at or
//!   after `at` right by its duration (`all_tracks`: on every track). `at`
//!   must not be inside a clip (split first).
//! - `roll`: move the edit point between `clip` and the next clip by `delta`
//!   (the left clip's out and the right clip's in move together); the
//!   track's total duration is unchanged.
//! - `slip`: shift the clip's source window by `delta` (`source_in += delta`);
//!   position and duration are unchanged.
//! - `slide`: move the clip by `delta` between its neighbors: the previous
//!   clip's out and the next clip's in follow, so the track duration is
//!   unchanged.
//! - `move`: place the clip at `to` (optionally on another track of the same
//!   kind); it must not overlap.
//! - `jl_cut`: set the clip's audio `in_offset` / `out_offset` (J-cut:
//!   negative in-offset; L-cut: positive out-offset).
//!
//! Clip-local keyframes (audio gain/pan, opacity, transform) keep their
//! timeline position when an op moves a clip's start without moving its
//! content (trim-in, roll, slide's right neighbor, split's right part).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail, ensure};
use ferrocut_core::{Rational, RationalTime};
use serde::{Deserialize, Serialize};

use crate::timeline::{AudioClip, Clip, ClipAudio, Timeline};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    In,
    Out,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditOp {
    Split {
        clip: String,
        at: RationalTime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_id: Option<String>,
    },
    Trim {
        clip: String,
        edge: Edge,
        delta: RationalTime,
    },
    RippleDelete {
        clip: String,
        #[serde(default)]
        all_tracks: bool,
    },
    RippleInsert {
        track: String,
        at: RationalTime,
        /// A clip object for the track's kind (video `Clip` or `AudioClip`);
        /// its `start` is set to `at`.
        clip: serde_json::Value,
        #[serde(default)]
        all_tracks: bool,
    },
    Roll {
        clip: String,
        delta: RationalTime,
    },
    Slip {
        clip: String,
        delta: RationalTime,
    },
    Slide {
        clip: String,
        delta: RationalTime,
    },
    Move {
        clip: String,
        to: RationalTime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
    },
    JlCut {
        clip: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_offset: Option<RationalTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        out_offset: Option<RationalTime>,
    },
}

impl EditOp {
    pub fn kind(&self) -> &'static str {
        match self {
            EditOp::Split { .. } => "split",
            EditOp::Trim { .. } => "trim",
            EditOp::RippleDelete { .. } => "ripple_delete",
            EditOp::RippleInsert { .. } => "ripple_insert",
            EditOp::Roll { .. } => "roll",
            EditOp::Slip { .. } => "slip",
            EditOp::Slide { .. } => "slide",
            EditOp::Move { .. } => "move",
            EditOp::JlCut { .. } => "jl_cut",
        }
    }
    fn target(&self) -> String {
        match self {
            EditOp::RippleInsert { track, .. } => format!("track {track:?}"),
            EditOp::Split { clip, .. }
            | EditOp::Trim { clip, .. }
            | EditOp::RippleDelete { clip, .. }
            | EditOp::Roll { clip, .. }
            | EditOp::Slip { clip, .. }
            | EditOp::Slide { clip, .. }
            | EditOp::Move { clip, .. }
            | EditOp::JlCut { clip, .. } => clip.clone(),
        }
    }
}

/// What one op did: a summary and the timeline span whose output may change.
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub op: usize,
    pub kind: &'static str,
    pub summary: String,
    /// `[start, end)` of the affected timeline span (video and audio).
    pub span: (RationalTime, RationalTime),
}

/// Common view of video and audio clips for the edit algorithms.
pub trait Item: Clone {
    fn id(&self) -> &str;
    fn id_mut(&mut self) -> &mut String;
    fn start_mut(&mut self) -> &mut RationalTime;
    fn source_in_mut(&mut self) -> &mut RationalTime;
    fn duration_mut(&mut self) -> &mut RationalTime;
    fn start(&self) -> RationalTime;
    fn source_in(&self) -> RationalTime;
    fn duration(&self) -> RationalTime;
    fn source(&self) -> &Path;
    fn audio(&self) -> &ClipAudio;
    fn audio_mut(&mut self) -> &mut ClipAudio;
    /// Shift clip-local keyframe times by `dt`.
    fn shift_local_keys(&mut self, dt: Rational);
    /// Remove the incoming transition (video dissolve, audio crossfade).
    fn drop_transition_in(&mut self);
    fn end(&self) -> RationalTime {
        self.start() + self.duration()
    }
}

macro_rules! item_common {
    () => {
        fn id(&self) -> &str {
            &self.id
        }
        fn id_mut(&mut self) -> &mut String {
            &mut self.id
        }
        fn start_mut(&mut self) -> &mut RationalTime {
            &mut self.start
        }
        fn source_in_mut(&mut self) -> &mut RationalTime {
            &mut self.source_in
        }
        fn duration_mut(&mut self) -> &mut RationalTime {
            &mut self.duration
        }
        fn start(&self) -> RationalTime {
            self.start
        }
        fn source_in(&self) -> RationalTime {
            self.source_in
        }
        fn duration(&self) -> RationalTime {
            self.duration
        }
        fn source(&self) -> &Path {
            &self.source
        }
        fn audio(&self) -> &ClipAudio {
            &self.audio
        }
        fn audio_mut(&mut self) -> &mut ClipAudio {
            &mut self.audio
        }
    };
}

impl Item for Clip {
    item_common!();
    fn shift_local_keys(&mut self, dt: Rational) {
        self.audio.gain_db = self.audio.gain_db.shifted(dt);
        self.audio.pan = self.audio.pan.shifted(dt);
        self.shift_video_keys(dt);
    }
    fn drop_transition_in(&mut self) {
        self.transition_in = None;
        self.audio.crossfade_in = None;
    }
}

impl Item for AudioClip {
    item_common!();
    fn shift_local_keys(&mut self, dt: Rational) {
        self.audio.gain_db = self.audio.gain_db.shifted(dt);
        self.audio.pan = self.audio.pan.shifted(dt);
    }
    fn drop_transition_in(&mut self) {
        self.audio.crossfade_in = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackRef {
    Video(usize),
    Audio(usize),
}

/// Run `$body` with `$clips: &mut Vec<impl Item>` for track `$tr`.
macro_rules! on_track {
    ($tl:expr, $tr:expr, |$clips:ident| $body:expr) => {
        match $tr {
            TrackRef::Video(i) => {
                let $clips = &mut $tl.tracks[i].clips;
                $body
            }
            TrackRef::Audio(i) => {
                let $clips = &mut $tl.audio_tracks[i].clips;
                $body
            }
        }
    };
}

fn locate(tl: &Timeline, id: &str) -> anyhow::Result<(TrackRef, usize)> {
    for (ti, t) in tl.tracks.iter().enumerate() {
        if let Some(ci) = t.clips.iter().position(|c| c.id == id) {
            return Ok((TrackRef::Video(ti), ci));
        }
    }
    for (ti, t) in tl.audio_tracks.iter().enumerate() {
        if let Some(ci) = t.clips.iter().position(|c| c.id == id) {
            return Ok((TrackRef::Audio(ti), ci));
        }
    }
    bail!("no clip with id {id:?}")
}

fn track_by_name(tl: &Timeline, name: &str) -> anyhow::Result<TrackRef> {
    if let Some(i) = tl.tracks.iter().position(|t| t.name == name) {
        return Ok(TrackRef::Video(i));
    }
    if let Some(i) = tl.audio_tracks.iter().position(|t| t.name == name) {
        return Ok(TrackRef::Audio(i));
    }
    bail!("no track named {name:?}")
}

fn all_tracks(tl: &Timeline) -> Vec<TrackRef> {
    (0..tl.tracks.len())
        .map(TrackRef::Video)
        .chain((0..tl.audio_tracks.len()).map(TrackRef::Audio))
        .collect()
}

/// Sort a track by start (stable) and return the new index of clip `id`.
fn sort_track<T: Item>(clips: &mut [T], id: &str) -> usize {
    clips.sort_by_key(|c| c.start());
    clips
        .iter()
        .position(|c| c.id() == id)
        .expect("clip present")
}

fn z() -> RationalTime {
    RationalTime::ZERO
}

fn unique_id(tl: &Timeline, base: &str) -> String {
    let taken: std::collections::HashSet<&str> = tl.clip_ids().collect();
    let mut k = 2;
    loop {
        let id = format!("{base}.{k}");
        if !taken.contains(id.as_str()) {
            return id;
        }
        k += 1;
    }
}

fn split<T: Item>(
    clips: &mut Vec<T>,
    ci: usize,
    at: RationalTime,
    new_id: String,
) -> anyhow::Result<Change> {
    let c = clips[ci].clone();
    ensure!(
        at > c.start() && at < c.end(),
        "split time {at} is not strictly inside clip {} ({}..{})",
        c.id(),
        c.start(),
        c.end()
    );
    let off = at - c.start();
    let mut left = c.clone();
    *left.duration_mut() = off;
    left.audio_mut().out_offset = z();
    left.audio_mut().fade_out = None;
    let mut right = c.clone();
    *right.id_mut() = new_id.clone();
    *right.start_mut() = at;
    *right.source_in_mut() = c.source_in() + off;
    *right.duration_mut() = c.end() - at;
    right.drop_transition_in();
    right.audio_mut().in_offset = z();
    right.audio_mut().fade_in = None;
    right.shift_local_keys(-off.0);
    clips[ci] = left;
    clips.insert(ci + 1, right);
    Ok(Change {
        op: 0,
        kind: "split",
        summary: format!("split {} at {at}: {} + {new_id}", c.id(), c.id()),
        span: (at, at),
    })
}

fn trim<T: Item>(
    clips: &mut [T],
    ci: usize,
    edge: Edge,
    d: RationalTime,
) -> anyhow::Result<Change> {
    let c = &mut clips[ci];
    let (s0, e0) = (c.start(), c.end());
    match edge {
        Edge::In => {
            ensure!(
                d < c.duration(),
                "trim in by {d} would leave clip {} with no duration ({})",
                c.id(),
                c.duration()
            );
            *c.start_mut() = c.start() + d;
            *c.source_in_mut() = c.source_in() + d;
            *c.duration_mut() = c.duration() - d;
            c.shift_local_keys(-d.0);
        }
        Edge::Out => {
            ensure!(
                c.duration() + d > z(),
                "trim out by {d} would leave clip {} with no duration ({})",
                c.id(),
                c.duration()
            );
            *c.duration_mut() = c.duration() + d;
        }
    }
    let span = match edge {
        Edge::In => (s0.min(c.start()), s0.max(c.start())),
        Edge::Out => (e0.min(c.end()), e0.max(c.end())),
    };
    Ok(Change {
        op: 0,
        kind: "trim",
        summary: format!(
            "trim {} {edge:?} by {d}: now {}..{}",
            c.id(),
            c.start(),
            c.end()
        ),
        span,
    })
}

fn slip<T: Item>(clips: &mut [T], ci: usize, d: RationalTime) -> anyhow::Result<Change> {
    let c = &mut clips[ci];
    *c.source_in_mut() = c.source_in() + d;
    let a = c.audio().clone();
    let span = (
        c.start() + a.in_offset.min(z()),
        c.end() + a.out_offset.max(z()),
    );
    Ok(Change {
        op: 0,
        kind: "slip",
        summary: format!("slip {} by {d}: source_in {}", c.id(), c.source_in()),
        span,
    })
}

fn roll<T: Item>(clips: &mut [T], ci: usize, d: RationalTime) -> anyhow::Result<Change> {
    ensure!(
        ci + 1 < clips.len(),
        "roll needs a clip after {} on its track",
        clips[ci].id()
    );
    let (a, b) = (clips[ci].clone(), clips[ci + 1].clone());
    ensure!(
        b.start() <= a.end(),
        "roll needs {} to meet the next clip {} (it ends at {}, {} starts at {}): there is a gap",
        a.id(),
        b.id(),
        a.end(),
        b.id(),
        b.start()
    );
    ensure!(
        a.duration() + d > z(),
        "roll by {d} would leave {} with no duration",
        a.id()
    );
    ensure!(
        b.duration() - d > z(),
        "roll by {d} would leave {} with no duration",
        b.id()
    );
    let old = b.start();
    *clips[ci].duration_mut() = a.duration() + d;
    let r = &mut clips[ci + 1];
    *r.start_mut() = b.start() + d;
    *r.source_in_mut() = b.source_in() + d;
    *r.duration_mut() = b.duration() - d;
    r.shift_local_keys(-d.0);
    let new = old + d;
    Ok(Change {
        op: 0,
        kind: "roll",
        summary: format!(
            "roll {}|{} by {d}: edit point {old} -> {new}",
            a.id(),
            b.id()
        ),
        span: (old.min(new), old.max(new) + (a.end() - b.start())),
    })
}

fn slide<T: Item>(clips: &mut [T], ci: usize, d: RationalTime) -> anyhow::Result<Change> {
    let c = clips[ci].clone();
    let touches_prev = ci > 0 && clips[ci - 1].end() >= c.start();
    let touches_next = ci + 1 < clips.len() && clips[ci + 1].start() <= c.end();
    if touches_prev {
        let p = &mut clips[ci - 1];
        ensure!(
            p.duration() + d > z(),
            "slide by {d} would leave {} with no duration",
            p.id()
        );
        *p.duration_mut() = p.duration() + d;
    }
    if touches_next {
        let n = &mut clips[ci + 1];
        ensure!(
            n.duration() - d > z(),
            "slide by {d} would leave {} with no duration",
            n.id()
        );
        *n.start_mut() = n.start() + d;
        *n.source_in_mut() = n.source_in() + d;
        *n.duration_mut() = n.duration() - d;
        n.shift_local_keys(-d.0);
    }
    *clips[ci].start_mut() = c.start() + d;
    let lo = c.start().min(c.start() + d);
    let hi = c.end().max(c.end() + d);
    Ok(Change {
        op: 0,
        kind: "slide",
        summary: format!(
            "slide {} by {d}: now {}..{}",
            c.id(),
            c.start() + d,
            c.end() + d
        ),
        span: (lo, hi),
    })
}

/// Close the space clip `ci` occupied. Returns (removed clip span, shift).
fn ripple_delete<T: Item>(
    clips: &mut Vec<T>,
    ci: usize,
) -> (RationalTime, RationalTime, RationalTime) {
    let c = clips.remove(ci);
    // Shift: up to where the next clip begins (it may overlap via a dissolve).
    let shift = match clips.get(ci) {
        Some(n) if n.start() < c.end() => n.start() - c.start(),
        _ => c.duration(),
    };
    if let Some(n) = clips.get_mut(ci)
        && n.start() < c.end()
    {
        n.drop_transition_in();
    }
    for n in clips.iter_mut().skip(ci) {
        *n.start_mut() = n.start() - shift;
    }
    (c.start(), c.end(), shift)
}

fn shift_after<T: Item>(clips: &mut [T], at: RationalTime, by: RationalTime, strictly: bool) {
    for c in clips.iter_mut() {
        if c.start() > at || (!strictly && c.start() == at) {
            *c.start_mut() = c.start() + by;
        }
    }
}

/// Clip `id`'s source window must stay inside its media.
fn check_bounds<T: Item>(c: &T, media_len: Option<RationalTime>) -> anyhow::Result<()> {
    ensure!(
        c.source_in() >= z(),
        "clip {}: source_in would be {} (before the start of {})",
        c.id(),
        c.source_in(),
        c.source().display()
    );
    let a = c.audio();
    ensure!(
        c.source_in() + a.in_offset >= z(),
        "clip {}: audio would start {} before the start of {} (in_offset {} with source_in {})",
        c.id(),
        z() - (c.source_in() + a.in_offset),
        c.source().display(),
        a.in_offset,
        c.source_in()
    );
    if let Some(len) = media_len {
        let need = c.source_in() + c.duration();
        ensure!(
            need <= len,
            "clip {}: needs source up to {} but {} is {} long (max source_in for this duration: {})",
            c.id(),
            need,
            c.source().display(),
            len,
            len - c.duration()
        );
        let aneed = need + a.out_offset;
        ensure!(
            a.out_offset <= z() || aneed <= len,
            "clip {}: audio out_offset {} needs source up to {} but {} is {} long",
            c.id(),
            a.out_offset,
            aneed,
            c.source().display(),
            len
        );
    }
    Ok(())
}

type Probe<'a> = Box<dyn FnMut(&Path) -> Option<RationalTime> + 'a>;

/// Media length lookup with a cache; `probe` returns `None` if unknown.
pub struct MediaLengths<'a> {
    probe: Probe<'a>,
    cache: HashMap<PathBuf, Option<RationalTime>>,
    base: PathBuf,
}

impl<'a> MediaLengths<'a> {
    /// `base`: directory that relative clip sources resolve against.
    pub fn new(
        base: impl Into<PathBuf>,
        probe: impl FnMut(&Path) -> Option<RationalTime> + 'a,
    ) -> Self {
        MediaLengths {
            probe: Box::new(probe),
            cache: HashMap::new(),
            base: base.into(),
        }
    }
    /// No media bounds (unknown lengths).
    pub fn unbounded() -> MediaLengths<'static> {
        MediaLengths::new(".", |_| None)
    }
    fn get(&mut self, p: &Path) -> Option<RationalTime> {
        let full = if p.is_relative() {
            self.base.join(p)
        } else {
            p.to_path_buf()
        };
        if let Some(v) = self.cache.get(&full) {
            return *v;
        }
        let v = (self.probe)(&full);
        self.cache.insert(full, v);
        v
    }
}

fn check_track_bounds(
    tl: &Timeline,
    tr: TrackRef,
    media: &mut MediaLengths<'_>,
) -> anyhow::Result<()> {
    match tr {
        TrackRef::Video(i) => tl.tracks[i]
            .clips
            .iter()
            .try_for_each(|c| check_bounds(c, media.get(&c.source))),
        TrackRef::Audio(i) => tl.audio_tracks[i]
            .clips
            .iter()
            .try_for_each(|c| check_bounds(c, media.get(&c.source))),
    }
}

/// Apply one op (without re-validating the whole timeline).
fn apply_one(tl: &mut Timeline, op: &EditOp) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    let sorted = |tl: &mut Timeline, tr: TrackRef, id: &str| {
        on_track!(tl, tr, |clips| sort_track(clips, id))
    };
    Ok(match op {
        EditOp::Split { clip, at, new_id } => {
            let (tr, _) = locate(tl, clip)?;
            let ci = sorted(tl, tr, clip);
            let new_id = match new_id {
                Some(n) => {
                    ensure!(
                        !tl.clip_ids().any(|i| i == n),
                        "new_id {n:?} is already used"
                    );
                    n.clone()
                }
                None => unique_id(tl, clip),
            };
            (
                on_track!(tl, tr, |clips| split(clips, ci, *at, new_id.clone()))?,
                vec![tr],
            )
        }
        EditOp::Trim { clip, edge, delta } => {
            let (tr, ci) = locate(tl, clip)?;
            (
                on_track!(tl, tr, |clips| trim(clips, ci, *edge, *delta))?,
                vec![tr],
            )
        }
        EditOp::Slip { clip, delta } => {
            let (tr, ci) = locate(tl, clip)?;
            (
                on_track!(tl, tr, |clips| slip(clips, ci, *delta))?,
                vec![tr],
            )
        }
        EditOp::Roll { clip, delta } => {
            let (tr, _) = locate(tl, clip)?;
            let ci = sorted(tl, tr, clip);
            (
                on_track!(tl, tr, |clips| roll(clips, ci, *delta))?,
                vec![tr],
            )
        }
        EditOp::Slide { clip, delta } => {
            let (tr, _) = locate(tl, clip)?;
            let ci = sorted(tl, tr, clip);
            (
                on_track!(tl, tr, |clips| slide(clips, ci, *delta))?,
                vec![tr],
            )
        }
        EditOp::Move { clip, to, track } => {
            let (tr, ci) = locate(tl, clip)?;
            let dst = match track {
                Some(name) => track_by_name(tl, name)?,
                None => tr,
            };
            let old = on_track!(tl, tr, |clips| {
                let c = &mut clips[ci];
                let old = (c.start(), c.end());
                *c.start_mut() = *to;
                old
            });
            let dur = old.1 - old.0;
            let span_new = (*to, *to + dur);
            match (tr, dst) {
                (a, b) if a == b => {}
                (TrackRef::Video(a), TrackRef::Video(b)) => {
                    let c = tl.tracks[a].clips.remove(ci);
                    tl.tracks[b].clips.push(c);
                }
                (TrackRef::Audio(a), TrackRef::Audio(b)) => {
                    let c = tl.audio_tracks[a].clips.remove(ci);
                    tl.audio_tracks[b].clips.push(c);
                }
                _ => bail!(
                    "move: clip {clip:?} can only move to a track of the same kind (video/audio)"
                ),
            }
            let change = Change {
                op: 0,
                kind: "move",
                summary: format!(
                    "move {clip} {}..{} -> {}..{}",
                    old.0, old.1, span_new.0, span_new.1
                ),
                span: (old.0.min(span_new.0), old.1.max(span_new.1)),
            };
            (change, vec![tr, dst])
        }
        EditOp::JlCut {
            clip,
            in_offset,
            out_offset,
        } => {
            ensure!(
                in_offset.is_some() || out_offset.is_some(),
                "jl_cut needs in_offset and/or out_offset"
            );
            let (tr, ci) = locate(tl, clip)?;
            let change = on_track!(tl, tr, |clips| {
                let c = &mut clips[ci];
                let before = (
                    c.start() + c.audio().in_offset,
                    c.end() + c.audio().out_offset,
                );
                if let Some(v) = in_offset {
                    c.audio_mut().in_offset = *v;
                }
                if let Some(v) = out_offset {
                    c.audio_mut().out_offset = *v;
                }
                let after = (
                    c.start() + c.audio().in_offset,
                    c.end() + c.audio().out_offset,
                );
                Change {
                    op: 0,
                    kind: "jl_cut",
                    summary: format!(
                        "jl_cut {}: audio {}..{} -> {}..{} (in_offset {}, out_offset {})",
                        c.id(),
                        before.0,
                        before.1,
                        after.0,
                        after.1,
                        c.audio().in_offset,
                        c.audio().out_offset
                    ),
                    span: (before.0.min(after.0), before.1.max(after.1)),
                }
            });
            (change, vec![tr])
        }
        EditOp::RippleDelete {
            clip,
            all_tracks: all,
        } => {
            let (tr, _) = locate(tl, clip)?;
            let ci = sorted(tl, tr, clip);
            let (s, e, shift) = on_track!(tl, tr, |clips| ripple_delete(clips, ci));
            let mut touched = vec![tr];
            if *all {
                for other in all_tracks(tl).into_iter().filter(|t| *t != tr) {
                    on_track!(tl, other, |clips| {
                        ensure!(
                            !clips
                                .iter()
                                .any(|c| c.start() < e && c.end() > s && c.start() < s + shift),
                            "ripple_delete {clip} on all tracks: another track has a clip spanning the removed range {s}..{e}"
                        );
                        shift_after(clips, s, -shift, false);
                        Ok::<_, anyhow::Error>(())
                    })?;
                    touched.push(other);
                }
            }
            let end = tl.duration();
            let change = Change {
                op: 0,
                kind: "ripple_delete",
                summary: format!(
                    "ripple_delete {clip} ({s}..{e}): later clips moved {shift} earlier"
                ),
                span: (s, end.max(e)),
            };
            (change, touched)
        }
        EditOp::RippleInsert {
            track,
            at,
            clip,
            all_tracks: all,
        } => {
            let tr = track_by_name(tl, track)?;
            let mut v = clip.clone();
            let obj = v
                .as_object_mut()
                .ok_or_else(|| anyhow!("ripple_insert: clip must be a JSON object"))?;
            obj.insert("start".into(), serde_json::Value::String(at.0.to_string()));
            let targets = if *all { all_tracks(tl) } else { vec![tr] };
            for t in &targets {
                on_track!(tl, *t, |clips| {
                    if let Some(c) = clips.iter().find(|c| c.start() < *at && c.end() > *at) {
                        bail!(
                            "ripple_insert at {at}: inside clip {} ({}..{}); split it first",
                            c.id(),
                            c.start(),
                            c.end()
                        );
                    }
                    Ok::<(), anyhow::Error>(())
                })?;
            }
            let (id, dur) = match tr {
                TrackRef::Video(i) => {
                    let c: Clip =
                        serde_json::from_value(v).context("ripple_insert: bad video clip")?;
                    let out = (c.id.clone(), c.duration);
                    for t in &targets {
                        on_track!(tl, *t, |clips| shift_after(clips, *at, c.duration, false));
                    }
                    tl.tracks[i].clips.push(c);
                    out
                }
                TrackRef::Audio(i) => {
                    let c: AudioClip =
                        serde_json::from_value(v).context("ripple_insert: bad audio clip")?;
                    let out = (c.id.clone(), c.duration);
                    for t in &targets {
                        on_track!(tl, *t, |clips| shift_after(clips, *at, c.duration, false));
                    }
                    tl.audio_tracks[i].clips.push(c);
                    out
                }
            };
            ensure!(
                tl.clip_ids().filter(|i| *i == id).count() == 1,
                "ripple_insert: clip id {id:?} is already used"
            );
            let change = Change {
                op: 0,
                kind: "ripple_insert",
                summary: format!(
                    "ripple_insert {id} at {at} on {track:?}: later clips moved {dur} later"
                ),
                span: (*at, tl.duration()),
            };
            (change, targets)
        }
    })
}

/// Apply `ops` in order. On error nothing is returned (the input is untouched).
pub fn apply(
    tl: &Timeline,
    ops: &[EditOp],
    media: &mut MediaLengths<'_>,
) -> anyhow::Result<(Timeline, Vec<Change>)> {
    let mut out = tl.clone();
    let mut changes = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let ctx = || format!("op {i} ({} {})", op.kind(), op.target());
        let (mut ch, touched) = apply_one(&mut out, op).with_context(ctx)?;
        for tr in touched {
            on_track!(out, tr, |clips| clips.sort_by_key(|c| c.start()));
            check_track_bounds(&out, tr, media).with_context(ctx)?;
        }
        out.validate().with_context(ctx)?;
        ch.op = i;
        changes.push(ch);
    }
    Ok((out, changes))
}

/// Parse an edit script: a JSON array of ops (or `{"ops": [...]}`).
pub fn parse_ops(text: &str) -> anyhow::Result<Vec<EditOp>> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Script {
        List(Vec<serde_json::Value>),
        Obj { ops: Vec<serde_json::Value> },
    }
    let raw = match serde_json::from_str::<Script>(text)
        .context("edit script must be a JSON array of ops")?
    {
        Script::List(v) | Script::Obj { ops: v } => v,
    };
    raw.into_iter()
        .enumerate()
        .map(|(i, v)| serde_json::from_value(v).with_context(|| format!("op {i}: invalid op")))
        .collect()
}
