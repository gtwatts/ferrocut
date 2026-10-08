//! Timeline (JSON) form of the per-clip and per-track audio effects:
//! parametric EQ, high/low-pass, compressor, limiter and gate.
//!
//! `{"type": "compressor", "threshold_db": -18, "ratio": 3}`: every numeric
//! parameter is a rational or `{"keyframes": [...]}` (clip-local time on a
//! clip's `audio.effects`, timeline time on a track bus's `effects`) and has
//! a default. The DSP lives in [`ferrocut_audio::effects`].

use anyhow::{Context as _, anyhow, bail, ensure};
use ferrocut_audio::effects::{BandKind, Effect, EqBand};
use ferrocut_core::{Animatable, Rational};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn a(n: i64, d: i64) -> Animatable {
    Animatable::constant(Rational::new(n, d))
}
fn d_q() -> Animatable {
    a(7071, 10000)
}
fn d_zero() -> Animatable {
    a(0, 1)
}
fn d_comp_threshold() -> Animatable {
    a(-20, 1)
}
fn d_ratio() -> Animatable {
    a(4, 1)
}
fn d_comp_attack() -> Animatable {
    a(10, 1)
}
fn d_comp_release() -> Animatable {
    a(100, 1)
}
fn d_knee() -> Animatable {
    a(6, 1)
}
fn d_ceiling() -> Animatable {
    a(-1, 1)
}
fn d_lim_release() -> Animatable {
    a(50, 1)
}
fn d_gate_threshold() -> Animatable {
    a(-50, 1)
}
fn d_gate_range() -> Animatable {
    a(40, 1)
}
fn d_gate_attack() -> Animatable {
    a(1, 1)
}
fn d_hold() -> Animatable {
    a(50, 1)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandType {
    #[default]
    Peak,
    LowShelf,
    HighShelf,
}

/// One EQ band.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BandSpec {
    #[serde(default)]
    pub kind: BandType,
    pub freq_hz: Animatable,
    #[serde(default = "d_zero")]
    pub gain_db: Animatable,
    #[serde(default = "d_q")]
    pub q: Animatable,
}

/// One audio effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectSpec {
    /// Parametric EQ (bands in series).
    Eq { bands: Vec<BandSpec> },
    HighPass {
        freq_hz: Animatable,
        #[serde(default = "d_q")]
        q: Animatable,
    },
    LowPass {
        freq_hz: Animatable,
        #[serde(default = "d_q")]
        q: Animatable,
    },
    Compressor {
        #[serde(default = "d_comp_threshold")]
        threshold_db: Animatable,
        #[serde(default = "d_ratio")]
        ratio: Animatable,
        #[serde(default = "d_comp_attack")]
        attack_ms: Animatable,
        #[serde(default = "d_comp_release")]
        release_ms: Animatable,
        #[serde(default = "d_knee")]
        knee_db: Animatable,
        #[serde(default = "d_zero")]
        makeup_db: Animatable,
    },
    Limiter {
        #[serde(default = "d_ceiling")]
        ceiling_db: Animatable,
        #[serde(default = "d_lim_release")]
        release_ms: Animatable,
    },
    Gate {
        #[serde(default = "d_gate_threshold")]
        threshold_db: Animatable,
        #[serde(default = "d_gate_range")]
        range_db: Animatable,
        #[serde(default = "d_gate_attack")]
        attack_ms: Animatable,
        #[serde(default = "d_hold")]
        hold_ms: Animatable,
        #[serde(default = "d_comp_release")]
        release_ms: Animatable,
    },
}

impl EffectSpec {
    /// The mixer's form.
    pub fn to_effect(&self) -> Effect {
        match self.clone() {
            EffectSpec::Eq { bands } => Effect::Eq {
                bands: bands
                    .into_iter()
                    .map(|b| EqBand {
                        kind: match b.kind {
                            BandType::Peak => BandKind::Peak,
                            BandType::LowShelf => BandKind::LowShelf,
                            BandType::HighShelf => BandKind::HighShelf,
                        },
                        freq_hz: b.freq_hz,
                        gain_db: b.gain_db,
                        q: b.q,
                    })
                    .collect(),
            },
            EffectSpec::HighPass { freq_hz, q } => Effect::HighPass { freq_hz, q },
            EffectSpec::LowPass { freq_hz, q } => Effect::LowPass { freq_hz, q },
            EffectSpec::Compressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                knee_db,
                makeup_db,
            } => Effect::Compressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                knee_db,
                makeup_db,
            },
            EffectSpec::Limiter {
                ceiling_db,
                release_ms,
            } => Effect::Limiter {
                ceiling_db,
                release_ms,
            },
            EffectSpec::Gate {
                threshold_db,
                range_db,
                attack_ms,
                hold_ms,
                release_ms,
            } => Effect::Gate {
                threshold_db,
                range_db,
                attack_ms,
                hold_ms,
                release_ms,
            },
        }
    }
}

/// The mixer form of a chain.
pub fn to_effects(fx: &[EffectSpec]) -> Vec<Effect> {
    fx.iter().map(EffectSpec::to_effect).collect()
}

/// Validate a chain at `rate` Hz; `what` prefixes errors.
pub fn validate(fx: &[EffectSpec], rate: u32, what: &str) -> anyhow::Result<()> {
    for (i, e) in fx.iter().enumerate() {
        if let EffectSpec::Eq { bands } = e {
            ensure!(
                !bands.is_empty(),
                "{what} effects[{i}]: eq needs at least one band"
            );
        }
        e.to_effect()
            .validate(rate)
            .map_err(|m| anyhow!("{what} effects[{i}]: {m}"))?;
    }
    Ok(())
}

/// Effect types and their parameters (for docs, schema and error messages).
pub const TYPES: &[(&str, &str)] = &[
    (
        "eq",
        "bands: [{kind: peak|low_shelf|high_shelf (peak), freq_hz, gain_db (0), q (0.7071)}]",
    ),
    ("high_pass", "freq_hz, q (0.7071); 12 dB/oct"),
    ("low_pass", "freq_hz, q (0.7071); 12 dB/oct"),
    (
        "compressor",
        "threshold_db (-20), ratio (4), attack_ms (10), release_ms (100), knee_db (6), makeup_db (0)",
    ),
    (
        "limiter",
        "ceiling_db (-1), release_ms (50); sample-peak, zero latency",
    ),
    (
        "gate",
        "threshold_db (-50), range_db (40), attack_ms (1), hold_ms (50), release_ms (100)",
    ),
];

/// Set (or keyframe) one parameter of an effect in its JSON form: `param`
/// is a dotted path inside the effect (`threshold_db`, `bands.1.gain_db`).
/// Returns the old value (or `null` when it was defaulted).
pub fn set_param(effect: &mut Value, param: &str, value: Value) -> anyhow::Result<Value> {
    ensure!(
        !param.is_empty() && param != "type",
        "bad effect parameter {param:?}"
    );
    let ty = effect
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let segs: Vec<&str> = param.split('.').collect();
    let (last, parents) = segs.split_last().expect("non-empty");
    let mut work = effect.clone();
    let mut cur = &mut work;
    for s in parents {
        cur = match s.parse::<usize>() {
            Ok(i) => cur
                .as_array_mut()
                .and_then(|a| a.get_mut(i))
                .ok_or_else(|| anyhow!("{ty}: no element {i} in {param:?}"))?,
            Err(_) => cur
                .get_mut(*s)
                .ok_or_else(|| anyhow!("{ty}: no {s:?} in {param:?}"))?,
        };
    }
    let obj = cur
        .as_object_mut()
        .ok_or_else(|| anyhow!("{ty}: {param:?} does not name a parameter"))?;
    let old = obj.get(*last).cloned().unwrap_or(Value::Null);
    if value.is_null() {
        obj.remove(*last);
    } else {
        obj.insert(last.to_string(), value);
    }
    // Re-parse so unknown parameters and bad values are rejected here.
    serde_json::from_value::<EffectSpec>(work.clone())
        .with_context(|| format!("{ty}: setting {param}"))?;
    *effect = work;
    Ok(old)
}

/// Parse a chain from JSON (`null` = empty).
pub fn parse_chain(v: &Value) -> anyhow::Result<Vec<EffectSpec>> {
    if v.is_null() {
        return Ok(Vec::new());
    }
    let Some(arr) = v.as_array() else {
        bail!("effects must be an array of {{\"type\": ...}} objects");
    };
    arr.iter()
        .enumerate()
        .map(|(i, e)| serde_json::from_value(e.clone()).with_context(|| format!("effects[{i}]")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_and_round_trip() {
        let e: EffectSpec = serde_json::from_value(json!({"type": "compressor"})).unwrap();
        assert!(matches!(e, EffectSpec::Compressor { .. }));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(serde_json::from_value::<EffectSpec>(v).unwrap(), e);
        assert!(serde_json::from_value::<EffectSpec>(json!({"type": "gate", "bogus": 1})).is_err());
        assert!(serde_json::from_value::<EffectSpec>(json!({"type": "high_pass"})).is_err());
        let chain = parse_chain(&json!([
            {"type": "eq", "bands": [{"freq_hz": 1000, "gain_db": 3}]},
            {"type": "limiter", "ceiling_db": {"keyframes": [{"t": 0, "v": -1}, {"t": 2, "v": -6}]}}
        ]))
        .unwrap();
        validate(&chain, 48000, "clip").unwrap();
        let bad = parse_chain(&json!([{"type": "low_pass", "freq_hz": 30000}])).unwrap();
        assert!(validate(&bad, 48000, "clip").is_err());
    }

    #[test]
    fn set_param_paths() {
        let mut e = json!({"type": "eq", "bands": [{"freq_hz": 100}, {"freq_hz": 2000}]});
        set_param(&mut e, "bands.1.gain_db", json!(-4)).unwrap();
        assert_eq!(e["bands"][1]["gain_db"], json!(-4));
        assert!(set_param(&mut e, "bands.5.q", json!(1)).is_err());
        let mut c = json!({"type": "compressor"});
        let old = set_param(&mut c, "ratio", json!(8)).unwrap();
        assert_eq!(old, Value::Null);
        assert!(set_param(&mut c, "nope", json!(1)).is_err());
        assert_eq!(c, json!({"type": "compressor", "ratio": 8}));
    }
}
