//! The video effect contract: one ordered, per-clip / per-track effect stack
//! shared by the engine's native GPU effects (blur, glow, crop, ...) and by
//! effects implemented in other crates (ferrocut-color's grading nodes,
//! ferrocut-ofx's plugin host) without either side depending on the other.
//!
//! # Implementing an effect
//!
//! ```ignore
//! use std::sync::Arc;
//! use ferrocut_core::effect::{self, EffectParams, EffectRequest, VideoEffect, WorkingSpace};
//! use ferrocut_core::param::{ParamSpec, TimeBase};
//! use ferrocut_core::{Frame, NodeError, PixelRect, RenderCtx};
//!
//! struct Exposure;
//! const PARAMS: &[ParamSpec] = &[
//!     ParamSpec::scalar("stops", TimeBase::ClipLocal, "stops", "0", "exposure change").range(-10.0, 10.0),
//! ];
//! impl VideoEffect for Exposure {
//!     fn type_name(&self) -> &str { "exposure" }
//!     fn doc(&self) -> &str { "exposure in stops" }
//!     fn params(&self) -> &[ParamSpec] { PARAMS }
//!     fn working_space(&self, _p: &EffectParams) -> WorkingSpace { WorkingSpace::ACESCG }
//!     fn is_identity(&self, p: &EffectParams) -> bool { p.scalar("stops") == 0.0 }
//!     fn render(&self, ctx: &mut RenderCtx<'_>, input: &Frame, p: &EffectParams,
//!               req: &EffectRequest) -> Result<Frame, NodeError> { todo!() }
//! }
//! effect::register(Arc::new(Exposure)).unwrap();
//! ```
//!
//! The effect then appears in the engine's timeline JSON as
//! `{"type": "exposure", "stops": 1}` in a clip's or track's `effects` list,
//! with the generic edit ops (`add_video_effect`, `set_video_effect_param`,
//! ...), validation, keyframes, expressions and MCP docs coming for free from
//! [`VideoEffect::params`]. Register before the timeline is validated or
//! compiled (the engine calls [`register`] for its built-ins on first use;
//! other crates expose an `ensure_registered()` the engine calls the same way).
//!
//! # What the stack does around an effect
//!
//! - **Parameters** are sampled at the frame's time from the timeline's
//!   [`Animatable`](crate::Animatable) values (constants, keyframes, or baked
//!   expressions) and handed over as [`EffectParams`]; missing values get the
//!   [`ParamSpec::default`] when it is plain JSON.
//! - **Regions of interest.** Data windows flow downstream:
//!   [`VideoEffect::output_window`] maps the input frame's data window to the
//!   effect's (a blur grows it, a crop shrinks it). Requests flow upstream: the
//!   stack asks each effect, last to first, for the input pixels its requested
//!   output needs ([`VideoEffect::input_region`]), so an effect is only asked
//!   for what later effects read. [`EffectRequest::region`] is the data window
//!   the effect must return; its input covers `input_region(region)` clipped to
//!   the input's data window (pixels outside a data window are transparent).
//! - **Working color space.** Each effect declares the space it wants its
//!   input in ([`VideoEffect::working_space`]); frames travel in ACEScg linear
//!   and the stack converts with ferrocut-colorspace only where the space
//!   changes (two adjacent effects in the same space get no conversion between
//!   them; a change from A to B is one direct conversion), and back to ACEScg
//!   after the last effect. Display-referred conversions unpremultiply around
//!   the transfer function. The effect returns its output in the same space
//!   (tag it via [`Frame::color_space`]; the stack trusts the declaration).
//! - **Cache keys** include the type name, [`VideoEffect::version`], the
//!   working space, the enabled flag and every sampled parameter value, so a
//!   change to any of them re-renders; bump `version` when the output for the
//!   same parameters changes.
//! - **Errors.** Return [`NodeError::retryable`] for transient failures (an
//!   OFX plugin process crashed, a device lost): the chunk goes through the
//!   scheduler's normal retry path. [`NodeError::permanent`] fails the chunk;
//!   the stack prefixes the message with the effect's id or index and type
//!   (`effect "grade1" (#0 ofx:com.foo.Grain): ...`) and keeps the kind.
//!   Poll [`RenderCtx::check`] in long renders.
//! - Disabled effects, and effects whose [`VideoEffect::is_identity`] is true
//!   for the sampled parameters, are skipped and left out of cache keys.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

use ferrocut_types::param::ParamSpec;
use ferrocut_types::{NodeError, PixelRect, RationalTime};

use crate::frame::Frame;
use crate::node::RenderCtx;

/// The color space an effect wants its input (and returns its output) in, by
/// its ferrocut-colorspace / OCIO name. The stack converts frames from and
/// back to ACEScg; unknown names fail validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkingSpace(pub &'static str);

impl WorkingSpace {
    /// ACEScg scene-linear: the frame working space (no conversion).
    pub const ACESCG: WorkingSpace = WorkingSpace("ACEScg");
    /// Scene-linear Rec.709 primaries.
    pub const LINEAR_REC709: WorkingSpace = WorkingSpace("Linear Rec.709 (sRGB)");
    /// Display-referred sRGB-encoded Rec.709 (what most 8-bit OFX plugins expect).
    pub const SRGB_REC709: WorkingSpace = WorkingSpace("sRGB Encoded Rec.709 (sRGB)");
    /// Display-referred gamma 2.4 Rec.709 (BT.1886 video).
    pub const GAMMA24_REC709: WorkingSpace = WorkingSpace("Gamma 2.4 Encoded Rec.709");
    /// Rec.709 camera (BT.709 OETF) encoding.
    pub const CAMERA_REC709: WorkingSpace = WorkingSpace("Camera Rec.709");

    pub fn name(&self) -> &'static str {
        self.0
    }
}

/// One parameter value sampled at a frame time.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    /// A [`Scalar`](crate::param::ParamKind::Scalar) / [`Time`](crate::param::ParamKind::Time) value.
    Scalar(f64),
    /// A [`Vec2`](crate::param::ParamKind::Vec2), [`Vec3`](crate::param::ParamKind::Vec3),
    /// [`Color`](crate::param::ParamKind::Color) (`[r, g, b, a]`, alpha
    /// defaulting to 1) or a [`ScalarOrVec2`](crate::param::ParamKind::ScalarOrVec2)
    /// (always two components).
    Vec(Vec<f64>),
    Bool(bool),
    Choice(String),
}

/// An effect's parameters sampled at one frame time, by [`ParamSpec::name`].
/// A parameter is absent only when the timeline doesn't set it and its
/// default isn't plain JSON (e.g. `"[w/2, h/2]"`, computed by the effect).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectParams {
    values: BTreeMap<String, ParamValue>,
}

impl EffectParams {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, name: impl Into<String>, v: ParamValue) {
        self.values.insert(name.into(), v);
    }
    pub fn get(&self, name: &str) -> Option<&ParamValue> {
        self.values.get(name)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ParamValue)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v))
    }
    /// A scalar (the first component of a vector, 1/0 for a bool); 0 when absent.
    pub fn scalar(&self, name: &str) -> f64 {
        self.scalar_opt(name).unwrap_or(0.0)
    }
    pub fn scalar_opt(&self, name: &str) -> Option<f64> {
        match self.values.get(name)? {
            ParamValue::Scalar(v) => Some(*v),
            ParamValue::Vec(v) => v.first().copied(),
            ParamValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            ParamValue::Choice(_) => None,
        }
    }
    /// A vector of `n` components (a scalar is splatted; missing components
    /// are 0, or 1 for a color's alpha); `None` when absent.
    pub fn vec_opt(&self, name: &str, n: usize) -> Option<Vec<f64>> {
        let mut v = match self.values.get(name)? {
            ParamValue::Scalar(s) => vec![*s; n],
            ParamValue::Vec(v) => v.clone(),
            _ => return None,
        };
        while v.len() < n {
            v.push(if v.len() == 3 { 1.0 } else { 0.0 });
        }
        v.truncate(n);
        Some(v)
    }
    pub fn vec2(&self, name: &str) -> Option<[f64; 2]> {
        self.vec_opt(name, 2).map(|v| [v[0], v[1]])
    }
    /// `[r, g, b, a]`; transparent black when absent.
    pub fn color(&self, name: &str) -> [f64; 4] {
        self.vec_opt(name, 4)
            .map(|v| [v[0], v[1], v[2], v[3]])
            .unwrap_or([0.0; 4])
    }
    pub fn bool(&self, name: &str) -> bool {
        matches!(self.values.get(name), Some(ParamValue::Bool(true)))
    }
    pub fn choice(&self, name: &str) -> Option<&str> {
        match self.values.get(name)? {
            ParamValue::Choice(s) => Some(s),
            _ => None,
        }
    }
    /// Canonical bytes of every value (part of the frame cache key).
    pub fn hash_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        for (k, v) in &self.values {
            b.extend_from_slice(&(k.len() as u32).to_le_bytes());
            b.extend_from_slice(k.as_bytes());
            match v {
                ParamValue::Scalar(x) => {
                    b.push(0);
                    b.extend_from_slice(&x.to_bits().to_le_bytes());
                }
                ParamValue::Vec(xs) => {
                    b.push(1);
                    b.extend_from_slice(&(xs.len() as u32).to_le_bytes());
                    for x in xs {
                        b.extend_from_slice(&x.to_bits().to_le_bytes());
                    }
                }
                ParamValue::Bool(x) => b.extend_from_slice(&[2, *x as u8]),
                ParamValue::Choice(s) => {
                    b.push(3);
                    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    b.extend_from_slice(s.as_bytes());
                }
            }
        }
        b
    }
}

/// The display window effects render into.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    /// Pixel aspect ratio (width / height of one pixel).
    pub pixel_aspect: f64,
}

impl Canvas {
    pub fn display_window(&self) -> PixelRect {
        PixelRect::full(self.width, self.height)
    }
}

/// What the stack asks of one effect for one frame.
#[derive(Clone, Debug)]
pub struct EffectRequest {
    /// Timeline time of the frame being rendered.
    pub time: RationalTime,
    /// The time the parameters were sampled at (clip-local seconds for a clip
    /// effect, timeline seconds for a track / adjustment effect).
    pub param_time: RationalTime,
    /// The data window to return (display-window coordinates).
    pub region: PixelRect,
    /// The display window.
    pub canvas: Canvas,
    /// The effect's id in the timeline, or `#<index>` when it has none (for
    /// error messages and logs).
    pub label: String,
}

/// A video effect: see the [module docs](self).
pub trait VideoEffect: Send + Sync + 'static {
    /// Unique type name used in the timeline's `{"type": ...}`, e.g.
    /// `gaussian_blur`, `color:grade`, `ofx:com.vendor.Plugin`.
    fn type_name(&self) -> &str;
    /// One-line description for the MCP params list and guide.
    fn doc(&self) -> &str;
    /// The settable parameters (names are the timeline JSON keys; `type`,
    /// `id` and `enabled` are reserved). Animatable scalars/vectors/colors take
    /// constants, keyframes or expressions.
    fn params(&self) -> &[ParamSpec];
    /// Bump when the output for the same parameters changes (cache keys).
    fn version(&self) -> &str {
        "1"
    }
    /// The space the input is converted to before [`render`](Self::render)
    /// (default ACEScg linear: no conversion).
    fn working_space(&self, _params: &EffectParams) -> WorkingSpace {
        WorkingSpace::ACESCG
    }
    /// Checks beyond the [`ParamSpec`] ranges, on sampled values (e.g. "min
    /// must be below max"). Run at validation for constant parameters and at
    /// render for animated ones.
    fn validate(&self, _params: &EffectParams) -> Result<(), String> {
        Ok(())
    }
    /// True when the effect returns its input unchanged for these parameters
    /// (skipped and left out of cache keys).
    fn is_identity(&self, _params: &EffectParams) -> bool {
        false
    }
    /// The output data window for an input data window (default: unchanged).
    /// The stack clips it to a bounded overscan around the display window.
    fn output_window(
        &self,
        input: PixelRect,
        _params: &EffectParams,
        _canvas: Canvas,
    ) -> PixelRect {
        input
    }
    /// The input region needed to produce `output` (default: the same
    /// region). Return a larger region for kernels that read neighbours
    /// (blur radius, distortion bounds); the stack clips it to the input's
    /// data window.
    fn input_region(
        &self,
        output: PixelRect,
        _params: &EffectParams,
        _time: RationalTime,
    ) -> PixelRect {
        output
    }
    /// True when [`render`](Self::render) only records GPU work and can be
    /// batched with other nodes' work in one submission (false for effects
    /// that read back or wait on another process).
    fn batches_gpu_work(&self) -> bool {
        true
    }
    /// Render: `input` is in [`working_space`](Self::working_space) and covers
    /// at least `input_region(req.region)` ∩ its data window; return a frame
    /// whose data window is `req.region`, premultiplied, same space.
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        params: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError>;
}

fn registry() -> &'static RwLock<BTreeMap<String, Arc<dyn VideoEffect>>> {
    static R: OnceLock<RwLock<BTreeMap<String, Arc<dyn VideoEffect>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Register an effect type. Fails if the type name is taken by another
/// instance, or a parameter name is reserved or repeated; registering the
/// same `Arc` again is a no-op.
pub fn register(effect: Arc<dyn VideoEffect>) -> Result<(), String> {
    let name = effect.type_name().to_string();
    if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err(format!("invalid video effect type name {name:?}"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in effect.params() {
        if ["type", "id", "enabled"].contains(&p.name) {
            return Err(format!(
                "video effect {name}: parameter name {:?} is reserved",
                p.name
            ));
        }
        if !seen.insert(p.name) {
            return Err(format!(
                "video effect {name}: parameter {:?} is listed twice",
                p.name
            ));
        }
    }
    let mut r = registry().write().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = r.get(&name) {
        if Arc::ptr_eq(old, &effect) {
            return Ok(());
        }
        return Err(format!("video effect type {name:?} is already registered"));
    }
    r.insert(name, effect);
    Ok(())
}

/// The registered effect called `type_name`.
pub fn lookup(type_name: &str) -> Option<Arc<dyn VideoEffect>> {
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(type_name)
        .cloned()
}

/// Every registered effect, sorted by type name.
pub fn registered() -> Vec<Arc<dyn VideoEffect>> {
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect()
}
