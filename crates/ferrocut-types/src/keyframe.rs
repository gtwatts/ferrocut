//! Keyframed parameters evaluated at exact [`RationalTime`].
//!
//! A parameter is an [`Animatable`]: either a constant or a [`KeyframeTrack`].
//! Key times and values are exact rationals (so timelines hash and diff
//! exactly); evaluation returns `f64` and is a pure, deterministic function of
//! the track and the time: no state, no fast-math, the same bits on every run.
//!
//! Each key's [`Interp`] describes the segment from that key to the next:
//! `hold`, `linear`, CSS-style cubic bezier (`bezier: [x1, y1, x2, y2]`, plus
//! the CSS presets `ease`, `ease_in`, `ease_out`, `ease_in_out`), or After
//! Effects-style temporal ease (`speed: {out_speed, out_influence, in_speed,
//! in_influence}`, speeds in value units per second, influences in (0, 1];
//! `easy_ease` is AE's Easy Ease: speed 0, influence 1/3 at both ends).
//! Before the first key the first value holds; after the last, the last.
//!
//! Exactness: at a key time the key's value is returned exactly
//! (`Rational::to_f64`), independent of interpolation. Bezier segments solve
//! `x(u) = s` by fixed 64-step bisection, which is monotone in `s`, so an
//! easing whose control values are monotone yields a monotone curve.
//!
//! Expressions: an [`Expression`] (`{"expression": "wiggle(2, 30)", "value":
//! ...}`) is an After Effects-style script over the parameter. This crate only
//! stores it (structural hash, shifting, validation of its shape); the engine
//! (`ferrocut_engine::expr`) evaluates it deterministically and bakes it into
//! a per-frame [`KeyframeTrack`] before anything renders. Evaluated unbaked,
//! an expression yields its `value` (or 0).

use serde::{Deserialize, Serialize};

use crate::time::{Rational, RationalTime};

/// A constant or keyframed parameter. JSON: `"1/2"`, `3`,
/// `{"keyframes": [{"t": "0", "v": "0", "interp": "ease_in_out"}, ...]}`, or
/// an expression `{"expression": "value + wiggle(2, 30)", "value": "960"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Animatable {
    Constant(Rational),
    Keyframes(KeyframeTrack),
    Expression(Expression),
}

/// Longest accepted expression source, in bytes.
pub const MAX_EXPRESSION_LEN: usize = 16 * 1024;

/// An expression over a parameter (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Expression {
    /// The script (the engine's expression language: rhai syntax).
    pub expression: String,
    /// The pre-expression value (`value` in the script): a constant or
    /// keyframes in the parameter's time base. Default: the parameter's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Box<Animatable>>,
    /// Added to the parameter time to get the script's `time` (a split moves
    /// the second half's clip-local origin; this keeps its expression in place).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub time_offset: Rational,
}

impl<'de> Deserialize<'de> for Expression {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A derived struct deserializer also accepts positional arrays. In an
        // untagged Animatable this misread ["1.5","0.75"] scale as a script
        // with a pre-expression value. The documented expression form is a map.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            expression: String,
            #[serde(default)]
            value: Option<Box<Animatable>>,
            #[serde(default)]
            time_offset: Rational,
        }
        struct MapOnly;
        impl<'de> serde::de::Visitor<'de> for MapOnly {
            type Value = Expression;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(
                    "an expression object with expression, optional value and time_offset",
                )
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                map: M,
            ) -> Result<Expression, M::Error> {
                let f = Fields::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(Expression {
                    expression: f.expression,
                    value: f.value,
                    time_offset: f.time_offset,
                })
            }
        }
        deserializer.deserialize_map(MapOnly)
    }
}

fn is_zero(r: &Rational) -> bool {
    r.is_zero()
}

impl Expression {
    pub fn validate(&self) -> Result<(), String> {
        if self.expression.trim().is_empty() {
            return Err("expression is empty".into());
        }
        if self.expression.len() > MAX_EXPRESSION_LEN {
            return Err(format!(
                "expression is {} bytes; the limit is {MAX_EXPRESSION_LEN}",
                self.expression.len()
            ));
        }
        match self.value.as_deref() {
            Some(Animatable::Expression(_)) => Err(
                "an expression's value must be a constant or keyframes, not another expression"
                    .into(),
            ),
            Some(v) => v.validate().map_err(|e| format!("value: {e}")),
            None => Ok(()),
        }
    }
}

impl Default for Animatable {
    fn default() -> Self {
        Animatable::Constant(Rational::ZERO)
    }
}

impl From<Rational> for Animatable {
    fn from(v: Rational) -> Self {
        Animatable::Constant(v)
    }
}

impl Animatable {
    pub const fn constant(v: Rational) -> Self {
        Animatable::Constant(v)
    }

    /// The value if it never changes (a constant, or a track whose keys are all equal).
    pub fn as_constant(&self) -> Option<Rational> {
        match self {
            Animatable::Constant(v) => Some(*v),
            Animatable::Keyframes(k) => {
                let v = k.keyframes.first()?.v;
                k.keyframes.iter().all(|kf| kf.v == v).then_some(v)
            }
            Animatable::Expression(_) => None,
        }
    }

    pub fn is_expression(&self) -> bool {
        matches!(self, Animatable::Expression(_))
    }

    pub fn is_animated(&self) -> bool {
        self.as_constant().is_none()
    }

    pub fn eval(&self, t: RationalTime) -> f64 {
        match self {
            Animatable::Constant(v) => v.to_f64(),
            Animatable::Keyframes(k) => k.eval(t),
            Animatable::Expression(e) => e.value.as_ref().map_or(0.0, |v| v.eval(t)),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Animatable::Constant(_) => Ok(()),
            Animatable::Keyframes(k) => k.validate(),
            Animatable::Expression(e) => e.validate(),
        }
    }

    /// Smallest and largest key value (the constant for a constant). Bezier
    /// segments may overshoot these; callers clamp where the range matters.
    /// An expression reports its `value`'s range (0 without one): its real
    /// range is only known once the engine evaluates it.
    pub fn key_range(&self) -> (Rational, Rational) {
        match self {
            Animatable::Constant(v) => (*v, *v),
            Animatable::Keyframes(k) => {
                let mut it = k.keyframes.iter().map(|kf| kf.v);
                let first = it.next().unwrap_or_default();
                it.fold((first, first), |(lo, hi), v| (lo.min(v), hi.max(v)))
            }
            Animatable::Expression(e) => e
                .value
                .as_ref()
                .map_or((Rational::ZERO, Rational::ZERO), |v| v.key_range()),
        }
    }

    /// Same curve with every key time shifted by `dt` (e.g. after a split the
    /// second half's local time starts later).
    pub fn shifted(&self, dt: Rational) -> Animatable {
        match self {
            Animatable::Constant(v) => Animatable::Constant(*v),
            Animatable::Keyframes(k) => Animatable::Keyframes(KeyframeTrack {
                keyframes: k
                    .keyframes
                    .iter()
                    .map(|kf| Keyframe {
                        t: RationalTime(kf.t.0 + dt),
                        ..kf.clone()
                    })
                    .collect(),
            }),
            Animatable::Expression(e) => Animatable::Expression(Expression {
                expression: e.expression.clone(),
                value: e.value.as_ref().map(|v| Box::new(v.shifted(dt))),
                time_offset: e.time_offset - dt,
            }),
        }
    }

    /// Stable structural hash input (for node content hashes).
    pub fn hash_into(&self, h: &mut blake3::Hasher) {
        match self {
            Animatable::Constant(v) => {
                h.update(b"c");
                h.update(&v.hash_bytes());
            }
            Animatable::Keyframes(k) => {
                h.update(b"k");
                h.update(&(k.keyframes.len() as u64).to_le_bytes());
                for kf in &k.keyframes {
                    h.update(&kf.t.hash_bytes());
                    h.update(&kf.v.hash_bytes());
                    kf.interp.hash_into(h);
                }
            }
            Animatable::Expression(e) => {
                h.update(b"x");
                h.update(&(e.expression.len() as u64).to_le_bytes());
                h.update(e.expression.as_bytes());
                h.update(&e.time_offset.hash_bytes());
                match &e.value {
                    Some(v) => {
                        h.update(b"v");
                        v.hash_into(h);
                    }
                    None => {
                        h.update(b"-");
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyframeTrack {
    pub keyframes: Vec<Keyframe>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframe {
    /// Key time (the owner defines the origin: clip-local or timeline time).
    pub t: RationalTime,
    pub v: Rational,
    /// Interpolation of the segment from this key to the next.
    #[serde(default, skip_serializing_if = "Interp::is_linear")]
    pub interp: Interp,
}

/// After Effects-style temporal ease for one segment (both ends on the
/// segment's first key). Speeds are value units per second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedInfluence {
    pub out_speed: Rational,
    pub out_influence: Rational,
    pub in_speed: Rational,
    pub in_influence: Rational,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    Hold,
    #[default]
    Linear,
    /// CSS `ease` = cubic-bezier(0.25, 0.1, 0.25, 1).
    Ease,
    /// CSS `ease-in` = cubic-bezier(0.42, 0, 1, 1).
    EaseIn,
    /// CSS `ease-out` = cubic-bezier(0, 0, 0.58, 1).
    EaseOut,
    /// CSS `ease-in-out` = cubic-bezier(0.42, 0, 0.58, 1).
    EaseInOut,
    /// After Effects Easy Ease: speed 0, influence 1/3 at both ends.
    EasyEase,
    /// CSS cubic-bezier control points `[x1, y1, x2, y2]`, `x` in [0, 1].
    Bezier([Rational; 4]),
    Speed(SpeedInfluence),
}

impl Interp {
    pub fn is_linear(&self) -> bool {
        *self == Interp::Linear
    }

    /// Normalized CSS control points for the bezier presets.
    pub fn css_points(&self) -> Option<[Rational; 4]> {
        let r = Rational::new;
        Some(match self {
            Interp::Ease => [r(1, 4), r(1, 10), r(1, 4), r(1, 1)],
            Interp::EaseIn => [r(21, 50), r(0, 1), r(1, 1), r(1, 1)],
            Interp::EaseOut => [r(0, 1), r(0, 1), r(29, 50), r(1, 1)],
            Interp::EaseInOut => [r(21, 50), r(0, 1), r(29, 50), r(1, 1)],
            Interp::Bezier(p) => *p,
            _ => return None,
        })
    }

    fn hash_into(&self, h: &mut blake3::Hasher) {
        let tag: u8 = match self {
            Interp::Hold => 0,
            Interp::Linear => 1,
            Interp::Ease => 2,
            Interp::EaseIn => 3,
            Interp::EaseOut => 4,
            Interp::EaseInOut => 5,
            Interp::EasyEase => 6,
            Interp::Bezier(_) => 7,
            Interp::Speed(_) => 8,
        };
        h.update(&[tag]);
        match self {
            Interp::Bezier(p) => p.iter().for_each(|v| {
                h.update(&v.hash_bytes());
            }),
            Interp::Speed(s) => {
                for v in [s.out_speed, s.out_influence, s.in_speed, s.in_influence] {
                    h.update(&v.hash_bytes());
                }
            }
            _ => {}
        }
    }
}

impl KeyframeTrack {
    pub fn validate(&self) -> Result<(), String> {
        let k = &self.keyframes;
        if k.is_empty() {
            return Err("keyframe track has no keyframes".into());
        }
        for w in k.windows(2) {
            if w[1].t <= w[0].t {
                return Err(format!(
                    "keyframe times must strictly increase ({} then {})",
                    w[0].t, w[1].t
                ));
            }
        }
        let unit = |v: Rational| v >= Rational::ZERO && v <= Rational::ONE;
        for kf in k {
            match kf.interp {
                Interp::Bezier([x1, _, x2, _]) if !unit(x1) || !unit(x2) => {
                    return Err(format!(
                        "keyframe at {}: bezier x1/x2 must be in [0, 1] (got {x1}, {x2})",
                        kf.t
                    ));
                }
                Interp::Speed(s)
                    if !(s.out_influence > Rational::ZERO && unit(s.out_influence))
                        || !(s.in_influence > Rational::ZERO && unit(s.in_influence)) =>
                {
                    return Err(format!(
                        "keyframe at {}: influences must be in (0, 1] (got {}, {})",
                        kf.t, s.out_influence, s.in_influence
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn eval(&self, t: RationalTime) -> f64 {
        let k = &self.keyframes;
        let Some(first) = k.first() else { return 0.0 };
        if t <= first.t {
            return first.v.to_f64();
        }
        let i = k.partition_point(|kf| kf.t <= t);
        if i == k.len() {
            return k[i - 1].v.to_f64();
        }
        let (a, b) = (&k[i - 1], &k[i]);
        if a.t == t {
            return a.v.to_f64();
        }
        let s = (t.0 - a.t.0)
            .checked_div(b.t.0 - a.t.0)
            .map(Rational::to_f64)
            .unwrap_or_else(|_| {
                (t.0.to_f64() - a.t.0.to_f64()) / (b.t.0.to_f64() - a.t.0.to_f64())
            });
        segment(a, b, s)
    }
}

/// Value of the segment `a -> b` at normalized time `s` in (0, 1).
fn segment(a: &Keyframe, b: &Keyframe, s: f64) -> f64 {
    let (v0, v1) = (a.v.to_f64(), b.v.to_f64());
    match a.interp {
        Interp::Hold => v0,
        Interp::Linear => v0 + (v1 - v0) * s,
        Interp::EasyEase => {
            let third = 1.0 / 3.0;
            let u = solve_u(third, 1.0 - third, s);
            bezier(v0, v0, v1, v1, u)
        }
        Interp::Speed(si) => {
            let d = (b.t.0 - a.t.0).to_f64();
            let (oi, ii) = (si.out_influence.to_f64(), si.in_influence.to_f64());
            let y1 = v0 + si.out_speed.to_f64() * oi * d;
            let y2 = v1 - si.in_speed.to_f64() * ii * d;
            let u = solve_u(oi, 1.0 - ii, s);
            bezier(v0, y1, y2, v1, u)
        }
        ref css => {
            let [x1, y1, x2, y2] = css.css_points().expect("bezier preset");
            let u = solve_u(x1.to_f64(), x2.to_f64(), s);
            let y = bezier(0.0, y1.to_f64(), y2.to_f64(), 1.0, u);
            v0 + (v1 - v0) * y
        }
    }
}

/// Cubic Bernstein polynomial with control values `p0..p3` at `u`.
fn bezier(p0: f64, p1: f64, p2: f64, p3: f64, u: f64) -> f64 {
    let m = 1.0 - u;
    m * m * m * p0 + 3.0 * m * m * u * p1 + 3.0 * m * u * u * p2 + u * u * u * p3
}

/// `u` with `x(u) = s` for the x-curve `0, x1, x2, 1` (monotone for x1, x2 in
/// [0, 1]): fixed 64-step bisection, deterministic and monotone in `s`.
fn solve_u(x1: f64, x2: f64, s: f64) -> f64 {
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        if bezier(0.0, x1, x2, 1.0, mid) < s {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> Rational {
        s.parse().unwrap()
    }
    fn t(s: &str) -> RationalTime {
        RationalTime(r(s))
    }
    fn track(keys: &[(&str, &str, Interp)]) -> Animatable {
        Animatable::Keyframes(KeyframeTrack {
            keyframes: keys
                .iter()
                .map(|(tt, v, i)| Keyframe {
                    t: t(tt),
                    v: r(v),
                    interp: *i,
                })
                .collect(),
        })
    }
    const ALL: [Interp; 7] = [
        Interp::Hold,
        Interp::Linear,
        Interp::Ease,
        Interp::EaseIn,
        Interp::EaseOut,
        Interp::EaseInOut,
        Interp::EasyEase,
    ];

    #[test]
    fn exact_at_keyframes_for_every_interpolation() {
        let speed = Interp::Speed(SpeedInfluence {
            out_speed: r("3"),
            out_influence: r("1/2"),
            in_speed: r("-2"),
            in_influence: r("1/4"),
        });
        let bez = Interp::Bezier([r("1/10"), r("7/5"), r("9/10"), r("-1/2")]);
        for i in ALL.into_iter().chain([speed, bez]) {
            let a = track(&[("1/3", "1/10", i), ("2", "-7/3", i), ("5/2", "9", i)]);
            a.validate().unwrap();
            assert_eq!(a.eval(t("1/3")), r("1/10").to_f64(), "{i:?}");
            assert_eq!(a.eval(t("2")), r("-7/3").to_f64(), "{i:?}");
            assert_eq!(a.eval(t("5/2")), 9.0, "{i:?}");
            // Holds outside the keyed range.
            assert_eq!(a.eval(t("-4")), r("1/10").to_f64());
            assert_eq!(a.eval(t("100")), 9.0);
        }
    }

    #[test]
    fn linear_hold_and_presets() {
        let lin = track(&[("0", "0", Interp::Linear), ("2", "10", Interp::Linear)]);
        assert_eq!(lin.eval(t("1/2")), 2.5);
        let hold = track(&[("0", "0", Interp::Hold), ("2", "10", Interp::Linear)]);
        assert_eq!(hold.eval(t("1999/1000")), 0.0);
        // ease-in-out is symmetric: halfway in time is halfway in value.
        let e = track(&[("0", "0", Interp::EaseInOut), ("1", "1", Interp::Linear)]);
        assert!((e.eval(t("1/2")) - 0.5).abs() < 1e-12);
        // ease-in starts slow, ease-out starts fast.
        let ei = track(&[("0", "0", Interp::EaseIn), ("1", "1", Interp::Linear)]);
        let eo = track(&[("0", "0", Interp::EaseOut), ("1", "1", Interp::Linear)]);
        assert!(ei.eval(t("1/4")) < 0.25 && eo.eval(t("1/4")) > 0.25);
        // CSS `ease` at 0.5 is ~0.8024 (reference value of cubic-bezier(.25,.1,.25,1)).
        let ease = track(&[("0", "0", Interp::Ease), ("1", "1", Interp::Linear)]);
        assert!((ease.eval(t("1/2")) - 0.8024).abs() < 1e-3);
        // AE speed/influence with speed = linear slope and influence 1/3 is linear.
        let si = Interp::Speed(SpeedInfluence {
            out_speed: r("5"),
            out_influence: r("1/3"),
            in_speed: r("5"),
            in_influence: r("1/3"),
        });
        let s = track(&[("0", "0", si), ("2", "10", Interp::Linear)]);
        for k in 1..20 {
            let tt = RationalTime::new(k, 10);
            assert!((s.eval(tt) - 5.0 * tt.0.to_f64()).abs() < 1e-9);
        }
    }

    #[test]
    fn easing_is_monotone() {
        for i in ALL {
            let a = track(&[("0", "-3", i), ("7/3", "11/2", Interp::Linear)]);
            let mut prev = f64::NEG_INFINITY;
            for k in 0..=7000 {
                let v = a.eval(RationalTime::new(k, 3000));
                assert!(v >= prev, "{i:?} not monotone at {k}: {v} < {prev}");
                prev = v;
            }
        }
    }

    #[test]
    fn deterministic_and_hash_stable() {
        let a = track(&[("0", "0", Interp::EaseInOut), ("1", "1", Interp::Linear)]);
        let b = a.clone();
        for k in 0..100 {
            let tt = RationalTime::new(k, 97);
            assert_eq!(a.eval(tt).to_bits(), b.eval(tt).to_bits());
        }
        let h = |x: &Animatable| {
            let mut h = blake3::Hasher::new();
            x.hash_into(&mut h);
            h.finalize()
        };
        assert_eq!(h(&a), h(&b));
        assert_ne!(h(&a), h(&a.shifted(r("1"))));
        assert_eq!(a.shifted(r("1")).eval(t("3/2")), a.eval(t("1/2")));
    }

    #[test]
    fn json_forms_and_validation() {
        let a: Animatable = serde_json::from_str(r#""1/2""#).unwrap();
        assert_eq!(a.as_constant(), Some(r("1/2")));
        let a: Animatable = serde_json::from_str(
            r#"{"keyframes": [
                {"t": "0", "v": "0", "interp": "ease_in_out"},
                {"t": 1, "v": "1", "interp": {"bezier": ["0.42", "0", "1", "1"]}},
                {"t": 2, "v": "0", "interp": {"speed": {"out_speed": "0", "out_influence": "1/3", "in_speed": "0", "in_influence": "1/3"}}},
                {"t": 3, "v": "1", "interp": "hold"}
            ]}"#,
        )
        .unwrap();
        a.validate().unwrap();
        assert!(a.is_animated());
        let back: Animatable = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(a, back);
        let bad: Animatable =
            serde_json::from_str(r#"{"keyframes": [{"t": "1", "v": "0"}, {"t": "1", "v": "1"}]}"#)
                .unwrap();
        assert!(bad.validate().unwrap_err().contains("strictly increase"));
        let bad: Animatable = serde_json::from_str(
            r#"{"keyframes": [{"t": "0", "v": "0", "interp": {"bezier": ["3/2", "0", "1", "1"]}}]}"#,
        )
        .unwrap();
        assert!(bad.validate().is_err());
        assert!(
            serde_json::from_str::<Animatable>(r#"{"keyframes": [{"t": 0, "v": 0, "x": 1}]}"#)
                .is_err()
        );
    }

    #[test]
    fn expressions_roundtrip_shift_and_hash() {
        let a: Animatable =
            serde_json::from_str(r#"{"expression": "wiggle(2, 30)", "value": "960"}"#).unwrap();
        a.validate().unwrap();
        assert!(a.is_expression() && a.is_animated() && a.as_constant().is_none());
        // Unbaked, an expression evaluates as its value.
        assert_eq!(a.eval(t("1")), 960.0);
        assert_eq!(a.key_range(), (r("960"), r("960")));
        let back: Animatable = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(a, back);
        let s = a.shifted(r("-1"));
        let Animatable::Expression(e) = &s else {
            panic!()
        };
        assert_eq!(e.time_offset, r("1"));
        assert!(
            serde_json::to_string(&s)
                .unwrap()
                .contains(r#""time_offset":"1""#)
        );
        let h = |x: &Animatable| {
            let mut h = blake3::Hasher::new();
            x.hash_into(&mut h);
            h.finalize()
        };
        assert_ne!(h(&a), h(&s));
        let b: Animatable =
            serde_json::from_str(r#"{"expression": "wiggle(2, 31)", "value": "960"}"#).unwrap();
        assert_ne!(h(&a), h(&b));
        for bad in [
            r#"{"expression": " "}"#,
            r#"{"expression": "1", "value": {"expression": "2"}}"#,
        ] {
            let x: Animatable = serde_json::from_str(bad).unwrap();
            assert!(x.validate().is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<Animatable>(r#"{"expression": "1", "x": 1}"#).is_err());
    }
}
