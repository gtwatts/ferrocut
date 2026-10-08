//! Expressions: After Effects-style scripts on any numeric parameter.
//!
//! Any animatable value in a timeline (a clip's `opacity`, one component of
//! `transform.position`, a video effect's `sigma`, a track bus's `gain_db`,
//! the camera's `zoom`, ...) may be an expression object instead of a
//! constant or keyframes:
//!
//! ```json
//! {"expression": "value + wiggle(2, 30) - value", "value": "960"}
//! ```
//!
//! `value` (optional) is the pre-expression value: a constant or keyframes in
//! the parameter's time base (default: the parameter's default).
//!
//! # Language
//!
//! [rhai](https://rhai.rs) syntax, sandboxed: no modules/imports, no file,
//! network, clock or environment access, no `eval`, `print`/`debug` are
//! discarded, every evaluation is bounded (operations, call depth, string and
//! array sizes; see [`LIMITS`]). Evaluation is a pure function of the timeline
//! and the time, so renders stay bit-exact. Note rhai integer division
//! truncates (`1 / 2 == 0`); write `1.0 / 2`.
//!
//! Variables (constants): `time` (seconds in the parameter's time base:
//! clip-local for clip parameters, source time for generator parameters,
//! timeline time for tracks and the timeline), `value`, `fps`, `frame`
//! (integer frame index of `time`), `comp_time` (timeline seconds),
//! `duration` (the owner's duration: clip duration, or the timeline's),
//! `in_point` (owner start in timeline seconds; 0 for tracks/the timeline).
//!
//! Functions (numbers may be integers or floats):
//! - `wiggle(freq, amp [, octaves = 1, amp_mult = 0.5, t = time])`: `value`
//!   plus smooth deterministic noise (`freq` wiggles per second, roughly
//!   ±`amp`), seeded by the parameter's path (two parameters with the same
//!   expression wiggle differently) and by `seed_random`.
//! - `seed_random(n [, timeless = false])`: reseed `wiggle`/`random` for
//!   this evaluation; `timeless` makes `random` constant over time.
//! - `random()`, `random(max)`, `random(min, max)`: deterministic uniform
//!   numbers, new each frame (unless timeless), different per call.
//! - `noise(x)`: smooth 1D noise in [-1, 1] (fixed seed).
//! - `linear(t, t_min, t_max, v1, v2)`, `linear(t, v1, v2)` (t in [0, 1]),
//!   and `ease`, `ease_in`, `ease_out` with the same arguments.
//! - `clamp(x, lo, hi)`, `lerp(a, b, s)`.
//! - `value_at_time(t)`: the pre-expression value at `t` (script time).
//! - `loop_out(type [, keys = 0])`, `loop_in(type [, keys = 0])`: AE
//!   `loopOut`/`loopIn` over `value`'s keyframes: type `"cycle"`,
//!   `"pingpong"`, `"offset"` or `"continue"`; `keys` = how many trailing
//!   (leading) segments to loop (0 = all). `loop_out()` = `"cycle"`.
//! - `param(path)`: another parameter of the same clip/track/timeline at the
//!   same timeline time, e.g. `param("transform.rotation")`,
//!   `param("transform.position.x")`, `param("effects.blur.sigma")` (effects
//!   by id or index), `param("opacity")`.
//! - `layer(clip_id).param(path)`, `track(name).param(path)`,
//!   `comp().param(path)`: the same for another clip, a track, or the
//!   timeline (e.g. `comp().param("camera.zoom")`). Referenced expressions
//!   are evaluated too; a reference cycle is an error naming the cycle.
//!
//! # Baking and caching
//!
//! [`bake`] evaluates every expression on its parameter's frame grid (one
//! sample per output frame over the owner's range plus one frame on each
//! side) and replaces it with linear keyframes (exact decimals where the
//! value is one, else a 2^-30 grid), so the renderer, cache keys, audio and
//! retiming see ordinary keyframes: an expression is part of every cache key
//! through its values, and changing the script, its `value`, or anything it
//! references re-renders exactly the frames whose values changed.
//! Sub-frame samples (motion blur) interpolate linearly between frames.
//! [`Timeline::validate`] bakes too, so a bad expression is reported (with the
//! owner, parameter, time, line and column) when the timeline is loaded or
//! edited, never at render time. Values outside a parameter's range are
//! errors naming the time.
//! Source properties sample the clip's actual speed/remap clock; references
//! and `comp_time` retain the actual timeline clock. Nonmonotonic source
//! expression remaps, expression-driven timing plus source expressions, and
//! frozen sources requiring changing timeline-dependent values are rejected
//! with guidance rather than represented by ambiguous source keyframes.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::anyhow;
use ferrocut_core::param::{ParamKind, ParamSpec, TimeBase};
use ferrocut_core::{Animatable, Expression, Rational, RationalTime};
use rhai::{AST, Dynamic, Engine, EvalAltResult, NativeCallContext, Position, Scope};
use serde_json::{Value, json};

use crate::params::Scope as PScope;
use crate::timeline::Timeline;

/// Sandbox limits per evaluation.
pub const LIMITS: Limits = Limits {
    max_operations: 100_000,
    max_call_levels: 32,
    max_expr_depth: 64,
    max_string_size: 4096,
    max_array_size: 4096,
    max_map_size: 256,
};

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Limits {
    pub max_operations: u64,
    pub max_call_levels: usize,
    pub max_expr_depth: usize,
    pub max_string_size: usize,
    pub max_array_size: usize,
    pub max_map_size: usize,
}

/// Script variables (also the template scope for strict-variable compiles).
pub const VARIABLES: &[(&str, &str)] = &[
    (
        "time",
        "seconds in the parameter's time base (clip-local / source / timeline)",
    ),
    ("value", "the pre-expression value at `time`"),
    ("fps", "output frames per second"),
    ("frame", "integer frame index of `time`"),
    ("comp_time", "timeline seconds"),
    (
        "duration",
        "the owner's duration in seconds (clip, or timeline)",
    ),
    (
        "in_point",
        "the owner's start in timeline seconds (0 for tracks / the timeline)",
    ),
];

/// Script functions (name, signature, doc), for the schema/params tools.
pub const FUNCTIONS: &[(&str, &str)] = &[
    (
        "wiggle",
        "wiggle(freq, amp [, octaves=1, amp_mult=0.5, t=time]): value + smooth deterministic noise seeded by the parameter path",
    ),
    (
        "seed_random",
        "seed_random(n [, timeless=false]): reseed wiggle/random",
    ),
    (
        "random",
        "random() / random(max) / random(min, max): deterministic uniform, new each frame",
    ),
    ("noise", "noise(x): smooth 1D noise in [-1, 1]"),
    (
        "linear",
        "linear(t, t_min, t_max, v1, v2) or linear(t, v1, v2) with t in [0, 1]",
    ),
    ("ease", "ease(...): like linear, smoothstep easing"),
    ("ease_in", "ease_in(...): like linear, eases in"),
    ("ease_out", "ease_out(...): like linear, eases out"),
    ("clamp", "clamp(x, lo, hi)"),
    ("lerp", "lerp(a, b, s)"),
    (
        "value_at_time",
        "value_at_time(t): pre-expression value at script time t",
    ),
    (
        "loop_out",
        "loop_out([type=\"cycle\" | \"pingpong\" | \"offset\" | \"continue\" [, keys=0]]): loop value's keyframes after the last key",
    ),
    (
        "loop_in",
        "loop_in([type [, keys=0]]): loop value's keyframes before the first key",
    ),
    (
        "param",
        "param(path): another parameter of the same owner at the same timeline time, e.g. \"transform.position.x\", \"effects.blur.sigma\"",
    ),
    (
        "layer",
        "layer(clip_id).param(path): a parameter of another clip",
    ),
    ("track", "track(name).param(path): a parameter of a track"),
    (
        "comp",
        "comp().param(path): a timeline parameter, e.g. \"camera.zoom\"",
    ),
];

/// The expression language for the schema / params tools.
pub fn language_json() -> Value {
    let vars: Vec<Value> = VARIABLES
        .iter()
        .map(|(n, d)| json!({"name": n, "doc": d}))
        .collect();
    let fns: Vec<Value> = FUNCTIONS
        .iter()
        .map(|(n, d)| json!({"name": n, "doc": d}))
        .collect();
    json!({
        "syntax": "rhai (https://rhai.rs); integer division truncates, write 1.0 / 2",
        "form": {"expression": "<script>", "value": "optional constant or keyframes (the pre-expression value)"},
        "variables": vars,
        "functions": fns,
        "limits": LIMITS,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Seg {
    Key(String),
    Idx(usize),
}

fn segs_name(segs: &[Seg]) -> String {
    let mut s = String::new();
    for (i, g) in segs.iter().enumerate() {
        if i > 0 {
            s.push('.');
        }
        match g {
            Seg::Key(k) => s.push_str(k),
            Seg::Idx(n) => {
                let _ = write!(s, "{n}");
            }
        }
    }
    s
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerKind {
    VideoClip,
    AudioClip,
    VideoTrack,
    AudioTrack,
    Timeline,
}

struct Owner {
    kind: OwnerKind,
    /// For messages: `clip "dot"`, `track "V1"`, `timeline`.
    label: String,
    /// Seed namespace: `clip:dot`, `track:V1`, `timeline`.
    key: String,
    /// The owner's object in the timeline JSON (expressions still in place).
    json: Value,
    /// Path of the owner object in the timeline JSON.
    pointer: Vec<Seg>,
    start: Rational,
    duration: Rational,
    time_map: crate::retime::TimeMap,
}

struct Site {
    owner: usize,
    rel: Vec<Seg>,
    name: String,
    base: TimeBase,
    expr: Expression,
    ast: Arc<AST>,
    default: f64,
    spec: Option<ParamSpec>,
    seed: u64,
}

impl Site {
    /// The pre-expression value at parameter time `t`.
    fn value_at(&self, t: Rational) -> f64 {
        match &self.expr.value {
            Some(v) => v.eval(RationalTime(t)),
            None => self.default,
        }
    }
}

struct EvalFrame {
    site: usize,
    /// Parameter time.
    t: Rational,
    ct: Rational,
    seed: u64,
    timeless: bool,
    calls: u64,
}

struct State {
    owners: Vec<Owner>,
    sites: Vec<Site>,
    by_path: HashMap<(usize, Vec<Seg>), usize>,
    memo: HashMap<(usize, Rational), f64>,
    stack: Vec<EvalFrame>,
    fps: Rational,
    frame: (u32, u32),
    tl_duration: Rational,
}

type Shared = Arc<Mutex<State>>;
type EvalResult<T> = Result<T, Box<EvalAltResult>>;

fn lock(s: &Shared) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

fn is_expression(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.contains_key("expression"))
}

/// True if any parameter of `tl` is an expression.
pub fn has_expressions(tl: &Timeline) -> bool {
    fn walk(v: &Value) -> bool {
        match v {
            Value::Object(o) => o.contains_key("expression") || o.values().any(walk),
            Value::Array(a) => a.iter().any(walk),
            _ => false,
        }
    }
    serde_json::to_value(tl).is_ok_and(|v| walk(&v))
}

impl State {
    fn owner_of_param_time(&self, site: &Site, ct: Rational) -> Rational {
        let o = &self.owners[site.owner];
        param_time(site.base, o, ct)
    }
}

fn param_time(base: TimeBase, o: &Owner, ct: Rational) -> Rational {
    match base {
        TimeBase::ClipLocal => ct - o.start,
        TimeBase::Source => o.time_map.source_at(RationalTime(ct - o.start)).0,
        _ => ct,
    }
}

fn scope_of(kind: OwnerKind) -> PScope {
    match kind {
        OwnerKind::VideoClip => PScope::VideoClip,
        OwnerKind::AudioClip => PScope::AudioClip,
        OwnerKind::VideoTrack => PScope::Track { audio_track: false },
        OwnerKind::AudioTrack => PScope::Track { audio_track: true },
        OwnerKind::Timeline => PScope::Timeline,
    }
}

fn is_clip(kind: OwnerKind) -> bool {
    matches!(kind, OwnerKind::VideoClip | OwnerKind::AudioClip)
}

/// The registry spec (and vector component) of the parameter at `rel` in
/// `owner`, if it has one.
fn spec_of(owner: &Owner, rel: &[Seg]) -> (Option<ParamSpec>, Option<usize>) {
    let comp = match rel.last() {
        Some(Seg::Idx(i)) => Some(*i),
        _ => None,
    };
    // Video effects: effects.<i>.<param>[.<comp>]
    if let [Seg::Key(e), Seg::Idx(i), Seg::Key(p), rest @ ..] = rel
        && e == "effects"
        && rest.len() <= 1
    {
        let kind = owner.json["effects"][*i]["type"].as_str().unwrap_or("");
        return (crate::fx::param_spec(kind, p), comp);
    }
    if owner.kind == OwnerKind::VideoClip
        && matches!(rel.first(), Some(Seg::Key(k)) if k == "generator" || k == "masks")
    {
        let path = if comp.is_some() {
            &rel[..rel.len() - 1]
        } else {
            rel
        };
        return (
            crate::params::resolve_path(PScope::VideoClip, &segs_name(path))
                .ok()
                .map(|(s, _, _)| *s),
            comp,
        );
    }
    let keys: Vec<&str> = rel
        .iter()
        .filter_map(|s| match s {
            Seg::Key(k) => Some(k.as_str()),
            Seg::Idx(_) => None,
        })
        .collect();
    if keys.len() + usize::from(comp.is_some()) != rel.len() {
        return (None, comp);
    }
    let mut name = keys.join(".");
    if owner.kind == OwnerKind::VideoTrack && name.starts_with("audio.") {
        name = name.replacen("audio", "bus", 1);
    }
    let specs = scope_of(owner.kind).specs();
    (specs.iter().find(|s| s.name == name).copied(), comp)
}

fn base_of(owner: &Owner, rel: &[Seg], spec: Option<&ParamSpec>) -> TimeBase {
    if let Some(s) = spec
        && matches!(
            s.time,
            TimeBase::ClipLocal | TimeBase::Source | TimeBase::Timeline
        )
        && !matches!(rel.first(), Some(Seg::Key(k)) if k == "effects")
    {
        return s.time;
    }
    if is_clip(owner.kind) {
        match rel.first() {
            Some(Seg::Key(k)) if k == "generator" || k == "masks" => TimeBase::Source,
            _ => TimeBase::ClipLocal,
        }
    } else {
        TimeBase::Timeline
    }
}

/// The default of `spec` (component `comp`) as a number.
fn default_of(spec: Option<&ParamSpec>, comp: Option<usize>, frame: (u32, u32)) -> f64 {
    let Some(spec) = spec else { return 0.0 };
    let v: Value = if matches!(
        spec.kind,
        ParamKind::Vec2 | ParamKind::Vec3 | ParamKind::Color | ParamKind::ScalarOrVec2
    ) && comp.is_some()
    {
        crate::params::vec_default(spec, frame)
    } else {
        serde_json::from_str(spec.default).unwrap_or(Value::Null)
    };
    let v = match (comp, &v) {
        (Some(i), Value::Array(a)) => a.get(i).cloned().unwrap_or(json!("1")),
        _ => v,
    };
    number_of(&v).unwrap_or(0.0)
}

fn number_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<Rational>().ok().map(Rational::to_f64),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

fn get_path<'a>(v: &'a Value, segs: &[Seg]) -> Option<&'a Value> {
    let mut cur = v;
    for s in segs {
        cur = match (s, cur) {
            (Seg::Key(k), Value::Object(o)) => o.get(k)?,
            (Seg::Idx(i), Value::Array(a)) => a.get(*i)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Resolve a user path (`transform.position.x`, `effects.blur.sigma`) in an
/// owner's JSON to concrete segments.
fn resolve_path(json: &Value, path: &str) -> Result<Vec<Seg>, String> {
    if path.trim().is_empty() {
        return Err("empty parameter path".into());
    }
    let mut out = Vec::new();
    let mut cur = Some(json);
    for part in path.split('.') {
        let comp_idx = match part {
            "x" | "r" => Some(0),
            "y" | "g" => Some(1),
            "z" | "b" => Some(2),
            "a" => Some(3),
            _ => part.parse::<usize>().ok(),
        };
        let seg = match cur {
            Some(Value::Array(a)) => {
                if let Some(i) = comp_idx {
                    Seg::Idx(i)
                } else if let Some(i) = a.iter().position(|e| e["id"].as_str() == Some(part)) {
                    Seg::Idx(i)
                } else {
                    return Err(format!("{path:?}: no element {part:?}"));
                }
            }
            // A uniform (scalar) value: `.x` / `.y` read the scalar itself.
            Some(Value::String(_) | Value::Number(_)) if comp_idx.is_some() => {
                return Ok(out);
            }
            Some(o) if is_expression(o) && comp_idx.is_some() => return Ok(out),
            _ => match comp_idx {
                Some(i) if part.parse::<usize>().is_ok() || out.last().is_some() => {
                    // Missing vector parameter: component of its default.
                    let _ = i;
                    Seg::Idx(i)
                }
                _ => Seg::Key(part.to_string()),
            },
        };
        cur = match (&seg, cur) {
            (Seg::Key(k), Some(Value::Object(o))) => o.get(k),
            (Seg::Idx(i), Some(Value::Array(a))) => a.get(*i),
            _ => None,
        };
        out.push(seg);
    }
    Ok(out)
}

fn num(d: &Dynamic, what: &str) -> EvalResult<f64> {
    if let Ok(f) = d.as_float() {
        return Ok(f);
    }
    if let Ok(i) = d.as_int() {
        return Ok(i as f64);
    }
    Err(format!("{what}: expected a number, got {}", d.type_name()).into())
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn unit(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// 1D gradient noise in about [-1, 1], 0 at integers, C2-smooth.
fn gnoise(x: f64, seed: u64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let g = |k: f64| unit(splitmix(seed ^ splitmix(k as i64 as u64))) * 2.0 - 1.0;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    2.0 * ((1.0 - u) * g(i) * f + u * g(i + 1.0) * (f - 1.0))
}

fn fractal(x: f64, seed: u64, octaves: i64, amp_mult: f64) -> f64 {
    let (mut sum, mut a, mut fr) = (0.0, 1.0, 1.0);
    for o in 0..octaves.clamp(1, 16) {
        let s = splitmix(seed.wrapping_add(o as u64));
        // A per-octave phase, so whole seconds are not lattice zeros.
        let phase = unit(splitmix(s ^ 0x0070_6861_7365));
        sum += a * gnoise(x * fr + phase, s);
        a *= amp_mult;
        fr *= 2.0;
    }
    sum
}

fn ease_curve(kind: &str, s: f64) -> f64 {
    let s = s.clamp(0.0, 1.0);
    match kind {
        "ease" => s * s * (3.0 - 2.0 * s),
        "ease_in" => s * s * s,
        "ease_out" => 1.0 - (1.0 - s).powi(3),
        _ => s,
    }
}

fn interp5(kind: &str, t: f64, t0: f64, t1: f64, v0: f64, v1: f64) -> f64 {
    let s = if t1 == t0 {
        if t < t0 { 0.0 } else { 1.0 }
    } else {
        (t - t0) / (t1 - t0)
    };
    v0 + (v1 - v0) * ease_curve(kind, s)
}

/// AE loopOut / loopIn over `value`'s keyframes at parameter time `t`.
fn looped(
    site: &Site,
    t: Rational,
    kind: &str,
    keys: i64,
    out: bool,
    fps: Rational,
) -> EvalResult<f64> {
    let Some(Animatable::Keyframes(k)) = site.expr.value.as_deref() else {
        return Ok(site.value_at(t));
    };
    let ks = &k.keyframes;
    if ks.len() < 2 {
        return Ok(site.value_at(t));
    }
    let n = ks.len() - 1;
    let segs = if keys <= 0 { n } else { (keys as usize).min(n) };
    let (a, b) = if out {
        (ks[n - segs].t.0, ks[n].t.0)
    } else {
        (ks[0].t.0, ks[segs].t.0)
    };
    let v = |x: Rational| site.value_at(x);
    let p = b - a;
    if (out && t <= b) || (!out && t >= a) || p <= Rational::ZERO {
        return Ok(site.value_at(t));
    }
    // Exact rational phase: (t - a) = cycles * p + r, 0 <= r < p.
    let cycles = (t - a)
        .checked_div(p)
        .map_err(|e| format!("loop: {e}"))?
        .floor();
    let r = (t - a) - p * Rational::from_int(cycles);
    let tf = t.to_f64();
    Ok(match kind {
        "cycle" => v(a + r),
        "pingpong" => {
            if cycles.rem_euclid(2) == 1 {
                v(b - r)
            } else {
                v(a + r)
            }
        }
        "offset" => v(a + r) + cycles as f64 * (v(b) - v(a)),
        "continue" => {
            let h = Rational::ONE / fps;
            let hf = h.to_f64();
            if out {
                let slope = (v(b) - v(b - h)) / hf;
                v(b) + slope * (tf - b.to_f64())
            } else {
                let slope = (v(a + h) - v(a)) / hf;
                v(a) + slope * (tf - a.to_f64())
            }
        }
        other => {
            return Err(format!(
                "loop type {other:?} is not one of \"cycle\", \"pingpong\", \"offset\", \"continue\""
            )
            .into());
        }
    })
}

/// `x` as a rational: the shortest exact decimal with up to 9 places if one
/// equals `x`, else the nearest multiple of 2^-30.
pub fn to_rational(x: f64) -> Option<Rational> {
    if !x.is_finite() || x.abs() >= (1u64 << 31) as f64 {
        return None;
    }
    let mut d: i64 = 1;
    for _ in 0..=9 {
        let n = (x * d as f64).round();
        if n / d as f64 == x {
            return Some(Rational::new(n as i64, d));
        }
        d *= 10;
    }
    Some(Rational::new(
        (x * (1u64 << 30) as f64).round() as i64,
        1 << 30,
    ))
}

#[derive(Clone)]
struct LayerRef(usize);

fn eval_site(engine: &Engine, st: &Shared, idx: usize, ct: Rational) -> EvalResult<f64> {
    let (ast, scope) = {
        let mut s = lock(st);
        if let Some(v) = s.memo.get(&(idx, ct)) {
            return Ok(*v);
        }
        if let Some(pos) = s.stack.iter().position(|f| f.site == idx) {
            let mut chain: Vec<String> = s.stack[pos..]
                .iter()
                .map(|f| site_label(&s, f.site))
                .collect();
            chain.push(site_label(&s, idx));
            return Err(format!("expression cycle: {}", chain.join(" -> ")).into());
        }
        let site = &s.sites[idx];
        let o = &s.owners[site.owner];
        let t = s.owner_of_param_time(site, ct);
        let time = t + site.expr.time_offset;
        let mut scope = Scope::new();
        scope.push_constant("time", time.to_f64());
        scope.push_constant("value", site.value_at(t));
        scope.push_constant("fps", s.fps.to_f64());
        scope.push_constant("frame", RationalTime(time).frame_floor(s.fps));
        scope.push_constant("comp_time", ct.to_f64());
        let (dur, inp) = if is_clip(o.kind) {
            (o.duration, o.start)
        } else {
            (s.tl_duration, Rational::ZERO)
        };
        scope.push_constant("duration", dur.to_f64());
        scope.push_constant("in_point", inp.to_f64());
        let ast = site.ast.clone();
        let seed = site.seed;
        s.stack.push(EvalFrame {
            site: idx,
            t,
            ct,
            seed,
            timeless: false,
            calls: 0,
        });
        (ast, scope)
    };
    let mut scope = scope;
    let r = engine.eval_ast_with_scope::<Dynamic>(&mut scope, &ast);
    let mut s = lock(st);
    s.stack.pop();
    let d = r?;
    let v = if let Ok(f) = d.as_float() {
        f
    } else if let Ok(i) = d.as_int() {
        i as f64
    } else {
        return Err(format!(
            "{}: the expression must return a number, got {} ({d})",
            site_label(&s, idx),
            d.type_name()
        )
        .into());
    };
    if !v.is_finite() {
        return Err(format!("{}: the expression returned {v}", site_label(&s, idx)).into());
    }
    s.memo.insert((idx, ct), v);
    Ok(v)
}

fn site_label(s: &State, idx: usize) -> String {
    let site = &s.sites[idx];
    format!("{}: {}", s.owners[site.owner].label, site.name)
}

/// Value of the parameter `path` of owner `oi` at timeline time `ct`.
fn param_value(
    engine: &Engine,
    st: &Shared,
    oi: usize,
    path: &str,
    ct: Rational,
) -> EvalResult<f64> {
    enum Next {
        Site(usize),
        Value(f64),
    }
    let next = {
        let s = lock(st);
        let o = &s.owners[oi];
        let rel = resolve_path(&o.json, path).map_err(|e| format!("{}: {e}", o.label))?;
        let (spec, comp) = spec_of(o, &rel);
        let base = base_of(o, &rel, spec.as_ref());
        // An expression on another clip can read this owner's source-clock
        // keyframes even when this owner has no source expressions itself.
        // Its original TimeMap cannot represent expression-driven timing.
        if base == TimeBase::Source
            && s.sites.iter().any(|site| {
                site.owner == oi && matches!(site.name.as_str(), "speed" | "time_remap")
            })
        {
            return Err(format!(
                "{}: cannot read source parameter {path:?} with expression-driven speed/time_remap; bake the timing curve to keyframes first",
                o.label
            )
            .into());
        }
        if let Some(&i) = s.by_path.get(&(oi, rel.clone())) {
            Next::Site(i)
        } else {
            let t = param_time(base, o, ct);
            match get_path(&o.json, &rel) {
                Some(Value::Array(_)) => {
                    return Err(format!(
                        "{}: {path:?} is a vector; pick a component ({path}.x, {path}.y or {path}.0, ...)",
                        o.label
                    )
                    .into());
                }
                Some(v @ (Value::String(_) | Value::Number(_) | Value::Object(_))) => {
                    let a: Animatable = serde_json::from_value(v.clone()).map_err(|e| {
                        format!("{}: {path:?} is not a numeric parameter ({e})", o.label)
                    })?;
                    Next::Value(a.eval(RationalTime(t)))
                }
                Some(Value::Bool(b)) => Next::Value(f64::from(u8::from(*b))),
                _ if spec.is_some() => Next::Value(default_of(spec.as_ref(), comp, s.frame)),
                _ => {
                    let names: Vec<&str> =
                        scope_of(o.kind).specs().iter().map(|s| s.name).collect();
                    return Err(format!(
                        "{}: no parameter {path:?} (parameters: {})",
                        o.label,
                        names.join(", ")
                    )
                    .into());
                }
            }
        }
    };
    match next {
        Next::Value(v) => Ok(v),
        Next::Site(i) => eval_site(engine, st, i, ct),
    }
}

fn current(st: &Shared) -> EvalResult<(usize, Rational, Rational)> {
    let s = lock(st);
    let f = s.stack.last().ok_or("no expression is being evaluated")?;
    let site = &s.sites[f.site];
    Ok((site.owner, f.ct, f.t))
}

fn build_engine(st: &Shared) -> Engine {
    let mut e = Engine::new();
    e.set_max_operations(LIMITS.max_operations);
    e.set_max_call_levels(LIMITS.max_call_levels);
    e.set_max_expr_depths(LIMITS.max_expr_depth, LIMITS.max_expr_depth);
    e.set_max_string_size(LIMITS.max_string_size);
    e.set_max_array_size(LIMITS.max_array_size);
    e.set_max_map_size(LIMITS.max_map_size);
    e.set_strict_variables(true);
    e.set_fail_on_invalid_map_property(true);
    e.disable_symbol("eval");
    e.on_print(|_| {});
    e.on_debug(|_, _, _| {});

    // wiggle
    let w = |st: Shared| {
        move |freq: f64, amp: f64, oct: i64, mult: f64, t: Option<f64>| -> EvalResult<f64> {
            let s = lock(&st);
            let f = s.stack.last().ok_or("no expression is being evaluated")?;
            let site = &s.sites[f.site];
            let tt = t.unwrap_or_else(|| (f.t + site.expr.time_offset).to_f64());
            Ok(site.value_at(f.t) + amp * fractal(tt * freq, f.seed, oct, mult))
        }
    };
    let f = w(st.clone());
    e.register_fn("wiggle", move |a: Dynamic, b: Dynamic| -> EvalResult<f64> {
        f(
            num(&a, "wiggle freq")?,
            num(&b, "wiggle amp")?,
            1,
            0.5,
            None,
        )
    });
    let f = w(st.clone());
    e.register_fn(
        "wiggle",
        move |a: Dynamic, b: Dynamic, c: Dynamic| -> EvalResult<f64> {
            f(
                num(&a, "wiggle freq")?,
                num(&b, "wiggle amp")?,
                num(&c, "wiggle octaves")? as i64,
                0.5,
                None,
            )
        },
    );
    let f = w(st.clone());
    e.register_fn(
        "wiggle",
        move |a: Dynamic, b: Dynamic, c: Dynamic, d: Dynamic| -> EvalResult<f64> {
            f(
                num(&a, "wiggle freq")?,
                num(&b, "wiggle amp")?,
                num(&c, "wiggle octaves")? as i64,
                num(&d, "wiggle amp_mult")?,
                None,
            )
        },
    );
    let f = w(st.clone());
    e.register_fn(
        "wiggle",
        move |a: Dynamic, b: Dynamic, c: Dynamic, d: Dynamic, t: Dynamic| -> EvalResult<f64> {
            f(
                num(&a, "wiggle freq")?,
                num(&b, "wiggle amp")?,
                num(&c, "wiggle octaves")? as i64,
                num(&d, "wiggle amp_mult")?,
                Some(num(&t, "wiggle t")?),
            )
        },
    );

    // seed_random / random / noise
    let sr = |st: Shared| {
        move |n: f64, timeless: bool| -> EvalResult<()> {
            let mut s = lock(&st);
            let base = {
                let f = s.stack.last().ok_or("no expression is being evaluated")?;
                s.sites[f.site].seed
            };
            let f = s.stack.last_mut().expect("frame");
            f.seed = splitmix(base ^ splitmix(n.to_bits()));
            f.timeless = timeless;
            Ok(())
        }
    };
    let f = sr(st.clone());
    e.register_fn("seed_random", move |n: Dynamic| -> EvalResult<()> {
        f(num(&n, "seed_random n")?, false)
    });
    let f = sr(st.clone());
    e.register_fn(
        "seed_random",
        move |n: Dynamic, timeless: bool| -> EvalResult<()> {
            f(num(&n, "seed_random n")?, timeless)
        },
    );
    let rnd = |st: Shared| {
        move || -> EvalResult<f64> {
            let mut s = lock(&st);
            let fps = s.fps;
            let f = s
                .stack
                .last_mut()
                .ok_or("no expression is being evaluated")?;
            let frame = if f.timeless {
                0
            } else {
                RationalTime(f.t).frame_floor(fps)
            };
            f.calls += 1;
            Ok(unit(splitmix(
                f.seed ^ splitmix(frame as u64 ^ splitmix(f.calls)),
            )))
        }
    };
    let f = rnd(st.clone());
    e.register_fn("random", move || -> EvalResult<f64> { f() });
    let f = rnd(st.clone());
    e.register_fn("random", move |m: Dynamic| -> EvalResult<f64> {
        Ok(f()? * num(&m, "random max")?)
    });
    let f = rnd(st.clone());
    e.register_fn("random", move |a: Dynamic, b: Dynamic| -> EvalResult<f64> {
        let (a, b) = (num(&a, "random min")?, num(&b, "random max")?);
        Ok(a + f()? * (b - a))
    });
    e.register_fn("noise", |x: Dynamic| -> EvalResult<f64> {
        Ok(gnoise(num(&x, "noise x")?, 0x5eed).clamp(-1.0, 1.0))
    });

    // interpolation helpers
    for kind in ["linear", "ease", "ease_in", "ease_out"] {
        e.register_fn(
            kind,
            move |t: Dynamic,
                  t0: Dynamic,
                  t1: Dynamic,
                  v0: Dynamic,
                  v1: Dynamic|
                  -> EvalResult<f64> {
                Ok(interp5(
                    kind,
                    num(&t, "t")?,
                    num(&t0, "t_min")?,
                    num(&t1, "t_max")?,
                    num(&v0, "v1")?,
                    num(&v1, "v2")?,
                ))
            },
        );
        e.register_fn(
            kind,
            move |t: Dynamic, v0: Dynamic, v1: Dynamic| -> EvalResult<f64> {
                Ok(interp5(
                    kind,
                    num(&t, "t")?,
                    0.0,
                    1.0,
                    num(&v0, "v1")?,
                    num(&v1, "v2")?,
                ))
            },
        );
    }
    e.register_fn(
        "clamp",
        |x: Dynamic, lo: Dynamic, hi: Dynamic| -> EvalResult<f64> {
            let (x, lo, hi) = (num(&x, "x")?, num(&lo, "lo")?, num(&hi, "hi")?);
            if lo > hi {
                return Err(format!("clamp: lo {lo} is above hi {hi}").into());
            }
            Ok(x.clamp(lo, hi))
        },
    );
    e.register_fn(
        "lerp",
        |a: Dynamic, b: Dynamic, s: Dynamic| -> EvalResult<f64> {
            let (a, b, s) = (num(&a, "a")?, num(&b, "b")?, num(&s, "s")?);
            Ok(a + (b - a) * s)
        },
    );

    // value_at_time / loops
    let s2 = st.clone();
    e.register_fn("value_at_time", move |t: Dynamic| -> EvalResult<f64> {
        let t = num(&t, "value_at_time t")?;
        let s = lock(&s2);
        let f = s.stack.last().ok_or("no expression is being evaluated")?;
        let site = &s.sites[f.site];
        let r = to_rational(t).ok_or_else(|| format!("value_at_time: bad time {t}"))?;
        Ok(site.value_at(r - site.expr.time_offset))
    });
    let lp = |st: Shared, out: bool| {
        move |kind: &str, keys: i64| -> EvalResult<f64> {
            let s = lock(&st);
            let f = s.stack.last().ok_or("no expression is being evaluated")?;
            looped(&s.sites[f.site], f.t, kind, keys, out, s.fps)
        }
    };
    for (name, out) in [("loop_out", true), ("loop_in", false)] {
        let f = lp(st.clone(), out);
        e.register_fn(name, move || -> EvalResult<f64> { f("cycle", 0) });
        let f = lp(st.clone(), out);
        e.register_fn(name, move |k: &str| -> EvalResult<f64> { f(k, 0) });
        let f = lp(st.clone(), out);
        e.register_fn(name, move |k: &str, n: Dynamic| -> EvalResult<f64> {
            f(k, num(&n, "keys")? as i64)
        });
    }

    // references
    e.register_type_with_name::<LayerRef>("Layer");
    let s2 = st.clone();
    e.register_fn(
        "param",
        move |ctx: NativeCallContext, path: &str| -> EvalResult<f64> {
            let (oi, ct, _) = current(&s2)?;
            param_value(ctx.engine(), &s2, oi, path, ct)
        },
    );
    let s2 = st.clone();
    e.register_fn(
        "param",
        move |ctx: NativeCallContext, l: &mut LayerRef, path: &str| -> EvalResult<f64> {
            let (_, ct, _) = current(&s2)?;
            param_value(ctx.engine(), &s2, l.0, path, ct)
        },
    );
    let finder = |st: Shared, kinds: &'static [OwnerKind], what: &'static str| {
        move |id: &str| -> EvalResult<LayerRef> {
            let s = lock(&st);
            let hit = s.owners.iter().position(|o| {
                kinds.contains(&o.kind) && o.key.split_once(':').is_some_and(|(_, k)| k == id)
            });
            hit.map(LayerRef).ok_or_else(|| {
                let names: Vec<&str> = s
                    .owners
                    .iter()
                    .filter(|o| kinds.contains(&o.kind))
                    .filter_map(|o| o.key.split_once(':').map(|(_, k)| k))
                    .collect();
                format!(
                    "{what}({id:?}): no such {what} (known: {})",
                    names.join(", ")
                )
                .into()
            })
        }
    };
    let f = finder(
        st.clone(),
        &[OwnerKind::VideoClip, OwnerKind::AudioClip],
        "layer",
    );
    e.register_fn("layer", move |id: &str| f(id));
    let f = finder(
        st.clone(),
        &[OwnerKind::VideoTrack, OwnerKind::AudioTrack],
        "track",
    );
    e.register_fn("track", move |id: &str| f(id));
    let s2 = st.clone();
    e.register_fn("comp", move || -> EvalResult<LayerRef> {
        let s = lock(&s2);
        Ok(LayerRef(
            s.owners
                .iter()
                .position(|o| o.kind == OwnerKind::Timeline)
                .unwrap_or(0),
        ))
    });
    e
}

/// Find the expression sites under `v` (skipping `skip` keys at the top).
fn collect(v: &Value, path: &mut Vec<Seg>, skip: &[&str], out: &mut Vec<(Vec<Seg>, Value)>) {
    match v {
        Value::Object(o) if o.contains_key("expression") => out.push((path.clone(), v.clone())),
        Value::Object(o) => {
            for (k, x) in o {
                if path.is_empty() && skip.contains(&k.as_str()) {
                    continue;
                }
                path.push(Seg::Key(k.clone()));
                collect(x, path, &[], out);
                path.pop();
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(Seg::Idx(i));
                collect(x, path, &[], out);
                path.pop();
            }
        }
        _ => {}
    }
}

fn set_path(root: &mut Value, segs: &[Seg], new: Value) {
    let mut cur = root;
    for s in segs {
        cur = match s {
            Seg::Key(k) => &mut cur[k.as_str()],
            Seg::Idx(i) => &mut cur[*i],
        };
    }
    *cur = new;
}

fn rhai_error(e: &EvalAltResult) -> String {
    let pos = e.position();
    let mut msg = e.to_string();
    // rhai appends " (line L, position C)"; restate it as line/column.
    if let Some(i) = msg.rfind(" (line ") {
        msg.truncate(i);
    }
    if pos == Position::NONE {
        msg
    } else {
        format!(
            "line {}, column {}: {msg}",
            pos.line().unwrap_or(0),
            pos.position().unwrap_or(0)
        )
    }
}

fn time_desc(base: TimeBase, t: Rational, fps: Rational) -> String {
    let what = match base {
        TimeBase::ClipLocal => "clip time",
        TimeBase::Source => "source time",
        _ => "timeline time",
    };
    format!(
        "{what} {} s (frame {})",
        t,
        RationalTime(t).frame_floor(fps)
    )
}

/// Evaluate every expression in `tl` and replace it with keyframes (see the
/// module docs). Borrowed when the timeline has no expressions.
pub fn bake(tl: &Timeline) -> anyhow::Result<Cow<'_, Timeline>> {
    let root = serde_json::to_value(tl)?;
    let mut found = Vec::new();
    collect(&root, &mut Vec::new(), &[], &mut found);
    if found.is_empty() {
        return Ok(Cow::Borrowed(tl));
    }
    let fps = tl.output.fps;
    let frame = (tl.output.width, tl.output.height);
    let tl_dur = tl.duration().0;

    // Owners.
    let mut owners = Vec::new();
    let mut tl_json = root.clone();
    if let Some(o) = tl_json.as_object_mut() {
        o.remove("tracks");
        o.remove("audio_tracks");
    }
    owners.push(Owner {
        kind: OwnerKind::Timeline,
        label: "timeline".into(),
        key: "timeline:".into(),
        json: tl_json,
        pointer: vec![],
        start: Rational::ZERO,
        duration: tl_dur,
        time_map: crate::retime::TimeMap::new(
            RationalTime::ZERO,
            &Animatable::constant(Rational::ONE),
            None,
        ),
    });
    for (field, tkind, ckind) in [
        ("tracks", OwnerKind::VideoTrack, OwnerKind::VideoClip),
        ("audio_tracks", OwnerKind::AudioTrack, OwnerKind::AudioClip),
    ] {
        let Some(tracks) = root[field].as_array() else {
            continue;
        };
        for (ti, t) in tracks.iter().enumerate() {
            let name = t["name"].as_str().unwrap_or("").to_string();
            let mut tj = t.clone();
            if let Some(o) = tj.as_object_mut() {
                o.remove("clips");
            }
            owners.push(Owner {
                kind: tkind,
                label: format!("track {ti} ({name:?})"),
                key: format!("track:{name}"),
                json: tj,
                pointer: vec![Seg::Key(field.into()), Seg::Idx(ti)],
                start: Rational::ZERO,
                duration: tl_dur,
                time_map: crate::retime::TimeMap::new(
                    RationalTime::ZERO,
                    &Animatable::constant(Rational::ONE),
                    None,
                ),
            });
            for (ci, c) in t["clips"].as_array().into_iter().flatten().enumerate() {
                let id = c["id"].as_str().unwrap_or("").to_string();
                let r = |k: &str| -> Rational {
                    c[k].as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| c[k].as_i64().map(Rational::from_int))
                        .unwrap_or(Rational::ZERO)
                };
                owners.push(Owner {
                    kind: ckind,
                    label: format!("clip {id:?}"),
                    key: format!("clip:{id}"),
                    json: c.clone(),
                    pointer: vec![
                        Seg::Key(field.into()),
                        Seg::Idx(ti),
                        Seg::Key("clips".into()),
                        Seg::Idx(ci),
                    ],
                    start: r("start"),
                    duration: r("duration"),
                    time_map: if ckind == OwnerKind::VideoClip {
                        serde_json::from_value::<crate::timeline::Clip>(c.clone())?.time_map()
                    } else {
                        serde_json::from_value::<crate::timeline::AudioClip>(c.clone())?.time_map()
                    },
                });
            }
        }
    }

    // Sites.
    let mut sites = Vec::new();
    let mut by_path = HashMap::new();
    // Declares the variables for strict compiles. Not constants: the
    // optimizer would fold their placeholder values into the script.
    let mut template = Scope::new();
    for (n, _) in VARIABLES {
        if *n == "frame" {
            template.push(*n, 0i64);
        } else {
            template.push(*n, 0.0f64);
        }
    }
    let compiler = {
        let mut e = Engine::new();
        e.set_max_expr_depths(LIMITS.max_expr_depth, LIMITS.max_expr_depth);
        e.set_strict_variables(true);
        e.disable_symbol("eval");
        e
    };
    for (oi, o) in owners.iter().enumerate() {
        let skip: &[&str] = match o.kind {
            OwnerKind::Timeline => &["tracks", "audio_tracks"],
            OwnerKind::VideoTrack | OwnerKind::AudioTrack => &["clips"],
            _ => &[],
        };
        let mut here = Vec::new();
        collect(&o.json, &mut Vec::new(), skip, &mut here);
        for (rel, v) in here {
            let name = segs_name(&rel);
            let at = format!("{}: {name}", o.label);
            let expr: Expression = serde_json::from_value(v.clone()).map_err(|e| {
                anyhow!("{at}: bad expression object {v} ({e}); expected {{\"expression\": \"...\", \"value\": optional constant or keyframes}}")
            })?;
            expr.validate().map_err(|e| anyhow!("{at}: {e}"))?;
            let ast = compiler
                .compile_with_scope(&template, &expr.expression)
                .map_err(|e| {
                    let p = e.position();
                    anyhow!(
                        "{at}: expression syntax error at line {}, column {}: {}",
                        p.line().unwrap_or(0),
                        p.position().unwrap_or(0),
                        e.err_type()
                    )
                })?;
            let (spec, comp) = spec_of(o, &rel);
            if let Some(s) = &spec
                && !s.animatable
            {
                anyhow::bail!(
                    "{at}: {} is not animatable, so it cannot have an expression",
                    s.name
                );
            }
            let base = base_of(o, &rel, spec.as_ref());
            let seed = {
                let h = blake3::hash(format!("{}/{name}", o.key).as_bytes());
                u64::from_le_bytes(h.as_bytes()[..8].try_into().expect("8 bytes"))
            };
            by_path.insert((oi, rel.clone()), sites.len());
            sites.push(Site {
                owner: oi,
                rel,
                name,
                base,
                expr,
                ast: Arc::new(ast),
                default: default_of(spec.as_ref(), comp, frame),
                spec,
                seed,
            });
        }
    }

    // A source expression is a function of source time. Its samples must
    // follow the actual retime map, rather than assume speed = 1. Timing
    // expressions plus source expressions need a dependency-aware timing
    // pass; reject that combination until it can be evaluated faithfully.
    for site in &sites {
        if site.base == TimeBase::Source {
            let o = &owners[site.owner];
            anyhow::ensure!(
                !sites
                    .iter()
                    .any(|s| s.owner == site.owner
                        && matches!(s.name.as_str(), "speed" | "time_remap")),
                "{}: source expressions cannot yet be combined with expression-driven speed/time_remap; bake the timing curve to keyframes first",
                o.label
            );
        }
    }
    let st: Shared = Arc::new(Mutex::new(State {
        owners,
        sites,
        by_path,
        memo: HashMap::new(),
        stack: Vec::new(),
        fps,
        frame,
        tl_duration: tl_dur,
    }));
    let engine = build_engine(&st);
    let n_sites = lock(&st).sites.len();
    let mut out = root;
    let step = Rational::ONE / fps;
    for i in 0..n_sites {
        let (base, owner_start, owner_dur, time_map, at, pointer) = {
            let s = lock(&st);
            let site = &s.sites[i];
            let o = &s.owners[site.owner];
            let mut p = o.pointer.clone();
            p.extend(site.rel.iter().cloned());
            (
                site.base,
                o.start,
                o.duration,
                o.time_map.clone(),
                site_label(&s, i),
                p,
            )
        };
        let span = match base {
            TimeBase::ClipLocal | TimeBase::Source => owner_dur,
            _ => tl_dur,
        };
        let n = RationalTime(span).frame_ceil(fps).max(1);
        let mut keys = Vec::with_capacity(n as usize + 3);
        let mut previous_source = None;
        let mut source_direction = 0;
        // In-range frames first (so errors name a real frame), then the guards.
        for k in (0..=n).chain([-1, n + 1]) {
            let u = step * Rational::from_int(k);
            let (t, ct) = match base {
                TimeBase::Source => (time_map.source_at(RationalTime(u)).0, owner_start + u),
                TimeBase::ClipLocal => (u, owner_start + u),
                _ => (u, u),
            };
            // Remap curves hold outside their endpoints. A guard must not
            // replace a real sample at the same source time with a different
            // timeline-dependent value from outside the clip.
            if base == TimeBase::Source
                && (k < 0 || k > n)
                && keys.iter().any(|(time, _)| *time == t)
            {
                continue;
            }
            if base == TimeBase::Source && k >= 0 && k <= n {
                if let Some(previous) = previous_source {
                    let direction = if t > previous {
                        1
                    } else if t < previous {
                        -1
                    } else {
                        0
                    };
                    anyhow::ensure!(
                        direction == 0 || source_direction == 0 || direction == source_direction,
                        "{at}: source expressions require monotonic time remapping; keyframe source values for a remap that changes direction"
                    );
                    if direction != 0 {
                        source_direction = direction;
                    }
                }
                previous_source = Some(t);
            }
            let v = eval_site(&engine, &st, i, ct).map_err(|e| {
                anyhow!(
                    "{at}: expression error at {}: {}",
                    time_desc(base, t, fps),
                    rhai_error(&e)
                )
            })?;
            let mut v = v;
            {
                let s = lock(&st);
                if let Some(spec) = &s.sites[i].spec {
                    let bad = spec
                        .min
                        .filter(|m| v < *m)
                        .map(|m| ("below the minimum", m))
                        .or(spec
                            .max
                            .filter(|m| v > *m)
                            .map(|m| ("above the maximum", m)));
                    if let Some((what, lim)) = bad {
                        if k >= 0 && k <= n {
                            anyhow::bail!(
                                "{at}: the expression gives {v} at {}, {what} {lim}",
                                time_desc(base, t, fps)
                            );
                        }
                        // The guard samples outside the owner only shape
                        // sub-frame interpolation at its ends: clamp them.
                        v = lim;
                    }
                }
            }
            let r = to_rational(v).ok_or_else(|| {
                anyhow!(
                    "{at}: the expression gives {v} at {}, too large",
                    time_desc(base, t, fps)
                )
            })?;
            keys.push((t, r));
        }
        keys.sort_by_key(|(t, _)| *t);
        for pair in keys.windows(2) {
            anyhow::ensure!(
                pair[0].0 != pair[1].0 || pair[0].1 == pair[1].1,
                "{at}: source time {} maps to different expression values; a frozen/repeated source cannot represent changing comp_time or referenced timeline values",
                pair[0].0
            );
        }
        keys.dedup_by_key(|(t, _)| *t);
        let first = keys[0].1;
        let baked = if keys.iter().all(|(_, v)| *v == first) {
            json!(first.to_string())
        } else {
            let ks: Vec<Value> = keys
                .iter()
                .map(|(t, v)| json!({"t": t.to_string(), "v": v.to_string()}))
                .collect();
            json!({ "keyframes": ks })
        };
        set_path(&mut out, &pointer, baked);
    }
    let baked: Timeline = serde_json::from_value(out)
        .map_err(|e| anyhow!("baking expressions produced an invalid timeline: {e}"))?;
    Ok(Cow::Owned(baked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rationals_are_exact_decimals_when_possible() {
        assert_eq!(to_rational(0.1), Some(Rational::new(1, 10)));
        assert_eq!(to_rational(-2.5), Some(Rational::new(-5, 2)));
        assert_eq!(to_rational(3.0), Some(Rational::from_int(3)));
        let third = to_rational(1.0 / 3.0).unwrap();
        assert_eq!(third.den(), 1 << 30);
        assert!(to_rational(f64::NAN).is_none());
        assert!(to_rational(1e12).is_none());
    }

    #[test]
    fn noise_is_smooth_bounded_and_seeded() {
        let mut prev = gnoise(0.0, 7);
        assert_eq!(prev, 0.0);
        let mut max: f64 = 0.0;
        for i in 1..4000 {
            let x = i as f64 / 100.0;
            let v = gnoise(x, 7);
            assert!((v - prev).abs() < 0.1, "jump at {x}");
            max = max.max(v.abs());
            prev = v;
        }
        assert!(max <= 1.0 && max > 0.3, "{max}");
        assert_ne!(gnoise(0.5, 7), gnoise(0.5, 8));
        assert_eq!(gnoise(12.25, 7), gnoise(12.25, 7));
    }
}
