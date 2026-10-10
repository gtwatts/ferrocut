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
    /// Present for a selected-range render (engine `render --range-frames`):
    /// the master holds only part of the timeline, so it cannot be graded
    /// against the whole timeline. Kept raw; only its presence is used.
    #[serde(default)]
    pub range: Option<serde_json::Value>,
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
    /// The first search directory that exists (a recorded absolute directory is
    /// kept even when missing); `None` when ambiguous or nothing exists.
    pub dir: Option<PathBuf>,
    pub resolved_by: ChunkDirSource,
    /// Directories searched for each chunk file, in order. A chunk is read from
    /// the first one holding `<key>.mkv`, so a cache that evicted some keys
    /// still falls through to another holding them (the per-file lookup of
    /// earlier checkers). Empty when ambiguous without `--cache-dir`.
    pub search: Vec<(ChunkDirSource, PathBuf)>,
    /// Every candidate considered, including ones that do not exist.
    pub tried: Vec<PathBuf>,
}

impl ChunkDirResolution {
    /// The file chunk `key` is read from: the first search directory holding
    /// `<key>.mkv`, or `None` when no directory does.
    pub fn locate(&self, key: &str) -> anyhow::Result<Option<PathBuf>> {
        let name = chunk_file_name(key)?;
        Ok(self
            .search
            .iter()
            .map(|(_, d)| d.join(&name))
            .find(|p| p.exists()))
    }

    /// Whether chunk files can be looked up at all: false for an ambiguous or
    /// unresolved directory. Checked before any cache is read.
    pub fn is_usable(&self) -> bool {
        self.dir.is_some()
    }

    /// The error for a report whose chunks cannot be located, with the action
    /// that fixes it.
    pub fn error(&self, report: &Path) -> String {
        let tried = self
            .tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let advice = match self.resolved_by {
            ChunkDirSource::Ambiguous => {
                "the relative chunk_dir names a different existing directory from the report's \
                 location and from this working directory; pass --cache-dir <the render's cache \
                 dir> (its chunks/ is searched instead), or check from the render's working directory"
            }
            _ => {
                "pass --cache-dir <the render's cache dir>, or re-render (new reports record absolute paths)"
            }
        };
        format!(
            "chunk directory {} for {} (tried dirs: [{tried}]); {advice}",
            self.resolved_by,
            report.display()
        )
    }
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

/// The plain directory names of a relative `output`'s parent, or `None` when it
/// has a `..` or root: the report-location inference is then skipped rather
/// than folding `..` by text, which disagrees with the filesystem when the
/// component before it is a symlink.
fn plain_names(path: &Path) -> Option<Vec<OsString>> {
    path.components()
        .filter(|c| *c != Component::CurDir)
        .map(|c| match c {
            Component::Normal(s) => Some(s.to_os_string()),
            _ => None,
        })
        .collect()
}

/// `dir` without its last components when they are exactly the plain names
/// `suffix`; `None` otherwise.
fn strip_suffix_names(dir: &Path, suffix: &[OsString]) -> Option<PathBuf> {
    let comps: Vec<Component<'_>> = dir.components().collect();
    let keep = comps.len().checked_sub(suffix.len())?;
    let matches = comps[keep..]
        .iter()
        .zip(suffix)
        .all(|(c, s)| matches!(c, Component::Normal(n) if *n == s.as_os_str()));
    if !matches {
        return None;
    }
    let out: PathBuf = comps[..keep].iter().map(|c| c.as_os_str()).collect();
    Some(if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    })
}

/// Same directory on disk (aliases through symlinks or `..` count once).
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// Effective chunk directories a checker searches.
///
/// Paths are joined, never folded by text: `x/..` means what the filesystem
/// says, also when `x` is a symlink.
///
/// Rule 2: an absolute `chunk_dir` is used as recorded. A relative one is
/// resolved from the render's working directory, recovered from the report
/// path when the report still has the engine's default name
/// (`<output stem>.report.json`) and its directory ends with `output`'s parent
/// names. That name match is an inference: a report copied into another
/// directory under the same name still produces a report-location candidate.
/// The checker's own cwd is a separate candidate. When both exist and are
/// different directories the result is `Ambiguous`; only `--cache-dir` (its
/// candidates below) can then locate chunks.
///
/// After the rule-2 directory, in order: `cache/chunks/<tag>` and
/// `cache/chunks` for the explicit `--cache-dir`
/// ([`ChunkDirSource::CacheDirOverride`]); then the engine cache the rule-2
/// directory sits in (`<dir>/../..` when its parent is `chunks`), the default
/// the checker has always derived; then `<report dir>/.ferrocut-cache`
/// ([`ChunkDirSource::ReportDefaultCache`]: a tree moved with its report, or
/// a report without `chunk_dir`). Each chunk file is looked up through that
/// list ([`ChunkDirResolution::locate`]).
pub fn resolve_chunk_dir(
    rr: &RenderReport,
    report_path: &Path,
    cwd: &Path,
    cache_dir: Option<&Path>,
) -> ChunkDirResolution {
    let report_dir = cwd
        .join(report_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let chunk = rr.chunk_dir.as_deref();
    let mut tried = Vec::new();
    // The rule-2 directory and how it was found, or the reason there is none.
    let primary: Result<(ChunkDirSource, PathBuf), ChunkDirSource> = match chunk {
        None => Err(ChunkDirSource::Absent),
        Some(c) if c.is_absolute() => {
            tried.push(c.to_path_buf());
            Ok((ChunkDirSource::Recorded, c.to_path_buf()))
        }
        Some(c) => {
            let mut hits: Vec<(ChunkDirSource, PathBuf)> = Vec::new();
            if let Some(output) = rr.output.as_ref().filter(|p| p.is_relative()) {
                let expected = output.with_extension("report.json");
                let render_cwd = plain_names(output.parent().unwrap_or(Path::new("")))
                    .and_then(|names| strip_suffix_names(&report_dir, &names));
                if let (true, Some(render_cwd)) =
                    (report_path.file_name() == expected.file_name(), render_cwd)
                {
                    let candidate = render_cwd.join(c);
                    tried.push(candidate.clone());
                    if candidate.is_dir() {
                        hits.push((ChunkDirSource::ReportLocation, candidate));
                    }
                }
            }
            let process = cwd.join(c);
            tried.push(process.clone());
            if process.is_dir() {
                hits.push((ChunkDirSource::ProcessCwd, process));
            }
            match hits.as_slice() {
                [] => Err(ChunkDirSource::Unresolved),
                [one] => Ok(one.clone()),
                [a, b] if same_dir(&a.1, &b.1) => Ok(a.clone()),
                _ => Err(ChunkDirSource::Ambiguous),
            }
        }
    };
    let ambiguous = primary == Err(ChunkDirSource::Ambiguous);
    let tag = chunk.and_then(Path::file_name);
    let cache_cands = |cache: &Path| {
        let mut v = Vec::new();
        if let Some(tag) = tag {
            v.push(cache.join("chunks").join(tag));
        }
        v.push(cache.join("chunks"));
        v
    };

    let mut search: Vec<(ChunkDirSource, PathBuf)> = Vec::new();
    let mut push = |source: ChunkDirSource, dir: PathBuf, tried: &mut Vec<PathBuf>| {
        if !search.iter().any(|(_, d)| *d == dir) {
            if !tried.contains(&dir) {
                tried.push(dir.clone());
            }
            search.push((source, dir));
        }
    };
    if let Ok((source, dir)) = &primary {
        push(*source, dir.clone(), &mut tried);
    }
    if let Some(cache) = cache_dir {
        for d in cache_cands(cache) {
            push(ChunkDirSource::CacheDirOverride, d, &mut tried);
        }
    }
    // An ambiguous relative directory gets no implicit fallback: only an
    // explicit --cache-dir says which render's chunks to use.
    if !ambiguous {
        if let Ok((source, dir)) = &primary
            && let Some(engine_cache) = dir
                .parent()
                .filter(|p| p.file_name().is_some_and(|n| n == "chunks"))
                .and_then(Path::parent)
        {
            for d in cache_cands(engine_cache) {
                push(*source, d, &mut tried);
            }
        }
        for d in cache_cands(&report_dir.join(".ferrocut-cache")) {
            push(ChunkDirSource::ReportDefaultCache, d, &mut tried);
        }
    }

    let found = search.iter().find(|(_, d)| d.is_dir()).cloned();
    let (dir, resolved_by) = match (found, primary) {
        (Some((source, d)), _) => (Some(d), source),
        // A recorded absolute directory stays the answer when nothing exists;
        // opening a chunk then names it.
        (None, Ok((ChunkDirSource::Recorded, d))) => (Some(d), ChunkDirSource::Recorded),
        (None, Ok(_)) => (None, ChunkDirSource::Unresolved),
        (None, Err(reason)) => (None, reason),
    };
    ChunkDirResolution {
        dir,
        resolved_by,
        search,
        tried,
    }
}

/// A chunk master's file name from its report key. The engine writes keys as
/// hex; anything else (a path separator, `..`) is refused rather than joined.
pub fn chunk_file_name(key: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !key.is_empty() && key.len() <= 128 && key.bytes().all(|b| b.is_ascii_hexdigit()),
        "render report chunk key {key:?} is not a hex chunk key"
    );
    Ok(format!("{key}.mkv"))
}

/// Default engine cache for a check: the cache the resolved chunk dir sits in
/// (its grandparent when its parent is `chunks`), else
/// `<report dir>/.ferrocut-cache`.
pub fn default_cache_dir(resolution: &ChunkDirResolution, report_dir: &Path) -> PathBuf {
    resolution
        .dir
        .as_deref()
        .and_then(|d| d.parent())
        .filter(|p| p.file_name().is_some_and(|n| n == "chunks"))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| report_dir.join(".ferrocut-cache"))
}

/// The master whose audio a check measures. Pointed at a master (no explicit
/// report), that file; otherwise the report's `output`: absolute, else
/// relative to `cwd` when it exists there, else its file name in the report's
/// directory (as [`crate::analyze::AudioInput::from_render`] does with the
/// process cwd).
pub fn master_audio_path(
    rr: &RenderReport,
    render: Option<&Path>,
    report_dir: &Path,
    cwd: &Path,
) -> Option<PathBuf> {
    rr.audio.as_ref()?;
    if let Some(r) = render {
        return Some(r.to_path_buf());
    }
    let out = rr.output.as_ref()?;
    Some(if out.is_absolute() {
        out.clone()
    } else if cwd.join(out).exists() {
        cwd.join(out)
    } else {
        report_dir.join(out.file_name()?)
    })
}

/// Every path a `ferrocut-perceive check` reads or writes for one render,
/// decided once so the checker and a caller that validates paths (MCP) agree.
#[derive(Clone, Debug)]
pub struct CheckPaths {
    /// The render report read.
    pub report: PathBuf,
    pub resolution: ChunkDirResolution,
    /// Each chunk master that exists, from [`ChunkDirResolution::locate`], in
    /// report order. A chunk found nowhere is absent here; the check reads its
    /// cached analysis or fails naming the directories searched.
    pub chunk_files: Vec<PathBuf>,
    /// The master whose audio is measured, when the report has audio.
    pub audio: Option<PathBuf>,
    /// The engine cache: analysis is cached in `perceive/v1`, audio in `audio/`.
    pub cache_dir: PathBuf,
    /// Directories the checker creates or writes when not told otherwise.
    pub writes: Vec<PathBuf>,
}

/// Decide [`CheckPaths`] for `render` (a master, or a render report when it
/// ends in `.json`). `report` and `cache_dir` are the explicit
/// `--render-report` / `--cache-dir`; `cwd` is the checker's working
/// directory. A report that lists chunks but whose chunk directory is
/// ambiguous or unresolved is an error here, before any cache is read, and so
/// is a chunk key that is not a plain hex name.
pub fn check_paths(
    render: &Path,
    report: Option<&Path>,
    cache_dir: Option<&Path>,
    cwd: &Path,
) -> anyhow::Result<(RenderReport, CheckPaths)> {
    let is_report = render.extension().is_some_and(|e| e == "json");
    let report_path = match report {
        Some(p) => p.to_path_buf(),
        None if is_report => render.to_path_buf(),
        None => render.with_extension("report.json"),
    };
    let mut rr = RenderReport::load(&report_path)?;
    let resolution = resolve_chunk_dir(&rr, &report_path, cwd, cache_dir);
    if !resolution.is_usable() && !rr.chunks.is_empty() {
        anyhow::bail!("{}", resolution.error(&report_path));
    }
    let mut chunk_files = Vec::new();
    for c in &rr.chunks {
        if let Some(f) = resolution.locate(&c.key)? {
            chunk_files.push(f);
        }
    }
    let report_dir = report_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let pointed = (report.is_none() && !is_report).then_some(render);
    let audio = master_audio_path(&rr, pointed, &report_dir, cwd);
    let cache = cache_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_cache_dir(&resolution, &report_dir));
    let writes = vec![
        cache.join("perceive").join("v1"),
        cache.join("perceive").join("check-out"),
        cache.join("audio"),
    ];
    rr.chunk_resolution = Some(resolution.clone());
    Ok((
        rr,
        CheckPaths {
            report: report_path,
            resolution,
            chunk_files,
            audio,
            cache_dir: cache,
            writes,
        },
    ))
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
