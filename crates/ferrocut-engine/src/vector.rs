//! Native vector geometry with animated fills and strokes.
//!
//! Coordinates are output pixels, +x right and +y down. All numeric properties
//! use source-time Animatable values, like other generator parameters. Existing
//! clip transforms provide placement, parenting, camera projection, and opacity.
//! Open contours are implicitly closed for fill and remain open for stroke.
//!
//! Color inputs use the generator convention: display-referred Rec.709 with
//! straight alpha. Paint interpolation is premultiplied in the chosen display or
//! linear working space. tiny-skia supplies antialiased 8-bit geometric coverage;
//! color/gradient evaluation remains floating point through ACEScg half-float
//! output. Gradient offsets are clamped, then stably sorted at each sample;
//! coincident stops form hard edges. Legacy dimensions and stroke/style values
//! are clamped at evaluation to handle easing overshoot. New polystar/operator
//! values have explicit bounds, checked at keys and evaluated samples.
//! A zero-width stroke is
//! invisible (not a device-dependent hairline).

use std::sync::Arc;

use effectcraft_path::{BezPath, PathEl, ops};
use ferrocut_colorspace::{Transfer, named};
use ferrocut_core::{
    Animatable, ColorSpace, CpuFrame, Frame, NodeError, NodeHash, Pull, Rational, RationalTime,
    RenderCtx, RenderNode,
};
use half::f16;
use serde::{Deserialize, Serialize};
use tiny_skia::{
    FillRule, LineCap, LineJoin, Mask, Path, PathBuilder, PathSegment, Point, Rect, Stroke,
    StrokeDash, Transform,
};

use crate::generator::{Color, GradientSpace};

pub const VECTOR_VERSION: &[u8] = b"vector.v2.effectcraft-path.6943872";
/// Bounds temporary CPU raster storage to roughly 1.6 GiB at the upper limit.
pub const MAX_VECTOR_PIXELS: usize = 64 * 1024 * 1024;
/// Operator stacks and expanded paths have separate limits from legacy paths.
pub const MAX_VECTOR_OPERATORS: usize = 32;
pub const MAX_OPERATOR_ELEMENTS: usize = 32_768;
const MAX_BOOLEAN_ELEMENTS: usize = 512;
const MAX_OPERATOR_COORDINATE: f64 = 1_000_000.0;

fn zero() -> Animatable {
    Animatable::constant(Rational::ZERO)
}
fn one() -> Animatable {
    Animatable::constant(Rational::ONE)
}
fn four() -> Animatable {
    Animatable::constant(Rational::from_int(4))
}
fn ten() -> Animatable {
    Animatable::constant(Rational::from_int(10))
}
fn two() -> Animatable {
    Animatable::constant(Rational::from_int(2))
}
fn fifty() -> Animatable {
    Animatable::constant(Rational::from_int(50))
}
fn hundred() -> Animatable {
    Animatable::constant(Rational::from_int(100))
}
fn origin() -> [Animatable; 2] {
    [zero(), zero()]
}
fn yes() -> bool {
    true
}
fn default_fill() -> Option<VectorPaint> {
    Some(VectorPaint::Solid {
        color: Color::rgba(1, 1, 1),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorSpec {
    pub geometry: VectorGeometry,
    /// Applied in order before fill/stroke; omitted is the original renderer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operators: Vec<VectorOperator>,
    #[serde(default = "default_fill")]
    pub fill: Option<VectorPaint>,
    #[serde(default)]
    pub stroke: Option<VectorStroke>,
    #[serde(default)]
    pub fill_rule: VectorFillRule,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VectorGeometry {
    Rectangle {
        #[serde(default = "zero")]
        x: Animatable,
        #[serde(default = "zero")]
        y: Animatable,
        width: Animatable,
        height: Animatable,
        #[serde(default = "zero")]
        radius: Animatable,
    },
    Ellipse {
        center: [Animatable; 2],
        radius: [Animatable; 2],
    },
    Polygon {
        center: [Animatable; 2],
        points: Animatable,
        radius: Animatable,
        #[serde(default = "zero")]
        rotation: Animatable,
        #[serde(default = "zero")]
        roundness: Animatable,
    },
    Star {
        center: [Animatable; 2],
        points: Animatable,
        inner_radius: Animatable,
        outer_radius: Animatable,
        #[serde(default = "zero")]
        rotation: Animatable,
        #[serde(default = "zero")]
        inner_roundness: Animatable,
        #[serde(default = "zero")]
        outer_roundness: Animatable,
    },
    Path {
        commands: Vec<VectorCommand>,
    },
}

/// Native, ordered EffectCraft path operations. All numeric leaves use the
/// generator's source clock, including wiggle's intrinsic time input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VectorOperator {
    Trim {
        #[serde(default = "zero")]
        start: Animatable,
        #[serde(default = "hundred")]
        end: Animatable,
        #[serde(default = "zero")]
        offset: Animatable,
        #[serde(default)]
        mode: VectorTrimMode,
    },
    RoundCorners {
        radius: Animatable,
    },
    Offset {
        amount: Animatable,
        #[serde(default)]
        join: VectorJoin,
        #[serde(default = "four")]
        miter_limit: Animatable,
        #[serde(default = "one")]
        copies: Animatable,
        #[serde(default = "zero")]
        copy_offset: Animatable,
    },
    PuckerBloat {
        amount: Animatable,
    },
    Zigzag {
        size: Animatable,
        #[serde(default = "one")]
        ridges: Animatable,
        #[serde(default)]
        smooth: bool,
    },
    Twist {
        angle: Animatable,
        #[serde(default = "origin")]
        center: [Animatable; 2],
    },
    Wiggle {
        size: Animatable,
        #[serde(default = "ten")]
        detail: Animatable,
        #[serde(default = "yes")]
        smooth: bool,
        #[serde(default = "two")]
        speed: Animatable,
        #[serde(default = "fifty")]
        correlation: Animatable,
        #[serde(default = "zero")]
        phase: Animatable,
        #[serde(default = "zero")]
        seed: Animatable,
    },
    Reverse {},
    Merge {
        #[serde(default)]
        mode: VectorMergeMode,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorTrimMode {
    #[default]
    Simultaneous,
    Individual,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorMergeMode {
    #[default]
    Merge,
    Add,
    Subtract,
    Intersect,
    Exclude,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VectorCommand {
    MoveTo {
        point: [Animatable; 2],
    },
    LineTo {
        point: [Animatable; 2],
    },
    QuadTo {
        control: [Animatable; 2],
        to: [Animatable; 2],
    },
    CubicTo {
        control1: [Animatable; 2],
        control2: [Animatable; 2],
        to: [Animatable; 2],
    },
    Close,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorFillRule {
    #[default]
    Nonzero,
    EvenOdd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VectorPaint {
    Solid {
        color: Color,
    },
    LinearGradient {
        start: [Animatable; 2],
        end: [Animatable; 2],
        stops: Vec<VectorStop>,
        #[serde(default)]
        interpolation: GradientSpace,
    },
    RadialGradient {
        center: [Animatable; 2],
        radius: Animatable,
        stops: Vec<VectorStop>,
        #[serde(default)]
        interpolation: GradientSpace,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorStop {
    pub offset: Animatable,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorStroke {
    pub paint: VectorPaint,
    #[serde(default = "one")]
    pub width: Animatable,
    #[serde(default)]
    pub cap: VectorCap,
    #[serde(default)]
    pub join: VectorJoin,
    #[serde(default = "four")]
    pub miter_limit: Animatable,
    #[serde(default)]
    pub dashes: Vec<Animatable>,
    #[serde(default = "zero")]
    pub dash_offset: Animatable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

fn vec2<'a>(out: &mut Vec<(String, &'a Animatable)>, prefix: &str, p: &'a [Animatable; 2]) {
    out.push((format!("{prefix}.x"), &p[0]));
    out.push((format!("{prefix}.y"), &p[1]));
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
fn color_params_mut<'a>(
    out: &mut Vec<(String, &'a mut Animatable)>,
    prefix: &str,
    c: &'a mut Color,
) {
    for (a, name) in c.0.iter_mut().zip(["r", "g", "b", "a"]) {
        out.push((format!("{prefix}.{name}"), a));
    }
}

// One field list supplies both immutable and mutable visitors. Their identical
// paths are the boundary used by expression baking, editing, and source shifts.
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
                            $vec2(&mut $out, &format!("{base}.point"), point)
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

macro_rules! operator_params {
    ($op:expr, $prefix:expr, $out:ident, $vec2:ident) => {
        match $op {
            VectorOperator::Trim {
                start, end, offset, ..
            } => {
                for (name, a) in [("start", start), ("end", end), ("offset", offset)] {
                    $out.push((format!("{}.{name}", $prefix), a));
                }
            }
            VectorOperator::RoundCorners { radius } => {
                $out.push((format!("{}.radius", $prefix), radius))
            }
            VectorOperator::Offset {
                amount,
                miter_limit,
                copies,
                copy_offset,
                ..
            } => {
                for (name, a) in [
                    ("amount", amount),
                    ("miter_limit", miter_limit),
                    ("copies", copies),
                    ("copy_offset", copy_offset),
                ] {
                    $out.push((format!("{}.{name}", $prefix), a));
                }
            }
            VectorOperator::PuckerBloat { amount } => {
                $out.push((format!("{}.amount", $prefix), amount))
            }
            VectorOperator::Zigzag { size, ridges, .. } => {
                for (name, a) in [("size", size), ("ridges", ridges)] {
                    $out.push((format!("{}.{name}", $prefix), a));
                }
            }
            VectorOperator::Twist { angle, center } => {
                $out.push((format!("{}.angle", $prefix), angle));
                $vec2(&mut $out, &format!("{}.center", $prefix), center);
            }
            VectorOperator::Wiggle {
                size,
                detail,
                speed,
                correlation,
                phase,
                seed,
                ..
            } => {
                for (name, a) in [
                    ("size", size),
                    ("detail", detail),
                    ("speed", speed),
                    ("correlation", correlation),
                    ("phase", phase),
                    ("seed", seed),
                ] {
                    $out.push((format!("{}.{name}", $prefix), a));
                }
            }
            VectorOperator::Reverse {} | VectorOperator::Merge { .. } => {}
        }
    };
}

fn color_params<'a>(out: &mut Vec<(String, &'a Animatable)>, prefix: &str, c: &'a Color) {
    for (a, n) in c.0.iter().zip(["r", "g", "b", "a"]) {
        out.push((format!("{prefix}.{n}"), a));
    }
}

impl VectorPaint {
    fn all_mut<'a>(&'a mut self, prefix: &str, out: &mut Vec<(String, &'a mut Animatable)>) {
        match self {
            Self::Solid { color } => color_params_mut(out, &format!("{prefix}.color"), color),
            Self::LinearGradient {
                start, end, stops, ..
            } => {
                vec2_mut(out, &format!("{prefix}.start"), start);
                vec2_mut(out, &format!("{prefix}.end"), end);
                stop_params_mut(out, prefix, stops);
            }
            Self::RadialGradient {
                center,
                radius,
                stops,
                ..
            } => {
                vec2_mut(out, &format!("{prefix}.center"), center);
                out.push((format!("{prefix}.radius"), radius));
                stop_params_mut(out, prefix, stops);
            }
        }
    }
    fn all<'a>(&'a self, prefix: &str, out: &mut Vec<(String, &'a Animatable)>) {
        match self {
            Self::Solid { color } => color_params(out, &format!("{prefix}.color"), color),
            Self::LinearGradient {
                start, end, stops, ..
            } => {
                vec2(out, &format!("{prefix}.start"), start);
                vec2(out, &format!("{prefix}.end"), end);
                stop_params(out, prefix, stops);
            }
            Self::RadialGradient {
                center,
                radius,
                stops,
                ..
            } => {
                vec2(out, &format!("{prefix}.center"), center);
                out.push((format!("{prefix}.radius"), radius));
                stop_params(out, prefix, stops);
            }
        }
    }

    fn validate(&self, prefix: &str) -> Result<(), String> {
        match self {
            Self::Solid { color } => validate_color(color, &format!("{prefix}.color")),
            Self::LinearGradient { stops, .. } | Self::RadialGradient { stops, .. } => {
                if !(2..=256).contains(&stops.len()) {
                    return Err(format!("{prefix}.stops: a gradient needs 2..=256 stops"));
                }
                for (i, stop) in stops.iter().enumerate() {
                    validate_color(&stop.color, &format!("{prefix}.stops.{i}.color"))?;
                }
                Ok(())
            }
        }
    }
}

fn validate_color(c: &Color, name: &str) -> Result<(), String> {
    if !(3..=4).contains(&c.0.len()) {
        return Err(format!("{name}: expected 3 or 4 color components"));
    }
    Ok(())
}

fn stop_params<'a>(out: &mut Vec<(String, &'a Animatable)>, prefix: &str, stops: &'a [VectorStop]) {
    for (i, stop) in stops.iter().enumerate() {
        out.push((format!("{prefix}.stops.{i}.offset"), &stop.offset));
        color_params(out, &format!("{prefix}.stops.{i}.color"), &stop.color);
    }
}
fn stop_params_mut<'a>(
    out: &mut Vec<(String, &'a mut Animatable)>,
    prefix: &str,
    stops: &'a mut [VectorStop],
) {
    for (i, stop) in stops.iter_mut().enumerate() {
        out.push((format!("{prefix}.stops.{i}.offset"), &mut stop.offset));
        color_params_mut(out, &format!("{prefix}.stops.{i}.color"), &mut stop.color);
    }
}

impl VectorSpec {
    /// Numeric leaves, with paths relative to this shape. Vector/color component
    /// names follow the engine parameter registry's x/y and r/g/b/a convention.
    pub fn all(&self) -> Vec<(String, &Animatable)> {
        let mut out = Vec::new();
        geometry_params!(&self.geometry, out, vec2, iter);
        for (i, op) in self.operators.iter().enumerate() {
            operator_params!(op, format!("operators.{i}"), out, vec2);
        }
        if let Some(fill) = &self.fill {
            fill.all("fill", &mut out);
        }
        if let Some(stroke) = &self.stroke {
            stroke.paint.all("stroke.paint", &mut out);
            out.push(("stroke.width".into(), &stroke.width));
            out.push(("stroke.miter_limit".into(), &stroke.miter_limit));
            out.push(("stroke.dash_offset".into(), &stroke.dash_offset));
            for (i, dash) in stroke.dashes.iter().enumerate() {
                out.push((format!("stroke.dashes.{i}"), dash));
            }
        }
        out
    }

    pub fn all_mut(&mut self) -> Vec<(String, &mut Animatable)> {
        let mut out = Vec::new();
        geometry_params!(&mut self.geometry, out, vec2_mut, iter_mut);
        for (i, op) in self.operators.iter_mut().enumerate() {
            operator_params!(op, format!("operators.{i}"), out, vec2_mut);
        }
        if let Some(fill) = &mut self.fill {
            fill.all_mut("fill", &mut out);
        }
        if let Some(stroke) = &mut self.stroke {
            stroke.paint.all_mut("stroke.paint", &mut out);
            out.push(("stroke.width".into(), &mut stroke.width));
            out.push(("stroke.miter_limit".into(), &mut stroke.miter_limit));
            out.push(("stroke.dash_offset".into(), &mut stroke.dash_offset));
            for (i, dash) in stroke.dashes.iter_mut().enumerate() {
                out.push((format!("stroke.dashes.{i}"), dash));
            }
        }
        out
    }

    pub fn animatables(&self) -> Vec<(String, &Animatable)> {
        self.all()
    }
    pub fn animatables_mut(&mut self) -> Vec<(String, &mut Animatable)> {
        self.all_mut()
    }

    /// Validate structure and animation tracks. Numeric evaluation clamps
    /// color/offset to [0,1] and dimensions/width/dashes to nonnegative values;
    /// this remains safe before expressions have been baked by the engine.
    pub fn validate(&self) -> Result<(), String> {
        if self.operators.len() > MAX_VECTOR_OPERATORS {
            return Err(format!(
                "operators: at most {MAX_VECTOR_OPERATORS} operators are allowed"
            ));
        }
        self.geometry.validate_polystar()?;
        for (i, operator) in self.operators.iter().enumerate() {
            operator
                .validate()
                .map_err(|e| format!("operators.{i}: {e}"))?;
        }
        if let VectorGeometry::Path { commands } = &self.geometry {
            if commands.is_empty() || commands.len() > 100_000 {
                return Err("geometry.commands: expected 1..=100000 path commands".into());
            }
            let mut active = false;
            let mut has_segment = false;
            let mut any_segment = false;
            for (i, c) in commands.iter().enumerate() {
                match c {
                    VectorCommand::MoveTo { .. } => {
                        active = true;
                        has_segment = false;
                    }
                    VectorCommand::Close => {
                        if !active || !has_segment {
                            return Err(format!(
                                "geometry.commands.{i}: close requires an active contour with a segment"
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
                        has_segment = true;
                        any_segment = true;
                    }
                }
            }
            if !any_segment {
                return Err("geometry.commands: path has no drawable segments".into());
            }
        }
        if let Some(fill) = &self.fill {
            fill.validate("fill")?;
        }
        if let Some(stroke) = &self.stroke {
            stroke.paint.validate("stroke.paint")?;
            if !stroke.dashes.is_empty()
                && (stroke.dashes.len() < 2
                    || stroke.dashes.len() % 2 != 0
                    || stroke.dashes.len() > 256)
            {
                return Err(
                    "stroke.dashes: expected an even number of 2..=256 dash/gap lengths".into(),
                );
            }
        }
        for (name, a) in self.all() {
            a.validate().map_err(|e| format!("{name}: {e}"))?;
        }
        Ok(())
    }

    pub fn is_animated(&self) -> bool {
        self.all().iter().any(|(_, a)| a.is_animated())
            || self
                .operators
                .iter()
                .any(VectorOperator::has_intrinsic_animation)
    }

    pub fn hash_into(&self, h: &mut blake3::Hasher) {
        h.update(VECTOR_VERSION);
        h.update(
            serde_json::to_string(self)
                .expect("vector serializes")
                .as_bytes(),
        );
    }

    fn evaluated_bytes(&self, t: RationalTime) -> Vec<u8> {
        // Evaluate numeric leaves without including key times or distant keys.
        // Keep enum names, topology, and discrete styles in the hash.
        fn sample(v: &mut serde_json::Value, t: RationalTime) {
            if let Ok(a) = serde_json::from_value::<Animatable>(v.clone()) {
                *v = serde_json::Value::String(format!("{:016x}", a.eval(t).to_bits()));
                return;
            }
            match v {
                serde_json::Value::Object(o) => {
                    for v in o.values_mut() {
                        sample(v, t);
                    }
                }
                serde_json::Value::Array(a) => {
                    for v in a {
                        sample(v, t);
                    }
                }
                _ => {}
            }
        }
        let mut value = serde_json::to_value(self).expect("vector serializes");
        sample(&mut value, t);
        // Constant wiggle knobs still produce time-varying geometry. Include
        // the actual source clock only when that sample's wiggle is active.
        if self.operators.iter().any(|op| op.wiggles_at(t)) {
            value.as_object_mut().expect("vector object").insert(
                "wiggle_source_time".into(),
                serde_json::Value::String(format!("{:016x}", t.0.to_f64().to_bits())),
            );
        }
        serde_json::to_vec(&value).expect("sampled vector serializes")
    }

    /// Render a full-window, premultiplied linear ACEScg CPU frame. No GPU or
    /// external renderer is required. Geometry outside the frame is clipped.
    pub fn rasterize(&self, t: RationalTime, width: u32, height: u32) -> Result<CpuFrame, String> {
        self.rasterize_checked(t, width, height, &|| Ok(()))
    }

    /// Paint a native shape into a shared premultiplied ACEScg float buffer.
    /// Used only by vector groups; the legacy single-shape renderer below is
    /// unchanged. Curves and local stroke outlines are transformed before
    /// antialiased coverage, and gradient samples are mapped back to local space.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_transformed_checked(
        &self,
        t: RationalTime,
        width: u32,
        height: u32,
        transform: &effectcraft_geom::Mat3,
        opacity: f32,
        pixels: &mut [[f32; 4]],
        geometry_budget: &mut usize,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        self.validate()?;
        let count = (width as usize)
            .checked_mul(height as usize)
            .filter(|n| *n > 0 && *n <= MAX_VECTOR_PIXELS)
            .ok_or("invalid vector group buffer dimensions")?;
        if pixels.len() != count || !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err("invalid vector group buffer or opacity".into());
        }
        if !transform.is_affine()
            || !transform
                .0
                .iter()
                .flatten()
                .all(|v| v.is_finite() && v.abs() <= 1e6)
        {
            return Err("invalid bounded affine vector group transform".into());
        }
        if opacity == 0.0 || transform.determinant() == 0.0 {
            return Ok(());
        }
        let inverse = transform
            .inverse()
            .ok_or("vector group transform cannot be inverted")?;
        if !inverse.0.iter().flatten().all(|v| v.is_finite()) {
            return Err("vector group inverse transform is not finite".into());
        }
        let Some(path) = self.operated_path(t, check)? else {
            return Ok(());
        };
        let mut transformed = |path: &Path| -> Result<Option<Path>, String> {
            let paths = native_paths(path);
            validate_paths(&paths, MAX_OPERATOR_ELEMENTS)?;
            let elements = paths.iter().map(|p| p.elements().len()).sum::<usize>();
            *geometry_budget = geometry_budget
                .checked_sub(elements)
                .ok_or("vector instance transformed geometry exceeds complexity budget")?;
            // Calling the retained EffectCraft implementation transforms actual
            // Bezier control points, preserving curved geometry and winding.
            let paths = effectcraft_path::transform(&paths, transform);
            skia_paths(&paths)
        };
        if let Some(fill) = &self.fill {
            check()?;
            if let Some(path) = transformed(&path)? {
                let mut mask = Mask::new(width, height).ok_or("invalid vector mask dimensions")?;
                let rule = match self.fill_rule {
                    VectorFillRule::Nonzero => FillRule::Winding,
                    VectorFillRule::EvenOdd => FillRule::EvenOdd,
                };
                mask.fill_path(&path, rule, true, Transform::identity());
                shade_transformed(
                    pixels,
                    &mask,
                    &PaintAt::new(fill, t)?,
                    width,
                    &inverse,
                    opacity,
                    check,
                )?;
            }
        }
        if let Some(stroke) = &self.stroke {
            check()?;
            let stroke_width = nonnegative(&stroke.width, t)?;
            if stroke_width > 0.0 {
                let dashes: Vec<f32> = stroke
                    .dashes
                    .iter()
                    .map(|a| nonnegative(a, t))
                    .collect::<Result<_, _>>()?;
                if dashes.is_empty() || dashes.iter().any(|d| *d > 0.0) {
                    let dash = if dashes.is_empty() {
                        None
                    } else {
                        validate_dash_budget(&path, &dashes)?;
                        Some(
                            StrokeDash::new(dashes, scalar(&stroke.dash_offset, t)?)
                                .ok_or("stroke dashes cannot be represented")?,
                        )
                    };
                    let style = Stroke {
                        width: stroke_width,
                        miter_limit: nonnegative(&stroke.miter_limit, t)?.max(1.0),
                        line_cap: match stroke.cap {
                            VectorCap::Butt => LineCap::Butt,
                            VectorCap::Round => LineCap::Round,
                            VectorCap::Square => LineCap::Square,
                        },
                        line_join: match stroke.join {
                            VectorJoin::Miter => LineJoin::Miter,
                            VectorJoin::Round => LineJoin::Round,
                            VectorJoin::Bevel => LineJoin::Bevel,
                        },
                        dash,
                    };
                    let centerline = match &style.dash {
                        Some(dash) => path.dash(dash, 1.0),
                        None => Some(path.clone()),
                    };
                    if let Some(centerline) = centerline {
                        // Outline in local space first: anisotropic scaling must
                        // stretch the full stroke, including caps and joins.
                        let outline = centerline
                            .stroke(&style, 1.0)
                            .ok_or("stroke outline could not be constructed")?;
                        if let Some(outline) = transformed(&outline)? {
                            let mut mask =
                                Mask::new(width, height).ok_or("invalid vector mask dimensions")?;
                            mask.fill_path(
                                &outline,
                                FillRule::Winding,
                                true,
                                Transform::identity(),
                            );
                            shade_transformed(
                                pixels,
                                &mask,
                                &PaintAt::new(&stroke.paint, t)?,
                                width,
                                &inverse,
                                opacity,
                                check,
                            )?;
                        }
                    }
                }
            }
        }
        check()?;
        Ok(())
    }

    fn rasterize_checked(
        &self,
        t: RationalTime,
        width: u32,
        height: u32,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<CpuFrame, String> {
        check()?;
        self.validate()?;
        // Build and validate the shape before allocating the frame buffers.
        let path = self.operated_path(t, check)?;
        let count = (width as usize)
            .checked_mul(height as usize)
            .filter(|n| *n > 0 && *n <= MAX_VECTOR_PIXELS)
            .ok_or("vector output size must be nonzero and at most 67108864 pixels")?;
        let mut pixels = Vec::<[f32; 4]>::new();
        pixels
            .try_reserve_exact(count)
            .map_err(|e| format!("vector frame allocation: {e}"))?;
        pixels.resize(count, [0.0; 4]);
        if let Some(path) = path {
            check()?;
            if let Some(fill) = &self.fill {
                let mut mask = Mask::new(width, height).ok_or("invalid vector mask dimensions")?;
                let rule = match self.fill_rule {
                    VectorFillRule::Nonzero => FillRule::Winding,
                    VectorFillRule::EvenOdd => FillRule::EvenOdd,
                };
                mask.fill_path(&path, rule, true, Transform::identity());
                shade(&mut pixels, &mask, &PaintAt::new(fill, t)?, width);
            }
            if let Some(stroke) = &self.stroke {
                check()?;
                let stroke_width = nonnegative(&stroke.width, t)?;
                if stroke_width > 0.0 {
                    let dashes: Vec<f32> = stroke
                        .dashes
                        .iter()
                        .map(|a| nonnegative(a, t))
                        .collect::<Result<_, _>>()?;
                    // An easing overshoot may temporarily reduce every dash to
                    // zero: this is an invisible stroke, not a solid fallback.
                    if dashes.is_empty() || dashes.iter().any(|d| *d > 0.0) {
                        let dash = if dashes.is_empty() {
                            None
                        } else {
                            validate_dash_budget(&path, &dashes)?;
                            Some(
                                StrokeDash::new(dashes, scalar(&stroke.dash_offset, t)?)
                                    .ok_or("stroke dashes cannot be represented")?,
                            )
                        };
                        let style = Stroke {
                            width: stroke_width,
                            miter_limit: nonnegative(&stroke.miter_limit, t)?.max(1.0),
                            line_cap: match stroke.cap {
                                VectorCap::Butt => LineCap::Butt,
                                VectorCap::Round => LineCap::Round,
                                VectorCap::Square => LineCap::Square,
                            },
                            line_join: match stroke.join {
                                VectorJoin::Miter => LineJoin::Miter,
                                VectorJoin::Round => LineJoin::Round,
                                VectorJoin::Bevel => LineJoin::Bevel,
                            },
                            dash,
                        };
                        // Path::stroke ignores Stroke::dash; apply dashing to the
                        // centerline first, then construct the stroke outline.
                        let centerline = if let Some(dash) = &style.dash {
                            // An empty result also occurs when the entire path
                            // falls in a gap. The resource budget is checked
                            // above so that excessive patterns still fail.
                            path.dash(dash, 1.0)
                        } else {
                            Some(path.clone())
                        };
                        if let Some(centerline) = centerline {
                            let outline = centerline
                                .stroke(&style, 1.0)
                                .ok_or("stroke outline could not be constructed")?;
                            let mut mask =
                                Mask::new(width, height).ok_or("invalid vector mask dimensions")?;
                            mask.fill_path(
                                &outline,
                                FillRule::Winding,
                                true,
                                Transform::identity(),
                            );
                            shade(&mut pixels, &mask, &PaintAt::new(&stroke.paint, t)?, width);
                        }
                    }
                }
            }
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(count * 4)
            .map_err(|e| format!("vector output allocation: {e}"))?;
        for p in pixels {
            output.extend(p.map(f16::from_f32));
        }
        check()?;
        Ok(CpuFrame::new(width, height, ColorSpace::acescg(), output))
    }

    fn operated_path(
        &self,
        t: RationalTime,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<Path>, String> {
        let Some(path) = self.geometry.path(t)? else {
            return Ok(None);
        };
        if self.operators.is_empty() {
            return Ok(Some(path));
        }
        let mut paths = native_paths(&path);
        validate_paths(&paths, MAX_OPERATOR_ELEMENTS)?;
        for (i, operator) in self.operators.iter().enumerate() {
            check()?;
            paths = operator
                .apply(&paths, t)
                .map_err(|e| format!("operators.{i}: {e}"))?;
            validate_paths(&paths, MAX_OPERATOR_ELEMENTS)
                .map_err(|e| format!("operators.{i}: {e}"))?;
        }
        skia_paths(&paths)
    }
}

fn validate_range(a: &Animatable, min: f64, max: f64, name: &str) -> Result<(), String> {
    // The engine validates expressions after baking; an unbaked expression
    // may have an unrelated fallback value (or the implicit zero).
    if a.is_expression() {
        return Ok(());
    }
    let (lo, hi) = a.key_range();
    if lo.to_f64() < min || hi.to_f64() > max {
        Err(format!("{name}: keys must be in {min}..={max}"))
    } else {
        Ok(())
    }
}

fn bounded(a: &Animatable, t: RationalTime, min: f64, max: f64, name: &str) -> Result<f64, String> {
    let value = a.eval(t);
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "{name}: evaluated value must be finite and in {min}..={max}"
        ))
    }
}

impl VectorOperator {
    fn bounds(&self) -> Vec<(&'static str, &Animatable, f64, f64)> {
        match self {
            Self::Trim {
                start, end, offset, ..
            } => vec![
                ("start", start, 0.0, 100.0),
                ("end", end, 0.0, 100.0),
                ("offset", offset, -1e6, 1e6),
            ],
            Self::RoundCorners { radius } => vec![("radius", radius, 0.0, 1e4)],
            Self::Offset {
                amount,
                miter_limit,
                copies,
                copy_offset,
                ..
            } => vec![
                ("amount", amount, -1e4, 1e4),
                ("miter_limit", miter_limit, 1.0, 100.0),
                ("copies", copies, 1.0, 16.0),
                ("copy_offset", copy_offset, -16.0, 16.0),
            ],
            Self::PuckerBloat { amount } => vec![("amount", amount, -100.0, 100.0)],
            Self::Zigzag { size, ridges, .. } => {
                vec![("size", size, -1e4, 1e4), ("ridges", ridges, 0.0, 128.0)]
            }
            Self::Twist { angle, center } => vec![
                ("angle", angle, -36000.0, 36000.0),
                ("center.x", &center[0], -1e6, 1e6),
                ("center.y", &center[1], -1e6, 1e6),
            ],
            Self::Wiggle {
                size,
                detail,
                speed,
                correlation,
                phase,
                seed,
                ..
            } => vec![
                ("size", size, 0.0, 1e4),
                ("detail", detail, 0.0, 128.0),
                ("speed", speed, -1000.0, 1000.0),
                ("correlation", correlation, 0.0, 100.0),
                ("phase", phase, -1e6, 1e6),
                ("seed", seed, -2147483648.0, 2147483647.0),
            ],
            Self::Reverse {} | Self::Merge { .. } => Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        for (name, a, min, max) in self.bounds() {
            validate_range(a, min, max, name)?;
        }
        Ok(())
    }

    fn has_intrinsic_animation(&self) -> bool {
        match self {
            Self::Wiggle { size, speed, .. } => {
                size.as_constant().is_none_or(|v| !v.is_zero())
                    && speed.as_constant().is_none_or(|v| !v.is_zero())
            }
            _ => false,
        }
    }

    fn wiggles_at(&self, t: RationalTime) -> bool {
        matches!(self, Self::Wiggle { size, speed, .. } if size.eval(t) != 0.0 && speed.eval(t) != 0.0)
    }

    fn apply(&self, paths: &[BezPath], t: RationalTime) -> Result<Vec<BezPath>, String> {
        for (name, a, min, max) in self.bounds() {
            bounded(a, t, min, max, name)?;
        }
        let count = paths.iter().map(|p| p.elements().len()).sum::<usize>();
        let growth = match self {
            Self::RoundCorners { .. } | Self::PuckerBloat { .. } | Self::Trim { .. } => 3,
            Self::Twist { angle, .. } if angle.eval(t) != 0.0 => 64,
            Self::Zigzag { ridges, .. } => ridges.eval(t).round() as usize + 3,
            Self::Wiggle { detail, size, .. } if size.eval(t) != 0.0 => {
                detail.eval(t).round() as usize + 3
            }
            Self::Offset { copies, .. } => copies.eval(t).round() as usize * 8,
            _ => 1,
        };
        if count.saturating_mul(growth) > MAX_OPERATOR_ELEMENTS {
            return Err(format!(
                "expanded path complexity exceeds {MAX_OPERATOR_ELEMENTS} elements"
            ));
        }
        // Offset and booleans perform curve intersection work. Bound them
        // separately before entering upstream code, not after allocation.
        if matches!(
            self,
            Self::Offset { .. }
                | Self::Merge {
                    mode: VectorMergeMode::Add
                        | VectorMergeMode::Subtract
                        | VectorMergeMode::Intersect
                        | VectorMergeMode::Exclude
                }
        ) {
            validate_paths(paths, MAX_BOOLEAN_ELEMENTS)?;
            if paths.len() > 64 {
                return Err("boolean/offset operations accept at most 64 contours".into());
            }
        }
        Ok(match self {
            Self::Trim {
                start,
                end,
                offset,
                mode,
            } => match mode {
                VectorTrimMode::Simultaneous => {
                    ops::trim(paths, start.eval(t), end.eval(t), offset.eval(t))
                }
                VectorTrimMode::Individual => {
                    ops::trim_individually(paths, start.eval(t), end.eval(t), offset.eval(t))
                }
            },
            Self::RoundCorners { radius } => ops::round_corners(paths, radius.eval(t)),
            Self::Offset {
                amount,
                join,
                miter_limit,
                copies,
                copy_offset,
            } => ops::offset(
                paths,
                amount.eval(t),
                match join {
                    VectorJoin::Miter => effectcraft_path::Join::Miter,
                    VectorJoin::Round => effectcraft_path::Join::Round,
                    VectorJoin::Bevel => effectcraft_path::Join::Bevel,
                },
                miter_limit.eval(t),
                copies.eval(t),
                copy_offset.eval(t),
            ),
            Self::PuckerBloat { amount } => ops::pucker_bloat(paths, amount.eval(t)),
            Self::Zigzag {
                size,
                ridges,
                smooth,
            } => ops::zigzag(paths, size.eval(t), ridges.eval(t), *smooth),
            Self::Twist { angle, center } => {
                ops::twist(paths, angle.eval(t), [center[0].eval(t), center[1].eval(t)])
            }
            Self::Wiggle {
                size,
                detail,
                smooth,
                speed,
                correlation,
                phase,
                seed,
            } => {
                let clock = t.0.to_f64() * speed.eval(t) + phase.eval(t) / 360.0;
                // The upstream noise lattice uses i64 indices (+/- neighbours).
                if size.eval(t) != 0.0 && (!clock.is_finite() || clock.abs() > 1e12) {
                    return Err(
                        "wiggle source clock exceeds the safe noise range (+/-1e12 periods)".into(),
                    );
                }
                ops::wiggle(
                    paths,
                    &ops::WiggleParams {
                        size: size.eval(t),
                        detail: detail.eval(t),
                        smooth: *smooth,
                        speed: speed.eval(t),
                        correlation: correlation.eval(t),
                        phase_deg: phase.eval(t),
                        seed: seed.eval(t),
                    },
                    t.0.to_f64(),
                )
            }
            Self::Reverse {} => ops::reverse(paths),
            Self::Merge { mode } => {
                // Upstream removes empty inputs before boolean evaluation.
                // Preserve the first operand for subtraction and every
                // operand for intersection when an earlier trim erased one.
                let empty = |p: &BezPath| p.segments().next().is_none();
                if (*mode == VectorMergeMode::Subtract && paths.first().is_none_or(empty))
                    || (*mode == VectorMergeMode::Intersect && paths.iter().any(empty))
                {
                    return Ok(Vec::new());
                }
                vec![ops::merge(
                    paths,
                    match mode {
                        VectorMergeMode::Merge => ops::MergeMode::Merge,
                        VectorMergeMode::Add => ops::MergeMode::Add,
                        VectorMergeMode::Subtract => ops::MergeMode::Subtract,
                        VectorMergeMode::Intersect => ops::MergeMode::Intersect,
                        VectorMergeMode::Exclude => ops::MergeMode::Exclude,
                    },
                )]
            }
        })
    }
}

/// Split only at original contour boundaries. Operators retain their path-list
/// grouping afterwards: merge produces one compound path for later operations.
fn native_paths(path: &Path) -> Vec<BezPath> {
    let mut paths = Vec::new();
    let mut current = BezPath::new();
    let p = |p: Point| effectcraft_path::Point::new(p.x as f64, p.y as f64);
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(q) => {
                if !current.elements().is_empty() {
                    paths.push(std::mem::take(&mut current));
                }
                current.move_to(p(q));
            }
            PathSegment::LineTo(q) => current.line_to(p(q)),
            PathSegment::QuadTo(c, q) => current.quad_to(p(c), p(q)),
            PathSegment::CubicTo(a, b, q) => current.curve_to(p(a), p(b), p(q)),
            PathSegment::Close => current.close_path(),
        }
    }
    if !current.elements().is_empty() {
        paths.push(current);
    }
    paths
}

fn validate_paths(paths: &[BezPath], limit: usize) -> Result<(), String> {
    let count = paths.iter().map(|p| p.elements().len()).sum::<usize>();
    if count > limit {
        return Err(format!(
            "path complexity exceeds the {limit}-element resource limit"
        ));
    }
    for path in paths {
        for el in path.elements() {
            let points = match *el {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => [Some(p), None, None],
                PathEl::QuadTo(a, b) => [Some(a), Some(b), None],
                PathEl::CurveTo(a, b, c) => [Some(a), Some(b), Some(c)],
                PathEl::ClosePath => [None; 3],
            };
            for p in points.into_iter().flatten() {
                if !p.x.is_finite()
                    || !p.y.is_finite()
                    || p.x.abs() > MAX_OPERATOR_COORDINATE
                    || p.y.abs() > MAX_OPERATOR_COORDINATE
                {
                    return Err(
                        "operator path coordinates must be finite and within +/-1000000 pixels"
                            .into(),
                    );
                }
            }
        }
    }
    Ok(())
}

fn skia_paths(paths: &[BezPath]) -> Result<Option<Path>, String> {
    validate_paths(paths, MAX_OPERATOR_ELEMENTS)?;
    if !paths.iter().any(|p| {
        p.elements().iter().any(|el| {
            matches!(
                el,
                PathEl::LineTo(_) | PathEl::QuadTo(_, _) | PathEl::CurveTo(_, _, _)
            )
        })
    }) {
        return Ok(None);
    }
    let mut out = PathBuilder::new();
    for path in paths {
        for el in path.elements() {
            match *el {
                PathEl::MoveTo(p) => out.move_to(p.x as f32, p.y as f32),
                PathEl::LineTo(p) => out.line_to(p.x as f32, p.y as f32),
                PathEl::QuadTo(a, p) => out.quad_to(a.x as f32, a.y as f32, p.x as f32, p.y as f32),
                PathEl::CurveTo(a, b, p) => out.cubic_to(
                    a.x as f32, a.y as f32, b.x as f32, b.y as f32, p.x as f32, p.y as f32,
                ),
                PathEl::ClosePath => out.close(),
            }
        }
    }
    out.finish()
        .map(Some)
        .ok_or("operated vector path cannot be represented".into())
}

fn validate_dash_budget(path: &Path, dashes: &[f32]) -> Result<(), String> {
    // The control polygon bounds Bezier arc length. Using it conservatively
    // prevents tiny-skia's one-million-dash failure from being confused with
    // the legitimate empty-path result of an invisible dash phase.
    let distance = |a: Point, b: Point| {
        ((a.x as f64 - b.x as f64).powi(2) + (a.y as f64 - b.y as f64).powi(2)).sqrt()
    };
    let mut length = 0.0;
    let mut current = Point::from_xy(0.0, 0.0);
    let mut start = current;
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(p) => {
                current = p;
                start = p;
            }
            PathSegment::LineTo(p) => {
                length += distance(current, p);
                current = p;
            }
            PathSegment::QuadTo(c, p) => {
                length += distance(current, c) + distance(c, p);
                current = p;
            }
            PathSegment::CubicTo(a, b, p) => {
                length += distance(current, a) + distance(a, b) + distance(b, p);
                current = p;
            }
            PathSegment::Close => {
                length += distance(current, start);
                current = start;
            }
        }
    }
    let interval = dashes.iter().map(|v| *v as f64).sum::<f64>();
    let estimate = length * (dashes.len() / 2) as f64 / interval;
    if estimate >= 999_999.0 {
        Err("stroke dash complexity exceeds the one-million-segment resource limit".into())
    } else {
        Ok(())
    }
}

fn scalar(a: &Animatable, t: RationalTime) -> Result<f32, String> {
    let v = a.eval(t) as f32;
    if v.is_finite() {
        Ok(v)
    } else {
        Err("vector parameter evaluated outside finite f32 range".into())
    }
}
fn nonnegative(a: &Animatable, t: RationalTime) -> Result<f32, String> {
    Ok(scalar(a, t)?.max(0.0))
}
fn point(p: &[Animatable; 2], t: RationalTime) -> Result<[f32; 2], String> {
    Ok([scalar(&p[0], t)?, scalar(&p[1], t)?])
}

impl VectorGeometry {
    fn polystar_bounds(&self) -> Vec<(&'static str, &Animatable, f64, f64)> {
        match self {
            Self::Polygon {
                center,
                points,
                radius,
                rotation,
                roundness,
            } => vec![
                ("center.x", &center[0], -1e6, 1e6),
                ("center.y", &center[1], -1e6, 1e6),
                ("points", points, 3.0, 256.0),
                ("radius", radius, 0.0, 1e6),
                ("rotation", rotation, -1e6, 1e6),
                ("roundness", roundness, 0.0, 100.0),
            ],
            Self::Star {
                center,
                points,
                inner_radius,
                outer_radius,
                rotation,
                inner_roundness,
                outer_roundness,
            } => vec![
                ("center.x", &center[0], -1e6, 1e6),
                ("center.y", &center[1], -1e6, 1e6),
                ("points", points, 3.0, 256.0),
                ("inner_radius", inner_radius, 0.0, 1e6),
                ("outer_radius", outer_radius, 0.0, 1e6),
                ("rotation", rotation, -1e6, 1e6),
                ("inner_roundness", inner_roundness, 0.0, 100.0),
                ("outer_roundness", outer_roundness, 0.0, 100.0),
            ],
            _ => Vec::new(),
        }
    }

    fn validate_polystar(&self) -> Result<(), String> {
        for (name, a, min, max) in self.polystar_bounds() {
            validate_range(a, min, max, &format!("geometry.{name}"))?;
        }
        Ok(())
    }

    fn path(&self, t: RationalTime) -> Result<Option<Path>, String> {
        for (name, a, min, max) in self.polystar_bounds() {
            bounded(a, t, min, max, &format!("geometry.{name}"))?;
        }
        let mut pb = PathBuilder::new();
        match self {
            Self::Rectangle {
                x,
                y,
                width,
                height,
                radius,
            } => {
                let (x, y, w, h) = (
                    scalar(x, t)?,
                    scalar(y, t)?,
                    nonnegative(width, t)?,
                    nonnegative(height, t)?,
                );
                if w == 0.0 || h == 0.0 {
                    return Ok(None);
                }
                let r = nonnegative(radius, t)?.min(w.min(h) / 2.0);
                if r == 0.0 {
                    pb.push_rect(
                        Rect::from_xywh(x, y, w, h).ok_or("rectangle is not representable")?,
                    );
                } else {
                    let k = 0.552_284_8 * r;
                    let (right, bottom) = (x + w, y + h);
                    pb.move_to(x + r, y);
                    pb.line_to(right - r, y);
                    pb.cubic_to(right - r + k, y, right, y + r - k, right, y + r);
                    pb.line_to(right, bottom - r);
                    pb.cubic_to(
                        right,
                        bottom - r + k,
                        right - r + k,
                        bottom,
                        right - r,
                        bottom,
                    );
                    pb.line_to(x + r, bottom);
                    pb.cubic_to(x + r - k, bottom, x, bottom - r + k, x, bottom - r);
                    pb.line_to(x, y + r);
                    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
                    pb.close();
                }
            }
            Self::Ellipse { center, radius } => {
                let [cx, cy] = point(center, t)?;
                let [rx, ry] = [nonnegative(&radius[0], t)?, nonnegative(&radius[1], t)?];
                if rx == 0.0 || ry == 0.0 {
                    return Ok(None);
                }
                // Four cubic arcs retain subpixel precision for small ovals;
                // the library's conic-to-quadratic oval approximation can
                // noticeably reduce coverage at these sizes.
                let (kx, ky) = (0.552_284_8 * rx, 0.552_284_8 * ry);
                pb.move_to(cx + rx, cy);
                pb.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
                pb.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
                pb.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
                pb.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
                pb.close();
            }
            Self::Polygon {
                center,
                points,
                radius,
                rotation,
                roundness,
            } => {
                if radius.eval(t) == 0.0 {
                    return Ok(None);
                }
                return skia_paths(&[effectcraft_path::polystar(
                    false,
                    points.eval(t),
                    [center[0].eval(t), center[1].eval(t)],
                    rotation.eval(t),
                    0.0,
                    radius.eval(t),
                    0.0,
                    roundness.eval(t),
                )]);
            }
            Self::Star {
                center,
                points,
                inner_radius,
                outer_radius,
                rotation,
                inner_roundness,
                outer_roundness,
            } => {
                if inner_radius.eval(t) == 0.0 && outer_radius.eval(t) == 0.0 {
                    return Ok(None);
                }
                return skia_paths(&[effectcraft_path::polystar(
                    true,
                    points.eval(t),
                    [center[0].eval(t), center[1].eval(t)],
                    rotation.eval(t),
                    inner_radius.eval(t),
                    outer_radius.eval(t),
                    inner_roundness.eval(t),
                    outer_roundness.eval(t),
                )]);
            }
            Self::Path { commands } => {
                for c in commands {
                    match c {
                        VectorCommand::MoveTo { point: p } => {
                            let [x, y] = point(p, t)?;
                            pb.move_to(x, y);
                        }
                        VectorCommand::LineTo { point: p } => {
                            let [x, y] = point(p, t)?;
                            pb.line_to(x, y);
                        }
                        VectorCommand::QuadTo { control, to } => {
                            let [cx, cy] = point(control, t)?;
                            let [x, y] = point(to, t)?;
                            pb.quad_to(cx, cy, x, y);
                        }
                        VectorCommand::CubicTo {
                            control1,
                            control2,
                            to,
                        } => {
                            let [a, b] = point(control1, t)?;
                            let [c, d] = point(control2, t)?;
                            let [x, y] = point(to, t)?;
                            pb.cubic_to(a, b, c, d, x, y);
                        }
                        VectorCommand::Close => pb.close(),
                    }
                }
            }
        }
        pb.finish()
            .map(Some)
            .ok_or("vector path is not representable".into())
    }
}

struct ColorTransform {
    transfer: Transfer,
    matrix: [[f32; 3]; 3],
}

impl ColorTransform {
    fn new() -> Self {
        Self {
            transfer: named::transfer(crate::compositor::SOURCE_SPACE)
                .expect("known source color space"),
            matrix: named::matrix(crate::compositor::SOURCE_SPACE, ColorSpace::acescg().name())
                .expect("known working color space"),
        }
    }
    fn working(&self, c: [f32; 4]) -> [f32; 4] {
        let linear = self.transfer.decode([c[0], c[1], c[2]]);
        let rgb = self
            .matrix
            .map(|row| row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2]);
        [rgb[0] * c[3], rgb[1] * c[3], rgb[2] * c[3], c[3]]
    }
}

enum PaintKind {
    Solid([f32; 4]),
    Linear([f32; 2], [f32; 2]),
    Radial([f32; 2], f32),
}

struct PaintAt {
    kind: PaintKind,
    stops: Vec<(f32, [f32; 4])>,
    linear: bool,
    transform: ColorTransform,
}

impl PaintAt {
    fn new(paint: &VectorPaint, t: RationalTime) -> Result<Self, String> {
        let transform = ColorTransform::new();
        let (kind, source_stops, space) = match paint {
            VectorPaint::Solid { color } => {
                return Ok(Self {
                    kind: PaintKind::Solid(transform.working(color.at(t).map(|x| x as f32))),
                    stops: Vec::new(),
                    linear: true,
                    transform,
                });
            }
            VectorPaint::LinearGradient {
                start,
                end,
                stops,
                interpolation,
            } => (
                PaintKind::Linear(point(start, t)?, point(end, t)?),
                stops,
                *interpolation,
            ),
            VectorPaint::RadialGradient {
                center,
                radius,
                stops,
                interpolation,
            } => (
                PaintKind::Radial(point(center, t)?, nonnegative(radius, t)?),
                stops,
                *interpolation,
            ),
        };
        let linear = space == GradientSpace::Linear;
        let mut stops: Vec<_> = source_stops
            .iter()
            .map(|s| {
                let c = s.color.at(t).map(|v| v as f32);
                let color = if linear {
                    transform.working(c)
                } else {
                    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
                };
                Ok((scalar(&s.offset, t)?.clamp(0.0, 1.0), color))
            })
            .collect::<Result<_, String>>()?;
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(Self {
            kind,
            stops,
            linear,
            transform,
        })
    }

    fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        let p = [x as f32 + 0.5, y as f32 + 0.5];
        self.pixel_at(p)
    }

    fn pixel_at(&self, p: [f32; 2]) -> [f32; 4] {
        let position = match self.kind {
            PaintKind::Solid(c) => return c,
            PaintKind::Linear(a, b) => {
                let d = [b[0] - a[0], b[1] - a[1]];
                let len = d[0] * d[0] + d[1] * d[1];
                if len > 0.0 {
                    ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len
                } else {
                    0.0
                }
            }
            PaintKind::Radial(c, r) => {
                if r > 0.0 {
                    ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt() / r
                } else {
                    1.0
                }
            }
        }
        .clamp(0.0, 1.0);
        let i = self.stops.partition_point(|s| s.0 <= position);
        let c = if i == 0 {
            self.stops[0].1
        } else if i == self.stops.len() {
            self.stops[i - 1].1
        } else {
            let (a, b) = (self.stops[i - 1], self.stops[i]);
            let amount = (position - a.0) / (b.0 - a.0);
            [0, 1, 2, 3].map(|k| a.1[k] + (b.1[k] - a.1[k]) * amount)
        };
        if self.linear {
            c
        } else if c[3] > 0.0 {
            self.transform
                .working([c[0] / c[3], c[1] / c[3], c[2] / c[3], c[3]])
        } else {
            [0.0; 4]
        }
    }
}

fn shade(pixels: &mut [[f32; 4]], mask: &Mask, paint: &PaintAt, width: u32) {
    for (i, (&coverage, dst)) in mask.data().iter().zip(pixels).enumerate() {
        if coverage == 0 {
            continue;
        }
        let c = paint.pixel(i as u32 % width, i as u32 / width);
        let alpha = coverage as f32 / 255.0;
        let src = c.map(|v| v * alpha);
        *dst = [0, 1, 2, 3].map(|k| src[k] + dst[k] * (1.0 - src[3]));
    }
}

#[allow(clippy::too_many_arguments)]
fn shade_transformed(
    pixels: &mut [[f32; 4]],
    mask: &Mask,
    paint: &PaintAt,
    width: u32,
    inverse: &effectcraft_geom::Mat3,
    opacity: f32,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    for (i, (&coverage, dst)) in mask.data().iter().zip(pixels).enumerate() {
        if i % 65_536 == 0 {
            check()?;
        }
        if coverage == 0 {
            continue;
        }
        let local = inverse.apply(effectcraft_geom::vec2(
            (i % width as usize) as f64 + 0.5,
            (i / width as usize) as f64 + 0.5,
        ));
        if !local.x.is_finite()
            || !local.y.is_finite()
            || local.x.abs() > f32::MAX as f64
            || local.y.abs() > f32::MAX as f64
        {
            return Err("vector group paint coordinates are not representable".into());
        }
        let c = paint.pixel_at([local.x as f32, local.y as f32]);
        let alpha = coverage as f32 / 255.0 * opacity;
        let src = c.map(|v| v * alpha);
        *dst = [0, 1, 2, 3].map(|k| src[k] + dst[k] * (1.0 - src[3]));
    }
    Ok(())
}

/// Native vector source node; clip source-time mapping is applied by ClipNode.
pub struct VectorNode {
    pub spec: VectorSpec,
    pub width: u32,
    pub height: u32,
}

impl RenderNode for VectorNode {
    fn kind(&self) -> &'static str {
        "vector"
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        let mut h = blake3::Hasher::new();
        self.spec.hash_into(&mut h);
        NodeHash::of(
            "vector",
            &[
                h.finalize().as_bytes(),
                &self.width.to_le_bytes(),
                &self.height.to_le_bytes(),
            ],
        )
    }
    fn content_hash_at(&self, t: RationalTime) -> NodeHash {
        NodeHash::of(
            "vector.at",
            &[
                VECTOR_VERSION,
                &self.spec.evaluated_bytes(t),
                &self.width.to_le_bytes(),
                &self.height.to_le_bytes(),
            ],
        )
    }
    fn pulls(&self, _t: RationalTime) -> Vec<Pull> {
        Vec::new()
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        _inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        ctx.check()?;
        let frame = self
            .spec
            .rasterize_checked(t, self.width, self.height, &|| {
                ctx.check().map_err(|e| e.to_string())
            });
        // Retain cancellation/deadline classification across the CPU boundary.
        ctx.check()?;
        let frame = frame.map_err(NodeError::new)?;
        Ok(Arc::new(Frame::from_cpu(&frame).to_gpu(ctx.gpu)))
    }
}
