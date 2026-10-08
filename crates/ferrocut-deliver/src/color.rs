//! Master RGB → BT.709 limited-range Y'CbCr 4:2:0 (the delivery encoding).
//!
//! The engine composites in linear ACEScg and writes the master in its
//! output space ("Camera Rec.709": Rec.709 primaries, BT.709 OETF, full-range
//! 8-bit R'G'B'). Delivery converts master → Rec.709/BT.709 with
//! `ferrocut-colorspace` (exact pass-through when the master already is that
//! space, as today), then applies the BT.709 Y'CbCr matrix (Kr 0.2126,
//! Kb 0.0722) with limited quantisation (Y' 16-235, C 16-240, 8-bit), and
//! subsamples chroma to 4:2:0 at the H.264 default "left" siting
//! (chroma_sample_loc_type 0: co-sited horizontally with even luma columns,
//! midway between row pairs): a [1 2 1]/4 horizontal filter on each row of
//! the pair, averaged vertically. The stream's VUI says exactly this:
//! colour_primaries 1, transfer_characteristics 1, matrix_coefficients 1,
//! video_full_range_flag 0; OpenH264 writes no chroma_loc_info, which by
//! the spec means chroma_sample_loc_type 0 (left), the siting used here.

use ferrocut_colorspace::{Space, convert_rgb, named};
use ferrocut_types::error::NodeError;

pub const KR: f32 = 0.2126;
pub const KB: f32 = 0.0722;
pub const KG: f32 = 1.0 - KR - KB;

/// An 8-bit I420 picture (planes tightly packed, even dimensions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct I420 {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl I420 {
    pub fn new(width: u32, height: u32) -> Self {
        let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
        Self {
            width,
            height,
            y: vec![0; width as usize * height as usize],
            u: vec![0; cw * ch],
            v: vec![0; cw * ch],
        }
    }
    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }
}

/// Converter from a named master space to BT.709 limited Y'CbCr 4:2:0.
#[derive(Clone, Debug)]
pub struct ToBt709 {
    src: Space,
    /// Master code value → normalised BT.709-encoded value, when the master
    /// already is Rec.709/BT.709 (no cross-channel math needed).
    passthrough: Option<[f32; 256]>,
}

/// The delivery space: Rec.709 primaries, BT.709 transfer.
pub const DELIVERY_SPACE: Space = Space::REC709_BT709;

impl ToBt709 {
    pub fn new(master_space: &str) -> Result<Self, NodeError> {
        let src = named::space(master_space).map_err(|e| NodeError::permanent(e.to_string()))?;
        let passthrough =
            (src == DELIVERY_SPACE).then(|| std::array::from_fn(|i| i as f32 / 255.0));
        Ok(Self { src, passthrough })
    }

    pub fn source_space(&self) -> Space {
        self.src
    }

    /// One master pixel (8-bit code values) as normalised BT.709 R'G'B'.
    #[inline]
    fn rgb(&self, r: u8, g: u8, b: u8) -> [f32; 3] {
        match &self.passthrough {
            Some(lut) => [lut[r as usize], lut[g as usize], lut[b as usize]],
            None => {
                let v = convert_rgb(
                    self.src,
                    DELIVERY_SPACE,
                    [r, g, b].map(|c| c as f64 / 255.0),
                );
                // Out-of-gamut / super-white values clip at delivery.
                v.map(|c| c.clamp(0.0, 1.0) as f32)
            }
        }
    }

    /// Convert tightly packed BGRZ (B, G, R, x) into `out`.
    pub fn convert_bgrz(&self, bgrz: &[u8], out: &mut I420) {
        let (w, h) = (out.width as usize, out.height as usize);
        assert!(bgrz.len() >= w * h * 4, "BGRZ buffer too small");
        let cw = out.chroma_width() as usize;
        let mut rows = [vec![[0f32; 3]; w], vec![[0f32; 3]; w]];
        for cy in 0..h.div_ceil(2) {
            let ys = [2 * cy, (2 * cy + 1).min(h - 1)];
            for (k, &y) in ys.iter().enumerate() {
                let src = &bgrz[y * w * 4..(y + 1) * w * 4];
                for (x, px) in src.as_chunks::<4>().0.iter().enumerate() {
                    rows[k][x] = self.rgb(px[2], px[1], px[0]);
                }
                if k == 0 || ys[1] != ys[0] {
                    let yrow = &mut out.y[y * w..(y + 1) * w];
                    for (o, p) in yrow.iter_mut().zip(&rows[k]) {
                        *o = quant(16.0 + 219.0 * luma(*p));
                    }
                }
            }
            for cx in 0..cw {
                let x0 = 2 * cx;
                let taps = [
                    (x0.saturating_sub(1), 1.0f32),
                    (x0, 2.0),
                    ((x0 + 1).min(w - 1), 1.0),
                ];
                let mut s = [0f32; 3];
                for row in &rows {
                    for &(x, wgt) in &taps {
                        for c in 0..3 {
                            s[c] += wgt * row[x][c];
                        }
                    }
                }
                let p = s.map(|v| v / 8.0);
                let yv = luma(p);
                let cb = (p[2] - yv) / (2.0 * (1.0 - KB));
                let cr = (p[0] - yv) / (2.0 * (1.0 - KR));
                out.u[cy * cw + cx] = quant(128.0 + 224.0 * cb);
                out.v[cy * cw + cx] = quant(128.0 + 224.0 * cr);
            }
        }
    }
}

#[inline]
fn luma(p: [f32; 3]) -> f32 {
    KR * p[0] + KG * p[1] + KB * p[2]
}

#[inline]
fn quant(v: f32) -> u8 {
    (v + 0.5).clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(bgr: [u8; 3], w: u32, h: u32, space: &str) -> I420 {
        let px: Vec<u8> = (0..w * h)
            .flat_map(|_| [bgr[0], bgr[1], bgr[2], 255])
            .collect();
        let mut out = I420::new(w, h);
        ToBt709::new(space).unwrap().convert_bgrz(&px, &mut out);
        out
    }

    #[test]
    fn bt709_limited_reference_values() {
        let cam = named::names::CAMERA_REC709;
        let check = |bgr, yuv: [u8; 3]| {
            let o = solid(bgr, 4, 2, cam);
            assert_eq!([o.y[0], o.u[0], o.v[0]], yuv, "{bgr:?}");
            assert!(o.y.iter().all(|&v| v == yuv[0]));
        };
        check([0, 0, 0], [16, 128, 128]);
        check([255, 255, 255], [235, 128, 128]);
        // BT.709 limited 75%/100% bars, 100%: red, green, blue.
        check([0, 0, 255], [63, 102, 240]);
        check([0, 255, 0], [173, 42, 26]);
        check([255, 0, 0], [32, 240, 118]);
    }

    #[test]
    fn left_sited_chroma_filter() {
        // Column 0 white, others black: chroma sample 0 sees [1,2,1] around
        // x=0 with edge clamp -> (1*W + 2*W + 1*B)/4 = 3/4 white: neutral
        // chroma; luma is exact per pixel.
        let w = 4;
        let px: Vec<u8> = (0..w * 2)
            .flat_map(|i| {
                if i % w == 0 {
                    [255u8, 255, 255, 255]
                } else {
                    [0, 0, 0, 255]
                }
            })
            .collect();
        let mut out = I420::new(w, 2);
        ToBt709::new(named::names::CAMERA_REC709)
            .unwrap()
            .convert_bgrz(&px, &mut out);
        assert_eq!(&out.y[..4], &[235, 16, 16, 16]);
        assert_eq!((out.u[0], out.v[0]), (128, 128));
        // Red in column 1 only: chroma sample 0 weights it 1/4, sample 1 1/4.
        let px: Vec<u8> = (0..w * 2)
            .flat_map(|i| {
                if i % w == 1 {
                    [0u8, 0, 255, 255]
                } else {
                    [0, 0, 0, 255]
                }
            })
            .collect();
        ToBt709::new(named::names::CAMERA_REC709)
            .unwrap()
            .convert_bgrz(&px, &mut out);
        let quarter_red_cr = quant(128.0 + 224.0 * (0.25 * (1.0 - KR)) / (2.0 * (1.0 - KR)));
        assert_eq!(out.v[0], quarter_red_cr);
        assert_eq!(out.v[1], quarter_red_cr);
    }

    #[test]
    fn non_delivery_master_spaces_convert_through_colorspace() {
        // A linear-ACEScg master: mid grey 0.18 -> BT.709 OETF(0.18) = 0.409.
        let v = (0.18f64 * 255.0).round() as u8;
        let o = solid([v, v, v], 2, 2, named::names::ACESCG);
        let want = 16.0
            + 219.0 * ferrocut_colorspace::Transfer::Bt709.from_linear(v as f64 / 255.0) as f32;
        assert!(
            (o.y[0] as f32 - want).abs() <= 0.5 + 1e-3,
            "{} vs {want}",
            o.y[0]
        );
        assert!((o.u[0] as i32 - 128).abs() <= 1 && (o.v[0] as i32 - 128).abs() <= 1);
        assert!(ToBt709::new("not a space").is_err());
    }
}
