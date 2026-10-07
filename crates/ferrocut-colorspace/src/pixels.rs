//! Deterministic 8-bit → RGBA f16 conversions for layer nodes (Lottie, HTML).
//!
//! The transfer function is evaluated once per (alpha, value) pair in f64 into
//! a 64K-entry table; per pixel there are only table lookups, a fixed 3x3 f32
//! matrix ([`LEGACY_REC709_TO_ACESCG_F32`]; Rust never contracts to FMA) and
//! round-to-nearest-even to f16. No libm call happens per
//! pixel, so output is bit-identical across machines and runs.
//!
//! Input pixels are `[r, g, b, a]` byte arrays; callers adapt their layout with
//! an iterator map (e.g. BGRA: `|&[b, g, r, a]| [r, g, b, a]`).

use std::sync::OnceLock;

use half::f16;

use crate::Transfer;

/// The f32 Rec.709 → ACEScg matrix the 8-bit paths use: the literals
/// `ferrocut.lottie/1` and `ferrocut.html/1` shipped with. Equal to
/// [`REC709_TO_ACESCG_F32`](crate::REC709_TO_ACESCG_F32) except `[2][2]`, which
/// is 1 ulp (6e-8) low. Frozen so existing node output hashes don't change;
/// switching to the exact rounding is a version bump of those nodes.
#[allow(clippy::excessive_precision)]
pub const LEGACY_REC709_TO_ACESCG_F32: [[f32; 3]; 3] = [
    [0.613_097_43, 0.339_523_14, 0.047_379_453],
    [0.070_193_72, 0.916_353_9, 0.013_452_399],
    [0.020_615_593, 0.109_569_77, 0.869_814_6],
];

/// `table[(a << 8) | c]` = premultiplied linear value of premultiplied
/// sRGB-encoded `c` at alpha `a`, i.e. `a/255 · eotf(c/a)`. Values above alpha
/// (invalid premultiplied input) clamp to alpha.
fn premult_srgb_table() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![0.0f32; 65536];
        for a in 1..256usize {
            for c in 0..=a {
                let alpha = a as f64 / 255.0;
                t[(a << 8) | c] = (alpha * Transfer::Srgb.to_linear(c as f64 / a as f64)) as f32;
            }
            for c in a + 1..256 {
                t[(a << 8) | c] = t[(a << 8) | a];
            }
        }
        t
    })
}

/// Premultiplied sRGB-encoded Rec.709 8-bit → premultiplied linear ACEScg
/// RGBA f16: unpremultiply, decode sRGB, Rec.709 → ACEScg, re-premultiply.
/// Replaces the contents of `out` (4 values per pixel).
pub fn srgb8_premul_to_acescg_f16(px: impl IntoIterator<Item = [u8; 4]>, out: &mut Vec<f16>) {
    out.clear();
    let t = premult_srgb_table();
    let m = &LEGACY_REC709_TO_ACESCG_F32;
    for [r, g, b, a] in px {
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

/// 8-bit values as-is (`v / 255`), no color conversion. Replaces `out`.
pub fn rgba8_to_f16(px: impl IntoIterator<Item = [u8; 4]>, out: &mut Vec<f16>) {
    out.clear();
    for p in px {
        for v in p {
            out.push(f16::from_f32(v as f32 / 255.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Space, convert_rgb};

    #[test]
    fn white_stays_white_and_transparent_is_zero() {
        let mut out = Vec::new();
        srgb8_premul_to_acescg_f16([[255; 4], [0; 4], [9, 9, 9, 0]], &mut out);
        assert!(out[..4].iter().all(|v| (v.to_f32() - 1.0).abs() < 1e-3), "{out:?}");
        assert!(out[4..].iter().all(|v| *v == f16::ZERO));
    }

    #[test]
    fn premultiplied_input_matches_the_f64_reference() {
        // Every (value, alpha) pair on a coarse grid, against convert_rgb in f64.
        for a in (1..=255u8).step_by(7) {
            for c in (0..=a).step_by(5) {
                let mut out = Vec::new();
                srgb8_premul_to_acescg_f16([[c, a, 0, a]], &mut out);
                let alpha = a as f64 / 255.0;
                let straight = [c as f64 / a as f64, 1.0, 0.0];
                let want = convert_rgb(Space::SRGB, Space::ACESCG, straight).map(|v| v * alpha);
                for (g, w) in out[..3].iter().zip(want) {
                    assert!((g.to_f64() - w).abs() <= w.abs() * 1e-3 + 1e-4, "c={c} a={a}: {g} vs {w}");
                }
                assert_eq!(out[3], f16::from_f32(a as f32 / 255.0));
            }
        }
    }

    #[test]
    fn raw_path_is_value_over_255() {
        let mut out = Vec::new();
        rgba8_to_f16([[0, 51, 255, 128]], &mut out);
        assert_eq!(out, [0.0, 0.2, 1.0, 128.0 / 255.0].map(f16::from_f32));
    }
}
