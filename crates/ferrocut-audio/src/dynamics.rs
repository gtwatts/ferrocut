//! Sidechain ducking and the true-peak limiter (sequential, whole-program).

use ferrocut_types::{Animatable, Rational, RationalTime};

use crate::program::Duck;

/// Per-sample linear gains for a ducked track from its key signal (sample 0
/// = timeline time 0).
///
/// Peak detector on `max(|L|, |R|)` with one-pole attack/release smoothing,
/// then a hard-knee gain computer: above `threshold_db` the level is reduced
/// by `over · (1 - 1/ratio)` dB, at most `range_db`. Keyframed parameters are
/// evaluated per sample; all-constant parameters take the original fast path
/// (same arithmetic, same bits).
pub fn duck_gains(key_l: &[f32], key_r: &[f32], d: &Duck, rate: u32) -> Vec<f32> {
    duck_gains_at(key_l, key_r, d, rate, 0, &mut 0.0)
}

/// [`duck_gains`] for a key signal starting at program sample `n0`, entered
/// with detector envelope `env` (updated to the state after the last sample):
/// chaining chunks equals one whole-program call, bit for bit.
pub fn duck_gains_at(
    key_l: &[f32],
    key_r: &[f32],
    d: &Duck,
    rate: u32,
    n0: i64,
    env_io: &mut f64,
) -> Vec<f32> {
    let fs = rate as f64;
    let c = |a: &Animatable| a.as_constant().map(|v| v.to_f64());
    if let (Some(thr), Some(ratio), Some(att), Some(rel), Some(range)) = (
        c(&d.threshold_db),
        c(&d.ratio),
        c(&d.attack_ms),
        c(&d.release_ms),
        c(&d.range_db),
    ) {
        let a_att = (-1.0 / (att / 1000.0 * fs)).exp();
        let a_rel = (-1.0 / (rel / 1000.0 * fs)).exp();
        let slope = 1.0 - 1.0 / ratio;
        let mut env = *env_io;
        let out = key_l
            .iter()
            .zip(key_r)
            .map(|(&l, &r)| {
                let x = (l.abs().max(r.abs())) as f64;
                let c = if x > env { a_att } else { a_rel };
                env = x + (env - x) * c;
                let over = 20.0 * env.max(1e-10).log10() - thr;
                if over > 0.0 {
                    let red = (over * slope).min(range);
                    10f64.powf(-red / 20.0) as f32
                } else {
                    1.0
                }
            })
            .collect();
        *env_io = env;
        return out;
    }
    let at = |a: &Animatable, n: usize| match a.as_constant() {
        Some(v) => v.to_f64(),
        None => a.eval(RationalTime(Rational::new(n0 + n as i64, rate as i64))),
    };
    let mut env = *env_io;
    let out = key_l
        .iter()
        .zip(key_r)
        .enumerate()
        .map(|(n, (&l, &r))| {
            let x = (l.abs().max(r.abs())) as f64;
            // Clamp to the ranges validation enforces on the keys (bezier
            // segments can overshoot between keys).
            let ms = |a: &Animatable| at(a, n).max(1e-3);
            let a = if x > env {
                (-1.0 / (ms(&d.attack_ms) / 1000.0 * fs)).exp()
            } else {
                (-1.0 / (ms(&d.release_ms) / 1000.0 * fs)).exp()
            };
            env = x + (env - x) * a;
            let over = 20.0 * env.max(1e-10).log10() - at(&d.threshold_db, n);
            if over > 0.0 {
                let slope = 1.0 - 1.0 / at(&d.ratio, n).max(1.0);
                let red = (over * slope).min(at(&d.range_db, n).max(0.0));
                10f64.powf(-red / 20.0) as f32
            } else {
                1.0
            }
        })
        .collect();
    *env_io = env;
    out
}

/// Half-length of the true-peak interpolation filter (taps per phase = 2·H).
const H: usize = 8;

/// Samples of history the limiter needs before a chunk (true-peak filter).
pub const LIMITER_HISTORY: i64 = H as i64 - 1;

/// Look-ahead of the limiter in samples at `rate`.
pub fn limiter_lookahead(rate: u32) -> i64 {
    ((rate as f64 * LIMITER_LOOKAHEAD_S).round() as i64).max(1)
}

/// Samples of future the limiter needs after a chunk.
pub fn limiter_future(rate: u32) -> i64 {
    limiter_lookahead(rate) + H as i64
}

/// 4x oversampling true-peak estimator (windowed-sinc polyphase, phases 1/4,
/// 2/4, 3/4 between samples; each phase normalized to unity DC gain).
pub struct TruePeak {
    phases: [[f64; 2 * H]; 3],
}

impl Default for TruePeak {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeak {
    pub fn new() -> Self {
        let mut phases = [[0.0; 2 * H]; 3];
        for (k, ph) in phases.iter_mut().enumerate() {
            let f = (k + 1) as f64 / 4.0;
            for (j, c) in ph.iter_mut().enumerate() {
                // Tap j multiplies x[n + j - H + 1]; its distance from n + f:
                let x = (j as f64 - H as f64 + 1.0) - f;
                let sinc = if x == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                };
                let w = 0.5 * (1.0 + (std::f64::consts::PI * x / H as f64).cos());
                *c = sinc * w;
            }
            let s: f64 = ph.iter().sum();
            ph.iter_mut().for_each(|c| *c /= s);
        }
        TruePeak { phases }
    }

    /// Estimated true peak (linear) of one channel between samples `n` and `n + 1`, including `x[n]`.
    #[inline]
    pub fn at(&self, x: &[f32], n: usize) -> f64 {
        let mut m = (x[n] as f64).abs();
        let lo = n as isize - H as isize + 1;
        for ph in &self.phases {
            let mut acc = 0.0f64;
            for (j, c) in ph.iter().enumerate() {
                let i = lo + j as isize;
                if i >= 0 && (i as usize) < x.len() {
                    acc += c * x[i as usize] as f64;
                }
            }
            m = m.max(acc.abs());
        }
        m
    }

    /// [`TruePeak::at`] for absolute sample `n` of a signal whose samples
    /// `[x0, x0 + x.len())` are in `x`; samples outside count as silence.
    #[inline]
    pub fn at_abs(&self, x: &[f32], x0: i64, n: i64) -> f64 {
        let mut m = (x[(n - x0) as usize] as f64).abs();
        let lo = n - H as i64 + 1;
        let hi = x0 + x.len() as i64;
        for ph in &self.phases {
            let mut acc = 0.0f64;
            for (j, c) in ph.iter().enumerate() {
                let i = lo + j as i64;
                if i >= x0 && i < hi {
                    acc += c * x[(i - x0) as usize] as f64;
                }
            }
            m = m.max(acc.abs());
        }
        m
    }
}

/// [`limiter_gains`] for output samples `[a, b)` of a `total`-sample signal,
/// given its samples `[y0, y0 + y_l.len())`, which must cover
/// `[a - LIMITER_HISTORY, b + limiter_future(rate))` clamped to `[0, total)`.
/// `env` is the release envelope entering sample `a` (1.0 at sample 0) and is
/// updated to the state after `b - 1`. Chaining chunks equals one
/// whole-signal [`limiter_gains`] call, bit for bit. Returns the gains and
/// their minimum.
#[allow(clippy::too_many_arguments)]
pub fn limiter_chunk(
    y_l: &[f32],
    y_r: &[f32],
    y0: i64,
    total: i64,
    a: i64,
    b: i64,
    ceiling: f64,
    rate: u32,
    env: &mut f64,
) -> (Vec<f32>, f64) {
    let la = limiter_lookahead(rate);
    let (w0, w1) = (y0, y0 + y_l.len() as i64);
    assert!(
        w0 <= (a - LIMITER_HISTORY).max(0) && w1 >= (b + limiter_future(rate)).min(total),
        "limiter window {w0}..{w1} does not cover {a}..{b}"
    );
    let tp = TruePeak::new();
    let mut hot: Vec<(i64, f64)> = Vec::new();
    for k in a..(b + la).min(total) {
        let p = tp.at_abs(y_l, y0, k).max(tp.at_abs(y_r, y0, k));
        if p > ceiling {
            hot.push((k, ceiling / p));
        }
    }
    let rel = 1.0 - (-1.0 / (LIMITER_RELEASE_S * rate as f64)).exp();
    let mut out = Vec::with_capacity((b - a).max(0) as usize);
    let (mut e, mut min_g, mut first) = (*env, 1.0f64, 0usize);
    for i in a..b {
        while first < hot.len() && hot[first].0 < i {
            first += 1;
        }
        let mut ramp = 1.0f64;
        for &(k, req) in hot[first..].iter().take_while(|(k, _)| *k <= i + la) {
            let g = req + (1.0 - req) * (k - i) as f64 / la as f64;
            ramp = ramp.min(g);
        }
        e = ramp.min(e + (1.0 - e) * rel);
        min_g = min_g.min(e);
        out.push(e as f32);
    }
    *env = e;
    (out, min_g)
}

/// Look-ahead of the limiter (attack ramp length), seconds.
pub const LIMITER_LOOKAHEAD_S: f64 = 0.005;
/// Release time constant of the limiter, seconds.
pub const LIMITER_RELEASE_S: f64 = 0.08;

/// Per-sample limiter gains keeping the stereo-linked true peak of `l`/`r`
/// at or below `ceiling` (linear). Offline look-ahead: the gain ramps down
/// linearly over the look-ahead before each over-peak (so it reaches the
/// required gain exactly at the peak), then releases exponentially. No delay
/// line is needed because the whole signal is available. Returns the gains
/// and the minimum gain.
pub fn limiter_gains(l: &[f32], r: &[f32], ceiling: f64, rate: u32) -> (Vec<f32>, f64) {
    let n = l.len();
    let tp = TruePeak::new();
    // Required gain at each over-ceiling sample.
    let mut hot: Vec<(usize, f64)> = Vec::new();
    for i in 0..n {
        let p = tp.at(l, i).max(tp.at(r, i));
        if p > ceiling {
            hot.push((i, ceiling / p));
        }
    }
    let la = ((rate as f64 * LIMITER_LOOKAHEAD_S).round() as usize).max(1);
    let rel = 1.0 - (-1.0 / (LIMITER_RELEASE_S * rate as f64)).exp();
    let mut out = Vec::with_capacity(n);
    let (mut env, mut min_g, mut first) = (1.0f64, 1.0f64, 0usize);
    for i in 0..n {
        while first < hot.len() && hot[first].0 < i {
            first += 1;
        }
        let mut ramp = 1.0f64;
        for &(k, req) in hot[first..].iter().take_while(|(k, _)| *k <= i + la) {
            let g = req + (1.0 - req) * (k - i) as f64 / la as f64;
            ramp = ramp.min(g);
        }
        env = ramp.min(env + (1.0 - env) * rel);
        min_g = min_g.min(env);
        out.push(env as f32);
    }
    (out, min_g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrocut_types::{Interp, Keyframe, KeyframeTrack};

    fn c(v: i64) -> Animatable {
        Animatable::Constant(Rational::from_int(v))
    }

    #[test]
    fn keyframed_duck_range_follows_its_curve() {
        let rate = 1000;
        let key = vec![0.5f32; 4000]; // -6 dBFS, far above the threshold
        let mut d = Duck {
            keys: vec![0],
            threshold_db: c(-30),
            ratio: c(1000),
            attack_ms: c(1),
            release_ms: c(100),
            range_db: c(12),
        };
        let flat = duck_gains(&key, &key, &d, rate);
        d.range_db = Animatable::Keyframes(KeyframeTrack {
            keyframes: [(0, 0), (4, 20)]
                .iter()
                .map(|&(t, v)| Keyframe {
                    t: RationalTime(Rational::from_int(t)),
                    v: Rational::from_int(v),
                    interp: Interp::Linear,
                })
                .collect(),
        });
        let ramp = duck_gains(&key, &key, &d, rate);
        let db = |g: f32| -20.0 * (g as f64).log10();
        assert!((db(flat[2000]) - 12.0).abs() < 1e-3);
        assert!((db(ramp[1000]) - 5.0).abs() < 1e-3, "{}", db(ramp[1000]));
        assert!((db(ramp[3000]) - 15.0).abs() < 1e-3, "{}", db(ramp[3000]));
    }
}
