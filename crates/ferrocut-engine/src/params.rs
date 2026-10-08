//! The settable parameters of timeline objects ([`ParamSpec`] registries) and
//! the generic setter behind the `set_param` / `set_keyframes` edit ops.
//!
//! A parameter is addressed by a target (a clip id, a track name, or the
//! timeline itself) and a dotted name from the target's registry:
//!
//! - video clips: [`VIDEO_CLIP`] (`opacity`, `transform.*`, `audio.*`, ...)
//! - audio-track clips: [`AUDIO_CLIP`] (`audio.*`)
//! - tracks (video or audio): [`TRACK`] (`bus.*`, the track's audio bus;
//!   `matte` on video tracks)
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
use TimeBase::{ClipLocal, Source as Src, Timeline as Tl};

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
            ParamSpec::fixed("audio.preserve_pitch", Bool, "", "false", "keep pitch when the clip is retimed (WSOLA time-stretch); false = varispeed"),
            s("speed", ClipLocal, "x", "1", "playback speed: 2 = twice as fast, -1 = reverse, 0 = freeze; keyframes = speed ramp (set_speed keeps the source range and changes the duration; setting this directly does not)").range(-100.0, 100.0),
            s("time_remap", ClipLocal, "s", "null", "AE time remap: keys map clip-local time to source seconds; overrides speed (must be 1); null removes it"),
            ParamSpec::fixed("audio.effects", Object, "", "[]", "clip audio effects in order, after gain/fades/pan: [{type: eq | high_pass | low_pass | compressor | limiter | gate, ...params}], every numeric param a rational or keyframes (clip-local); prefer add_effect / set_effect_param / remove_effect"),
        ]
    };
}

const CA: [ParamSpec; 10] = clip_audio_params!();

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
        "rotation in degrees, clockwise (the Z rotation of a 3D layer)",
    ),
    ParamSpec::fixed(
        "three_d",
        Bool,
        "",
        "false",
        "After Effects 3D layer switch: the clip becomes a card in 3D space seen through the timeline camera, with the transform.position_z / anchor_z / rotation_x / rotation_y / orientation params, depth-sorted against neighbouring 3D layers",
    ),
    s(
        "transform.position_z",
        ClipLocal,
        "px",
        "0",
        "3D layers: depth of the position, pixels, positive = away from the viewer",
    ),
    s(
        "transform.anchor_z",
        ClipLocal,
        "px",
        "0",
        "3D layers: depth of the anchor point, pixels",
    ),
    s(
        "transform.rotation_x",
        ClipLocal,
        "deg",
        "0",
        "3D layers: X rotation in degrees (positive tilts the bottom edge away)",
    ),
    s(
        "transform.rotation_y",
        ClipLocal,
        "deg",
        "0",
        "3D layers: Y rotation in degrees (positive brings the right edge towards the viewer)",
    ),
    s(
        "transform.orientation",
        ClipLocal,
        "deg",
        "[0, 0, 0]",
        "3D layers: orientation [x, y, z] in degrees, applied after the X/Y/Z rotations (outermost)",
    )
    .with_kind(Vec3),
    ParamSpec::fixed(
        "motion_blur",
        Bool,
        "",
        "false",
        "After Effects layer motion blur switch: blur the layer's transform (and camera) motion with the timeline's motion_blur shutter; needs timeline motion_blur",
    ),
    ParamSpec::fixed(
        "transition_in",
        Object,
        "",
        "null",
        "{kind: dissolve, duration}: prefer the add_transition op, which also makes the overlap",
    ),
    ParamSpec::fixed(
        "sampling",
        Choice,
        "",
        "\"nearest\"",
        "source frame sampling when retimed: nearest | frame_blend (optical_flow is a reserved hook)",
    ),
    ParamSpec::fixed(
        "blend_mode",
        Choice,
        "",
        "\"normal\"",
        "how the track composites onto the tracks below while this clip is active: normal | add | multiply | screen | overlay | soft_light | hard_light | darken | lighten | difference | exclusion | color_dodge | color_burn | hue | saturation | color | luminosity",
    ),
    ParamSpec::fixed(
        "generator",
        Object,
        "",
        "null",
        "generator clips only: {type: solid | linear_gradient | radial_gradient, ...}; add generator clips with add_clip {generator}",
    ),
    s(
        "generator.color",
        Src,
        "",
        "[1, 1, 1, 1]",
        "solid: color [r, g, b] or [r, g, b, a] in [0, 1], display-referred Rec.709, straight alpha; keys in source time",
    )
    .with_kind(Color)
    .range(0.0, 1.0),
    s(
        "generator.start_color",
        Src,
        "",
        "[0, 0, 0, 1]",
        "gradients: color at the start / center",
    )
    .with_kind(Color)
    .range(0.0, 1.0),
    s(
        "generator.end_color",
        Src,
        "",
        "[1, 1, 1, 1]",
        "gradients: color at the end / radius",
    )
    .with_kind(Color)
    .range(0.0, 1.0),
    s(
        "generator.start",
        Src,
        "px",
        "[0, h/2]",
        "linear_gradient: start point, output pixels",
    )
    .with_kind(Vec2),
    s(
        "generator.end",
        Src,
        "px",
        "[w, h/2]",
        "linear_gradient: end point, output pixels",
    )
    .with_kind(Vec2),
    s(
        "generator.center",
        Src,
        "px",
        "[w/2, h/2]",
        "radial_gradient: center, output pixels",
    )
    .with_kind(Vec2),
    s(
        "generator.radius",
        Src,
        "px",
        "half the frame diagonal",
        "radial_gradient: radius in pixels, >= 0",
    )
    .min(0.0),
    ParamSpec::fixed(
        "generator.interpolation",
        Choice,
        "",
        "\"display\"",
        "gradients: display (mix encoded colors, After Effects Gradient Ramp) | linear (mix linear light)",
    ),
    ParamSpec::fixed(
        "effects",
        Object,
        "",
        "[]",
        "video effects in order on the clip's picture, before its transform (opacity applies after): [{type, id?, enabled?, ...params}] (types and params: video_effects), keyframes in clip-local time; prefer add_video_effect / set_video_effect_param / remove_video_effect / move_video_effect",
    ),
    ParamSpec::fixed(
        "adjustment",
        Bool,
        "",
        "false",
        "adjustment layer: no source; its effects apply to the composite of the tracks below while it is active, mixed by its opacity and its track's matte (its track holds only adjustment clips)",
    ),
    CA[0],
    CA[1],
    CA[2],
    CA[3],
    CA[4],
    CA[5],
    CA[6],
    CA[7],
    CA[8],
    CA[9],
];

/// Parameters of a clip on an audio track.
pub const AUDIO_CLIP: &[ParamSpec] = &[
    CA[0], CA[1], CA[2], CA[3], CA[4], CA[5], CA[6], CA[7], CA[8], CA[9],
];

/// Parameters of a track's audio bus (video tracks' linked audio, or audio tracks).
pub const TRACK: &[ParamSpec] = &[
    ParamSpec::fixed(
        "matte",
        Object,
        "",
        "null",
        "video tracks only: {mode: alpha | alpha_inverted | luma | luma_inverted}; the track above becomes this track's matte and is not composited itself",
    ),
    ParamSpec::fixed(
        "effects",
        Object,
        "",
        "[]",
        "video tracks only: video effects in order on the track's picture, before its matte: [{type, id?, enabled?, ...params}], keyframes in timeline time; prefer add_video_effect / set_video_effect_param / remove_video_effect / move_video_effect",
    ),
    s("bus.gain_db", Tl, "dB", "0", "track bus gain"),
    s("bus.pan", Tl, "", "0", "track balance -1 .. 1").range(-1.0, 1.0),
    ParamSpec::fixed("bus.mute", Bool, "", "false", "mute the track's audio"),
    ParamSpec::fixed(
        "bus.effects",
        Object,
        "",
        "[]",
        "track audio effects in order on the summed clips, before bus gain/balance (pre-fader): [{type: eq | high_pass | low_pass | compressor | limiter | gate, ...params}], keyframes in timeline time; prefer add_effect / set_effect_param / remove_effect",
    ),
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
    ParamSpec::fixed(
        "camera",
        Object,
        "",
        "null",
        "{position, point_of_interest, zoom | fov_deg}: the camera 3D layers are seen through; null = After Effects' default 50 mm camera (an untransformed 3D layer looks like the 2D layer)",
    ),
    s(
        "camera.position",
        Tl,
        "px",
        "[w/2, h/2, -w*50/36]",
        "camera position [x, y, z], output pixels (z negative = in front of the layers); setting one component fills the others from the default camera",
    )
    .with_kind(Vec3),
    s(
        "camera.point_of_interest",
        Tl,
        "px",
        "[w/2, h/2, 0]",
        "point the camera looks at [x, y, z]",
    )
    .with_kind(Vec3),
    s(
        "camera.zoom",
        Tl,
        "px",
        "w*50/36",
        "distance at which a layer appears at 100 %, > 0 (exclusive with fov_deg; set the position z to -zoom to keep layers at z=0 at 100 %)",
    )
    .min(1e-9),
    s(
        "camera.fov_deg",
        Tl,
        "deg",
        "39.6 (from zoom)",
        "horizontal angle of view in degrees, in (0, 180) (exclusive with zoom)",
    )
    .range(1e-9, 179.999),
    ParamSpec::fixed(
        "motion_blur",
        Object,
        "",
        "null",
        "{shutter_angle, shutter_phase, samples}: enables motion blur for clips with motion_blur: true (After Effects composition switch); null = off",
    ),
    ParamSpec::fixed(
        "motion_blur.shutter_angle",
        Scalar,
        "deg",
        "180",
        "shutter angle in degrees, [0, 720]; 360 = the whole frame interval",
    )
    .range(0.0, 720.0),
    ParamSpec::fixed(
        "motion_blur.shutter_phase",
        Scalar,
        "deg",
        "-90",
        "where the shutter opens relative to the frame time, degrees, [-360, 360] (-90 with 180 centers it)",
    )
    .range(-360.0, 360.0),
    ParamSpec::fixed(
        "motion_blur.samples",
        Scalar,
        "",
        "16",
        "sub-frame samples per frame, 2..64 (integer)",
    )
    .range(2.0, 64.0),
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
        "video_effects": crate::fx::registry_json(),
        "expressions": crate::expr::language_json(),
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
pub(crate) fn vec_default(spec: &ParamSpec, frame: (u32, u32)) -> Value {
    let half = |v: u32| Rational::new(v as i64, 2).to_string();
    let zoom = || Rational::new(frame.0 as i64 * 50, 36);
    match spec.kind {
        Vec3 if spec.name == "camera.position" => {
            json!([half(frame.0), half(frame.1), (-zoom()).to_string()])
        }
        Vec3 if spec.name == "camera.point_of_interest" => {
            json!([half(frame.0), half(frame.1), "0"])
        }
        Color | Vec3 => {
            // The spec default is a JSON array of integers.
            let v: Vec<i64> = serde_json::from_str(spec.default).unwrap_or_default();
            Value::Array(v.into_iter().map(|x| json!(x.to_string())).collect())
        }
        _ if spec.name.ends_with("scale") => json!(["1", "1"]),
        _ if spec.name == "generator.start" => json!(["0", half(frame.1)]),
        _ if spec.name == "generator.end" => json!([frame.0.to_string(), half(frame.1)]),
        _ => json!([half(frame.0), half(frame.1)]),
    }
}

fn check_value_shape(spec: &ParamSpec, comp: Option<usize>, v: &Value) -> anyhow::Result<()> {
    let scalar_ok = |v: &Value| {
        v.is_string()
            || v.is_i64()
            || v.is_u64()
            || (v.is_object() && (v.get("keyframes").is_some() || v.get("expression").is_some()))
    };
    let ok = match (spec.kind, comp) {
        // Optional animatables (default null, e.g. time_remap): null removes.
        (Scalar, _) if v.is_null() && spec.default == "null" => true,
        (Choice, _) => v.is_string(),
        (Scalar, _) | (Vec2 | ScalarOrVec2 | Vec3 | Color, Some(_)) => {
            if spec.animatable {
                scalar_ok(v)
            } else {
                v.is_string() || v.is_i64() || v.is_u64()
            }
        }
        (Vec2, None) => v
            .as_array()
            .is_some_and(|a| a.len() == 2 && a.iter().all(scalar_ok)),
        (Vec3, None) => v
            .as_array()
            .is_some_and(|a| a.len() == 3 && a.iter().all(scalar_ok)),
        (Color, None) => v
            .as_array()
            .is_some_and(|a| (3..=4).contains(&a.len()) && a.iter().all(scalar_ok)),
        (ScalarOrVec2, None) => {
            scalar_ok(v)
                || v.as_array()
                    .is_some_and(|a| a.len() == 2 && a.iter().all(scalar_ok))
        }
        (Bool, _) => v.is_boolean(),
        (Time, _) => v.is_null() || v.is_string() || v.is_i64() || v.is_u64(),
        (Object, _) if spec.name.ends_with("effects") => v.is_null() || v.is_array(),
        (Object, _) => v.is_null() || v.is_object(),
    };
    ensure!(
        ok,
        "{}: expected {}, got {v}",
        spec.name,
        match (spec.kind, comp) {
            (Scalar, _) | (Vec2 | ScalarOrVec2 | Vec3 | Color, Some(_)) if spec.animatable =>
                "a rational (\"1/2\", \"0.5\", 2), {\"keyframes\": [...]} or {\"expression\": \"...\"}",
            (Scalar, _) => "a rational (\"1/2\", \"0.5\", 2)",
            (Vec2, None) => "[x, y] (each a rational or {\"keyframes\": [...]})",
            (Vec3, None) => "[x, y, z] (each a rational or {\"keyframes\": [...]})",
            (Color, None) =>
                "[r, g, b] or [r, g, b, a] (each a rational in [0, 1] or {\"keyframes\": [...]})",
            (ScalarOrVec2, None) => "a rational, {\"keyframes\": [...]} or [x, y]",
            (Bool, _) => "true or false",
            (Object, _) if spec.name.ends_with("effects") => "an array of effect objects or null",
            (Time, _) => "a rational time in seconds or null",
            (Choice, _) => "one of the strings listed in the parameter's doc",
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
                .or_insert_with(|| vec_default(spec, frame));
            if !e.is_array() {
                // uniform scale -> per-axis
                let u = e.clone();
                *e = json!([u.clone(), u]);
            }
            let a = e.as_array_mut().expect("array");
            while a.len() <= i {
                // `.a` of an opaque [r, g, b] color.
                a.push(json!("1"));
            }
            a[i] = value;
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
        // Colors: components, alpha added to an opaque color, defaults.
        let mut g = json!({"id": "g", "generator": {"type": "solid", "color": ["1", "0", "0"]}});
        let (sp, c) = lookup(Scope::VideoClip, "generator.color.a").unwrap();
        set(&mut g, Scope::VideoClip, sp, c, json!("1/2"), (64, 32)).unwrap();
        assert_eq!(g["generator"]["color"], json!(["1", "0", "0", "1/2"]));
        let mut g = json!({"id": "g", "generator": {"type": "linear_gradient"}});
        let (sp, c) = lookup(Scope::VideoClip, "generator.end_color.g").unwrap();
        set(&mut g, Scope::VideoClip, sp, c, json!("0"), (64, 32)).unwrap();
        assert_eq!(g["generator"]["end_color"], json!(["1", "0", "1", "1"]));
        let (sp, c) = lookup(Scope::VideoClip, "generator.end.y").unwrap();
        set(&mut g, Scope::VideoClip, sp, c, json!("0"), (64, 32)).unwrap();
        assert_eq!(g["generator"]["end"], json!(["64", "0"]));
        let (sp, c) = lookup(Scope::VideoClip, "generator.color").unwrap();
        assert!(set(&mut g, Scope::VideoClip, sp, c, json!(["1", "1"]), (64, 32)).is_err());
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
