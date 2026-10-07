//! Pixel windows and the CPU-side frame (what crosses process boundaries).

use std::sync::Arc;

use half::f16;

use crate::color::{AlphaMode, ColorSpace};
use crate::time::Rational;

/// Integer pixel rectangle in display-window coordinates (origin top-left,
/// y down). Half-open: covers `x..x+width`, `y..y+height`. A frame's *data
/// window* may be smaller than the display window (e.g. a lower third) or
/// extend beyond it (overscan, blur margins); pixels outside the data window
/// are transparent black.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }
    /// The display window itself: `(0, 0, width, height)`.
    pub const fn full(width: u32, height: u32) -> Self {
        PixelRect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
    pub fn right(&self) -> i64 {
        self.x as i64 + self.width as i64
    }
    pub fn bottom(&self) -> i64 {
        self.y as i64 + self.height as i64
    }
    /// Smallest rectangle containing both (an empty rect is the identity).
    pub fn union(&self, o: &PixelRect) -> PixelRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        let (r, b) = (self.right().max(o.right()), self.bottom().max(o.bottom()));
        PixelRect {
            x,
            y,
            width: (r - x as i64) as u32,
            height: (b - y as i64) as u32,
        }
    }
    pub fn intersect(&self, o: &PixelRect) -> PixelRect {
        let (x, y) = (self.x.max(o.x), self.y.max(o.y));
        let (r, b) = (self.right().min(o.right()), self.bottom().min(o.bottom()));
        if r <= x as i64 || b <= y as i64 {
            return PixelRect {
                x,
                y,
                width: 0,
                height: 0,
            };
        }
        PixelRect {
            x,
            y,
            width: (r - x as i64) as u32,
            height: (b - y as i64) as u32,
        }
    }
    pub fn hash_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[..4].copy_from_slice(&self.x.to_le_bytes());
        b[4..8].copy_from_slice(&self.y.to_le_bytes());
        b[8..12].copy_from_slice(&self.width.to_le_bytes());
        b[12..].copy_from_slice(&self.height.to_le_bytes());
        b
    }
}

/// RGBA half floats, tightly packed rows, top row first. Covers a frame's data window.
#[derive(Clone, Debug)]
pub struct CpuImage {
    pub pixels: Vec<f16>,
}

/// A frame in CPU memory with all of its metadata. No GPU types, so the
/// OpenFX host side (or any out-of-process tool) can use it directly.
#[derive(Clone, Debug)]
pub struct CpuFrame {
    /// Display window size.
    pub width: u32,
    pub height: u32,
    /// Pixel bounds of `image` in display-window coordinates.
    pub data_window: PixelRect,
    /// Pixel aspect ratio (width / height of one pixel), e.g. 1, or 4/3 for anamorphic.
    pub pixel_aspect: Rational,
    pub color_space: ColorSpace,
    pub alpha: AlphaMode,
    pub image: Arc<CpuImage>,
}

impl CpuFrame {
    /// Full-window, square-pixel frame.
    pub fn new(width: u32, height: u32, color_space: ColorSpace, pixels: Vec<f16>) -> Self {
        assert_eq!(
            pixels.len(),
            width as usize * height as usize * 4,
            "pixel count"
        );
        CpuFrame {
            width,
            height,
            data_window: PixelRect::full(width, height),
            pixel_aspect: Rational::ONE,
            color_space,
            alpha: AlphaMode::Premultiplied,
            image: Arc::new(CpuImage { pixels }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_union_and_intersect() {
        let a = PixelRect::full(1920, 1080);
        let lower_third = PixelRect::new(100, 800, 900, 200);
        let overscan = PixelRect::new(-64, -64, 2048, 1208);
        assert_eq!(a.union(&lower_third), a);
        assert_eq!(a.union(&overscan), overscan);
        assert_eq!(a.intersect(&overscan), a);
        assert_eq!(a.intersect(&lower_third), lower_third);
        assert!(a.intersect(&PixelRect::new(2000, 0, 10, 10)).is_empty());
        assert_eq!(PixelRect::default().union(&lower_third), lower_third);
    }
}
