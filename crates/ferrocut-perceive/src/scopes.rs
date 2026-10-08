//! Scope math on 8-bit BGRZ frames (the engine's FFV1 masters), all in
//! integers so the GPU pass ([`crate::gpu`]) and this CPU reference produce
//! identical counts, and every summary is a pure function of those counts.
//!
//! Values are the encoded (Rec.709 OETF, full range) 8-bit codes the engine
//! writes. Luma and chroma use BT.709 coefficients on encoded values (Y'CbCr),
//! as broadcast scopes do:
//! - `Y' = round((2126 R + 7152 G + 722 B) / 10000)`
//! - `Cb = (B' − Y') / 1.8556`, `Cr = (R' − Y') / 1.5748`, each in [−0.5, 0.5],
//!   binned to 256 (truncating integer division, identical in WGSL and Rust).

use serde::{Deserialize, Serialize};

/// Thumbnail grid used for shot detection (cell means of R, G, B).
pub const THUMB_W: usize = 32;
pub const THUMB_H: usize = 18;
/// Waveform / parade columns (frame width is binned to this many columns).
pub const WAVE_COLS: usize = 256;

pub const HIST_LEN: usize = 4 * 256; // luma, R, G, B
pub const THUMB_LEN: usize = THUMB_W * THUMB_H * 3;
pub const WAVE_LEN: usize = WAVE_COLS * 256;
pub const PARADE_LEN: usize = 3 * WAVE_LEN;
pub const VEC_LEN: usize = 256 * 256;

/// Offsets into the flat counter array shared with the GPU shader.
pub const OFF_HIST: usize = 0;
pub const OFF_THUMB: usize = OFF_HIST + HIST_LEN;
pub const OFF_WAVE: usize = OFF_THUMB + THUMB_LEN;
pub const OFF_PARADE: usize = OFF_WAVE + WAVE_LEN;
pub const OFF_VEC: usize = OFF_PARADE + PARADE_LEN;
/// Counters for every frame (histograms + thumbnail).
pub const BASIC_LEN: usize = OFF_WAVE;
/// Counters for a sampled frame (+ waveform, parade, vectorscope).
pub const FULL_LEN: usize = OFF_VEC + VEC_LEN;

const CB_DIV: i32 = 18556 * 255;
const CR_DIV: i32 = 15748 * 255;

#[inline]
fn y10k(r: u32, g: u32, b: u32) -> u32 {
    2126 * r + 7152 * g + 722 * b
}

#[inline]
fn chroma_bin(num: i32, div: i32) -> usize {
    // floor((c + 0.5) * 256) with c = num / div; truncating division like WGSL.
    ((num * 256 + 128 * div) / div).clamp(0, 255) as usize
}

/// Counts for one frame: the CPU reference of the GPU shader. `px` is BGRZ
/// (byte 0 = B), `width * height * 4` bytes. Returns [`BASIC_LEN`] or
/// [`FULL_LEN`] counters.
pub fn counts_cpu(px: &[u8], width: usize, height: usize, full: bool) -> Vec<u32> {
    assert_eq!(px.len(), width * height * 4, "frame size");
    let mut c = vec![0u32; if full { FULL_LEN } else { BASIC_LEN }];
    for y in 0..height {
        let cy = y * THUMB_H / height;
        for x in 0..width {
            let i = (y * width + x) * 4;
            let (b, g, r) = (px[i] as u32, px[i + 1] as u32, px[i + 2] as u32);
            let yk = y10k(r, g, b);
            let yl = ((yk + 5000) / 10000) as usize;
            c[OFF_HIST + yl] += 1;
            c[OFF_HIST + 256 + r as usize] += 1;
            c[OFF_HIST + 512 + g as usize] += 1;
            c[OFF_HIST + 768 + b as usize] += 1;
            let cx = x * THUMB_W / width;
            let t = OFF_THUMB + (cy * THUMB_W + cx) * 3;
            c[t] += r;
            c[t + 1] += g;
            c[t + 2] += b;
            if full {
                let col = x * WAVE_COLS / width;
                c[OFF_WAVE + col * 256 + yl] += 1;
                c[OFF_PARADE + col * 256 + r as usize] += 1;
                c[OFF_PARADE + WAVE_LEN + col * 256 + g as usize] += 1;
                c[OFF_PARADE + 2 * WAVE_LEN + col * 256 + b as usize] += 1;
                let cb = chroma_bin(b as i32 * 10000 - yk as i32, CB_DIV);
                let cr = chroma_bin(r as i32 * 10000 - yk as i32, CR_DIV);
                c[OFF_VEC + cr * 256 + cb] += 1;
            }
        }
    }
    c
}

/// Per-frame signature for shot detection: luma histogram, thumbnail cell
/// means and a content hash (exact freeze detection).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// 256 luma counts.
    pub luma_hist: Vec<u32>,
    /// `THUMB_W * THUMB_H * 3` cell means (R, G, B), 8-bit.
    pub thumb: Vec<u8>,
    /// First 16 bytes of blake3 of the frame, hex.
    pub hash: String,
}

/// Cell pixel counts for the thumbnail grid (cells differ in size when the
/// frame doesn't divide evenly).
pub fn thumb_cell_pixels(width: usize, height: usize) -> Vec<u32> {
    let mut n = vec![0u32; THUMB_W * THUMB_H];
    let cols: Vec<usize> = (0..width).map(|x| x * THUMB_W / width).collect();
    for y in 0..height {
        let cy = y * THUMB_H / height;
        for &cx in &cols {
            n[cy * THUMB_W + cx] += 1;
        }
    }
    n
}

pub fn signature(counts: &[u32], cell_px: &[u32], frame: &[u8]) -> Signature {
    let thumb = counts[OFF_THUMB..OFF_THUMB + THUMB_LEN]
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let n = cell_px[i / 3].max(1);
            ((s + n / 2) / n).min(255) as u8
        })
        .collect();
    let h = blake3::hash(frame);
    Signature {
        luma_hist: counts[OFF_HIST..OFF_HIST + 256].to_vec(),
        thumb,
        hash: hex(&h.as_bytes()[..16]),
    }
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Round to `d` decimals so reports stay stable and diffable.
pub fn round(x: f64, d: i32) -> f64 {
    let p = 10f64.powi(d);
    let r = (x * p).round() / p;
    if r == 0.0 { 0.0 } else { r } // no "-0.0"
}

/// Level statistics from luma and RGB histograms (values are 0..1 codes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Levels {
    /// 1st / 99th percentile luma (robust black / white level).
    pub luma_black: f64,
    pub luma_white: f64,
    pub luma_mean: f64,
    pub luma_median: f64,
    /// % of pixels with Y' ≥ 253/255 (clipped highlights).
    pub luma_clipped_pct: f64,
    /// % of pixels with Y' ≤ 2/255 (crushed shadows).
    pub luma_crushed_pct: f64,
    /// Per-channel 1st / 99th percentiles (parade black / white), [R, G, B].
    pub rgb_black: [f64; 3],
    pub rgb_white: [f64; 3],
    pub rgb_mean: [f64; 3],
    /// % of pixels at code 255 / code 0 per channel.
    pub rgb_clipped_pct: [f64; 3],
    pub rgb_crushed_pct: [f64; 3],
}

fn percentile(h: &[u64], total: u64, p: f64) -> f64 {
    // Smallest code whose cumulative count reaches p * total.
    let target = ((total as f64) * p).ceil().max(1.0) as u64;
    let mut acc = 0;
    for (i, &c) in h.iter().enumerate() {
        acc += c;
        if acc >= target {
            return i as f64 / 255.0;
        }
    }
    1.0
}

fn mean(h: &[u64], total: u64) -> f64 {
    h.iter()
        .enumerate()
        .map(|(i, &c)| i as u64 * c)
        .sum::<u64>() as f64
        / total.max(1) as f64
        / 255.0
}

/// `hist`: [`HIST_LEN`] counters (luma, R, G, B), possibly summed over frames.
pub fn levels(hist: &[u64]) -> Levels {
    let total: u64 = hist[..256].iter().sum();
    let pct = |n: u64| round(100.0 * n as f64 / total.max(1) as f64, 3);
    let ch = |k: usize| &hist[256 * (k + 1)..256 * (k + 2)];
    let l = &hist[..256];
    Levels {
        luma_black: round(percentile(l, total, 0.01), 4),
        luma_white: round(percentile(l, total, 0.99), 4),
        luma_mean: round(mean(l, total), 4),
        luma_median: round(percentile(l, total, 0.5), 4),
        luma_clipped_pct: pct(l[253..].iter().sum()),
        luma_crushed_pct: pct(l[..3].iter().sum()),
        rgb_black: std::array::from_fn(|k| round(percentile(ch(k), total, 0.01), 4)),
        rgb_white: std::array::from_fn(|k| round(percentile(ch(k), total, 0.99), 4)),
        rgb_mean: std::array::from_fn(|k| round(mean(ch(k), total), 4)),
        rgb_clipped_pct: std::array::from_fn(|k| pct(ch(k)[255])),
        rgb_crushed_pct: std::array::from_fn(|k| pct(ch(k)[0])),
    }
}

/// Vectorscope statistics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorStats {
    /// Chroma magnitude `sqrt(Cb² + Cr²)` (0 = neutral, ≈0.5 = fully saturated primary).
    pub chroma_mean: f64,
    pub chroma_p95: f64,
    /// % of pixels with chroma > 0.35 (very saturated; risky for broadcast).
    pub saturated_pct: f64,
    /// Mean hue angle (degrees, vectorscope convention: 0° = +Cb, counter-
    /// clockwise towards +Cr) weighted by chroma; `null` if the frame is neutral.
    pub hue_mean_deg: Option<f64>,
    /// Pixels within ±25° of the skin-tone line (123°) with chroma > 0.03.
    pub skin_pct: f64,
    /// Their mean signed deviation from the skin-tone line (degrees; positive
    /// = towards red/magenta, negative = towards yellow/green); `null` if none.
    pub skin_line_dev_deg: Option<f64>,
}

/// Vectorscope bin centre → (Cb, Cr) in [−0.5, 0.5].
fn bin_centre(i: usize) -> f64 {
    (i as f64 + 0.5) / 256.0 - 0.5
}

pub const SKIN_LINE_DEG: f64 = 123.0;

pub fn color_stats(vec: &[u32]) -> ColorStats {
    let total: u64 = vec.iter().map(|&c| c as u64).sum();
    let mut chroma_hist = vec![0u64; 512]; // chroma in [0, 0.71] at 1/720 resolution
    let (mut sum_c, mut sat, mut skin_n, mut skin_dev) = (0.0f64, 0u64, 0u64, 0.0f64);
    let (mut hx, mut hy) = (0.0f64, 0.0f64);
    for cr_i in 0..256 {
        for cb_i in 0..256 {
            let n = vec[cr_i * 256 + cb_i] as u64;
            if n == 0 {
                continue;
            }
            let (cb, cr) = (bin_centre(cb_i), bin_centre(cr_i));
            let c = (cb * cb + cr * cr).sqrt();
            sum_c += c * n as f64;
            chroma_hist[((c * 720.0) as usize).min(511)] += n;
            if c > 0.35 {
                sat += n;
            }
            hx += cb * n as f64;
            hy += cr * n as f64;
            if c > 0.03 {
                let ang = cr.atan2(cb).to_degrees();
                let dev = ang - SKIN_LINE_DEG;
                if dev.abs() <= 25.0 {
                    skin_n += n;
                    skin_dev += dev * n as f64;
                }
            }
        }
    }
    let t = total.max(1) as f64;
    let target = ((total as f64) * 0.95).ceil() as u64;
    let mut acc = 0;
    let mut p95 = 0.0;
    for (i, &c) in chroma_hist.iter().enumerate() {
        acc += c;
        if acc >= target {
            p95 = (i as f64 + 0.5) / 720.0;
            break;
        }
    }
    let hue = if (hx * hx + hy * hy).sqrt() / t > 0.005 {
        let a = hy.atan2(hx).to_degrees();
        Some(round(if a < 0.0 { a + 360.0 } else { a }, 1))
    } else {
        None
    };
    ColorStats {
        chroma_mean: round(sum_c / t, 4),
        chroma_p95: round(p95, 4),
        saturated_pct: round(100.0 * sat as f64 / t, 3),
        hue_mean_deg: hue,
        skin_pct: round(100.0 * skin_n as f64 / t, 3),
        skin_line_dev_deg: (skin_n > 0).then(|| round(skin_dev / skin_n as f64, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, r: u8, g: u8, b: u8) -> Vec<u8> {
        (0..w * h).flat_map(|_| [b, g, r, 255]).collect()
    }

    #[test]
    fn primaries_land_where_a_vectorscope_puts_them() {
        let c = counts_cpu(&solid(8, 8, 255, 0, 0), 8, 8, true);
        let s = color_stats(&c[OFF_VEC..OFF_VEC + VEC_LEN]);
        // Rec.709 red: Cb = -0.1146, Cr = 0.5 -> ~103 degrees, chroma ~0.51.
        assert!((s.hue_mean_deg.unwrap() - 103.0).abs() < 1.5, "{s:?}");
        assert!(s.chroma_mean > 0.49 && s.saturated_pct == 100.0, "{s:?}");
        let g = counts_cpu(&solid(8, 8, 128, 128, 128), 8, 8, true);
        let s = color_stats(&g[OFF_VEC..OFF_VEC + VEC_LEN]);
        assert!(
            s.chroma_mean < 0.003 && s.hue_mean_deg.is_none() && s.skin_pct == 0.0,
            "{s:?}"
        );
        // A typical skin tone sits near the I line.
        let k = counts_cpu(&solid(8, 8, 224, 172, 140), 8, 8, true);
        let s = color_stats(&k[OFF_VEC..OFF_VEC + VEC_LEN]);
        assert_eq!(s.skin_pct, 100.0, "{s:?}");
        assert!(s.skin_line_dev_deg.unwrap().abs() < 10.0, "{s:?}");
    }

    #[test]
    fn levels_of_a_ramp() {
        let w = 256;
        let px: Vec<u8> = (0..w)
            .flat_map(|x| [x as u8, x as u8, x as u8, 255])
            .collect();
        let c = counts_cpu(&px, w, 1, false);
        let l = levels(&c[..HIST_LEN].iter().map(|&v| v as u64).collect::<Vec<_>>());
        assert_eq!(l.luma_black, round(2.0 / 255.0, 4)); // ceil(2.56) = 3rd code
        assert_eq!(l.luma_white, round(253.0 / 255.0, 4));
        assert_eq!(l.luma_clipped_pct, round(300.0 / 256.0, 3));
        assert_eq!(l.rgb_clipped_pct, [round(100.0 / 256.0, 3); 3]);
        assert!((l.luma_mean - 0.5).abs() < 0.003);
    }

    #[test]
    fn thumbnail_means_and_cells() {
        let (w, h) = (70, 37); // uneven cells
        let px = solid(w, h, 10, 20, 30);
        let c = counts_cpu(&px, w, h, false);
        let cells = thumb_cell_pixels(w, h);
        assert_eq!(cells.iter().sum::<u32>(), (w * h) as u32);
        let s = signature(&c, &cells, &px);
        assert!(s.thumb.chunks(3).all(|p| p == [10, 20, 30]));
    }
}
