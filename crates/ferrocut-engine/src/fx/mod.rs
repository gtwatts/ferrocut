//! Video effect stacks: ordered, per-clip / per-track / adjustment-layer
//! lists of [`VideoEffect`]s (the contract in [`ferrocut_core::effect`]),
//! with enable flags and keyframable (or expression-driven) parameters.
//!
//! Timeline JSON: `"effects": [{"type": "gaussian_blur", "id"?: "soft",
//! "enabled"?: true, "sigma": 4, ...}]` on a clip (parameter keys in
//! clip-local time), a video track (timeline time) or an adjustment clip.
//! The native effects live in [`native`]; other crates register theirs with
//! [`ferrocut_core::effect::register`].
//!
//! Graph placement: a clip's stack runs on the clip's own picture (after
//! retiming, before its transform; the clip opacity moves after the stack),
//! a track's stack on the track's sequence (before its matte), and an
//! adjustment clip's stack on the composite of everything below
//! ([`AdjustNode`]). Evaluation, regions of interest, working-space
//! conversions, cache keys and error wrapping are in [`EffectStack`].

pub mod kernels;
pub mod native;

use std::collections::BTreeMap;
use std::sync::Arc;

use ferrocut_core::effect::{
    self, Canvas, EffectParams, EffectRequest, ParamValue, VideoEffect, WorkingSpace,
};
use ferrocut_core::param::{ParamKind, ParamSpec};
use ferrocut_core::{
    Animatable, ColorSpace, Frame, NodeError, NodeHash, PixelRect, Pull, RationalTime, RenderCtx,
    RenderNode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blend::MatteMode;
use crate::compositor::compositor;
use crate::nodes::ClipRange;

/// One entry of an `effects` list.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VideoEffectSpec {
    #[serde(rename = "type")]
    pub kind: String,
    /// Optional name, unique within the stack (edit ops and error messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Parameters by name (see the type's [`VideoEffect::params`]).
    #[serde(flatten)]
    pub params: BTreeMap<String, Value>,
}

fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}

impl VideoEffectSpec {
    /// The same effect with every key time of its animated parameters
    /// shifted by `dt` (clip-local keys after a split / trim).
    pub fn shifted(&self, dt: ferrocut_core::Rational) -> VideoEffectSpec {
        fn shift(v: &Value, dt: ferrocut_core::Rational) -> Value {
            match v {
                Value::Array(a) => Value::Array(a.iter().map(|x| shift(x, dt)).collect()),
                Value::Object(o) if o.contains_key("keyframes") => {
                    match serde_json::from_value::<Animatable>(v.clone()) {
                        Ok(a) => serde_json::to_value(a.shifted(dt)).unwrap_or_else(|_| v.clone()),
                        Err(_) => v.clone(),
                    }
                }
                _ => v.clone(),
            }
        }
        VideoEffectSpec {
            params: self
                .params
                .iter()
                .map(|(k, v)| (k.clone(), shift(v, dt)))
                .collect(),
            ..self.clone()
        }
    }

    /// `"id"` or `#index`, for messages.
    pub fn label(&self, index: usize) -> String {
        match &self.id {
            Some(id) => format!("{id:?}"),
            None => format!("#{index}"),
        }
    }
}

/// An effect in a stack, by position or by id.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EffectRef {
    Index(usize),
    Id(String),
}

impl EffectRef {
    /// Position of the effect in `stack`.
    pub fn resolve(&self, stack: &[VideoEffectSpec]) -> Result<usize, String> {
        match self {
            EffectRef::Index(i) if *i < stack.len() => Ok(*i),
            EffectRef::Index(i) => Err(format!("no effect {i} ({} effects)", stack.len())),
            EffectRef::Id(id) => stack
                .iter()
                .position(|e| e.id.as_deref() == Some(id))
                .ok_or_else(|| {
                    let ids: Vec<String> =
                        stack.iter().enumerate().map(|(i, e)| e.label(i)).collect();
                    format!(
                        "no effect with id {id:?} (effects: {})",
                        if ids.is_empty() {
                            "none".into()
                        } else {
                            ids.join(", ")
                        }
                    )
                }),
        }
    }
}

/// Every registered effect (built-ins included), sorted by type name.
pub fn registered() -> Vec<Arc<dyn VideoEffect>> {
    ensure_builtins();
    effect::registered()
}

/// Registered effect type names, sorted.
pub fn type_names() -> Vec<String> {
    ensure_builtins();
    effect::registered()
        .iter()
        .map(|e| e.type_name().to_string())
        .collect()
}

/// Set one parameter of `e`: a parameter name (a vector / color component
/// as `name.x` / `name.g`), `enabled` or `id`. `null` restores the default
/// (removes the key). The result is validated.
pub fn set_effect_param(e: &mut VideoEffectSpec, param: &str, value: Value) -> Result<(), String> {
    match param {
        "enabled" => {
            e.enabled = match value {
                Value::Null => true,
                Value::Bool(b) => b,
                v => return Err(format!("enabled: expected true or false, got {v}")),
            };
            return Ok(());
        }
        "id" => {
            e.id = match value {
                Value::Null => None,
                Value::String(s) if !s.is_empty() => Some(s),
                v => return Err(format!("id: expected a non-empty string or null, got {v}")),
            };
            return Ok(());
        }
        "type" => return Err("type cannot be changed; remove the effect and add another".into()),
        _ => {}
    }
    ensure_builtins();
    let fx =
        effect::lookup(&e.kind).ok_or_else(|| format!("unknown video effect type {:?}", e.kind))?;
    let (spec, comp) = ferrocut_core::param::find(fx.params(), param).ok_or_else(|| {
        let names: Vec<&str> = fx.params().iter().map(|s| s.name).collect();
        format!(
            "{}: unknown parameter {param:?} (parameters: {}, enabled, id)",
            e.kind,
            names.join(", ")
        )
    })?;
    match comp {
        None if value.is_null() => {
            e.params.remove(spec.name);
        }
        None => {
            e.params.insert(spec.name.to_string(), value);
        }
        Some(i) => {
            if value.is_null() {
                return Err(format!(
                    "{param}: a component cannot be null; set {} to null for the default",
                    spec.name
                ));
            }
            let cur = e
                .params
                .get(spec.name)
                .cloned()
                .or_else(|| serde_json::from_str::<Value>(spec.default).ok())
                .filter(|v| !v.is_null());
            let mut arr = match cur {
                Some(Value::Array(a)) => a,
                Some(v) if spec.kind == ParamKind::ScalarOrVec2 => vec![v.clone(), v],
                _ => {
                    return Err(format!(
                        "{param}: {} has no value to edit a component of; set the whole {} first",
                        spec.name, spec.name
                    ));
                }
            };
            if spec.kind == ParamKind::Color && arr.len() == 3 && i == 3 {
                arr.push(Value::from(1));
            }
            if i >= arr.len() {
                return Err(format!(
                    "{param}: {} has only {} components",
                    spec.name,
                    arr.len()
                ));
            }
            arr[i] = value;
            e.params.insert(spec.name.to_string(), Value::Array(arr));
        }
    }
    ParsedEffect::parse(e).map(|_| ())
}

/// Register the native effects (idempotent). Called before validation and
/// compilation; other crates' effects register themselves the same way.
pub fn ensure_builtins() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for e in native::all() {
            effect::register(e).expect("native video effect registers");
        }
    });
}

/// A parsed parameter value.
#[derive(Clone, Debug)]
enum Val {
    Anim(Animatable),
    Vec(Vec<Animatable>),
    Bool(bool),
    Str(String),
}

impl Val {
    fn sample(&self, spec: &ParamSpec, t: RationalTime) -> ParamValue {
        match self {
            Val::Anim(a) if spec.kind == ParamKind::ScalarOrVec2 => {
                let v = a.eval(t);
                ParamValue::Vec(vec![v, v])
            }
            Val::Anim(a) => ParamValue::Scalar(a.eval(t)),
            Val::Vec(v) => {
                let mut out: Vec<f64> = v.iter().map(|a| a.eval(t)).collect();
                if spec.kind == ParamKind::Color && out.len() == 3 {
                    out.push(1.0);
                }
                ParamValue::Vec(out)
            }
            Val::Bool(b) => ParamValue::Bool(*b),
            Val::Str(s) => ParamValue::Choice(s.clone()),
        }
    }
    fn animated(&self) -> bool {
        match self {
            Val::Anim(a) => a.is_animated(),
            Val::Vec(v) => v.iter().any(|a| a.is_animated()),
            _ => false,
        }
    }
}

fn parse_value(spec: &ParamSpec, v: &Value) -> Result<Val, String> {
    let n = spec.name;
    let anim = |v: &Value| -> Result<Animatable, String> {
        let a: Animatable = serde_json::from_value(v.clone())
            .map_err(|e| format!("{n}: expected an integer, a rational string (\"3/2\", \"0.8\") or {{\"keyframes\": [...]}} ({e})"))?;
        if !spec.animatable && a.is_animated() {
            return Err(format!("{n} is not animatable"));
        }
        spec.check(&a)?;
        Ok(a)
    };
    let list = |v: &Value, lens: &[usize]| -> Result<Vec<Animatable>, String> {
        let a = v
            .as_array()
            .filter(|a| lens.contains(&a.len()))
            .ok_or_else(|| {
                let want: Vec<String> = lens.iter().map(|l| l.to_string()).collect();
                format!("{n}: expected an array of {} values", want.join(" or "))
            })?;
        a.iter().map(anim).collect()
    };
    Ok(match spec.kind {
        ParamKind::Scalar | ParamKind::Time => Val::Anim(anim(v)?),
        ParamKind::Vec2 => Val::Vec(list(v, &[2])?),
        ParamKind::Vec3 => Val::Vec(list(v, &[3])?),
        ParamKind::Color => Val::Vec(list(v, &[3, 4])?),
        ParamKind::ScalarOrVec2 if v.is_array() => Val::Vec(list(v, &[2])?),
        ParamKind::ScalarOrVec2 => Val::Anim(anim(v)?),
        ParamKind::Bool => Val::Bool(
            v.as_bool()
                .ok_or_else(|| format!("{n}: expected true or false"))?,
        ),
        ParamKind::Choice => {
            let s = v
                .as_str()
                .ok_or_else(|| format!("{n}: expected a string"))?;
            if !spec.choices.is_empty() && !spec.choices.contains(&s) {
                return Err(format!(
                    "{n}: {s:?} is not one of {}",
                    spec.choices.join(", ")
                ));
            }
            Val::Str(s.to_string())
        }
        ParamKind::Object => {
            return Err(format!(
                "{n}: object parameters are not supported in video effects"
            ));
        }
    })
}

/// The spec of parameter `name` of the registered effect `kind`.
pub(crate) fn param_spec(kind: &str, name: &str) -> Option<ParamSpec> {
    ensure_builtins();
    effect::lookup(kind)?
        .params()
        .iter()
        .find(|s| s.name == name)
        .copied()
}

/// Defaults are JSON text written by effect authors: decimals there (`0.8`)
/// mean the exact decimal (`"0.8"`), as rationals require.
fn exact_numbers(v: Value) -> Value {
    match v {
        Value::Number(n) if !n.is_i64() => Value::String(n.to_string()),
        Value::Array(a) => Value::Array(a.into_iter().map(exact_numbers).collect()),
        v => v,
    }
}

/// An effect entry resolved against the registry, ready to sample.
#[derive(Clone)]
pub struct ParsedEffect {
    pub effect: Arc<dyn VideoEffect>,
    pub spec: VideoEffectSpec,
    vals: Vec<(ParamSpec, Val)>,
}

impl std::fmt::Debug for ParsedEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedEffect")
            .field("spec", &self.spec)
            .finish()
    }
}

impl ParsedEffect {
    /// Resolve `spec`: known type, known parameters with valid values, and
    /// (for constant parameters) the effect's own checks and working space.
    pub fn parse(spec: &VideoEffectSpec) -> Result<ParsedEffect, String> {
        ensure_builtins();
        let effect = effect::lookup(&spec.kind).ok_or_else(|| {
            let known: Vec<String> = effect::registered()
                .iter()
                .map(|e| e.type_name().to_string())
                .collect();
            format!(
                "unknown video effect type {:?} (known: {})",
                spec.kind,
                known.join(", ")
            )
        })?;
        let specs = effect.params();
        for k in spec.params.keys() {
            if !specs.iter().any(|s| s.name == k) {
                let names: Vec<&str> = specs.iter().map(|s| s.name).collect();
                return Err(format!(
                    "{}: unknown parameter {k:?} (parameters: {})",
                    spec.kind,
                    if names.is_empty() {
                        "none".into()
                    } else {
                        names.join(", ")
                    }
                ));
            }
        }
        let mut vals = Vec::new();
        for s in specs {
            let v = match spec.params.get(s.name) {
                Some(v) if !v.is_null() => Some(v.clone()),
                _ => serde_json::from_str::<Value>(s.default)
                    .ok()
                    .filter(|v| !v.is_null())
                    .map(exact_numbers),
            };
            if let Some(v) = v {
                vals.push((
                    *s,
                    parse_value(s, &v).map_err(|e| format!("{}: {e}", spec.kind))?,
                ));
            }
        }
        let p = ParsedEffect {
            effect,
            spec: spec.clone(),
            vals,
        };
        if !p.animated() {
            let s = p.sample(RationalTime::ZERO);
            p.check_sampled(&s)?;
        }
        Ok(p)
    }

    pub fn animated(&self) -> bool {
        self.vals.iter().any(|(_, v)| v.animated())
    }

    /// Parameters at `t` (in the stack's parameter time base).
    pub fn sample(&self, t: RationalTime) -> EffectParams {
        let mut p = EffectParams::new();
        for (s, v) in &self.vals {
            p.insert(s.name, v.sample(s, t));
        }
        p
    }

    fn check_sampled(&self, p: &EffectParams) -> Result<(), String> {
        self.effect
            .validate(p)
            .map_err(|e| format!("{}: {e}", self.spec.kind))?;
        let ws = self.effect.working_space(p);
        ferrocut_colorspace::named::space(ws.name())
            .map_err(|e| format!("{}: working space: {e}", self.spec.kind))?;
        Ok(())
    }
}

/// Validate an `effects` list (`owner` names it in messages, e.g. `clip c1`).
pub fn validate_list(
    effects: &[VideoEffectSpec],
    owner: &str,
) -> Result<Vec<ParsedEffect>, String> {
    let mut ids = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (i, e) in effects.iter().enumerate() {
        if let Some(id) = &e.id {
            if id.is_empty() {
                return Err(format!("{owner}: effects[{i}]: id must not be empty"));
            }
            if !ids.insert(id.as_str()) {
                return Err(format!(
                    "{owner}: effects[{i}]: effect id {id:?} is already used"
                ));
            }
        }
        out.push(
            ParsedEffect::parse(e)
                .map_err(|m| format!("{owner}: effects[{i}] ({}): {m}", e.kind))?,
        );
    }
    Ok(out)
}

/// The data-window bound of a stack: the display window plus half its size
/// of overscan on every side, and at most `max_dim` pixels per axis.
pub fn overscan_bound(display: (u32, u32), max_dim: u32) -> PixelRect {
    let (w, h) = display;
    let bw = (2 * w).min(max_dim.max(w));
    let bh = (2 * h).min(max_dim.max(h));
    PixelRect::new(-(((bw - w) / 2) as i32), -(((bh - h) / 2) as i32), bw, bh)
}

/// One sampled, enabled, non-identity effect.
struct Active<'a> {
    index: usize,
    fx: &'a ParsedEffect,
    params: EffectParams,
    space: WorkingSpace,
}

/// A compiled effect list.
#[derive(Clone, Debug)]
pub struct EffectStack {
    /// For messages: `clip c1`, `track "V2"`.
    pub owner: String,
    pub effects: Vec<ParsedEffect>,
    /// Parameter time = frame time minus this (the clip start for clip-local
    /// parameters, zero for timeline time).
    pub time_offset: RationalTime,
}

impl EffectStack {
    pub fn new(
        owner: impl Into<String>,
        specs: &[VideoEffectSpec],
        time_offset: RationalTime,
    ) -> anyhow::Result<Self> {
        let owner = owner.into();
        let effects = validate_list(specs, &owner).map_err(|e| anyhow::anyhow!(e))?;
        Ok(EffectStack {
            owner,
            effects,
            time_offset,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    pub fn batches_gpu_work(&self) -> bool {
        self.effects.iter().all(|e| e.effect.batches_gpu_work())
    }

    fn param_time(&self, t: RationalTime) -> RationalTime {
        t - self.time_offset
    }

    fn active(&self, t: RationalTime) -> Vec<Active<'_>> {
        let pt = self.param_time(t);
        self.effects
            .iter()
            .enumerate()
            .filter(|(_, e)| e.spec.enabled)
            .filter_map(|(index, fx)| {
                let params = fx.sample(pt);
                if fx.effect.is_identity(&params) {
                    return None;
                }
                let space = fx.effect.working_space(&params);
                Some(Active {
                    index,
                    fx,
                    params,
                    space,
                })
            })
            .collect()
    }

    /// Every parameter of every effect (the node's content hash).
    pub fn hash_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        for e in &self.effects {
            for s in [e.spec.kind.as_str(), e.effect.version()] {
                b.extend_from_slice(&(s.len() as u32).to_le_bytes());
                b.extend_from_slice(s.as_bytes());
            }
            b.push(e.spec.enabled as u8);
            let j = serde_json::to_string(&e.spec.params).unwrap_or_default();
            b.extend_from_slice(&(j.len() as u32).to_le_bytes());
            b.extend_from_slice(j.as_bytes());
        }
        b.extend_from_slice(&self.time_offset.hash_bytes());
        b
    }

    /// What changes the pixels at `t`: type, version, working space and
    /// sampled parameters of each enabled, non-identity effect.
    pub fn hash_bytes_at(&self, t: RationalTime) -> Vec<u8> {
        let mut b = Vec::new();
        for a in self.active(t) {
            for s in [
                a.fx.spec.kind.as_str(),
                a.fx.effect.version(),
                a.space.name(),
            ] {
                b.extend_from_slice(&(s.len() as u32).to_le_bytes());
                b.extend_from_slice(s.as_bytes());
            }
            let p = a.params.hash_bytes();
            b.extend_from_slice(&(p.len() as u32).to_le_bytes());
            b.extend_from_slice(&p);
        }
        b
    }

    fn wrap(&self, index: usize, kind: &str, e: NodeError) -> NodeError {
        let label = self.effects[index].spec.label(index);
        NodeError {
            message: format!("{}: effect {label} ({kind}): {}", self.owner, e.message),
            ..e
        }
    }

    /// Run the stack on `input` for the frame at timeline time `t`.
    pub fn apply(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: Arc<Frame>,
        t: RationalTime,
    ) -> Result<Arc<Frame>, NodeError> {
        let active = self.active(t);
        if active.is_empty() {
            return Ok(input);
        }
        let display = (input.width, input.height);
        let canvas = Canvas {
            width: input.width,
            height: input.height,
            pixel_aspect: input.pixel_aspect.to_f64(),
        };
        let max_dim = ctx.gpu.device.limits().max_texture_dimension_2d;
        let bound = overscan_bound(display, max_dim);
        let pt = self.param_time(t);
        for a in &active {
            if a.fx.animated() {
                a.fx.check_sampled(&a.params).map_err(|m| {
                    self.wrap(
                        a.index,
                        &a.fx.spec.kind,
                        NodeError::permanent(format!("at {pt}: {m}")),
                    )
                })?;
            }
        }
        // Data windows downstream.
        let mut windows = vec![input.data_window];
        for a in &active {
            let w =
                a.fx.effect
                    .output_window(*windows.last().unwrap(), &a.params, canvas);
            windows.push(w.intersect(&bound));
        }
        // Requests upstream: out[i] is what effect i must produce.
        let n = active.len();
        let mut out = vec![PixelRect::default(); n];
        out[n - 1] = windows[n];
        for i in (1..n).rev() {
            out[i - 1] = active[i]
                .fx
                .effect
                .input_region(out[i], &active[i].params, pt)
                .intersect(&windows[i]);
        }
        let fxk = kernels::kernels(ctx)?;
        let mut cur = input;
        let mut space = ColorSpace::ACESCG;
        for (i, a) in active.iter().enumerate() {
            ctx.check()?;
            let kind = a.fx.spec.kind.as_str();
            let need =
                a.fx.effect
                    .input_region(out[i], &a.params, pt)
                    .intersect(&cur.data_window);
            if a.space.name() != space {
                let region = if need.is_empty() {
                    PixelRect::new(0, 0, 1, 1)
                } else {
                    need
                };
                cur = Arc::new(fxk.convert(ctx, &cur, space, a.space.name(), region)?);
                space = a.space.name();
            }
            if out[i].is_empty() {
                let mut f = fxk.clear(ctx, &cur, out[i])?;
                f.color_space = ColorSpace::new(space);
                cur = Arc::new(f);
                continue;
            }
            let req = EffectRequest {
                time: t,
                param_time: pt,
                region: out[i],
                canvas,
                label: self.effects[a.index].spec.label(a.index),
            };
            let mut f =
                a.fx.effect
                    .render(ctx, &cur, &a.params, &req)
                    .map_err(|e| self.wrap(a.index, kind, e))?;
            if f.data_window != out[i] || (f.width, f.height) != display {
                return Err(self.wrap(
                    a.index,
                    kind,
                    NodeError::permanent(format!(
                        "returned a {}x{} frame with data window {:?}, asked for {:?}",
                        f.width, f.height, f.data_window, out[i]
                    )),
                ));
            }
            f.color_space = ColorSpace::new(space);
            cur = Arc::new(f);
        }
        if space != ColorSpace::ACESCG {
            let w = cur.data_window;
            cur = Arc::new(fxk.convert(ctx, &cur, space, ColorSpace::ACESCG, w)?);
        }
        Ok(cur)
    }
}

/// A clip's or track's effect stack, then (clips) the clip opacity.
pub struct EffectNode {
    pub stack: EffectStack,
    /// Clip opacity in clip-local time (`(opacity, clip start)`), applied after the stack.
    pub opacity: Option<(Animatable, RationalTime)>,
}

impl EffectNode {
    fn opacity_at(&self, t: RationalTime) -> f32 {
        match &self.opacity {
            None => 1.0,
            Some((Animatable::Constant(v), _)) => v.to_f32_param(),
            Some((a, start)) => a.eval(t - *start).clamp(0.0, 1.0) as f32,
        }
    }
}

impl RenderNode for EffectNode {
    fn kind(&self) -> &'static str {
        "effects"
    }
    fn batches_gpu_work(&self) -> bool {
        self.stack.batches_gpu_work()
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        let op = match &self.opacity {
            None => Vec::new(),
            Some((a, s)) => {
                let mut h = blake3::Hasher::new();
                a.hash_into(&mut h);
                h.update(&s.hash_bytes());
                h.finalize().as_bytes().to_vec()
            }
        };
        NodeHash::of("effects", &[&self.stack.hash_bytes(), &op])
    }
    fn content_hash_at(&self, t: RationalTime) -> NodeHash {
        NodeHash::of(
            "effects.at",
            &[
                &self.stack.hash_bytes_at(t),
                &self.opacity_at(t).to_bits().to_le_bytes(),
            ],
        )
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull { input: 0, time: t }]
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        let f = self.stack.apply(ctx, inputs[0].clone(), t)?;
        let o = self.opacity_at(t);
        if o == 1.0 {
            return Ok(f);
        }
        let comp = compositor(ctx)?;
        Ok(Arc::new(comp.opacity(ctx, &f, o)?))
    }
}

/// One adjustment clip: its time range, stack (clip-local parameters) and opacity.
#[derive(Clone, Debug)]
pub struct AdjustClip {
    pub range: ClipRange,
    pub stack: EffectStack,
    pub opacity: Animatable,
}

/// Adjustment layers (After Effects / Premiere): while one of `clips` is
/// active, its effect stack runs on the composite below (input 0) and the
/// result replaces it, mixed by the clip opacity and the track matte
/// (input 1, when `matte` is set). Elsewhere input 0 passes through.
pub struct AdjustNode {
    pub clips: Vec<AdjustClip>,
    pub matte: Option<MatteMode>,
}

impl AdjustNode {
    fn active(&self, t: RationalTime) -> Option<&AdjustClip> {
        self.clips
            .iter()
            .find(|c| c.range.start <= t && t < c.range.end)
    }
    fn opacity_at(c: &AdjustClip, t: RationalTime) -> f64 {
        c.opacity.eval(t - c.range.start).clamp(0.0, 1.0)
    }
}

impl RenderNode for AdjustNode {
    fn kind(&self) -> &'static str {
        "adjustment"
    }
    fn batches_gpu_work(&self) -> bool {
        self.clips.iter().all(|c| c.stack.batches_gpu_work())
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn content_hash(&self) -> NodeHash {
        let mut b = Vec::new();
        for c in &self.clips {
            b.extend_from_slice(&c.range.start.hash_bytes());
            b.extend_from_slice(&c.range.end.hash_bytes());
            let mut h = blake3::Hasher::new();
            c.opacity.hash_into(&mut h);
            b.extend_from_slice(h.finalize().as_bytes());
            b.extend_from_slice(blake3::hash(&c.stack.hash_bytes()).as_bytes());
        }
        let m = self.matte.map_or("none", |m| m.name());
        NodeHash::of("adjustment", &[&b, m.as_bytes()])
    }
    fn content_hash_at(&self, t: RationalTime) -> NodeHash {
        match self.active(t) {
            None => NodeHash::of("adjustment.pass", &[]),
            Some(c) => {
                let m = self.matte.map_or("none", |m| m.name());
                NodeHash::of(
                    "adjustment.at",
                    &[
                        &c.stack.hash_bytes_at(t),
                        &Self::opacity_at(c, t).to_bits().to_le_bytes(),
                        m.as_bytes(),
                    ],
                )
            }
        }
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        let mut p = vec![Pull { input: 0, time: t }];
        if self.active(t).is_some() && self.matte.is_some() {
            p.push(Pull { input: 1, time: t });
        }
        p
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        let bg = inputs[0].clone();
        let Some(c) = self.active(t) else {
            return Ok(bg);
        };
        let k = Self::opacity_at(c, t);
        let fx = c.stack.apply(ctx, bg.clone(), t)?;
        if k == 0.0 || (Arc::ptr_eq(&fx, &bg)) {
            return Ok(bg);
        }
        let matte = match (self.matte, inputs.get(1)) {
            (Some(m), Some(f)) => Some((f.as_ref(), m)),
            _ => None,
        };
        if k == 1.0 && matte.is_none() {
            return Ok(fx);
        }
        let fxk = kernels::kernels(ctx)?;
        Ok(Arc::new(fxk.adjust_mix(ctx, &bg, &fx, k, matte)?))
    }
}

/// The `video_effects` section of the params registry: every registered
/// effect with its parameters (MCP `timeline_schema` / params list).
pub fn registry_json() -> Value {
    ensure_builtins();
    Value::Array(
        effect::registered()
            .iter()
            .map(|e| {
                serde_json::json!({
                    "type": e.type_name(),
                    "doc": e.doc(),
                    "params": e.params(),
                })
            })
            .collect(),
    )
}
