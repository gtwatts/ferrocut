//! Bounded numeric color feedback using FilmCraft's actual scope algorithms.
//!
//! Input is straight display-encoded RGBA8. These are code-value scopes, not
//! scene-linear or HDR mastering measurements. Alpha is reported separately;
//! RGB is measured as stored, without compositing or changing its transfer.

use std::path::Path;

use anyhow::{Context as _, ensure};
use ferrocut_core::RationalTime;
use filmcraft_scopes::{self as fc, summary};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use fc::{ParadeType, WaveformType};

/// Bound full-resolution decoding before the upstream nearest-sample reduction.
pub const MAX_DECODE_PIXELS: usize = 16_777_216;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMatrix {
    Bt601,
    #[default]
    Bt709,
    Bt2020Ncl,
}

impl ScopeMatrix {
    fn native(self) -> fc::Matrix {
        match self {
            Self::Bt601 => fc::Matrix::Bt601,
            Self::Bt709 => fc::Matrix::Bt709,
            Self::Bt2020Ncl => fc::Matrix::Bt2020Ncl,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeOptions {
    pub matrix: ScopeMatrix,
    pub waveform: WaveformType,
    pub parade: ParadeType,
    pub columns: usize,
    pub vector_cells: usize,
    pub peaks: usize,
}

impl Default for ScopeOptions {
    fn default() -> Self {
        Self {
            matrix: ScopeMatrix::Bt709,
            waveform: WaveformType::Rgb,
            parade: ParadeType::Rgb,
            columns: 16,
            vector_cells: 16,
            peaks: 8,
        }
    }
}

impl ScopeOptions {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            (1..=64).contains(&self.columns),
            "scope columns must be 1..=64"
        );
        ensure!(
            (1..=32).contains(&self.vector_cells),
            "vector_cells must be 1..=32"
        );
        ensure!((1..=32).contains(&self.peaks), "scope peaks must be 1..=32");
        Ok(())
    }
}

fn pixel_count(width: u32, height: u32) -> anyhow::Result<usize> {
    let n = (width as usize)
        .checked_mul(height as usize)
        .context("scope frame dimensions overflow")?;
    ensure!(
        width > 0 && height > 0,
        "scope frame dimensions must be positive"
    );
    ensure!(
        n <= MAX_DECODE_PIXELS,
        "scope frame exceeds {MAX_DECODE_PIXELS} pixels"
    );
    Ok(n)
}

/// Measure one complete tightly packed frame. Dimensions and options are
/// checked before entering upstream algorithms that assume valid buffers.
pub fn analyze_rgba8(
    width: u32,
    height: u32,
    pixels: &[u8],
    options: &ScopeOptions,
) -> anyhow::Result<Value> {
    options.validate()?;
    let count = pixel_count(width, height)?;
    ensure!(
        pixels.len() == count * 4,
        "scope RGBA buffer length does not match dimensions"
    );
    let signal = fc::Signal::from_rgba8(width as usize, height as usize, pixels);
    let params = fc::Params {
        matrix: options.matrix.native(),
        ..Default::default()
    };
    let wave = fc::waveform(&signal, options.waveform, &params);
    let parade = fc::parade(&signal, options.parade, &params);
    let columns = |w: &fc::Waveform| -> Value {
        Value::Object(
            w.traces
                .iter()
                .map(|g| {
                    (
                        g.name.to_owned(),
                        json!(summary::trace_columns(w, g, options.columns)),
                    )
                })
                .collect(),
        )
    };
    let vector = |v: fc::Vectorscope| -> Value {
        json!({
            "samples":v.samples,
            "peaks":summary::peaks(&v, options.peaks, 0.0),
            "coarse_size":options.vector_cells,
            "coarse":summary::coarse(&v, options.vector_cells)
        })
    };
    let mut alpha_min = 255u8;
    let mut alpha_max = 0u8;
    let mut alpha_sum = 0u64;
    for p in pixels.as_chunks::<4>().0 {
        alpha_min = alpha_min.min(p[3]);
        alpha_max = alpha_max.max(p[3]);
        alpha_sum += p[3] as u64;
    }
    Ok(json!({
        "engine":"filmcraft-scopes",
        "revision":"5231852443363f001c3f6b396dd9b1e6461ae2be",
        "input":{"width":width,"height":height,"encoding":"display-encoded RGB8 code values; no transfer conversion",
            "alpha":{"min":alpha_min as f64 / 255.0,"max":alpha_max as f64 / 255.0,
                "mean":alpha_sum as f64 / (count as f64 * 255.0),"rgb_composited":false}},
        "sampling":{"method":"nearest sample, no averaging","width":signal.w,"height":signal.h,
            "samples":signal.len(),"max_width":fc::MAX_W,"max_height":fc::MAX_H},
        "options":options,
        "level_unit":"percent of full code scale (0..100), not calibrated display luminance",
        "channels":summary::channel_stats(&signal, params.matrix),
        "waveform":columns(&wave),
        "parade":columns(&parade),
        "histogram":fc::histogram(&signal, &params),
        "vectorscope_yuv":vector(fc::vectorscope_yuv(&signal, &params)),
        "vectorscope_hls":vector(fc::vectorscope_hls(&signal, &params))
    }))
}

/// Decode one source/render frame at source time, then measure its code values.
/// MCP callers must check the path against their project root before calling.
pub fn read(path: &Path, at: RationalTime, options: &ScopeOptions) -> anyhow::Result<Value> {
    options.validate()?;
    ensure!(at >= RationalTime::ZERO, "scope time must be nonnegative");
    let info = crate::media::probe(path)?;
    ensure!(info.has_video, "scope input has no video stream");
    if let Some(duration) = info.duration {
        ensure!(
            at < duration,
            "scope time {at} is outside the source duration {duration}"
        );
    } else {
        ensure!(
            at == RationalTime::ZERO,
            "unknown source duration: scopes only support time 0"
        );
    }
    let width = info.width.context("scope input has no width")?;
    let height = info.height.context("scope input has no height")?;
    pixel_count(width, height)?;
    let mut decoder = crate::media::decode::Decoder::open(path, width, height)?;
    let pixels = decoder.frame_at(at)?;
    let mut result = analyze_rgba8(width, height, pixels, options)?;
    result["path"] = json!(path);
    result["requested_source_time"] = json!(at);
    result["frame_selection"] =
        json!("Ferrocut decoder: frame at source time with half-frame timestamp tolerance");
    result["source_duration"] = json!(info.duration);
    Ok(result)
}
