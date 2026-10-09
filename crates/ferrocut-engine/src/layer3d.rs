//! Legacy 2.5D: After Effects-style 3D layers seen through a camera, and layer
//! motion blur. Both are evaluated on the CPU in f64 and rendered by one GPU
//! kernel (`shaders/transform_ms.wgsl`) that resamples the layer through one
//! or more projective maps (homographies) and averages them.
//!
//! **Coordinates** (as in After Effects): x right, y down, z away from the
//! viewer, in output pixels. A 3D layer (clip `three_d: true`) is a flat
//! card: source pixel `s` sits at `(s, 0)` in layer space and lands at
//! `position + O·Rx·Ry·Rz·S·((s, 0) − anchor)` where `position` / `anchor`
//! get their z from `transform.position_z` / `anchor_z`, `S = diag(scale_x,
//! scale_y, 1)`, `R*` are the X/Y/Z rotations (`rotation_x`, `rotation_y`,
//! `rotation`; positive Z is clockwise on screen like 2D rotation) and `O`
//! is `orientation` (`[x, y, z]`, the same `Rx·Ry·Rz` product). Rotation
//! matrices are the standard right-handed ones in this y-down, z-in frame:
//! positive Z turns clockwise on screen, positive Y brings the layer's right
//! edge towards the viewer, positive X tilts its bottom edge away.
//!
//! **Camera** (`timeline.camera`, keyframes in timeline time): `position`,
//! `point_of_interest` (the camera looks at it, with screen-up = −y), and
//! `zoom` (distance in pixels at which a layer appears at 100 %) or
//! `fov_deg` (horizontal angle of view; `zoom = (w/2) / tan(fov/2)`).
//! Defaults are After Effects' default camera, the 50 mm preset: `zoom =
//! w·50/36`, at `[w/2, h/2, −zoom]` looking at `[w/2, h/2, 0]`, so a 3D layer
//! with a default transform looks exactly like the 2D layer. Projection:
//! `screen = (w/2, h/2) + zoom · (x_c, y_c) / z_c` in camera space.
//! No lights, shadows or depth of field.
//!
//! **Compositing order**: consecutive 3D layers (tracks whose clip at that
//! time is 3D) are painter-sorted by depth (camera-space z of the layer's
//! position), farthest first; equal depths keep track order. 2D layers
//! composite in track order and split runs of 3D layers, as in After
//! Effects. Layers do not intersect (no z-buffer).
//!
//! **Pixels in front of the camera only**: parts of a layer less than one
//! pixel in front of the camera plane are not drawn.
//!
//! **Motion blur** (`timeline.motion_blur` enables it for the timeline, the
//! clip's `motion_blur: true` for the layer, like After Effects' comp and
//! layer switches): the layer's transform (and the camera, for 3D layers) is
//! sampled at `samples` sub-frame times `t + (phase + angle·(i + ½)/n) /
//! (360·fps)` (exact rational times; defaults angle 180°, phase −90°,
//! 16 samples) and the resampled layers are averaged in linear premultiplied
//! light, in a fixed order. The layer's content is the frame at `t` (content
//! motion inside the clip is not blurred). A frame whose samples are all the
//! same pose renders exactly like an unblurred one.
//!
//! **Filtering**: each sample uses the transform's Catmull-Rom kernel,
//! widened per output pixel by the local minification (the projective map's
//! Jacobian), up to [`MAX_FILTER_SCALE`]; past that, 2x box levels are
//! taken once for all samples (the smallest level any sample needs at the
//! window's corners and center). Very oblique 3D layers can still alias
//! towards the horizon.
//!
//! The opt-in [`crate::depth`] scene reuses these coordinates and transform
//! math, with its own validated camera basis, clipping, raster filtering and
//! per-pixel transparency/depth order. The legacy kernel is unchanged.

use ferrocut_core::{Animatable, PixelRect, Rational, RationalTime};
use serde::{Deserialize, Serialize};

use crate::transform::{KERNEL_RADIUS, MAX_FILTER_SCALE, TransformAt, TransformSpec, mip_levels};

/// Bump when the 3D / motion-blur math or kernel changes.
pub const LAYER3D_VERSION: &[u8] = b"layer3d.v1";
/// Most motion blur samples per frame.
pub const MAX_SAMPLES: u32 = 64;
/// Points closer to the camera plane than this (pixels) are not drawn.
pub const NEAR: f64 = 1.0;

pub type Mat3 = [[f64; 3]; 3];

/// The timeline's camera. See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[Animatable; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point_of_interest: Option<[Animatable; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<Animatable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fov_deg: Option<Animatable>,
    /// Explicit world-up reference for depth_layers_v1; default [0, -1, 0].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_up: Option<[Animatable; 3]>,
    /// Camera-axis rotation around forward, degrees; depth_layers_v1 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roll: Option<Animatable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near: Option<Rational>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub far: Option<Rational>,
}

/// The camera at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraAt {
    pub position: [f64; 3],
    pub point_of_interest: [f64; 3],
    pub zoom: f64,
}

/// One shared, explicitly validated camera for a depth scene.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthCameraAt {
    pub camera: CameraAt,
    pub basis: Mat3,
    pub near: f64,
    pub far: f64,
}

pub const DEPTH_FAR: i64 = 100_000;

/// After Effects' 50 mm camera preset for a `w`-pixel-wide frame.
pub fn default_zoom(w: f64) -> f64 {
    w * 50.0 / 36.0
}

fn xyz(v: &Option<[Animatable; 3]>, t: RationalTime, d: [f64; 3]) -> [f64; 3] {
    v.as_ref()
        .map_or(d, |[x, y, z]| [x.eval(t), y.eval(t), z.eval(t)])
}

impl CameraSpec {
    /// The camera at timeline time `t`; `w` / `h` in square units.
    pub fn at(&self, t: RationalTime, w: f64, h: f64) -> CameraAt {
        let zoom = match (&self.zoom, &self.fov_deg) {
            (Some(z), _) => z.eval(t),
            (None, Some(f)) => {
                let f = f.eval(t).clamp(1e-3, 179.0);
                (w / 2.0) / (f.to_radians() / 2.0).tan()
            }
            (None, None) => default_zoom(w),
        };
        CameraAt {
            position: xyz(&self.position, t, [w / 2.0, h / 2.0, -zoom]),
            point_of_interest: xyz(&self.point_of_interest, t, [w / 2.0, h / 2.0, 0.0]),
            zoom,
        }
    }

    fn all(&self) -> Vec<(&'static str, &Animatable)> {
        let mut v = Vec::new();
        for (n, p) in [
            (["position.x", "position.y", "position.z"], &self.position),
            (
                [
                    "point_of_interest.x",
                    "point_of_interest.y",
                    "point_of_interest.z",
                ],
                &self.point_of_interest,
            ),
            (
                ["reference_up.x", "reference_up.y", "reference_up.z"],
                &self.reference_up,
            ),
        ] {
            if let Some([x, y, z]) = p {
                v.push((n[0], x));
                v.push((n[1], y));
                v.push((n[2], z));
            }
        }
        if let Some(z) = &self.zoom {
            v.push(("zoom", z));
        }
        if let Some(f) = &self.fov_deg {
            v.push(("fov_deg", f));
        }
        if let Some(r) = &self.roll {
            v.push(("roll", r));
        }
        v
    }

    pub fn validate(&self) -> Result<(), String> {
        for (n, a) in self.all() {
            a.validate().map_err(|e| format!("camera.{n}: {e}"))?;
        }
        if self.zoom.is_some() && self.fov_deg.is_some() {
            return Err("camera: give zoom or fov_deg, not both".into());
        }
        if let Some(z) = &self.zoom
            && z.key_range().0 <= Rational::ZERO
        {
            return Err("camera.zoom must be > 0".into());
        }
        if let Some(f) = &self.fov_deg {
            let (lo, hi) = f.key_range();
            if lo <= Rational::ZERO || hi >= Rational::from_int(180) {
                return Err("camera.fov_deg must be in (0, 180)".into());
            }
        }
        let near = self.near.unwrap_or(Rational::ONE);
        let far = self.far.unwrap_or(Rational::from_int(DEPTH_FAR));
        if near <= Rational::ZERO || far <= near {
            return Err("camera clipping requires 0 < near < far".into());
        }
        Ok(())
    }

    pub fn has_depth_controls(&self) -> bool {
        self.reference_up.is_some()
            || self.roll.is_some()
            || self.near.is_some()
            || self.far.is_some()
    }

    /// No fallback axis in the opt-in scene. Recheck at every exact shutter
    /// sample, since valid key endpoints can interpolate through degeneracy.
    pub fn depth_at(&self, t: RationalTime, w: f64, h: f64) -> Result<DepthCameraAt, String> {
        let camera = self.at(t, w, h);
        let forward = depth_norm(sub3(camera.point_of_interest, camera.position))
            .ok_or_else(|| format!("camera at {t}: position and point_of_interest must differ"))?;
        let up = depth_norm(xyz(&self.reference_up, t, [0.0, -1.0, 0.0])).ok_or_else(|| {
            format!("camera at {t}: reference_up must be a nonzero finite vector")
        })?;
        let right = depth_norm(cross(forward, up)).ok_or_else(|| {
            format!("camera at {t}: reference_up must not be collinear with the viewing axis")
        })?;
        let down = cross(forward, right);
        let (s, c) = sc(self.roll.as_ref().map_or(0.0, |r| r.eval(t)));
        let basis = [
            [0, 1, 2].map(|i| c * right[i] + s * down[i]),
            [0, 1, 2].map(|i| -s * right[i] + c * down[i]),
            forward,
        ];
        let near = self.near.unwrap_or(Rational::ONE).to_f64();
        let far = self.far.unwrap_or(Rational::from_int(DEPTH_FAR)).to_f64();
        if !camera.zoom.is_finite()
            || camera.zoom <= 0.0
            || !basis.iter().flatten().all(|v| v.is_finite())
            || !near.is_finite()
            || !far.is_finite()
            || near <= 0.0
            || far <= near
        {
            return Err(format!(
                "camera at {t}: nonfinite/invalid evaluated camera or clipping planes"
            ));
        }
        Ok(DepthCameraAt {
            camera,
            basis,
            near,
            far,
        })
    }

    pub fn is_animated(&self) -> bool {
        self.all().iter().any(|(_, a)| a.is_animated())
    }

    /// Same camera with every key time shifted by `dt`.
    pub fn shifted(&self, dt: Rational) -> CameraSpec {
        let s3 = |v: &Option<[Animatable; 3]>| {
            v.as_ref()
                .map(|[x, y, z]| [x.shifted(dt), y.shifted(dt), z.shifted(dt)])
        };
        CameraSpec {
            position: s3(&self.position),
            point_of_interest: s3(&self.point_of_interest),
            zoom: self.zoom.as_ref().map(|a| a.shifted(dt)),
            fov_deg: self.fov_deg.as_ref().map(|a| a.shifted(dt)),
            reference_up: s3(&self.reference_up),
            roll: self.roll.as_ref().map(|a| a.shifted(dt)),
            near: self.near,
            far: self.far,
        }
    }

    pub fn hash_into(&self, h: &mut blake3::Hasher) {
        h.update(LAYER3D_VERSION);
        for (n, a) in self.all() {
            h.update(n.as_bytes());
            a.hash_into(h);
        }
        for (name, value) in [
            (b"near".as_slice(), self.near),
            (b"far".as_slice(), self.far),
        ] {
            if let Some(value) = value {
                h.update(name);
                h.update(&value.hash_bytes());
            }
        }
    }
}

/// Timeline motion blur settings (After Effects composition settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionBlurSpec {
    /// Shutter angle in degrees, [0, 720]; 360 = the whole frame interval.
    #[serde(default = "default_angle")]
    pub shutter_angle: Rational,
    /// Shutter phase in degrees, [−360, 360]: where the shutter opens
    /// relative to the frame time (−90 with 180 centers it on the frame).
    #[serde(default = "default_phase")]
    pub shutter_phase: Rational,
    /// Samples per frame, 2..=64 (a JSON integer or integer string).
    #[serde(default = "default_samples", deserialize_with = "int_or_string")]
    pub samples: u32,
}

fn default_angle() -> Rational {
    Rational::from_int(180)
}
fn default_phase() -> Rational {
    Rational::from_int(-90)
}
fn default_samples() -> u32 {
    16
}

fn int_or_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        N(u32),
        S(String),
    }
    match V::deserialize(d)? {
        V::N(n) => Ok(n),
        V::S(s) => s.trim().parse().map_err(|_| {
            serde::de::Error::custom(format!("samples: expected an integer, got {s:?}"))
        }),
    }
}

impl Default for MotionBlurSpec {
    fn default() -> Self {
        MotionBlurSpec {
            shutter_angle: default_angle(),
            shutter_phase: default_phase(),
            samples: default_samples(),
        }
    }
}

impl MotionBlurSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.shutter_angle < Rational::ZERO || self.shutter_angle > Rational::from_int(720) {
            return Err("motion_blur.shutter_angle must be in [0, 720]".into());
        }
        if self.shutter_phase < Rational::from_int(-360)
            || self.shutter_phase > Rational::from_int(360)
        {
            return Err("motion_blur.shutter_phase must be in [-360, 360]".into());
        }
        if !(2..=MAX_SAMPLES).contains(&self.samples) {
            return Err(format!("motion_blur.samples must be in 2..={MAX_SAMPLES}"));
        }
        Ok(())
    }

    /// Sub-frame sample times around timeline time `t` (exact).
    pub fn sample_times(&self, t: RationalTime, fps: Rational) -> Vec<RationalTime> {
        let n = i64::from(self.samples);
        (0..n)
            .map(|i| {
                let deg = self.shutter_phase + self.shutter_angle * Rational::new(2 * i + 1, 2 * n);
                RationalTime(t.0 + deg / Rational::from_int(360) / fps)
            })
            .collect()
    }

    pub fn hash_bytes(&self) -> Vec<u8> {
        let mut v = self.shutter_angle.hash_bytes().to_vec();
        v.extend_from_slice(&self.shutter_phase.hash_bytes());
        v.extend_from_slice(&self.samples.to_le_bytes());
        v
    }
}

// ------------------------------------------------------------------ math

fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    m
}

fn apply(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn sc(deg: f64) -> (f64, f64) {
    if deg == 0.0 {
        (0.0, 1.0)
    } else {
        deg.to_radians().sin_cos()
    }
}

fn rx(deg: f64) -> Mat3 {
    let (s, c) = sc(deg);
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}
fn ry(deg: f64) -> Mat3 {
    let (s, c) = sc(deg);
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}
fn rz(deg: f64) -> Mat3 {
    let (s, c) = sc(deg);
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: [f64; 3]) -> Option<[f64; 3]> {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    (l > 1e-12 && l.is_finite()).then(|| a.map(|v| v / l))
}

// Scale first so finite very small/large reference vectors remain valid.
// This is separate from the legacy camera's fallback/threshold math.
fn depth_norm(a: [f64; 3]) -> Option<[f64; 3]> {
    if !a.iter().all(|v| v.is_finite()) {
        return None;
    }
    let scale = a.iter().map(|v| v.abs()).fold(0.0, f64::max);
    if scale == 0.0 {
        return None;
    }
    let v = a.map(|x| x / scale);
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    Some(v.map(|x| x / length))
}

/// Source pixel coordinates -> camera XYZ affine matrix for the opt-in scene.
/// Column 2 is translation, not a source Z axis; the source is a flat card.
pub fn layer_camera_affine(
    t: &TransformAt,
    d3: [f64; 7],
    cam: &DepthCameraAt,
    pixel_aspect: f64,
) -> Mat3 {
    let [pz, az, ox, oy, oz, rxd, ryd] = d3;
    let r = mul(
        &mul(&mul(&rx(ox), &ry(oy)), &rz(oz)),
        &mul(&mul(&rx(rxd), &ry(ryd)), &rz(t.rotation_deg)),
    );
    let m = mul(
        &r,
        &[
            [t.scale[0], 0.0, 0.0],
            [0.0, t.scale[1], 0.0],
            [0.0, 0.0, 1.0],
        ],
    );
    let pos = [t.position[0] * pixel_aspect, t.position[1], pz];
    let anchor = [t.anchor[0] * pixel_aspect, t.anchor[1], az];
    let a = apply(&cam.basis, [m[0][0], m[1][0], m[2][0]]);
    let b = apply(&cam.basis, [m[0][1], m[1][1], m[2][1]]);
    let c = apply(
        &cam.basis,
        sub3(sub3(pos, apply(&m, anchor)), cam.camera.position),
    );
    [0, 1, 2].map(|k| [a[k] * pixel_aspect, b[k], c[k]])
}

/// World -> camera rotation (rows: camera right, down, forward).
pub fn view_basis(cam: &CameraAt) -> Mat3 {
    let f = norm(sub3(cam.point_of_interest, cam.position)).unwrap_or([0.0, 0.0, 1.0]);
    let x = norm(cross([0.0, 1.0, 0.0], f))
        .or_else(|| norm(cross([0.0, 0.0, 1.0], f)))
        .unwrap_or([1.0, 0.0, 0.0]);
    let y = cross(f, x);
    [x, y, f]
}

/// 3x3 inverse (`None` if singular).
pub fn inverse(m: &Mat3) -> Option<Mat3> {
    let c =
        |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let adj = [
        [c(1, 1, 2, 2), -c(0, 1, 2, 2), c(0, 1, 1, 2)],
        [-c(1, 0, 2, 2), c(0, 0, 2, 2), -c(0, 0, 1, 2)],
        [c(1, 0, 2, 1), -c(0, 0, 2, 1), c(0, 0, 1, 1)],
    ];
    let det = m[0][0] * adj[0][0] + m[0][1] * adj[1][0] + m[0][2] * adj[2][0];
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    Some(adj.map(|r| r.map(|v| v / det)))
}

/// Forward map of a 2D layer transform as a homography (pixels).
pub fn affine_homography(t: &TransformAt, pixel_aspect: f64) -> Mat3 {
    let a = t.affine(pixel_aspect);
    [
        [a.m[0], a.m[1], a.t[0]],
        [a.m[2], a.m[3], a.t[1]],
        [0.0, 0.0, 1.0],
    ]
}

/// Forward homography (source pixels -> output pixels) of a 3D layer and
/// its depth (camera-space z of its position). `d3` is
/// [`crate::transform::TransformSpec::at_3d`]; `w` x `h` the display window.
pub fn layer_homography(
    t: &TransformAt,
    d3: [f64; 7],
    cam_spec: Option<&CameraSpec>,
    cam_time: RationalTime,
    w: u32,
    h: u32,
    pixel_aspect: f64,
) -> (Mat3, f64) {
    let p = pixel_aspect;
    // Square units: x scaled by the pixel aspect.
    let (wq, hq) = (w as f64 * p, h as f64);
    let cam = cam_spec.cloned().unwrap_or_default().at(cam_time, wq, hq);
    let [pz, az, ox, oy, oz, rxd, ryd] = d3;
    let r = mul(
        &mul(&mul(&rx(ox), &ry(oy)), &rz(oz)),
        &mul(&mul(&rx(rxd), &ry(ryd)), &rz(t.rotation_deg)),
    );
    let m = mul(
        &r,
        &[
            [t.scale[0], 0.0, 0.0],
            [0.0, t.scale[1], 0.0],
            [0.0, 0.0, 1.0],
        ],
    );
    let pos = [t.position[0] * p, t.position[1], pz];
    let anc = [t.anchor[0] * p, t.anchor[1], az];
    let v = view_basis(&cam);
    let ma = apply(&m, anc);
    let c0 = apply(&v, [m[0][0], m[1][0], m[2][0]]);
    let c1 = apply(&v, [m[0][1], m[1][1], m[2][1]]);
    let c2 = apply(&v, sub3(sub3(pos, ma), cam.position));
    let depth = apply(&v, sub3(pos, cam.position))[2];
    let (cx, cy, z) = (wq / 2.0, hq / 2.0, cam.zoom);
    let row = |k: usize, ctr: f64| {
        [
            z * c0[k] + ctr * c0[2],
            z * c1[k] + ctr * c1[2],
            z * c2[k] + ctr * c2[2],
        ]
    };
    let hq_ = [row(0, cx), row(1, cy), [c0[2], c1[2], c2[2]]];
    // Pixels: source x -> square (·p), screen square x -> pixels (/p).
    let mut hm = hq_;
    for row in hm.iter_mut() {
        row[0] *= p;
    }
    for v in hm[0].iter_mut() {
        *v /= p;
    }
    (hm, depth)
}

/// Depth (camera-space z of the position) of a 3D layer with transform
/// `spec` (clip starting at `start`) at timeline time `t`, for the painter's
/// sort. Square pixels.
pub fn layer_depth(
    spec: &TransformSpec,
    start: RationalTime,
    camera: Option<&CameraSpec>,
    t: RationalTime,
    placement: crate::placement::Placement,
) -> f64 {
    let (w, h) = placement.output;
    let local = t - start;
    layer_homography(
        &placement.placed(spec, local),
        spec.at_3d(local),
        camera,
        t,
        w,
        h,
        1.0,
    )
    .1
}

/// Bytes of a list of homographies (for frame keys).
pub fn hash_mats(ms: &[Mat3]) -> Vec<u8> {
    let mut v = Vec::with_capacity(ms.len() * 72);
    for m in ms {
        for x in m.iter().flatten() {
            v.extend_from_slice(&x.to_bits().to_le_bytes());
        }
    }
    v
}

/// What the multi-sample projective kernel needs.
#[derive(Clone, Debug, PartialEq)]
pub struct MultiSetup {
    /// Destination display dimensions.
    pub display: (u32, u32),
    /// Output pixel -> full-resolution source pixel, per sample (`None`:
    /// singular, the sample is transparent). Averaged with equal weights.
    pub inverses: Vec<Option<Mat3>>,
    /// 2x box levels taken before the kernel.
    pub mip: [u32; 2],
    /// Output data window (display pixels), never empty.
    pub window: PixelRect,
}

/// Source pixels covered by one output pixel along source x / y at output
/// point `d` for inverse map `g`, or `None` behind the camera.
fn minification(g: &Mat3, d: [f64; 2]) -> Option<[f64; 2]> {
    let sh = apply(g, [d[0], d[1], 1.0]);
    if sh[2] <= 0.0 || sh[2] > 1.0 / NEAR {
        return None;
    }
    let s = [sh[0] / sh[2], sh[1] / sh[2]];
    let row = |k: usize| {
        let a = (g[k][0] - s[k] * g[2][0]) / sh[2];
        let b = (g[k][1] - s[k] * g[2][1]) / sh[2];
        (a * a + b * b).sqrt()
    };
    Some([row(0), row(1)])
}

/// Plan the kernel for forward maps `fwds` (one per sample) of a source
/// covering `src` in a `width` x `height` display window. `None` if every
/// sample is singular (fully transparent result).
pub fn plan_multi(fwds: &[Mat3], src: PixelRect, width: u32, height: u32) -> Option<MultiSetup> {
    let inverses: Vec<Option<Mat3>> = fwds.iter().map(inverse).collect();
    if inverses.iter().all(Option::is_none) {
        return None;
    }
    let corners = |m: f64| {
        let (x0, y0) = (src.x as f64 - m, src.y as f64 - m);
        let (x1, y1) = (src.right() as f64 + m, src.bottom() as f64 + m);
        [[x0, y0], [x1, y0], [x0, y1], [x1, y1]]
    };
    let center = [
        src.x as f64 + src.width as f64 / 2.0,
        src.y as f64 + src.height as f64 / 2.0,
    ];
    // Project a source point; `None` when it is behind / at the camera.
    let project = |h: &Mat3, s: [f64; 2]| {
        let v = apply(h, [s[0], s[1], 1.0]);
        (v[2] >= NEAR).then(|| [v[0] / v[2], v[1] / v[2]])
    };
    // Mip: the smallest level any sample needs at the projected corners and
    // center (so no evaluated point is filtered wider than it needs).
    let mut mip = [MAX_MIP; 2];
    let mut max_fs = [1.0f64; 2];
    for (h, g) in fwds.iter().zip(&inverses) {
        let Some(g) = g else { continue };
        for s in corners(0.0).into_iter().chain([center]) {
            if let Some(d) = project(h, s)
                && let Some(e) = minification(g, d)
            {
                for k in 0..2 {
                    mip[k] = mip[k].min(mip_levels(e[k]));
                    max_fs[k] = max_fs[k].max(e[k]);
                }
            }
        }
    }
    let mip = mip.map(|m| if m == MAX_MIP { 0 } else { m });
    let radius = [0, 1].map(|k| {
        let fs = (max_fs[k] / f64::from(1u32 << mip[k])).clamp(1.0, MAX_FILTER_SCALE);
        (KERNEL_RADIUS * fs).ceil()
    });
    let margin = [0, 1].map(|k| {
        if mip[k] == 0 {
            radius[k]
        } else {
            (radius[k] + 1.0) * f64::from(1u32 << mip[k])
        }
    });
    let m = margin[0].max(margin[1]);
    let mut bb = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut full = false;
    for (h, g) in fwds.iter().zip(&inverses) {
        if g.is_none() {
            continue;
        }
        for s in corners(m) {
            match project(h, s) {
                Some(d) => {
                    bb[0] = bb[0].min(d[0]);
                    bb[1] = bb[1].min(d[1]);
                    bb[2] = bb[2].max(d[0]);
                    bb[3] = bb[3].max(d[1]);
                }
                None => full = true,
            }
        }
    }
    let window = if full {
        PixelRect::full(width, height)
    } else {
        let cx = |v: f64| v.clamp(0.0, width as f64) as i64;
        let cy = |v: f64| v.clamp(0.0, height as f64) as i64;
        let (x0, x1) = (cx(bb[0].floor()), cx(bb[2].ceil()));
        let (y0, y1) = (cy(bb[1].floor()), cy(bb[3].ceil()));
        if x1 > x0 && y1 > y0 {
            PixelRect::new(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32)
        } else {
            PixelRect::new(0, 0, 1, 1)
        }
    };
    Some(MultiSetup {
        display: (width, height),
        inverses,
        mip,
        window,
    })
}

const MAX_MIP: u32 = u32::MAX;

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }
    fn proj(h: &Mat3, s: [f64; 2]) -> [f64; 2] {
        let v = apply(h, [s[0], s[1], 1.0]);
        [v[0] / v[2], v[1] / v[2]]
    }

    #[test]
    fn default_3d_layer_is_the_2d_layer() {
        let t = TransformAt {
            position: [50.0, 20.0],
            anchor: [32.0, 16.0],
            scale: [2.0, 0.5],
            rotation_deg: 30.0,
        };
        for par in [1.0, 2.0] {
            let (h, depth) = layer_homography(&t, [0.0; 7], None, RationalTime::ZERO, 64, 32, par);
            let a = affine_homography(&t, par);
            for s in [[0.0, 0.0], [10.0, 3.0], [63.0, 31.0]] {
                assert!(near(proj(&h, s), proj(&a, s)), "{par} {s:?}");
            }
            assert!((depth - default_zoom(64.0 * par)).abs() < 1e-9);
        }
    }

    #[test]
    fn depth_perspective_and_rotations() {
        let id = TransformAt::identity(100, 100);
        // Pushed back by zoom: half size around the center.
        let z = default_zoom(100.0);
        let (h, d) = layer_homography(
            &id,
            [z, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            None,
            RationalTime::ZERO,
            100,
            100,
            1.0,
        );
        assert!((d - 2.0 * z).abs() < 1e-9);
        assert!(near(proj(&h, [100.0, 50.0]), [75.0, 50.0]));
        // Y rotation by 90 degrees: the card is edge-on (singular).
        let (h, _) = layer_homography(
            &id,
            [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 90.0],
            None,
            RationalTime::ZERO,
            100,
            100,
            1.0,
        );
        let (a, b) = (proj(&h, [0.0, 50.0]), proj(&h, [100.0, 50.0]));
        assert!((a[0] - b[0]).abs() < 1e-6 && (a[0] - 50.0).abs() < 1e-6);
        // Y rotation by 30 degrees: the right edge comes towards the viewer
        // (right-handed Ry maps +x to -z), so it is taller on screen.
        let (h, _) = layer_homography(
            &id,
            [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 30.0],
            None,
            RationalTime::ZERO,
            100,
            100,
            1.0,
        );
        let l = proj(&h, [0.0, 100.0])[1] - proj(&h, [0.0, 0.0])[1];
        let r = proj(&h, [100.0, 100.0])[1] - proj(&h, [100.0, 0.0])[1];
        assert!(l < 100.0 && r > 100.0, "{l} {r}");
        // X rotation by 30 degrees: the bottom edge moves away (narrower).
        let (h3, _) = layer_homography(
            &id,
            [0.0, 0.0, 0.0, 0.0, 0.0, 30.0, 0.0],
            None,
            RationalTime::ZERO,
            100,
            100,
            1.0,
        );
        let top = proj(&h3, [100.0, 0.0])[0] - proj(&h3, [0.0, 0.0])[0];
        let bottom = proj(&h3, [100.0, 100.0])[0] - proj(&h3, [0.0, 100.0])[0];
        assert!(top > 100.0 && bottom < 100.0, "{top} {bottom}");
        // Orientation and rotation compose: orientation y 30 = rotation_y 30.
        let (h2, _) = layer_homography(
            &id,
            [0.0, 0.0, 0.0, 30.0, 0.0, 0.0, 0.0],
            None,
            RationalTime::ZERO,
            100,
            100,
            1.0,
        );
        assert!(near(proj(&h, [3.0, 7.0]), proj(&h2, [3.0, 7.0])));
    }

    #[test]
    fn camera_moves_and_fov() {
        let id = TransformAt::identity(100, 100);
        let cam: CameraSpec = serde_json::from_str(
            r#"{"position": ["60", "50", "-100"], "point_of_interest": ["60", "50", "0"], "zoom": "100"}"#,
        )
        .unwrap();
        cam.validate().unwrap();
        let (h, d) = layer_homography(&id, [0.0; 7], Some(&cam), RationalTime::ZERO, 100, 100, 1.0);
        assert_eq!(d, 100.0);
        // The camera moved right by 10: the layer moves left by 10.
        assert!(near(proj(&h, [50.0, 50.0]), [40.0, 50.0]));
        let fov: CameraSpec = serde_json::from_str(r#"{"fov_deg": "90"}"#).unwrap();
        assert!((fov.at(RationalTime::ZERO, 100.0, 50.0).zoom - 50.0).abs() < 1e-9);
        let both: CameraSpec = serde_json::from_str(r#"{"fov_deg": "90", "zoom": "5"}"#).unwrap();
        assert!(both.validate().is_err());
        // Looking straight down still has a basis.
        let down = CameraAt {
            position: [0.0, -10.0, 0.0],
            point_of_interest: [0.0, 0.0, 0.0],
            zoom: 1.0,
        };
        let v = view_basis(&down);
        assert!(v.iter().flatten().all(|x| x.is_finite()));
    }

    #[test]
    fn shutter_times_are_exact_and_centered() {
        let mb = MotionBlurSpec::default();
        mb.validate().unwrap();
        let fps = Rational::from_int(24);
        let ts = mb.sample_times(RationalTime::new(1, 1), fps);
        assert_eq!(ts.len(), 16);
        // 180 degrees at -90: [t - 1/96, t + 1/96], sample centers.
        assert_eq!(
            ts[0],
            RationalTime(Rational::ONE - Rational::new(1, 96) + Rational::new(1, 1536))
        );
        assert_eq!(
            ts[15],
            RationalTime(Rational::ONE + Rational::new(1, 96) - Rational::new(1, 1536))
        );
        let bad = MotionBlurSpec { samples: 1, ..mb };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn plan_windows() {
        let full = PixelRect::full(64, 32);
        let t = TransformAt::identity(64, 32);
        let a = affine_homography(&t, 1.0);
        let k = plan_multi(&[a], full, 64, 32).unwrap();
        assert_eq!((k.mip, k.window), ([0, 0], full));
        // Two horizontal positions: the window covers both.
        let b = affine_homography(
            &TransformAt {
                position: [32.0 - 40.0, 16.0],
                scale: [0.25, 0.25],
                ..t
            },
            1.0,
        );
        let c = affine_homography(
            &TransformAt {
                position: [32.0 + 10.0, 16.0],
                scale: [0.25, 0.25],
                ..t
            },
            1.0,
        );
        let k = plan_multi(&[b, c], full, 64, 32).unwrap();
        assert_eq!(k.window.x, 0);
        assert!(k.window.right() >= 52 && k.window.right() <= 64);
        // Strong minification takes a box level.
        let s = affine_homography(
            &TransformAt {
                scale: [1.0 / 20.0, 1.0],
                ..t
            },
            1.0,
        );
        assert_eq!(plan_multi(&[s], full, 64, 32).unwrap().mip, [2, 0]);
        // Behind the camera: full window.
        let behind = layer_homography(
            &TransformAt::identity(64, 32),
            [-2.0 * default_zoom(64.0), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            None,
            RationalTime::ZERO,
            64,
            32,
            1.0,
        )
        .0;
        assert_eq!(plan_multi(&[behind], full, 64, 32).unwrap().window, full);
        // Singular: nothing.
        assert!(plan_multi(&[[[0.0; 3]; 3]], full, 64, 32).is_none());
    }
}
