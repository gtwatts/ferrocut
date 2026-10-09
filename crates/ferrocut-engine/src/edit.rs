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
//! - `set_speed` (Premiere "Speed/Duration"): constant speed (`2` = twice as
//!   fast, `-1` = reverse) keeping the clip's source range, so the duration
//!   becomes `range / |speed|`; `ripple` moves later clips on the track by
//!   the change (otherwise a longer clip must fit before the next one).
//!   `preserve_pitch` sets the audio's pitch preservation. Keyframed ramps
//!   and AE time-remap curves are `set_keyframes` on `speed` / `time_remap`.
//! - `freeze_frame`: hold the frame shown at timeline time `at`. Without
//!   `duration` the clip is split at `at` and the rest becomes the hold
//!   (Premiere "Add Frame Hold"); with `duration` a hold segment of that
//!   length is inserted at `at` and the rest of the clip and later clips on
//!   the track move right (Premiere "Insert Frame Hold Segment";
//!   `all_tracks` moves every track). The hold's audio is muted.
//!
//! Retimed clips (non-default `speed` / `time_remap`): split, trim-in, roll
//! and slide advance `source_in` along the clip's time map, so content stays
//! in place; slip also shifts a time-remap curve's values.
//!
//! Building ops (so agents never hand-edit the JSON):
//! - `add_track`: a new empty video track (`index` 0 = bottom; default on
//!   top) or audio track (default last).
//! - `add_clip`: a clip from a media file on a track. The file is probed: it
//!   must have the stream the track needs (video/audio); `duration` defaults
//!   to the rest of the media after `source_in`, `start` to the end of the
//!   track, `id` to the file stem (made unique). The range must be free.
//! - `add_transition`: a dissolve into `clip` from the clip before it.
//!   Clips that meet at a cut get the overlap from source handles, `align`ed
//!   `center` (default: half before, half after the cut), `start` (the
//!   dissolve starts at the cut: the previous clip runs longer) or `end` (it
//!   ends at the cut: `clip` starts earlier); nothing else moves. Clips that
//!   already overlap by at least `duration` just get the transition. With
//!   zero audio offsets the linked audio gets a matching crossfade.
//! - `set_param`: set any parameter of a clip, a track (its audio bus) or
//!   the timeline by name (see [`crate::params`]); `null` removes an
//!   optional object.
//! - `set_keyframes`: keyframe an animatable parameter (`replace`, or
//!   `merge` with the existing keys); `timeline_time` takes key times in
//!   timeline seconds and converts them to the parameter's time base.
//!
//! Clip-local keyframes (audio gain/pan, opacity, transform) keep their
//! timeline position when an op moves a clip's start without moving its
//! content (trim-in, roll, slide's right neighbor, split's right part).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail, ensure};
use ferrocut_core::{Animatable, Rational, RationalTime};
use serde::{Deserialize, Serialize};

use crate::markers::{ClipPlacement, Marker, MarkerColor};
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
    AddTrack {
        kind: TrackKind,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    AddClip {
        track: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fit: Option<crate::placement::Fit>,
        /// Media file or comp (omit for a generator clip).
        #[serde(default, skip_serializing_if = "path_is_empty")]
        source: PathBuf,
        /// A generator layer instead of media (`{"type": "solid", "color":
        /// [r, g, b]}`, `linear_gradient`, `radial_gradient`; video tracks,
        /// `duration` required). See [`crate::generator`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        generator: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<RationalTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_in: Option<RationalTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<RationalTime>,
        /// An adjustment layer (no source or generator; `duration` required;
        /// its track must hold only adjustment clips). See [`crate::fx`].
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        adjustment: bool,
    },
    AddTransition {
        clip: String,
        duration: RationalTime,
        #[serde(default)]
        kind: TransitionKind,
        #[serde(default)]
        align: Align,
    },
    SetParam {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
        param: String,
        value: serde_json::Value,
    },
    SetSpeed {
        clip: String,
        speed: Rational,
        #[serde(default)]
        ripple: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preserve_pitch: Option<bool>,
    },
    FreezeFrame {
        clip: String,
        at: RationalTime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<RationalTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_id: Option<String>,
        #[serde(default)]
        all_tracks: bool,
    },
    Nest {
        clips: Vec<String>,
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Unnest {
        clip: String,
    },
    /// Insert an audio effect (`{"type": ..., params}`) on a clip or track bus.
    AddEffect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
        effect: serde_json::Value,
        /// Position in the chain (default: the end).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Set one parameter of effect `index` (`threshold_db`, `bands.1.gain_db`);
    /// `value` is a rational, `{"keyframes": [...]}`, or null for the default.
    SetEffectParam {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
        index: usize,
        param: String,
        value: serde_json::Value,
    },
    RemoveEffect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
        index: usize,
    },
    /// Insert a video effect (`{"type": ..., "id"?, "enabled"?, params}`,
    /// see [`crate::fx`]) on a clip (incl. adjustment layers) or video track.
    AddVideoEffect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
        effect: serde_json::Value,
        /// Position in the stack (default: the end, i.e. applied last).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Set one parameter of a video effect (by index or id): a parameter
    /// name (`sigma`, `color.g`, `position.x`), `enabled` or `id`; `value` is
    /// a constant, `{"keyframes": [...]}`, an expression, or null for the default.
    SetVideoEffectParam {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
        effect: crate::fx::EffectRef,
        param: String,
        value: serde_json::Value,
    },
    RemoveVideoEffect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
        effect: crate::fx::EffectRef,
    },
    /// Reorder: move a video effect to position `to` in its stack.
    MoveVideoEffect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
        effect: crate::fx::EffectRef,
        to: usize,
    },
    SetKeyframes {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
        param: String,
        keyframes: Vec<serde_json::Value>,
        #[serde(default)]
        mode: KeyMode,
        #[serde(default)]
        timeline_time: bool,
    },
    /// A timeline marker, or a clip marker with `clip` (`time` in source
    /// seconds, or timeline seconds with `timeline_time`). See
    /// [`crate::markers`].
    AddMarker {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        time: RationalTime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<RationalTime>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(default, skip_serializing_if = "MarkerColor::is_default")]
        color: MarkerColor,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        comment: String,
        #[serde(default)]
        timeline_time: bool,
    },
    /// Change the given fields of marker `id`.
    UpdateMarker {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time: Option<RationalTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<RationalTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<MarkerColor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment: Option<String>,
        #[serde(default)]
        timeline_time: bool,
    },
    RemoveMarker {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        id: String,
    },
    /// Point clips at moved / offline media: `clip` + `to` (every clip using
    /// that clip's source), `from` + `to` (a file, or a directory prefix), or
    /// `search` (offline sources found by file name under a directory;
    /// with `clip`, only its source).
    Relink {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        search: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    #[default]
    Dissolve,
}

/// Where a new dissolve sits relative to the cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Center,
    Start,
    End,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyMode {
    #[default]
    Replace,
    Merge,
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
            EditOp::AddTrack { .. } => "add_track",
            EditOp::AddClip { .. } => "add_clip",
            EditOp::AddTransition { .. } => "add_transition",
            EditOp::SetParam { .. } => "set_param",
            EditOp::SetKeyframes { .. } => "set_keyframes",
            EditOp::SetSpeed { .. } => "set_speed",
            EditOp::FreezeFrame { .. } => "freeze_frame",
            EditOp::Nest { .. } => "nest",
            EditOp::Unnest { .. } => "unnest",
            EditOp::AddEffect { .. } => "add_effect",
            EditOp::SetEffectParam { .. } => "set_effect_param",
            EditOp::RemoveEffect { .. } => "remove_effect",
            EditOp::AddVideoEffect { .. } => "add_video_effect",
            EditOp::SetVideoEffectParam { .. } => "set_video_effect_param",
            EditOp::RemoveVideoEffect { .. } => "remove_video_effect",
            EditOp::MoveVideoEffect { .. } => "move_video_effect",
            EditOp::AddMarker { .. } => "add_marker",
            EditOp::UpdateMarker { .. } => "update_marker",
            EditOp::RemoveMarker { .. } => "remove_marker",
            EditOp::Relink { .. } => "relink",
        }
    }
    fn target(&self) -> String {
        match self {
            EditOp::RippleInsert { track, .. }
            | EditOp::AddClip { track, .. }
            | EditOp::AddTrack { name: track, .. } => format!("track {track:?}"),
            EditOp::SetParam {
                clip, track, param, ..
            }
            | EditOp::SetKeyframes {
                clip, track, param, ..
            } => match (clip, track) {
                (Some(c), _) => format!("{c}.{param}"),
                (None, Some(t)) => format!("track {t:?} {param}"),
                (None, None) => format!("timeline {param}"),
            },
            EditOp::Split { clip, .. }
            | EditOp::Trim { clip, .. }
            | EditOp::RippleDelete { clip, .. }
            | EditOp::Roll { clip, .. }
            | EditOp::Slip { clip, .. }
            | EditOp::Slide { clip, .. }
            | EditOp::Move { clip, .. }
            | EditOp::JlCut { clip, .. }
            | EditOp::SetSpeed { clip, .. }
            | EditOp::FreezeFrame { clip, .. }
            | EditOp::Unnest { clip }
            | EditOp::AddTransition { clip, .. } => clip.clone(),
            EditOp::Nest { path, .. } => path.display().to_string(),
            EditOp::AddEffect { clip, track, .. }
            | EditOp::SetEffectParam { clip, track, .. }
            | EditOp::RemoveEffect { clip, track, .. } => match (clip, track) {
                (Some(c), _) => format!("{c}.audio.effects"),
                (None, Some(t)) => format!("track {t:?} effects"),
                (None, None) => "effects".into(),
            },
            EditOp::AddVideoEffect { clip, track, .. }
            | EditOp::SetVideoEffectParam { clip, track, .. }
            | EditOp::RemoveVideoEffect { clip, track, .. }
            | EditOp::MoveVideoEffect { clip, track, .. } => match (clip, track) {
                (Some(c), _) => format!("{c}.effects"),
                (None, Some(t)) => format!("track {t:?} effects"),
                (None, None) => "effects".into(),
            },
            EditOp::AddMarker { clip, .. } => match clip {
                Some(c) => format!("{c} markers"),
                None => "timeline markers".into(),
            },
            EditOp::UpdateMarker { clip, id, .. } | EditOp::RemoveMarker { clip, id } => match clip
            {
                Some(c) => format!("{c} marker {id}"),
                None => format!("timeline marker {id}"),
            },
            EditOp::Relink { clip, from, .. } => match (clip, from) {
                (Some(c), _) => c.clone(),
                (None, Some(f)) => f.display().to_string(),
                (None, None) => "offline media".into(),
            },
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
    fn shift_local_keys(&mut self, dt: Rational) -> anyhow::Result<()>;
    /// Remove the incoming transition (video dissolve, audio crossfade).
    fn drop_transition_in(&mut self);
    fn speed(&self) -> &Animatable;
    fn speed_mut(&mut self) -> &mut Animatable;
    fn time_remap_mut(&mut self) -> &mut Option<Animatable>;
    fn time_map(&self) -> crate::retime::TimeMap;
    fn end(&self) -> RationalTime {
        self.start() + self.duration()
    }
    /// Source time at clip-local time `local` (`source_in + local` unless retimed).
    fn source_at(&self, local: RationalTime) -> RationalTime {
        self.time_map().source_at(local)
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
        fn speed(&self) -> &Animatable {
            &self.speed
        }
        fn speed_mut(&mut self) -> &mut Animatable {
            &mut self.speed
        }
        fn time_remap_mut(&mut self) -> &mut Option<Animatable> {
            &mut self.time_remap
        }
        fn time_map(&self) -> crate::retime::TimeMap {
            self.time_map()
        }
    };
}

impl Item for Clip {
    item_common!();
    fn shift_local_keys(&mut self, dt: Rational) -> anyhow::Result<()> {
        self.audio.gain_db = self.audio.gain_db.shifted(dt);
        self.audio.pan = self.audio.pan.shifted(dt);
        self.shift_video_keys(dt)
    }
    fn drop_transition_in(&mut self) {
        self.transition_in = None;
        self.audio.crossfade_in = None;
    }
}

impl Item for AudioClip {
    item_common!();
    fn shift_local_keys(&mut self, dt: Rational) -> anyhow::Result<()> {
        self.audio.gain_db = self.audio.gain_db.shifted(dt);
        self.audio.pan = self.audio.pan.shifted(dt);
        self.speed = self.speed.shifted(dt);
        self.time_remap = self.time_remap.as_ref().map(|a| a.shifted(dt));
        Ok(())
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
    *right.source_in_mut() = c.source_at(off);
    *right.duration_mut() = c.end() - at;
    right.drop_transition_in();
    right.audio_mut().in_offset = z();
    right.audio_mut().fade_in = None;
    right.shift_local_keys(-off.0)?;
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
            *c.source_in_mut() = c.source_at(d);
            *c.duration_mut() = c.duration() - d;
            c.shift_local_keys(-d.0)?;
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
    if let Some(r) = c.time_remap_mut() {
        *r = crate::retime::shift_values(r, d.0);
    }
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
    *r.source_in_mut() = b.source_at(d);
    *r.duration_mut() = b.duration() - d;
    r.shift_local_keys(-d.0)?;
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
        *n.source_in_mut() = n.source_at(d);
        *n.duration_mut() = n.duration() - d;
        n.shift_local_keys(-d.0)?;
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
    let m = c.time_map();
    if !m.is_identity() {
        let a = c.audio();
        let (vlo, vhi) = m.source_range(Rational::ZERO, c.duration().0);
        let (alo, ahi) = m.source_range(a.in_offset.0, (c.duration() + a.out_offset).0);
        ensure!(
            vlo >= -1e-9,
            "clip {}: its speed/time remap reaches source time {vlo:.4} s, before the start of {}",
            c.id(),
            c.source().display()
        );
        ensure!(
            alo >= -1e-9,
            "clip {}: its audio (with in/out offsets) reaches source time {alo:.4} s, before the start of {}",
            c.id(),
            c.source().display()
        );
        if let Some(len) = media_len {
            let l = len.0.to_f64() + 1e-9;
            // The last frame shown starts before `hi` (frames are [t, t + 1/fps)).
            ensure!(
                vhi <= l,
                "clip {}: its speed/time remap needs source up to {vhi:.4} s but {} is {} long",
                c.id(),
                c.source().display(),
                len
            );
            ensure!(
                ahi <= l || a.out_offset <= z(),
                "clip {}: its audio needs source up to {ahi:.4} s but {} is {} long",
                c.id(),
                c.source().display(),
                len
            );
        }
        return Ok(());
    }
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
type InfoProbe<'a> = Box<dyn FnMut(&Path) -> anyhow::Result<MediaFacts> + 'a>;

/// What `add_clip` needs to know about a media file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaFacts {
    pub duration: Option<RationalTime>,
    pub has_video: bool,
    pub has_audio: bool,
    pub size: Option<(u32, u32)>,
}

/// Media length lookup with a cache; `probe` returns `None` if unknown.
pub struct MediaLengths<'a> {
    probe: Probe<'a>,
    info: Option<InfoProbe<'a>>,
    cache: HashMap<PathBuf, Option<RationalTime>>,
    base: PathBuf,
    placeholder: bool,
    /// Comps created by `nest` in this batch: (full path, timeline as it
    /// will be written, sources relative to its own directory).
    new_comps: Vec<(PathBuf, Timeline)>,
}

impl<'a> MediaLengths<'a> {
    /// `base`: directory that relative clip sources resolve against.
    pub fn new(
        base: impl Into<PathBuf>,
        probe: impl FnMut(&Path) -> Option<RationalTime> + 'a,
    ) -> Self {
        MediaLengths {
            probe: Box::new(probe),
            info: None,
            cache: HashMap::new(),
            base: base.into(),
            placeholder: false,
            new_comps: Vec::new(),
        }
    }
    /// Comp files created by `nest` ops (full path, timeline). The caller
    /// writes them (they must not exist) along with the edited timeline.
    pub fn take_new_comps(&mut self) -> Vec<(PathBuf, Timeline)> {
        std::mem::take(&mut self.new_comps)
    }
    /// A nested comp's timeline as stored (sources relative to its own
    /// directory): one created earlier in this batch, or read from disk.
    fn comp(&self, full: &Path) -> anyhow::Result<Timeline> {
        if let Some((_, t)) = self.new_comps.iter().find(|(p, _)| p == full) {
            return Ok(t.clone());
        }
        crate::project::read_timeline(full)
    }
    /// Also probe streams for `add_clip` (missing file / missing stream errors,
    /// default durations).
    pub fn with_info(mut self, info: impl FnMut(&Path) -> anyhow::Result<MediaFacts> + 'a) -> Self {
        self.info = Some(Box::new(info));
        self
    }
    /// Resolve relative paths (clip sources, `nest` comp files) against `base`.
    pub fn with_base(mut self, base: impl Into<PathBuf>) -> Self {
        self.base = base.into();
        self
    }
    /// No media bounds (unknown lengths).
    pub fn unbounded() -> MediaLengths<'static> {
        MediaLengths::new(".", |_| None)
    }
    /// Structure only, for pre-checks that must not open media: unknown
    /// lengths, and `add_clip` without a duration uses a 1 s placeholder.
    pub fn placeholder() -> MediaLengths<'static> {
        let mut m = MediaLengths::unbounded();
        m.placeholder = true;
        m
    }
    fn full(&self, p: &Path) -> PathBuf {
        if p.is_relative() {
            self.base.join(p)
        } else {
            p.to_path_buf()
        }
    }
    fn get(&mut self, p: &Path) -> Option<RationalTime> {
        let full = self.full(p);
        if let Some(v) = self.cache.get(&full) {
            return *v;
        }
        let v = (self.probe)(&full);
        self.cache.insert(full, v);
        v
    }
    /// Stream facts, if this lookup probes streams (`Ok(None)`: it doesn't).
    fn facts(&mut self, p: &Path) -> anyhow::Result<Option<MediaFacts>> {
        let full = self.full(p);
        match &mut self.info {
            Some(f) => f(&full).map(Some),
            None => Ok(None),
        }
    }
}

fn check_track_bounds(
    tl: &Timeline,
    tr: TrackRef,
    media: &mut MediaLengths<'_>,
) -> anyhow::Result<()> {
    match tr {
        TrackRef::Video(i) => tl.tracks[i].clips.iter().try_for_each(|c| {
            let len = if c.is_generator() {
                None
            } else {
                media.get(&c.source)
            };
            check_bounds(c, len)
        }),
        TrackRef::Audio(i) => tl.audio_tracks[i]
            .clips
            .iter()
            .try_for_each(|c| check_bounds(c, media.get(&c.source))),
    }
}

/// Apply one op (without re-validating the whole timeline).
fn apply_one(
    tl: &mut Timeline,
    op: &EditOp,
    media: &mut MediaLengths<'_>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
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
        EditOp::SetSpeed {
            clip,
            speed,
            ripple,
            preserve_pitch,
        } => {
            let (tr, _) = locate(tl, clip)?;
            let ci = sorted(tl, tr, clip);
            let fps = tl.output.fps;
            let change = on_track!(tl, tr, |clips| set_speed(
                clips,
                ci,
                *speed,
                fps,
                *ripple,
                *preserve_pitch
            ))?;
            (change, vec![tr])
        }
        EditOp::FreezeFrame {
            clip,
            at,
            duration,
            new_id,
            all_tracks: all,
        } => freeze_frame(tl, clip, *at, *duration, new_id.as_deref(), *all)?,
        EditOp::AddTrack { kind, name, index } => add_track(tl, *kind, name, *index)?,
        EditOp::Nest { clips, path, id } => nest(tl, clips, path, id.as_deref(), media)?,
        EditOp::Unnest { clip } => unnest(tl, clip, media)?,
        EditOp::AddClip {
            track,
            fit,
            source,
            generator,
            id,
            start,
            source_in,
            duration,
            adjustment,
        } => add_clip(
            tl,
            media,
            track,
            source,
            generator.as_ref(),
            id.as_deref(),
            *start,
            *source_in,
            *duration,
            *adjustment,
            *fit,
        )?,
        EditOp::AddTransition {
            clip,
            duration,
            kind: TransitionKind::Dissolve,
            align,
        } => add_transition(tl, clip, *duration, *align)?,
        EditOp::AddEffect {
            clip,
            track,
            effect,
            index,
        } => edit_effects(tl, "add_effect", clip.as_deref(), track.as_deref(), |fx| {
            serde_json::from_value::<crate::audio_fx::EffectSpec>(effect.clone()).map_err(|e| {
                anyhow!(
                    "add_effect: bad effect {effect}: {e} (types: {})",
                    crate::audio_fx::TYPES
                        .iter()
                        .map(|(t, p)| format!("{t}: {p}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            })?;
            let i = index.unwrap_or(fx.len());
            ensure!(
                i <= fx.len(),
                "add_effect: index {i} is past the end ({} effects)",
                fx.len()
            );
            fx.insert(i, effect.clone());
            Ok(())
        })?,
        EditOp::SetEffectParam {
            clip,
            track,
            index,
            param,
            value,
        } => edit_effects(
            tl,
            "set_effect_param",
            clip.as_deref(),
            track.as_deref(),
            |fx| {
                let n = fx.len();
                let e = fx
                    .get_mut(*index)
                    .ok_or_else(|| anyhow!("set_effect_param: no effect {index} ({n} effects)"))?;
                crate::audio_fx::set_param(e, param, value.clone())?;
                Ok(())
            },
        )?,
        EditOp::RemoveEffect { clip, track, index } => edit_effects(
            tl,
            "remove_effect",
            clip.as_deref(),
            track.as_deref(),
            |fx| {
                ensure!(
                    *index < fx.len(),
                    "remove_effect: no effect {index} ({} effects)",
                    fx.len()
                );
                fx.remove(*index);
                Ok(())
            },
        )?,
        EditOp::AddVideoEffect {
            clip,
            track,
            effect,
            index,
        } => edit_video_effects(
            tl,
            "add_video_effect",
            clip.as_deref(),
            track.as_deref(),
            |fx| {
                let spec: crate::fx::VideoEffectSpec = serde_json::from_value(effect.clone())
                .map_err(|e| anyhow!("add_video_effect: bad effect {effect}: {e} (expected {{\"type\": ..., params}}; types: {})", crate::fx::type_names().join(", ")))?;
                crate::fx::ParsedEffect::parse(&spec)
                    .map_err(|e| anyhow!("add_video_effect: {e}"))?;
                let i = index.unwrap_or(fx.len());
                ensure!(
                    i <= fx.len(),
                    "add_video_effect: index {i} is past the end ({} effects)",
                    fx.len()
                );
                fx.insert(i, spec);
                Ok(())
            },
        )?,
        EditOp::SetVideoEffectParam {
            clip,
            track,
            effect,
            param,
            value,
        } => edit_video_effects(
            tl,
            "set_video_effect_param",
            clip.as_deref(),
            track.as_deref(),
            |fx| {
                let i = effect
                    .resolve(fx)
                    .map_err(|e| anyhow!("set_video_effect_param: {e}"))?;
                crate::fx::set_effect_param(&mut fx[i], param, value.clone())
                    .map_err(|e| anyhow!("set_video_effect_param: effect {}: {e}", fx[i].label(i)))
            },
        )?,
        EditOp::RemoveVideoEffect {
            clip,
            track,
            effect,
        } => edit_video_effects(
            tl,
            "remove_video_effect",
            clip.as_deref(),
            track.as_deref(),
            |fx| {
                let i = effect
                    .resolve(fx)
                    .map_err(|e| anyhow!("remove_video_effect: {e}"))?;
                fx.remove(i);
                Ok(())
            },
        )?,
        EditOp::MoveVideoEffect {
            clip,
            track,
            effect,
            to,
        } => edit_video_effects(
            tl,
            "move_video_effect",
            clip.as_deref(),
            track.as_deref(),
            |fx| {
                let i = effect
                    .resolve(fx)
                    .map_err(|e| anyhow!("move_video_effect: {e}"))?;
                ensure!(
                    *to < fx.len(),
                    "move_video_effect: to {to} is past the end ({} effects)",
                    fx.len()
                );
                let e = fx.remove(i);
                fx.insert(*to, e);
                Ok(())
            },
        )?,
        EditOp::SetParam {
            clip,
            track,
            param,
            value,
        } => set_param(
            tl,
            media,
            "set_param",
            clip.as_deref(),
            track.as_deref(),
            param,
            |_, _| Ok(value.clone()),
        )?,
        EditOp::SetKeyframes {
            clip,
            track,
            param,
            keyframes,
            mode,
            timeline_time,
        } => {
            let clip_place = match clip {
                Some(c) => {
                    let (tr, ci) = locate(tl, c)?;
                    Some(on_track!(tl, tr, |clips| (
                        clips[ci].start(),
                        clips[ci].time_map()
                    )))
                }
                None => None,
            };
            set_param(
                tl,
                media,
                "set_keyframes",
                clip.as_deref(),
                track.as_deref(),
                param,
                |spec, cur| {
                    crate::params::ensure_animatable(spec)?;
                    let mut mapped = keyframes.clone();
                    if *timeline_time
                        && spec.time == ferrocut_core::TimeBase::Source
                        && let Some((start, map)) = &clip_place
                    {
                        let mut seen = std::collections::HashSet::new();
                        for key in &mut mapped {
                            let time: RationalTime = serde_json::from_value(
                                key.get("t").cloned().unwrap_or(serde_json::Value::Null),
                            )
                            .context("source keyframe timeline time")?;
                            let source_time = map.source_at(RationalTime(time.0 - start.0));
                            ensure!(
                                seen.insert(source_time),
                                "multiple timeline keyframes map to source time {source_time}; use distinct source times or clip-local controls"
                            );
                            key.as_object_mut()
                                .context("keyframe must be an object")?
                                .insert("t".into(), serde_json::to_value(source_time)?);
                        }
                    }
                    let shift = match (timeline_time, spec.time, &clip_place) {
                        (true, ferrocut_core::TimeBase::ClipLocal, Some((s, _))) => s.0,
                        _ => Rational::ZERO,
                    };
                    crate::params::keyframes_value(cur, &mapped, shift, *mode == KeyMode::Merge)
                },
            )?
        }
        EditOp::AddMarker {
            clip,
            time,
            id,
            duration,
            name,
            color,
            comment,
            timeline_time,
        } => markers_op(tl, clip.as_deref(), "add_marker", |list, place| {
            let t = marker_time(*time, *timeline_time, place)?;
            let id = match id {
                Some(i) => {
                    ensure!(
                        !list.iter().any(|m| m.id == *i),
                        "marker id {i:?} is already used"
                    );
                    i.clone()
                }
                None => crate::markers::fresh_id(list),
            };
            list.push(Marker {
                id: id.clone(),
                time: t,
                duration: duration.unwrap_or_default(),
                name: name.clone(),
                color: *color,
                comment: comment.clone(),
            });
            list.sort_by_key(|m| m.time);
            Ok(format!("marker {id} {name:?} at {t}"))
        })?,
        EditOp::UpdateMarker {
            clip,
            id,
            time,
            duration,
            name,
            color,
            comment,
            timeline_time,
        } => markers_op(tl, clip.as_deref(), "update_marker", |list, place| {
            let t = match time {
                Some(t) => Some(marker_time(*t, *timeline_time, place)?),
                None => None,
            };
            let m = list
                .iter_mut()
                .find(|m| m.id == *id)
                .ok_or_else(|| anyhow!("no marker {id:?}"))?;
            if let Some(t) = t {
                m.time = t;
            }
            if let Some(d) = duration {
                m.duration = *d;
            }
            if let Some(n) = name {
                m.name = n.clone();
            }
            if let Some(c) = color {
                m.color = *c;
            }
            if let Some(c) = comment {
                m.comment = c.clone();
            }
            list.sort_by_key(|m| m.time);
            Ok(format!("marker {id} updated"))
        })?,
        EditOp::RemoveMarker { clip, id } => {
            markers_op(tl, clip.as_deref(), "remove_marker", |list, _| {
                let n = list.len();
                list.retain(|m| m.id != *id);
                ensure!(list.len() < n, "no marker {id:?}");
                Ok(format!("marker {id} removed"))
            })?
        }
        EditOp::Relink {
            clip,
            from,
            to,
            search,
        } => relink(
            tl,
            clip.as_deref(),
            from.as_deref(),
            to.as_deref(),
            search.as_deref(),
            media,
        )?,
    })
}

/// Run `f` on the timeline's markers or on `clip`'s (with its placement).
/// Markers never change the output: the span is empty.
fn markers_op(
    tl: &mut Timeline,
    clip: Option<&str>,
    kind: &'static str,
    f: impl FnOnce(&mut Vec<Marker>, Option<&ClipPlacement>) -> anyhow::Result<String>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    let summary = match clip {
        None => f(&mut tl.markers, None)?,
        Some(id) => match locate(tl, id)? {
            (TrackRef::Video(t), ci) => {
                let c = &mut tl.tracks[t].clips[ci];
                let p = ClipPlacement {
                    start: c.start,
                    duration: c.duration,
                    map: c.time_map(),
                };
                f(&mut c.markers, Some(&p))?
            }
            (TrackRef::Audio(t), ci) => {
                let c = &mut tl.audio_tracks[t].clips[ci];
                let p = ClipPlacement {
                    start: c.start,
                    duration: c.duration,
                    map: c.time_map(),
                };
                f(&mut c.markers, Some(&p))?
            }
        },
    };
    Ok((
        Change {
            op: 0,
            kind,
            summary,
            span: (z(), z()),
        },
        vec![],
    ))
}

/// A marker time as stored: timeline time for timeline markers; source
/// time for clip markers (`timeline_time` converts through the clip).
fn marker_time(
    t: RationalTime,
    timeline_time: bool,
    place: Option<&ClipPlacement>,
) -> anyhow::Result<RationalTime> {
    match (place, timeline_time) {
        (Some(p), true) => {
            ensure!(
                p.start <= t && t < p.start + p.duration,
                "timeline time {t} is outside the clip ({}..{})",
                p.start,
                p.start + p.duration
            );
            Ok(p.source_at(t))
        }
        _ => Ok(t),
    }
}

/// Files named `name` under `dir` (recursive, hidden directories skipped).
fn find_named(dir: &Path, name: &std::ffi::OsStr, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let hidden = p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if p.is_dir() {
            if depth > 0 && !hidden {
                find_named(&p, name, depth - 1, out);
            }
        } else if p.file_name() == Some(name) {
            out.push(p);
        }
    }
}

fn relink(
    tl: &mut Timeline,
    clip: Option<&str>,
    from: Option<&Path>,
    to: Option<&Path>,
    search: Option<&Path>,
    media: &mut MediaLengths<'_>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    let only = match clip {
        Some(id) => {
            let (tr, ci) = locate(tl, id)?;
            let src = match tr {
                TrackRef::Video(t) => {
                    let c = &tl.tracks[t].clips[ci];
                    ensure!(!c.is_generator(), "relink: {id} is a generator clip");
                    c.source.clone()
                }
                TrackRef::Audio(t) => tl.audio_tracks[t].clips[ci].source.clone(),
            };
            Some(src)
        }
        None => None,
    };
    // old -> new for every source that changes.
    let mut map: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let sources: Vec<PathBuf> = {
        let mut v: Vec<PathBuf> = tl.sources_mut().map(|s| s.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    match (from, to, search) {
        (None, Some(to), None) => {
            let old = only.clone().context(
                "relink: give `clip` + `to`, `from` + `to`, or `search` (+ optional `clip`)",
            )?;
            map.push((old, to.to_path_buf()));
        }
        (Some(from), Some(to), None) => {
            ensure!(only.is_none(), "relink: give `clip` or `from`, not both");
            for s in &sources {
                if s == from {
                    map.push((s.clone(), to.to_path_buf()));
                } else if let Ok(rest) = s.strip_prefix(from) {
                    map.push((s.clone(), to.join(rest)));
                }
            }
            ensure!(
                !map.is_empty(),
                "relink: no clip source is {} or under it",
                from.display()
            );
        }
        (None, None, Some(dir)) => {
            let root = media.full(dir);
            ensure!(
                root.is_dir(),
                "relink: search directory {} not found",
                root.display()
            );
            for s in &sources {
                if only.as_ref().is_some_and(|o| o != s) || media.full(s).exists() {
                    continue;
                }
                let Some(name) = s.file_name() else { continue };
                let mut found = Vec::new();
                find_named(&root, name, 8, &mut found);
                match found.as_slice() {
                    [one] => {
                        let rel = one.strip_prefix(&root).unwrap_or(one);
                        map.push((s.clone(), dir.join(rel)));
                    }
                    [] => missing.push(s.display().to_string()),
                    many => bail!(
                        "relink: {} matches several files: {}",
                        s.display(),
                        many.iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
            }
            ensure!(
                !map.is_empty(),
                "relink: no offline source found under {}{}",
                dir.display(),
                if missing.is_empty() {
                    " (no source is offline)".to_string()
                } else {
                    format!(" (not found: {})", missing.join(", "))
                }
            );
        }
        _ => bail!("relink: give `clip` + `to`, `from` + `to`, or `search` (+ optional `clip`)"),
    }
    for (_, new) in &map {
        ensure!(
            media.placeholder || media.full(new).is_file(),
            "relink: {} does not exist",
            media.full(new).display()
        );
    }
    let mut n = 0;
    for s in tl.sources_mut() {
        if let Some((_, new)) = map.iter().find(|(o, _)| o == s) {
            *s = new.clone();
            n += 1;
        }
    }
    let mut summary = format!(
        "relink {n} clip(s): {}",
        map.iter()
            .map(|(o, n)| format!("{} -> {}", o.display(), n.display()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !missing.is_empty() {
        summary.push_str(&format!("; still offline: {}", missing.join(", ")));
    }
    Ok((
        Change {
            op: 0,
            kind: "relink",
            summary,
            span: (z(), tl.duration()),
        },
        all_tracks(tl),
    ))
}

/// Edit the video effect stack (`effects`) of a clip or video track.
fn edit_video_effects(
    tl: &mut Timeline,
    kind: &'static str,
    clip: Option<&str>,
    track: Option<&str>,
    f: impl FnOnce(&mut Vec<crate::fx::VideoEffectSpec>) -> anyhow::Result<()>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    ensure!(
        clip.is_some() != track.is_some(),
        "{kind}: give a clip or a video track"
    );
    let (mut change, refs) = set_param(
        tl,
        &mut MediaLengths::unbounded(),
        kind,
        clip,
        track,
        "effects",
        |_, cur| {
            let mut fx: Vec<crate::fx::VideoEffectSpec> = match cur {
                Some(v @ serde_json::Value::Array(_)) => serde_json::from_value(v.clone())?,
                _ => Vec::new(),
            };
            f(&mut fx)?;
            Ok(if fx.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::to_value(fx)?
            })
        },
    )?;
    change.kind = kind;
    Ok((change, refs))
}

/// Edit the effect chain of a clip (`audio.effects`) or track bus (`effects`).
fn edit_effects(
    tl: &mut Timeline,
    kind: &'static str,
    clip: Option<&str>,
    track: Option<&str>,
    f: impl FnOnce(&mut Vec<serde_json::Value>) -> anyhow::Result<()>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    ensure!(
        clip.is_some() != track.is_some(),
        "{kind}: give a clip or a track"
    );
    let name = if clip.is_some() {
        "audio.effects"
    } else {
        "bus.effects"
    };
    let (mut change, refs) = set_param(
        tl,
        &mut MediaLengths::unbounded(),
        kind,
        clip,
        track,
        name,
        |_, cur| {
            let mut fx = match cur {
                Some(serde_json::Value::Array(a)) => a.clone(),
                _ => Vec::new(),
            };
            f(&mut fx)?;
            Ok(if fx.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::Array(fx)
            })
        },
    )?;
    change.kind = kind;
    Ok((change, refs))
}

fn add_track(
    tl: &mut Timeline,
    kind: TrackKind,
    name: &str,
    index: Option<usize>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    ensure!(!name.is_empty(), "add_track: name must not be empty");
    ensure!(
        !tl.track_names().any(|n| n == name),
        "add_track: a track named {name:?} already exists"
    );
    let (len, what) = match kind {
        TrackKind::Video => (tl.tracks.len(), "video"),
        TrackKind::Audio => (tl.audio_tracks.len(), "audio"),
    };
    let i = index.unwrap_or(len);
    ensure!(
        i <= len,
        "add_track: index {i} is past the end ({len} {what} tracks)"
    );
    match kind {
        TrackKind::Video => tl.tracks.insert(
            i,
            crate::timeline::Track {
                name: name.into(),
                audio: Default::default(),
                matte: None,
                effects: vec![],
                clips: vec![],
            },
        ),
        TrackKind::Audio => tl.audio_tracks.insert(
            i,
            crate::timeline::AudioTrack {
                name: name.into(),
                bus: Default::default(),
                clips: vec![],
            },
        ),
    }
    Ok((
        Change {
            op: 0,
            kind: "add_track",
            summary: format!("add {what} track {name:?} at index {i}"),
            span: (z(), z()),
        },
        vec![],
    ))
}

#[allow(clippy::too_many_arguments)]
fn add_clip(
    tl: &mut Timeline,
    media: &mut MediaLengths<'_>,
    track: &str,
    source: &Path,
    generator: Option<&serde_json::Value>,
    id: Option<&str>,
    start: Option<RationalTime>,
    source_in: Option<RationalTime>,
    duration: Option<RationalTime>,
    adjustment: bool,
    fit: Option<crate::placement::Fit>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    let tr = track_by_name(tl, track).map_err(|e| {
        let names: Vec<&str> = tl.track_names().collect();
        anyhow!("{e} (tracks: {names:?}; add one with add_track)")
    })?;
    if adjustment {
        ensure!(
            path_is_empty(source) && generator.is_none(),
            "add_clip: an adjustment layer takes no source or generator"
        );
        ensure!(
            matches!(tr, TrackRef::Video(_)),
            "add_clip: adjustment layers go on video tracks"
        );
        ensure!(
            duration.is_some(),
            "add_clip: an adjustment layer has no media length; pass `duration`"
        );
    }
    let generator = match generator {
        None if adjustment => None,
        None => {
            ensure!(
                !path_is_empty(source),
                "add_clip: give a source (media file or comp) or a generator"
            );
            None
        }
        Some(g) => {
            ensure!(
                path_is_empty(source),
                "add_clip: give either source or generator, not both"
            );
            ensure!(
                matches!(tr, TrackRef::Video(_)),
                "add_clip: generators go on video tracks"
            );
            ensure!(
                duration.is_some(),
                "add_clip: a generator has no media length; pass `duration`"
            );
            let g: crate::generator::GeneratorSpec = serde_json::from_value(g.clone())
                .map_err(|e| anyhow!("add_clip: generator: {e}"))?;
            g.validate().map_err(|e| anyhow!("add_clip: {e}"))?;
            Some(g)
        }
    };
    let facts = if generator.is_some() || adjustment {
        None
    } else {
        media.facts(source)?
    };
    if let Some(f) = facts {
        match tr {
            TrackRef::Video(_) => ensure!(
                f.has_video,
                "{} has no video stream; put it on an audio track",
                source.display()
            ),
            TrackRef::Audio(_) => ensure!(f.has_audio, "{} has no audio stream", source.display()),
        }
    }
    let source_in = source_in.unwrap_or(z());
    ensure!(source_in >= z(), "add_clip: source_in must be >= 0");
    let len = match &generator {
        Some(_) => None,
        None if adjustment => None,
        None => facts.and_then(|f| f.duration).or_else(|| media.get(source)),
    };
    let duration = match (duration, len) {
        (Some(d), _) => d,
        (None, Some(l)) => {
            ensure!(
                l > source_in,
                "add_clip: source_in {source_in} is past the end of {} ({l})",
                source.display()
            );
            l - source_in
        }
        (None, None) if media.placeholder => RationalTime(Rational::ONE),
        (None, None) => bail!(
            "add_clip: {} has no known duration; pass `duration`",
            source.display()
        ),
    };
    ensure!(duration > z(), "add_clip: duration must be positive");
    let start = match start {
        Some(s) => s,
        None => on_track!(tl, tr, |clips| clips
            .iter()
            .map(|c| c.end())
            .max()
            .unwrap_or(z())),
    };
    ensure!(start >= z(), "add_clip: start must be >= 0");
    let end = start + duration;
    on_track!(tl, tr, |clips| {
        if let Some(c) = clips.iter().find(|c| c.start() < end && c.end() > start) {
            bail!(
                "add_clip: {start}..{end} overlaps clip {} ({}..{}) on track {track:?}; pick a free range, or use ripple_insert to push clips right",
                c.id(),
                c.start(),
                c.end()
            );
        }
        Ok::<(), anyhow::Error>(())
    })?;
    let id = match id {
        Some(i) => {
            ensure!(!i.is_empty(), "add_clip: id must not be empty");
            ensure!(
                !tl.clip_ids().any(|x| x == i),
                "add_clip: clip id {i:?} is already used"
            );
            i.to_string()
        }
        None => {
            let stem = match &generator {
                Some(g) => g.type_name().to_string(),
                None if adjustment => "adjustment".to_string(),
                None => source
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "clip".into()),
            };
            if tl.clip_ids().any(|x| x == stem) {
                unique_id(tl, &stem)
            } else {
                stem
            }
        }
    };
    ensure!(
        fit.is_none() || matches!(tr, TrackRef::Video(_)),
        "add_clip: fit applies to video clips only"
    );
    match tr {
        TrackRef::Video(i) => tl.tracks[i].clips.push(Clip {
            id: id.clone(),
            source: source.to_path_buf(),
            generator: generator.clone(),
            start,
            source_in,
            duration,
            opacity: Animatable::constant(Rational::ONE),
            transform: None,
            fit,
            three_d: false,
            motion_blur: false,
            transition_in: None,
            speed: crate::timeline::one(),
            time_remap: None,
            sampling: Default::default(),
            blend_mode: Default::default(),
            audio: ClipAudio::default(),
            markers: Vec::new(),
            effects: Vec::new(),
            masks: Default::default(),
            adjustment,
        }),
        TrackRef::Audio(i) => tl.audio_tracks[i].clips.push(AudioClip {
            id: id.clone(),
            source: source.to_path_buf(),
            start,
            source_in,
            duration,
            speed: crate::timeline::one(),
            time_remap: None,
            audio: ClipAudio::default(),
            markers: Vec::new(),
        }),
    }
    Ok((
        Change {
            op: 0,
            kind: "add_clip",
            summary: format!(
                "add clip {id} ({}, source {}..{}) at {start}..{end} on {track:?}",
                match &generator {
                    Some(g) => format!("{} generator", g.type_name()),
                    None => source.display().to_string(),
                },
                source_in,
                source_in + duration
            ),
            span: (start, end),
        },
        vec![tr],
    ))
}

fn add_transition(
    tl: &mut Timeline,
    clip: &str,
    d: RationalTime,
    align: Align,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    let (tr, _) = locate(tl, clip)?;
    let TrackRef::Video(ti) = tr else {
        bail!("add_transition: {clip} is on an audio track (use set_param audio.crossfade_in)");
    };
    ensure!(d > z(), "add_transition: duration must be positive");
    let clips = &mut tl.tracks[ti].clips;
    clips.sort_by_key(|c| c.start);
    let ci = clips.iter().position(|c| c.id == clip).expect("located");
    ensure!(
        ci > 0,
        "add_transition: {clip} is the first clip on its track; a dissolve needs a clip before it"
    );
    let (a, b) = (clips[ci - 1].clone(), clips[ci].clone());
    ensure!(
        b.transition_in.is_none(),
        "add_transition: {clip} already has a transition_in (set_param transition_in null to remove it)"
    );
    let overlap = a.end() - b.start;
    let half = RationalTime(d.0 / Rational::from_int(2));
    let (region, how) = if overlap > z() {
        ensure!(
            overlap >= d,
            "add_transition: {} and {clip} overlap by {overlap}, less than the {d} dissolve",
            a.id
        );
        ((b.start, b.start + d), "existing overlap".to_string())
    } else {
        ensure!(
            overlap == z(),
            "add_transition: there is a gap of {} between {} (ends {}) and {clip} (starts {}); the clips must meet",
            z() - overlap,
            a.id,
            a.end(),
            b.start
        );
        let cut = b.start;
        let (ext_a, ext_b) = match align {
            Align::Center => (half, d - half),
            Align::Start => (d, z()),
            Align::End => (z(), d),
        };
        ensure!(
            ext_a < b.duration && ext_b < a.duration,
            "add_transition: a {d} dissolve doesn't fit between {} and {clip}",
            a.id
        );
        // `a` runs longer (source handle after its out point); `b` starts
        // earlier (handle before its in point), content staying in place.
        let pa = &mut clips[ci - 1];
        pa.duration = pa.duration + ext_a;
        let pb = &mut clips[ci];
        pb.start = pb.start - ext_b;
        pb.source_in = pb.source_in - ext_b;
        pb.duration = pb.duration + ext_b;
        pb.shift_local_keys(ext_b.0)?;
        (
            (cut - ext_b, cut + ext_a),
            format!(
                "{} extended by {ext_a}, {clip} starts {ext_b} earlier",
                a.id
            ),
        )
    };
    let pb = &mut clips[ci];
    pb.transition_in = Some(crate::timeline::Transition::Dissolve { duration: d });
    let a_audio = &clips[ci - 1].audio;
    let xfade = a_audio.out_offset == z()
        && clips[ci].audio.in_offset == z()
        && clips[ci].audio.crossfade_in.is_none()
        && clips[ci - 1].end() == clips[ci].start + d;
    if xfade {
        clips[ci].audio.crossfade_in = Some(crate::timeline::FadeSpec {
            duration: d,
            curve: Default::default(),
        });
    }
    Ok((
        Change {
            op: 0,
            kind: "add_transition",
            summary: format!(
                "dissolve {} -> {clip} over {}..{} ({how}{})",
                a.id,
                region.0,
                region.1,
                if xfade { "; audio crossfade" } else { "" }
            ),
            span: region,
        },
        vec![tr],
    ))
}

/// Shared by set_param / set_keyframes: `make(spec, current)` builds the new value.
fn set_param(
    tl: &mut Timeline,
    media: &mut MediaLengths<'_>,
    kind: &'static str,
    clip: Option<&str>,
    track: Option<&str>,
    name: &str,
    make: impl FnOnce(
        &ferrocut_core::ParamSpec,
        Option<&serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    use crate::params::{self, Scope};
    let frame = (tl.output.width, tl.output.height);
    ensure!(
        !(clip.is_some() && track.is_some()),
        "give clip or track, not both (neither = the timeline)"
    );
    let (scope, tr) = match (clip, track) {
        (Some(c), _) => {
            let (tr, ci) = locate(tl, c)?;
            (
                match tr {
                    TrackRef::Video(_) => Scope::VideoClip,
                    TrackRef::Audio(_) => Scope::AudioClip,
                },
                Some((tr, Some(ci))),
            )
        }
        (None, Some(t)) => {
            let tr = track_by_name(tl, t)?;
            (
                Scope::Track {
                    audio_track: matches!(tr, TrackRef::Audio(_)),
                },
                Some((tr, None)),
            )
        }
        (None, None) => (Scope::Timeline, None),
    };
    let (spec, comp, path) = params::resolve_path(scope, name)?;
    ensure!(
        !(scope == (Scope::Track { audio_track: true }) && spec.name == "matte"),
        "matte applies to video tracks only"
    );
    ensure!(
        !(scope == (Scope::Track { audio_track: true }) && spec.name == "effects"),
        "video effects apply to clips and video tracks; audio tracks take audio effects (add_effect)"
    );
    let mut obj = match tr {
        Some((TrackRef::Video(i), Some(ci))) => serde_json::to_value(&tl.tracks[i].clips[ci])?,
        Some((TrackRef::Audio(i), Some(ci))) => {
            serde_json::to_value(&tl.audio_tracks[i].clips[ci])?
        }
        Some((TrackRef::Video(i), None)) => serde_json::to_value(&tl.tracks[i])?,
        Some((TrackRef::Audio(i), None)) => serde_json::to_value(&tl.audio_tracks[i])?,
        None => serde_json::to_value(&*tl)?,
    };
    let cur = params::get_path(&obj, &path, comp);
    let before = cur.clone().unwrap_or(serde_json::Value::Null);
    let value = make(spec, cur.as_ref())?;
    let frame = if spec.name == "transform.anchor"
        && comp.is_some()
        && params::get_path(&obj, &path, None).is_none()
    {
        let Some((TrackRef::Video(ti), Some(ci))) = tr else {
            unreachable!("anchor is a video parameter")
        };
        let c = &tl.tracks[ti].clips[ci];
        if c.is_generator() || media.placeholder {
            // Path-only MCP preflight must not open media. The real edit
            // repeats this step after all source paths have been checked.
            frame
        } else {
            let size = if crate::comp::is_comp(&c.source) {
                let inner = media.comp(&media.full(&c.source))?;
                Some((inner.output.width, inner.output.height))
            } else {
                media.facts(&c.source)?.and_then(|f| f.size)
            };
            size.context(format!("{name}: the source size is needed to fill the other component; set both components, or enable probing / relink the media"))?
        }
    } else {
        frame
    };
    params::set_path(&mut obj, spec, comp, value.clone(), frame, &path)?;
    params::check_range_path(&obj, spec, &path)?;
    let bad = |e: serde_json::Error| anyhow!("{name}: invalid value {value}: {e}");
    let span = match tr {
        Some((TrackRef::Video(i), Some(ci))) => {
            let c: Clip = serde_json::from_value(obj).map_err(bad)?;
            tl.tracks[i].clips[ci] = c;
            let c = &tl.tracks[i].clips[ci];
            let (a0, a1) = c.audio_region();
            (c.start.min(a0), c.end().max(a1))
        }
        Some((TrackRef::Audio(i), Some(ci))) => {
            let c: AudioClip = serde_json::from_value(obj).map_err(bad)?;
            tl.audio_tracks[i].clips[ci] = c;
            tl.audio_tracks[i].clips[ci].audio_region()
        }
        Some((TrackRef::Video(i), None)) => {
            tl.tracks[i] = serde_json::from_value(obj).map_err(bad)?;
            (z(), tl.duration())
        }
        Some((TrackRef::Audio(i), None)) => {
            tl.audio_tracks[i] = serde_json::from_value(obj).map_err(bad)?;
            (z(), tl.duration())
        }
        None => {
            let old_end = tl.duration();
            let new: Timeline = serde_json::from_value(obj).map_err(bad)?;
            *tl = new;
            (z(), old_end.max(tl.duration()))
        }
    };
    let after = {
        let obj = match tr {
            Some((TrackRef::Video(i), Some(ci))) => serde_json::to_value(&tl.tracks[i].clips[ci])?,
            Some((TrackRef::Audio(i), Some(ci))) => {
                serde_json::to_value(&tl.audio_tracks[i].clips[ci])?
            }
            Some((TrackRef::Video(i), None)) => serde_json::to_value(&tl.tracks[i])?,
            Some((TrackRef::Audio(i), None)) => serde_json::to_value(&tl.audio_tracks[i])?,
            None => serde_json::to_value(&*tl)?,
        };
        params::get_path(&obj, &path, comp).unwrap_or(serde_json::Value::Null)
    };
    let target = match (clip, track) {
        (Some(c), _) => c.to_string(),
        (None, Some(t)) => format!("track {t:?}"),
        (None, None) => "timeline".into(),
    };
    Ok((
        Change {
            op: 0,
            kind,
            summary: format!("{target} {name}: {before} -> {after}"),
            span,
        },
        tr.map(|(t, _)| vec![t]).unwrap_or_default(),
    ))
}

fn set_speed<T: Item>(
    clips: &mut [T],
    ci: usize,
    s: Rational,
    fps: Rational,
    ripple: bool,
    preserve_pitch: Option<bool>,
) -> anyhow::Result<Change> {
    let c = clips[ci].clone();
    ensure!(
        !matches!(c.time_map(), crate::retime::TimeMap::Remap { .. }),
        "set_speed: clip {} has a time_remap curve; remove it first (set_param time_remap null)",
        c.id()
    );
    let Some(s0) = c.speed().as_constant() else {
        bail!(
            "set_speed: clip {} has a keyframed speed ramp; use set_keyframes / set_param on `speed`",
            c.id()
        );
    };
    ensure!(
        !s.is_zero(),
        "set_speed: speed 0 is a freeze; use the freeze_frame op"
    );
    ensure!(
        !s0.is_zero(),
        "set_speed: clip {} is a freeze frame (speed 0): it has no source range to retime",
        c.id()
    );
    ensure!(
        s >= Rational::from_int(-100) && s <= Rational::from_int(100),
        "set_speed: speed must be within [-100, 100]"
    );
    // Source range [lo, hi) the clip shows; reverse clips start one output
    // frame (at their speed) before `hi`, so they end exactly on `lo`.
    let step = Rational::ONE / fps;
    let (si, d0) = (c.source_in().0, c.duration().0);
    let (lo, hi) = if s0 > Rational::ZERO {
        (si, si + s0 * d0)
    } else {
        let hi = si - s0 * step;
        (hi + s0 * d0, hi)
    };
    let mag = if s < Rational::ZERO { -s } else { s };
    let d1 = (hi - lo) / mag;
    let si1 = if s > Rational::ZERO {
        lo
    } else {
        hi - mag * step
    };
    let old_end = c.end();
    {
        let m = &mut clips[ci];
        *m.speed_mut() = Animatable::constant(s);
        *m.source_in_mut() = RationalTime(si1);
        *m.duration_mut() = RationalTime(d1);
        if let Some(p) = preserve_pitch {
            m.audio_mut().preserve_pitch = p;
        }
    }
    let delta = RationalTime(d1 - d0);
    if ripple && delta != z() {
        for (i, o) in clips.iter_mut().enumerate() {
            if i != ci && o.start() >= old_end {
                *o.start_mut() = o.start() + delta;
            }
        }
    }
    let new_end = c.start() + RationalTime(d1);
    Ok(Change {
        op: 0,
        kind: "set_speed",
        summary: format!(
            "set_speed {} {s0} -> {s}: duration {} -> {}, source {lo}..{hi}{}",
            c.id(),
            c.duration(),
            RationalTime(d1),
            if ripple { " (rippled)" } else { "" }
        ),
        span: (c.start(), old_end.max(new_end)),
    })
}

fn freeze_frame(
    tl: &mut Timeline,
    clip: &str,
    at: RationalTime,
    duration: Option<RationalTime>,
    new_id: Option<&str>,
    all: bool,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    let (tr, ci) = locate(tl, clip)?;
    let TrackRef::Video(ti) = tr else {
        bail!("freeze_frame: {clip} is an audio clip; frame holds are for video clips");
    };
    let c = tl.tracks[ti].clips[ci].clone();
    ensure!(
        at >= c.start && at < c.end(),
        "freeze_frame: {at} is not inside clip {clip} ({}..{})",
        c.start,
        c.end()
    );
    if let Some(d) = duration {
        ensure!(d > z(), "freeze_frame: duration must be positive");
    }
    let src = c.source_at(at - c.start);
    let fresh = |tl: &Timeline, want: Option<&str>, base: &str| -> anyhow::Result<String> {
        match want {
            Some(n) => {
                ensure!(
                    !tl.clip_ids().any(|i| i == n),
                    "new_id {n:?} is already used"
                );
                Ok(n.to_string())
            }
            None => Ok(unique_id(tl, base)),
        }
    };
    let make_hold =
        |c: &Clip, id: String, start: RationalTime, dur: RationalTime| -> anyhow::Result<Clip> {
            let mut h = c.clone();
            h.shift_local_keys(-(start - c.start).0)?;
            h.id = id;
            h.start = start;
            h.duration = dur;
            h.source_in = src;
            h.speed = Animatable::constant(Rational::ZERO);
            h.time_remap = None;
            h.sampling = crate::retime::Sampling::Nearest;
            h.transition_in = None;
            h.audio = ClipAudio {
                mute: true,
                ..ClipAudio::default()
            };
            Ok(h)
        };
    let mut touched = vec![tr];
    let summary;
    match duration {
        None => {
            if at == c.start {
                let h = make_hold(&c, c.id.clone(), c.start, c.duration)?;
                let keep_tx = c.transition_in.clone();
                tl.tracks[ti].clips[ci] = Clip {
                    transition_in: keep_tx,
                    ..h
                };
                summary = format!("freeze_frame {clip}: holds source {src} for its whole length");
            } else {
                let id = fresh(tl, new_id, &format!("{clip}.hold"))?;
                let ci = on_track!(tl, tr, |clips| sort_track(clips, clip));
                on_track!(tl, tr, |clips| split(clips, ci, at, id.clone()))?;
                let (_, ri) = locate(tl, &id)?;
                let right = tl.tracks[ti].clips[ri].clone();
                let mut h = make_hold(&c, id.clone(), at, right.duration)?;
                h.audio.mute = true;
                tl.tracks[ti].clips[ri] = h;
                summary = format!(
                    "freeze_frame {clip} at {at}: {id} holds source {src} to {}",
                    c.end()
                );
            }
        }
        Some(d) => {
            let targets = if all { all_tracks(tl) } else { vec![tr] };
            for t in &targets {
                if *t == tr {
                    continue;
                }
                on_track!(tl, *t, |clips| {
                    if let Some(o) = clips.iter().find(|o| o.start() < at && o.end() > at) {
                        bail!(
                            "freeze_frame on all tracks: clip {} ({}..{}) spans {at}; split it first",
                            o.id(),
                            o.start(),
                            o.end()
                        );
                    }
                    Ok::<(), anyhow::Error>(())
                })?;
            }
            let hold_id = fresh(tl, new_id, &format!("{clip}.hold"))?;
            if at > c.start {
                let right_id = unique_id(tl, clip);
                let ci = on_track!(tl, tr, |clips| sort_track(clips, clip));
                on_track!(tl, tr, |clips| split(clips, ci, at, right_id))?;
            }
            for t in &targets {
                on_track!(tl, *t, |clips| shift_after(clips, at, d, false));
            }
            let h = make_hold(&c, hold_id.clone(), at, d)?;
            tl.tracks[ti].clips.push(h);
            touched = targets;
            summary = format!(
                "freeze_frame {clip} at {at}: inserted {hold_id} holding source {src} for {d}; later clips moved {d} later"
            );
        }
    }
    let end = tl.duration();
    Ok((
        Change {
            op: 0,
            kind: "freeze_frame",
            summary,
            span: (at, end.max(c.end())),
        },
        touched,
    ))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let c = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    c(a) == c(b)
}

fn dir_of(p: &Path) -> PathBuf {
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Re-express `src` (relative to `from`, or absolute) for a timeline stored
/// in `to`: unchanged when the directories match, else absolute.
fn path_is_empty(p: &Path) -> bool {
    p.as_os_str().is_empty()
}

fn rebase_source(src: &Path, from: &Path, to: &Path) -> PathBuf {
    if path_is_empty(src) || src.is_absolute() || same_dir(from, to) {
        return src.to_path_buf();
    }
    let j = from.join(src);
    std::fs::canonicalize(&j).unwrap_or(j)
}

/// The clip on the same track that a clip at sorted index `i` dissolves from.
fn previous(clips: &[Clip], i: usize) -> Option<&Clip> {
    i.checked_sub(1).map(|j| &clips[j])
}

fn nest(
    tl: &mut Timeline,
    ids: &[String],
    path: &Path,
    id: Option<&str>,
    media: &mut MediaLengths<'_>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    ensure!(!ids.is_empty(), "nest: give at least one clip");
    ensure!(
        crate::comp::is_comp(path),
        "nest: path {} must be a .json timeline file",
        path.display()
    );
    let full = media.full(path);
    ensure!(
        !full.exists() && !media.new_comps.iter().any(|(p, _)| *p == full),
        "nest: {} already exists; choose a new file name",
        full.display()
    );
    let wanted: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    ensure!(wanted.len() == ids.len(), "nest: a clip is listed twice");
    let mut sel: Vec<(usize, Clip)> = Vec::new();
    for cid in ids {
        let (tr, ci) = locate(tl, cid)?;
        let TrackRef::Video(ti) = tr else {
            bail!(
                "nest: {cid} is an audio clip; nest takes video clips (their linked audio comes along)"
            );
        };
        sel.push((ti, tl.tracks[ti].clips[ci].clone()));
    }
    let s0 = sel.iter().map(|(_, c)| c.start).min().expect("non-empty");
    let s1 = sel.iter().map(|(_, c)| c.end()).max().expect("non-empty");
    for (_, c) in &sel {
        let (a0, a1) = c.audio_region();
        ensure!(
            c.audio.mute || (a0 >= s0 && a1 <= s1),
            "nest: clip {}'s linked audio ({a0}..{a1}) extends outside the nested span {s0}..{s1}; remove its J/L offset or nest the neighbouring clip too",
            c.id
        );
    }
    // Dissolves must stay inside the selection.
    let mut used: Vec<usize> = sel.iter().map(|(t, _)| *t).collect();
    used.sort_unstable();
    used.dedup();
    for &ti in &used {
        let mut clips = tl.tracks[ti].clips.clone();
        clips.sort_by_key(|c| c.start);
        for (i, c) in clips.iter().enumerate() {
            if c.transition_in.is_none() {
                continue;
            }
            if let Some(p) = previous(&clips, i) {
                let (a, b) = (
                    wanted.contains(c.id.as_str()),
                    wanted.contains(p.id.as_str()),
                );
                ensure!(
                    a == b,
                    "nest: clip {} dissolves from {}; nest both or neither",
                    c.id,
                    p.id
                );
            }
        }
    }
    let comp_dir = dir_of(&full);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "comp".into());
    let mut inner = Timeline {
        name: stem.clone(),
        output: crate::timeline::OutputSpec {
            duration: None,
            ..tl.output.clone()
        },
        tracks: Vec::new(),
        audio_tracks: Vec::new(),
        audio: crate::timeline::AudioSettings {
            sample_rate: tl.audio.sample_rate,
            ..Default::default()
        },
        // 3D layers and motion blur look the same inside the comp.
        camera: tl.camera.as_ref().map(|c| c.shifted(-s0.0)),
        motion_blur: tl.motion_blur,
        markers: Vec::new(),
    };
    for &ti in &used {
        let mut clips: Vec<Clip> = sel
            .iter()
            .filter(|(t, _)| *t == ti)
            .map(|(_, c)| {
                let mut c = c.clone();
                c.start = c.start - s0;
                c.source = rebase_source(&c.source, &media.base, &comp_dir);
                c
            })
            .collect();
        clips.sort_by_key(|c| c.start);
        inner.tracks.push(crate::timeline::Track {
            name: tl.tracks[ti].name.clone(),
            audio: Default::default(),
            matte: None,
            effects: vec![],
            clips,
        });
    }
    inner.validate().context("nest: the nested timeline")?;
    for &ti in &used {
        tl.tracks[ti]
            .clips
            .retain(|c| !wanted.contains(c.id.as_str()));
    }
    let cid = match id {
        Some(n) => {
            ensure!(!n.is_empty(), "nest: id must not be empty");
            ensure!(
                !tl.clip_ids().any(|i| i == n),
                "nest: id {n:?} is already used"
            );
            n.to_string()
        }
        None if !tl.clip_ids().any(|i| i == stem) => stem.clone(),
        None => unique_id(tl, &stem),
    };
    let comp_clip: Clip = serde_json::from_value(serde_json::json!({
        "id": cid,
        "source": path,
        "start": s0,
        "duration": s1 - s0,
    }))?;
    let low = used[0];
    tl.tracks[low].clips.push(comp_clip);
    media.cache.insert(full.clone(), Some(inner.duration()));
    media.new_comps.push((full, inner));
    Ok((
        Change {
            op: 0,
            kind: "nest",
            summary: format!(
                "nest: {} clip(s) from {} track(s) into {} ({s0}..{s1}); comp clip {cid} on track {:?}",
                sel.len(),
                used.len(),
                path.display(),
                tl.tracks[low].name
            ),
            span: (s0, s1),
        },
        used.into_iter().map(TrackRef::Video).collect(),
    ))
}

fn unnest(
    tl: &mut Timeline,
    clip: &str,
    media: &mut MediaLengths<'_>,
) -> anyhow::Result<(Change, Vec<TrackRef>)> {
    let (tr, ci) = locate(tl, clip)?;
    let TrackRef::Video(ti) = tr else {
        bail!("unnest: {clip} is an audio clip");
    };
    let c = tl.tracks[ti].clips[ci].clone();
    ensure!(
        crate::comp::is_comp(&c.source),
        "unnest: {clip} is not a nested composition (its source is {})",
        c.source.display()
    );
    ensure!(
        c.time_map().is_identity()
            && c.transform.is_none()
            && c.blend_mode.is_normal()
            && c.transition_in.is_none()
            && crate::timeline::is_one_anim(&c.opacity)
            && c.audio == ClipAudio::default(),
        "unnest: {clip} has its own speed/time remap, transform, opacity, blend mode, transition or audio settings, which unnesting would drop; reset them first"
    );
    let full = media.full(&c.source);
    let inner = media
        .comp(&full)
        .with_context(|| format!("unnest: reading {}", full.display()))?;
    ensure!(
        c.fit.is_none()
            && (inner.output.width, inner.output.height) == (tl.output.width, tl.output.height),
        "unnest would change the picture; reset explicit fit and match the composition size first"
    );
    ensure!(
        inner.output.fit.unwrap_or_default() == tl.output.fit.unwrap_or_default(),
        "unnest would change the picture: inner and parent output.fit differ"
    );
    ensure!(
        inner.audio_tracks.iter().all(|t| t.clips.is_empty()),
        "unnest: {} has audio tracks; unnesting those isn't supported yet",
        full.display()
    );
    ensure!(
        inner.audio.master_gain_db == Animatable::default() && inner.audio.loudness.is_none(),
        "unnest: {} has master audio settings that unnesting would drop",
        full.display()
    );
    for t in &inner.tracks {
        ensure!(
            t.audio == Default::default() && t.matte.is_none(),
            "unnest: inner track {:?} has bus or matte settings that unnesting would drop",
            t.name
        );
    }
    ensure!(
        inner.tracks.len() <= 1 || tl.tracks[ti].matte.is_none(),
        "unnest: track {:?} has a matte (the track above); unnesting a multi-track comp would change it",
        tl.tracks[ti].name
    );
    let comp_dir = dir_of(&full);
    let (w0, w1) = (c.source_in, c.source_in + c.duration);
    let off = c.start - c.source_in;
    tl.tracks[ti].clips.remove(ci);
    let mut placed = 0;
    let mut touched = vec![tr];
    for (k, it) in inner.tracks.iter().enumerate() {
        let target = if k == 0 {
            ti
        } else {
            let mut name = it.name.clone();
            let mut n = 2;
            while name.is_empty() || tl.track_names().any(|x| x == name) {
                name = format!("{}.{n}", it.name);
                n += 1;
            }
            add_track(tl, TrackKind::Video, &name, Some(ti + k))?;
            touched.push(TrackRef::Video(ti + k));
            ti + k
        };
        for ic in &it.clips {
            if ic.end() <= w0 || ic.start >= w1 {
                continue;
            }
            let mut n = ic.clone();
            if n.start < w0 {
                let d = w0 - n.start;
                n.source_in = ic.source_at(d);
                n.start = w0;
                n.duration = n.duration - d;
                n.shift_local_keys(-d.0)?;
                n.drop_transition_in();
                n.audio.in_offset = z();
                n.audio.fade_in = None;
            }
            if n.end() > w1 {
                n.duration = w1 - n.start;
                n.audio.out_offset = z();
                n.audio.fade_out = None;
            }
            n.audio.in_offset = n.audio.in_offset.max(w0 - n.start);
            n.audio.out_offset = n.audio.out_offset.min(w1 - n.end());
            n.start = n.start + off;
            if tl.clip_ids().any(|x| x == n.id) {
                n.id = unique_id(tl, &n.id);
            }
            n.source = rebase_source(&n.source, &comp_dir, &media.base);
            tl.tracks[target].clips.push(n);
            placed += 1;
        }
    }
    Ok((
        Change {
            op: 0,
            kind: "unnest",
            summary: format!(
                "unnest {clip}: {placed} clip(s) from {} back onto track {:?}{}",
                c.source.display(),
                tl.tracks[ti].name,
                if inner.tracks.len() > 1 {
                    format!(" and {} new track(s) above it", inner.tracks.len() - 1)
                } else {
                    String::new()
                }
            ),
            span: (c.start, c.end()),
        },
        touched,
    ))
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
        let (mut ch, touched) = apply_one(&mut out, op, media).with_context(ctx)?;
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
