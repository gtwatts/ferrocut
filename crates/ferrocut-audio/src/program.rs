//! The resolved, sample-domain description of what to mix.

use ferrocut_types::{Animatable, Rational};
use serde::{Deserialize, Serialize};

use crate::effects::Effect;

/// Decoded source audio at the program rate: one (mono) or two (L, R) planes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceAudio {
    pub planes: Vec<Vec<f32>>,
}

impl SourceAudio {
    pub fn len(&self) -> usize {
        self.planes.first().map_or(0, Vec::len)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn is_mono(&self) -> bool {
        self.planes.len() == 1
    }
    /// Sample `m` of plane `ch` (mono sources ignore `ch`); silence outside the source.
    #[inline]
    pub fn get(&self, ch: usize, m: i64) -> f32 {
        let p = &self.planes[ch.min(self.planes.len() - 1)];
        if m < 0 || m as usize >= p.len() {
            0.0
        } else {
            p[m as usize]
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeCurve {
    /// Gains `x` / `1 - x`: amplitudes sum to 1 (right for correlated material).
    Linear,
    /// Gains `sin(x·π/2)` / `cos(x·π/2)`: powers sum to 1 (uncorrelated material).
    #[default]
    EqualPower,
}

/// A fade over samples `[start, start + len)`; sample `n` uses
/// `x = (n - start + 1/2) / len` (midpoint rule, symmetric in and out).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fade {
    pub start: i64,
    pub len: i64,
    pub curve: FadeCurve,
    /// Fade in (0 -> 1) if true, out (1 -> 0) if false.
    pub fade_in: bool,
}

impl Fade {
    #[inline]
    pub fn gain(&self, n: i64) -> f64 {
        let before = n < self.start;
        let after = n >= self.start + self.len;
        if before || after || self.len <= 0 {
            return if self.fade_in == after { 1.0 } else { 0.0 };
        }
        let x = ((n - self.start) as f64 + 0.5) / self.len as f64;
        match (self.curve, self.fade_in) {
            (FadeCurve::Linear, true) => x,
            (FadeCurve::Linear, false) => 1.0 - x,
            (FadeCurve::EqualPower, true) => (x * std::f64::consts::FRAC_PI_2).sin(),
            (FadeCurve::EqualPower, false) => (x * std::f64::consts::FRAC_PI_2).cos(),
        }
    }
}

/// One clip's audio on a track.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipProg {
    /// Label for reports/errors (the timeline clip id).
    pub id: String,
    /// Index into the source list.
    pub source: usize,
    /// Audible region `[start, end)` in program samples (J/L offsets applied).
    pub start: i64,
    pub end: i64,
    /// Source sample of program sample `n` is `n + src_offset`.
    pub src_offset: i64,
    /// Timeline time (seconds) of the clip's local time 0, for its keyframes.
    pub origin: Rational,
    /// Clip gain in dB (keyframes in clip-local time).
    pub gain_db: Animatable,
    /// Clip pan in [-1, 1] (keyframes in clip-local time). Mono sources pan
    /// with a constant-power law (-3 dB at center); stereo sources balance.
    pub pan: Animatable,
    pub fades: Vec<Fade>,
    /// Clip effects, applied in order after gain, fades and pan (keyframes
    /// in clip-local time; state starts fresh at `start`).
    pub effects: Vec<Effect>,
}

/// Sidechain ducking of a track by the sum of `keys` (other tracks' buses).
/// Every parameter may be keyframed (timeline time).
#[derive(Clone, Debug, PartialEq)]
pub struct Duck {
    pub keys: Vec<usize>,
    pub threshold_db: Animatable,
    pub ratio: Animatable,
    pub attack_ms: Animatable,
    pub release_ms: Animatable,
    /// Maximum gain reduction in dB (positive).
    pub range_db: Animatable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackProg {
    pub name: String,
    pub clips: Vec<ClipProg>,
    /// Track bus gain in dB (keyframes in timeline time).
    pub gain_db: Animatable,
    /// Track bus balance in [-1, 1] (keyframes in timeline time).
    pub pan: Animatable,
    pub mute: bool,
    pub duck: Option<Duck>,
    /// Track effects, applied in order to the summed clips before track gain
    /// and balance (pre-fader; keyframes in timeline time).
    pub effects: Vec<Effect>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessTarget {
    /// Integrated loudness target, e.g. -23 (EBU R128) or -14 (streaming).
    pub target_lufs: f64,
    /// True-peak ceiling, e.g. -1 dBTP.
    pub true_peak_dbtp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub rate: u32,
    /// Output length in samples.
    pub total: i64,
    pub tracks: Vec<TrackProg>,
    /// Master bus gain in dB (keyframes in timeline time), before normalization.
    pub master_gain_db: Animatable,
    pub loudness: Option<LoudnessTarget>,
}

impl Program {
    /// Seconds (exact) of program sample `n`.
    pub fn time_of(&self, n: i64) -> Rational {
        Rational::new(n, self.rate as i64)
    }

    /// Structural checks the mixer relies on.
    pub fn validate(&self, n_sources: usize) -> Result<(), String> {
        for (ti, t) in self.tracks.iter().enumerate() {
            if let Some(d) = &t.duck {
                if d.keys.is_empty() {
                    return Err(format!("track {:?}: duck has no key tracks", t.name));
                }
                for &k in &d.keys {
                    let kt = self
                        .tracks
                        .get(k)
                        .ok_or_else(|| format!("track {:?}: bad duck key {k}", t.name))?;
                    if k == ti || kt.duck.is_some() {
                        return Err(format!(
                            "track {:?}: duck key {:?} must be another track that is not itself ducked",
                            t.name, kt.name
                        ));
                    }
                }
                let lo = |a: &Animatable| a.key_range().0;
                let keys_ok = [
                    &d.threshold_db,
                    &d.ratio,
                    &d.attack_ms,
                    &d.release_ms,
                    &d.range_db,
                ]
                .iter()
                .all(|a| a.validate().is_ok());
                if !(keys_ok
                    && lo(&d.ratio) >= Rational::ONE
                    && lo(&d.attack_ms) > Rational::ZERO
                    && lo(&d.release_ms) > Rational::ZERO
                    && lo(&d.range_db) >= Rational::ZERO)
                {
                    return Err(format!(
                        "track {:?}: duck needs ratio >= 1, attack/release > 0, range >= 0",
                        t.name
                    ));
                }
            }
            crate::effects::validate(&t.effects, self.rate)
                .map_err(|e| format!("track {:?}: {e}", t.name))?;
            for c in &t.clips {
                crate::effects::validate(&c.effects, self.rate)
                    .map_err(|e| format!("clip {:?}: {e}", c.id))?;
                if c.source >= n_sources {
                    return Err(format!("clip {:?}: bad source index", c.id));
                }
                if c.end < c.start {
                    return Err(format!("clip {:?}: negative audio region", c.id));
                }
            }
        }
        Ok(())
    }
}
