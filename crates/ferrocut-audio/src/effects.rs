//! Per-clip and per-track audio effects: parametric EQ, high/low-pass,
//! compressor, limiter and gate.
//!
//! Pure Rust, f64 state, processed sample by sample in a fixed order, so
//! results are bit-exact run to run. Every parameter is an [`Animatable`]
//! (keyframes in the owner's time base: clip-local for clip effects,
//! timeline time for track effects). Keyframed parameters are evaluated on a
//! global grid of [`CONTROL_BLOCK`] samples (the value at the block's first
//! sample holds for the block), so a chunked render equals a whole render:
//! a sample's coefficients depend only on its absolute index, never on where
//! a chunk starts. Constant parameters are evaluated once.
//!
//! State ([`FxState`]: biquad histories, detector envelopes, hold counters)
//! is plain `f64`s, carried across chunks by the streaming mixer and hashed
//! into its cache keys.

use ferrocut_types::{Animatable, Rational, RationalTime};

use crate::db_to_gain;

/// Keyframed effect parameters are re-evaluated every this many samples.
pub const CONTROL_BLOCK: i64 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandKind {
    /// RBJ peaking EQ.
    Peak,
    /// RBJ low shelf (`q` = shelf slope-style Q).
    LowShelf,
    /// RBJ high shelf.
    HighShelf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EqBand {
    pub kind: BandKind,
    pub freq_hz: Animatable,
    pub gain_db: Animatable,
    pub q: Animatable,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Parametric EQ: bands in series.
    Eq { bands: Vec<EqBand> },
    /// 12 dB/oct RBJ high-pass.
    HighPass { freq_hz: Animatable, q: Animatable },
    /// 12 dB/oct RBJ low-pass.
    LowPass { freq_hz: Animatable, q: Animatable },
    /// Feed-forward, stereo-linked peak compressor with a soft knee.
    Compressor {
        threshold_db: Animatable,
        ratio: Animatable,
        attack_ms: Animatable,
        release_ms: Animatable,
        knee_db: Animatable,
        makeup_db: Animatable,
    },
    /// Zero-latency, stereo-linked brickwall sample-peak limiter: instant
    /// attack (the output never exceeds the ceiling), exponential release.
    Limiter {
        ceiling_db: Animatable,
        release_ms: Animatable,
    },
    /// Noise gate: opens above `threshold_db`, stays open for `hold_ms` after
    /// the level falls below, attenuates by `range_db` when closed.
    Gate {
        threshold_db: Animatable,
        range_db: Animatable,
        attack_ms: Animatable,
        hold_ms: Animatable,
        release_ms: Animatable,
    },
}

impl Effect {
    pub fn name(&self) -> &'static str {
        match self {
            Effect::Eq { .. } => "eq",
            Effect::HighPass { .. } => "high_pass",
            Effect::LowPass { .. } => "low_pass",
            Effect::Compressor { .. } => "compressor",
            Effect::Limiter { .. } => "limiter",
            Effect::Gate { .. } => "gate",
        }
    }

    /// Every parameter with its name, for validation.
    pub fn params(&self) -> Vec<(String, &Animatable)> {
        let mut v: Vec<(String, &Animatable)> = Vec::new();
        match self {
            Effect::Eq { bands } => {
                for (i, b) in bands.iter().enumerate() {
                    v.push((format!("bands.{i}.freq_hz"), &b.freq_hz));
                    v.push((format!("bands.{i}.gain_db"), &b.gain_db));
                    v.push((format!("bands.{i}.q"), &b.q));
                }
            }
            Effect::HighPass { freq_hz, q } | Effect::LowPass { freq_hz, q } => {
                v.push(("freq_hz".into(), freq_hz));
                v.push(("q".into(), q));
            }
            Effect::Compressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                knee_db,
                makeup_db,
            } => {
                v.push(("threshold_db".into(), threshold_db));
                v.push(("ratio".into(), ratio));
                v.push(("attack_ms".into(), attack_ms));
                v.push(("release_ms".into(), release_ms));
                v.push(("knee_db".into(), knee_db));
                v.push(("makeup_db".into(), makeup_db));
            }
            Effect::Limiter {
                ceiling_db,
                release_ms,
            } => {
                v.push(("ceiling_db".into(), ceiling_db));
                v.push(("release_ms".into(), release_ms));
            }
            Effect::Gate {
                threshold_db,
                range_db,
                attack_ms,
                hold_ms,
                release_ms,
            } => {
                v.push(("threshold_db".into(), threshold_db));
                v.push(("range_db".into(), range_db));
                v.push(("attack_ms".into(), attack_ms));
                v.push(("hold_ms".into(), hold_ms));
                v.push(("release_ms".into(), release_ms));
            }
        }
        v
    }

    /// Range checks on the keyframe values (`rate`: the program rate, for
    /// the Nyquist limit on frequencies).
    pub fn validate(&self, rate: u32) -> Result<(), String> {
        let nyq = Rational::new(rate as i64, 2);
        for (name, a) in self.params() {
            a.validate()
                .map_err(|e| format!("{} {name}: {e}", self.name()))?;
            let (lo, hi) = a.key_range();
            let leaf = name.rsplit('.').next().unwrap_or(&name);
            let bad = match leaf {
                "freq_hz" => lo <= Rational::ZERO || hi >= nyq,
                "q" => lo <= Rational::ZERO,
                "ratio" => lo < Rational::ONE,
                "attack_ms" | "release_ms" => lo <= Rational::ZERO,
                "hold_ms" | "knee_db" | "range_db" => lo < Rational::ZERO,
                "ceiling_db" => hi > Rational::ZERO,
                _ => false,
            };
            if bad {
                return Err(format!(
                    "{} {name}: out of range ({})",
                    self.name(),
                    match leaf {
                        "freq_hz" => "0 < freq_hz < sample_rate/2",
                        "q" => "q > 0",
                        "ratio" => "ratio >= 1",
                        "attack_ms" | "release_ms" => "> 0 ms",
                        "ceiling_db" => "ceiling_db <= 0",
                        _ => ">= 0",
                    }
                ));
            }
        }
        Ok(())
    }

    fn state_len(&self) -> usize {
        match self {
            Effect::Eq { bands } => bands.len() * 8,
            Effect::HighPass { .. } | Effect::LowPass { .. } => 8,
            Effect::Compressor { .. } => 1,
            Effect::Limiter { .. } => 1,
            Effect::Gate { .. } => 3,
        }
    }

    fn initial_state(&self) -> Vec<f64> {
        let mut s = vec![0.0; self.state_len()];
        match self {
            // Limiter gain and gate gain start at unity (open).
            Effect::Limiter { .. } => s[0] = 1.0,
            Effect::Gate { .. } => s[2] = 1.0,
            _ => {}
        }
        s
    }
}

/// Effect chain state: one `Vec<f64>` per effect.
pub type FxState = Vec<Vec<f64>>;

/// Fresh state for a chain.
pub fn initial_state(fx: &[Effect]) -> FxState {
    fx.iter().map(Effect::initial_state).collect()
}

/// Validate a chain.
pub fn validate(fx: &[Effect], rate: u32) -> Result<(), String> {
    fx.iter().try_for_each(|e| e.validate(rate))
}

/// Biquad coefficients `(b0, b1, b2, a1, a2)`, normalized by `a0`.
type Coefs = [f64; 5];

fn rbj(kind: u8, f: f64, q: f64, gain_db: f64, fs: f64) -> Coefs {
    // Clamp to the validated ranges (bezier segments can overshoot keys).
    let f = f.clamp(1e-3, fs * 0.499_999);
    let q = q.max(1e-6);
    let w0 = std::f64::consts::TAU * f / fs;
    let (sn, cs) = w0.sin_cos();
    let alpha = sn / (2.0 * q);
    let a = 10f64.powf(gain_db / 40.0);
    let (b0, b1, b2, a0, a1, a2) = match kind {
        // Peak.
        0 => (
            1.0 + alpha * a,
            -2.0 * cs,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cs,
            1.0 - alpha / a,
        ),
        // Low shelf.
        1 => {
            let k = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) - (a - 1.0) * cs + k),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                a * ((a + 1.0) - (a - 1.0) * cs - k),
                (a + 1.0) + (a - 1.0) * cs + k,
                -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                (a + 1.0) + (a - 1.0) * cs - k,
            )
        }
        // High shelf.
        2 => {
            let k = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) + (a - 1.0) * cs + k),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                a * ((a + 1.0) + (a - 1.0) * cs - k),
                (a + 1.0) - (a - 1.0) * cs + k,
                2.0 * ((a - 1.0) - (a + 1.0) * cs),
                (a + 1.0) - (a - 1.0) * cs - k,
            )
        }
        // High-pass.
        3 => (
            (1.0 + cs) / 2.0,
            -(1.0 + cs),
            (1.0 + cs) / 2.0,
            1.0 + alpha,
            -2.0 * cs,
            1.0 - alpha,
        ),
        // Low-pass.
        _ => (
            (1.0 - cs) / 2.0,
            1.0 - cs,
            (1.0 - cs) / 2.0,
            1.0 + alpha,
            -2.0 * cs,
            1.0 - alpha,
        ),
    };
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// Direct form I step; `s` = `[x1, x2, y1, y2]`.
#[inline]
fn biquad(c: &Coefs, s: &mut [f64], x: f64) -> f64 {
    let y = c[0] * x + c[1] * s[0] + c[2] * s[1] - c[3] * s[2] - c[4] * s[3];
    s[1] = s[0];
    s[0] = x;
    s[3] = s[2];
    s[2] = y;
    y
}

/// One-pole smoothing coefficient for a time constant of `ms` at `fs`.
#[inline]
fn pole(ms: f64, fs: f64) -> f64 {
    (-1.0 / (ms.max(1e-3) / 1000.0 * fs)).exp()
}

/// Per-effect values derived from the parameters at one control block.
enum Ctl {
    Filters(Vec<Coefs>),
    Comp {
        thr: f64,
        slope: f64,
        knee: f64,
        makeup: f64,
        att: f64,
        rel: f64,
    },
    Lim {
        ceil: f64,
        rel: f64,
    },
    Gate {
        thr: f64,
        floor: f64,
        att: f64,
        hold: f64,
        rel: f64,
        det: f64,
    },
}

fn control(e: &Effect, t: RationalTime, fs: f64) -> Ctl {
    let v = |a: &Animatable| a.eval(t);
    match e {
        Effect::Eq { bands } => Ctl::Filters(
            bands
                .iter()
                .map(|b| {
                    let k = match b.kind {
                        BandKind::Peak => 0,
                        BandKind::LowShelf => 1,
                        BandKind::HighShelf => 2,
                    };
                    rbj(k, v(&b.freq_hz), v(&b.q), v(&b.gain_db), fs)
                })
                .collect(),
        ),
        Effect::HighPass { freq_hz, q } => Ctl::Filters(vec![rbj(3, v(freq_hz), v(q), 0.0, fs)]),
        Effect::LowPass { freq_hz, q } => Ctl::Filters(vec![rbj(4, v(freq_hz), v(q), 0.0, fs)]),
        Effect::Compressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            knee_db,
            makeup_db,
        } => Ctl::Comp {
            thr: v(threshold_db),
            slope: 1.0 / v(ratio).max(1.0) - 1.0,
            knee: v(knee_db).max(0.0),
            makeup: v(makeup_db),
            att: pole(v(attack_ms), fs),
            rel: pole(v(release_ms), fs),
        },
        Effect::Limiter {
            ceiling_db,
            release_ms,
        } => Ctl::Lim {
            ceil: db_to_gain(v(ceiling_db).min(0.0)),
            rel: 1.0 - pole(v(release_ms), fs),
        },
        Effect::Gate {
            threshold_db,
            range_db,
            attack_ms,
            hold_ms,
            release_ms,
        } => Ctl::Gate {
            thr: db_to_gain(v(threshold_db)),
            floor: db_to_gain(-v(range_db).max(0.0)),
            att: pole(v(attack_ms), fs),
            hold: (v(hold_ms).max(0.0) / 1000.0 * fs).round(),
            rel: pole(v(release_ms), fs),
            // Level detector: instant attack, 10 ms release.
            det: pole(10.0, fs),
        },
    }
}

/// Soft-knee compressor gain change in dB (<= 0) for level `lvl` dB.
#[inline]
fn comp_db(lvl: f64, thr: f64, slope: f64, knee: f64) -> f64 {
    let over = lvl - thr;
    if knee > 0.0 && 2.0 * over.abs() <= knee {
        let x = over + knee / 2.0;
        slope * x * x / (2.0 * knee)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

/// Process `l`/`r` in place. Sample `i` of the buffers is absolute sample
/// `n0 + i` at `rate`; keyframes are evaluated at `n / rate - origin`.
pub fn process(
    fx: &[Effect],
    state: &mut FxState,
    l: &mut [f32],
    r: &mut [f32],
    n0: i64,
    rate: u32,
    origin: Rational,
) {
    if fx.is_empty() || l.is_empty() {
        return;
    }
    let fs = rate as f64;
    let time = |n: i64| RationalTime(Rational::new(n, rate as i64) - origin);
    let animated: Vec<bool> = fx
        .iter()
        .map(|e| e.params().iter().any(|(_, a)| a.as_constant().is_none()))
        .collect();
    let block_of = |n: i64| n.div_euclid(CONTROL_BLOCK);
    let mut ctl: Vec<Ctl> = fx
        .iter()
        .map(|e| control(e, time(block_of(n0) * CONTROL_BLOCK), fs))
        .collect();
    let mut cur_block = block_of(n0);
    for i in 0..l.len() {
        let n = n0 + i as i64;
        let b = block_of(n);
        if b != cur_block {
            cur_block = b;
            for (k, e) in fx.iter().enumerate() {
                if animated[k] {
                    ctl[k] = control(e, time(b * CONTROL_BLOCK), fs);
                }
            }
        }
        let (mut x, mut y) = (l[i] as f64, r[i] as f64);
        for (k, s) in state.iter_mut().enumerate() {
            match &ctl[k] {
                Ctl::Filters(cs) => {
                    for (j, c) in cs.iter().enumerate() {
                        x = biquad(c, &mut s[j * 8..j * 8 + 4], x);
                        y = biquad(c, &mut s[j * 8 + 4..j * 8 + 8], y);
                    }
                }
                Ctl::Comp {
                    thr,
                    slope,
                    knee,
                    makeup,
                    att,
                    rel,
                } => {
                    let lvl = x.abs().max(y.abs());
                    let c = if lvl > s[0] { *att } else { *rel };
                    s[0] = lvl + (s[0] - lvl) * c;
                    let db = 20.0 * s[0].max(1e-10).log10();
                    let g = db_to_gain(comp_db(db, *thr, *slope, *knee) + makeup);
                    x *= g;
                    y *= g;
                }
                Ctl::Lim { ceil, rel } => {
                    let lvl = x.abs().max(y.abs());
                    let need = if lvl > *ceil { ceil / lvl } else { 1.0 };
                    s[0] = need.min(s[0] + (1.0 - s[0]) * rel);
                    x *= s[0];
                    y *= s[0];
                }
                Ctl::Gate {
                    thr,
                    floor,
                    att,
                    hold,
                    rel,
                    det,
                } => {
                    let lvl = x.abs().max(y.abs());
                    s[0] = if lvl > s[0] { lvl } else { s[0] * det };
                    let open = if s[0] > *thr {
                        s[1] = *hold;
                        true
                    } else if s[1] > 0.0 {
                        s[1] -= 1.0;
                        true
                    } else {
                        false
                    };
                    let target = if open { 1.0 } else { *floor };
                    let c = if target > s[2] { *att } else { *rel };
                    s[2] = target + (s[2] - target) * c;
                    x *= s[2];
                    y *= s[2];
                }
            }
        }
        l[i] = x as f32;
        r[i] = y as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrocut_types::{Interp, Keyframe, KeyframeTrack};

    fn c(v: f64) -> Animatable {
        Animatable::Constant(Rational::new((v * 10000.0).round() as i64, 10000))
    }

    fn sine(f: f64, n: usize, amp: f64, fs: f64) -> Vec<f32> {
        (0..n)
            .map(|i| (amp * (std::f64::consts::TAU * f * i as f64 / fs).sin()) as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn run(fx: &[Effect], x: &[f32], fs: u32) -> Vec<f32> {
        let mut st = initial_state(fx);
        let (mut l, mut r) = (x.to_vec(), x.to_vec());
        process(fx, &mut st, &mut l, &mut r, 0, fs, Rational::ZERO);
        l
    }

    #[test]
    fn filters_pass_and_stop() {
        let fs = 48000;
        let hp = [Effect::HighPass {
            freq_hz: c(1000.0),
            q: c(std::f64::consts::FRAC_1_SQRT_2),
        }];
        let lo = sine(100.0, 48000, 0.5, fs as f64);
        let hi = sine(8000.0, 48000, 0.5, fs as f64);
        let r_lo = rms(&run(&hp, &lo, fs)[4800..]) / rms(&lo[4800..]);
        let r_hi = rms(&run(&hp, &hi, fs)[4800..]) / rms(&hi[4800..]);
        assert!(r_lo < 0.02, "{r_lo}");
        assert!((r_hi - 1.0).abs() < 0.01, "{r_hi}");
        let peak = [Effect::Eq {
            bands: vec![EqBand {
                kind: BandKind::Peak,
                freq_hz: c(1000.0),
                gain_db: c(6.0),
                q: c(1.0),
            }],
        }];
        let x = sine(1000.0, 48000, 0.25, fs as f64);
        let g = rms(&run(&peak, &x, fs)[4800..]) / rms(&x[4800..]);
        assert!((20.0 * g.log10() - 6.0).abs() < 0.05, "{g}");
    }

    #[test]
    fn dynamics_behave() {
        let fs = 48000;
        let x = sine(1000.0, 48000, 0.9, fs as f64);
        let lim = [Effect::Limiter {
            ceiling_db: c(-6.0),
            release_ms: c(50.0),
        }];
        let y = run(&lim, &x, fs);
        let ceil = db_to_gain(-6.0) as f32;
        assert!(y.iter().all(|v| v.abs() <= ceil * 1.000_001));
        let comp = [Effect::Compressor {
            threshold_db: c(-20.0),
            ratio: c(4.0),
            attack_ms: c(1.0),
            release_ms: c(100.0),
            knee_db: c(0.0),
            makeup_db: c(0.0),
        }];
        let y = run(&comp, &x, fs);
        // Peak 0.9 (-0.9 dBFS): 19.1 dB over, 4:1 -> about -14.3 dB.
        let g = rms(&y[24000..]) / rms(&x[24000..]);
        assert!(
            (20.0 * g.log10() + 14.3).abs() < 0.5,
            "{}",
            20.0 * g.log10()
        );
        let gate = [Effect::Gate {
            threshold_db: c(-30.0),
            range_db: c(60.0),
            attack_ms: c(1.0),
            hold_ms: c(10.0),
            release_ms: c(20.0),
        }];
        let quiet = sine(1000.0, 48000, 0.001, fs as f64);
        assert!(rms(&run(&gate, &quiet, fs)[24000..]) < 1e-5);
        let loud = run(&gate, &x, fs);
        assert!((rms(&loud[24000..]) / rms(&x[24000..]) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn chunked_equals_whole_with_keyframes() {
        let fs = 44100;
        let x = sine(440.0, 44100, 0.8, fs as f64);
        let key = |t: i64, v: i64| Keyframe {
            t: RationalTime(Rational::new(t, 10)),
            v: Rational::from_int(v),
            interp: Interp::Linear,
        };
        let fx = [
            Effect::LowPass {
                freq_hz: Animatable::Keyframes(KeyframeTrack {
                    keyframes: vec![key(0, 200), key(10, 8000)],
                }),
                q: c(std::f64::consts::FRAC_1_SQRT_2),
            },
            Effect::Compressor {
                threshold_db: Animatable::Keyframes(KeyframeTrack {
                    keyframes: vec![key(0, -10), key(10, -30)],
                }),
                ratio: c(3.0),
                attack_ms: c(5.0),
                release_ms: c(80.0),
                knee_db: c(6.0),
                makeup_db: c(2.0),
            },
        ];
        let whole = run(&fx, &x, fs);
        let mut st = initial_state(&fx);
        let mut out = Vec::new();
        let mut a = 0usize;
        for len in [1000usize, 4410, 77, 10_000, 28_613] {
            let (mut l, mut r) = (x[a..a + len].to_vec(), x[a..a + len].to_vec());
            process(&fx, &mut st, &mut l, &mut r, a as i64, fs, Rational::ZERO);
            out.extend(l);
            a += len;
        }
        assert_eq!(a, x.len());
        assert!(
            whole
                .iter()
                .zip(&out)
                .all(|(p, q)| p.to_bits() == q.to_bits())
        );
    }
}
