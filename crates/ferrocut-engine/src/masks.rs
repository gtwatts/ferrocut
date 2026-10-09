//! GPU-free animated layer-mask coverage in layer-pixel coordinates.
//! Media/comp layers use native source pixels; generators use output pixels.
//! Masks run before clip effects, fit and the layer transform; animation uses source time.
//!
//! Calls the pinned EffectCraft path constructors/converter and FilmCraft's
//! `MaskPath::flatten` and `MaskMode::{start,combine}`. The bounded scan-row
//! signed-distance kernel below is adapted from FilmCraft's MIT/Apache-2.0
//! `crates/render/src/mask.rs` at 5231852443363f001c3f6b396dd9b1e6461ae2be.
//! Geometry is sampled at the exact supplied source clock; no frame/tick
//! quantization or clip-local key shifting is performed here.
//!
//! Feather is an isotropic smooth distance falloff of the specified pixel width,
//! centred on the expanded edge. A hard edge has a one-pixel linear ramp.
//! Open paths fill as if closed. Multiple contours respect nonzero/even-odd
//! winding. This primitive does not claim anisotropic/variable feather,
//! tracking, rotoscoping, effect masks, or motion-blur sampling.

use std::sync::Arc;

use effectcraft_path::BezPath;
use ferrocut_core::{
    AlphaMode, Animatable, CpuFrame, CpuImage, Keyframe, KeyframeTrack, NodeError, PixelRect,
    Rational, RationalTime,
};
use filmcraft_geom::Vec2;
use filmcraft_project::mask::{MaskMode as FilmMode, MaskPath, MaskVertex};
use half::f16;
use serde::{Deserialize, Serialize};

use crate::vector::{VectorCommand, VectorFillRule, VectorGeometry};

pub const MASK_VERSION: &[u8] = b"masks.v1.filmcraft.5231852.effectcraft.6943872.signed-distance";
pub const MAX_MASKS: usize = 64;
pub const MAX_MASK_COMMANDS: usize = 256;
pub const MAX_MASK_PARAMETERS: usize = 16_384;
pub const MAX_MASK_KEYS: usize = 65_536;
pub const MAX_FLAT_EDGES: usize = 32_768;
pub const MAX_MASK_PIXELS: usize = 8 * 1024 * 1024;
pub const MAX_MASK_WORK: u64 = 256_000_000;
pub const MAX_MASK_AXIS: u32 = 8192;
pub const FLATTEN_TOLERANCE: f64 = 0.05;
const MAX_COORDINATE: f64 = 1_000_000.0;
const MAX_REGION_COORDINATE: i64 = 65_536;
const MAX_STYLE_PIXELS: f64 = 512.0;

fn one() -> Animatable {
    Animatable::constant(Rational::ONE)
}
fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskMode {
    None,
    #[default]
    Add,
    Subtract,
    Intersect,
    Lighten,
    Darken,
    Difference,
}

impl MaskMode {
    fn upstream(self) -> FilmMode {
        match self {
            Self::None => FilmMode::None,
            Self::Add => FilmMode::Add,
            Self::Subtract => FilmMode::Subtract,
            Self::Intersect => FilmMode::Intersect,
            Self::Lighten => FilmMode::Lighten,
            Self::Darken => FilmMode::Darken,
            Self::Difference => FilmMode::Difference,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskSpec {
    pub geometry: VectorGeometry,
    #[serde(default)]
    pub mode: MaskMode,
    #[serde(default)]
    pub inverted: bool,
    /// Coverage fraction (0..1), applied after inversion.
    #[serde(default = "one")]
    pub opacity: Animatable,
    /// Isotropic smooth distance-falloff width in working-layer pixels (0..512).
    #[serde(default)]
    pub feather: Animatable,
    /// Signed distance expansion in working-layer pixels (-512..512).
    #[serde(default)]
    pub expansion: Animatable,
    #[serde(default)]
    pub fill_rule: VectorFillRule,
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// Serialized as an array, so a clip's `masks` value has no wrapper object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaskStack {
    pub masks: Vec<MaskSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaskCoverage {
    pub width: u32,
    pub height: u32,
    pub region: PixelRect,
    /// Row-major coverage in [0,1], for `region` (including negative origins).
    pub values: Vec<f32>,
    /// False means the stack is a pass-through; values are then all one.
    pub active: bool,
}

fn vec2<'a>(out: &mut Vec<(String, &'a Animatable)>, prefix: &str, p: &'a [Animatable; 2]) {
    for (name, a) in ["x", "y"].into_iter().zip(p) {
        out.push((format!("{prefix}.{name}"), a));
    }
}
fn vec2_mut<'a>(
    out: &mut Vec<(String, &'a mut Animatable)>,
    prefix: &str,
    p: &'a mut [Animatable; 2],
) {
    for (name, a) in ["x", "y"].into_iter().zip(p) {
        out.push((format!("{prefix}.{name}"), a));
    }
}

// A shared field list keeps mutable and immutable property paths identical.
macro_rules! geometry_params {
    ($geometry:expr, $out:ident, $vec2:ident, $iter:ident) => {
        match $geometry {
            VectorGeometry::Rectangle {
                x,
                y,
                width,
                height,
                radius,
            } => {
                for (name, a) in [
                    ("x", x),
                    ("y", y),
                    ("width", width),
                    ("height", height),
                    ("radius", radius),
                ] {
                    $out.push((format!("geometry.{name}"), a));
                }
            }
            VectorGeometry::Ellipse { center, radius } => {
                $vec2(&mut $out, "geometry.center", center);
                $vec2(&mut $out, "geometry.radius", radius);
            }
            VectorGeometry::Polygon {
                center,
                points,
                radius,
                rotation,
                roundness,
            } => {
                $vec2(&mut $out, "geometry.center", center);
                for (name, a) in [
                    ("points", points),
                    ("radius", radius),
                    ("rotation", rotation),
                    ("roundness", roundness),
                ] {
                    $out.push((format!("geometry.{name}"), a));
                }
            }
            VectorGeometry::Star {
                center,
                points,
                inner_radius,
                outer_radius,
                rotation,
                inner_roundness,
                outer_roundness,
            } => {
                $vec2(&mut $out, "geometry.center", center);
                for (name, a) in [
                    ("points", points),
                    ("inner_radius", inner_radius),
                    ("outer_radius", outer_radius),
                    ("rotation", rotation),
                    ("inner_roundness", inner_roundness),
                    ("outer_roundness", outer_roundness),
                ] {
                    $out.push((format!("geometry.{name}"), a));
                }
            }
            VectorGeometry::Path { commands } => {
                for (i, c) in commands.$iter().enumerate() {
                    let base = format!("geometry.commands.{i}");
                    match c {
                        VectorCommand::MoveTo { point } | VectorCommand::LineTo { point } => {
                            $vec2(&mut $out, &format!("{base}.point"), point);
                        }
                        VectorCommand::QuadTo { control, to } => {
                            $vec2(&mut $out, &format!("{base}.control"), control);
                            $vec2(&mut $out, &format!("{base}.to"), to);
                        }
                        VectorCommand::CubicTo {
                            control1,
                            control2,
                            to,
                        } => {
                            $vec2(&mut $out, &format!("{base}.control1"), control1);
                            $vec2(&mut $out, &format!("{base}.control2"), control2);
                            $vec2(&mut $out, &format!("{base}.to"), to);
                        }
                        VectorCommand::Close => {}
                    }
                }
            }
        }
    };
}

fn range(name: &str, geometry: &VectorGeometry) -> (f64, f64) {
    match name {
        "opacity" => (0.0, 1.0),
        "feather" => (0.0, MAX_STYLE_PIXELS),
        "expansion" => (-MAX_STYLE_PIXELS, MAX_STYLE_PIXELS),
        "geometry.points" => (
            3.0,
            if matches!(geometry, VectorGeometry::Star { .. }) {
                128.0
            } else {
                256.0
            },
        ),
        "geometry.rotation" => (-36_000.0, 36_000.0),
        "geometry.roundness" | "geometry.inner_roundness" | "geometry.outer_roundness" => {
            (0.0, 100.0)
        }
        "geometry.width"
        | "geometry.height"
        | "geometry.radius"
        | "geometry.radius.x"
        | "geometry.radius.y"
        | "geometry.inner_radius"
        | "geometry.outer_radius" => (0.0, MAX_COORDINATE),
        _ => (-MAX_COORDINATE, MAX_COORDINATE),
    }
}

fn keys(a: &Animatable) -> usize {
    match a {
        Animatable::Constant(_) => 0,
        Animatable::Keyframes(k) => k.keyframes.len(),
        Animatable::Expression(e) => match e.value.as_deref() {
            Some(Animatable::Keyframes(k)) => k.keyframes.len(),
            Some(Animatable::Expression(_)) => MAX_MASK_KEYS + 1,
            _ => 0,
        },
    }
}

impl MaskSpec {
    pub fn has_active(&self) -> bool {
        self.enabled && self.mode != MaskMode::None
    }

    pub fn all(&self) -> Vec<(String, &Animatable)> {
        let mut out = Vec::new();
        geometry_params!(&self.geometry, out, vec2, iter);
        out.extend([
            ("opacity".into(), &self.opacity),
            ("feather".into(), &self.feather),
            ("expansion".into(), &self.expansion),
        ]);
        out
    }

    pub fn all_mut(&mut self) -> Vec<(String, &mut Animatable)> {
        let mut out = Vec::new();
        geometry_params!(&mut self.geometry, out, vec2_mut, iter_mut);
        out.extend([
            ("opacity".into(), &mut self.opacity),
            ("feather".into(), &mut self.feather),
            ("expansion".into(), &mut self.expansion),
        ]);
        out
    }

    pub fn validate(&self) -> Result<(), String> {
        if let VectorGeometry::Path { commands } = &self.geometry {
            if commands.is_empty() || commands.len() > MAX_MASK_COMMANDS {
                return Err(format!(
                    "geometry.commands: expected 1..={MAX_MASK_COMMANDS} commands"
                ));
            }
            let (mut active, mut segment, mut any_segment) = (false, false, false);
            for (i, command) in commands.iter().enumerate() {
                match command {
                    VectorCommand::MoveTo { .. } => {
                        active = true;
                        segment = false;
                    }
                    VectorCommand::Close => {
                        if !active || !segment {
                            return Err(format!(
                                "geometry.commands.{i}: close requires a contour with a segment"
                            ));
                        }
                        active = false;
                    }
                    _ => {
                        if !active {
                            return Err(format!(
                                "geometry.commands.{i}: start a contour with move_to"
                            ));
                        }
                        segment = true;
                        any_segment = true;
                    }
                }
            }
            if !any_segment {
                return Err("geometry.commands: path has no drawable segments".into());
            }
        }
        let parameters = self.all();
        let total_keys = parameters
            .iter()
            .try_fold(0usize, |n, (_, a)| n.checked_add(keys(a)))
            .ok_or("keyframe count overflow")?;
        if total_keys > MAX_MASK_KEYS {
            return Err(format!("at most {MAX_MASK_KEYS} keyframes are allowed"));
        }
        for (name, a) in parameters {
            a.validate().map_err(|e| format!("{name}: {e}"))?;
            if !a.is_expression() {
                let (lo, hi) = a.key_range();
                let (min, max) = range(&name, &self.geometry);
                if lo.to_f64() < min || hi.to_f64() > max {
                    return Err(format!("{name}: keys must be in {min}..={max}"));
                }
            }
        }
        Ok(())
    }
}

/// Checked rational subtraction that does not negate i64::MIN.
fn difference(a: Rational, b: Rational) -> Result<Rational, String> {
    Rational::try_new(
        a.num() as i128 * b.den() as i128 - b.num() as i128 * a.den() as i128,
        a.den() as i128 * b.den() as i128,
    )
    .map_err(|e| format!("mask keyframe time arithmetic: {e}"))
}

/// Preserve the core interpolation semantics, using a checked local segment
/// origin so hostile rational denominators cannot panic inside its evaluator.
fn sample(a: &Animatable, t: RationalTime) -> Result<f64, String> {
    match a {
        Animatable::Constant(v) => Ok(v.to_f64()),
        Animatable::Expression(_) => {
            Err("expression must be baked before mask coverage is sampled".into())
        }
        Animatable::Keyframes(track) => {
            let first = track
                .keyframes
                .first()
                .ok_or("mask keyframe track is empty")?;
            if t <= first.t {
                return Ok(first.v.to_f64());
            }
            let i = track.keyframes.partition_point(|key| key.t <= t);
            let a = track
                .keyframes
                .get(i.saturating_sub(1))
                .ok_or("invalid mask keyframe interval")?;
            let Some(b) = track.keyframes.get(i) else {
                return Ok(a.v.to_f64());
            };
            if a.t == t {
                return Ok(a.v.to_f64());
            }
            let elapsed = difference(t.0, a.t.0)?;
            let span = difference(b.t.0, a.t.0)?;
            let local = KeyframeTrack {
                keyframes: vec![
                    Keyframe {
                        t: RationalTime::ZERO,
                        ..a.clone()
                    },
                    Keyframe {
                        t: RationalTime(span),
                        ..b.clone()
                    },
                ],
            };
            Ok(local.eval(RationalTime(elapsed)))
        }
    }
}

fn value(a: &Animatable, t: RationalTime, name: &str, min: f64, max: f64) -> Result<f64, String> {
    let v = sample(a, t).map_err(|e| format!("{name}: {e}"))?;
    if v.is_finite() && (min..=max).contains(&v) {
        Ok(v)
    } else {
        Err(format!(
            "{name}: sampled value must be finite and in {min}..={max}"
        ))
    }
}
fn point(p: &[Animatable; 2], t: RationalTime, name: &str) -> Result<[f64; 2], String> {
    Ok([
        value(
            &p[0],
            t,
            &format!("{name}.x"),
            -MAX_COORDINATE,
            MAX_COORDINATE,
        )?,
        value(
            &p[1],
            t,
            &format!("{name}.y"),
            -MAX_COORDINATE,
            MAX_COORDINATE,
        )?,
    ])
}

fn geometry_path(geometry: &VectorGeometry, t: RationalTime) -> Result<BezPath, String> {
    let dimension = |a: &Animatable, name: &str| value(a, t, name, 0.0, MAX_COORDINATE);
    match geometry {
        VectorGeometry::Rectangle {
            x,
            y,
            width,
            height,
            radius,
        } => {
            let x = value(x, t, "geometry.x", -MAX_COORDINATE, MAX_COORDINATE)?;
            let y = value(y, t, "geometry.y", -MAX_COORDINATE, MAX_COORDINATE)?;
            let (w, h) = (
                dimension(width, "geometry.width")?,
                dimension(height, "geometry.height")?,
            );
            let r = dimension(radius, "geometry.radius")?;
            if w == 0.0 || h == 0.0 {
                return Ok(BezPath::new());
            }
            Ok(effectcraft_path::rect(
                [w, h],
                [x + w * 0.5, y + h * 0.5],
                r,
            ))
        }
        VectorGeometry::Ellipse { center, radius } => {
            let center = point(center, t, "geometry.center")?;
            let r = [
                dimension(&radius[0], "geometry.radius.x")?,
                dimension(&radius[1], "geometry.radius.y")?,
            ];
            if r[0] == 0.0 || r[1] == 0.0 {
                return Ok(BezPath::new());
            }
            Ok(effectcraft_path::ellipse([r[0] * 2.0, r[1] * 2.0], center))
        }
        VectorGeometry::Polygon {
            center,
            points,
            radius,
            rotation,
            roundness,
        } => {
            let center = point(center, t, "geometry.center")?;
            let n = value(points, t, "geometry.points", 3.0, 256.0)?;
            let radius = dimension(radius, "geometry.radius")?;
            let rotation = value(rotation, t, "geometry.rotation", -36_000.0, 36_000.0)?;
            let roundness = value(roundness, t, "geometry.roundness", 0.0, 100.0)?;
            if radius == 0.0 {
                return Ok(BezPath::new());
            }
            Ok(effectcraft_path::polystar(
                false, n, center, rotation, 0.0, radius, 0.0, roundness,
            ))
        }
        VectorGeometry::Star {
            center,
            points,
            inner_radius,
            outer_radius,
            rotation,
            inner_roundness,
            outer_roundness,
        } => {
            let center = point(center, t, "geometry.center")?;
            let n = value(points, t, "geometry.points", 3.0, 128.0)?;
            let inner = dimension(inner_radius, "geometry.inner_radius")?;
            let outer = dimension(outer_radius, "geometry.outer_radius")?;
            let rotation = value(rotation, t, "geometry.rotation", -36_000.0, 36_000.0)?;
            let ir = value(inner_roundness, t, "geometry.inner_roundness", 0.0, 100.0)?;
            let or = value(outer_roundness, t, "geometry.outer_roundness", 0.0, 100.0)?;
            if inner == 0.0 && outer == 0.0 {
                return Ok(BezPath::new());
            }
            Ok(effectcraft_path::polystar(
                true, n, center, rotation, inner, outer, ir, or,
            ))
        }
        VectorGeometry::Path { commands } => {
            let mut path = BezPath::new();
            for (i, command) in commands.iter().enumerate() {
                let base = format!("geometry.commands.{i}");
                match command {
                    VectorCommand::MoveTo { point: p } => {
                        let p = point(p, t, &format!("{base}.point"))?;
                        path.move_to((p[0], p[1]));
                    }
                    VectorCommand::LineTo { point: p } => {
                        let p = point(p, t, &format!("{base}.point"))?;
                        path.line_to((p[0], p[1]));
                    }
                    VectorCommand::QuadTo { control, to } => {
                        let c = point(control, t, &format!("{base}.control"))?;
                        let p = point(to, t, &format!("{base}.to"))?;
                        path.quad_to((c[0], c[1]), (p[0], p[1]));
                    }
                    VectorCommand::CubicTo {
                        control1,
                        control2,
                        to,
                    } => {
                        let a = point(control1, t, &format!("{base}.control1"))?;
                        let b = point(control2, t, &format!("{base}.control2"))?;
                        let p = point(to, t, &format!("{base}.to"))?;
                        path.curve_to((a[0], a[1]), (b[0], b[1]), (p[0], p[1]));
                    }
                    VectorCommand::Close => path.close_path(),
                }
            }
            Ok(path)
        }
    }
}

#[derive(Clone, Debug)]
struct FlatMask {
    edges: Vec<([f32; 2], [f32; 2])>,
    feather: f32,
    expansion: f32,
    opacity: f32,
    inverted: bool,
    mode: MaskMode,
    fill_rule: VectorFillRule,
}
impl FlatMask {
    fn band(&self) -> f32 {
        self.expansion.abs() + self.feather.max(1.0) * 0.5 + 1.0
    }
}

// Predict FilmCraft's Wang-bound flattening before calling its bounded but
// allocating flatten(). Reject curves that would hit its 256-chord accuracy cap.
fn flattened_bound(path: &MaskPath) -> Result<usize, String> {
    let mut points = 1usize;
    let mut closed = path.clone();
    closed.closed = true;
    for [p0, c1, c2, p3] in closed.segments() {
        let straight = (c1 - p0).length() < 1e-12 && (c2 - p3).length() < 1e-12;
        let steps = if straight {
            1
        } else {
            let m = (p0 - c1 * 2.0 + c2)
                .length()
                .max((c1 - c2 * 2.0 + p3).length());
            let steps = (0.75 * m / FLATTEN_TOLERANCE).sqrt().ceil().max(1.0);
            if !steps.is_finite() || steps > 256.0 {
                return Err(
                    "curve exceeds the 256-chord per-segment accuracy budget; split or simplify it"
                        .into(),
                );
            }
            steps as usize
        };
        points = points
            .checked_add(steps)
            .ok_or("flattened point count overflow")?;
    }
    Ok(points)
}

fn prepare(mask: &MaskSpec, t: RationalTime, total: &mut usize) -> Result<FlatMask, String> {
    let path = geometry_path(&mask.geometry, t)?;
    let shapes = effectcraft_path::from_kurbo(&path);
    let mut edges = Vec::new();
    for shape in shapes {
        let mut vertices = Vec::new();
        vertices
            .try_reserve_exact(shape.vertices.len())
            .map_err(|e| format!("mask vertices allocation: {e}"))?;
        for (i, p) in shape.vertices.iter().enumerate() {
            let a = shape.in_tangents.get(i).copied().unwrap_or([0.0; 2]);
            let b = shape.out_tangents.get(i).copied().unwrap_or([0.0; 2]);
            vertices.push(MaskVertex {
                p: Vec2::new(p[0], p[1]),
                t_in: Vec2::new(a[0], a[1]),
                t_out: Vec2::new(b[0], b[1]),
            });
        }
        let native = MaskPath {
            vertices,
            closed: shape.closed,
        };
        let bound = flattened_bound(&native)?;
        let next = total
            .checked_add(bound)
            .filter(|n| *n <= MAX_FLAT_EDGES)
            .ok_or_else(|| format!("flattened mask geometry exceeds {MAX_FLAT_EDGES} edges"))?;
        let flat = native.flatten(FLATTEN_TOLERANCE);
        if flat.len() < 3 {
            continue;
        }
        *total = next;
        edges
            .try_reserve(flat.len())
            .map_err(|e| format!("mask edges allocation: {e}"))?;
        for (a, b) in flat
            .iter()
            .zip(flat.iter().cycle().skip(1))
            .take(flat.len())
        {
            let a = [a.x as f32, a.y as f32];
            let b = [b.x as f32, b.y as f32];
            if a.iter().chain(&b).any(|v| !v.is_finite()) {
                return Err("flattened coordinates must be finite".into());
            }
            edges.push((a, b));
        }
    }
    Ok(FlatMask {
        edges,
        feather: value(&mask.feather, t, "feather", 0.0, MAX_STYLE_PIXELS)? as f32,
        expansion: value(
            &mask.expansion,
            t,
            "expansion",
            -MAX_STYLE_PIXELS,
            MAX_STYLE_PIXELS,
        )? as f32,
        opacity: value(&mask.opacity, t, "opacity", 0.0, 1.0)? as f32,
        inverted: mask.inverted,
        mode: mask.mode,
        fill_rule: mask.fill_rule,
    })
}

fn dimensions(width: u32, height: u32, region: PixelRect) -> Result<usize, String> {
    if width == 0 || height == 0 || width > MAX_MASK_AXIS || height > MAX_MASK_AXIS {
        return Err(format!("mask canvas axes must be in 1..={MAX_MASK_AXIS}"));
    }
    let display = (width as usize)
        .checked_mul(height as usize)
        .ok_or("mask canvas size overflow")?;
    if display > MAX_MASK_PIXELS {
        return Err(format!("mask canvas exceeds {MAX_MASK_PIXELS} pixels"));
    }
    if region.width > MAX_MASK_AXIS || region.height > MAX_MASK_AXIS {
        return Err(format!("mask region axes must not exceed {MAX_MASK_AXIS}"));
    }
    if [
        region.x as i64,
        region.y as i64,
        region.right(),
        region.bottom(),
    ]
    .iter()
    .any(|v| v.abs() > MAX_REGION_COORDINATE)
    {
        return Err(format!(
            "mask region coordinates must be in ±{MAX_REGION_COORDINATE}"
        ));
    }
    (region.width as usize)
        .checked_mul(region.height as usize)
        .filter(|n| *n <= MAX_MASK_PIXELS)
        .ok_or_else(|| format!("mask region exceeds {MAX_MASK_PIXELS} pixels"))
}

fn plane(size: usize, value: f32) -> Result<Vec<f32>, String> {
    let mut out = Vec::new();
    out.try_reserve_exact(size)
        .map_err(|e| format!("mask coverage allocation: {e}"))?;
    out.resize(size, value);
    Ok(out)
}

impl MaskStack {
    pub fn is_empty(&self) -> bool {
        self.masks.is_empty()
    }
    pub fn has_active(&self) -> bool {
        self.masks.iter().any(MaskSpec::has_active)
    }
    pub fn all(&self) -> Vec<(String, &Animatable)> {
        self.masks
            .iter()
            .enumerate()
            .flat_map(|(i, m)| {
                m.all()
                    .into_iter()
                    .map(move |(name, a)| (format!("{i}.{name}"), a))
            })
            .collect()
    }
    pub fn all_mut(&mut self) -> Vec<(String, &mut Animatable)> {
        self.masks
            .iter_mut()
            .enumerate()
            .flat_map(|(i, m)| {
                m.all_mut()
                    .into_iter()
                    .map(move |(name, a)| (format!("{i}.{name}"), a))
            })
            .collect()
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.masks.len() > MAX_MASKS {
            return Err(format!("at most {MAX_MASKS} masks are allowed"));
        }
        let (mut count, mut key_count) = (0usize, 0usize);
        for (i, mask) in self.masks.iter().enumerate() {
            mask.validate().map_err(|e| format!("masks.{i}: {e}"))?;
            let parameters = mask.all();
            count = count
                .checked_add(parameters.len())
                .ok_or("mask parameter count overflow")?;
            for (_, a) in parameters {
                key_count = key_count
                    .checked_add(keys(a))
                    .ok_or("mask keyframe count overflow")?;
            }
            if count > MAX_MASK_PARAMETERS || key_count > MAX_MASK_KEYS {
                return Err(format!(
                    "mask stack exceeds {MAX_MASK_PARAMETERS} numeric properties or {MAX_MASK_KEYS} keys"
                ));
            }
        }
        Ok(())
    }
    fn prepared(
        &self,
        source_time: RationalTime,
        check: &dyn Fn() -> Result<(), NodeError>,
    ) -> Result<Vec<FlatMask>, NodeError> {
        check()?;
        self.validate().map_err(NodeError::permanent)?;
        let mut total = 0usize;
        let mut out = Vec::new();
        for (i, mask) in self
            .masks
            .iter()
            .enumerate()
            .filter(|(_, m)| m.has_active())
        {
            check()?;
            out.push(
                prepare(mask, source_time, &mut total)
                    .map_err(|e| NodeError::permanent(format!("masks.{i}: {e}")))?,
            );
        }
        Ok(out)
    }
    pub fn hash_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut out = MASK_VERSION.to_vec();
        out.extend(serde_json::to_vec(self).map_err(|e| format!("serialize mask stack: {e}"))?);
        Ok(out)
    }
    /// Evaluated source-time coverage identity, independent of canvas/ROI.
    /// Include display dimensions and region in the caller's frame cache key.
    pub fn hash_bytes_at(&self, source_time: RationalTime) -> Result<Vec<u8>, String> {
        let prepared = self
            .prepared(source_time, &|| Ok(()))
            .map_err(|e| e.message)?;
        let mut out = MASK_VERSION.to_vec();
        out.extend_from_slice(&(prepared.len() as u32).to_le_bytes());
        for mask in prepared {
            out.extend([mask.mode as u8, mask.inverted as u8, mask.fill_rule as u8]);
            for v in [mask.feather, mask.expansion, mask.opacity] {
                out.extend_from_slice(&v.to_bits().to_le_bytes());
            }
            out.extend_from_slice(&(mask.edges.len() as u32).to_le_bytes());
            for (a, b) in mask.edges {
                for v in a.into_iter().chain(b) {
                    out.extend_from_slice(&v.to_bits().to_le_bytes());
                }
            }
        }
        Ok(out)
    }
    pub fn coverage(
        &self,
        source_time: RationalTime,
        width: u32,
        height: u32,
        region: PixelRect,
    ) -> Result<MaskCoverage, String> {
        self.coverage_checked(source_time, width, height, region, || Ok(()))
            .map_err(|e| e.message)
    }
    pub fn coverage_checked(
        &self,
        source_time: RationalTime,
        width: u32,
        height: u32,
        region: PixelRect,
        check: impl Fn() -> Result<(), NodeError>,
    ) -> Result<MaskCoverage, NodeError> {
        check()?;
        let count = dimensions(width, height, region).map_err(NodeError::permanent)?;
        let prepared = self.prepared(source_time, &check)?;
        let Some(first) = prepared.first() else {
            return Ok(MaskCoverage {
                width,
                height,
                region,
                values: plane(count, 1.0).map_err(NodeError::permanent)?,
                active: false,
            });
        };
        check_work(&prepared, region, &check)?;
        let mut values =
            plane(count, first.mode.upstream().start()).map_err(NodeError::permanent)?;
        if region.is_empty() {
            return Ok(MaskCoverage {
                width,
                height,
                region,
                values,
                active: true,
            });
        }
        let mut row = plane(region.width as usize, 0.0).map_err(NodeError::permanent)?;
        for (y, acc) in values.chunks_mut(region.width as usize).enumerate() {
            check()?;
            for mask in &prepared {
                mask_row(mask, region.x, region.y as f32 + y as f32 + 0.5, &mut row)
                    .map_err(NodeError::permanent)?;
                for (a, v) in acc.iter_mut().zip(&row) {
                    *a = mask.mode.upstream().combine(*a, *v).clamp(0.0, 1.0);
                }
            }
        }
        check()?;
        Ok(MaskCoverage {
            width,
            height,
            region,
            values,
            active: true,
        })
    }
    pub fn apply_cpu(
        &self,
        input: &CpuFrame,
        source_time: RationalTime,
    ) -> Result<CpuFrame, String> {
        self.coverage(source_time, input.width, input.height, input.data_window)?
            .apply_cpu(input)
    }
}

fn check_work(
    masks: &[FlatMask],
    region: PixelRect,
    check: &dyn Fn() -> Result<(), NodeError>,
) -> Result<(), NodeError> {
    let mut work = 0u64;
    for mask in masks {
        let band = mask.band();
        for y in 0..region.height {
            check()?;
            let py = region.y as f32 + y as f32 + 0.5;
            let near_work: u64 = mask
                .edges
                .iter()
                .filter(|(a, b)| py >= a[1].min(b[1]) - band && py <= a[1].max(b[1]) + band)
                .map(|(a, b)| {
                    horizontal_span(*a, *b, band, region.x, region.width as usize).len() as u64
                })
                .sum();
            // Full row edge scan, sorting upper bound, exact near-edge spans,
            // row initialization, coverage conversion and combination.
            let edges = mask.edges.len() as u64;
            let cost =
                edges * (2 + edges.max(1).ilog2() as u64) + region.width as u64 * 3 + near_work;
            work = work.checked_add(cost).filter(|v| *v <= MAX_MASK_WORK).ok_or_else(|| NodeError::permanent(format!("mask workload exceeds {MAX_MASK_WORK} row/edge operations; reduce feather, geometry, masks, or region")))?;
        }
    }
    Ok(())
}

fn falloff(distance: f32, feather: f32) -> f32 {
    let u = (distance / feather.max(1.0) + 0.5).clamp(0.0, 1.0);
    let smooth = u * u * (3.0 - 2.0 * u);
    u + (smooth - u) * feather.min(1.0)
}
fn segment_distance2(px: f32, py: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (qx, qy) = (px - a[0], py - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        ((qx * dx + qy * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (x, y) = (qx - dx * t, qy - dy * t);
    x * x + y * y
}

/// Half-open row indices whose exact pixel centres lie in an edge's band.
fn horizontal_span(
    a: [f32; 2],
    b: [f32; 2],
    band: f32,
    origin: i32,
    width: usize,
) -> std::ops::Range<usize> {
    let lo = (a[0].min(b[0]) - band) as f64 - origin as f64 - 0.5;
    let hi = (a[0].max(b[0]) + band) as f64 - origin as f64 - 0.5;
    let start = lo.ceil().clamp(0.0, width as f64) as usize;
    let end = (hi.floor() + 1.0).clamp(0.0, width as f64) as usize;
    start..end.max(start)
}

fn mask_row(mask: &FlatMask, origin_x: i32, py: f32, out: &mut [f32]) -> Result<(), String> {
    let band = mask.band();
    let coverage = |signed: f32| {
        let c = falloff(signed + mask.expansion, mask.feather);
        mask.opacity * if mask.inverted { 1.0 - c } else { c }
    };
    if mask.edges.is_empty() {
        out.fill(coverage(-band));
        return Ok(());
    }
    let mut crossings = Vec::new();
    crossings
        .try_reserve_exact(mask.edges.len())
        .map_err(|e| format!("mask scan crossings allocation: {e}"))?;
    // Use the row as a distance plane until the final winding/falloff pass.
    out.fill(band * band);
    for &(a, b) in &mask.edges {
        if (a[1] <= py) != (b[1] <= py) {
            let t = (py - a[1]) / (b[1] - a[1]);
            crossings.push((
                a[0] + (b[0] - a[0]) * t,
                if b[1] > a[1] { 1i32 } else { -1 },
            ));
        }
        if py >= a[1].min(b[1]) - band && py <= a[1].max(b[1]) + band {
            let span = horizontal_span(a, b, band, origin_x, out.len());
            let start = span.start;
            let distances = out
                .get_mut(span)
                .ok_or("mask horizontal span is outside its row")?;
            for (dx, d2) in distances.iter_mut().enumerate() {
                let px = origin_x as f32 + (start + dx) as f32 + 0.5;
                *d2 = d2.min(segment_distance2(px, py, a, b));
            }
        }
    }
    crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut ci, mut winding) = (0usize, 0i32);
    for (x, result) in out.iter_mut().enumerate() {
        let px = origin_x as f32 + x as f32 + 0.5;
        while let Some(crossing) = crossings.get(ci).filter(|c| c.0 < px) {
            winding += crossing.1;
            ci += 1;
        }
        let inside = match mask.fill_rule {
            VectorFillRule::Nonzero => winding != 0,
            VectorFillRule::EvenOdd => winding % 2 != 0,
        };
        let distance = result.sqrt();
        *result = coverage(if inside { distance } else { -distance });
    }
    Ok(())
}

impl MaskCoverage {
    /// Scale every premultiplied channel, retaining HDR/negative RGB and metadata.
    pub fn apply_cpu(&self, input: &CpuFrame) -> Result<CpuFrame, String> {
        let count = dimensions(self.width, self.height, self.region)?;
        if (input.width, input.height, input.data_window) != (self.width, self.height, self.region)
        {
            return Err("mask coverage and input display/data windows differ".into());
        }
        if input.alpha != AlphaMode::Premultiplied {
            return Err("mask application requires premultiplied alpha".into());
        }
        if self.values.len() != count || input.image.pixels.len() != count * 4 {
            return Err("mask coverage/input storage does not match the data window".into());
        }
        if self
            .values
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        {
            return Err("mask coverage values must be finite and in [0,1]".into());
        }
        for pixel in input.image.pixels.as_chunks::<4>().0 {
            if pixel.iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&pixel[3].to_f32()) {
                return Err("mask input must have finite pixels and alpha in [0,1]".into());
            }
        }
        if !self.active {
            return Ok(input.clone());
        }
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(count * 4)
            .map_err(|e| format!("masked frame allocation: {e}"))?;
        for (source, coverage) in input
            .image
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(&self.values)
        {
            pixels.extend(source.iter().map(|v| f16::from_f32(v.to_f32() * coverage)));
        }
        Ok(CpuFrame {
            image: Arc::new(CpuImage { pixels }),
            ..input.clone()
        })
    }
}
