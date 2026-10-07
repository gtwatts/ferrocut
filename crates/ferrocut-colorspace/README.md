# ferrocut-colorspace

Pure-Rust (no OpenColorIO) color math for the conversions Ferrocut needs
everywhere: Rec.709 / sRGB encoded ↔ linear Rec.709 ↔ linear ACEScg. It
covers the CPU (f64 reference plus deterministic 8-bit fast paths) and the
GPU (a generated WGSL snippet). Used by `ferrocut-lottie`,
`ferrocut-html` and the engine.

Validated against OCIO 2.5 in `ferrocut-color/tests/colorspace_vs_ocio.rs`:

- every named space within 4.1e-7 (linear light) in both directions;
- every name and alias of `named` against OCIO's own processor for that
  name: within 5.3e-7, except `Camera Rec.709` at 5.5e-5 (see below);
- the matrices within 7e-7 relative over [-2, 100];
- the 8-bit layer path within f16 rounding;
- the WGSL on an RTX 5090 within 2.2e-7 of the f64 reference.

## API

```rust
pub const VERSION: &str = "ferrocut-colorspace/1";  // put in node hashes

pub enum Transfer { Linear, Srgb, Bt709, Gamma22, Gamma24 }
impl Transfer {
    pub const BT1886: Transfer;              // = Gamma24 (zero black level)
    pub fn to_linear(self, v: f64) -> f64;   // encoded -> linear
    pub fn from_linear(self, l: f64) -> f64; // linear -> encoded
    pub fn decode(self, rgb: [f32; 3]) -> [f32; 3];  // CPU reference (f64 inside)
    pub fn encode(self, rgb: [f32; 3]) -> [f32; 3];
    pub fn wgsl_decode_fn(self) -> &'static str;     // e.g. "fc_srgb_to_linear"
    pub fn wgsl_encode_fn(self) -> &'static str;
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

pub mod named {                   // keyed by ferrocut_types::ColorSpace names
    pub mod names { ACESCG, LINEAR_REC709, SRGB_ENCODED_REC709,
                    GAMMA22_REC709, GAMMA24_REC709, CAMERA_REC709 }
    pub struct UnknownSpace(pub String);
    pub fn space(name: &str) -> Result<Space, UnknownSpace>;
    pub fn matrix(from: &str, to: &str) -> Result<[[f32; 3]; 3], UnknownSpace>; // = FC_* constants
    pub fn transfer(space: &str) -> Result<Transfer, UnknownSpace>;
    pub fn wgsl_matrix_fn(from: &str, to: &str) -> Result<&'static str, UnknownSpace>;
    pub fn all() -> impl Iterator<Item = (&'static str, Space)>;
}

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
fn fc_identity(c)                                      // linear / same primaries
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

## Names (`named`)

`named` keys everything by the strings frames are tagged with. It accepts
the OCIO 2.5 built-in config names and all of their aliases (e.g. `ACEScg`,
`lin_ap1`, `sRGB - Texture`, `srgb_tx`, `g24_rec709`, `Camera Rec.709`).
Matching is exact, as in OCIO. Each name gives one matrix and one curve, and
the shader-side equivalents are named by `wgsl_matrix_fn` and
`Transfer::wgsl_{de,en}code_fn`:

```rust
use ferrocut_colorspace::named::{self, names};
let m = named::matrix(frame.color_space.name(), names::ACESCG)?;   // [[f32;3];3], row-major
let t = named::transfer(frame.color_space.name())?;                // Transfer
let wgsl = format!("{}({}(c))", named::wgsl_matrix_fn(src, names::ACESCG)?, t.wgsl_decode_fn());
```

Only scene-referred spaces are listed. OCIO's display spaces (`sRGB -
Display`, `Rec.1886 Rec.709 - Display`) are reached through a view
transform (tone mapping) and return `UnknownSpace`. Use a `ferrocut-color`
node for those. BT.1886 with a zero black level is `Gamma 2.4 Encoded
Rec.709` (`Transfer::BT1886`).

Known differences from OCIO:

- `Camera Rec.709` (studio config) maps to `Transfer::Bt709`. That uses the
  BT.709 spec constants (0.018 / 4.5 / 0.099), which the engine and FFmpeg
  also use. OCIO uses the continuous `ExponentWithLinear` form instead. The
  two differ by up to 5.5e-5 in linear light, less than half an 8-bit step.
- Negative values (out of gamut after the matrix) follow each curve's
  extension: linear segment for piecewise curves, clamp for pure powers.
  OCIO extends sRGB and BT.709 differently below 0, by up to 1.4e-4 and
  1.7e-3 in linear light. In-gamut values agree as above.

## Engine status

The engine prepends `wgsl()` to its shaders and keys chunks by `VERSION`.
The old hand-written matrices (`REC709_TO_ACESCG_ROWS` was off from OCIO by
up to 6.9e-5, and its rows didn't sum to 1) are gone, so video, Lottie and
HTML layers now share one set of constants.

## Why `LEGACY_REC709_TO_ACESCG_F32`

The literals `ferrocut.lottie/1` and `ferrocut.html/1` shipped with put
element `[2][2]` 1 ulp (6e-8) below the correctly rounded f32 of OCIO's
value. The 8-bit paths keep those exact constants so existing node output
(and cached frames) stays bit-identical. A test pins the 1-ulp difference.
Moving to the exact rounding is a node version bump.
