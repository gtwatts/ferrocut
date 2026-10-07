# ferrocut-colorspace

Pure-Rust (no OpenColorIO) color math for the conversions Ferrocut needs
everywhere: Rec.709 / sRGB encoded ↔ linear Rec.709 ↔ linear ACEScg. It
covers the CPU (f64 reference plus deterministic 8-bit fast paths) and the
GPU (a generated WGSL snippet). Used by `ferrocut-lottie` and
`ferrocut-html`, and meant to replace the engine's hand-written BT.709
transforms.

Validated against OCIO 2.5 in `ferrocut-color/tests/colorspace_vs_ocio.rs`:

- every named space within 4.1e-7 (linear light) in both directions;
- the matrices within 7e-7 relative over [-2, 100];
- the 8-bit layer path within f16 rounding;
- the WGSL on an RTX 5090 within 2.2e-7 of the f64 reference.

## API

```rust
pub const VERSION: &str = "ferrocut-colorspace/1";  // put in node hashes

pub enum Transfer { Linear, Srgb, Bt709, Gamma22, Gamma24 }
impl Transfer {
    pub fn to_linear(self, v: f64) -> f64;   // encoded -> linear
    pub fn from_linear(self, l: f64) -> f64; // linear -> encoded
}

pub enum Gamut { Rec709, AcesCg }
pub type Mat3 = [[f64; 3]; 3];                       // row-major
pub const REC709_TO_ACESCG: Mat3;                    // OCIO 2.5 digits
pub const ACESCG_TO_REC709: Mat3;                    // exact f64 inverse
pub const REC709_TO_ACESCG_F32: [[f32; 3]; 3];       // correctly rounded
pub const ACESCG_TO_REC709_F32: [[f32; 3]; 3];
pub const fn matrix(from: Gamut, to: Gamut) -> Mat3;
pub fn apply(m: &Mat3, rgb: [f64; 3]) -> [f64; 3];

pub struct Space { pub gamut: Gamut, pub transfer: Transfer }
// Space::ACESCG, LINEAR_REC709, SRGB, REC709_BT709, REC709_GAMMA22, REC709_GAMMA24
impl Space { pub fn ocio_name(self) -> Option<&'static str>; }
pub fn convert_rgb(src: Space, dst: Space, rgb: [f64; 3]) -> [f64; 3];

pub fn wgsl() -> &'static str;   // fc_* functions + FC_* matrices, see below

pub mod pixels {                  // deterministic 8-bit -> f16 (layer nodes)
    pub fn srgb8_premul_to_acescg_f16(px: impl IntoIterator<Item = [u8; 4]>, out: &mut Vec<f16>);
    pub fn rgba8_to_f16(px: impl IntoIterator<Item = [u8; 4]>, out: &mut Vec<f16>);
    pub const LEGACY_REC709_TO_ACESCG_F32: [[f32; 3]; 3];
}
```

WGSL (`wgsl()`, prepend to a shader). Everything works on straight
`vec3<f32>`:

```wgsl
const FC_REC709_TO_ACESCG: mat3x3<f32>;  const FC_ACESCG_TO_REC709: mat3x3<f32>;
fn fc_rec709_to_acescg(c) / fc_acescg_to_rec709(c)
fn fc_srgb_to_linear(v)    / fc_linear_to_srgb(l)
fn fc_bt709_to_linear(v)   / fc_linear_to_bt709(l)     // same as the engine's curves
fn fc_gamma22_to_linear(v) / fc_linear_to_gamma22(l)
fn fc_gamma24_to_linear(v) / fc_linear_to_gamma24(l)   // BT.1886, zero black
```

Out-of-range values:

- Piecewise curves (sRGB, BT.709) extend their linear segment below the
  breakpoint, including negatives.
- Pure power curves clamp negatives to 0.
- Matrices are linear everywhere.

## For the engine (not done here; Rusty owns the engine)

Today `composite.wgsl` / `output.wgsl` hard-code their own matrices:

- `REC709_TO_ACESCG_ROWS` differs from OCIO 2.5 by up to 6.9e-5 per
  element, and its rows sum to 1.000087 / 0.99997 / 0.99995 (white isn't
  preserved exactly).
- `ACESCG_TO_REC709_ROWS` differs from the exact inverse of OCIO's matrix by
  up to 1.9e-4.

So a video layer and a Lottie/HTML layer currently use slightly different
color math. The differences are invisible at 8 bits but real in float.

Migration, if you want it:

1. Build the shader source as `format!("{}{}", ferrocut_colorspace::wgsl(), include_str!("composite.wgsl"))`.
2. Replace the local functions:
   - `bt709_to_linear` → `fc_bt709_to_linear`
   - the `REC709_TO_ACESCG_ROWS` dots → `fc_rec709_to_acescg`
   - `ACESCG_TO_REC709_ROWS` → `fc_acescg_to_rec709`
   - `linear_to_bt709` → `fc_linear_to_bt709`
3. Add `ferrocut_colorspace::VERSION` to the source/output node hash strings.
   Outputs change by up to ~1e-4, so goldens and caches move once.
4. Decide policy. `Transfer::Bt709` (scene-referred, what the engine does
   now) and `Transfer::Gamma24` (BT.1886 display-referred, closer to what a
   player shows) are both available.

Tell me if you need more (f32 CPU variants, PQ/HLG, Display P3, a
`Space` ↔ `ColorSpace` tag mapping). The API is kept small until there's a
user.

## Why `LEGACY_REC709_TO_ACESCG_F32`

The literals `ferrocut.lottie/1` and `ferrocut.html/1` shipped with put
element `[2][2]` 1 ulp (6e-8) below the correctly rounded f32 of OCIO's
value. The 8-bit paths keep those exact constants so existing node output
(and cached frames) stays bit-identical. A test pins the 1-ulp difference.
Moving to the exact rounding is a node version bump.
