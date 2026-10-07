//! Chromium's 8-bit premultiplied sRGB-encoded paint buffer (BGRA) -> Ferrocut's
//! RGBA half-float working format. Same math as ferrocut-lottie's (proposed to
//! move into a shared crate).
//!
//! Default ([`OutputEncoding::AcesCg`]): unpremultiply, decode the sRGB transfer
//! function, convert Rec.709 primaries to ACEScg (AP1), re-premultiply. The frame
//! is then tagged `ACEScg`, like every other layer in the working space, so no
//! per-layer color node is needed. Chromium (run with --force-color-profile=srgb)
//! composites in encoded sRGB, so the decode happens *after* its blending: this
//! reproduces what the page looks like in a browser.
//!
//! Determinism: the transfer function is evaluated once per (alpha, value) pair
//! in f64 into a 64K-entry table; the per-pixel work is table lookups plus a
//! fixed 3x3 f32 matrix (Rust never contracts to FMA), then round-to-nearest-even
//! to f16. No libm call happens per pixel, and f64 table values are far below
//! f16 resolution, so outputs are bit-identical across machines in practice.

use half::f16;
use std::sync::OnceLock;

/// OCIO colorspace names (built-in `cg-config-v4.0.0_aces-v2.0_ocio-v2.5`).
pub const ACESCG: &str = "ACEScg";
pub const SRGB_ENCODED: &str = "sRGB Encoded Rec.709 (sRGB)";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OutputEncoding {
    /// Linear ACEScg, premultiplied. Tagged [`ACESCG`].
    #[default]
    AcesCg,
    /// Chromium's sRGB-encoded values as-is (premultiplied, /255). Tagged
    /// [`SRGB_ENCODED`]; leave the conversion to an OCIO node.
    SrgbEncoded,
}

impl OutputEncoding {
    pub fn colorspace(self) -> &'static str {
        match self {
            OutputEncoding::AcesCg => ACESCG,
            OutputEncoding::SrgbEncoded => SRGB_ENCODED,
        }
    }
}

/// Linear Rec.709 -> ACEScg, as evaluated by OCIO 2.5's built-in CG config
/// (`Linear Rec.709 (sRGB)` -> `ACEScg`; Bradford D65->D60).
// Digits as printed by OCIO (f64); the f32 rounding is intended.
#[allow(clippy::excessive_precision)]
pub const REC709_TO_ACESCG: [[f32; 3]; 3] = [
    [0.613_097_43, 0.339_523_14, 0.047_379_453],
    [0.070_193_72, 0.916_353_9, 0.013_452_399],
    [0.020_615_593, 0.109_569_77, 0.869_814_6],
];

pub fn srgb_eotf(v: f64) -> f64 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// `table[(a << 8) | c]` = premultiplied linear value of premultiplied encoded `c`
/// at alpha `a` (i.e. `a/255 * eotf(c/a)`).
fn premult_linear_table() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![0.0f32; 65536];
        for a in 1..256usize {
            for c in 0..256usize {
                let c = c.min(a);
                let alpha = a as f64 / 255.0;
                t[(a << 8) | c] = (alpha * srgb_eotf(c as f64 / a as f64)) as f32;
            }
            // values above alpha (invalid premultiplied input) clamp to alpha
            for c in a + 1..256 {
                t[(a << 8) | c] = t[(a << 8) | a];
            }
        }
        t
    })
}

/// Convert `src` (4 bytes per pixel: B,G,R,A, premultiplied, as CEF paints)
/// into tightly packed RGBA f16, premultiplied.
pub fn convert_bgra(src: &[u8], encoding: OutputEncoding, out: &mut Vec<f16>) {
    out.clear();
    out.reserve(src.len());
    match encoding {
        OutputEncoding::SrgbEncoded => {
            for px in src.as_chunks::<4>().0 {
                for v in [px[2], px[1], px[0], px[3]] {
                    out.push(f16::from_f32(v as f32 / 255.0));
                }
            }
        }
        OutputEncoding::AcesCg => {
            let t = premult_linear_table();
            let m = &REC709_TO_ACESCG;
            for px in src.as_chunks::<4>().0 {
                let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
                if a == 0 {
                    out.extend_from_slice(&[f16::ZERO; 4]);
                    continue;
                }
                let base = (a as usize) << 8;
                let (r, g, b) = (t[base | r as usize], t[base | g as usize], t[base | b as usize]);
                out.push(f16::from_f32(m[0][0] * r + m[0][1] * g + m[0][2] * b));
                out.push(f16::from_f32(m[1][0] * r + m[1][1] * g + m[1][2] * b));
                out.push(f16::from_f32(m[2][0] * r + m[2][1] * g + m[2][2] * b));
                out.push(f16::from_f32(a as f32 / 255.0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_stays_white_and_black_is_zero() {
        let mut out = Vec::new();
        convert_bgra(&[255, 255, 255, 255, 0, 0, 0, 0], OutputEncoding::AcesCg, &mut out);
        for v in &out[..4] {
            assert!((v.to_f32() - 1.0).abs() < 1e-3, "{v}");
        }
        assert!(out[4..].iter().all(|v| *v == f16::ZERO));
    }

    #[test]
    fn premultiplied_half_alpha_matches_reference() {
        // 50% alpha orange: straight sRGB (255, 128, 0) -> premult (128, 64, 0, 128)
        let mut out = Vec::new();
        convert_bgra(&[0, 64, 128, 128], OutputEncoding::AcesCg, &mut out);
        let a = 128.0 / 255.0;
        let lin = [srgb_eotf(1.0), srgb_eotf(64.0 / 128.0), 0.0];
        for (row, got) in REC709_TO_ACESCG.iter().zip(&out[..3]) {
            let want: f64 = row.iter().zip(lin).map(|(m, l)| *m as f64 * l).sum::<f64>() * a;
            assert!((got.to_f64() - want).abs() <= want * 1e-3 + 1e-4, "{got} vs {want}");
        }
        assert!((out[3].to_f64() - a).abs() < 1e-3);
    }

    #[test]
    fn bgra_channel_order() {
        let mut out = Vec::new();
        convert_bgra(&[255, 0, 0, 255], OutputEncoding::SrgbEncoded, &mut out); // pure blue
        let v: Vec<f32> = out.iter().map(|h| h.to_f32()).collect();
        assert_eq!(v, vec![0.0, 0.0, 1.0, 1.0]);
    }
}
