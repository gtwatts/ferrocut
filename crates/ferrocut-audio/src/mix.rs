//! Stateless mixing of a sample range.

use std::collections::BTreeMap;

use ferrocut_types::{Animatable, RationalTime};

use crate::analysis::Control;
use crate::db_to_gain;
use crate::effects::{self, FxState};
use crate::program::{Program, SourceAudio, TrackProg};

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

/// Read access to source audio by (source index, plane, source sample):
/// the decoded sources themselves, or the windows of them one chunk needs.
pub trait Sources {
    fn count(&self) -> usize;
    fn is_mono(&self, i: usize) -> bool;
    /// Sample `m` of plane `ch` of source `i` (silence outside the source).
    fn sample(&self, i: usize, ch: usize, m: i64) -> f32;
}

impl Sources for [SourceAudio] {
    fn count(&self) -> usize {
        self.len()
    }
    fn is_mono(&self, i: usize) -> bool {
        self[i].is_mono()
    }
    #[inline]
    fn sample(&self, i: usize, ch: usize, m: i64) -> f32 {
        self[i].get(ch, m)
    }
}

impl Sources for Vec<SourceAudio> {
    fn count(&self) -> usize {
        self.len()
    }
    fn is_mono(&self, i: usize) -> bool {
        self[i].is_mono()
    }
    #[inline]
    fn sample(&self, i: usize, ch: usize, m: i64) -> f32 {
        self[i].get(ch, m)
    }
}

/// Effect state of one track bus: per active clip (by clip index) and the
/// track's own chain.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackState {
    pub clips: BTreeMap<usize, FxState>,
    pub fx: FxState,
}

impl TrackState {
    pub fn initial(t: &TrackProg) -> Self {
        TrackState {
            clips: BTreeMap::new(),
            fx: effects::initial_state(&t.effects),
        }
    }
}

/// Track `ti`'s bus over program samples `[a, b)`: its clips summed in order
/// (each times clip gain, fades and pan, then its effects), the track
/// effects, then track gain and balance. Ducking is not applied here (see
/// [`crate::stream::premix`]). `st` carries effect state from the previous
/// range (a clip's state starts fresh at its first sample).
pub fn render_track_with<S: Sources + ?Sized>(
    p: &Program,
    sources: &S,
    ti: usize,
    a: i64,
    b: i64,
    st: &mut TrackState,
) -> Stereo {
    let n = (b - a).max(0) as usize;
    let mut out = Stereo::silence(n);
    let t = &p.tracks[ti];
    if t.mute {
        return out;
    }
    for (ci, c) in t.clips.iter().enumerate() {
        let (lo, hi) = (c.start.max(a), c.end.min(b));
        if lo >= hi {
            continue;
        }
        let mono = sources.is_mono(c.source);
        let gain = Param::new(&c.gain_db, db_to_gain);
        let pan = Param::new(&c.pan, |v| v);
        let pan_c = pan
            .constant
            .map(|v| if mono { pan_mono(v) } else { balance(v) });
        let fx = !c.effects.is_empty();
        let mut tmp = if fx {
            Stereo::silence((hi - lo) as usize)
        } else {
            Stereo::default()
        };
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
            let (sl, sr) = (
                sources.sample(c.source, 0, m) as f64,
                sources.sample(c.source, 1, m) as f64,
            );
            if fx {
                let i = (s - lo) as usize;
                tmp.l[i] = (sl * g * gl) as f32;
                tmp.r[i] = (sr * g * gr) as f32;
            } else {
                let i = (s - a) as usize;
                out.l[i] += (sl * g * gl) as f32;
                out.r[i] += (sr * g * gr) as f32;
            }
        }
        if fx {
            let state = st
                .clips
                .entry(ci)
                .or_insert_with(|| effects::initial_state(&c.effects));
            effects::process(
                &c.effects, state, &mut tmp.l, &mut tmp.r, lo, p.rate, c.origin,
            );
            if hi == c.end {
                st.clips.remove(&ci);
            }
            let off = (lo - a) as usize;
            for i in 0..tmp.len() {
                out.l[off + i] += tmp.l[i];
                out.r[off + i] += tmp.r[i];
            }
        }
    }
    let zero = ferrocut_types::Rational::ZERO;
    if !t.effects.is_empty() {
        if st.fx.len() != t.effects.len() {
            st.fx = effects::initial_state(&t.effects);
        }
        effects::process(
            &t.effects, &mut st.fx, &mut out.l, &mut out.r, a, p.rate, zero,
        );
    }
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

/// [`render_track_with`] from fresh effect state: exact for any range when
/// the track and its clips have no effects, otherwise only from sample 0.
pub fn render_track(p: &Program, sources: &[SourceAudio], ti: usize, a: i64, b: i64) -> Stereo {
    let mut st = TrackState::initial(&p.tracks[ti]);
    render_track_with(p, sources, ti, a, b, &mut st)
}

/// Master gain (linear) per program sample over `[a, b)`.
pub fn master_gains(p: &Program, a: i64, b: i64) -> Vec<f64> {
    let mg = Param::new(&p.master_gain_db, db_to_gain);
    let zero = ferrocut_types::Rational::ZERO;
    (a..b).map(|s| mg.at(p, s, zero, db_to_gain)).collect()
}

/// The normalized (pre-limiter) master sample.
#[inline]
pub fn finish(sum: f32, master_gain: f64, norm: f64) -> f32 {
    (sum as f64 * master_gain * norm) as f32
}

/// Final master over program samples `[a, b)`: a slice of the analysis
/// pass's premix, normalized and limited. Stateless given `ctl`, so
/// rendering `[a, b)` and `[b, c)` and appending equals rendering `[a, c)`.
pub fn render_range(
    p: &Program,
    _sources: &[SourceAudio],
    ctl: &Control,
    a: i64,
    b: i64,
) -> Stereo {
    assert!(
        0 <= a && a <= b && b <= p.total,
        "range {a}..{b} outside 0..{}",
        p.total
    );
    let mg = master_gains(p, a, b);
    let mut out = Stereo::silence((b - a) as usize);
    for (i, &g) in mg.iter().enumerate() {
        let j = a as usize + i;
        let lim = ctl.limiter.as_ref().map_or(1.0, |g| g[j]);
        out.l[i] = finish(ctl.premix.l[j], g, ctl.norm_gain) * lim;
        out.r[i] = finish(ctl.premix.r[j], g, ctl.norm_gain) * lim;
    }
    out
}
