//! EBU R128 / ITU-R BS.1770-4 measurement via the pure-Rust `ebur128` crate (MIT).

use ebur128::{EbuR128, Mode};

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Measurement {
    /// Integrated (gated) loudness, LUFS; `-inf` for silence.
    pub integrated_lufs: f64,
    /// Maximum true peak over both channels, dBTP.
    pub true_peak_dbtp: f64,
    /// Maximum sample peak over both channels, dBFS.
    pub sample_peak_dbfs: f64,
}

/// Measure planar stereo `l`/`r` at `rate` Hz.
pub fn measure(l: &[f32], r: &[f32], rate: u32) -> Result<Measurement, String> {
    let e = |e: ebur128::Error| format!("ebur128: {e:?}");
    let mut m = EbuR128::new(2, rate, Mode::I | Mode::TRUE_PEAK | Mode::SAMPLE_PEAK).map_err(e)?;
    m.add_frames_planar_f32(&[l, r]).map_err(e)?;
    let tp = m.true_peak(0).map_err(e)?.max(m.true_peak(1).map_err(e)?);
    let sp = m
        .sample_peak(0)
        .map_err(e)?
        .max(m.sample_peak(1).map_err(e)?);
    Ok(Measurement {
        integrated_lufs: m.loudness_global().map_err(e)?,
        true_peak_dbtp: crate::gain_to_db(tp),
        sample_peak_dbfs: crate::gain_to_db(sp),
    })
}
