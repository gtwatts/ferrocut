//! Stateless mixing of a sample range.

use ferrocut_types::{Animatable, RationalTime};

use crate::analysis::Control;
use crate::db_to_gain;
use crate::program::{Program, SourceAudio};

/// Planar stereo f32.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stereo {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

impl Stereo {
    pub fn silence(n: usize) -> Self {
        Stereo {
            l: vec![0.0; n],
            r: vec![0.0; n],
        }
    }
    pub fn len(&self) -> usize {
        self.l.len()
    }
    pub fn is_empty(&self) -> bool {
        self.l.is_empty()
    }
    /// Interleaved little-endian f32 bytes (the PCM `pcm_f32le` layout).
    pub fn to_f32le_interleaved(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len() * 8);
        for (l, r) in self.l.iter().zip(&self.r) {
            out.extend_from_slice(&l.to_le_bytes());
            out.extend_from_slice(&r.to_le_bytes());
        }
        out
    }
    pub fn append(&mut self, o: &Stereo) {
        self.l.extend_from_slice(&o.l);
        self.r.extend_from_slice(&o.r);
    }
}

/// Constant-power pan of a mono signal: (-3 dB, -3 dB) at center.
#[inline]
pub fn pan_mono(p: f64) -> (f64, f64) {
    let th = (p.clamp(-1.0, 1.0) + 1.0) * std::f64::consts::FRAC_PI_4;
    (th.cos(), th.sin())
}

/// Balance of a stereo signal: unity at center, the far side attenuated linearly.
#[inline]
pub fn balance(p: f64) -> (f64, f64) {
    let p = p.clamp(-1.0, 1.0);
    ((1.0 - p).min(1.0), (1.0 + p).min(1.0))
}

/// Evaluates an [`Animatable`] per sample, skipping the work when constant.
struct Param<'a> {
    anim: &'a Animatable,
    constant: Option<f64>,
}

impl<'a> Param<'a> {
    fn new(anim: &'a Animatable, map: impl Fn(f64) -> f64) -> Self {
        Param {
            anim,
            constant: anim.as_constant().map(|v| map(v.to_f64())),
        }
    }
    #[inline]
    fn at(
        &self,
        p: &Program,
        n: i64,
        origin: ferrocut_types::Rational,
        map: impl Fn(f64) -> f64,
    ) -> f64 {
        match self.constant {
            Some(v) => v,
            None => map(self.anim.eval(RationalTime(p.time_of(n) - origin))),
        }
    }
}

/// Track `ti`'s bus over program samples `[a, b)`: its clips summed in order
/// (each times clip gain, fades and pan), then track gain and balance.
/// Ducking is not applied here (see [`render_range`]).
pub fn render_track(p: &Program, sources: &[SourceAudio], ti: usize, a: i64, b: i64) -> Stereo {
    let n = (b - a).max(0) as usize;
    let mut out = Stereo::silence(n);
    let t = &p.tracks[ti];
    if t.mute {
        return out;
    }
    for c in &t.clips {
        let (lo, hi) = (c.start.max(a), c.end.min(b));
        if lo >= hi {
            continue;
        }
        let src = &sources[c.source];
        let mono = src.is_mono();
        let gain = Param::new(&c.gain_db, db_to_gain);
        let pan = Param::new(&c.pan, |v| v);
        let pan_c = pan
            .constant
            .map(|v| if mono { pan_mono(v) } else { balance(v) });
        for s in lo..hi {
            let mut g = gain.at(p, s, c.origin, db_to_gain);
            for f in &c.fades {
                g *= f.gain(s);
            }
            let (gl, gr) = pan_c.unwrap_or_else(|| {
                let v = pan.at(p, s, c.origin, |v| v);
                if mono { pan_mono(v) } else { balance(v) }
            });
            let m = s + c.src_offset;
            let (sl, sr) = (src.get(0, m) as f64, src.get(1, m) as f64);
            let i = (s - a) as usize;
            out.l[i] += (sl * g * gl) as f32;
            out.r[i] += (sr * g * gr) as f32;
        }
    }
    let zero = ferrocut_types::Rational::ZERO;
    let gain = Param::new(&t.gain_db, db_to_gain);
    let pan = Param::new(&t.pan, |v| v);
    if gain.constant == Some(1.0) && pan.constant == Some(0.0) {
        return out;
    }
    for s in a..b {
        let g = gain.at(p, s, zero, db_to_gain);
        let (gl, gr) = balance(pan.at(p, s, zero, |v| v));
        let i = (s - a) as usize;
        out.l[i] = (out.l[i] as f64 * g * gl) as f32;
        out.r[i] = (out.r[i] as f64 * g * gr) as f32;
    }
    out
}

/// Master bus before normalization over `[a, b)`: track buses times their
/// duck gains, summed in track order. Returns the sum and the master gain
/// (linear) per sample.
pub(crate) fn master_sum(
    p: &Program,
    sources: &[SourceAudio],
    duck: &[Option<Vec<f32>>],
    a: i64,
    b: i64,
) -> (Stereo, Vec<f64>) {
    let n = (b - a).max(0) as usize;
    let mut sum = Stereo::silence(n);
    for ti in 0..p.tracks.len() {
        let t = render_track(p, sources, ti, a, b);
        let d = duck.get(ti).and_then(|d| d.as_deref());
        for i in 0..n {
            let g = d.map_or(1.0, |d| d[a as usize + i]);
            sum.l[i] += t.l[i] * g;
            sum.r[i] += t.r[i] * g;
        }
    }
    let mg = Param::new(&p.master_gain_db, db_to_gain);
    let zero = ferrocut_types::Rational::ZERO;
    let gains = (a..b).map(|s| mg.at(p, s, zero, db_to_gain)).collect();
    (sum, gains)
}

/// The normalized (pre-limiter) master sample.
#[inline]
pub(crate) fn finish(sum: f32, master_gain: f64, norm: f64) -> f32 {
    (sum as f64 * master_gain * norm) as f32
}

/// Final master over program samples `[a, b)`. Stateless given `ctl`, so
/// rendering `[a, b)` and `[b, c)` and appending equals rendering `[a, c)`.
pub fn render_range(p: &Program, sources: &[SourceAudio], ctl: &Control, a: i64, b: i64) -> Stereo {
    assert!(
        0 <= a && a <= b && b <= p.total,
        "range {a}..{b} outside 0..{}",
        p.total
    );
    let (mut sum, mg) = master_sum(p, sources, &ctl.duck, a, b);
    for i in 0..sum.len() {
        let lim = ctl.limiter.as_ref().map_or(1.0, |g| g[a as usize + i]);
        sum.l[i] = finish(sum.l[i], mg[i], ctl.norm_gain) * lim;
        sum.r[i] = finish(sum.r[i], mg[i], ctl.norm_gain) * lim;
    }
    sum
}
