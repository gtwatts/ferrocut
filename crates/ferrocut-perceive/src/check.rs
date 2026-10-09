//! Grading: turn a perception [`Report`] into a pass/fail verdict with
//! machine-readable problems (`ferrocut-perceive check`, used by the engine's
//! `ferrocut check` / `render --check` / MCP `quality_check` hook).
//!
//! The output ([`CheckReport`], schema `ferrocut.perceive.check/1`) is a
//! stable contract: `schema_version`, `pass`, `problems[]` with `reason`,
//! `range: [start, end]` (ferrocut-types rational time strings, end
//! exclusive), `measured`, `threshold`. Every other field is additive.
//! `problems` only holds failures, so `pass == problems.is_empty()`;
//! observations below the thresholds go to `warnings` (same shape).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use ferrocut_core::{FrameRate, Rational, RationalTime};
use serde::{Deserialize, Serialize};

use crate::audio::AudioReport;
use crate::input::{ChunkDirSource, Timeline};
use crate::report::{ChunkReport, Report, timecode};
use crate::targets::{self, Resolved, ThresholdSource};

pub const CHECK_SCHEMA_VERSION: &str = "ferrocut.perceive.check/1";

/// Stable reason codes. The first seven are the grader contract; the rest
/// are additive (a consumer that doesn't know them should treat them as
/// generic failures).
pub mod reason {
    pub const MISSED_CUT: &str = "missed_cut";
    pub const EXTRA_CUT: &str = "extra_cut";
    pub const BLACK_FRAMES: &str = "black_frames";
    pub const FROZEN_FRAMES: &str = "frozen_frames";
    pub const FLASH: &str = "flash";
    pub const LOUDNESS_OFF_TARGET: &str = "loudness_off_target";
    pub const TRUE_PEAK_OVER: &str = "true_peak_over";
    /// The timeline/brief needs audio but the render has none (additive).
    pub const MISSING_AUDIO: &str = "missing_audio";
    /// Decoded master audio differs from the engine's mix (additive).
    pub const AUDIO_JOIN_MISMATCH: &str = "audio_join_mismatch";
    /// The render's recorded loudness target or ceiling differs from the
    /// timeline's current `audio.loudness` (a warning, additive).
    pub const LOUDNESS_TARGET_MISMATCH: &str = "loudness_target_mismatch";
}

/// Grading thresholds. Defaults are the eval grader's. The loudness target
/// and true-peak ceiling default to what the render recorded (else the
/// timeline's `audio.loudness`); a config file (`--config`, JSON with any
/// subset of these keys) overrides the keys it names and command-line flags
/// override the file. See [`crate::targets`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckThresholds {
    /// Integrated loudness must be within `target ± tolerance`.
    pub loudness_target_lufs: f64,
    pub loudness_tolerance_lu: f64,
    /// True peak must be ≤ this.
    pub true_peak_max_dbtp: f64,
    /// A detected cut within this many frames of an expected one matches it.
    pub cut_tolerance_frames: i64,
    /// Longest allowed run of black frames mid-program (0: any black run
    /// between shots fails, e.g. a gap in the timeline).
    pub max_black_frames: i64,
    /// Black at the very start or end (fade from/to black) is allowed up to
    /// this many seconds.
    pub max_edge_black_s: f64,
    /// Frozen runs (identical frames) longer than this many seconds fail;
    /// shorter ones are warnings.
    pub max_frozen_s: f64,
    /// Longest allowed flash (a ≤ 2-frame shot between matching shots);
    /// 0: any flash fails.
    pub max_flash_frames: i64,
    /// Fail when the render has no audio.
    pub require_audio: bool,
    /// Grade cuts at all (missed_cut / extra_cut).
    pub check_cuts: bool,
}

impl Default for CheckThresholds {
    fn default() -> Self {
        CheckThresholds {
            loudness_target_lufs: -14.0,
            loudness_tolerance_lu: 1.0,
            true_peak_max_dbtp: -1.0,
            cut_tolerance_frames: 1,
            max_black_frames: 0,
            max_edge_black_s: 2.0,
            max_frozen_s: 2.0,
            max_flash_frames: 0,
            require_audio: true,
            check_cuts: true,
        }
    }
}

impl CheckThresholds {
    /// Defaults overridden by a JSON config file.
    pub fn load(config: Option<&Path>) -> anyhow::Result<Self> {
        match config {
            None => Ok(Self::default()),
            Some(p) => {
                let t = std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?;
                serde_json::from_str(&t)
                    .with_context(|| format!("parsing check config {}", p.display()))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProblemSeverity {
    /// Fails the check (everything in `problems`).
    Error,
    /// Informational (everything in `warnings`).
    Warning,
}

/// One problem (or warning).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    /// Stable code, see [`reason`].
    pub reason: String,
    /// `[start, end)` in timeline seconds, ferrocut-types `RationalTime`
    /// serde form (`"num/den"` or `"num"`).
    pub range: [RationalTime; 2],
    /// Measured value (`null` when there is none, e.g. no audio), in `unit`.
    pub measured: Option<f64>,
    /// The threshold it was held to, in `unit`. For `loudness_off_target`
    /// this is the target; the allowed deviation is `tolerance`.
    pub threshold: Option<f64>,
    // ---- additive fields ----
    pub severity: ProblemSeverity,
    /// `frames`, `s`, `LUFS`, `dBTP`, `LU` or `distance` (shot-change
    /// distance, 0..1).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tolerance: Option<f64>,
    /// `[start, end)` as non-drop timecodes at the rounded frame rate.
    pub timecode: [String; 2],
    /// `[start, end)` frames.
    pub frames: [i64; 2],
    pub message: String,
    /// Where the thresholds of a loudness or true-peak problem came from.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub threshold_sources: Option<BTreeMap<String, ThresholdSource>>,
}

/// Measured figures, for context (additive).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Measured {
    pub frames: i64,
    pub fps: FrameRate,
    pub duration: RationalTime,
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    /// Expected hard cuts (from `--brief-cuts`, else the timeline) and the
    /// cuts detected in the picture, as frame indices.
    pub expected_cuts: Vec<i64>,
    pub detected_cuts: Vec<i64>,
    /// `brief` or `timeline`.
    pub cuts_from: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckReport {
    pub schema_version: String,
    pub pass: bool,
    pub problems: Vec<Problem>,
    // ---- additive fields ----
    pub warnings: Vec<Problem>,
    pub thresholds: CheckThresholds,
    /// Where each threshold came from (`flag`, `config`, `render`,
    /// `timeline`, `default`).
    #[serde(default)]
    pub threshold_sources: BTreeMap<String, ThresholdSource>,
    /// The audio thresholds used, each with its source.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub loudness_target: Option<targets::LoudnessTarget>,
    pub measured: Measured,
    /// The perception report's schema version this was graded from.
    pub perceive_schema_version: String,
    /// The checker's primary chunk directory and how it was resolved (not
    /// per-chunk provenance). `grade_resolved` leaves
    /// this empty; `ferrocut-perceive check` fills it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_chunks: Option<RenderChunks>,
}

/// The PRIMARY chunk directory (the first existing search directory) and the
/// rule that produced it. Each chunk file is looked up through the whole
/// search list, so an individual chunk may have been read from a later
/// directory (e.g. an explicit `--cache-dir`); this is not per-file provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderChunks {
    pub chunk_dir: Option<PathBuf>,
    pub resolved_by: ChunkDirSource,
}

/// What `check` prints (with `--json`) when it can't grade: exit code 2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckError {
    pub schema_version: String,
    pub pass: bool,
    pub problems: Vec<Problem>,
    pub error: String,
}

impl CheckError {
    pub fn new(e: &anyhow::Error) -> Self {
        CheckError {
            schema_version: CHECK_SCHEMA_VERSION.into(),
            pass: false,
            problems: Vec::new(),
            error: format!("{e:#}"),
        }
    }
}

/// Parse a brief's expected cut times. Accepted: a JSON array, or an object
/// with a `cuts` array, of rational-time strings (`"121/24"`, `"5"`), numbers
/// (seconds) or `HH:MM:SS:FF` timecodes at the timeline rate; or plain text
/// with one such entry per line (`#` starts a comment). Returns the frame
/// each cut lands on (the first frame of the new shot, nearest frame).
pub fn parse_brief_cuts(text: &str, fps: FrameRate) -> anyhow::Result<Vec<i64>> {
    fn entry(s: &str, fps: FrameRate) -> anyhow::Result<i64> {
        let s = s.trim();
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 4 {
            let n: Vec<i64> = parts
                .iter()
                .map(|p| p.parse::<i64>())
                .collect::<Result<_, _>>()
                .with_context(|| format!("bad timecode {s:?}"))?;
            let nominal = fps.round().max(1);
            return Ok(((n[0] * 60 + n[1]) * 60 + n[2]) * nominal + n[3]);
        }
        let t: RationalTime = serde_json::from_value(serde_json::Value::String(s.into()))
            .or_else(|_| {
                // Decimal seconds ("2.5"): exact as a decimal fraction.
                let (i, f) = s.split_once('.').context("not a time")?;
                let den = 10i64.pow(f.len() as u32);
                let sign = if i.starts_with('-') { -1 } else { 1 };
                let num = i.parse::<i64>()? * den + sign * f.parse::<i64>()?;
                anyhow::Ok(RationalTime(Rational::new(num, den)))
            })
            .with_context(|| format!("bad cut time {s:?}"))?;
        Ok(t.frame_round(fps))
    }
    let v: Option<serde_json::Value> = serde_json::from_str(text).ok();
    let items: Vec<String> = match v {
        Some(serde_json::Value::Array(a)) => {
            a.iter().map(json_item).collect::<anyhow::Result<_>>()?
        }
        Some(serde_json::Value::Object(o)) => match o.get("cuts") {
            Some(serde_json::Value::Array(a)) => {
                a.iter().map(json_item).collect::<anyhow::Result<_>>()?
            }
            _ => bail!("brief cuts JSON object needs a \"cuts\" array"),
        },
        Some(_) => bail!("brief cuts must be a JSON array, an object with \"cuts\", or text lines"),
        None => text
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
    };
    let mut f: Vec<i64> = items
        .iter()
        .map(|s| entry(s, fps))
        .collect::<anyhow::Result<_>>()?;
    f.sort();
    f.dedup();
    Ok(f)
}

fn json_item(v: &serde_json::Value) -> anyhow::Result<String> {
    match v {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Object(o) => match o.get("time").or_else(|| o.get("at")) {
            Some(t) => json_item(t),
            None => bail!("cut entry {v} has no \"time\""),
        },
        _ => bail!("bad cut entry {v}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn mk(
    reason: &str,
    sev: ProblemSeverity,
    s: i64,
    e: i64,
    fps: FrameRate,
    measured: Option<f64>,
    threshold: Option<f64>,
    unit: &str,
    message: String,
) -> Problem {
    Problem {
        reason: reason.into(),
        range: [
            RationalTime::from_frames(s, fps),
            RationalTime::from_frames(e, fps),
        ],
        measured,
        threshold,
        severity: sev,
        unit: Some(unit.into()),
        tolerance: None,
        timecode: [timecode(s, fps), timecode(e, fps)],
        frames: [s, e],
        message,
        threshold_sources: None,
    }
}

/// Grade `report` (analysed from a render of `tl`) against `th`, every
/// threshold counted as `default`. `brief_cuts`, when given, replaces the
/// timeline's hard cuts as the expected cuts.
pub fn grade(
    report: &Report,
    tl: &Timeline,
    brief_cuts: Option<&[i64]>,
    th: &CheckThresholds,
) -> CheckReport {
    grade_resolved(report, tl, brief_cuts, &Resolved::defaults(th.clone()))
}

/// [`grade`] with resolved thresholds and their sources ([`crate::targets`]).
pub fn grade_resolved(
    report: &Report,
    tl: &Timeline,
    brief_cuts: Option<&[i64]>,
    resolved: &Resolved,
) -> CheckReport {
    let th = &resolved.thresholds;
    let fps = report.timeline.fps;
    let total = report.timeline.total_frames;
    let time = |f: i64| RationalTime::from_frames(f, fps);
    let secs = |frames: i64| frames as f64 * fps.den() as f64 / fps.num() as f64;
    let r2 = |v: f64| crate::scopes::round(v, 3);
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    use ProblemSeverity::*;
    let sh = &report.shots;

    // Cuts.
    let intended = tl.intended(total);
    // Timeline cuts on the base track are hard expectations; boundaries that
    // exist only on upper tracks (overlays, often small or translucent) are
    // soft: if not visible, that's a warning, not a failure.
    let base_cuts = {
        let mut base = tl.clone();
        base.tracks.truncate(1);
        base.intended(total).cuts
    };
    let (expected, cuts_from) = match brief_cuts {
        Some(b) => (
            b.iter()
                .copied()
                .filter(|&f| f > 0 && f < total)
                .collect::<Vec<_>>(),
            "brief",
        ),
        None => (intended.cuts.clone(), "timeline"),
    };
    let hard = |f: i64| brief_cuts.is_some() || base_cuts.contains(&f);
    let flash_edge = |f: i64| sh.flash_frames.iter().any(|x| f == x.start || f == x.end);
    let in_dissolve = |f: i64| {
        intended.dissolves.iter().any(|&(s, e)| f >= s && f <= e)
            || sh.dissolves.iter().any(|d| f >= d.start && f <= d.end)
    };
    let detected: Vec<i64> = sh.cuts.iter().map(|c| c.frame).collect();
    let tol = th.cut_tolerance_frames.max(0);
    let cut_min = report.settings.thresholds.cut_min;
    if th.check_cuts {
        for &f in &expected {
            if !detected.iter().any(|&d| (d - f).abs() <= tol) {
                let dist = sh
                    .missed_cuts
                    .iter()
                    .find(|m| m.frame == f)
                    .map(|m| m.distance);
                let fail = hard(f);
                (if fail { &mut problems } else { &mut warnings }).push(mk(
                    reason::MISSED_CUT,
                    if fail { Error } else { Warning },
                    f,
                    f + 1,
                    fps,
                    dist,
                    Some(cut_min),
                    "distance",
                    if fail {
                        format!(
                            "expected a cut at frame {f} ({cuts_from}), none visible within ±{tol} frame(s)"
                        )
                    } else {
                        format!(
                            "overlay boundary at frame {f} not visible as a cut (small or translucent layer?)"
                        )
                    },
                ));
            }
        }
        for c in &sh.cuts {
            let f = c.frame;
            if flash_edge(f) || in_dissolve(f) {
                continue;
            }
            if !expected.iter().any(|&x| (x - f).abs() <= tol) {
                problems.push(mk(
                    reason::EXTRA_CUT,
                    Error,
                    f,
                    f + 1,
                    fps,
                    Some(c.distance),
                    Some(cut_min),
                    "distance",
                    format!("visible cut at frame {f} that the {cuts_from} doesn't call for"),
                ));
            }
        }
    }

    // Flash frames.
    for x in &sh.flash_frames {
        let n = x.end - x.start;
        let fail = n > th.max_flash_frames;
        (if fail { &mut problems } else { &mut warnings }).push(mk(
            reason::FLASH,
            if fail { Error } else { Warning },
            x.start,
            x.end,
            fps,
            Some(n as f64),
            Some(th.max_flash_frames as f64),
            "frames",
            format!("{n}-frame flash between matching shots"),
        ));
    }

    // Black frames.
    for x in &sh.black {
        let n = x.end - x.start;
        let edge = x.start == 0 || x.end == total;
        let (fail, measured, threshold, unit, what) = if edge {
            let s = secs(n);
            (
                s > th.max_edge_black_s,
                r2(s),
                th.max_edge_black_s,
                "s",
                "at the start/end",
            )
        } else {
            (
                n > th.max_black_frames,
                n as f64,
                th.max_black_frames as f64,
                "frames",
                "mid-program",
            )
        };
        (if fail { &mut problems } else { &mut warnings }).push(mk(
            reason::BLACK_FRAMES,
            if fail { Error } else { Warning },
            x.start,
            x.end,
            fps,
            Some(measured),
            Some(threshold),
            unit,
            format!("{n} black frame(s) {what}"),
        ));
    }

    // Frozen frames.
    for x in &sh.frozen {
        let s = secs(x.end - x.start);
        let fail = s > th.max_frozen_s;
        (if fail { &mut problems } else { &mut warnings }).push(mk(
            reason::FROZEN_FRAMES,
            if fail { Error } else { Warning },
            x.start,
            x.end,
            fps,
            Some(r2(s)),
            Some(th.max_frozen_s),
            "s",
            format!(
                "{} identical frames (still image or stalled source)",
                x.end - x.start
            ),
        ));
    }

    // Audio.
    let (p, w) = grade_audio(report.audio.as_ref(), &report.chunks, total, fps, resolved);
    problems.extend(p);
    warnings.extend(w);
    let key = |p: &Problem| (p.frames[0], p.frames[1], p.reason.clone());
    problems.sort_by_key(key);
    warnings.sort_by_key(key);
    CheckReport {
        schema_version: CHECK_SCHEMA_VERSION.into(),
        pass: problems.is_empty(),
        problems,
        warnings,
        thresholds: th.clone(),
        threshold_sources: resolved.sources.clone(),
        loudness_target: Some(resolved.loudness_target()),
        measured: Measured {
            frames: total,
            fps,
            duration: time(total),
            integrated_lufs: report
                .audio
                .as_ref()
                .and_then(|a| a.loudness.integrated_lufs),
            true_peak_dbtp: report
                .audio
                .as_ref()
                .and_then(|a| a.loudness.true_peak_dbtp),
            expected_cuts: expected,
            detected_cuts: detected,
            cuts_from: cuts_from.into(),
        },
        perceive_schema_version: report.schema_version.clone(),
        render_chunks: None,
    }
}

/// Grade the master's audio against `resolved`: missing audio (no stream),
/// loudness (an existing stream measured silent still fails
/// `loudness_off_target`), true peak, join mismatches, and the non-failing
/// `loudness_target_mismatch` when the render and the timeline disagree.
/// Returns (problems, warnings).
pub fn grade_audio(
    audio: Option<&AudioReport>,
    chunks: &[ChunkReport],
    total: i64,
    fps: FrameRate,
    resolved: &Resolved,
) -> (Vec<Problem>, Vec<Problem>) {
    use ProblemSeverity::*;
    let th = &resolved.thresholds;
    let sources = |keys: &[&str]| {
        Some(
            keys.iter()
                .map(|k| (k.to_string(), resolved.source(k)))
                .collect::<BTreeMap<_, _>>(),
        )
    };
    let (mut problems, mut warnings) = (Vec::new(), Vec::new());
    match audio {
        None => {
            if th.require_audio {
                problems.push(mk(
                    reason::MISSING_AUDIO,
                    Error,
                    0,
                    total,
                    fps,
                    None,
                    None,
                    "LUFS",
                    "the render has no audio track".into(),
                ));
            }
            return (problems, warnings);
        }
        Some(a) => {
            let l = &a.loudness;
            let i = l.integrated_lufs;
            let off =
                i.is_none_or(|v| (v - th.loudness_target_lufs).abs() > th.loudness_tolerance_lu);
            let mut p = mk(
                reason::LOUDNESS_OFF_TARGET,
                if off { Error } else { Warning },
                0,
                total,
                fps,
                i,
                Some(th.loudness_target_lufs),
                "LUFS",
                match i {
                    Some(v) => format!(
                        "integrated loudness {v} LUFS, target {} ±{} LU ({}/{})",
                        th.loudness_target_lufs,
                        th.loudness_tolerance_lu,
                        source_name(resolved.source(targets::TARGET)),
                        source_name(resolved.source(targets::TOLERANCE)),
                    ),
                    None => "no signal above the loudness gates (silent)".into(),
                },
            );
            p.tolerance = Some(th.loudness_tolerance_lu);
            p.threshold_sources = sources(&[targets::TARGET, targets::TOLERANCE]);
            if off {
                problems.push(p);
            }
            if let Some(tp) = l.true_peak_dbtp
                && tp > th.true_peak_max_dbtp
            {
                // Range: the chunks whose own true peak is over.
                let over: Vec<_> = chunks
                    .iter()
                    .filter(|c| {
                        c.audio
                            .as_ref()
                            .and_then(|x| x.true_peak_dbtp)
                            .is_some_and(|v| v > th.true_peak_max_dbtp)
                    })
                    .collect();
                let (s, e) = match (over.first(), over.last()) {
                    (Some(f), Some(l)) => (f.start_frame, l.start_frame + l.frames),
                    _ => (0, total),
                };
                let mut p = mk(
                    reason::TRUE_PEAK_OVER,
                    Error,
                    s,
                    e,
                    fps,
                    Some(tp),
                    Some(th.true_peak_max_dbtp),
                    "dBTP",
                    format!(
                        "true peak {tp} dBTP exceeds {} dBTP ({}; {} chunk(s) over)",
                        th.true_peak_max_dbtp,
                        source_name(resolved.source(targets::CEILING)),
                        over.len()
                    ),
                );
                p.threshold_sources = sources(&[targets::CEILING]);
                problems.push(p);
            }
            for &ci in &a.join_mismatch_chunks {
                if let Some(c) = chunks.iter().find(|c| c.index == ci) {
                    problems.push(mk(
                        reason::AUDIO_JOIN_MISMATCH,
                        Error,
                        c.start_frame,
                        c.start_frame + c.frames,
                        fps,
                        None,
                        None,
                        "frames",
                        format!("chunk {ci}: master audio differs from the engine's mix"),
                    ));
                }
            }
        }
    }
    let show = |v: Option<f64>| v.map_or("none".to_string(), |v| v.to_string());
    for &(key, render, tl) in &resolved.mismatches {
        let (unit, what) = if key == targets::TARGET {
            ("LUFS", "loudness target")
        } else {
            ("dBTP", "true-peak ceiling")
        };
        let mut p = mk(
            reason::LOUDNESS_TARGET_MISMATCH,
            Warning,
            0,
            total,
            fps,
            render,
            tl,
            unit,
            format!(
                "the render was made with {what} {} {unit} but the timeline now says {}: re-render to grade the current intent (graded against {} {unit}, {})",
                show(render),
                show(tl),
                if key == targets::TARGET {
                    th.loudness_target_lufs
                } else {
                    th.true_peak_max_dbtp
                },
                source_name(resolved.source(key)),
            ),
        );
        p.threshold_sources = sources(&[key]);
        warnings.push(p);
    }
    (problems, warnings)
}

fn source_name(s: ThresholdSource) -> &'static str {
    match s {
        ThresholdSource::Flag => "flag",
        ThresholdSource::Config => "config",
        ThresholdSource::Render => "render",
        ThresholdSource::Timeline => "timeline",
        ThresholdSource::Default => "default",
    }
}

impl CheckReport {
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("serializable");
        s.push('\n');
        s
    }
    /// Human-readable summary (the CLI's default output).
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{}: {} problem(s), {} warning(s)\n",
            if self.pass { "PASS" } else { "FAIL" },
            self.problems.len(),
            self.warnings.len()
        );
        if let Some(t) = &self.loudness_target {
            s += &format!("  {}\n", t.line());
        }
        for (tag, list) in [("FAIL", &self.problems), ("warn", &self.warnings)] {
            for p in list {
                s += &format!(
                    "  {tag} {:<20} {}..{}  {}\n",
                    p.reason, p.timecode[0], p.timecode[1], p.message
                );
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_cut_formats() {
        let fps = Rational::from_int(24);
        assert_eq!(
            parse_brief_cuts(r#"["2", "121/24", 3.5]"#, fps).unwrap(),
            vec![48, 84, 121]
        );
        assert_eq!(
            parse_brief_cuts(r#"{"cuts": [{"time": "1"}, "00:00:02:12"]}"#, fps).unwrap(),
            vec![24, 60]
        );
        assert_eq!(
            parse_brief_cuts("# cuts\n1.5\n00:00:03:00 # second\n", fps).unwrap(),
            vec![36, 72]
        );
        assert!(parse_brief_cuts("[true]", fps).is_err());
        assert!(parse_brief_cuts("soon", fps).is_err());
    }

    #[test]
    fn config_overrides_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("c.json");
        std::fs::write(&p, r#"{"loudness_target_lufs": -23, "max_frozen_s": 0.5}"#).unwrap();
        let t = CheckThresholds::load(Some(&p)).unwrap();
        assert_eq!(
            (t.loudness_target_lufs, t.max_frozen_s, t.true_peak_max_dbtp),
            (-23.0, 0.5, -1.0)
        );
        std::fs::write(&p, r#"{"loudness": 1}"#).unwrap();
        assert!(
            CheckThresholds::load(Some(&p)).is_err(),
            "unknown keys are rejected"
        );
    }
}
