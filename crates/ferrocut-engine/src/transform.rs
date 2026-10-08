//! 2D layer transform (After Effects convention), evaluated on the CPU in f64.
//!
//! A source pixel `s` lands at `d = position + A·(s − anchor)` where, in square
//! (pixel-aspect corrected) units, `A = P⁻¹·R(rotation)·S(scale)·P` and
//! `P = diag(pixel_aspect, 1)`. Coordinates are display-window pixels with
//! pixel `i` spanning `[i, i+1)`. Rotation is in degrees, clockwise on screen
//! (y down); scale is a factor (1 = 100 %). Defaults: anchor and position at
//! the frame center, scale 1, rotation 0, which is the identity.
//!
//! The GPU kernel (`shaders/transform.wgsl`) inverse-maps every destination
//! pixel center and filters the source with a separable Catmull-Rom
//! (Mitchell-Netravali B=0, C=1/2) kernel in linear premultiplied space. The
//! kernel is interpolating, so integer translations are exact copies, and it
//! is widened by the minification factor along each source axis so downscales
//! are filtered rather than aliased.
//!
//! The widened kernel is used up to [`MAX_FILTER_SCALE`]. Beyond that, the
//! source is first halved with exact 2x box averages (per axis, in premultiplied
//! space, aligned to the even pixel grid) until the remaining factor is at most
//! [`MAX_FILTER_SCALE`], and the kernel filters the reduced image. Minification
//! up to 8x therefore renders exactly as before; only stronger downscales (which
//! used to alias once the kernel stopped widening) take the box pre-pass.

use ferrocut_core::{Animatable, PixelRect, Rational, RationalTime};
use serde::{Deserialize, Serialize};

/// Bump when the kernel or the math changes (part of the transform node hash).
pub const TRANSFORM_VERSION: &[u8] = b"transform-v2-mip";
/// Widest kernel scale (minification factor) the filter follows; beyond this
/// the source is pre-reduced with 2x box levels (see [`mip_levels`]).
pub const MAX_FILTER_SCALE: f64 = 8.0;
/// Most 2x box levels per axis (a 65536x reduction before the kernel).
pub const MAX_MIP_LEVELS: u32 = 16;
/// Kernel support radius at scale 1, in source pixels.
pub const KERNEL_RADIUS: f64 = 2.0;
pub const FILTER_B: f32 = 0.0;
pub const FILTER_C: f32 = 0.5;

/// Uniform (`"1.5"` or keyframes) or per-axis (`["1.5", "1"]`) scale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scale {
    Uniform(Animatable),
    Xy([Animatable; 2]),
}

/// Animated layer transform of a clip; times are clip-local (0 = clip start).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformSpec {
    /// Where the anchor lands, display pixels. Default: frame center.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[Animatable; 2]>,
    /// Pivot in source pixels. Default: frame center.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<[Animatable; 2]>,
    /// Scale factor. Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Scale>,
    /// Degrees, clockwise. Default 0. On a 3D layer this is the Z rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Animatable>,
    /// 3D layers only (see [`crate::layer3d`]): depth of the position, pixels
    /// (positive = away from the viewer). Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_z: Option<Animatable>,
    /// 3D layers only: depth of the anchor point. Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_z: Option<Animatable>,
    /// 3D layers only: orientation `[x, y, z]` in degrees. Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<[Animatable; 3]>,
    /// 3D layers only: X rotation, degrees. Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_x: Option<Animatable>,
    /// 3D layers only: Y rotation, degrees. Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_y: Option<Animatable>,
}

/// The transform's parameters at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformAt {
    pub position: [f64; 2],
    pub anchor: [f64; 2],
    pub scale: [f64; 2],
    pub rotation_deg: f64,
}

impl TransformAt {
    pub fn identity(width: u32, height: u32) -> Self {
        let c = [width as f64 / 2.0, height as f64 / 2.0];
        TransformAt {
            position: c,
            anchor: c,
            scale: [1.0, 1.0],
            rotation_deg: 0.0,
        }
    }

    /// No-op for any pixel aspect: unit scale, no rotation, anchor on position.
    pub fn is_identity(&self) -> bool {
        self.scale == [1.0, 1.0] && self.rotation_deg == 0.0 && self.position == self.anchor
    }

    /// Bytes that fully determine the output given the input frame.
    pub fn hash_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(56);
        for x in [
            self.position[0],
            self.position[1],
            self.anchor[0],
            self.anchor[1],
            self.scale[0],
            self.scale[1],
            self.rotation_deg,
        ] {
            v.extend_from_slice(&x.to_bits().to_le_bytes());
        }
        v
    }

    /// Forward map in pixels for the given pixel aspect ratio.
    pub fn affine(&self, pixel_aspect: f64) -> Affine {
        let (s, c) = if self.rotation_deg == 0.0 {
            (0.0, 1.0)
        } else {
            self.rotation_deg.to_radians().sin_cos()
        };
        let p = pixel_aspect;
        let [sx, sy] = self.scale;
        // A = P^-1 R S P with R = [c -s; s c], S = diag(sx, sy), P = diag(p, 1).
        let a = c * sx;
        let b = -s * sy / p;
        let cc = s * sx * p;
        let d = c * sy;
        let [px, py] = self.position;
        let [ax, ay] = self.anchor;
        Affine {
            m: [a, b, cc, d],
            t: [px - (a * ax + b * ay), py - (cc * ax + d * ay)],
        }
    }
}

/// `x' = m·x + t` with `m = [[m0, m1], [m2, m3]]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m: [f64; 4],
    pub t: [f64; 2],
}

impl Affine {
    pub fn apply(&self, x: [f64; 2]) -> [f64; 2] {
        let [a, b, c, d] = self.m;
        [
            a * x[0] + b * x[1] + self.t[0],
            c * x[0] + d * x[1] + self.t[1],
        ]
    }

    pub fn is_identity(&self) -> bool {
        self.m == [1.0, 0.0, 0.0, 1.0] && self.t == [0.0, 0.0]
    }

    pub fn inverse(&self) -> Option<Affine> {
        let [a, b, c, d] = self.m;
        let det = a * d - b * c;
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let m = [d / det, -b / det, -c / det, a / det];
        let t = [
            -(m[0] * self.t[0] + m[1] * self.t[1]),
            -(m[2] * self.t[0] + m[3] * self.t[1]),
        ];
        Some(Affine { m, t })
    }
}

/// Everything the kernel needs, derived from a forward map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KernelSetup {
    /// Destination pixel -> source pixel.
    pub inverse: Affine,
    /// Kernel scale along the (reduced) source x / y (>= 1 when minifying).
    pub filter_scale: [f64; 2],
    /// Taps on each side along the (reduced) source x / y.
    pub radius: [i32; 2],
    /// 2x box levels applied to the source along x / y before the kernel
    /// (`[0, 0]` up to [`MAX_FILTER_SCALE`]). `inverse` still maps to the
    /// full-resolution source; the kernel divides by `2^mip`.
    pub mip: [u32; 2],
    /// Output data window (display pixels), never empty.
    pub window: PixelRect,
}

/// Plan the kernel for `fwd` applied to a source covering `src_window`, in a
/// `width` x `height` display window. `None` if the transform is singular (zero
/// scale): the result is fully transparent.
pub fn plan(fwd: &Affine, src_window: PixelRect, width: u32, height: u32) -> Option<KernelSetup> {
    let inv = fwd.inverse()?;
    let [a, b, c, d] = inv.m;
    // Source distance covered by one destination pixel along each source axis.
    let ex = (a * a + b * b).sqrt();
    let ey = (c * c + d * d).sqrt();
    let mip = [mip_levels(ex), mip_levels(ey)];
    let filter_scale = [
        (ex / f64::from(1u32 << mip[0])).clamp(1.0, MAX_FILTER_SCALE),
        (ey / f64::from(1u32 << mip[1])).clamp(1.0, MAX_FILTER_SCALE),
    ];
    let radius = filter_scale.map(|s| (KERNEL_RADIUS * s).ceil() as i32);
    // Kernel support in full-resolution source pixels: the reduced radius times
    // the level size, plus one level pixel of slack for the grid alignment of
    // each box pass (exactly `radius` without a pre-pass).
    let margin = |r: i32, l: u32| {
        if l == 0 {
            f64::from(r)
        } else {
            f64::from(r + 1) * f64::from(1u32 << l)
        }
    };
    let (mx, my) = (margin(radius[0], mip[0]), margin(radius[1], mip[1]));
    // Bounding box of the source window grown by the kernel support, mapped forward.
    let (x0, y0) = (src_window.x as f64 - mx, src_window.y as f64 - my);
    let (x1, y1) = (
        src_window.right() as f64 + mx,
        src_window.bottom() as f64 + my,
    );
    let corners = [[x0, y0], [x1, y0], [x0, y1], [x1, y1]].map(|p| fwd.apply(p));
    let fold =
        |f: fn(f64, f64) -> f64, i: usize, init: f64| corners.iter().map(|p| p[i]).fold(init, f);
    let (bx0, by0) = (
        fold(f64::min, 0, f64::INFINITY),
        fold(f64::min, 1, f64::INFINITY),
    );
    let (bx1, by1) = (
        fold(f64::max, 0, f64::NEG_INFINITY),
        fold(f64::max, 1, f64::NEG_INFINITY),
    );
    let clampx = |v: f64| v.clamp(0.0, width as f64) as i64;
    let clampy = |v: f64| v.clamp(0.0, height as f64) as i64;
    let (wx0, wx1) = (clampx(bx0.floor()), clampx(bx1.ceil()));
    let (wy0, wy1) = (clampy(by0.floor()), clampy(by1.ceil()));
    let window = if wx1 > wx0 && wy1 > wy0 {
        PixelRect::new(
            wx0 as i32,
            wy0 as i32,
            (wx1 - wx0) as u32,
            (wy1 - wy0) as u32,
        )
    } else {
        // Off screen: a 1x1 window the kernel fills with transparent black.
        PixelRect::new(0, 0, 1, 1)
    };
    Some(KernelSetup {
        inverse: inv,
        filter_scale,
        radius,
        mip,
        window,
    })
}

/// 2x box levels needed so a minification of `e` leaves at most
/// [`MAX_FILTER_SCALE`] for the kernel: 0 up to 8x, 1 up to 16x, and so on.
pub fn mip_levels(e: f64) -> u32 {
    let mut l = 0;
    // Exact halvings (powers of two), so the boundary is not subject to log2 rounding.
    while l < MAX_MIP_LEVELS && e / f64::from(1u32 << l) > MAX_FILTER_SCALE {
        l += 1;
    }
    l
}

/// The data window of a 2x box level of `w` (factors 1 or 2 per axis): level
/// pixel `k` averages source pixels `f*k .. f*k + f`, on the even grid.
pub fn box_window(w: PixelRect, f: [u32; 2]) -> PixelRect {
    let lo = |v: i64, f: u32| v.div_euclid(i64::from(f));
    let hi = |v: i64, f: u32| (v + i64::from(f) - 1).div_euclid(i64::from(f));
    let (x0, x1) = (lo(i64::from(w.x), f[0]), hi(w.right(), f[0]));
    let (y0, y1) = (lo(i64::from(w.y), f[1]), hi(w.bottom(), f[1]));
    PixelRect::new(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32)
}

fn xy(v: &Option<[Animatable; 2]>, t: RationalTime, default: [f64; 2]) -> [f64; 2] {
    match v {
        Some([x, y]) => [x.eval(t), y.eval(t)],
        None => default,
    }
}

impl TransformSpec {
    /// Parameters at clip-local time `t`.
    pub fn at(&self, t: RationalTime, width: u32, height: u32) -> TransformAt {
        let id = TransformAt::identity(width, height);
        TransformAt {
            position: xy(&self.position, t, id.position),
            anchor: xy(&self.anchor, t, id.anchor),
            scale: match &self.scale {
                None => id.scale,
                Some(Scale::Uniform(s)) => {
                    let v = s.eval(t);
                    [v, v]
                }
                Some(Scale::Xy([x, y])) => [x.eval(t), y.eval(t)],
            },
            rotation_deg: self.rotation.as_ref().map_or(0.0, |r| r.eval(t)),
        }
    }

    fn all(&self) -> Vec<(&'static str, &Animatable)> {
        let mut v = Vec::new();
        if let Some([x, y]) = &self.position {
            v.push(("position.x", x));
            v.push(("position.y", y));
        }
        if let Some([x, y]) = &self.anchor {
            v.push(("anchor.x", x));
            v.push(("anchor.y", y));
        }
        match &self.scale {
            Some(Scale::Uniform(s)) => v.push(("scale", s)),
            Some(Scale::Xy([x, y])) => {
                v.push(("scale.x", x));
                v.push(("scale.y", y));
            }
            None => {}
        }
        if let Some(r) = &self.rotation {
            v.push(("rotation", r));
        }
        // 3D fields only when present, so 2D specs hash as before.
        v.extend(self.all_3d());
        v
    }

    fn all_3d(&self) -> Vec<(&'static str, &Animatable)> {
        let mut v = Vec::new();
        if let Some(a) = &self.position_z {
            v.push(("position_z", a));
        }
        if let Some(a) = &self.anchor_z {
            v.push(("anchor_z", a));
        }
        if let Some([x, y, z]) = &self.orientation {
            v.push(("orientation.x", x));
            v.push(("orientation.y", y));
            v.push(("orientation.z", z));
        }
        if let Some(a) = &self.rotation_x {
            v.push(("rotation_x", a));
        }
        if let Some(a) = &self.rotation_y {
            v.push(("rotation_y", a));
        }
        v
    }

    /// Does the spec set any 3D-only field (which needs the clip's `three_d`)?
    pub fn has_3d_fields(&self) -> Option<&'static str> {
        self.all_3d().first().map(|(n, _)| *n)
    }

    /// The 3D-only parameters at clip-local `t`:
    /// `[position_z, anchor_z, orientation x, y, z, rotation_x, rotation_y]`.
    pub fn at_3d(&self, t: RationalTime) -> [f64; 7] {
        let e = |a: &Option<Animatable>| a.as_ref().map_or(0.0, |a| a.eval(t));
        let o = self
            .orientation
            .as_ref()
            .map_or([0.0; 3], |[x, y, z]| [x.eval(t), y.eval(t), z.eval(t)]);
        [
            e(&self.position_z),
            e(&self.anchor_z),
            o[0],
            o[1],
            o[2],
            e(&self.rotation_x),
            e(&self.rotation_y),
        ]
    }

    pub fn validate(&self) -> Result<(), String> {
        for (name, a) in self.all() {
            a.validate().map_err(|e| format!("transform {name}: {e}"))?;
        }
        Ok(())
    }

    pub fn is_animated(&self) -> bool {
        self.all().iter().any(|(_, a)| a.is_animated())
    }

    /// Keyframes stay on the timeline when the clip start moves by `-dt`.
    pub fn shifted(&self, dt: Rational) -> TransformSpec {
        let s2 =
            |v: &Option<[Animatable; 2]>| v.as_ref().map(|[x, y]| [x.shifted(dt), y.shifted(dt)]);
        TransformSpec {
            position: s2(&self.position),
            anchor: s2(&self.anchor),
            scale: self.scale.as_ref().map(|s| match s {
                Scale::Uniform(a) => Scale::Uniform(a.shifted(dt)),
                Scale::Xy([x, y]) => Scale::Xy([x.shifted(dt), y.shifted(dt)]),
            }),
            rotation: self.rotation.as_ref().map(|r| r.shifted(dt)),
            position_z: self.position_z.as_ref().map(|a| a.shifted(dt)),
            anchor_z: self.anchor_z.as_ref().map(|a| a.shifted(dt)),
            orientation: self
                .orientation
                .as_ref()
                .map(|[x, y, z]| [x.shifted(dt), y.shifted(dt), z.shifted(dt)]),
            rotation_x: self.rotation_x.as_ref().map(|a| a.shifted(dt)),
            rotation_y: self.rotation_y.as_ref().map(|a| a.shifted(dt)),
        }
    }

    pub fn hash_into(&self, h: &mut blake3::Hasher) {
        h.update(TRANSFORM_VERSION);
        for (name, a) in self.all() {
            h.update(name.as_bytes());
            a.hash_into(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9
    }

    #[test]
    fn defaults_are_identity() {
        let a = TransformAt::identity(1920, 1080).affine(1.0);
        assert!(a.is_identity());
        let spec: TransformSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(
            spec.at(RationalTime::ZERO, 64, 32),
            TransformAt::identity(64, 32)
        );
    }

    #[test]
    fn ae_convention() {
        // Anchor lands on position; 90 degrees clockwise maps +x to +y (y down).
        let t = TransformAt {
            position: [100.0, 50.0],
            anchor: [10.0, 10.0],
            scale: [2.0, 2.0],
            rotation_deg: 90.0,
        };
        let a = t.affine(1.0);
        assert!(near(a.apply([10.0, 10.0]), [100.0, 50.0]));
        assert!(near(a.apply([11.0, 10.0]), [100.0, 52.0]));
        assert!(near(
            a.inverse().unwrap().apply([100.0, 52.0]),
            [11.0, 10.0]
        ));
    }

    #[test]
    fn pixel_aspect_keeps_shapes_square_on_screen() {
        // PAR 2: a pixel is twice as wide as tall. Rotating a 1-pixel step in x
        // (2 square units) by 90 degrees must give 2 pixels in y.
        let t = TransformAt {
            position: [0.0, 0.0],
            anchor: [0.0, 0.0],
            scale: [1.0, 1.0],
            rotation_deg: 90.0,
        };
        assert!(near(t.affine(2.0).apply([1.0, 0.0]), [0.0, 2.0]));
        assert!(near(t.affine(2.0).apply([0.0, 2.0]), [-1.0, 0.0]));
    }

    #[test]
    fn plan_windows_and_filter_scale() {
        let full = PixelRect::full(64, 32);
        // Half size around the center: minification 2 along both axes.
        let t = TransformAt {
            scale: [0.5, 0.5],
            ..TransformAt::identity(64, 32)
        };
        let k = plan(&t.affine(1.0), full, 64, 32).unwrap();
        assert_eq!(k.filter_scale, [2.0, 2.0]);
        assert_eq!((k.radius, k.mip), ([4, 4], [0, 0]));
        assert_eq!(k.window, PixelRect::new(14, 6, 36, 20));
        // Integer translation: magnification 1, window shifted and clipped.
        let t = TransformAt {
            position: [32.0 + 5.0, 16.0 - 3.0],
            ..TransformAt::identity(64, 32)
        };
        let k = plan(&t.affine(1.0), full, 64, 32).unwrap();
        assert_eq!(k.filter_scale, [1.0, 1.0]);
        assert_eq!(k.window, PixelRect::new(3, 0, 61, 31));
        // Zero scale is singular; far off screen gives a 1x1 transparent window.
        let t = TransformAt {
            scale: [0.0, 1.0],
            ..TransformAt::identity(64, 32)
        };
        assert!(plan(&t.affine(1.0), full, 64, 32).is_none());
        let t = TransformAt {
            position: [1e4, 1e4],
            ..TransformAt::identity(64, 32)
        };
        assert_eq!(
            plan(&t.affine(1.0), full, 64, 32).unwrap().window,
            PixelRect::new(0, 0, 1, 1)
        );
    }

    #[test]
    fn mip_levels_start_past_the_kernel_cap() {
        assert_eq!(mip_levels(1.0), 0);
        assert_eq!(mip_levels(2.5), 0);
        assert_eq!(mip_levels(8.0), 0);
        assert_eq!(mip_levels(8.0001), 1);
        assert_eq!(mip_levels(16.0), 1);
        assert_eq!(mip_levels(16.5), 2);
        assert_eq!(mip_levels(1e30), MAX_MIP_LEVELS);
        // 20x horizontally, 4x vertically: two x levels, kernel 5 x 4.
        let t = TransformAt {
            scale: [0.05, 0.25],
            ..TransformAt::identity(640, 320)
        };
        let k = plan(&t.affine(1.0), PixelRect::full(640, 320), 640, 320).unwrap();
        assert_eq!(k.mip, [2, 0]);
        assert!((k.filter_scale[0] - 5.0).abs() < 1e-9 && k.filter_scale[1] == 4.0);
        assert_eq!(k.radius, [10, 8]);
        // Box windows follow the even grid, including negative and odd origins.
        assert_eq!(
            box_window(PixelRect::new(-3, 1, 6, 4), [2, 1]),
            PixelRect::new(-2, 1, 4, 4)
        );
        assert_eq!(
            box_window(PixelRect::new(1, 1, 3, 3), [2, 2]),
            PixelRect::new(0, 0, 2, 2)
        );
    }

    #[test]
    fn spec_json_and_keyframes() {
        let s: TransformSpec = serde_json::from_str(
            r#"{ "position": ["10", { "keyframes": [ { "t": "0", "v": "0", "interp": "ease_in_out" },
                                                     { "t": "1", "v": "100" } ] }],
                 "scale": "1/2", "rotation": "-15" }"#,
        )
        .unwrap();
        s.validate().unwrap();
        assert!(s.is_animated());
        let a = s.at(RationalTime::ZERO, 64, 32);
        let b = s.at(RationalTime(Rational::ONE), 64, 32);
        let m = s.at(RationalTime(Rational::new(1, 2)), 64, 32);
        assert_eq!((a.position, b.position[1]), ([10.0, 0.0], 100.0));
        assert!(
            (m.position[1] - 50.0).abs() < 1e-9,
            "ease_in_out is symmetric"
        );
        assert_eq!((a.scale, a.rotation_deg), ([0.5, 0.5], -15.0));
        // Shifting by -1/2 moves the keys earlier.
        let sh = s.shifted(Rational::new(-1, 2));
        assert_eq!(
            sh.at(RationalTime(Rational::new(1, 2)), 64, 32).position,
            b.position
        );
        assert!(serde_json::from_str::<TransformSpec>(r#"{ "skew": "1" }"#).is_err());
    }
}
