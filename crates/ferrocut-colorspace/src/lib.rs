//! The handful of color conversions Ferrocut needs without linking OpenColorIO:
//! Rec.709 / sRGB encoded ↔ linear Rec.709 ↔ linear ACEScg (AP1).
//!
//! - [`Transfer`]: transfer functions (sRGB, BT.709 OETF, gamma 2.2 / 2.4).
//! - [`Gamut`] / [`matrix`]: Rec.709 ↔ ACEScg primaries matrices, exactly
//!   OCIO 2.5's values ([`REC709_TO_ACESCG`], [`ACESCG_TO_REC709`]).
//! - [`Space`] / [`convert_rgb`]: gamut + transfer, with the matching OCIO
//!   colorspace name where the built-in config has one.
//! - [`named`]: the same, keyed by color-space name (`ferrocut_types::ColorSpace`
//!   names = OCIO names/aliases): `named::matrix(from, to)`,
//!   `named::transfer(space)`.
//! - [`pixels`]: deterministic 8-bit → f16 fast paths used by layer nodes.
//! - [`wgsl()`]: the same math as a WGSL snippet for shaders, generated from
//!   the Rust constants so CPU and GPU can't drift apart.
//!
//! The math is checked against OCIO 2.5 in `ferrocut-color`'s tests
//! (`tests/colorspace_vs_ocio.rs`, CPU and GPU).
//!
//! Conventions: RGB triples are straight (not premultiplied) unless a function
//! says otherwise. Matrices are row-major: `out[i] = Σ m[i][j] * in[j]`.
//! Values outside [0, 1] are allowed: piecewise curves extend their linear
//! segment below the breakpoint (including negatives) and their power segment
//! above 1; pure power curves clamp negatives to 0 (OCIO's defaults).

pub mod named;
pub mod pixels;
mod wgsl;

pub use wgsl::wgsl;

/// Bump when any conversion's output changes; put it in node hashes of nodes
/// that use this crate.
pub const VERSION: &str = "ferrocut-colorspace/1";

/// A transfer function (encoding ↔ linear light).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transfer {
    /// Identity.
    Linear,
    /// IEC 61966-2-1 sRGB piecewise curve (sRGB images, CSS, Lottie, PNG).
    Srgb,
    /// ITU-R BT.709 camera OETF (`4.5·L` below 0.018, `1.099·L^0.45 − 0.099`
    /// above) and its inverse (`V/4.5` below 0.081). The scene-referred
    /// interpretation of Rec.709 video; what the engine's `bt709_to_linear` /
    /// `linear_to_bt709` do. The standard's rounded constants leave a gap:
    /// encoded values in [0.081, 0.08124) decode slightly below 0.018 and
    /// re-encode up to 2.5e-4 lower. This matches the engine and the spec.
    Bt709,
    /// Pure power 2.2.
    Gamma22,
    /// Pure power 2.4: the BT.1886 display EOTF with a zero black level (the
    /// display-referred interpretation of Rec.709 video).
    Gamma24,
}

impl Transfer {
    /// BT.1886 display EOTF with a zero black level: [`Transfer::Gamma24`].
    pub const BT1886: Transfer = Transfer::Gamma24;

    /// [`to_linear`](Self::to_linear) per channel, computed in f64 then
    /// rounded: the CPU reference for the WGSL `fc_*_to_linear` functions.
    pub fn decode(self, rgb: [f32; 3]) -> [f32; 3] {
        rgb.map(|v| self.to_linear(v as f64) as f32)
    }

    /// [`from_linear`](Self::from_linear) per channel (f64, rounded).
    pub fn encode(self, rgb: [f32; 3]) -> [f32; 3] {
        rgb.map(|l| self.from_linear(l as f64) as f32)
    }

    /// Name of the [`wgsl()`] function implementing [`decode`](Self::decode).
    pub fn wgsl_decode_fn(self) -> &'static str {
        match self {
            Transfer::Linear => "fc_identity",
            Transfer::Srgb => "fc_srgb_to_linear",
            Transfer::Bt709 => "fc_bt709_to_linear",
            Transfer::Gamma22 => "fc_gamma22_to_linear",
            Transfer::Gamma24 => "fc_gamma24_to_linear",
        }
    }

    /// Name of the [`wgsl()`] function implementing [`encode`](Self::encode).
    pub fn wgsl_encode_fn(self) -> &'static str {
        match self {
            Transfer::Linear => "fc_identity",
            Transfer::Srgb => "fc_linear_to_srgb",
            Transfer::Bt709 => "fc_linear_to_bt709",
            Transfer::Gamma22 => "fc_linear_to_gamma22",
            Transfer::Gamma24 => "fc_linear_to_gamma24",
        }
    }

    /// Encoded value → linear light.
    pub fn to_linear(self, v: f64) -> f64 {
        match self {
            Transfer::Linear => v,
            Transfer::Srgb => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Transfer::Bt709 => {
                if v < 0.081 {
                    v / 4.5
                } else {
                    ((v + 0.099) / 1.099).powf(1.0 / 0.45)
                }
            }
            Transfer::Gamma22 => v.max(0.0).powf(2.2),
            Transfer::Gamma24 => v.max(0.0).powf(2.4),
        }
    }

    /// Linear light → encoded value (exact inverse of [`to_linear`](Self::to_linear)).
    pub fn from_linear(self, l: f64) -> f64 {
        match self {
            Transfer::Linear => l,
            Transfer::Srgb => {
                if l <= 0.0031308 {
                    l * 12.92
                } else {
                    1.055 * l.powf(1.0 / 2.4) - 0.055
                }
            }
            Transfer::Bt709 => {
                if l < 0.018 {
                    l * 4.5
                } else {
                    1.099 * l.powf(0.45) - 0.099
                }
            }
            Transfer::Gamma22 => l.max(0.0).powf(1.0 / 2.2),
            Transfer::Gamma24 => l.max(0.0).powf(1.0 / 2.4),
        }
    }
}

/// RGB primaries + white point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gamut {
    /// ITU-R BT.709 / sRGB primaries, D65.
    Rec709,
    /// ACES AP1 primaries, ACES white (≈D60). The working space.
    AcesCg,
}

pub type Mat3 = [[f64; 3]; 3];

/// Linear Rec.709 → ACEScg exactly as OCIO 2.5's built-in CG config
/// evaluates `Linear Rec.709 (sRGB)` → `ACEScg` (Bradford D65 → ACES white).
// Digits as printed by OCIO.
#[allow(clippy::excessive_precision)]
pub const REC709_TO_ACESCG: Mat3 = [
    [0.6130974293, 0.3395231366, 0.0473794527],
    [0.0701937228, 0.9163538814, 0.0134523986],
    [0.0206155926, 0.1095697731, 0.8698146343],
];

/// ACEScg → linear Rec.709: the exact (f64) inverse of [`REC709_TO_ACESCG`].
pub const ACESCG_TO_REC709: Mat3 = invert(&REC709_TO_ACESCG);

/// [`REC709_TO_ACESCG`] rounded to f32 (what CPU f32 paths and shaders use).
pub const REC709_TO_ACESCG_F32: [[f32; 3]; 3] = to_f32(&REC709_TO_ACESCG);
/// [`ACESCG_TO_REC709`] rounded to f32.
pub const ACESCG_TO_REC709_F32: [[f32; 3]; 3] = to_f32(&ACESCG_TO_REC709);

const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Matrix converting linear `from` RGB to linear `to` RGB.
pub const fn matrix(from: Gamut, to: Gamut) -> Mat3 {
    match (from, to) {
        (Gamut::Rec709, Gamut::AcesCg) => REC709_TO_ACESCG,
        (Gamut::AcesCg, Gamut::Rec709) => ACESCG_TO_REC709,
        _ => IDENTITY,
    }
}

/// `m · rgb`.
pub fn apply(m: &Mat3, rgb: [f64; 3]) -> [f64; 3] {
    let row = |r: &[f64; 3]| r[0] * rgb[0] + r[1] * rgb[1] + r[2] * rgb[2];
    [row(&m[0]), row(&m[1]), row(&m[2])]
}

const fn invert(m: &Mat3) -> Mat3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let (ca, cb, cc) = (e * i - f * h, f * g - d * i, d * h - e * g);
    let det = a * ca + b * cb + c * cc;
    [
        [ca / det, (c * h - b * i) / det, (b * f - c * e) / det],
        [cb / det, (a * i - c * g) / det, (c * d - a * f) / det],
        [cc / det, (b * g - a * h) / det, (a * e - b * d) / det],
    ]
}

const fn to_f32(m: &Mat3) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    let mut i = 0;
    while i < 9 {
        out[i / 3][i % 3] = m[i / 3][i % 3] as f32;
        i += 1;
    }
    out
}

/// A color space: primaries plus encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Space {
    pub gamut: Gamut,
    pub transfer: Transfer,
}

impl Space {
    /// Linear ACEScg: the engine's working space.
    pub const ACESCG: Space = Space { gamut: Gamut::AcesCg, transfer: Transfer::Linear };
    pub const LINEAR_REC709: Space = Space { gamut: Gamut::Rec709, transfer: Transfer::Linear };
    /// sRGB-encoded Rec.709 (web content, Lottie, PNG).
    pub const SRGB: Space = Space { gamut: Gamut::Rec709, transfer: Transfer::Srgb };
    /// Rec.709 video, scene-referred (inverse camera OETF).
    pub const REC709_BT709: Space = Space { gamut: Gamut::Rec709, transfer: Transfer::Bt709 };
    pub const REC709_GAMMA22: Space = Space { gamut: Gamut::Rec709, transfer: Transfer::Gamma22 };
    /// Rec.709 video, display-referred (BT.1886, zero black).
    pub const REC709_GAMMA24: Space = Space { gamut: Gamut::Rec709, transfer: Transfer::Gamma24 };

    /// The colorspace name in OCIO 2.5's built-in default (CG) config, for
    /// interop with `ferrocut-color` and frame tags. `None` if that config has
    /// no colorspace with exactly this math (e.g. the BT.709 camera OETF).
    pub fn ocio_name(self) -> Option<&'static str> {
        match (self.gamut, self.transfer) {
            (Gamut::AcesCg, Transfer::Linear) => Some("ACEScg"),
            (Gamut::Rec709, Transfer::Linear) => Some("Linear Rec.709 (sRGB)"),
            (Gamut::Rec709, Transfer::Srgb) => Some("sRGB Encoded Rec.709 (sRGB)"),
            (Gamut::Rec709, Transfer::Gamma22) => Some("Gamma 2.2 Encoded Rec.709"),
            (Gamut::Rec709, Transfer::Gamma24) => Some("Gamma 2.4 Encoded Rec.709"),
            _ => None,
        }
    }
}

/// Convert one straight-alpha RGB triple from `src` to `dst`
/// (decode → matrix → encode, in f64).
pub fn convert_rgb(src: Space, dst: Space, rgb: [f64; 3]) -> [f64; 3] {
    let lin = rgb.map(|v| src.transfer.to_linear(v));
    let lin = if src.gamut == dst.gamut { lin } else { apply(&matrix(src.gamut, dst.gamut), lin) };
    lin.map(|v| dst.transfer.from_linear(v))
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)]
mod tests {
    use super::*;

    const ALL: [Transfer; 5] = [Transfer::Linear, Transfer::Srgb, Transfer::Bt709, Transfer::Gamma22, Transfer::Gamma24];

    #[test]
    fn transfers_round_trip_and_hit_the_endpoints() {
        for t in ALL {
            assert_eq!(t.to_linear(0.0), 0.0, "{t:?}");
            assert!((t.to_linear(1.0) - 1.0).abs() < 1e-12, "{t:?}");
            for i in 0..=1000 {
                let v = i as f64 / 1000.0;
                let back = t.from_linear(t.to_linear(v));
                let tol = if t == Transfer::Bt709 && (0.081..0.08125).contains(&v) { 2.5e-4 } else { 1e-12 };
                assert!((back - v).abs() < tol, "{t:?} {v} -> {back}");
            }
        }
    }

    #[test]
    fn piecewise_curves_are_continuous_enough() {
        // sRGB and BT.709 constants are rounded, so the joins have tiny steps.
        let step = |t: Transfer, b: f64| (t.to_linear(b + 1e-12) - t.to_linear(b - 1e-12)).abs();
        assert!(step(Transfer::Srgb, 0.04045) < 1e-7);
        assert!(step(Transfer::Bt709, 0.081) < 1e-4);
    }

    #[test]
    fn matrices_are_inverse_and_preserve_white() {
        let m = REC709_TO_ACESCG;
        let n = ACESCG_TO_REC709;
        for i in 0..3 {
            for j in 0..3 {
                let p: f64 = (0..3).map(|k| m[i][k] * n[k][j]).sum();
                assert!((p - if i == j { 1.0 } else { 0.0 }).abs() < 1e-14);
            }
            // White maps to white (rows sum to 1 with a white-adapting CAT; OCIO's
            // printed digits get within 2e-8).
            assert!((m[i].iter().sum::<f64>() - 1.0).abs() < 5e-8);
        }
    }

    #[test]
    fn f32_matrix_is_the_correct_rounding_and_legacy_differs_by_one_ulp() {
        let legacy = crate::pixels::LEGACY_REC709_TO_ACESCG_F32;
        for i in 0..3 {
            for j in 0..3 {
                let exact = REC709_TO_ACESCG[i][j];
                let f = REC709_TO_ACESCG_F32[i][j];
                // Correctly rounded: no f32 neighbour is closer.
                for n in [f32::from_bits(f.to_bits() - 1), f32::from_bits(f.to_bits() + 1)] {
                    assert!((f as f64 - exact).abs() <= (n as f64 - exact).abs());
                }
                let ulps = (legacy[i][j].to_bits() as i64 - f.to_bits() as i64).abs();
                assert_eq!(ulps, if (i, j) == (2, 2) { 1 } else { 0 }, "[{i}][{j}]");
            }
        }
    }

    #[test]
    fn convert_rgb_composes() {
        let white = convert_rgb(Space::SRGB, Space::ACESCG, [1.0; 3]);
        assert!(white.iter().all(|v| (v - 1.0).abs() < 5e-8), "{white:?}");
        let c = [0.2, 0.5, 0.9];
        let there = convert_rgb(Space::REC709_BT709, Space::ACESCG, c);
        let back = convert_rgb(Space::ACESCG, Space::REC709_BT709, there);
        assert!(c.iter().zip(back).all(|(a, b)| (a - b).abs() < 1e-12), "{back:?}");
        assert_eq!(Space::REC709_BT709.ocio_name(), None);
    }
}
