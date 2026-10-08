//! Ferrocut's audio engine core: a deterministic, offline mixer.
//!
//! The engine resolves a timeline into a [`Program`] (sample-domain clip
//! regions on tracks, gain/pan [`Animatable`]s, fades, ducking, master
//! settings) plus decoded [`SourceAudio`] at the project rate; this crate
//! mixes it. It does no I/O and has no FFmpeg or GPU dependency.
//!
//! Rendering is two-stage so that any sample range can be rendered on its own
//! and still be bit-identical to the same range of a whole-program render:
//!
//! 1. [`analyze`] runs once over the whole program, sequentially: it computes
//!    the sidechain duck gain curves, measures the pre-normalization master
//!    loudness (EBU R128 integrated, via the pure-Rust `ebur128` crate), and
//!    derives the normalization gain and the true-peak limiter gain curve.
//!    Everything with state or look-ahead (envelope followers, limiter,
//!    measurement) lives here and is stored per sample in [`Control`].
//! 2. [`render_range`] is stateless: each output sample is a fixed-order
//!    function of the sources, the program and [`Control`] at that sample, so
//!    chunked rendering equals unchunked rendering sample for sample.
//!
//! Determinism: f32 sample accumulation in a fixed order (tracks, then clips
//! in program order), f64 gain math, no fast-math, no threads in a reduction.
//! Results are bit-exact run to run on the same build and machine (libm
//! `sin`/`cos`/`exp`/`log10`/`powf` are the only transcendental calls).
//!
//! Known gap: [`Control`] and the sources are held in memory for the whole
//! program (about 11.5 MB per minute per stereo f32 buffer). Snapshotting the
//! detector/limiter state at chunk boundaries would make it O(chunk).

pub mod analysis;
pub mod dynamics;
pub mod loudness;
pub mod mix;
pub mod program;

pub use analysis::{AnalysisReport, Control, analyze};
pub use ferrocut_types::Animatable;
pub use loudness::{Measurement, measure};
pub use mix::{Stereo, render_range, render_track};
pub use program::{
    ClipProg, Duck, Fade, FadeCurve, LoudnessTarget, Program, SourceAudio, TrackProg,
};

/// Part of the engine's audio cache keys: bump when the mix of the same
/// program changes.
pub const VERSION: &str = concat!("ferrocut-audio ", env!("CARGO_PKG_VERSION"), " mix.v1");

/// Linear gain of `db` decibels.
pub fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Decibels of a linear gain (or level); `-inf` for 0.
pub fn gain_to_db(g: f64) -> f64 {
    20.0 * g.log10()
}

/// Sample index of time `t` at `rate` Hz: exact rational product, halves
/// rounded away from zero (the same rule as frame/pts rounding on the video side).
pub fn sample_at(t: ferrocut_types::RationalTime, rate: u32) -> i64 {
    t.frame_round(ferrocut_types::Rational::from_int(rate as i64))
}
