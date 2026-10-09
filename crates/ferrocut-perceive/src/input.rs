//! What perceive reads from the engine, mirrored as plain serde types so this
//! crate never depends on `ferrocut-engine` (the engine can then depend on
//! it to run analysis after each render). Unknown fields are ignored on
//! purpose: engine additions don't break perceive.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use ferrocut_core::{FrameRate, RationalTime};
use serde::{Deserialize, Serialize};

/// One rendered chunk: `<chunk_dir>/<key>.mkv`, an FFV1 BGRZ master.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub index: usize,
    pub start_frame: i64,
    pub frames: i64,
    /// The engine's chunk key (hash of the chunk's frame Merkle keys + encoder).
    pub key: String,
    /// blake3 of the chunk's mixed audio, when the timeline has audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_blake3: Option<String>,
}

/// The subset of the engine's render report (`<output>.report.json`) we use.
#[derive(Clone, Debug, Deserialize)]
pub struct RenderReport {
    /// The rendered master (video + PCM audio), as the engine was given it.
    #[serde(default)]
    pub output: Option<PathBuf>,
    pub total_frames: i64,
    pub chunk_frames: i64,
    pub chunks: Vec<ChunkRef>,
    /// Where the engine cached this render's chunk masters (one dir per GPU
    /// adapter); absent in older reports (`<cache>/chunks/`).
    #[serde(default)]
    pub chunk_dir: Option<PathBuf>,
    /// Present when the master carries audio.
    #[serde(default)]
    pub audio: Option<EngineAudio>,
    /// Filled by the checker after [`resolve_chunk_dir`]. Not in the engine JSON.
    #[serde(skip)]
    pub chunk_resolution: Option<ChunkDirResolution>,
}

/// The engine's audio summary for the master.
#[derive(Clone, Debug, Deserialize)]
pub struct EngineAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: i64,
    /// blake3 of the master's interleaved f32le samples (the PCM stream bytes).
    pub blake3: String,
    /// The engine's own measurement (libebur128 port), for cross-checks.
    #[serde(default)]
    pub output: Option<EngineMeasurement>,
    /// The engine's mix analysis, kept as JSON: `target_lufs` and
    /// `true_peak_ceiling_dbtp` are what the master was normalized and limited
    /// to (`null` when it had no loudness target; absent in older reports).
    #[serde(default)]
    pub analysis: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EngineMeasurement {
    /// `null` (JSON has no -inf) for silence.
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub sample_peak_dbfs: Option<f64>,
}

impl RenderReport {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let t =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_json(&t).with_context(|| format!("parsing render report {}", path.display()))
    }
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }
}

/// Where [`resolve_chunk_dir`] found the chunk masters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkDirResolution {
    pub dir: Option<PathBuf>,
    pub resolved_by: ChunkDirSource,
    /// Every candidate considered, including ones that do not exist.
    pub tried: Vec<PathBuf>,
}

/// How a render report's `chunk_dir` was turned into a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkDirSource {
    Recorded,
    ReportLocation,
    ProcessCwd,
    /// Explicit `--cache-dir` (`cache_dir/chunks/<tag>`, else `cache_dir/chunks`)
    /// when the rule-2 directory is missing.
    CacheDirOverride,
    /// The engine's default cache next to the report (`<report dir>/.ferrocut-cache`,
    /// `chunks/<tag>` then `chunks`) when nothing earlier exists: a tree moved
    /// together with its report, or a report without `chunk_dir`.
    ReportDefaultCache,
    Ambiguous,
    Unresolved,
    Absent,
}

impl std::fmt::Display for ChunkDirSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Recorded => "recorded",
            Self::ReportLocation => "report_location",
            Self::ProcessCwd => "process_cwd",
            Self::CacheDirOverride => "cache_dir_override",
            Self::ReportDefaultCache => "report_default_cache",
            Self::Ambiguous => "ambiguous",
            Self::Unresolved => "unresolved",
            Self::Absent => "absent",
        })
    }
}

/// Fold `.` and `..` without touching the filesystem.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Copy the last component first so `pop` is not borrowed.
                let last = out.components().next_back().map(|c| match c {
                    Component::Normal(_) => 0,
                    Component::ParentDir => 1,
                    _ => 2,
                });
                match last {
                    Some(0) => {
                        out.pop();
                    }
                    Some(1) | None => out.push(".."),
                    _ => {}
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn normal_components(path: &Path) -> Vec<OsString> {
    lexical_normalize(path)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_os_string()),
            _ => None,
        })
        .collect()
}

fn ends_with_normals(dir: &Path, suffix: &[OsString]) -> bool {
    if suffix.is_empty() {
        return true;
    }
    let normals: Vec<&std::ffi::OsStr> = dir
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    normals.len() >= suffix.len()
        && normals[normals.len() - suffix.len()..]
            .iter()
            .zip(suffix)
            .all(|(a, b)| *a == b.as_os_str())
}

fn strip_trailing_normals(dir: &Path, suffix: &[OsString]) -> PathBuf {
    if suffix.is_empty() {
        return dir.to_path_buf();
    }
    let mut comps: Vec<Component<'_>> = dir.components().collect();
    let mut rest = suffix.len();
    while rest > 0 {
        match comps.pop() {
            Some(Component::Normal(s)) if s == suffix[rest - 1].as_os_str() => rest -= 1,
            _ => return dir.to_path_buf(),
        }
    }
    let mut out = PathBuf::new();
    for c in comps {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => lexical_normalize(a) == lexical_normalize(b),
    }
}

/// When rule 2 did not yield an existing directory: the explicit `--cache-dir`
/// candidates, then the engine's default cache next to the report. Chunk files
/// are named by content key, so a directory found here cannot hold a different
/// chunk under the same name. `chunk` is the report's `chunk_dir` (its file name
/// is the adapter tag), when present.
fn apply_cache_fallbacks(
    chunk: Option<&Path>,
    cache_dir: Option<&Path>,
    report_dir: &Path,
    mut res: ChunkDirResolution,
) -> ChunkDirResolution {
    if res.resolved_by == ChunkDirSource::Ambiguous {
        return res;
    }
    if res.dir.as_ref().is_some_and(|d| d.is_dir()) {
        return res;
    }
    let default_cache = report_dir.join(".ferrocut-cache");
    let fallbacks = cache_dir
        .map(|c| (c.to_path_buf(), ChunkDirSource::CacheDirOverride))
        .into_iter()
        .chain([(default_cache, ChunkDirSource::ReportDefaultCache)]);
    for (cache, source) in fallbacks {
        let mut cands = Vec::new();
        if let Some(tag) = chunk.and_then(Path::file_name) {
            cands.push(lexical_normalize(&cache.join("chunks").join(tag)));
        }
        cands.push(lexical_normalize(&cache.join("chunks")));
        for cand in cands {
            let exists = cand.is_dir();
            res.tried.push(cand.clone());
            if exists {
                res.dir = Some(cand);
                res.resolved_by = source;
                return res;
            }
        }
    }
    res
}

/// Effective chunk directory a checker should open.
///
/// Rule 2: relative `chunk_dir` values are resolved from the render's working
/// directory, recovered from the report path when the report still has the
/// engine's default name (`<output stem>.report.json`) and sits under
/// `output`'s parent. That name match is an inference: a report copied into
/// another directory that happens to be named `<stem>.report.json` still
/// produces a report-location candidate, which loses to an existing process-cwd
/// candidate and is `Ambiguous` when both exist and differ. The checker's own
/// cwd is a separate candidate. Joining the report directory onto a `chunk_dir`
/// that already contains the output directory would double that prefix, so that
/// join is never done.
///
/// `cache_dir` is the explicit `--cache-dir` only. When rule 2's directory is
/// missing (including a recorded absolute path whose tree was relocated), or
/// the report has no `chunk_dir`, the first existing of
/// `cache_dir/chunks/<chunk_dir file name>` and `cache_dir/chunks` wins as
/// [`ChunkDirSource::CacheDirOverride`], then the same two under
/// `<report dir>/.ferrocut-cache` as [`ChunkDirSource::ReportDefaultCache`]
/// (the engine's default cache location, kept from the previous lookup).
/// Absent stays Absent only when none of those exist.
pub fn resolve_chunk_dir(
    rr: &RenderReport,
    report_path: &Path,
    cwd: &Path,
    cache_dir: Option<&Path>,
) -> ChunkDirResolution {
    let report_dir = lexical_normalize(&cwd.join(report_path))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let Some(chunk) = rr.chunk_dir.as_ref() else {
        return apply_cache_fallbacks(
            None,
            cache_dir,
            &report_dir,
            ChunkDirResolution {
                dir: None,
                resolved_by: ChunkDirSource::Absent,
                tried: Vec::new(),
            },
        );
    };
    if chunk.is_absolute() {
        return apply_cache_fallbacks(
            Some(chunk),
            cache_dir,
            &report_dir,
            ChunkDirResolution {
                dir: Some(chunk.clone()),
                resolved_by: ChunkDirSource::Recorded,
                tried: vec![chunk.clone()],
            },
        );
    }

    let mut tried = Vec::new();
    let mut hits: Vec<(ChunkDirSource, PathBuf)> = Vec::new();
    if let Some(output) = rr.output.as_ref().filter(|p| p.is_relative()) {
        let expected = output.with_extension("report.json");
        if report_path.file_name() == expected.file_name() {
            let suffix = normal_components(output.parent().unwrap_or(Path::new("")));
            if ends_with_normals(&report_dir, &suffix) {
                let render_cwd = strip_trailing_normals(&report_dir, &suffix);
                let candidate = lexical_normalize(&render_cwd.join(chunk));
                tried.push(candidate.clone());
                if candidate.is_dir() {
                    hits.push((ChunkDirSource::ReportLocation, candidate));
                }
            }
        }
    }
    let process = lexical_normalize(&cwd.join(chunk));
    tried.push(process.clone());
    if process.is_dir() {
        hits.push((ChunkDirSource::ProcessCwd, process));
    }

    let res = match hits.as_slice() {
        [] => ChunkDirResolution {
            dir: None,
            resolved_by: ChunkDirSource::Unresolved,
            tried,
        },
        [one] => ChunkDirResolution {
            dir: Some(one.1.clone()),
            resolved_by: one.0,
            tried,
        },
        [a, b] => {
            if same_dir(&a.1, &b.1) {
                ChunkDirResolution {
                    dir: Some(a.1.clone()),
                    resolved_by: a.0,
                    tried,
                }
            } else {
                ChunkDirResolution {
                    dir: None,
                    resolved_by: ChunkDirSource::Ambiguous,
                    tried,
                }
            }
        }
        _ => ChunkDirResolution {
            dir: None,
            resolved_by: ChunkDirSource::Ambiguous,
            tried,
        },
    };
    apply_cache_fallbacks(Some(chunk), cache_dir, &report_dir, res)
}

/// The subset of the engine's timeline JSON we use.
#[derive(Clone, Debug, Deserialize)]
pub struct Timeline {
    #[serde(default)]
    pub name: String,
    pub output: Output,
    pub tracks: Vec<Track>,
    /// The timeline's audio settings, kept as JSON (`loudness` is read).
    #[serde(default)]
    pub audio: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Output {
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
    #[serde(default)]
    pub duration: Option<RationalTime>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Track {
    pub clips: Vec<Clip>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Clip {
    pub id: String,
    pub start: RationalTime,
    pub duration: RationalTime,
    #[serde(default)]
    pub transition_in: Option<Transition>,
    /// Constant (`"3/4"`, `0.5`) or `{"keyframes": [{"t", "v", ...}]}` with
    /// `t` relative to the clip start. Only fades from/to 0 at the clip's
    /// edges matter here (they are gradual transitions, not cuts).
    #[serde(default)]
    pub opacity: Option<serde_json::Value>,
}

fn num(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => match s.split_once('/') {
            Some((a, b)) => Some(a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?),
            None => s.trim().parse().ok(),
        },
        _ => None,
    }
}

impl Clip {
    /// Opacity keyframes as `(t, value)`, when animated.
    fn keyframes(&self) -> Vec<(RationalTime, f64)> {
        let Some(kf) = self
            .opacity
            .as_ref()
            .and_then(|o| o.get("keyframes"))
            .and_then(|k| k.as_array())
        else {
            return Vec::new();
        };
        kf.iter()
            .filter_map(|k| {
                let t: RationalTime = serde_json::from_value(k.get("t")?.clone()).ok()?;
                Some((t, num(k.get("v")?)?))
            })
            .collect()
    }
    /// Fade-in from opacity 0 at the clip start: when it reaches non-zero.
    pub fn fade_in(&self) -> Option<RationalTime> {
        let k = self.keyframes();
        let first = k.first()?;
        (first.0 <= RationalTime::default() && first.1 == 0.0)
            .then(|| k.iter().find(|x| x.1 != 0.0).map(|x| x.0))
            .flatten()
    }
    /// Fade-out to opacity 0 at the clip end: when it leaves non-zero.
    pub fn fade_out(&self) -> Option<RationalTime> {
        let k = self.keyframes();
        let last = k.last()?;
        (last.0 >= self.duration && last.1 == 0.0)
            .then(|| k.iter().rev().find(|x| x.1 != 0.0).map(|x| x.0))
            .flatten()
    }
}

/// Any transition kind (today the engine has `dissolve`); every kind is
/// treated as a gradual picture change over `duration`.
#[derive(Clone, Debug, Deserialize)]
pub struct Transition {
    pub kind: String,
    pub duration: RationalTime,
}

impl Timeline {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let t =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_json(&t).with_context(|| format!("parsing timeline {}", path.display()))
    }
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }
    /// `audio.loudness`: (target LUFS, true-peak ceiling dBTP, default -1),
    /// when the timeline asks the engine to normalize.
    pub fn loudness(&self) -> Option<(f64, f64)> {
        let l = self.audio.as_ref()?.get("loudness")?;
        let tp = l.get("true_peak_dbtp").map_or(Some(-1.0), num)?;
        Some((num(l.get("target_lufs")?)?, tp))
    }
    pub fn duration(&self) -> RationalTime {
        self.output.duration.unwrap_or_else(|| {
            self.tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(|c| c.start + c.duration))
                .max()
                .unwrap_or_default()
        })
    }
    /// First frame at or after `t` (the engine renders frame i at i / fps).
    pub fn frame_at(&self, t: RationalTime) -> i64 {
        t.frame_ceil(self.output.fps)
    }

    /// Where the timeline intends the picture to change: hard cuts (first
    /// frame of the new shot) and dissolves (`[start, end)` frames), clipped to
    /// `(0, total)`. A clip boundary covered by an opaque dissolve-in is not
    /// a cut. Boundaries hidden by upper tracks still count (we can't know
    /// what's opaque), so `missed` cuts may be invisible-by-design edits.
    pub fn intended(&self, total: i64) -> Intended {
        let mut cuts = Vec::new();
        let mut dissolves = Vec::new();
        for t in &self.tracks {
            let mut clips: Vec<&Clip> = t.clips.iter().collect();
            clips.sort_by_key(|c| c.start);
            for (i, c) in clips.iter().enumerate() {
                let s = self.frame_at(c.start);
                match (&c.transition_in, c.fade_in()) {
                    (Some(Transition { duration, .. }), _) => {
                        dissolves.push((s, self.frame_at(c.start + *duration)));
                    }
                    (None, Some(t)) => dissolves.push((s, self.frame_at(c.start + t))),
                    (None, None) => cuts.push(s),
                }
                let end = c.start + c.duration;
                if let Some(t) = c.fade_out() {
                    dissolves.push((self.frame_at(c.start + t), self.frame_at(end)));
                    continue;
                }
                let covered = clips.get(i + 1).is_some_and(|n| match &n.transition_in {
                    Some(Transition { duration, .. }) => {
                        n.start <= end && end <= n.start + n.duration && n.start + *duration <= end
                    }
                    None => false,
                });
                if !covered {
                    cuts.push(self.frame_at(end));
                }
            }
        }
        cuts.retain(|&f| f > 0 && f < total);
        cuts.sort();
        cuts.dedup();
        dissolves.retain(|&(s, e)| e > s && s < total);
        dissolves.sort();
        dissolves.dedup();
        // A cut that falls inside an intended dissolve is part of it.
        cuts.retain(|&f| !dissolves.iter().any(|&(s, e)| f >= s && f <= e));
        Intended { cuts, dissolves }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intended {
    pub cuts: Vec<i64>,
    /// `(start, end)` frames, end exclusive.
    pub dissolves: Vec<(i64, i64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intended_cuts_and_dissolves() {
        // Engine's own demo timeline (render.rs tests).
        let tl = Timeline::from_json(
            r#"{ "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
              "tracks": [
                { "clips": [
                  { "id": "a", "source": "a.mov", "start": 0, "duration": "2" },
                  { "id": "b", "source": "b.mov", "start": "3/2", "source_in": "1/2", "duration": "2",
                    "transition_in": { "kind": "dissolve", "duration": "1/2" } } ]},
                { "clips": [ { "id": "t", "source": "c.mov", "start": 1, "duration": "1/2", "opacity": "1/2" } ] }
              ] }"#,
        )
        .unwrap();
        assert_eq!(tl.duration(), RationalTime::new(7, 2));
        let i = tl.intended(84);
        // a ends at 48 inside b's dissolve [36, 48] -> no cut; t: 24..36.
        assert_eq!(i.dissolves, vec![(36, 48)]);
        assert_eq!(i.cuts, vec![24]); // 36 (t's end) is the dissolve start
    }

    #[test]
    fn opacity_fades_are_not_cuts() {
        // demo-av's logo: fades in over 1/2 s and out over the last 1/2 s.
        let tl = Timeline::from_json(
            r#"{ "output": { "width": 64, "height": 32, "fps": "24" },
              "tracks": [
                { "clips": [ { "id": "a", "start": 0, "duration": "13" } ] },
                { "clips": [ { "id": "logo", "start": "6", "duration": "3",
                    "opacity": { "keyframes": [
                      { "t": "0", "v": "0" }, { "t": "1/2", "v": "0.8" },
                      { "t": "5/2", "v": "0.8" }, { "t": "3", "v": 0 } ] } },
                  { "id": "t", "start": "10", "duration": "1", "opacity": "1/2" } ] }
              ] }"#,
        )
        .unwrap();
        let i = tl.intended(312);
        assert_eq!(i.dissolves, vec![(144, 156), (204, 216)]);
        assert_eq!(i.cuts, vec![240, 264]);
    }
}
