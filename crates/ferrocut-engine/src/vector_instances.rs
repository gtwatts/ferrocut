//! Editable native vector groups and repeaters.
//!
//! EffectCraft's actual `Mat3` operations compose group and per-copy transforms;
//! the vector paint hook transforms actual EffectCraft paths before coverage.
//! The private upstream renderer/EvalCtx is not imported. Copy enumeration and
//! opacity are bounded Ferrocut adapter code, following its public model.

use std::collections::BTreeMap;
use std::sync::Arc;

use effectcraft_geom::{Mat3, vec2};
use ferrocut_core::{
    Animatable, ColorSpace, CpuFrame, Frame, NodeError, NodeHash, Pull, Rational, RationalTime,
    RenderCtx, RenderNode,
};
use half::f16;
use serde::{Deserialize, Serialize};

use crate::vector::{MAX_VECTOR_PIXELS, VectorGeometry, VectorNode, VectorSpec};

pub const VECTOR_INSTANCES_VERSION: &[u8] = b"vector.instances.v1.effectcraft.6943872";
pub const MAX_GROUP_DEPTH: usize = 8;
pub const MAX_GROUP_NODES: usize = 128;
pub const MAX_VECTOR_INSTANCES: usize = 512;
pub const MAX_INSTANCE_GEOMETRY: usize = 262_144;
pub const MAX_INSTANCE_PIXEL_WORK: usize = 256 * 1024 * 1024;
const MAX_MATRIX_COMPONENT: f64 = 1_000_000.0;

fn zero() -> Animatable {
    Animatable::constant(Rational::ZERO)
}
fn hundred() -> Animatable {
    Animatable::constant(Rational::from_int(100))
}
fn three() -> Animatable {
    Animatable::constant(Rational::from_int(3))
}
fn origin() -> [Animatable; 2] {
    [zero(), zero()]
}
fn full_scale() -> [Animatable; 2] {
    [hundred(), hundred()]
}
fn step_position() -> [Animatable; 2] {
    [hundred(), zero()]
}

/// Local-to-parent shape transform. Scale and opacity are percentages; positive
/// rotation is clockwise in the +y-down image. All values use exact source time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorTransform {
    #[serde(default = "origin")]
    pub anchor: [Animatable; 2],
    #[serde(default = "origin")]
    pub position: [Animatable; 2],
    #[serde(default = "full_scale")]
    pub scale: [Animatable; 2],
    #[serde(default = "zero")]
    pub rotation: Animatable,
    #[serde(default = "zero")]
    pub skew: Animatable,
    #[serde(default = "zero")]
    pub skew_axis: Animatable,
    #[serde(default = "hundred")]
    pub opacity: Animatable,
}

impl Default for VectorTransform {
    fn default() -> Self {
        Self {
            anchor: origin(),
            position: origin(),
            scale: full_scale(),
            rotation: zero(),
            skew: zero(),
            skew_axis: zero(),
            opacity: hundred(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorComposite {
    /// Paint higher copy indices first, keeping the original above later copies.
    #[default]
    Below,
    /// Paint higher copy indices last, placing later copies above the original.
    Above,
}

/// Repeat the complete group's contents. Fractional copies fade the final copy;
/// offset changes transform exponents, independently of the opacity ramp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorRepeat {
    #[serde(default = "three")]
    pub copies: Animatable,
    #[serde(default = "zero")]
    pub offset: Animatable,
    #[serde(default = "origin")]
    pub anchor: [Animatable; 2],
    #[serde(default = "step_position")]
    pub position: [Animatable; 2],
    #[serde(default = "full_scale")]
    pub scale: [Animatable; 2],
    #[serde(default = "zero")]
    pub rotation: Animatable,
    #[serde(default = "hundred")]
    pub start_opacity: Animatable,
    #[serde(default = "hundred")]
    pub end_opacity: Animatable,
    #[serde(default)]
    pub composite: VectorComposite,
}

impl Default for VectorRepeat {
    fn default() -> Self {
        Self {
            copies: three(),
            offset: zero(),
            anchor: origin(),
            position: step_position(),
            scale: full_scale(),
            rotation: zero(),
            start_opacity: hundred(),
            end_opacity: hundred(),
            composite: VectorComposite::Below,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VectorItem {
    Shape { shape: Box<VectorSpec> },
    Group { group: Box<VectorGroup> },
}

/// Items paint bottom to top. Group opacity multiplies each descendant paint;
/// it does not create an isolated intermediate image. A repeater transforms the
/// local contents before the group's local-to-parent transform is applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorGroup {
    pub items: Vec<VectorItem>,
    #[serde(default)]
    pub transform: VectorTransform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<VectorRepeat>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VectorRepeatInstance {
    pub index: u32,
    pub transform: Mat3,
    pub opacity: f32,
}

/// One leaf draw in final bottom-to-top order; no geometry or animation is
/// flattened into the saved document. Matrices are sampled in f64.
#[derive(Clone, Copy, Debug)]
pub struct VectorInstance<'a> {
    pub shape: &'a VectorSpec,
    pub transform: Mat3,
    pub opacity: f32,
}

fn pair<'a>(out: &mut Vec<(String, &'a Animatable)>, path: &str, value: &'a [Animatable; 2]) {
    out.push((format!("{path}.x"), &value[0]));
    out.push((format!("{path}.y"), &value[1]));
}
fn pair_mut<'a>(
    out: &mut Vec<(String, &'a mut Animatable)>,
    path: &str,
    value: &'a mut [Animatable; 2],
) {
    let [x, y] = value;
    out.push((format!("{path}.x"), x));
    out.push((format!("{path}.y"), y));
}

macro_rules! transform_params {
    ($value:expr, $prefix:expr, $out:ident, $pair:ident) => {{
        let value = $value;
        let prefix = $prefix;
        $pair(&mut $out, &format!("{prefix}.anchor"), &value.anchor);
        $pair(&mut $out, &format!("{prefix}.position"), &value.position);
        $pair(&mut $out, &format!("{prefix}.scale"), &value.scale);
        $out.push((format!("{prefix}.rotation"), &value.rotation));
        $out.push((format!("{prefix}.skew"), &value.skew));
        $out.push((format!("{prefix}.skew_axis"), &value.skew_axis));
        $out.push((format!("{prefix}.opacity"), &value.opacity));
    }};
}

// Separate mutable helpers make every component addressable by the same public
// path without relying on JSON-array deserialization or unchecked indices.
impl VectorTransform {
    fn all(&self) -> Vec<(String, &Animatable)> {
        let mut out = Vec::new();
        transform_params!(self, "transform", out, pair);
        out
    }
    fn all_mut(&mut self) -> Vec<(String, &mut Animatable)> {
        let mut out = Vec::new();
        pair_mut(&mut out, "transform.anchor", &mut self.anchor);
        pair_mut(&mut out, "transform.position", &mut self.position);
        pair_mut(&mut out, "transform.scale", &mut self.scale);
        out.push(("transform.rotation".into(), &mut self.rotation));
        out.push(("transform.skew".into(), &mut self.skew));
        out.push(("transform.skew_axis".into(), &mut self.skew_axis));
        out.push(("transform.opacity".into(), &mut self.opacity));
        out
    }
    fn bounds(&self) -> Vec<(&str, &Animatable, f64, f64)> {
        vec![
            ("anchor.x", &self.anchor[0], -1e6, 1e6),
            ("anchor.y", &self.anchor[1], -1e6, 1e6),
            ("position.x", &self.position[0], -1e6, 1e6),
            ("position.y", &self.position[1], -1e6, 1e6),
            ("scale.x", &self.scale[0], -10_000.0, 10_000.0),
            ("scale.y", &self.scale[1], -10_000.0, 10_000.0),
            ("rotation", &self.rotation, -360_000.0, 360_000.0),
            ("skew", &self.skew, -85.0, 85.0),
            ("skew_axis", &self.skew_axis, -360_000.0, 360_000.0),
            ("opacity", &self.opacity, 0.0, 100.0),
        ]
    }
    fn validate(&self) -> Result<(), String> {
        validate_bounds(self.bounds())
    }
    pub fn sample_at(&self, t: RationalTime) -> Result<(Mat3, f32), String> {
        let anchor = sample_pair(&self.anchor, t, -1e6, 1e6, "anchor")?;
        let pos = sample_pair(&self.position, t, -1e6, 1e6, "position")?;
        let scale = sample_pair(&self.scale, t, -10_000.0, 10_000.0, "scale")?;
        let rotation = sample(&self.rotation, t, -360_000.0, 360_000.0, "rotation")?;
        let skew = sample(&self.skew, t, -85.0, 85.0, "skew")?;
        let axis = sample(&self.skew_axis, t, -360_000.0, 360_000.0, "skew_axis")?;
        let opacity = sample(&self.opacity, t, 0.0, 100.0, "opacity")? as f32 / 100.0;
        let matrix = Mat3::translate(vec2(pos[0], pos[1]))
            * Mat3::rotate_deg(rotation)
            * Mat3::skew_deg(-skew, axis)
            * Mat3::scale(vec2(scale[0] / 100.0, scale[1] / 100.0))
            * Mat3::translate(vec2(-anchor[0], -anchor[1]));
        check_matrix(&matrix)?;
        Ok((matrix, opacity))
    }
}

impl VectorRepeat {
    fn all(&self) -> Vec<(String, &Animatable)> {
        let mut out = vec![
            ("repeat.copies".into(), &self.copies),
            ("repeat.offset".into(), &self.offset),
        ];
        pair(&mut out, "repeat.anchor", &self.anchor);
        pair(&mut out, "repeat.position", &self.position);
        pair(&mut out, "repeat.scale", &self.scale);
        out.push(("repeat.rotation".into(), &self.rotation));
        out.push(("repeat.start_opacity".into(), &self.start_opacity));
        out.push(("repeat.end_opacity".into(), &self.end_opacity));
        out
    }
    fn all_mut(&mut self) -> Vec<(String, &mut Animatable)> {
        let mut out = vec![
            ("repeat.copies".into(), &mut self.copies),
            ("repeat.offset".into(), &mut self.offset),
        ];
        pair_mut(&mut out, "repeat.anchor", &mut self.anchor);
        pair_mut(&mut out, "repeat.position", &mut self.position);
        pair_mut(&mut out, "repeat.scale", &mut self.scale);
        out.push(("repeat.rotation".into(), &mut self.rotation));
        out.push(("repeat.start_opacity".into(), &mut self.start_opacity));
        out.push(("repeat.end_opacity".into(), &mut self.end_opacity));
        out
    }
    fn bounds(&self) -> Vec<(&str, &Animatable, f64, f64)> {
        vec![
            ("copies", &self.copies, 0.0, 128.0),
            ("offset", &self.offset, -128.0, 128.0),
            ("anchor.x", &self.anchor[0], -1e6, 1e6),
            ("anchor.y", &self.anchor[1], -1e6, 1e6),
            ("position.x", &self.position[0], -1e6, 1e6),
            ("position.y", &self.position[1], -1e6, 1e6),
            ("scale.x", &self.scale[0], 0.01, 1000.0),
            ("scale.y", &self.scale[1], 0.01, 1000.0),
            ("rotation", &self.rotation, -360_000.0, 360_000.0),
            ("start_opacity", &self.start_opacity, 0.0, 100.0),
            ("end_opacity", &self.end_opacity, 0.0, 100.0),
        ]
    }
    fn validate(&self) -> Result<(), String> {
        validate_bounds(self.bounds())
    }
    pub fn instances_at(&self, t: RationalTime) -> Result<Vec<VectorRepeatInstance>, String> {
        let copies = sample(&self.copies, t, 0.0, 128.0, "copies")?;
        let n = copies.ceil() as usize;
        if n == 0 {
            return Ok(Vec::new());
        }
        let offset = sample(&self.offset, t, -128.0, 128.0, "offset")?;
        let anchor = sample_pair(&self.anchor, t, -1e6, 1e6, "anchor")?;
        let pos = sample_pair(&self.position, t, -1e6, 1e6, "position")?;
        let scale = sample_pair(&self.scale, t, 0.01, 1000.0, "scale")?;
        let rot = sample(&self.rotation, t, -360_000.0, 360_000.0, "rotation")?;
        let start = sample(&self.start_opacity, t, 0.0, 100.0, "start_opacity")? / 100.0;
        let end = sample(&self.end_opacity, t, 0.0, 100.0, "end_opacity")? / 100.0;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let k = i as f64 + offset;
            let matrix = Mat3::translate(vec2(pos[0] * k, pos[1] * k))
                * Mat3::translate(vec2(anchor[0], anchor[1]))
                * Mat3::rotate_deg(rot * k)
                * Mat3::scale(vec2((scale[0] / 100.0).powf(k), (scale[1] / 100.0).powf(k)))
                * Mat3::translate(vec2(-anchor[0], -anchor[1]));
            check_matrix(&matrix)?;
            let progress = if n > 1 {
                i as f64 / (n - 1) as f64
            } else {
                0.0
            };
            let partial = if i + 1 == n && copies.fract() > 0.0 {
                copies.fract()
            } else {
                1.0
            };
            out.push(VectorRepeatInstance {
                index: i as u32,
                transform: matrix,
                opacity: ((start + (end - start) * progress) * partial) as f32,
            });
        }
        if self.composite == VectorComposite::Below {
            out.reverse();
        }
        Ok(out)
    }
}

fn validate_bounds(bounds: Vec<(&str, &Animatable, f64, f64)>) -> Result<(), String> {
    for (name, a, min, max) in bounds {
        a.validate().map_err(|e| format!("{name}: {e}"))?;
        if !a.is_expression() {
            let (lo, hi) = a.key_range();
            if lo.to_f64() < min || hi.to_f64() > max {
                return Err(format!("{name}: keys must be in {min}..={max}"));
            }
        }
    }
    Ok(())
}
fn sample(a: &Animatable, t: RationalTime, min: f64, max: f64, name: &str) -> Result<f64, String> {
    let value = a.eval(t);
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "{name}: evaluated value must be finite and in {min}..={max}"
        ))
    }
}
fn sample_pair(
    a: &[Animatable; 2],
    t: RationalTime,
    min: f64,
    max: f64,
    name: &str,
) -> Result<[f64; 2], String> {
    Ok([
        sample(&a[0], t, min, max, &format!("{name}.x"))?,
        sample(&a[1], t, min, max, &format!("{name}.y"))?,
    ])
}
fn check_matrix(matrix: &Mat3) -> Result<(), String> {
    if matrix
        .0
        .iter()
        .flatten()
        .all(|v| v.is_finite() && v.abs() <= MAX_MATRIX_COMPONENT)
    {
        Ok(())
    } else {
        Err("vector instance transform exceeds finite 1000000 component bound".into())
    }
}
fn geometry_cost(shape: &VectorSpec) -> usize {
    match &shape.geometry {
        VectorGeometry::Path { commands } => commands.len(),
        VectorGeometry::Polygon { points, .. } => {
            (points.key_range().1.to_f64().ceil() as usize).clamp(3, 256) * 2 + 2
        }
        VectorGeometry::Star { points, .. } => {
            (points.key_range().1.to_f64().ceil() as usize).clamp(3, 256) * 4 + 2
        }
        _ => 16,
    }
}

impl VectorGroup {
    pub fn all(&self) -> Vec<(String, &Animatable)> {
        fn walk<'a>(group: &'a VectorGroup, prefix: &str, out: &mut Vec<(String, &'a Animatable)>) {
            let path = |name: String| {
                if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}.{name}")
                }
            };
            out.extend(group.transform.all().into_iter().map(|(p, a)| (path(p), a)));
            if let Some(repeat) = &group.repeat {
                out.extend(repeat.all().into_iter().map(|(p, a)| (path(p), a)));
            }
            for (i, item) in group.items.iter().enumerate() {
                match item {
                    VectorItem::Shape { shape } => out.extend(
                        shape
                            .all()
                            .into_iter()
                            .map(|(p, a)| (path(format!("items.{i}.shape.{p}")), a)),
                    ),
                    VectorItem::Group { group } => {
                        walk(group, &path(format!("items.{i}.group")), out)
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(self, "", &mut out);
        out
    }
    pub fn all_mut(&mut self) -> Vec<(String, &mut Animatable)> {
        fn walk<'a>(
            group: &'a mut VectorGroup,
            prefix: &str,
            out: &mut Vec<(String, &'a mut Animatable)>,
        ) {
            let path = |name: String| {
                if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}.{name}")
                }
            };
            out.extend(
                group
                    .transform
                    .all_mut()
                    .into_iter()
                    .map(|(p, a)| (path(p), a)),
            );
            if let Some(repeat) = &mut group.repeat {
                out.extend(repeat.all_mut().into_iter().map(|(p, a)| (path(p), a)));
            }
            for (i, item) in group.items.iter_mut().enumerate() {
                match item {
                    VectorItem::Shape { shape } => out.extend(
                        shape
                            .all_mut()
                            .into_iter()
                            .map(|(p, a)| (path(format!("items.{i}.shape.{p}")), a)),
                    ),
                    VectorItem::Group { group } => {
                        walk(group, &path(format!("items.{i}.group")), out)
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(self, "", &mut out);
        out
    }
    pub fn animatables(&self) -> Vec<(String, &Animatable)> {
        self.all()
    }
    pub fn animatables_mut(&mut self) -> Vec<(String, &mut Animatable)> {
        self.all_mut()
    }
    pub fn validate(&self) -> Result<(), String> {
        fn walk(
            group: &VectorGroup,
            depth: usize,
            nodes: &mut usize,
            geometry: &mut usize,
        ) -> Result<(), String> {
            if depth > MAX_GROUP_DEPTH {
                return Err(format!("vector groups exceed depth {MAX_GROUP_DEPTH}"));
            }
            *nodes += 1;
            if *nodes > MAX_GROUP_NODES || group.items.len() > MAX_GROUP_NODES {
                return Err(format!(
                    "vector groups exceed {MAX_GROUP_NODES} total nodes"
                ));
            }
            group
                .transform
                .validate()
                .map_err(|e| format!("transform: {e}"))?;
            if let Some(repeat) = &group.repeat {
                repeat.validate().map_err(|e| format!("repeat: {e}"))?;
            }
            for (i, item) in group.items.iter().enumerate() {
                match item {
                    VectorItem::Shape { shape } => {
                        *nodes += 1;
                        if *nodes > MAX_GROUP_NODES {
                            return Err(format!(
                                "vector groups exceed {MAX_GROUP_NODES} total nodes"
                            ));
                        }
                        shape
                            .validate()
                            .map_err(|e| format!("items.{i}.shape: {e}"))?;
                        *geometry += geometry_cost(shape);
                        if *geometry > MAX_INSTANCE_GEOMETRY {
                            return Err(
                                "vector group source geometry exceeds complexity budget".into()
                            );
                        }
                    }
                    VectorItem::Group { group } => walk(group, depth + 1, nodes, geometry)
                        .map_err(|e| format!("items.{i}.group: {e}"))?,
                }
            }
            Ok(())
        }
        walk(self, 1, &mut 0, &mut 0)
    }
    pub fn is_animated(&self) -> bool {
        self.all().iter().any(|(_, a)| a.is_animated())
            || self.items.iter().any(|item| match item {
                VectorItem::Shape { shape } => shape.is_animated(),
                VectorItem::Group { group } => group.is_animated(),
            })
    }
    pub fn hash_into(&self, h: &mut blake3::Hasher) {
        h.update(VECTOR_INSTANCES_VERSION);
        h.update(
            serde_json::to_string(self)
                .expect("vector group serializes")
                .as_bytes(),
        );
    }
    pub fn instances_at(&self, t: RationalTime) -> Result<Vec<VectorInstance<'_>>, String> {
        self.instances_checked(t, &|| Ok(()))
    }
    fn instances_checked(
        &self,
        t: RationalTime,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Vec<VectorInstance<'_>>, String> {
        fn walk<'a>(
            group: &'a VectorGroup,
            t: RationalTime,
            check: &dyn Fn() -> Result<(), String>,
        ) -> Result<Vec<VectorInstance<'a>>, String> {
            check()?;
            let (matrix, opacity) = group.transform.sample_at(t)?;
            if opacity == 0.0 || matrix.determinant() == 0.0 {
                return Ok(Vec::new());
            }
            let mut base = Vec::new();
            for item in &group.items {
                check()?;
                match item {
                    VectorItem::Shape { shape } => base.push(VectorInstance {
                        shape,
                        transform: Mat3::IDENTITY,
                        opacity: 1.0,
                    }),
                    VectorItem::Group { group } => base.extend(walk(group, t, check)?),
                }
                if base.len() > MAX_VECTOR_INSTANCES {
                    return Err(format!(
                        "vector expansion exceeds {MAX_VECTOR_INSTANCES} instances"
                    ));
                }
            }
            let repeats = match &group.repeat {
                Some(repeat) => repeat.instances_at(t)?,
                None => vec![VectorRepeatInstance {
                    index: 0,
                    transform: Mat3::IDENTITY,
                    opacity: 1.0,
                }],
            };
            let expanded = base
                .len()
                .checked_mul(repeats.len())
                .filter(|n| *n <= MAX_VECTOR_INSTANCES)
                .ok_or_else(|| {
                    format!("vector expansion exceeds {MAX_VECTOR_INSTANCES} instances")
                })?;
            let mut out = Vec::with_capacity(expanded);
            for copy in repeats {
                check()?;
                for draw in &base {
                    let transform = matrix * copy.transform * draw.transform;
                    check_matrix(&transform)?;
                    let opacity = opacity * copy.opacity * draw.opacity;
                    if opacity != 0.0 && transform.determinant() != 0.0 {
                        out.push(VectorInstance {
                            shape: draw.shape,
                            transform,
                            opacity,
                        });
                    }
                }
            }
            Ok(out)
        }
        self.validate()?;
        let draws = walk(self, t, check)?;
        let mut cost = 0usize;
        for draw in &draws {
            cost = cost
                .checked_add(geometry_cost(draw.shape))
                .filter(|n| *n <= MAX_INSTANCE_GEOMETRY)
                .ok_or("vector instance geometry exceeds complexity budget")?;
        }
        Ok(draws)
    }
    pub fn rasterize(&self, t: RationalTime, width: u32, height: u32) -> Result<CpuFrame, String> {
        self.rasterize_checked(t, width, height, &|| Ok(()))
    }
    fn rasterize_checked(
        &self,
        t: RationalTime,
        width: u32,
        height: u32,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<CpuFrame, String> {
        check()?;
        let draws = self.instances_checked(t, check)?;
        let count = (width as usize)
            .checked_mul(height as usize)
            .filter(|n| *n > 0 && *n <= MAX_VECTOR_PIXELS)
            .ok_or("vector group output must be nonzero and at most 67108864 pixels")?;
        count
            .checked_mul(draws.len().max(1))
            .filter(|n| *n <= MAX_INSTANCE_PIXEL_WORK)
            .ok_or("vector instance pixel work exceeds 268435456 pixels per frame")?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(count)
            .map_err(|e| format!("vector group allocation: {e}"))?;
        pixels.resize(count, [0.0f32; 4]);
        let mut geometry_budget = MAX_INSTANCE_GEOMETRY;
        for draw in draws {
            check()?;
            draw.shape.paint_transformed_checked(
                t,
                width,
                height,
                &draw.transform,
                draw.opacity,
                &mut pixels,
                &mut geometry_budget,
                check,
            )?;
        }
        check()?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(count * 4)
            .map_err(|e| format!("vector group output allocation: {e}"))?;
        for (i, pixel) in pixels.into_iter().enumerate() {
            if i % 65_536 == 0 {
                check()?;
            }
            output.extend(pixel.map(f16::from_f32));
        }
        Ok(CpuFrame::new(width, height, ColorSpace::acescg(), output))
    }
}

pub struct VectorGroupNode {
    pub group: VectorGroup,
    pub width: u32,
    pub height: u32,
}

impl RenderNode for VectorGroupNode {
    fn kind(&self) -> &'static str {
        "vector_group"
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        let mut h = blake3::Hasher::new();
        self.group.hash_into(&mut h);
        NodeHash::of(
            "vector_group",
            &[
                h.finalize().as_bytes(),
                &self.width.to_le_bytes(),
                &self.height.to_le_bytes(),
            ],
        )
    }
    fn content_hash_at(&self, t: RationalTime) -> NodeHash {
        let Ok(draws) = self.group.instances_at(t) else {
            return NodeHash::of(
                "vector_group.invalid",
                &[&self.content_hash().0, &t.hash_bytes()],
            );
        };
        let mut hashes = BTreeMap::new();
        let mut h = blake3::Hasher::new();
        h.update(VECTOR_INSTANCES_VERSION);
        h.update(&self.width.to_le_bytes());
        h.update(&self.height.to_le_bytes());
        h.update(&(draws.len() as u64).to_le_bytes());
        for draw in draws {
            let address = std::ptr::from_ref(draw.shape) as usize;
            let hash = hashes.entry(address).or_insert_with(|| {
                VectorNode {
                    spec: draw.shape.clone(),
                    width: self.width,
                    height: self.height,
                }
                .content_hash_at(t)
            });
            h.update(&hash.0);
            for value in draw.transform.0.iter().flatten() {
                h.update(&value.to_bits().to_le_bytes());
            }
            h.update(&draw.opacity.to_bits().to_le_bytes());
        }
        NodeHash::of("vector_group.at", &[h.finalize().as_bytes()])
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
            .group
            .rasterize_checked(t, self.width, self.height, &|| {
                ctx.check().map_err(|e| e.to_string())
            });
        ctx.check()?;
        let frame = frame.map_err(NodeError::new)?;
        Ok(Arc::new(Frame::from_cpu(&frame).to_gpu(ctx.gpu)))
    }
}
