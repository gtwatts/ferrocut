//! Chunked (streaming) mixing: the program is mixed one sample range at a
//! time in order, carrying all sequential state (duck detector envelopes,
//! clip and track effect state) in a [`MixState`] between ranges. Chaining
//! [`premix`] over consecutive ranges from sample 0 equals one whole-program
//! call bit for bit, so callers can cache each range's output keyed by its
//! inputs plus [`MixState::words`] of the entering state, and hold only one
//! range in memory.
//!
//! Stage boundaries: [`premix`] produces the master bus sum before master
//! gain, normalization and limiting; [`crate::mix::master_gains`] and
//! [`crate::mix::finish`] apply master gain and normalization; the true-peak
//! limiter runs per range with [`crate::dynamics::limiter_chunk`].

use std::collections::BTreeMap;

use crate::dynamics::duck_gains_at;
use crate::effects;
use crate::mix::{Sources, Stereo, TrackState, render_track_with};
use crate::program::Program;

/// All sequential mixer state at a range boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct MixState {
    pub tracks: Vec<TrackState>,
    /// Duck detector envelope per track (0 for tracks without ducking).
    pub duck_env: Vec<f64>,
}

impl MixState {
    /// State at sample 0.
    pub fn initial(p: &Program) -> Self {
        MixState {
            tracks: p.tracks.iter().map(TrackState::initial).collect(),
            duck_env: vec![0.0; p.tracks.len()],
        }
    }

    /// Flat, self-delimiting encoding (f64 bits and counts), for cache keys
    /// and for storing the state next to a cached range.
    pub fn words(&self) -> Vec<u64> {
        let mut w = vec![self.tracks.len() as u64];
        let chain = |w: &mut Vec<u64>, fx: &effects::FxState| {
            w.push(fx.len() as u64);
            for s in fx {
                w.push(s.len() as u64);
                w.extend(s.iter().map(|v| v.to_bits()));
            }
        };
        for (t, env) in self.tracks.iter().zip(&self.duck_env) {
            w.push(env.to_bits());
            chain(&mut w, &t.fx);
            w.push(t.clips.len() as u64);
            for (ci, fx) in &t.clips {
                w.push(*ci as u64);
                chain(&mut w, fx);
            }
        }
        w
    }

    /// Inverse of [`MixState::words`].
    pub fn from_words(w: &[u64]) -> Option<Self> {
        let mut it = w.iter().copied();
        let mut next = || it.next();
        fn chain(next: &mut dyn FnMut() -> Option<u64>) -> Option<effects::FxState> {
            let n = next()? as usize;
            let mut fx = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                let k = next()? as usize;
                let mut s = Vec::with_capacity(k.min(1024));
                for _ in 0..k {
                    s.push(f64::from_bits(next()?));
                }
                fx.push(s);
            }
            Some(fx)
        }
        let nt = next()? as usize;
        let mut st = MixState {
            tracks: Vec::with_capacity(nt.min(256)),
            duck_env: Vec::with_capacity(nt.min(256)),
        };
        for _ in 0..nt {
            st.duck_env.push(f64::from_bits(next()?));
            let fx = chain(&mut next)?;
            let nc = next()? as usize;
            let mut clips = BTreeMap::new();
            for _ in 0..nc {
                let ci = next()? as usize;
                clips.insert(ci, chain(&mut next)?);
            }
            st.tracks.push(TrackState { clips, fx });
        }
        next().is_none().then_some(st)
    }
}

/// One range's master bus sum and the duck gains applied in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Premix {
    /// Sum of the (ducked) track buses, before master gain.
    pub sum: Stereo,
    /// Per track: duck gain per sample of the range (`None` = not ducked).
    pub duck: Vec<Option<Vec<f32>>>,
}

/// Mix program samples `[a, b)`: every track bus once (effects advance their
/// state exactly once per sample), duck gains from the key buses, then the
/// buses times their duck gains summed in track order.
pub fn premix<S: Sources + ?Sized>(
    p: &Program,
    sources: &S,
    a: i64,
    b: i64,
    st: &mut MixState,
) -> Premix {
    let n = (b - a).max(0) as usize;
    let buses: Vec<Stereo> = (0..p.tracks.len())
        .map(|ti| render_track_with(p, sources, ti, a, b, &mut st.tracks[ti]))
        .collect();
    let mut duck: Vec<Option<Vec<f32>>> = vec![None; p.tracks.len()];
    for (ti, t) in p.tracks.iter().enumerate() {
        let Some(d) = &t.duck else { continue };
        let mut key = Stereo::silence(n);
        for &k in &d.keys {
            for i in 0..n {
                key.l[i] += buses[k].l[i];
                key.r[i] += buses[k].r[i];
            }
        }
        duck[ti] = Some(duck_gains_at(
            &key.l,
            &key.r,
            d,
            p.rate,
            a,
            &mut st.duck_env[ti],
        ));
    }
    let mut sum = Stereo::silence(n);
    for (ti, t) in buses.iter().enumerate() {
        let d = duck[ti].as_deref();
        for i in 0..n {
            let g = d.map_or(1.0, |d| d[i]);
            sum.l[i] += t.l[i] * g;
            sum.r[i] += t.r[i] * g;
        }
    }
    Premix { sum, duck }
}
