//! # ferrocut-perceive
//!
//! A perception report an agent reads after each render, to critique its own
//! edit the way an editor would look at scopes, a contact sheet and a
//! loudness meter:
//!
//! - **Scopes** ([`scopes`], [`gpu`]): luma waveform, RGB parade,
//!   vectorscope and histograms per sampled frame, as numeric summaries
//!   (levels, clipping/crush %, saturation, skin-tone line deviation) plus
//!   optional PNGs. GPU compute with an exact CPU reference.
//! - **Contact sheets** ([`images`]): per chunk and for the whole timeline.
//! - **Shots** ([`shots`]): cuts, dissolves, flash, black and frozen frames,
//!   cross-checked against the cuts the timeline intends.
//! - **Audio** ([`audio`]): EBU R128 loudness (integrated, short-term,
//!   momentary, LRA), true peak, silence and clipping spans.
//!
//! - **Grading** ([`check`]): pass/fail with stable reason codes for eval
//!   harnesses (`ferrocut-perceive check`).
//!
//! The [`report::Report`] is versioned JSON (`schema_version`), keyed to the
//! engine's output frames and chunks, byte-identical for identical inputs,
//! and diffable ([`diff`]). Per-chunk analysis is cached under the engine's
//! chunk key, so after an edit only re-rendered chunks are re-analyzed.
//!
//! This crate reads the engine's outputs (render report, chunk masters,
//! timeline JSON) through mirror types in [`input`] and does not depend on
//! `ferrocut-engine`.

pub mod analyze;
pub mod audio;
pub mod check;
pub mod diff;
pub mod gpu;
pub mod images;
pub mod input;
pub mod media;
pub mod report;
pub mod scopes;
pub mod shots;

pub use analyze::{AudioInput, Options, Request, Stats, analyze};
pub use check::{CHECK_SCHEMA_VERSION, CheckReport, CheckThresholds, grade};
pub use diff::{Diff, diff};
pub use report::{Report, SCHEMA_VERSION};

/// The JSON Schema (draft 2020-12) of [`Report`].
pub const REPORT_SCHEMA: &str = include_str!("../schema/perceive-report.schema.json");
/// The JSON Schema of `ferrocut-perceive check --json` output ([`CheckReport`]).
pub const CHECK_SCHEMA: &str = include_str!("../schema/perceive-check.schema.json");
/// The JSON Schema of an audio chunk cache entry ([`audio::AudioChunk`]).
pub const AUDIO_CHUNK_SCHEMA: &str = include_str!("../schema/perceive-audio-chunk.schema.json");
