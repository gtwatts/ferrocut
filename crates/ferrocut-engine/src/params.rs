//! The settable parameters of timeline objects ([`ParamSpec`] registries) and
//! the generic setter behind the `set_param` / `set_keyframes` edit ops.
//!
//! A parameter is addressed by a target (a clip id, a track name, or the
//! timeline itself) and a dotted name from the target's registry:
//!
//! - video clips: [`VIDEO_CLIP`] (`opacity`, `transform.*`, `audio.*`, ...)
//! - audio-track clips: [`AUDIO_CLIP`] (`audio.*`)
//! - tracks (video or audio): [`TRACK`] (`bus.*`, the track's audio bus)
//! - the timeline: [`TIMELINE`] (`audio.master_gain_db`, `audio.loudness.*`, ...)
//!
//! `<vec2>.x` / `<vec2>.y` address one component. Setting works on the
//! object's JSON form (missing parents are created with their defaults, `null`
//! removes an optional object), then the object is parsed back with the
//! timeline's strict serde types and range-checked, so a bad value is
//! reported, never written.

use anyhow::{Context as _, anyhow, bail, ensure};
use ferrocut_core::param::{self, ParamKind, ParamSpec, TimeBase};
use ferrocut_core::{Animatable, Rational, RationalTime};
use serde_json::{Value, json};

use ParamKind::*;
use TimeBase::{ClipLocal, Timeline as Tl};

const fn s(
    name: &'static str,
    time: TimeBase,
    unit: &'static str,
    d: &'static str,
    doc: &'static str,
) -> ParamSpec {
    ParamSpec::scalar(name, time, unit, d, doc)
}

/// Clip audio parameters (shared by video and audio clips).
macro_rules! clip_audio_params {
    () => {
        [
            s("audio.gain_db", ClipLocal, "dB", "0", "clip gain"),
            s("audio.pan", ClipLocal, "", "0", "pan / balance, -1 (left) .. 1 (right)").range(-1.0, 1.0),
            ParamSpec::fixed("audio.mute", Bool, "", "false", "mute the clip's audio"),
            ParamSpec::fixed("audio.fade_in", Object, "", "null", "{duration, curve: linear|equal_power}: fade in from the audio start"),
            ParamSpec::fixed("audio.fade_out", Object, "", "null", "{duration, curve}: fade out at the audio end"),
            ParamSpec::fixed("audio.crossfade_in", Object, "", "null", "{duration, curve}: crossfade from the clip whose audio ends exactly `duration` after this clip's audio starts"),
        ]
    };
}

const CA: [ParamSpec; 6] = clip_audio_params!();

/// Parameters of a clip on a video track.
pub const VIDEO_CLIP: &[ParamSpec] = &[
    s(
        "opacity",
        ClipLocal,
        "",
        "1",
        "layer opacity, 0 (transparent) .. 1",
    )
    .range(0.0, 1.0),
    s(
        "transform.position",
        ClipLocal,
        "px",
        "[w/2, h/2]",
        "where the anchor lands, output pixels [x, y] (default: frame center)",
    )
    .with_kind(Vec2),
    s(
        "transform.anchor",
        ClipLocal,
        "px",
        "[w/2, h/2]",
        "pivot in source pixels [x, y] (default: frame center)",
    )
    .with_kind(Vec2),
    s(
        "transform.scale",
        ClipLocal,
        "",
        "1",
        "scale factor: uniform, or [x, y] per axis (1 = 100 %)",
    )
    .with_kind(ScalarOrVec2),
    s(
        "transform.rotation",
        ClipLocal,
        "deg",
        "0",
        "rotation in degrees, clockwise",
    ),
    ParamSpec::fixed(
        "transition_in",
        Object,
        "",
        "null",
        "{kind: dissolve, duration}: prefer the add_transition op, which also makes the overlap",
    ),
    CA[0],
    CA[1],
    CA[2],
    CA[3],
    CA[4],
    CA[5],
];

/// Parameters of a clip on an audio track.
pub const AUDIO_CLIP: &[ParamSpec] = &[CA[0], CA[1], CA[2], CA[3], CA[4], CA[5]];

/// Parameters of a track's audio bus (video tracks' linked audio, or audio tracks).
pub const TRACK: &[ParamSpec] = &[
    s("bus.gain_db", Tl, "dB", "0", "track bus gain"),
    s("bus.pan", Tl, "", "0", "track balance -1 .. 1").range(-1.0, 1.0),
    ParamSpec::fixed("bus.mute", Bool, "", "false", "mute the track's audio"),
    ParamSpec::fixed(
        "bus.duck",
        Object,
        "",
        "null",
        "{key: [track names], threshold_db, ratio, attack_ms, release_ms, range_db}: sidechain ducking",
    ),
    s(
        "bus.duck.threshold_db",
        Tl,
        "dB",
        "-30",
        "key level above which ducking starts (needs bus.duck)",
    ),
    s(
        "bus.duck.ratio",
        Tl,
        "",
        "4",
        "reduction ratio above the threshold, >= 1",
    )
    .min(1.0),
    s("bus.duck.attack_ms", Tl, "ms", "10", "attack time, > 0").min(1e-9),
    s("bus.duck.release_ms", Tl, "ms", "250", "release time, > 0").min(1e-9),
    s(
        "bus.duck.range_db",
        Tl,
        "dB",
        "12",
        "maximum reduction, >= 0",
    )
    .min(0.0),
];

/// Timeline-level parameters.
pub const TIMELINE: &[ParamSpec] = &[
    s(
        "audio.master_gain_db",
        Tl,
        "dB",
        "0",
        "master gain before loudness normalization",
    ),
    ParamSpec::fixed(
        "audio.loudness",
        Object,
        "",
        "null",
        "{target_lufs, true_peak_dbtp}: two-pass loudness normalization + true-peak limiter",
    ),
    ParamSpec::fixed(
        "audio.loudness.target_lufs",
        Scalar,
        "LUFS",
        "-14",
        "integrated loudness target, [-70, 0) (not animatable: one gain for the whole program)",
    )
    .range(-70.0, 0.0),
    ParamSpec::fixed(
        "audio.loudness.true_peak_dbtp",
        Scalar,
        "dBTP",
        "-1",
        "true-peak ceiling, [-20, 0] (not animatable: one limiter ceiling)",
    )
    .range(-20.0, 0.0),
    ParamSpec::fixed(
        "output.duration",
        Time,
        "s",
        "null",
        "explicit output duration (default: end of the last clip); null clears it",
    ),
];

/// What a parameter edit targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    VideoClip,
    AudioClip,
    /// A video track (bus at `audio`) or an audio track (bus at `bus`).
    Track {
        audio_track: bool,
    },
    Timeline,
}

impl Scope {
    pub fn specs(self) -> &'static [ParamSpec] {
        match self {
            Scope::VideoClip => VIDEO_CLIP,
            Scope::AudioClip => AUDIO_CLIP,
            Scope::Track { .. } => TRACK,
            Scope::Timeline => TIMELINE,
        }
    }
}

/// Every registry as JSON (for the schema tool and docs).
pub fn registry_json() -> Value {
    json!({
        "video_clip": VIDEO_CLIP,
        "audio_clip": AUDIO_CLIP,
        "track": TRACK,
        "timeline": TIMELINE,
    })
}

/// Look `name` up in `scope`'s registry (with a helpful error).
pub fn lookup(scope: Scope, name: &str) -> anyhow::Result<(&'static ParamSpec, Option<usize>)> {
    param::find(scope.specs(), name).ok_or_else(|| {
        let names: Vec<&str> = scope.specs().iter().map(|s| s.name).collect();
        anyhow!(
            "unknown parameter {name:?} here; valid: {}",
            names.join(", ")
        )
    })
}

/// JSON pointer segments of a parameter inside its target object.
fn segments(scope: Scope, spec: &ParamSpec) -> Vec<String> {
    let mut name = spec.name.to_string();
    if let Scope::Track { audio_track } = scope {
        let field = if audio_track { "bus" } else { "audio" };
        name = name.replacen("bus", field, 1);
    }
    name.split('.').map(String::from).collect()
}

/// Default for a missing vector parameter (frame center for position/anchor).
fn vec2_default(spec: &ParamSpec, frame: (u32, u32)) -> Value {
    if spec.name.ends_with("scale") {
        json!(["1", "1"])
    } else {
        json!([
            Rational::new(frame.0 as i64, 2).to_string(),
            Rational::new(frame.1 as i64, 2).to_string()
        ])
    }
}

fn check_value_shape(spec: &ParamSpec, comp: Option<usize>, v: &Value) -> anyhow::Result<()> {
    let scalar_ok = |v: &Value| {
        v.is_string() || v.is_i64() || v.is_u64() || (v.is_object() && v.get("keyframes").is_some())
    };
    let ok = match (spec.kind, comp) {
        (Scalar, _) | (Vec2 | ScalarOrVec2, Some(_)) => {
            if spec.animatable {
                scalar_ok(v)
            } else {
                v.is_string() || v.is_i64() || v.is_u64()
            }
        }
        (Vec2, None) => v
            .as_array()
            .is_some_and(|a| a.len() == 2 && a.iter().all(scalar_ok)),
        (ScalarOrVec2, None) => {
            scalar_ok(v)
                || v.as_array()
                    .is_some_and(|a| a.len() == 2 && a.iter().all(scalar_ok))
        }
        (Bool, _) => v.is_boolean(),
        (Time, _) => v.is_null() || v.is_string() || v.is_i64() || v.is_u64(),
        (Object, _) => v.is_null() || v.is_object(),
    };
    ensure!(
        ok,
        "{}: expected {}, got {v}",
        spec.name,
        match (spec.kind, comp) {
            (Scalar, _) | (Vec2 | ScalarOrVec2, Some(_)) if spec.animatable =>
                "a rational (\"1/2\", \"0.5\", 2) or {\"keyframes\": [...]}",
            (Scalar, _) => "a rational (\"1/2\", \"0.5\", 2)",
            (Vec2, None) => "[x, y] (each a rational or {\"keyframes\": [...]})",
            (ScalarOrVec2, None) => "a rational, {\"keyframes\": [...]} or [x, y]",
            (Bool, _) => "true or false",
            (Time, _) => "a rational time in seconds or null",
            _ => "an object or null",
        }
    );
    Ok(())
}

/// Current value of a parameter in `obj` (`None`: unset = its default).
pub fn get(obj: &Value, scope: Scope, spec: &ParamSpec, comp: Option<usize>) -> Option<Value> {
    let mut cur = obj;
    for k in segments(scope, spec) {
        cur = cur.get(&k)?;
    }
    match comp {
        None => Some(cur.clone()),
        Some(i) => match cur {
            Value::Array(a) => a.get(i).cloned(),
            other => Some(other.clone()), // uniform scale: both axes
        },
    }
}

/// Set a parameter in the target's JSON. `frame` = output (w, h) for defaults.
pub fn set(
    obj: &mut Value,
    scope: Scope,
    spec: &ParamSpec,
    comp: Option<usize>,
    value: Value,
    frame: (u32, u32),
) -> anyhow::Result<()> {
    check_value_shape(spec, comp, &value)?;
    let segs = segments(scope, spec);
    let (last, parents) = segs.split_last().expect("non-empty name");
    let mut cur = obj;
    for k in parents {
        let m = cur
            .as_object_mut()
            .ok_or_else(|| anyhow!("{}: parent is not an object", spec.name))?;
        let e = m.entry(k.clone()).or_insert_with(|| json!({}));
        if e.is_null() {
            *e = json!({});
        }
        cur = e;
    }
    let m = cur
        .as_object_mut()
        .ok_or_else(|| anyhow!("{}: parent is not an object", spec.name))?;
    match comp {
        None if value.is_null() => {
            m.remove(last);
        }
        None => {
            m.insert(last.clone(), value);
        }
        Some(i) => {
            let e = m
                .entry(last.clone())
                .or_insert_with(|| vec2_default(spec, frame));
            if !e.is_array() {
                // uniform scale -> per-axis
                let u = e.clone();
                *e = json!([u.clone(), u]);
            }
            e.as_array_mut().expect("array")[i] = value;
        }
    }
    Ok(())
}

/// Range-check the (parsed-back) value of an animatable/numeric parameter.
pub fn check_range(obj: &Value, scope: Scope, spec: &ParamSpec) -> anyhow::Result<()> {
    if spec.min.is_none() && spec.max.is_none() {
        return Ok(());
    }
    let Some(v) = get(obj, scope, spec, None) else {
        return Ok(());
    };
    let vals: Vec<Value> = match v {
        Value::Array(a) => a,
        Value::Null => vec![],
        v => vec![v],
    };
    for v in vals {
        let a: Animatable = serde_json::from_value(v.clone())
            .with_context(|| format!("{}: bad value {v}", spec.name))?;
        spec.check(&a).map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

/// Keyframes for `set_keyframes`: parse, optionally shift from timeline time
/// to the parameter's time base, and merge with the current value.
pub fn keyframes_value(
    current: Option<&Value>,
    keys: &[Value],
    shift: Rational,
    merge: bool,
) -> anyhow::Result<Value> {
    ensure!(!keys.is_empty(), "keyframes must not be empty");
    let mut out: Vec<(RationalTime, Value)> = Vec::new();
    if merge
        && let Some(Value::Object(o)) = current
        && let Some(Value::Array(old)) = o.get("keyframes")
    {
        for k in old {
            let t: RationalTime = serde_json::from_value(k["t"].clone())?;
            out.push((t, k.clone()));
        }
    }
    for k in keys {
        let shown = k.to_string();
        let mut k = k.clone();
        let o = k
            .as_object_mut()
            .ok_or_else(|| anyhow!("keyframe must be an object {{t, v, interp?}}, got {shown}"))?;
        let t: RationalTime = serde_json::from_value(o.get("t").cloned().unwrap_or(Value::Null))
            .map_err(|e| anyhow!("keyframe t: {e}"))?;
        let t = RationalTime(t.0 - shift);
        o.insert("t".into(), json!(t.0.to_string()));
        out.retain(|(ot, _)| *ot != t);
        out.push((t, k));
    }
    out.sort_by_key(|(t, _)| *t);
    let v = json!({ "keyframes": out.into_iter().map(|(_, k)| k).collect::<Vec<_>>() });
    let a: Animatable = serde_json::from_value(v.clone()).context("keyframes")?;
    a.validate().map_err(anyhow::Error::msg)?;
    Ok(v)
}

/// Reject `set_keyframes` on parameters that can't be animated.
pub fn ensure_animatable(spec: &ParamSpec) -> anyhow::Result<()> {
    if !spec.animatable {
        bail!("{} is not animatable ({})", spec.name, spec.doc);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_are_unique_and_resolve() {
        for scope in [
            Scope::VideoClip,
            Scope::AudioClip,
            Scope::Track { audio_track: true },
            Scope::Timeline,
        ] {
            let mut seen = std::collections::HashSet::new();
            for s in scope.specs() {
                assert!(seen.insert(s.name), "{}", s.name);
                assert_eq!(lookup(scope, s.name).unwrap().0.name, s.name);
            }
        }
        assert_eq!(
            lookup(Scope::VideoClip, "transform.position.y").unwrap().1,
            Some(1)
        );
        assert!(
            lookup(Scope::VideoClip, "bogus")
                .unwrap_err()
                .to_string()
                .contains("opacity")
        );
    }

    #[test]
    fn sets_components_with_defaults_and_removes_with_null() {
        let mut clip = json!({"id": "a"});
        let (sp, c) = lookup(Scope::VideoClip, "transform.position.x").unwrap();
        set(&mut clip, Scope::VideoClip, sp, c, json!("10"), (64, 32)).unwrap();
        assert_eq!(clip["transform"]["position"], json!(["10", "16"]));
        let (sp, c) = lookup(Scope::VideoClip, "transform.scale.y").unwrap();
        clip["transform"]["scale"] = json!("2");
        set(&mut clip, Scope::VideoClip, sp, c, json!("3"), (64, 32)).unwrap();
        assert_eq!(clip["transform"]["scale"], json!(["2", "3"]));
        let (sp, c) = lookup(Scope::VideoClip, "audio.fade_in").unwrap();
        set(
            &mut clip,
            Scope::VideoClip,
            sp,
            c,
            json!({"duration": "1"}),
            (64, 32),
        )
        .unwrap();
        set(&mut clip, Scope::VideoClip, sp, c, Value::Null, (64, 32)).unwrap();
        assert!(clip["audio"].get("fade_in").is_none());
        let (sp, c) = lookup(Scope::VideoClip, "opacity").unwrap();
        assert!(set(&mut clip, Scope::VideoClip, sp, c, json!(true), (64, 32)).is_err());
    }

    #[test]
    fn track_bus_field_depends_on_track_kind() {
        let (sp, _) = lookup(Scope::Track { audio_track: false }, "bus.duck.ratio").unwrap();
        assert_eq!(
            segments(Scope::Track { audio_track: false }, sp),
            ["audio", "duck", "ratio"]
        );
        assert_eq!(
            segments(Scope::Track { audio_track: true }, sp),
            ["bus", "duck", "ratio"]
        );
    }

    #[test]
    fn keyframes_merge_and_shift() {
        let cur = json!({"keyframes": [{"t": "0", "v": "0"}, {"t": "2", "v": "1"}]});
        let v = keyframes_value(
            Some(&cur),
            &[json!({"t": "3", "v": "1/2"}), json!({"t": "4", "v": "0"})],
            Rational::from_int(2),
            true,
        )
        .unwrap();
        assert_eq!(
            v,
            json!({"keyframes": [{"t": "0", "v": "0"}, {"t": "1", "v": "1/2"}, {"t": "2", "v": "0"}]})
        );
        assert!(keyframes_value(None, &[], Rational::ZERO, false).is_err());
    }
}
