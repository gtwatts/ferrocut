//! ThorVG's 8-bit premultiplied sRGB-encoded output -> Ferrocut's RGBA half-float
//! working format.
//!
//! Default ([`OutputEncoding::AcesCg`]): unpremultiply, decode the sRGB transfer
//! function, convert Rec.709 primaries to ACEScg (AP1), re-premultiply. The frame
//! is then tagged `ACEScg`, like every other layer in the working space, so no
//! per-layer color node is needed. Lottie (like After Effects in 8-bit mode)
//! composites in encoded sRGB, so the decode happens *after* ThorVG's blending:
//! this reproduces the look the animator saw.
//!
//! The math lives in `ferrocut-colorspace` (shared with the engine; validated
//! against OCIO 2.5 in ferrocut-color's tests). Determinism: per-pixel work is
//! table lookups plus a fixed f32 matrix, so outputs are bit-identical across
//! machines (see `ferrocut_colorspace::pixels`).

use ferrocut_colorspace::{Transfer, pixels};
use half::f16;

/// OCIO colorspace names (built-in `cg-config-v4.0.0_aces-v2.0_ocio-v2.5`).
pub const ACESCG: &str = "ACEScg";
pub const SRGB_ENCODED: &str = "sRGB Encoded Rec.709 (sRGB)";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OutputEncoding {
    /// Linear ACEScg, premultiplied. Tagged [`ACESCG`].
    #[default]
    AcesCg,
    /// ThorVG's sRGB-encoded values as-is (premultiplied, /255). Tagged
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

/// Linear Rec.709 -> ACEScg (OCIO 2.5's values) as the f32 literals this
/// node has always used; see `ferrocut_colorspace::pixels::LEGACY_REC709_TO_ACESCG_F32`.
pub const REC709_TO_ACESCG: [[f32; 3]; 3] = pixels::LEGACY_REC709_TO_ACESCG_F32;

/// sRGB decoding (IEC 61966-2-1), in f64.
pub fn srgb_eotf(v: f64) -> f64 {
    Transfer::Srgb.to_linear(v)
}

/// Convert `src` (one u32 per pixel: little-endian bytes R,G,B,A, premultiplied)
/// into tightly packed RGBA f16, premultiplied.
pub fn convert(src: &[u32], encoding: OutputEncoding, out: &mut Vec<f16>) {
    let px = src.iter().map(|p| p.to_le_bytes());
    match encoding {
        OutputEncoding::SrgbEncoded => pixels::rgba8_to_f16(px, out),
        OutputEncoding::AcesCg => pixels::srgb8_premul_to_acescg_f16(px, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_stays_white_and_black_is_zero() {
        let mut out = Vec::new();
        convert(&[u32::from_le_bytes([255, 255, 255, 255]), 0], OutputEncoding::AcesCg, &mut out);
        for v in &out[..4] {
            assert!((v.to_f32() - 1.0).abs() < 1e-3, "{v}");
        }
        assert!(out[4..].iter().all(|v| *v == f16::ZERO));
    }

    #[test]
    fn premultiplied_half_alpha_matches_reference() {
        // 50% alpha orange: straight sRGB (255, 128, 0) -> premult (128, 64, 0, 128)
        let mut out = Vec::new();
        convert(&[u32::from_le_bytes([128, 64, 0, 128])], OutputEncoding::AcesCg, &mut out);
        let a = 128.0 / 255.0;
        let lin = [srgb_eotf(1.0), srgb_eotf(64.0 / 128.0), 0.0];
        for (row, got) in REC709_TO_ACESCG.iter().zip(&out[..3]) {
            let want: f64 = row.iter().zip(lin).map(|(m, l)| *m as f64 * l).sum::<f64>() * a;
            assert!((got.to_f64() - want).abs() <= want * 1e-3 + 1e-4, "{got} vs {want}");
        }
        assert!((out[3].to_f64() - a).abs() < 1e-3);
    }

    #[test]
    fn colorspace_names_match_ferrocut_colorspace() {
        use ferrocut_colorspace::Space;
        assert_eq!(Space::ACESCG.ocio_name(), Some(ACESCG));
        assert_eq!(Space::SRGB.ocio_name(), Some(SRGB_ENCODED));
        // Bumping the shared color math must bump this node's NODE_VERSION too.
        assert_eq!(ferrocut_colorspace::VERSION, "ferrocut-colorspace/1");
    }
}
