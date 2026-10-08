//! The perception report: a stable, versioned, deterministic JSON document
//! keyed to the engine's output frames and chunks. See
//! `schema/perceive-report.schema.json` and the README for the contract.
//!
//! Compatibility rule: `schema_version` changes (`ferrocut.perceive/N+1`)
//! whenever a field is removed, renamed or changes meaning. Adding optional
//! fields keeps the version.

use ferrocut_core::{FrameRate, RationalTime};
use serde::{Deserialize, Serialize};

use crate::audio::{AudioReport, Loudness};
use crate::scopes::{ColorStats, Levels};
use crate::shots::{Shots, Thresholds};

pub const SCHEMA_VERSION: &str = "ferrocut.perceive/1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema_version: String,
    /// `ferrocut-perceive <version>`.
    pub generator: String,
    pub timeline: TimelineInfo,
    pub settings: Settings,
    pub summary: Summary,
    /// Things an editor would flag, most severe first, then by frame.
    pub issues: Vec<Issue>,
    pub shots: Shots,
    pub audio: Option<AudioReport>,
    /// Contact sheet of the whole timeline (path relative to the report).
    pub contact_sheet: Option<String>,
    pub chunks: Vec<ChunkReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
    pub total_frames: i64,
    pub chunk_frames: i64,
    pub duration: RationalTime,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Full scopes on every `sample_every`-th output frame plus each chunk's
    /// first and last frame. Histogram/level stats and shot features use
    /// every frame.
    pub sample_every: i64,
    pub scope_images: bool,
    pub thumb_width: usize,
    pub thresholds: Thresholds,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub frames: i64,
    pub sampled_frames: usize,
    /// Over every frame.
    pub levels: Levels,
    /// Over sampled frames.
    pub color: ColorStats,
    pub shots: usize,
    pub cuts: usize,
    pub dissolves: usize,
    pub missed_cuts: usize,
    pub unexpected_cuts: usize,
    pub flash_frames: i64,
    pub black_frames: i64,
    pub frozen_frames: i64,
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub issues: IssueCounts,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueCounts {
    pub error: usize,
    pub warning: usize,
    pub info: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub severity: Severity,
    /// Stable machine-readable kind, e.g. `flash_frame`, `missed_cut`,
    /// `clipped_highlights`, `true_peak_over`.
    pub kind: String,
    /// `[frame, end_frame)` when the issue has a picture position.
    pub frame: Option<i64>,
    pub end_frame: Option<i64>,
    pub timecode: Option<String>,
    pub chunk: Option<usize>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkReport {
    pub index: usize,
    pub start_frame: i64,
    pub frames: i64,
    pub start: RationalTime,
    pub end: RationalTime,
    pub timecode: String,
    /// The engine's chunk key (render cache).
    pub key: String,
    /// Key of the cached analysis: chunk key + analysis version + settings.
    pub analysis_key: String,
    pub levels: Levels,
    pub color: ColorStats,
    /// Mean frame-to-frame distance (0 = static).
    pub motion: f64,
    pub black_frames: i64,
    pub frozen_frames: i64,
    pub samples: Vec<Sample>,
    /// Shot events starting in this chunk.
    pub events: Vec<Event>,
    pub audio: Option<Loudness>,
    pub contact_sheet: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub frame: i64,
    pub time: RationalTime,
    pub timecode: String,
    pub levels: Levels,
    pub color: ColorStats,
    pub scope_image: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Cut,
    Dissolve,
    Flash,
    Black,
    Frozen,
    MissedCut,
    UnexpectedCut,
    MissedDissolve,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub kind: EventKind,
    /// `[frame, end_frame)`; `end_frame` is `frame + 1` for point events.
    pub frame: i64,
    pub end_frame: i64,
}

impl Report {
    /// Canonical serialization: pretty JSON, trailing newline. Byte-identical
    /// for identical inputs.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("report serializes");
        s.push('\n');
        s
    }
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        let v: serde_json::Value = serde_json::from_str(text)?;
        let got = v
            .get("schema_version")
            .and_then(|s| s.as_str())
            .unwrap_or("<missing>");
        anyhow::ensure!(
            got == SCHEMA_VERSION,
            "unsupported schema_version {got} (this build reads {SCHEMA_VERSION})"
        );
        Ok(serde_json::from_value(v)?)
    }
}

/// Non-drop-frame timecode `HH:MM:SS:FF` at the nominal (rounded) rate.
pub fn timecode(frame: i64, fps: FrameRate) -> String {
    let nominal = fps.round().max(1);
    let (f, s) = (frame.rem_euclid(nominal), frame.div_euclid(nominal));
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        s / 3600,
        (s / 60) % 60,
        s % 60,
        f
    )
}
