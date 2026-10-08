//! Delivery: H.264/AAC MP4 from a finished render's FFV1 master, via
//! SeePlus's `ferrocut-deliver` (Cisco's OpenH264, loaded at run time).
//!
//! - IDRs land on render chunk boundaries: the delivery chunk plan is the
//!   render report's chunk starts (the master's keyframes are exactly there).
//! - The codec is never fetched implicitly. [`openh264::Provider::from_env`]
//!   only uses a library the user already enabled (or `FERROCUT_OPENH264_LIB`);
//!   a missing codec is a `Permanent` error that says how to enable it.
//!   Downloading needs an explicit opt-in (`ferrocut render
//!   --download-openh264`, the MCP `openh264` tool's `enable`, or
//!   `ferrocut-deliver openh264 enable`).
//! - Cancellation is checked before and after the encode (the encoder has no
//!   cancel hook yet); a delivery cancelled mid-way removes its output.

use std::path::PathBuf;
use std::time::Instant;

use ferrocut_core::{CancelToken, ErrorKind, NodeError};
pub use ferrocut_deliver::deliver::DEFAULT_QP;
pub use ferrocut_deliver::openh264;
use ferrocut_deliver::{ChunkPlan, DeliverOptions, DeliverReport};
use serde::{Deserialize, Serialize};

use crate::compositor::OUTPUT_SPACE;
use crate::render::RenderReport;

/// Delivery container/codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverFormat {
    /// H.264 (OpenH264, constant QP) + AAC in a faststart MP4.
    Mp4,
}

impl std::str::FromStr for DeliverFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "mp4" => Ok(DeliverFormat::Mp4),
            _ => Err(format!("unknown delivery format {s:?} (supported: mp4)")),
        }
    }
}

/// What to deliver and how.
#[derive(Clone, Debug)]
pub struct DeliverRequest {
    pub format: DeliverFormat,
    /// Default: the master with an `.mp4` extension (`out.mkv` -> `out.mp4`).
    pub output: Option<PathBuf>,
    /// Constant QP 0..=51.
    pub qp: u8,
    /// Encode the master's audio as AAC (if it has any).
    pub audio: bool,
    /// Parallel chunk encoders (CPU only).
    pub jobs: usize,
    pub openh264: openh264::Provider,
}

impl DeliverRequest {
    pub fn mp4(openh264: openh264::Provider) -> Self {
        DeliverRequest {
            format: DeliverFormat::Mp4,
            output: None,
            qp: DEFAULT_QP,
            audio: true,
            jobs: default_jobs(),
            openh264,
        }
    }
}

/// The `deliver` section of a [`RenderReport`]; the full delivery report is
/// written to [`DeliverSummary::report`] (`<output>.deliver.json`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeliverSummary {
    pub format: DeliverFormat,
    pub output: PathBuf,
    pub report: PathBuf,
    pub output_sha256: String,
    pub output_bytes: u64,
    pub frames: i64,
    pub video_codec: String,
    pub qp: u8,
    pub jobs: usize,
    /// Frames that start an IDR (one per delivery chunk = render chunk starts).
    pub idr_frames: Vec<i64>,
    /// e.g. "OpenH264 2.6.0 (cache)".
    pub encoder: String,
    pub encode_ms: u128,
    pub mux_ms: u128,
    pub total_ms: u128,
}

/// Default delivery encoders: min(cores, 12) (CPU only, unlike render jobs).
pub fn default_jobs() -> usize {
    ferrocut_deliver::default_jobs()
}

fn cancelled(cancel: Option<&CancelToken>) -> bool {
    cancel.is_some_and(CancelToken::is_cancelled)
}

/// Encode the master of `render` per `req`. Writes the MP4 and its
/// `<output>.deliver.json`; returns the summary and the full report.
/// A `Retryable` encoder failure is retried once.
pub fn deliver(
    render: &RenderReport,
    req: &DeliverRequest,
    cancel: Option<&CancelToken>,
) -> Result<(DeliverSummary, DeliverReport), NodeError> {
    let t0 = Instant::now();
    if cancelled(cancel) {
        return Err(NodeError::cancelled("delivery cancelled before it started"));
    }
    let DeliverFormat::Mp4 = req.format;
    let master = &render.output;
    let output = req
        .output
        .clone()
        .unwrap_or_else(|| ferrocut_deliver::default_output(master));
    if output == *master {
        return Err(NodeError::permanent(format!(
            "delivery output {} would overwrite the master",
            output.display()
        )));
    }
    let starts: Vec<i64> = render.chunks.iter().map(|c| c.plan.start_frame).collect();
    let mut opts = DeliverOptions::new(req.openh264.clone());
    opts.qp = req.qp;
    opts.jobs = req.jobs.max(1);
    opts.chunks = if starts.is_empty() {
        ChunkPlan::Default
    } else {
        ChunkPlan::Starts(starts)
    };
    opts.master_space = OUTPUT_SPACE.into();
    if !req.audio {
        opts.audio_bit_rate = None;
    }
    let report = match ferrocut_deliver::deliver(master, &output, &opts) {
        Err(e) if e.kind == ErrorKind::Retryable && !cancelled(cancel) => {
            eprintln!("ferrocut: delivery failed ({e}); retrying once");
            ferrocut_deliver::deliver(master, &output, &opts)
        }
        r => r,
    }?;
    if cancelled(cancel) {
        let _ = std::fs::remove_file(&output);
        return Err(NodeError::cancelled(
            "delivery cancelled (the partial MP4 was removed)",
        ));
    }
    let report_path = ferrocut_deliver::report_path(&output);
    let text = serde_json::to_string_pretty(&report)
        .map_err(|e| NodeError::permanent(format!("delivery report: {e}")))?;
    std::fs::write(&report_path, format!("{text}\n"))
        .map_err(|e| NodeError::permanent(format!("writing {}: {e}", report_path.display())))?;
    let summary = DeliverSummary {
        format: req.format,
        output: report.output.clone(),
        report: report_path,
        output_sha256: report.output_sha256.clone(),
        output_bytes: report.output_bytes,
        frames: report.frames,
        video_codec: report.video_codec.clone(),
        qp: report.qp,
        jobs: report.jobs,
        idr_frames: report.chunks.iter().map(|c| c.start_frame).collect(),
        encoder: format!(
            "OpenH264 {} ({})",
            report.openh264.version,
            serde_json::to_value(&report.openh264.source)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        ),
        encode_ms: report.encode_ms,
        mux_ms: report.mux_ms,
        total_ms: t0.elapsed().as_millis(),
    };
    Ok((summary, report))
}
