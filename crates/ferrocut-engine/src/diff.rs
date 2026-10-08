//! Structured, agent-readable timeline diffs.
//!
//! [`diff`] compares two timelines field by field: settings, tracks, and
//! clips matched by id (added / removed / changed, with classification tags
//! such as `moved`, `trimmed_in`, `trimmed_out`, `slipped`, `track_changed`,
//! `keyframes_changed`), keyframe-level changes matched by key time, and the
//! timeline spans whose output may change. [`render_impact`] adds exactly
//! which output chunks would re-render, by comparing chunk keys (the render
//! cache is content-addressed by those keys, so this is what a render will
//! actually redo; audio is re-mixed on every render).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use ferrocut_core::RationalTime;
use serde::Serialize;
use serde_json::Value;

use crate::project::{dir_of, read_timeline, resolved, timeline_hash};
use crate::timeline::Timeline;
use crate::{compile, plan};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FrameRange {
    /// `[start_frame, end_frame)`.
    pub start_frame: i64,
    pub end_frame: i64,
    /// The same range in seconds (exact rationals).
    pub start: RationalTime,
    pub end: RationalTime,
}

/// Which output chunks a render of `b` would redo given a cached render of `a`.
#[derive(Clone, Debug, Serialize)]
pub struct RenderImpact {
    pub total_chunks: usize,
    pub total_frames: i64,
    pub chunk_frames: i64,
    /// Chunks of `b` whose key no render of `a` produced.
    pub dirty_chunks: Vec<usize>,
    pub dirty_frames: i64,
    pub reused_chunks: usize,
    /// Dirty chunks merged into contiguous ranges.
    pub ranges: Vec<FrameRange>,
    pub audio: &'static str,
}

/// Plan both timelines (sources resolved; media files must exist, they are
/// hashed) and compare chunk keys.
pub fn render_impact(a: &Timeline, b: &Timeline) -> anyhow::Result<RenderImpact> {
    let pa = plan(a, &compile(a)?);
    let pb = plan(b, &compile(b)?);
    let old: HashSet<&str> = pa.iter().map(|p| p.key.as_str()).collect();
    let dirty: Vec<_> = pb
        .iter()
        .filter(|p| !old.contains(p.key.as_str()))
        .collect();
    let fps = b.output.fps;
    let mut ranges: Vec<FrameRange> = Vec::new();
    for p in &dirty {
        let (s, e) = (p.start_frame, p.start_frame + p.frames);
        match ranges.last_mut() {
            Some(r) if r.end_frame == s => {
                r.end_frame = e;
                r.end = RationalTime::from_frames(e, fps);
            }
            _ => ranges.push(FrameRange {
                start_frame: s,
                end_frame: e,
                start: RationalTime::from_frames(s, fps),
                end: RationalTime::from_frames(e, fps),
            }),
        }
    }
    Ok(RenderImpact {
        total_chunks: pb.len(),
        total_frames: b.frame_count(),
        chunk_frames: b.chunk_frames(),
        dirty_chunks: dirty.iter().map(|p| p.index).collect(),
        dirty_frames: dirty.iter().map(|p| p.frames).sum(),
        reused_chunks: pb.len() - dirty.len(),
        ranges,
        audio: "audio is re-mixed on every render (not chunk-cached)",
    })
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct KeyChange {
    pub t: Value,
    pub from: Value,
    pub to: Value,
}

/// Keyframes matched by key time `t`.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct KeyframeDiff {
    pub added: Vec<Value>,
    pub removed: Vec<Value>,
    pub changed: Vec<KeyChange>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct FieldChange {
    /// Dotted path, e.g. `transform.position[0]`, `audio.gain_db`.
    pub path: String,
    /// `null` when absent (defaulted) on that side.
    pub from: Value,
    pub to: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframes: Option<KeyframeDiff>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ClipTimes {
    pub start: RationalTime,
    pub end: RationalTime,
    pub source_in: RationalTime,
    pub duration: RationalTime,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ClipChange {
    pub id: String,
    /// `video` (track clip, may carry audio) or `audio` (audio-track clip).
    pub kind: &'static str,
    /// `added`, `removed` or `changed`.
    pub change: &'static str,
    /// Track in `b` (or in `a` if removed).
    pub track: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_track: Option<String>,
    /// `moved`, `trimmed_in`, `trimmed_out`, `slipped`, `retimed`,
    /// `track_changed`, `reordered`, `source_changed`, `opacity_changed`,
    /// `transform_changed`, `generator_changed`, `three_d_changed`,
    /// `motion_blur_changed`, `markers_changed` (no output change),
    /// `transition_changed`, `audio_changed`,
    /// `keyframes_changed`.
    pub tags: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<ClipTimes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<ClipTimes>,
    pub fields: Vec<FieldChange>,
    /// `[start, end)` timeline span whose output (video or audio) may change.
    pub span: (RationalTime, RationalTime),
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TrackChange {
    pub kind: &'static str,
    pub name: String,
    /// `added`, `removed`, `changed` (track-level fields) or `reordered`.
    pub change: &'static str,
    pub fields: Vec<FieldChange>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct DiffSummary {
    pub clips_added: usize,
    pub clips_removed: usize,
    pub clips_changed: usize,
    pub moved: usize,
    pub trimmed: usize,
    pub slipped: usize,
    pub keyframe_changes: usize,
    pub settings_changed: usize,
    pub tracks_changed: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct TimelineDiff {
    pub identical: bool,
    pub a_hash: String,
    pub b_hash: String,
    pub summary: DiffSummary,
    pub settings: Vec<FieldChange>,
    pub tracks: Vec<TrackChange>,
    pub clips: Vec<ClipChange>,
    /// Union of every change's span, merged.
    pub affected: Vec<(RationalTime, RationalTime)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<RenderImpact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_error: Option<String>,
}

fn flatten(v: &Value, path: String, out: &mut BTreeMap<String, Value>) {
    match v {
        Value::Object(m) if !m.is_empty() && !m.contains_key("keyframes") => {
            for (k, x) in m {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                flatten(x, p, out);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            for (i, x) in a.iter().enumerate() {
                flatten(x, format!("{path}[{i}]"), out);
            }
        }
        v => {
            out.insert(path, v.clone());
        }
    }
}

fn keyframe_diff(a: &Value, b: &Value) -> Option<KeyframeDiff> {
    let keys = |v: &Value| -> Option<Vec<Value>> { v.get("keyframes")?.as_array().cloned() };
    let (ka, kb) = (keys(a)?, keys(b)?);
    let by_t = |ks: &[Value]| -> BTreeMap<String, Value> {
        ks.iter()
            .map(|k| {
                (
                    k.get("t").map(|t| t.to_string()).unwrap_or_default(),
                    k.clone(),
                )
            })
            .collect()
    };
    let (ma, mb) = (by_t(&ka), by_t(&kb));
    let mut d = KeyframeDiff::default();
    for (t, k) in &ma {
        match mb.get(t) {
            None => d.removed.push(k.clone()),
            Some(k2) if k2 != k => d.changed.push(KeyChange {
                t: k.get("t").cloned().unwrap_or(Value::Null),
                from: k.clone(),
                to: k2.clone(),
            }),
            _ => {}
        }
    }
    for (t, k) in &mb {
        if !ma.contains_key(t) {
            d.added.push(k.clone());
        }
    }
    Some(d)
}

fn field_changes(a: &Value, b: &Value, skip: &[&str]) -> Vec<FieldChange> {
    let (mut fa, mut fb) = (BTreeMap::new(), BTreeMap::new());
    flatten(a, String::new(), &mut fa);
    flatten(b, String::new(), &mut fb);
    let paths: BTreeSet<&String> = fa.keys().chain(fb.keys()).collect();
    let mut out = Vec::new();
    for p in paths {
        let root = p.split(['.', '[']).next().unwrap_or("");
        if skip.contains(&root) {
            continue;
        }
        let (x, y) = (
            fa.get(p).cloned().unwrap_or(Value::Null),
            fb.get(p).cloned().unwrap_or(Value::Null),
        );
        if x != y {
            out.push(FieldChange {
                path: p.clone(),
                keyframes: keyframe_diff(&x, &y),
                from: x,
                to: y,
            });
        }
    }
    out
}

struct ClipInfo {
    kind: &'static str,
    track: String,
    index: usize,
    value: Value,
    times: ClipTimes,
    /// Video extent and audio region, unioned.
    extent: (RationalTime, RationalTime),
}

fn clips(tl: &Timeline) -> BTreeMap<String, ClipInfo> {
    let mut m = BTreeMap::new();
    for t in &tl.tracks {
        for (i, c) in t.clips.iter().enumerate() {
            let (a0, a1) = c.audio_region();
            m.insert(
                c.id.clone(),
                ClipInfo {
                    kind: "video",
                    track: t.name.clone(),
                    index: i,
                    value: serde_json::to_value(c).expect("clip json"),
                    times: ClipTimes {
                        start: c.start,
                        end: c.end(),
                        source_in: c.source_in,
                        duration: c.duration,
                    },
                    extent: (c.start.min(a0), c.end().max(a1)),
                },
            );
        }
    }
    for t in &tl.audio_tracks {
        for (i, c) in t.clips.iter().enumerate() {
            let (a0, a1) = c.audio_region();
            m.insert(
                c.id.clone(),
                ClipInfo {
                    kind: "audio",
                    track: t.name.clone(),
                    index: i,
                    value: serde_json::to_value(c).expect("clip json"),
                    times: ClipTimes {
                        start: c.start,
                        end: c.end(),
                        source_in: c.source_in,
                        duration: c.duration,
                    },
                    extent: (c.start.min(a0), c.end().max(a1)),
                },
            );
        }
    }
    m
}

fn classify(a: &ClipTimes, b: &ClipTimes, tags: &mut Vec<&'static str>) {
    let z = RationalTime::ZERO;
    let ds = b.start - a.start;
    let de = b.end - a.end;
    let di = b.source_in - a.source_in;
    if ds == z && de == z && di == z {
        return;
    }
    if ds == z && de == z {
        tags.push("slipped");
    } else if ds == de && di == z {
        tags.push("moved");
    } else if ds != z && ds == di && de == z {
        tags.push("trimmed_in");
    } else if ds == z && di == z {
        tags.push("trimmed_out");
    } else if ds == di {
        tags.push("trimmed_in");
        tags.push("trimmed_out");
    } else {
        tags.push("retimed");
    }
}

fn merge_spans(mut v: Vec<(RationalTime, RationalTime)>) -> Vec<(RationalTime, RationalTime)> {
    v.retain(|s| s.1 > s.0);
    v.sort();
    let mut out: Vec<(RationalTime, RationalTime)> = Vec::new();
    for s in v {
        match out.last_mut() {
            Some(l) if s.0 <= l.1 => l.1 = l.1.max(s.1),
            _ => out.push(s),
        }
    }
    out
}

fn track_values(tl: &Timeline) -> Vec<(&'static str, String, Value)> {
    let strip = |mut v: Value| {
        if let Some(m) = v.as_object_mut() {
            m.remove("clips");
        }
        v
    };
    tl.tracks
        .iter()
        .map(|t| {
            (
                "video",
                t.name.clone(),
                strip(serde_json::to_value(t).unwrap()),
            )
        })
        .chain(tl.audio_tracks.iter().map(|t| {
            (
                "audio",
                t.name.clone(),
                strip(serde_json::to_value(t).unwrap()),
            )
        }))
        .collect()
}

/// Diff two timelines (sources compared as given; see [`diff_files`]).
pub fn diff(a: &Timeline, b: &Timeline) -> TimelineDiff {
    let whole = (RationalTime::ZERO, a.duration().max(b.duration()));
    let mut spans = Vec::new();
    let mut summary = DiffSummary::default();

    // Settings: everything but tracks.
    let strip = |tl: &Timeline| {
        let mut v = serde_json::to_value(tl).unwrap();
        if let Some(m) = v.as_object_mut() {
            m.remove("tracks");
            m.remove("audio_tracks");
        }
        v
    };
    let settings = field_changes(&strip(a), &strip(b), &[]);
    // Name and markers never change the output.
    let annotation = |path: &str| path.split(['.', '[']).next() == Some("markers");
    if settings
        .iter()
        .any(|f| f.path != "name" && !annotation(&f.path))
    {
        spans.push(whole);
    }
    summary.settings_changed = settings.len();
    summary.keyframe_changes += settings.iter().filter(|f| f.keyframes.is_some()).count();

    // Tracks (by kind + name).
    let (ta, tb) = (track_values(a), track_values(b));
    let mut tracks = Vec::new();
    for (k, n, v) in &ta {
        match tb.iter().find(|(k2, n2, _)| k2 == k && n2 == n) {
            None => tracks.push(TrackChange {
                kind: k,
                name: n.clone(),
                change: "removed",
                fields: vec![],
            }),
            Some((_, _, v2)) => {
                let f = field_changes(v, v2, &["name"]);
                if !f.is_empty() {
                    summary.keyframe_changes += f.iter().filter(|f| f.keyframes.is_some()).count();
                    spans.push(whole);
                    tracks.push(TrackChange {
                        kind: k,
                        name: n.clone(),
                        change: "changed",
                        fields: f,
                    });
                }
            }
        }
    }
    for (k, n, _) in &tb {
        if !ta.iter().any(|(k2, n2, _)| k2 == k && n2 == n) {
            tracks.push(TrackChange {
                kind: k,
                name: n.clone(),
                change: "added",
                fields: vec![],
            });
        }
    }
    let order = |t: &[(&'static str, String, Value)], kind: &str| -> Vec<String> {
        t.iter()
            .filter(|x| x.0 == kind)
            .map(|x| x.1.clone())
            .collect()
    };
    for kind in ["video", "audio"] {
        let (oa, ob) = (order(&ta, kind), order(&tb, kind));
        let common_a: Vec<_> = oa.iter().filter(|n| ob.contains(n)).collect();
        let common_b: Vec<_> = ob.iter().filter(|n| oa.contains(n)).collect();
        if common_a != common_b {
            spans.push(whole);
            tracks.push(TrackChange {
                kind: if kind == "video" { "video" } else { "audio" },
                name: common_b
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                change: "reordered",
                fields: vec![],
            });
        }
    }
    summary.tracks_changed = tracks.len();

    // Clips by id.
    let (ca, cb) = (clips(a), clips(b));
    let mut out = Vec::new();
    for (id, x) in &ca {
        match cb.get(id) {
            None => {
                summary.clips_removed += 1;
                spans.push(x.extent);
                out.push(ClipChange {
                    id: id.clone(),
                    kind: x.kind,
                    change: "removed",
                    track: x.track.clone(),
                    from_track: None,
                    tags: vec![],
                    from: Some(x.times.clone()),
                    to: None,
                    fields: vec![],
                    span: x.extent,
                });
            }
            Some(y) => {
                let fields = field_changes(&x.value, &y.value, &["id"]);
                let mut tags = Vec::new();
                classify(&x.times, &y.times, &mut tags);
                let track_changed = x.track != y.track || x.kind != y.kind;
                if track_changed {
                    tags.push("track_changed");
                }
                // Same track, same times, different position in the clip list.
                let reordered = !track_changed && x.index != y.index && fields.is_empty();
                for (root, tag) in [
                    ("source", "source_changed"),
                    ("opacity", "opacity_changed"),
                    ("transform", "transform_changed"),
                    ("generator", "generator_changed"),
                    ("three_d", "three_d_changed"),
                    ("motion_blur", "motion_blur_changed"),
                    ("markers", "markers_changed"),
                    ("transition_in", "transition_changed"),
                    ("audio", "audio_changed"),
                ] {
                    if fields
                        .iter()
                        .any(|f| f.path.split(['.', '[']).next() == Some(root))
                    {
                        tags.push(tag);
                    }
                }
                let kf = fields.iter().filter(|f| f.keyframes.is_some()).count();
                if kf > 0 {
                    tags.push("keyframes_changed");
                    summary.keyframe_changes += kf;
                }
                if fields.is_empty() && !track_changed && !reordered {
                    continue;
                }
                if reordered && fields.is_empty() {
                    // No output change: list position only.
                    tags.push("reordered");
                }
                summary.clips_changed += 1;
                summary.moved += tags.contains(&"moved") as usize;
                summary.trimmed +=
                    (tags.contains(&"trimmed_in") || tags.contains(&"trimmed_out")) as usize;
                summary.slipped += tags.contains(&"slipped") as usize;
                let span = (x.extent.0.min(y.extent.0), x.extent.1.max(y.extent.1));
                let only_markers = !fields.is_empty() && fields.iter().all(|f| annotation(&f.path));
                if !(reordered && fields.is_empty()) && !(only_markers && !track_changed) {
                    spans.push(span);
                }
                out.push(ClipChange {
                    id: id.clone(),
                    kind: y.kind,
                    change: "changed",
                    track: y.track.clone(),
                    from_track: track_changed.then(|| x.track.clone()),
                    tags,
                    from: Some(x.times.clone()),
                    to: Some(y.times.clone()),
                    fields,
                    span,
                });
            }
        }
    }
    for (id, y) in &cb {
        if !ca.contains_key(id) {
            summary.clips_added += 1;
            spans.push(y.extent);
            out.push(ClipChange {
                id: id.clone(),
                kind: y.kind,
                change: "added",
                track: y.track.clone(),
                from_track: None,
                tags: vec![],
                from: None,
                to: Some(y.times.clone()),
                fields: vec![],
                span: y.extent,
            });
        }
    }
    let (ha, hb) = (timeline_hash(a), timeline_hash(b));
    TimelineDiff {
        identical: ha == hb,
        a_hash: ha,
        b_hash: hb,
        summary,
        settings,
        tracks,
        clips: out,
        affected: merge_spans(spans),
        render: None,
        render_error: None,
    }
}

fn normalized(tl: &Timeline, dir: &Path) -> Timeline {
    let mut t = resolved(tl, dir);
    for s in t.sources_mut() {
        if let Ok(c) = std::fs::canonicalize(&*s) {
            *s = c;
        }
    }
    for s in t.assets_mut() {
        if let Ok(c) = std::fs::canonicalize(&*s) {
            *s = c;
        }
    }
    t
}

/// Diff two timeline files. Sources are resolved against each file's
/// directory first (so the same media referenced differently isn't a change);
/// hashes are of the files as written. With `with_render`, also computes the
/// [`RenderImpact`] (needs the media; on failure `render_error` says why).
pub fn diff_files(a: &Path, b: &Path, with_render: bool) -> anyhow::Result<TimelineDiff> {
    let (ta, tb) = (read_timeline(a)?, read_timeline(b)?);
    let (na, nb) = (normalized(&ta, &dir_of(a)), normalized(&tb, &dir_of(b)));
    let mut d = diff(&na, &nb);
    d.a_hash = timeline_hash(&ta);
    d.b_hash = timeline_hash(&tb);
    d.identical = d.a_hash == d.b_hash
        || (d.settings.is_empty() && d.tracks.is_empty() && d.clips.is_empty());
    if with_render {
        match render_impact(&na, &nb) {
            Ok(r) => d.render = Some(r),
            Err(e) => d.render_error = Some(format!("{e:#}")),
        }
    }
    Ok(d)
}
