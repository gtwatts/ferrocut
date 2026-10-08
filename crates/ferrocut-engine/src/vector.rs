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
//! coincident stops form hard edges. Nonnegative geometry/style values are
//! clamped at evaluation to handle easing overshoot. A zero-width stroke is
//! invisible (not a device-dependent hairline).

use std::sync::Arc;

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

pub const VECTOR_VERSION: &[u8] = b"vector.v1";
/// Bounds temporary CPU raster storage to roughly 1.6 GiB at the upper limit.
pub const MAX_VECTOR_PIXELS: usize = 64 * 1024 * 1024;

fn zero() -> Animatable {
    Animatable::constant(Rational::ZERO)
}
fn one() -> Animatable {
    Animatable::constant(Rational::ONE)
}
fn four() -> Animatable {
    Animatable::constant(Rational::from_int(4))
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
    Path {
        commands: Vec<VectorCommand>,
    },
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

fn color_params<'a>(out: &mut Vec<(String, &'a Animatable)>, prefix: &str, c: &'a Color) {
    for (a, n) in c.0.iter().zip(["r", "g", "b", "a"]) {
        out.push((format!("{prefix}.{n}"), a));
    }
}

impl VectorPaint {
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

impl VectorSpec {
    /// Numeric leaves, with paths relative to this shape. Vector/color component
    /// names follow the engine parameter registry's x/y and r/g/b/a convention.
    pub fn all(&self) -> Vec<(String, &Animatable)> {
        let mut out = Vec::new();
        match &self.geometry {
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
                    out.push((format!("geometry.{name}"), a));
                }
            }
            VectorGeometry::Ellipse { center, radius } => {
                vec2(&mut out, "geometry.center", center);
                vec2(&mut out, "geometry.radius", radius);
            }
            VectorGeometry::Path { commands } => {
                for (i, c) in commands.iter().enumerate() {
                    let base = format!("geometry.commands.{i}");
                    match c {
                        VectorCommand::MoveTo { point } | VectorCommand::LineTo { point } => {
                            vec2(&mut out, &format!("{base}.point"), point);
                        }
                        VectorCommand::QuadTo { control, to } => {
                            vec2(&mut out, &format!("{base}.control"), control);
                            vec2(&mut out, &format!("{base}.to"), to);
                        }
                        VectorCommand::CubicTo {
                            control1,
                            control2,
                            to,
                        } => {
                            vec2(&mut out, &format!("{base}.control1"), control1);
                            vec2(&mut out, &format!("{base}.control2"), control2);
                            vec2(&mut out, &format!("{base}.to"), to);
                        }
                        VectorCommand::Close => {}
                    }
                }
            }
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

    /// Validate structure and animation tracks. Numeric evaluation clamps
    /// color/offset to [0,1] and dimensions/width/dashes to nonnegative values;
    /// this remains safe before expressions have been baked by the engine.
    pub fn validate(&self) -> Result<(), String> {
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
        serde_json::to_vec(&value).expect("sampled vector serializes")
    }

    /// Render a full-window, premultiplied linear ACEScg CPU frame. No GPU or
    /// external renderer is required. Geometry outside the frame is clipped.
    pub fn rasterize(&self, t: RationalTime, width: u32, height: u32) -> Result<CpuFrame, String> {
        self.validate()?;
        let count = (width as usize)
            .checked_mul(height as usize)
            .filter(|n| *n > 0 && *n <= MAX_VECTOR_PIXELS)
            .ok_or("vector output size must be nonzero and at most 67108864 pixels")?;
        let mut pixels = Vec::<[f32; 4]>::new();
        pixels
            .try_reserve_exact(count)
            .map_err(|e| format!("vector frame allocation: {e}"))?;
        pixels.resize(count, [0.0; 4]);
        if let Some(path) = self.geometry.path(t)? {
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
        Ok(CpuFrame::new(width, height, ColorSpace::acescg(), output))
    }
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
    fn path(&self, t: RationalTime) -> Result<Option<Path>, String> {
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
        let frame = self
            .spec
            .rasterize(t, self.width, self.height)
            .map_err(NodeError::new)?;
        Ok(Arc::new(Frame::from_cpu(&frame).to_gpu(ctx.gpu)))
    }
}
