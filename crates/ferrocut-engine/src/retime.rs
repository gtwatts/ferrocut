//! Clip time remapping: constant and reverse speed, keyframed speed ramps,
//! After Effects-style time-remap curves and freeze frames, plus the frame
//! sampling mode used when the source time falls between source frames.
//!
//! A clip maps its local time `u` (seconds from `start`) to a source time:
//!
//! - default (`speed` 1): `source_in + u` (unchanged from before retiming).
//! - constant `speed` s: `source_in + s·u`, exact. `s < 0` plays backwards
//!   from `source_in`, `s = 0` holds the frame at `source_in` (freeze).
//! - keyframed `speed` (clip-local key times): `source_in + ∫₀ᵘ speed`.
//!   Hold and linear segments integrate in closed form; eased/bezier ones
//!   with a fixed 32-interval Simpson rule. Speed holds its first/last value
//!   outside the keys.
//! - `time_remap` (AE "Time Remap"): keys map clip-local time straight to a
//!   source time in seconds (`source_in` and `speed` are then ignored).
//!
//! Non-constant maps are evaluated in f64 at exact (2⁻²⁰ s grid) sample
//! points and rounded back to that grid, so every result is a deterministic
//! exact rational. Audio evaluates the same function (see
//! [`TimeMap::source_seconds`]), so picture and sound stay in sync.

use ferrocut_core::{Animatable, FrameRate, Interp, Rational, RationalTime};
use serde::{Deserialize, Serialize};

/// Grid (per second) that non-constant source times are rounded to.
pub const GRID: i64 = 1 << 20;

/// Simpson intervals per eased segment (even).
const SIMPSON: usize = 32;

/// How a clip samples its source when the source time is between frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sampling {
    /// The source frame nearest the source time (no blending).
    #[default]
    Nearest,
    /// Linear blend of the two source frames around the source time
    /// (premultiplied linear light), weighted by the fractional position.
    FrameBlend,
    /// Reserved hook for motion-compensated interpolation (a future node
    /// pulls the same two frames as `frame_blend` plus flow fields).
    /// Rejected by validation until that node exists.
    OpticalFlow,
}

impl Sampling {
    pub fn is_default(&self) -> bool {
        *self == Sampling::Nearest
    }
    fn tag(self) -> &'static [u8] {
        match self {
            Sampling::Nearest => b"nearest",
            Sampling::FrameBlend => b"frame_blend",
            Sampling::OpticalFlow => b"optical_flow",
        }
    }
}

/// A clip's local-time -> source-time function.
#[derive(Clone, Debug, PartialEq)]
pub enum TimeMap {
    /// `source_in + speed·u` (exact). Speed 1 is the untouched default.
    Linear {
        source_in: Rational,
        speed: Rational,
    },
    /// `source_in + G(u) - G(0)` with `G(u) = ∫_{t₀}^u speed` from the
    /// first key `t₀`; `prefix[i] = G(tᵢ)`, `base = G(0)`.
    Ramp {
        source_in: Rational,
        speed: Animatable,
        prefix: Vec<f64>,
        base: f64,
    },
    /// Source seconds straight from the curve.
    Remap { curve: Animatable },
}

fn q(x: f64) -> Rational {
    Rational::new((x * GRID as f64).round() as i64, GRID)
}

impl TimeMap {
    pub fn new(source_in: RationalTime, speed: &Animatable, remap: Option<&Animatable>) -> Self {
        if let Some(c) = remap {
            return TimeMap::Remap { curve: c.clone() };
        }
        match speed {
            // Expressions are baked before compile; unbaked, use the value.
            Animatable::Expression(e) => {
                let one = Animatable::Constant(Rational::ONE);
                TimeMap::new(source_in, e.value.as_deref().unwrap_or(&one), None)
            }
            Animatable::Constant(s) => TimeMap::Linear {
                source_in: source_in.0,
                speed: *s,
            },
            Animatable::Keyframes(k) => {
                let mut prefix = vec![0.0f64];
                let keys = &k.keyframes;
                let mut acc = 0.0;
                for w in keys.windows(2) {
                    acc +=
                        segment_integral(speed, w[0].t.0, w[1].t.0, &w[0].interp, w[0].v, w[1].v);
                    prefix.push(acc);
                }
                let mut m = TimeMap::Ramp {
                    source_in: source_in.0,
                    speed: speed.clone(),
                    prefix,
                    base: 0.0,
                };
                let g0 = m.integral(Rational::ZERO);
                if let TimeMap::Ramp { base, .. } = &mut m {
                    *base = g0;
                }
                m
            }
        }
    }

    /// Untouched default mapping (`source_in + u`).
    pub fn is_identity(&self) -> bool {
        matches!(self, TimeMap::Linear { speed, .. } if *speed == Rational::ONE)
    }

    /// Exact constant speed, if any.
    pub fn constant_speed(&self) -> Option<Rational> {
        match self {
            TimeMap::Linear { speed, .. } => Some(*speed),
            _ => None,
        }
    }

    /// Source time at clip-local time `u`.
    pub fn source_at(&self, u: RationalTime) -> RationalTime {
        match self {
            TimeMap::Linear { source_in, speed } => RationalTime(*source_in + *speed * u.0),
            _ => RationalTime(q(self.source_seconds(u.0))),
        }
    }

    /// Source time (seconds, f64) at clip-local time `u` (exact for the
    /// linear map up to the final f64 conversion).
    pub fn source_seconds(&self, u: Rational) -> f64 {
        match self {
            TimeMap::Linear { source_in, speed } => (*source_in + *speed * u).to_f64(),
            TimeMap::Remap { curve } => curve.eval(RationalTime(u)),
            TimeMap::Ramp {
                source_in, base, ..
            } => source_in.to_f64() + (self.integral(u) - base),
        }
    }

    /// `G(u)`: ∫ speed from the first key to `u` (negative before it).
    fn integral(&self, u: Rational) -> f64 {
        let TimeMap::Ramp { speed, prefix, .. } = self else {
            return 0.0;
        };
        let Animatable::Keyframes(k) = speed else {
            unreachable!("ramp has keys")
        };
        let keys = &k.keyframes;
        let first = &keys[0];
        if u <= first.t.0 {
            return first.v.to_f64() * (u - first.t.0).to_f64();
        }
        let i = keys.partition_point(|kf| kf.t.0 <= u) - 1;
        let a = &keys[i];
        let part = match keys.get(i + 1) {
            None => a.v.to_f64() * (u - a.t.0).to_f64(),
            Some(b) => segment_partial(speed, a.t.0, b.t.0, u, &a.interp, a.v, b.v),
        };
        prefix[i] + part
    }

    /// Smallest and largest source time over clip-local `[u0, u1]`
    /// (keys, ends and 256 interior samples: eased curves may overshoot).
    pub fn source_range(&self, u0: Rational, u1: Rational) -> (f64, f64) {
        let mut pts: Vec<Rational> = vec![u0, u1];
        if let TimeMap::Ramp { speed: a, .. } | TimeMap::Remap { curve: a } = self {
            if let Animatable::Keyframes(k) = a {
                pts.extend(
                    k.keyframes
                        .iter()
                        .map(|kf| kf.t.0)
                        .filter(|t| *t > u0 && *t < u1),
                );
            }
            let (f0, f1) = (u0.to_f64(), u1.to_f64());
            pts.extend((1..256).map(|i| q(f0 + (f1 - f0) * i as f64 / 256.0)));
        }
        pts.iter()
            .map(|&u| self.source_seconds(u))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            })
    }

    /// Hash input; empty for the identity map so existing keys don't change.
    pub fn hash_bytes(&self) -> Vec<u8> {
        let mut h = blake3::Hasher::new();
        match self {
            TimeMap::Linear { speed, .. } if *speed == Rational::ONE => return Vec::new(),
            TimeMap::Linear { source_in, speed } => {
                h.update(b"retime.linear");
                h.update(&source_in.hash_bytes());
                h.update(&speed.hash_bytes());
            }
            TimeMap::Ramp {
                source_in, speed, ..
            } => {
                h.update(b"retime.ramp");
                h.update(&source_in.hash_bytes());
                speed.hash_into(&mut h);
            }
            TimeMap::Remap { curve } => {
                h.update(b"retime.remap");
                curve.hash_into(&mut h);
            }
        }
        h.finalize().as_bytes().to_vec()
    }
}

/// The same curve with every key value moved by `dv` (slipping a remap curve).
pub fn shift_values(a: &Animatable, dv: Rational) -> Animatable {
    match a {
        Animatable::Constant(v) => Animatable::Constant(*v + dv),
        Animatable::Keyframes(k) => {
            let mut k = k.clone();
            for kf in &mut k.keyframes {
                kf.v = kf.v + dv;
            }
            Animatable::Keyframes(k)
        }
        Animatable::Expression(e) => Animatable::Expression(ferrocut_core::Expression {
            expression: format!("{{\n{}\n}} + {}.0 / {}.0", e.expression, dv.num(), dv.den()),
            // The result moves; `value` (the script's input) stays.
            value: e.value.clone(),
            time_offset: e.time_offset,
        }),
    }
}

/// ∫ speed over a whole key segment `[t0, t1]`.
fn segment_integral(
    a: &Animatable,
    t0: Rational,
    t1: Rational,
    interp: &Interp,
    v0: Rational,
    v1: Rational,
) -> f64 {
    let dt = (t1 - t0).to_f64();
    match interp {
        Interp::Hold => v0.to_f64() * dt,
        Interp::Linear => 0.5 * (v0.to_f64() + v1.to_f64()) * dt,
        _ => simpson(a, t0.to_f64(), t1.to_f64()),
    }
}

/// ∫ speed over `[t0, u]` inside the segment `[t0, t1]`.
fn segment_partial(
    a: &Animatable,
    t0: Rational,
    t1: Rational,
    u: Rational,
    interp: &Interp,
    v0: Rational,
    v1: Rational,
) -> f64 {
    let d = (u - t0).to_f64();
    match interp {
        Interp::Hold => v0.to_f64() * d,
        Interp::Linear => {
            let s = d / (t1 - t0).to_f64();
            let vu = v0.to_f64() + (v1.to_f64() - v0.to_f64()) * s;
            0.5 * (v0.to_f64() + vu) * d
        }
        _ => simpson(a, t0.to_f64(), u.to_f64()),
    }
}

fn simpson(a: &Animatable, x0: f64, x1: f64) -> f64 {
    let h = (x1 - x0) / SIMPSON as f64;
    let f = |i: usize| a.eval(RationalTime(q(x0 + h * i as f64)));
    let mut s = f(0) + f(SIMPSON);
    for i in 1..SIMPSON {
        s += f(i) * if i % 2 == 1 { 4.0 } else { 2.0 };
    }
    s * h / 3.0
}

/// What the clip node pulls from its source at one output time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sample {
    One(RationalTime),
    /// `(a, b, w)`: blend `a·(1-w) + b·w`, `0 < w < 1`.
    Blend(RationalTime, RationalTime, Rational),
}

/// Pick the source frame(s) for source time `s` (already mapped). With a
/// known source frame rate, nearest snaps to the frame grid (so held and
/// repeated frames share cache keys) and frame blend splits between the
/// two neighbouring frames; without one both pull `s` as is.
pub fn sample(s: RationalTime, fps: Option<FrameRate>, mode: Sampling) -> Sample {
    let Some(fps) = fps else {
        return Sample::One(s);
    };
    match mode {
        Sampling::Nearest => Sample::One(RationalTime::from_frames(s.frame_round(fps), fps)),
        Sampling::FrameBlend | Sampling::OpticalFlow => {
            let x = s.0 * fps;
            let f0 = x.floor();
            let w = x - Rational::from_int(f0);
            if w.is_zero() {
                Sample::One(RationalTime::from_frames(f0, fps))
            } else {
                Sample::Blend(
                    RationalTime::from_frames(f0, fps),
                    RationalTime::from_frames(f0 + 1, fps),
                    w,
                )
            }
        }
    }
}

impl Sample {
    pub fn hash_tag(&self, mode: Sampling) -> Vec<u8> {
        let mut v = mode.tag().to_vec();
        if let Sample::Blend(_, _, w) = self {
            v.extend_from_slice(&w.hash_bytes());
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrocut_core::{Keyframe, KeyframeTrack};

    fn r(n: i64, d: i64) -> Rational {
        Rational::new(n, d)
    }
    fn keys(k: &[(i64, i64, Interp)]) -> Animatable {
        Animatable::Keyframes(KeyframeTrack {
            keyframes: k
                .iter()
                .map(|&(t, v, interp)| Keyframe {
                    t: RationalTime(Rational::from_int(t)),
                    v: Rational::from_int(v),
                    interp,
                })
                .collect(),
        })
    }

    #[test]
    fn constant_reverse_and_freeze_are_exact() {
        let si = RationalTime(r(10, 1));
        let m = TimeMap::new(si, &Animatable::constant(r(2, 1)), None);
        assert_eq!(m.source_at(RationalTime(r(3, 2))), RationalTime(r(13, 1)));
        let m = TimeMap::new(si, &Animatable::constant(r(-1, 1)), None);
        assert_eq!(m.source_at(RationalTime(r(4, 1))), RationalTime(r(6, 1)));
        let m = TimeMap::new(si, &Animatable::constant(Rational::ZERO), None);
        assert_eq!(m.source_at(RationalTime(r(4, 1))), si);
        assert!(TimeMap::new(si, &Animatable::constant(Rational::ONE), None).is_identity());
        assert!(
            TimeMap::new(si, &Animatable::constant(Rational::ONE), None)
                .hash_bytes()
                .is_empty()
        );
    }

    #[test]
    fn linear_ramp_integrates_exactly() {
        // speed 1 -> 3 over [0, 2], then 3: source = u + u²/2 on [0, 2].
        let m = TimeMap::new(
            RationalTime::ZERO,
            &keys(&[(0, 1, Interp::Linear), (2, 3, Interp::Linear)]),
            None,
        );
        assert_eq!(m.source_at(RationalTime(r(1, 1))), RationalTime(r(3, 2)));
        assert_eq!(m.source_at(RationalTime(r(2, 1))), RationalTime(r(4, 1)));
        assert_eq!(m.source_at(RationalTime(r(3, 1))), RationalTime(r(7, 1)));
        // Before the first key: the first speed.
        assert_eq!(m.source_at(RationalTime(r(-1, 1))), RationalTime(r(-1, 1)));
    }

    #[test]
    fn eased_ramp_is_monotone_and_close_to_linear_area() {
        let m = TimeMap::new(
            RationalTime::ZERO,
            &keys(&[(0, 1, Interp::EaseInOut), (2, 3, Interp::Linear)]),
            None,
        );
        // Symmetric ease from 1 to 3: area over [0, 2] is 4, like linear.
        let end = m.source_seconds(r(2, 1));
        assert!((end - 4.0).abs() < 1e-6, "{end}");
        let mut prev = f64::NEG_INFINITY;
        for i in 0..=40 {
            let v = m.source_seconds(r(i, 20));
            assert!(v > prev);
            prev = v;
        }
    }

    #[test]
    fn remap_curve_and_sampling() {
        let m = TimeMap::new(
            RationalTime::ZERO,
            &Animatable::constant(Rational::ONE),
            Some(&keys(&[(0, 5, Interp::Linear), (2, 1, Interp::Linear)])),
        );
        assert_eq!(m.source_at(RationalTime(r(1, 1))), RationalTime(r(3, 1)));
        let (lo, hi) = m.source_range(Rational::ZERO, r(2, 1));
        assert_eq!((lo, hi), (1.0, 5.0));
        let fps = Some(Rational::from_int(24));
        assert_eq!(
            sample(RationalTime(r(1, 30)), fps, Sampling::Nearest),
            Sample::One(RationalTime(r(1, 24)))
        );
        assert_eq!(
            sample(RationalTime(r(1, 48)), fps, Sampling::FrameBlend),
            Sample::Blend(RationalTime::ZERO, RationalTime(r(1, 24)), r(1, 2))
        );
        assert_eq!(
            sample(RationalTime(r(1, 24)), fps, Sampling::FrameBlend),
            Sample::One(RationalTime(r(1, 24)))
        );
    }
}
