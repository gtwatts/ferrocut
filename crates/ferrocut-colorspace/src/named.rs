//! Conversions keyed by color-space **name**: the names frames are tagged
//! with (`ferrocut_types::ColorSpace::name()`), i.e. OCIO colorspace names or
//! aliases from OCIO 2.5's built-in ACES configs.
//!
//! ```
//! use ferrocut_colorspace::named::{self, names};
//! let m = named::matrix(names::LINEAR_REC709, names::ACESCG).unwrap(); // [[f32; 3]; 3], row-major
//! let t = named::transfer("sRGB - Texture").unwrap(); // OCIO aliases work too
//! let lin = t.decode([0.5, 0.5, 0.5]);
//! assert!((lin[0] - 0.21404114).abs() < 1e-7);
//! // GPU: prepend wgsl(), then call the matching functions by name.
//! let src = format!("{}({}(c))", named::wgsl_matrix_fn(names::LINEAR_REC709, names::ACESCG).unwrap(), t.wgsl_decode_fn());
//! assert_eq!(src, "fc_rec709_to_acescg(fc_srgb_to_linear(c))");
//! ```
//!
//! Only scene-referred spaces are listed. OCIO's display spaces (`sRGB -
//! Display`, `Rec.1886 Rec.709 - Display`, ...) are reached from scene
//! spaces through a view transform (tone mapping), which is not a matrix plus
//! a curve; use a `ferrocut-color` node for those.

use crate::{ACESCG_TO_REC709_F32, Gamut, REC709_TO_ACESCG_F32, Space, Transfer};

/// Canonical names (OCIO 2.5 built-in config names).
pub mod names {
    /// Linear AP1, the working space.
    pub const ACESCG: &str = "ACEScg";
    pub const LINEAR_REC709: &str = "Linear Rec.709 (sRGB)";
    /// Rec.709 primaries, sRGB (IEC 61966-2-1) curve.
    pub const SRGB_ENCODED_REC709: &str = "sRGB Encoded Rec.709 (sRGB)";
    pub const GAMMA22_REC709: &str = "Gamma 2.2 Encoded Rec.709";
    /// Rec.709 primaries, pure 2.4 power: BT.1886 with a zero black level.
    pub const GAMMA24_REC709: &str = "Gamma 2.4 Encoded Rec.709";
    /// Rec.709 primaries, BT.709 camera OETF (decoded video). In OCIO's studio
    /// config (not the default CG config).
    pub const CAMERA_REC709: &str = "Camera Rec.709";
}

/// `(name or alias, space)`. Aliases are OCIO 2.5's for the same colorspace.
const TABLE: &[(&str, Space)] = &[
    (names::ACESCG, Space::ACESCG),
    ("ACES - ACEScg", Space::ACESCG),
    ("lin_ap1", Space::ACESCG),
    ("lin_ap1_scene", Space::ACESCG),
    (names::LINEAR_REC709, Space::LINEAR_REC709),
    ("lin_rec709_srgb", Space::LINEAR_REC709),
    ("lin_rec709", Space::LINEAR_REC709),
    ("lin_rec709_scene", Space::LINEAR_REC709),
    ("lin_srgb", Space::LINEAR_REC709),
    ("Utility - Linear - sRGB", Space::LINEAR_REC709),
    ("Utility - Linear - Rec.709", Space::LINEAR_REC709),
    (names::SRGB_ENCODED_REC709, Space::SRGB),
    ("srgb_encoded_rec709_srgb", Space::SRGB),
    ("srgb_texture", Space::SRGB),
    ("srgb_rec709_scene", Space::SRGB),
    ("Utility - sRGB - Texture", Space::SRGB),
    ("Input - Generic - sRGB - Texture", Space::SRGB),
    ("sRGB - Texture", Space::SRGB),
    ("srgb_tx", Space::SRGB),
    (names::GAMMA22_REC709, Space::REC709_GAMMA22),
    ("g22_encoded_rec709", Space::REC709_GAMMA22),
    ("g22_rec709", Space::REC709_GAMMA22),
    ("Utility - Gamma 2.2 - Rec.709 - Texture", Space::REC709_GAMMA22),
    ("Gamma 2.2 Rec.709 - Texture", Space::REC709_GAMMA22),
    ("g22_rec709_tx", Space::REC709_GAMMA22),
    ("g22_rec709_scene", Space::REC709_GAMMA22),
    (names::GAMMA24_REC709, Space::REC709_GAMMA24),
    ("g24_encoded_rec709", Space::REC709_GAMMA24),
    ("g24_rec709", Space::REC709_GAMMA24),
    ("rec709_display", Space::REC709_GAMMA24),
    ("Utility - Rec.709 - Display", Space::REC709_GAMMA24),
    ("Gamma 2.4 Rec.709 - Texture", Space::REC709_GAMMA24),
    ("g24_rec709_tx", Space::REC709_GAMMA24),
    ("ocio:g24_rec709_scene", Space::REC709_GAMMA24),
    (names::CAMERA_REC709, Space::REC709_BT709),
    ("camera_rec709", Space::REC709_BT709),
    ("rec709_camera", Space::REC709_BT709),
    ("Utility - Rec.709 - Camera", Space::REC709_BT709),
    ("ocio:itu709_rec709_scene", Space::REC709_BT709),
];

/// A name this crate doesn't know (not scene-referred Rec.709/ACEScg, or a
/// typo). Names are matched exactly, as OCIO does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownSpace(pub String);

impl std::fmt::Display for UnknownSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown color space {:?} (ferrocut-colorspace knows ACEScg and scene-referred Rec.709 spaces)",
            self.0
        )
    }
}

impl std::error::Error for UnknownSpace {}

/// Every accepted name and alias with its space.
pub fn all() -> impl Iterator<Item = (&'static str, Space)> {
    TABLE.iter().copied()
}

/// The space called `name`.
pub fn space(name: &str) -> Result<Space, UnknownSpace> {
    TABLE.iter().find(|(n, _)| *n == name).map(|(_, s)| *s).ok_or_else(|| UnknownSpace(name.into()))
}

/// Row-major f32 matrix from linear `from` to linear `to` primaries (Bradford
/// D65 → ACES white, OCIO's digits); identity when the primaries match. The
/// same constants as `FC_*` in [`wgsl()`](crate::wgsl).
pub fn matrix(from: &str, to: &str) -> Result<[[f32; 3]; 3], UnknownSpace> {
    Ok(gamut_matrix_f32(space(from)?.gamut, space(to)?.gamut))
}

/// The transfer function of `space` ([`Transfer::Linear`] for linear spaces).
pub fn transfer(space: &str) -> Result<Transfer, UnknownSpace> {
    Ok(self::space(space)?.transfer)
}

/// Name of the [`wgsl()`](crate::wgsl) function applying [`matrix`]`(from, to)`.
pub fn wgsl_matrix_fn(from: &str, to: &str) -> Result<&'static str, UnknownSpace> {
    Ok(match (space(from)?.gamut, space(to)?.gamut) {
        (Gamut::Rec709, Gamut::AcesCg) => "fc_rec709_to_acescg",
        (Gamut::AcesCg, Gamut::Rec709) => "fc_acescg_to_rec709",
        _ => "fc_identity",
    })
}

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn gamut_matrix_f32(from: Gamut, to: Gamut) -> [[f32; 3]; 3] {
    match (from, to) {
        (Gamut::Rec709, Gamut::AcesCg) => REC709_TO_ACESCG_F32,
        (Gamut::AcesCg, Gamut::Rec709) => ACESCG_TO_REC709_F32,
        _ => IDENTITY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_and_matrices_match_the_f32_constants() {
        assert_eq!(matrix(names::LINEAR_REC709, names::ACESCG).unwrap(), REC709_TO_ACESCG_F32);
        assert_eq!(matrix("lin_ap1", "sRGB - Texture").unwrap(), ACESCG_TO_REC709_F32);
        assert_eq!(matrix(names::CAMERA_REC709, names::GAMMA24_REC709).unwrap(), IDENTITY);
        assert_eq!(transfer(names::ACESCG).unwrap(), Transfer::Linear);
        assert_eq!(transfer(names::SRGB_ENCODED_REC709).unwrap(), Transfer::Srgb);
        assert_eq!(transfer(names::GAMMA24_REC709).unwrap(), Transfer::BT1886);
        assert_eq!(transfer(names::CAMERA_REC709).unwrap(), Transfer::Bt709);
        assert!(matches!(space("sRGB - Display"), Err(UnknownSpace(n)) if n == "sRGB - Display"));
        assert!(space("acescg").is_err(), "exact match, like OCIO");
        // The canonical names agree with Space::ocio_name where it has one.
        for (n, s) in all() {
            if let Some(o) = s.ocio_name() {
                assert_eq!(space(o).unwrap(), s, "{n}");
            }
        }
        let mut seen = std::collections::HashSet::new();
        assert!(all().all(|(n, _)| seen.insert(n)), "duplicate name");
    }

    #[test]
    fn wgsl_function_names_exist() {
        let src = crate::wgsl();
        for (from, _) in all() {
            for (to, _) in all() {
                let f = wgsl_matrix_fn(from, to).unwrap();
                assert!(src.contains(&format!("fn {f}(")), "{f}");
            }
            let t = transfer(from).unwrap();
            for f in [t.wgsl_decode_fn(), t.wgsl_encode_fn()] {
                assert!(src.contains(&format!("fn {f}(")), "{f}");
            }
        }
    }

    #[test]
    fn decode_encode_are_the_f64_reference() {
        for (_, s) in all() {
            let t = s.transfer;
            for v in [0.0f32, 0.01, 0.0405, 0.081, 0.5, 1.0] {
                let d = t.decode([v; 3])[0];
                assert_eq!(d, t.to_linear(v as f64) as f32);
                assert!((t.encode([d; 3])[0] - v).abs() < 2e-6 || t == Transfer::Bt709, "{t:?} {v}");
            }
        }
    }
}
