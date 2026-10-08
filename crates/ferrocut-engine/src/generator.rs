//! Generator layers: clips that synthesize their picture instead of decoding
//! a file. Native generators are a solid color and linear / radial gradients;
//! every parameter is keyframable.
//!
//! Native text and vector variants use explicit-font shaping and path coverage;
//! they enter the same working-space compositor as the gradient generators.
//!
//! Colors are `[r, g, b]` or `[r, g, b, a]` in [0, 1], display-referred
//! Rec.709 (BT.709 OETF encoded, the same space decoded video arrives in), so
//! a solid of `0.5` matches a video pixel of 50 % code value. Alpha is
//! straight. The kernel converts to the working space exactly like decoded
//! media (inverse OETF, Rec.709 -> ACEScg, premultiply).
//!
//! Gradients: positions are output pixels (pixel `i` spans `[i, i+1)`; the
//! gradient is sampled at pixel centers). Linear: `t` is the projection of
//! the pixel onto `start -> end`, clamped to [0, 1] (default left-center to
//! right-center). Radial: `t = |p - center| / radius`, clamped (default:
//! frame center, half the frame diagonal). `interpolation` is `display`
//! (default, After Effects' Gradient Ramp in a non-linear project: mix the
//! encoded premultiplied colors) or `linear` (mix in linear premultiplied
//! ACEScg, physically even light).
//!
//! Time: generator keyframes are in *source time*, which for a generator clip
//! is clip-local time plus `source_in` (and follows `speed` / `time_remap`).
//! A new generator clip has `source_in` 0, so its keys are clip-local; splits
//! and trims advance `source_in`, which keeps the animation where it was on
//! the timeline without rewriting keys.

use std::sync::Arc;

use ferrocut_core::{
    Animatable, ColorSpace, Frame, NodeError, NodeHash, Pull, Rational, RationalTime, RenderCtx,
    RenderNode,
};
use serde::{Deserialize, Serialize};

use crate::compositor::compositor;

/// Bump when the kernel math changes (part of every generator frame key).
pub const GENERATOR_VERSION: &[u8] = b"generator.v1";

/// A color: 3 (opaque) or 4 animatable components in [0, 1].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Color(pub Vec<Animatable>);

impl Color {
    pub fn rgba(r: i64, g: i64, b: i64) -> Color {
        Color(
            [r, g, b, 1]
                .map(|v| Animatable::constant(Rational::from_int(v)))
                .to_vec(),
        )
    }
    /// Straight RGBA at `t`, each clamped to [0, 1] (bezier keys may overshoot).
    pub fn at(&self, t: RationalTime) -> [f64; 4] {
        let c = |i: usize| self.0.get(i).map_or(1.0, |a| a.eval(t).clamp(0.0, 1.0));
        [c(0), c(1), c(2), c(3)]
    }
    pub(crate) fn validate(&self, what: &str) -> Result<(), String> {
        if !(3..=4).contains(&self.0.len()) {
            return Err(format!(
                "{what}: a color is [r, g, b] or [r, g, b, a], got {} components",
                self.0.len()
            ));
        }
        for (a, n) in self.0.iter().zip(["r", "g", "b", "a"]) {
            a.validate().map_err(|e| format!("{what}.{n}: {e}"))?;
            let (lo, hi) = a.key_range();
            if lo < Rational::ZERO || hi > Rational::ONE {
                return Err(format!("{what}.{n}: values must be in [0, 1]"));
            }
        }
        Ok(())
    }
}

fn black() -> Color {
    Color::rgba(0, 0, 0)
}
fn white() -> Color {
    Color::rgba(1, 1, 1)
}

/// Color space gradients interpolate in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradientSpace {
    #[default]
    Display,
    Linear,
}

impl GradientSpace {
    fn is_default(&self) -> bool {
        *self == GradientSpace::Display
    }
}

/// What a generator clip draws. See the module docs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum GeneratorSpec {
    Text {
        text: Box<crate::text::TextSpec>,
    },
    Shape {
        shape: Box<crate::vector::VectorSpec>,
    },
    VectorGroup {
        group: Box<crate::vector_instances::VectorGroup>,
    },
    Solid {
        color: Color,
    },
    LinearGradient {
        /// Output pixels; default `[0, h/2]`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<[Animatable; 2]>,
        /// Output pixels; default `[w, h/2]`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<[Animatable; 2]>,
        #[serde(default = "black")]
        start_color: Color,
        #[serde(default = "white")]
        end_color: Color,
        #[serde(default, skip_serializing_if = "GradientSpace::is_default")]
        interpolation: GradientSpace,
    },
    RadialGradient {
        /// Output pixels; default the frame center.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        center: Option<[Animatable; 2]>,
        /// Pixels; default half the frame diagonal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<Animatable>,
        #[serde(default = "black")]
        start_color: Color,
        #[serde(default = "white")]
        end_color: Color,
        #[serde(default, skip_serializing_if = "GradientSpace::is_default")]
        interpolation: GradientSpace,
    },
}

/// Evaluated generator parameters (what the kernel gets).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeneratorAt {
    /// 0 solid, 1 linear, 2 radial.
    pub kind: u32,
    pub c0: [f64; 4],
    pub c1: [f64; 4],
    /// Linear: start xy, end xy. Radial: center xy, radius, 0.
    pub geo: [f64; 4],
    pub linear: bool,
}

impl GeneratorAt {
    pub fn hash_bytes(&self) -> Vec<u8> {
        let mut v = vec![self.kind as u8, self.linear as u8];
        for x in self.c0.iter().chain(&self.c1).chain(&self.geo) {
            v.extend_from_slice(&x.to_bits().to_le_bytes());
        }
        v
    }

    /// Gradient position at the center of pixel (x, y), as the kernel
    /// computes it (f32).
    pub fn t_at(&self, x: u32, y: u32) -> f32 {
        let p = [x as f32 + 0.5, y as f32 + 0.5];
        let g = self.geo.map(|v| v as f32);
        let t = match self.kind {
            1 => {
                let ab = [g[2] - g[0], g[3] - g[1]];
                let l2 = ab[0] * ab[0] + ab[1] * ab[1];
                let d = (p[0] - g[0]) * ab[0] + (p[1] - g[1]) * ab[1];
                if l2 > 0.0 { d / l2 } else { 0.0 }
            }
            2 => {
                if g[2] > 0.0 {
                    ((p[0] - g[0]).powi(2) + (p[1] - g[1]).powi(2)).sqrt() / g[2]
                } else {
                    1.0
                }
            }
            _ => 0.0,
        };
        t.clamp(0.0, 1.0)
    }

    /// CPU reference of the kernel: the working-space (linear ACEScg,
    /// premultiplied) pixel at (x, y).
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        use ferrocut_colorspace::named;
        let tf = named::transfer(crate::compositor::SOURCE_SPACE).expect("known space");
        let m = named::matrix(crate::compositor::SOURCE_SPACE, ColorSpace::acescg().name())
            .expect("known spaces");
        let work = |rgb: [f32; 3], a: f32| -> [f32; 4] {
            let l = tf.decode(rgb);
            let r = [0, 1, 2].map(|i| m[i][0] * l[0] + m[i][1] * l[1] + m[i][2] * l[2]);
            [r[0] * a, r[1] * a, r[2] * a, a]
        };
        let c0 = self.c0.map(|v| v as f32);
        let c1 = self.c1.map(|v| v as f32);
        if self.kind == 0 {
            return work([c0[0], c0[1], c0[2]], c0[3]);
        }
        let t = self.t_at(x, y);
        let mix = |a: f32, b: f32| a + (b - a) * t;
        if self.linear {
            let (a, b) = (
                work([c0[0], c0[1], c0[2]], c0[3]),
                work([c1[0], c1[1], c1[2]], c1[3]),
            );
            return [0, 1, 2, 3].map(|i| mix(a[i], b[i]));
        }
        let pa = [c0[0] * c0[3], c0[1] * c0[3], c0[2] * c0[3], c0[3]];
        let pb = [c1[0] * c1[3], c1[1] * c1[3], c1[2] * c1[3], c1[3]];
        let p = [0, 1, 2, 3].map(|i| mix(pa[i], pb[i]));
        if p[3] <= 0.0 {
            return [0.0; 4];
        }
        work([p[0] / p[3], p[1] / p[3], p[2] / p[3]], p[3])
    }
}

fn xy(v: &Option<[Animatable; 2]>, t: RationalTime, d: [f64; 2]) -> [f64; 2] {
    v.as_ref().map_or(d, |[x, y]| [x.eval(t), y.eval(t)])
}

impl GeneratorSpec {
    pub fn type_name(&self) -> &'static str {
        match self {
            GeneratorSpec::Solid { .. } => "solid",
            GeneratorSpec::LinearGradient { .. } => "linear_gradient",
            GeneratorSpec::RadialGradient { .. } => "radial_gradient",
            GeneratorSpec::Text { .. } => "text",
            GeneratorSpec::Shape { .. } => "shape",
            GeneratorSpec::VectorGroup { .. } => "vector_group",
        }
    }

    /// Gradient parameters at source time `t` for a `w` x `h` output.
    /// Text and shape variants use their native nodes, not this gradient kernel.
    ///
    /// # Panics
    /// Panics for text and shape specifications. Use [`node`] to construct the
    /// appropriate render node for any generator variant.
    pub fn at(&self, t: RationalTime, w: u32, h: u32) -> GeneratorAt {
        let (wf, hf) = (w as f64, h as f64);
        match self {
            GeneratorSpec::Text { .. }
            | GeneratorSpec::Shape { .. }
            | GeneratorSpec::VectorGroup { .. } => {
                panic!("text and shapes must be rendered through generator::node")
            }
            GeneratorSpec::Solid { color } => GeneratorAt {
                kind: 0,
                c0: color.at(t),
                c1: [0.0; 4],
                geo: [0.0; 4],
                linear: false,
            },
            GeneratorSpec::LinearGradient {
                start,
                end,
                start_color,
                end_color,
                interpolation,
            } => {
                let a = xy(start, t, [0.0, hf / 2.0]);
                let b = xy(end, t, [wf, hf / 2.0]);
                GeneratorAt {
                    kind: 1,
                    c0: start_color.at(t),
                    c1: end_color.at(t),
                    geo: [a[0], a[1], b[0], b[1]],
                    linear: *interpolation == GradientSpace::Linear,
                }
            }
            GeneratorSpec::RadialGradient {
                center,
                radius,
                start_color,
                end_color,
                interpolation,
            } => {
                let c = xy(center, t, [wf / 2.0, hf / 2.0]);
                let r = radius
                    .as_ref()
                    .map_or_else(|| (wf * wf + hf * hf).sqrt() / 2.0, |r| r.eval(t).max(0.0));
                GeneratorAt {
                    kind: 2,
                    c0: start_color.at(t),
                    c1: end_color.at(t),
                    geo: [c[0], c[1], r, 0.0],
                    linear: *interpolation == GradientSpace::Linear,
                }
            }
        }
    }

    fn all(&self) -> Vec<(&'static str, &Animatable)> {
        let mut v = Vec::new();
        match self {
            GeneratorSpec::Text { .. }
            | GeneratorSpec::Shape { .. }
            | GeneratorSpec::VectorGroup { .. } => {}
            GeneratorSpec::Solid { color } => {
                for (a, n) in color
                    .0
                    .iter()
                    .zip(["color.r", "color.g", "color.b", "color.a"])
                {
                    v.push((n, a));
                }
            }
            GeneratorSpec::LinearGradient {
                start,
                end,
                start_color,
                end_color,
                ..
            } => {
                for (n, p) in [(["start.x", "start.y"], start), (["end.x", "end.y"], end)] {
                    if let Some([x, y]) = p {
                        v.push((n[0], x));
                        v.push((n[1], y));
                    }
                }
                colors(&mut v, start_color, end_color);
            }
            GeneratorSpec::RadialGradient {
                center,
                radius,
                start_color,
                end_color,
                ..
            } => {
                if let Some([x, y]) = center {
                    v.push(("center.x", x));
                    v.push(("center.y", y));
                }
                if let Some(r) = radius {
                    v.push(("radius", r));
                }
                colors(&mut v, start_color, end_color);
            }
        }
        v
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            GeneratorSpec::Text { text } => text.validate()?,
            GeneratorSpec::Shape { shape } => shape.validate()?,
            GeneratorSpec::VectorGroup { group } => group.validate()?,
            GeneratorSpec::Solid { color } => color.validate("generator color")?,
            GeneratorSpec::LinearGradient {
                start_color,
                end_color,
                ..
            }
            | GeneratorSpec::RadialGradient {
                start_color,
                end_color,
                ..
            } => {
                start_color.validate("generator start_color")?;
                end_color.validate("generator end_color")?;
            }
        }
        for (n, a) in self.all() {
            a.validate().map_err(|e| format!("generator {n}: {e}"))?;
        }
        if let GeneratorSpec::RadialGradient {
            radius: Some(r), ..
        } = self
            && r.key_range().0 < Rational::ZERO
        {
            return Err("generator radius must be >= 0".into());
        }
        Ok(())
    }

    pub fn is_animated(&self) -> bool {
        match self {
            Self::Text { text } => text.is_animated(),
            Self::Shape { shape } => shape.is_animated(),
            Self::VectorGroup { group } => group.is_animated(),
            _ => self.all().iter().any(|(_, a)| a.is_animated()),
        }
    }

    pub fn hash_into(&self, h: &mut blake3::Hasher) {
        h.update(GENERATOR_VERSION);
        // The serde form is canonical (strict types, rationals as strings).
        h.update(
            serde_json::to_string(self)
                .expect("generator serializes")
                .as_bytes(),
        );
    }
}

/// Construct the native source node, freezing text font bytes for stable keys.
pub fn node(
    spec: GeneratorSpec,
    width: u32,
    height: u32,
) -> Result<Arc<dyn RenderNode>, NodeError> {
    Ok(match spec {
        GeneratorSpec::Text { text } => Arc::new(crate::text::TextNode::new(*text, width, height)?),
        GeneratorSpec::Shape { shape } => Arc::new(crate::vector::VectorNode {
            spec: *shape,
            width,
            height,
        }),
        GeneratorSpec::VectorGroup { group } => {
            Arc::new(crate::vector_instances::VectorGroupNode {
                group: *group,
                width,
                height,
            })
        }
        spec => Arc::new(GeneratorNode {
            spec,
            width,
            height,
        }),
    })
}

fn colors<'a>(v: &mut Vec<(&'static str, &'a Animatable)>, a: &'a Color, b: &'a Color) {
    for (x, n) in a.0.iter().zip([
        "start_color.r",
        "start_color.g",
        "start_color.b",
        "start_color.a",
    ]) {
        v.push((n, x));
    }
    for (x, n) in
        b.0.iter()
            .zip(["end_color.r", "end_color.g", "end_color.b", "end_color.a"])
    {
        v.push((n, x));
    }
}

/// Renders a generator at source time `t` (pulled by its clip's
/// [`crate::nodes::ClipNode`] like a decoded source).
pub struct GeneratorNode {
    pub spec: GeneratorSpec,
    pub width: u32,
    pub height: u32,
}

impl RenderNode for GeneratorNode {
    fn kind(&self) -> &'static str {
        "generator"
    }
    fn batches_gpu_work(&self) -> bool {
        true
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        let mut h = blake3::Hasher::new();
        self.spec.hash_into(&mut h);
        NodeHash::of(
            "generator",
            &[
                h.finalize().as_bytes(),
                &self.width.to_le_bytes(),
                &self.height.to_le_bytes(),
            ],
        )
    }
    /// The evaluated parameters: a held or constant generator has the same
    /// per-frame hash everywhere.
    fn content_hash_at(&self, t: RationalTime) -> NodeHash {
        let a = self.spec.at(t, self.width, self.height);
        NodeHash::of(
            "generator.at",
            &[
                GENERATOR_VERSION,
                &a.hash_bytes(),
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
        let comp = compositor(ctx)?;
        let a = self.spec.at(t, self.width, self.height);
        Ok(Arc::new(comp.generate(ctx, self.width, self.height, &a)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_defaults_and_validation() {
        let s: GeneratorSpec = serde_json::from_str(r#"{"type": "linear_gradient"}"#).unwrap();
        s.validate().unwrap();
        let a = s.at(RationalTime::ZERO, 64, 32);
        assert_eq!(a.geo, [0.0, 16.0, 64.0, 16.0]);
        assert_eq!((a.c0, a.c1), ([0.0, 0.0, 0.0, 1.0], [1.0; 4]));
        assert_eq!(a.t_at(0, 0), 0.5 / 64.0);
        assert_eq!(a.t_at(63, 31), 63.5 / 64.0);
        let r: GeneratorSpec = serde_json::from_str(
            r#"{"type": "radial_gradient", "radius": "10", "interpolation": "linear"}"#,
        )
        .unwrap();
        let a = r.at(RationalTime::ZERO, 64, 32);
        assert_eq!((a.geo, a.linear), ([32.0, 16.0, 10.0, 0.0], true));
        let solid: GeneratorSpec =
            serde_json::from_str(r#"{"type": "solid", "color": ["1", "1/2", "0"]}"#).unwrap();
        solid.validate().unwrap();
        assert_eq!(solid.at(RationalTime::ZERO, 1, 1).c0, [1.0, 0.5, 0.0, 1.0]);
        for bad in [
            r#"{"type": "solid", "color": ["1", "0"]}"#,
            r#"{"type": "solid", "color": ["2", "0", "0"]}"#,
            r#"{"type": "radial_gradient", "radius": "-1"}"#,
        ] {
            let s: GeneratorSpec = serde_json::from_str(bad).unwrap();
            assert!(s.validate().is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<GeneratorSpec>(r#"{"type": "solid"}"#).is_err());
        assert!(
            serde_json::from_str::<GeneratorSpec>(r#"{"type": "rect", "size": ["1", "1"]}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<GeneratorSpec>(
                r#"{"type": "solid", "color": ["1", "1", "1"], "bogus": 1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn keyframed_colors_and_display_vs_linear() {
        let s: GeneratorSpec = serde_json::from_str(
            r#"{"type": "solid", "color": [{"keyframes": [{"t": "0", "v": "0"}, {"t": "1", "v": "1"}]}, "0", "0"]}"#,
        )
        .unwrap();
        assert!(s.is_animated());
        assert_eq!(s.at(RationalTime::new(1, 2), 1, 1).c0[0], 0.5);
        // Black -> white: the display mix at the middle is a 50 % code value,
        // the linear mix is 50 % light (brighter on screen).
        let g = |space: &str| -> GeneratorSpec {
            serde_json::from_str(&format!(
                r#"{{"type": "linear_gradient", "start": ["0", "0"], "end": ["2", "0"], "interpolation": "{space}"}}"#
            ))
            .unwrap()
        };
        let d = g("display").at(RationalTime::ZERO, 2, 1);
        let l = g("linear").at(RationalTime::ZERO, 2, 1);
        assert_eq!(d.t_at(0, 0), 0.25);
        let (pd, pl) = (d.pixel(0, 0), l.pixel(0, 0));
        assert!(pl[0] > pd[0] && (pl[3] - 1.0).abs() < 1e-6);
        // Linear: a quarter of the way to white is 0.25 light (white = 1 in
        // ACEScg too, the matrix preserves white).
        assert!((pl[1] - 0.25).abs() < 1e-3, "{pl:?}");
    }
}
