//! Hand-written JSON Schemas (draft 2020-12) for the tool inputs. Written out
//! rather than derived so every edit op is exact: one `oneOf` branch per op
//! with `op` as a `const`, required fields, `additionalProperties: false`,
//! and rationals as the strings the engine parses.

use serde_json::{Value, json};

/// Exact rational seconds/values: an integer, `"n"`, `"n/d"` or an exact decimal.
pub fn rational(desc: &str) -> Value {
    json!({
        "description": desc,
        "anyOf": [
            { "type": "integer" },
            { "type": "string", "pattern": "^-?[0-9]+(/[1-9][0-9]*|\\.[0-9]+)?$" }
        ]
    })
}

fn interp() -> Value {
    json!({
        "description": "Interpolation of the segment from this key to the next (default linear).",
        "anyOf": [
            { "enum": ["hold", "linear", "ease", "ease_in", "ease_out", "ease_in_out", "easy_ease"] },
            {
                "type": "object",
                "properties": { "bezier": {
                    "description": "CSS cubic-bezier control points [x1, y1, x2, y2].",
                    "type": "array", "minItems": 4, "maxItems": 4, "items": rational("control value")
                } },
                "required": ["bezier"], "additionalProperties": false
            },
            {
                "type": "object",
                "properties": { "speed": {
                    "description": "After Effects temporal ease; speeds in value units per second, influences in (0, 1].",
                    "type": "object",
                    "properties": {
                        "out_speed": rational("speed leaving this key"),
                        "out_influence": rational("influence leaving this key"),
                        "in_speed": rational("speed arriving at the next key"),
                        "in_influence": rational("influence arriving at the next key")
                    },
                    "required": ["out_speed", "out_influence", "in_speed", "in_influence"],
                    "additionalProperties": false
                } },
                "required": ["speed"], "additionalProperties": false
            }
        ]
    })
}

/// A constant or keyframed parameter (key times clip-local unless noted).
pub fn animatable(desc: &str) -> Value {
    json!({
        "description": desc,
        "anyOf": [
            rational("constant value"),
            {
                "type": "object",
                "properties": { "keyframes": {
                    "type": "array", "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "t": rational("key time in seconds"),
                            "v": rational("value"),
                            "interp": interp()
                        },
                        "required": ["t", "v"], "additionalProperties": false
                    }
                } },
                "required": ["keyframes"], "additionalProperties": false
            },
            expression()
        ]
    })
}

/// An expression on a numeric parameter (see the guide's "Expressions").
pub fn expression() -> Value {
    json!({
        "type": "object",
        "description": "After Effects-style expression (rhai syntax, sandboxed, deterministic): variables time, value, fps, frame, comp_time, duration, in_point; functions wiggle, seed_random, random, noise, linear, ease, ease_in, ease_out, clamp, lerp, value_at_time, loop_out, loop_in, param, layer(id).param, track(name).param, comp().param. Evaluated per frame when the timeline is validated; errors name the parameter, time, line and column.",
        "properties": {
            "expression": { "type": "string", "minLength": 1, "maxLength": 16384, "description": "the script; its result (a number) is the parameter's value. Integer division truncates: write 1.0 / 2" },
            "value": {
                "description": "pre-expression value (`value` in the script): a constant or {keyframes} in the parameter's time base; default: the parameter's default",
                "anyOf": [
                    rational("constant"),
                    { "type": "object", "properties": { "keyframes": { "type": "array" } }, "required": ["keyframes"] }
                ]
            },
            "time_offset": rational("added to the parameter time to get the script's `time` (set by split; normally omitted)")
        },
        "required": ["expression"], "additionalProperties": false
    })
}

fn fade() -> Value {
    json!({
        "type": "object",
        "properties": {
            "duration": rational("fade length in seconds"),
            "curve": { "enum": ["linear", "equal_power"], "default": "equal_power" }
        },
        "required": ["duration"], "additionalProperties": false
    })
}

fn clip_audio() -> Value {
    json!({
        "description": "Linked/clip audio settings.",
        "type": "object",
        "properties": {
            "in_offset": rational("audio start relative to the clip start (negative = J-cut)"),
            "out_offset": rational("audio end relative to the clip end (positive = L-cut)"),
            "gain_db": animatable("gain in dB"),
            "pan": animatable("pan -1..1"),
            "mute": { "type": "boolean" },
            "fade_in": fade(),
            "fade_out": fade(),
            "crossfade_in": fade(),
            "preserve_pitch": { "type": "boolean", "description": "keep pitch when the clip is retimed (WSOLA time-stretch); default false = varispeed (pitch follows speed)" }
        },
        "additionalProperties": false
    })
}

fn speed() -> Value {
    animatable(
        "playback speed, default 1: 2 = twice as fast, -1 = reverse, 0 = freeze; keyframes (clip-local) make a speed ramp. Range [-100, 100].",
    )
}

fn time_remap() -> Value {
    json!({
        "description": "After Effects-style time remap: keyframes from clip-local time to source time (seconds). Overrides speed (which must then stay 1); source_in is ignored.",
        "anyOf": [ { "type": "null" }, animatable("source seconds") ]
    })
}

fn blend_mode() -> Value {
    json!({
        "description": "How this clip's track composites onto the tracks below while the clip is active (linear-light, premultiplied; W3C/After Effects formulas). Default normal (= over).",
        "enum": ferrocut_engine::blend::BlendMode::ALL.iter().map(|m| m.name()).collect::<Vec<_>>()
    })
}

fn matte() -> Value {
    json!({
        "description": "Track matte: the track directly above is this track's matte source (and is not composited itself). alpha keeps this track where the matte is opaque, luma where it is bright (ACEScg luminance of the premultiplied matte, i.e. luminance times alpha); the _inverted variants keep the rest. The matte track's own linked audio still plays.",
        "anyOf": [
            { "type": "null" },
            {
                "type": "object",
                "properties": {
                    "mode": { "enum": ["alpha", "alpha_inverted", "luma", "luma_inverted"] },
                    "source": { "const": "track_above", "description": "default; other matte sources (e.g. vector masks) are a planned hook" }
                },
                "required": ["mode"], "additionalProperties": false
            }
        ]
    })
}

fn sampling() -> Value {
    json!({
        "description": "Source frame sampling for retimed clips: nearest (default) or frame_blend (mix of the two neighbouring source frames). optical_flow is a reserved hook and is rejected for now.",
        "enum": ["nearest", "frame_blend", "optical_flow"]
    })
}

/// A clip for `ripple_insert`: a video clip (on a video track) or an audio
/// clip (on an audio track; only id/source/source_in/duration/audio).
pub fn clip() -> Value {
    json!({
        "description": "Clip object; its start is set to `at`. Audio-track clips accept only id, source, source_in, duration, audio.",
        "type": "object",
        "properties": {
            "id": { "type": "string", "minLength": 1, "description": "unique across the timeline" },
            "source": { "type": "string", "minLength": 1, "description": "media path, or a timeline .json file (nested composition), relative to the timeline file's directory or absolute" },
            "start": rational("ignored: replaced by `at`"),
            "source_in": rational("source time of the clip's first frame (default 0)"),
            "duration": rational("clip length in seconds (> 0)"),
            "opacity": animatable("0..1, default 1"),
            "transform": transform(),
            "three_d": three_d(),
            "motion_blur": clip_motion_blur(),
            "transition_in": {
                "type": "object",
                "properties": {
                    "kind": { "const": "dissolve" },
                    "duration": rational("transition length in seconds")
                },
                "required": ["kind", "duration"], "additionalProperties": false
            },
            "speed": speed(),
            "time_remap": time_remap(),
            "sampling": sampling(),
            "blend_mode": blend_mode(),
            "audio": clip_audio(),
            "generator": generator(),
            "markers": markers(CLIP_MARKER_TIME),
            "masks": crate::native_schema::masks(),
            "effects": video_effects(),
            "adjustment": { "type": "boolean", "default": false, "description": "adjustment layer: no source; its effects apply to the composite of the tracks below while it is active, mixed by its opacity and its track's matte. Its track holds only adjustment clips; no transform, 3D, blend mode, speed or transition." }
        },
        "required": ["id", "duration"],
        "oneOf": [ { "required": ["source"] }, { "required": ["generator"] }, { "required": ["adjustment"], "properties": { "adjustment": { "const": true } } } ],
        "additionalProperties": false
    })
}

fn op(name: &str, desc: &str, props: Value, required: &[&str], example: Value) -> Value {
    let mut p = props.as_object().cloned().unwrap_or_default();
    p.insert("op".into(), json!({ "const": name }));
    let mut req = vec![json!("op")];
    req.extend(required.iter().map(|r| json!(r)));
    json!({
        "title": name,
        "description": desc,
        "type": "object",
        "properties": p,
        "required": req,
        "additionalProperties": false,
        "examples": [example]
    })
}

/// Schema of one video effect parameter from its spec.
fn video_param(s: &ferrocut_core::param::ParamSpec) -> Value {
    use ferrocut_core::param::ParamKind::*;
    let mut desc = s.doc.to_string();
    if !s.unit.is_empty() {
        desc += &format!(" [{}]", s.unit);
    }
    match (s.min, s.max) {
        (Some(a), Some(b)) => desc += &format!(" (range {a}..{b})"),
        (Some(a), None) => desc += &format!(" (>= {a})"),
        (None, Some(b)) => desc += &format!(" (<= {b})"),
        _ => {}
    }
    if s.default != "null" {
        desc += &format!(" default {}", s.default);
    }
    let comp = || animatable("component");
    match s.kind {
        Scalar | Time => animatable(&desc),
        Vec2 => {
            json!({ "description": desc, "type": "array", "minItems": 2, "maxItems": 2, "items": comp() })
        }
        Vec3 => {
            json!({ "description": desc, "type": "array", "minItems": 3, "maxItems": 3, "items": comp() })
        }
        Color => {
            json!({ "description": desc + " ([r, g, b] or [r, g, b, a])", "type": "array", "minItems": 3, "maxItems": 4, "items": comp() })
        }
        ScalarOrVec2 => json!({ "description": desc, "anyOf": [
            animatable("uniform"),
            { "type": "array", "minItems": 2, "maxItems": 2, "items": comp() }
        ] }),
        Bool => json!({ "description": desc, "type": "boolean" }),
        Choice if !s.choices.is_empty() => json!({ "description": desc, "enum": s.choices }),
        Choice | Object => json!({ "description": desc, "type": "string" }),
    }
}

/// One video effect: a branch per registered type (see `ferrocut_engine::fx`).
pub fn video_effect() -> Value {
    let branches: Vec<Value> = ferrocut_engine::fx::registered()
        .iter()
        .map(|e| {
            let mut props = serde_json::Map::new();
            props.insert("type".into(), json!({ "const": e.type_name() }));
            props.insert("id".into(), json!({ "type": "string", "minLength": 1, "description": "name, unique in the stack (ops and errors refer to it)" }));
            props.insert("enabled".into(), json!({ "type": "boolean", "default": true }));
            props.insert("clock_offset".into(), rational("intrinsic procedural phase in seconds; split/trim maintains this automatically, default 0"));
            for p in e.params() {
                props.insert(p.name.into(), video_param(p));
            }
            json!({
                "title": e.type_name(),
                "description": e.doc(),
                "type": "object",
                "properties": props,
                "required": ["type"],
                "additionalProperties": false
            })
        })
        .collect();
    json!({
        "description": "{type, id?, enabled?, ...params}; numeric params take a constant, {keyframes} (clip-local on clips, timeline time on tracks) or {expression}",
        "oneOf": branches
    })
}

fn video_effects() -> Value {
    json!({
        "description": "Video effects in order (first applied first). On a clip: on its picture before its transform, then the clip opacity; on a track: on the track's picture before its matte; on an adjustment clip: on the composite below. Prefer the add_video_effect / set_video_effect_param / remove_video_effect / move_video_effect ops.",
        "type": "array",
        "items": video_effect()
    })
}

fn effect_ref() -> Value {
    json!({
        "description": "the effect's index in the stack, or its id",
        "anyOf": [ { "type": "integer", "minimum": 0 }, { "type": "string", "minLength": 1 } ]
    })
}

fn video_target() -> (Value, Value) {
    (
        json!({ "type": "string", "minLength": 1, "description": "clip id (incl. adjustment layers)" }),
        json!({ "type": "string", "minLength": 1, "description": "video track name (track effects)" }),
    )
}

/// One audio effect (see `ferrocut_engine::audio_fx`).
pub fn effect() -> Value {
    let types: Vec<&str> = ferrocut_engine::audio_fx::TYPES
        .iter()
        .map(|(t, _)| *t)
        .collect();
    json!({
        "type": "object",
        "description": "{type, ...params}",
        "required": ["type"],
        "properties": { "type": { "enum": types } }
    })
}

/// A generator layer (see `ferrocut_engine::generator`).
pub fn generator() -> Value {
    let color = |d: &str| {
        json!({
            "description": d,
            "type": "array", "minItems": 3, "maxItems": 4,
            "items": animatable("component in [0, 1]")
        })
    };
    let xy = |d: &str| {
        json!({
            "description": d,
            "type": "array", "minItems": 2, "maxItems": 2,
            "items": animatable("output pixels")
        })
    };
    let space = json!({ "enum": ["display", "linear"], "default": "display",
        "description": "display: mix the encoded colors (After Effects Gradient Ramp); linear: mix linear light" });
    json!({
        "description": "Native synthesized layer: solids, gradients, explicit-font text, or editable vector shapes. Colors are encoded Rec.709 straight RGBA; output is linear ACEScg premultiplied. Numeric keys use source time (follows speed/remap). Text fonts are explicit project assets and their bytes affect cache keys.",
        "oneOf": [
            {
                "type": "object",
                "properties": { "type": {"const":"text"}, "text": crate::native_schema::text() },
                "required": ["type","text"], "additionalProperties": false
            },
            {
                "type": "object",
                "properties": { "type": {"const":"shape"}, "shape": crate::native_schema::shape() },
                "required": ["type","shape"], "additionalProperties": false
            },
            {
                "type":"object",
                "properties":{"type":{"const":"vector_group"},"group":crate::native_schema::vector_group()},
                "required":["type","group"],"additionalProperties":false
            },
            {
                "type": "object",
                "properties": { "type": { "const": "solid" }, "color": color("fill color") },
                "required": ["type", "color"], "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "linear_gradient" },
                    "start": xy("start point (default [0, h/2])"),
                    "end": xy("end point (default [w, h/2])"),
                    "start_color": color("color at start (default black)"),
                    "end_color": color("color at end (default white)"),
                    "interpolation": space.clone()
                },
                "required": ["type"], "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "radial_gradient" },
                    "center": xy("center (default frame center)"),
                    "radius": animatable("radius in pixels (default half the frame diagonal)"),
                    "start_color": color("color at the center (default black)"),
                    "end_color": color("color at the radius and beyond (default white)"),
                    "interpolation": space
                },
                "required": ["type"], "additionalProperties": false
            }
        ]
    })
}

fn clip_id() -> Value {
    json!({ "type": "string", "minLength": 1, "description": "clip id" })
}

/// One edit operation (`oneOf`, discriminated by `op`).
pub fn edit_op() -> Value {
    let ops = vec![
        op(
            "split",
            "Split `clip` at timeline time `at` (strictly inside the clip); the right part gets `new_id` (default `<clip>.2`; must be unused). The left part keeps the incoming transition/fade-in, the right the fade-out.",
            json!({ "clip": clip_id(), "at": rational("timeline time"), "new_id": { "type": "string", "minLength": 1 } }),
            &["clip", "at"],
            json!({ "op": "split", "clip": "cam_a", "at": "5/2" }),
        ),
        op(
            "trim",
            "Move one edge of `clip` by `delta` seconds without moving neighbours (in: start and source_in move together; out: the end moves). Bounded by neighbours and source media.",
            json!({ "clip": clip_id(), "edge": { "enum": ["in", "out"] }, "delta": rational("seconds, + = later") }),
            &["clip", "edge", "delta"],
            json!({ "op": "trim", "clip": "logo", "edge": "out", "delta": "-1/2" }),
        ),
        op(
            "ripple_delete",
            "Remove `clip` and pull later clips on its track left to close the space it occupied; all_tracks also pulls other tracks' clips that start after it (sync lock).",
            json!({ "clip": clip_id(), "all_tracks": { "type": "boolean", "default": false } }),
            &["clip"],
            json!({ "op": "ripple_delete", "clip": "grad" }),
        ),
        op(
            "ripple_insert",
            "Insert `clip` on `track` at `at`, pushing clips that start at or after `at` right by its duration (on every track if all_tracks). `at` must not be inside a clip (split first).",
            json!({ "track": { "type": "string", "minLength": 1, "description": "track name (video or audio)" }, "at": rational("timeline time"), "clip": clip(), "all_tracks": { "type": "boolean", "default": false } }),
            &["track", "at", "clip"],
            json!({ "op": "ripple_insert", "track": "V1", "at": "5", "clip": { "id": "ins", "source": "../media/bars.mov", "duration": "1" } }),
        ),
        op(
            "roll",
            "Move the edit point between `clip` and the clip right after it by `delta` (the left clip's out and the right clip's in move together; total duration unchanged).",
            json!({ "clip": clip_id(), "delta": rational("seconds, + = later") }),
            &["clip", "delta"],
            json!({ "op": "roll", "clip": "cam_a", "delta": "1/4" }),
        ),
        op(
            "slip",
            "Shift which source frames `clip` shows by `delta` (source_in += delta); position and duration unchanged. Bounded by source media length.",
            json!({ "clip": clip_id(), "delta": rational("seconds") }),
            &["clip", "delta"],
            json!({ "op": "slip", "clip": "cam_b", "delta": "1/2" }),
        ),
        op(
            "slide",
            "Move `clip` by `delta` between its neighbours, trimming the previous clip's out and the next clip's in to compensate; track duration unchanged.",
            json!({ "clip": clip_id(), "delta": rational("seconds, + = later") }),
            &["clip", "delta"],
            json!({ "op": "slide", "clip": "cam_b", "delta": "-1/4" }),
        ),
        op(
            "move",
            "Place `clip` to start at `to`, optionally on another `track` of the same kind; the destination range must be free.",
            json!({ "clip": clip_id(), "to": rational("new start, timeline time"), "track": { "type": "string", "minLength": 1 } }),
            &["clip", "to"],
            json!({ "op": "move", "clip": "logo", "to": "7" }),
        ),
        op(
            "jl_cut",
            "Set a video clip's linked-audio offsets: in_offset < 0 is a J-cut (audio leads), out_offset > 0 an L-cut (audio trails). Omitted offsets are unchanged.",
            json!({ "clip": clip_id(), "in_offset": rational("seconds relative to the clip start"), "out_offset": rational("seconds relative to the clip end") }),
            &["clip"],
            json!({ "op": "jl_cut", "clip": "cam_b", "in_offset": "-1", "out_offset": "1/2" }),
        ),
        op(
            "set_speed",
            "Constant speed for a clip (Premiere Speed/Duration): keeps the clip's source range, so its duration becomes range/|speed|. speed 2 = twice as fast (half as long), 1/2 = slow motion, -1 = reverse. ripple moves later clips on the track by the duration change (otherwise a longer clip must fit before the next one). preserve_pitch keeps the audio's pitch (WSOLA). For ramps use set_keyframes on `speed`; for AE time remap set_keyframes on `time_remap`.",
            json!({
                "clip": clip_id(),
                "speed": rational("speed factor, != 0, in [-100, 100]"),
                "ripple": { "type": "boolean", "default": false },
                "preserve_pitch": { "type": "boolean" }
            }),
            &["clip", "speed"],
            json!({ "op": "set_speed", "clip": "cam_b", "speed": "1/2", "ripple": true, "preserve_pitch": true }),
        ),
        op(
            "freeze_frame",
            "Hold the frame `clip` shows at timeline time `at`. Without duration the clip is split at `at` and the rest becomes a freeze (Premiere Add Frame Hold); with duration a hold of that length is inserted at `at` and the rest of the clip and later clips on the track move right (Insert Frame Hold Segment; all_tracks moves every track). The hold's audio is muted. new_id names the hold clip.",
            json!({
                "clip": clip_id(),
                "at": rational("timeline time inside the clip"),
                "duration": rational("hold length in seconds (insert mode)"),
                "new_id": { "type": "string", "minLength": 1 },
                "all_tracks": { "type": "boolean", "default": false }
            }),
            &["clip", "at"],
            json!({ "op": "freeze_frame", "clip": "cam_a", "at": "3", "duration": "2" }),
        ),
        op(
            "nest",
            "Nest video clips into a new composition (AE Pre-compose / Premiere Nest): writes a new timeline file at `path` (must not exist; relative to the timeline's directory) holding the clips with their tracks, timing (shifted so the earliest starts at 0), effects and linked audio, and replaces them with one clip (`id`, default the file stem) spanning them on the lowest of their tracks. Dissolves and J/L audio must stay inside the selection.",
            json!({
                "clips": { "type": "array", "minItems": 1, "items": clip_id() },
                "path": { "type": "string", "pattern": "\\.json$", "description": "new comp file, e.g. comps/intro.json" },
                "id": { "type": "string", "minLength": 1 }
            }),
            &["clips", "path"],
            json!({ "op": "nest", "clips": ["title", "bg"], "path": "comps/intro.json", "id": "intro" }),
        ),
        op(
            "unnest",
            "Replace a nested-comp clip by the comp's clips (trimmed to the part the clip shows, placed at the same timeline times): inner track 0 goes on the clip's track, further inner tracks on new tracks inserted directly above it. The comp clip must be plain (no speed, transform, opacity, blend mode, transition or audio settings) and the comp must have no audio tracks or bus/master settings. The comp file is left as is.",
            json!({ "clip": clip_id() }),
            &["clip"],
            json!({ "op": "unnest", "clip": "intro" }),
        ),
        op(
            "add_effect",
            "Insert an audio effect on a clip (`clip`: after its gain/fades/pan, keyframes clip-local) or a track bus (`track`: on the summed clips before bus gain/balance, keyframes in timeline time) at `index` (default: end of the chain). Types and params (defaults in parentheses; every numeric param a rational or {keyframes}): eq {bands: [{kind: peak|low_shelf|high_shelf, freq_hz, gain_db (0), q (0.7071)}]}; high_pass / low_pass {freq_hz, q (0.7071)}; compressor {threshold_db (-20), ratio (4), attack_ms (10), release_ms (100), knee_db (6), makeup_db (0)}; limiter {ceiling_db (-1), release_ms (50)}; gate {threshold_db (-50), range_db (40), attack_ms (1), hold_ms (50), release_ms (100)}.",
            json!({
                "clip": clip_id(),
                "track": { "type": "string", "minLength": 1, "description": "track name (its audio bus)" },
                "effect": effect(),
                "index": { "type": "integer", "minimum": 0 }
            }),
            &["effect"],
            json!({ "op": "add_effect", "clip": "interview", "effect": { "type": "compressor", "threshold_db": "-24", "ratio": "3" } }),
        ),
        op(
            "set_effect_param",
            "Set one parameter of effect `index` in a clip's or track's audio effect chain: `param` is a dotted path inside the effect (threshold_db, bands.1.gain_db); `value` a rational, {keyframes} or null (back to the default).",
            json!({
                "clip": clip_id(),
                "track": { "type": "string", "minLength": 1, "description": "track name (its audio bus)" },
                "index": { "type": "integer", "minimum": 0 },
                "param": { "type": "string", "minLength": 1 },
                "value": { "anyOf": [animatable("parameter value"), { "type": "null" }] }
            }),
            &["index", "param", "value"],
            json!({ "op": "set_effect_param", "track": "Music", "index": 0, "param": "bands.0.gain_db", "value": "-3" }),
        ),
        op(
            "remove_effect",
            "Remove effect `index` from a clip's or track's audio effect chain.",
            json!({
                "clip": clip_id(),
                "track": { "type": "string", "minLength": 1, "description": "track name (its audio bus)" },
                "index": { "type": "integer", "minimum": 0 }
            }),
            &["index"],
            json!({ "op": "remove_effect", "clip": "interview", "index": 0 }),
        ),
        op(
            "add_video_effect",
            "Insert a registered video effect on a clip (`clip`; keyframes clip-local; incl. adjustment layers) or a video track (`track`; timeline time) at `index` (default: end = applied last). Discover available types, parameters, ranges and defaults in the `effect` schema and timeline_schema `params` (`video_effects`). Includes native blurs, transforms, grading and chroma/luma keying.",
            json!({
                "clip": video_target().0,
                "track": video_target().1,
                "effect": video_effect(),
                "index": { "type": "integer", "minimum": 0 }
            }),
            &["effect"],
            json!({ "op": "add_video_effect", "clip": "title", "effect": { "type": "gaussian_blur", "id": "soft", "sigma": "4" } }),
        ),
        op(
            "set_video_effect_param",
            "Set one parameter of a video effect (by index or id): `param` is a parameter name (sigma, color, color.g, position.x), `enabled` (bypass with false) or `id`; `value` a constant, {keyframes}, {expression}, array, boolean, string, or null (back to the default).",
            json!({
                "clip": video_target().0,
                "track": video_target().1,
                "effect": effect_ref(),
                "param": { "type": "string", "minLength": 1 },
                "value": { "anyOf": [
                    animatable("number or keyframes"),
                    { "type": "array" },
                    { "type": "boolean" },
                    { "type": "string" },
                    { "type": "null" }
                ] }
            }),
            &["effect", "param", "value"],
            json!({ "op": "set_video_effect_param", "clip": "title", "effect": "soft", "param": "sigma", "value": { "keyframes": [ { "t": "0", "v": "0" }, { "t": "1", "v": "8" } ] } }),
        ),
        op(
            "remove_video_effect",
            "Remove a video effect (by index or id) from a clip's or video track's stack.",
            json!({
                "clip": video_target().0,
                "track": video_target().1,
                "effect": effect_ref()
            }),
            &["effect"],
            json!({ "op": "remove_video_effect", "clip": "title", "effect": "soft" }),
        ),
        op(
            "move_video_effect",
            "Reorder: move a video effect (by index or id) to position `to` of its stack (0 = applied first).",
            json!({
                "clip": video_target().0,
                "track": video_target().1,
                "effect": effect_ref(),
                "to": { "type": "integer", "minimum": 0 }
            }),
            &["effect", "to"],
            json!({ "op": "move_video_effect", "clip": "title", "effect": 1, "to": 0 }),
        ),
        op(
            "add_track",
            "Add an empty track: kind video (index 0 = bottom layer; default: on top) or audio (default: last). Names must be unique across all tracks.",
            json!({
                "kind": { "enum": ["video", "audio"] },
                "name": { "type": "string", "minLength": 1 },
                "index": { "type": "integer", "minimum": 0, "description": "position among tracks of that kind" }
            }),
            &["kind", "name"],
            json!({ "op": "add_track", "kind": "audio", "name": "Music" }),
        ),
        op(
            "add_clip",
            "Add a clip to a track: media (`source`), native generator (`generator`: solid, linear_gradient, radial_gradient, text or shape; video track; duration required), or adjustment layer (adjustment:true; duration required). Text uses explicit project font assets. Media is probed for the requested stream. Defaults: source_in 0, duration remaining media, start track end, unique id from source/generator. The range must be free; use ripple_insert to push clips right. All numeric generator parameters use source time.",
            json!({
                "track": { "type": "string", "minLength": 1, "description": "track name" },
                "source": path("media path or nested timeline (.json), relative to the timeline file's directory (or absolute, inside the project root)"),
                "generator": generator(),
                "id": { "type": "string", "minLength": 1, "description": "clip id (unique)" },
                "start": rational("timeline time of the clip's first frame"),
                "source_in": rational("source time of the clip's first frame"),
                "duration": rational("clip length in seconds"),
                "adjustment": { "type": "boolean", "default": false, "description": "an adjustment layer (no source/generator, `duration` required): its video effects apply to the composite below; its track holds only adjustment clips" }
            }),
            &["track"],
            json!({ "op": "add_clip", "track": "V1", "source": "media/s1.mkv", "source_in": "1", "duration": "4" }),
        ),
        op(
            "add_transition",
            "Add a dissolve into `clip` from the clip before it on its video track. For clips that meet at a cut the overlap comes from source handles, nothing else moves: align center (default; half before, half after the cut), start (begins at the cut: the previous clip runs longer) or end (ends at the cut: `clip` starts earlier). Clips that already overlap by >= duration just get the transition. Linked audio gets a matching crossfade when its offsets are zero.",
            json!({
                "clip": clip_id(),
                "duration": rational("dissolve length in seconds"),
                "kind": { "const": "dissolve", "default": "dissolve" },
                "align": { "enum": ["center", "start", "end"], "default": "center" }
            }),
            &["clip", "duration"],
            json!({ "op": "add_transition", "clip": "b", "duration": "1" }),
        ),
        op(
            "set_param",
            "Set one parameter by name on a clip (`clip`), a track's audio bus (`track`) or the timeline (neither). Names: see timeline_schema `params` (e.g. opacity, transform.position, transform.position.x, transform.scale, transform.rotation, audio.gain_db, audio.pan, audio.mute, audio.fade_in, bus.gain_db, bus.duck, bus.duck.ratio, audio.master_gain_db, audio.loudness, audio.loudness.target_lufs, output.duration). Numeric parameters take a constant, {keyframes} or {expression, value?} (see the guide's Expressions); objects take an object, or null to remove.",
            json!({
                "clip": clip_id(),
                "track": { "type": "string", "minLength": 1, "description": "track name (its audio bus)" },
                "param": { "type": "string", "minLength": 1, "description": "parameter name" },
                "value": param_value()
            }),
            &["param", "value"],
            json!({ "op": "set_param", "clip": "a", "param": "opacity", "value": "1/2" }),
        ),
        op(
            "set_keyframes",
            "Keyframe an animatable parameter (see set_param for targets/names). mode replace (default) sets exactly these keys; merge keeps existing keys at other times. Key times are in the parameter's time base (clip-local for clip parameters: 0 = clip start; timeline time for bus/master) unless timeline_time=true, which converts timeline times for you.",
            json!({
                "clip": clip_id(),
                "track": { "type": "string", "minLength": 1 },
                "param": { "type": "string", "minLength": 1 },
                "keyframes": keyframes(),
                "mode": { "enum": ["replace", "merge"], "default": "replace" },
                "timeline_time": { "type": "boolean", "default": false }
            }),
            &["param", "keyframes"],
            json!({ "op": "set_keyframes", "clip": "a", "param": "opacity", "keyframes": [{ "t": "0", "v": "0" }, { "t": "1", "v": "1" }] }),
        ),
        op(
            "add_marker",
            "Add a marker: on the timeline (no `clip`; `time` in timeline seconds) or on a clip (`clip`; `time` in the clip's source seconds, or timeline seconds with timeline_time=true, which must fall inside the clip). Optional duration makes a range marker. `id` defaults to m1, m2, ... (unique in that list). Markers annotate (beats, shots, problems, decisions) and never change the render.",
            json!({
                "clip": clip_id(),
                "time": rational("marker time"),
                "id": { "type": "string", "minLength": 1 },
                "duration": rational("range length in seconds (default 0)"),
                "name": { "type": "string" },
                "color": marker_color(),
                "comment": { "type": "string" },
                "timeline_time": { "type": "boolean", "default": false }
            }),
            &["time"],
            json!({ "op": "add_marker", "time": "12", "name": "music drop", "color": "red" }),
        ),
        op(
            "update_marker",
            "Change the given fields of marker `id` (timeline marker, or `clip`'s marker). time follows add_marker's rules (timeline_time for clip markers).",
            json!({
                "clip": clip_id(),
                "id": { "type": "string", "minLength": 1 },
                "time": rational("new time"),
                "duration": rational("new range length (0 = point marker)"),
                "name": { "type": "string" },
                "color": marker_color(),
                "comment": { "type": "string" },
                "timeline_time": { "type": "boolean", "default": false }
            }),
            &["id"],
            json!({ "op": "update_marker", "id": "m1", "color": "green", "comment": "fixed" }),
        ),
        op(
            "remove_marker",
            "Remove marker `id` from the timeline, or from `clip`.",
            json!({ "clip": clip_id(), "id": { "type": "string", "minLength": 1 } }),
            &["id"],
            json!({ "op": "remove_marker", "clip": "cam_a", "id": "m2" }),
        ),
        op(
            "relink",
            "Point clips at moved or offline media (media_status lists offline files). One of: `clip` + `to` (every clip using that clip's source gets `to`); `from` + `to` (a file, or a directory: every source under it keeps its relative path below `to`); `search` (+ optional `clip`): every offline source is looked up by file name under that directory (recursive, hidden folders skipped; a name found twice is an error). Paths are relative to the timeline's directory. New files must exist and be long enough for the clips.",
            json!({
                "clip": clip_id(),
                "from": path("old file or directory"),
                "to": path("new file (or directory with from)"),
                "search": path("directory to search for offline files")
            }),
            &[],
            json!({ "op": "relink", "search": "media_moved" }),
        ),
    ];
    json!({ "oneOf": ops })
}

/// A keyframe list: `[{t, v, interp?}, ...]`.
pub fn keyframes() -> Value {
    json!({
        "type": "array", "minItems": 1,
        "items": {
            "type": "object",
            "properties": {
                "t": rational("key time in seconds"),
                "v": rational("value"),
                "interp": interp()
            },
            "required": ["t", "v"], "additionalProperties": false
        }
    })
}

/// `set_param.value`: a number/keyframes, `[x, y]`, a bool, an object, or null.
fn param_value() -> Value {
    json!({
        "description": "rational/keyframes/expression, vector/color, string (text/path/choice), boolean, object, ordered animator/effect/font list, or null; target parameter registry defines the accepted shape",
        "anyOf": [
            animatable("numeric value"),
            { "type": "string", "description": "editable text, explicit asset path, or enum choice; parameter-specific validation applies" },
            { "type": "array", "minItems": 2, "maxItems": 4, "items": animatable("component"), "description": "[x, y], [x, y, z] or a color [r, g, b(, a)]" },
            { "type": "boolean" },
            { "type": "object" },
            { "type": "array", "items": { "type": "object" }, "description": "audio.effects / bus.effects chain" },
            { "type": "array", "items": { "type": "string" }, "description": "explicit fallback font assets" },
            { "type": "null" }
        ]
    })
}

fn path(desc: &str) -> Value {
    json!({ "type": "string", "minLength": 1, "description": desc })
}

fn object(props: Value, required: &[&str]) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false
    })
}

const TL: &str = "timeline JSON file (absolute, or relative to the server's working directory)";

pub fn timeline_get() -> Value {
    object(json!({ "timeline": path(TL) }), &["timeline"])
}

pub fn edit_apply() -> Value {
    object(
        json!({
            "timeline": path(TL),
            "ops": { "type": "array", "minItems": 1, "items": edit_op(), "description": "applied atomically, in order" },
            "dry_run": { "type": "boolean", "default": false, "description": "apply and report only; write nothing, journal nothing" },
            "output": path("write here instead of editing in place (journal goes beside it)"),
            "plan": { "type": "boolean", "default": false, "description": "also report which output chunks would re-render (hashes the source media)" },
            "probe": { "type": "boolean", "default": true, "description": "bound trims/slips by probed media lengths" },
            "return_timeline": { "type": "boolean", "default": false, "description": "include the resulting timeline JSON" }
        }),
        &["timeline", "ops"],
    )
}

pub fn diff() -> Value {
    object(
        json!({
            "a": path("old timeline file"),
            "b": path("new timeline file"),
            "render": { "type": "boolean", "default": true, "description": "compute the chunks that would re-render (needs the media)" }
        }),
        &["a", "b"],
    )
}

pub fn render() -> Value {
    object(
        json!({
            "timeline": path(TL),
            "output": path("output .mkv (FFV1 + PCM master)"),
            "jobs": { "type": "integer", "minimum": 1, "maximum": 32, "description": "parallel chunk workers (default: 4, lowered to fit free VRAM; backs off automatically if the GPU runs out of memory)" },
            "force": { "type": "boolean", "default": false, "description": "re-render every chunk even if cached" },
            "cache_dir": path("chunk cache directory (default <output dir>/.ferrocut-cache)"),
            "report": path("report JSON path (default <output>.report.json)"),
            "cpu": { "type": "boolean", "default": false, "description": "render on the software (CPU) Vulkan adapter (Mesa lavapipe)" },
            "timeout_s": { "type": "number", "exclusiveMinimum": 0, "description": "cancel the render after this many seconds" },
            "check": { "type": "boolean", "default": false, "description": "run the perceptual quality check (ferrocut-perceive) on the result; skipped if the checker isn't installed" },
            "check_args": check_args(),
            "expect_audio": expect_audio(),
            "deliver": deliver(),
            "proxies": { "type": "boolean", "default": false, "description": "draft render: read video from half-resolution proxies where they exist (proxy_generate); the summary gains draft=true and the proxies used. Ignored with deliver (a final render always uses the original media). Draft chunks are cached apart from full-resolution ones." }
        }),
        &["timeline", "output"],
    )
}

/// `render.deliver`: "mp4" or an object with options.
fn deliver() -> Value {
    json!({
        "description": "after rendering (and after check passes, if check=true), also encode a delivery file from the master: \"mp4\" = H.264 + AAC via Cisco's OpenH264. Uses the codec only if the user already enabled it (see the openh264 tool); never downloads it. IDRs land on render chunk boundaries; the summary gains a `deliver` section and <output>.deliver.json is written.",
        "oneOf": [
            { "type": "string", "enum": ["mp4"] },
            {
                "type": "object",
                "properties": {
                    "format": { "type": "string", "enum": ["mp4"], "default": "mp4" },
                    "output": path("delivery file (default: the render output with a .mp4 extension)"),
                    "qp": { "type": "integer", "minimum": 0, "maximum": 51, "default": 20, "description": "constant QP (lower = better quality, bigger file)" },
                    "audio": { "type": "boolean", "default": true, "description": "include the master's audio as AAC" },
                    "jobs": { "type": "integer", "minimum": 1, "maximum": 32, "description": "parallel CPU encoders (default: the render's jobs)" }
                },
                "additionalProperties": false
            }
        ]
    })
}

pub fn openh264() -> Value {
    object(
        json!({
            "action": {
                "type": "string",
                "enum": ["status", "enable", "disable", "license"],
                "description": "status: installed/enabled state (no network). enable: DOWNLOADS Cisco's OpenH264 binary from Cisco and records the user's consent; call only when the user explicitly asks to enable H.264 export. disable: stop using it (remove=true also deletes the cached binary). license: Cisco's binary license text."
            },
            "remove": { "type": "boolean", "default": false, "description": "with disable: also delete the cached library" }
        }),
        &["action"],
    )
}

pub fn plan() -> Value {
    object(json!({ "timeline": path(TL) }), &["timeline"])
}

pub fn log() -> Value {
    object(json!({ "timeline": path(TL) }), &["timeline"])
}

pub fn undo() -> Value {
    object(
        json!({
            "timeline": path(TL),
            "force": { "type": "boolean", "default": false, "description": "undo even if the file changed since that edit (the change stays in snapshots)" }
        }),
        &["timeline"],
    )
}

pub fn report_read() -> Value {
    object(
        json!({
            "report": path("render report JSON (the `report_path` returned by render)"),
            "full": { "type": "boolean", "default": false, "description": "include the full report, not just the summary" }
        }),
        &["report"],
    )
}

pub fn branch() -> Value {
    object(
        json!({
            "timeline": path(TL),
            "action": { "enum": ["create", "checkout", "merge"], "description": "create a branch at the current state, switch the file to a branch's tip, or replay a branch's edits onto the current branch" },
            "name": { "type": "string", "pattern": "^[A-Za-z0-9._/-]{1,64}$" },
            "force": { "type": "boolean", "default": false, "description": "checkout: discard unjournaled changes" }
        }),
        &["timeline", "action", "name"],
    )
}

fn check_args() -> Value {
    json!({
        "type": "array", "items": { "type": "string" },
        "description": "extra ferrocut-perceive arguments, verbatim (threshold flags, config file). Defaults: -14 LUFS ±1 LU, true peak ≤ -1 dBTP"
    })
}

pub fn quality_check() -> Value {
    object(
        json!({
            "render": path("rendered file to check"),
            "timeline": path("the timeline it was rendered from"),
            "args": check_args(),
            "expect_audio": expect_audio(),
            "timeout_s": { "type": "number", "exclusiveMinimum": 0, "description": "kill the checker after this many seconds" }
        }),
        &["render", "timeline"],
    )
}

// ---------------------------------------------------------------------------
// The timeline file format (docs://timeline/schema.json, timeline_schema).

fn transform() -> Value {
    let pair =
        |d: &str| json!({ "type": "array", "minItems": 2, "maxItems": 2, "items": animatable(d) });
    json!({
        "description": "Layer transform (After Effects convention); key times clip-local. Defaults are the identity: position and anchor at the frame center, scale 1, rotation 0. position_z, anchor_z, rotation_x, rotation_y and orientation need the clip's three_d switch.",
        "type": "object",
        "properties": {
            "position": pair("output pixels [x, y] where the anchor lands"),
            "anchor": pair("source pixels [x, y] of the pivot"),
            "scale": { "anyOf": [ animatable("uniform scale factor (1 = 100 %)"), pair("per-axis scale [x, y]") ] },
            "rotation": animatable("degrees, clockwise (the Z rotation of a 3D layer)"),
            "position_z": animatable("3D layers: depth of the position, pixels, positive = away from the viewer (default 0)"),
            "anchor_z": animatable("3D layers: depth of the anchor point, pixels (default 0)"),
            "rotation_x": animatable("3D layers: X rotation, degrees; positive tilts the bottom edge away"),
            "rotation_y": animatable("3D layers: Y rotation, degrees; positive brings the right edge towards the viewer"),
            "orientation": { "type": "array", "minItems": 3, "maxItems": 3, "items": animatable("degrees"), "description": "3D layers: orientation [x, y, z], applied outside the X/Y/Z rotations" }
        },
        "additionalProperties": false
    })
}

fn three_d() -> Value {
    json!({ "type": "boolean", "default": false, "description": "After Effects 3D layer switch: a card in 3D space seen through the timeline camera; consecutive 3D layers are depth-sorted (farthest first), 2D layers split the runs" })
}

fn clip_motion_blur() -> Value {
    json!({ "type": "boolean", "default": false, "description": "After Effects layer motion blur switch: blur this layer's transform (and camera) motion; needs the timeline's motion_blur" })
}

fn camera() -> Value {
    let triple =
        |d: &str| json!({ "type": "array", "minItems": 3, "maxItems": 3, "items": animatable(d) });
    json!({
        "description": "The camera 3D layers are seen through; keyframes in timeline time. Defaults: After Effects' 50 mm camera (zoom = width*50/36 at [w/2, h/2, -zoom] looking at [w/2, h/2, 0]), which shows an untransformed 3D layer exactly like the 2D layer. No lights, shadows or depth of field.",
        "type": "object",
        "properties": {
            "position": triple("camera position [x, y, z], output pixels"),
            "point_of_interest": triple("the point the camera looks at [x, y, z]"),
            "zoom": animatable("distance in pixels at which a layer appears at 100 %, > 0; exclusive with fov_deg"),
            "fov_deg": animatable("horizontal angle of view, degrees, (0, 180); exclusive with zoom")
        },
        "additionalProperties": false
    })
}

fn motion_blur() -> Value {
    json!({
        "description": "Motion blur for clips with motion_blur: true (After Effects composition switch and shutter). Each frame averages `samples` resampled copies of the layer at sub-frame times t + (phase + angle*(i+1/2)/samples)/360/fps; the layer content is the frame at t. Absent = off.",
        "type": "object",
        "properties": {
            "shutter_angle": rational("degrees, [0, 720], default 180"),
            "shutter_phase": rational("degrees, [-360, 360], default -90"),
            "samples": { "type": "integer", "minimum": 2, "maximum": 64, "default": 16, "description": "sub-frame samples per frame, 2..64" }
        },
        "additionalProperties": false
    })
}

fn duck() -> Value {
    json!({
        "description": "Sidechain ducking: this bus is turned down while the key tracks are loud. Numeric fields may be keyframed (timeline time).",
        "type": "object",
        "properties": {
            "key": { "type": "array", "minItems": 1, "items": { "type": "string" }, "description": "names of the key tracks (not this one, not ducked themselves)" },
            "threshold_db": animatable("dB, default -30"),
            "ratio": animatable(">= 1, default 4"),
            "attack_ms": animatable("> 0, default 10"),
            "release_ms": animatable("> 0, default 250"),
            "range_db": animatable("maximum reduction, >= 0, default 12")
        },
        "required": ["key"], "additionalProperties": false
    })
}

fn bus() -> Value {
    json!({
        "description": "A track's audio bus. gain/pan keyframes are in timeline time.",
        "type": "object",
        "properties": {
            "gain_db": animatable("bus gain, dB"),
            "pan": animatable("balance -1..1"),
            "mute": { "type": "boolean" },
            "duck": duck()
        },
        "additionalProperties": false
    })
}

fn marker_color() -> Value {
    json!({ "enum": ferrocut_engine::markers::MarkerColor::ALL, "default": "green", "description": "Premiere marker color" })
}

/// A marker list (`timeline.markers` or a clip's `markers`).
fn markers(time: &str) -> Value {
    json!({
        "description": "Named, colored annotations (never rendered). Ids are unique within the list.",
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "time": rational(time),
                "duration": rational("range marker length in seconds (default 0: a point marker)"),
                "name": { "type": "string" },
                "color": marker_color(),
                "comment": { "type": "string" }
            },
            "required": ["id", "time"], "additionalProperties": false
        }
    })
}

const CLIP_MARKER_TIME: &str =
    "source time of the marked frame, >= 0 (stays on that frame through trims/slips/splits)";

fn video_clip() -> Value {
    json!({
        "description": "A clip on a video track: media or a nested comp (`source`; its audio, if any, plays as linked audio on the track's bus) or a generator layer (`generator`).",
        "type": "object",
        "properties": {
            "id": { "type": "string", "minLength": 1, "description": "unique across the timeline; edit ops refer to it" },
            "source": { "type": "string", "minLength": 1, "description": "media path, or a timeline .json file (nested composition), relative to the timeline file's directory (or absolute)" },
            "start": rational("timeline time of the first frame, >= 0"),
            "source_in": rational("source time of the first frame, >= 0 (default 0)"),
            "duration": rational("length in seconds, > 0; source_in + duration must not exceed the media"),
            "opacity": animatable("0..1, default 1; keyframes clip-local"),
            "transform": transform(),
            "three_d": three_d(),
            "motion_blur": clip_motion_blur(),
            "transition_in": {
                "description": "Dissolve from the previous clip on this track. The previous clip must overlap this one by at least `duration` (use the add_transition op, which makes the overlap from handles).",
                "anyOf": [
                    { "type": "null" },
                    {
                        "type": "object",
                        "properties": { "kind": { "const": "dissolve" }, "duration": rational("seconds, > 0, <= this clip's duration") },
                        "required": ["kind", "duration"], "additionalProperties": false
                    }
                ]
            },
            "speed": speed(),
            "time_remap": time_remap(),
            "sampling": sampling(),
            "blend_mode": blend_mode(),
            "audio": clip_audio(),
            "generator": generator(),
            "markers": markers(CLIP_MARKER_TIME),
            "masks": crate::native_schema::masks(),
            "effects": video_effects(),
            "adjustment": { "type": "boolean", "default": false, "description": "adjustment layer: no source; its effects apply to the composite of the tracks below while it is active, mixed by its opacity and its track's matte. Its track holds only adjustment clips; no transform, 3D, blend mode, speed or transition." }
        },
        "required": ["id", "start", "duration"],
        "oneOf": [ { "required": ["source"] }, { "required": ["generator"] }, { "required": ["adjustment"], "properties": { "adjustment": { "const": true } } } ],
        "additionalProperties": false
    })
}

fn audio_clip() -> Value {
    json!({
        "description": "An audio-only clip (music, dialogue, effects).",
        "type": "object",
        "properties": {
            "id": { "type": "string", "minLength": 1 },
            "source": { "type": "string", "minLength": 1 },
            "start": rational("timeline time, >= 0"),
            "source_in": rational("source time, >= 0 (default 0)"),
            "duration": rational("seconds, > 0"),
            "speed": speed(),
            "time_remap": time_remap(),
            "audio": clip_audio(),
            "markers": markers(CLIP_MARKER_TIME)
        },
        "required": ["id", "source", "start", "duration"],
        "additionalProperties": false
    })
}

/// JSON Schema (draft 2020-12) of a whole timeline file.
pub fn timeline() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "docs://timeline/schema.json",
        "title": "Ferrocut timeline",
        "description": "Times and numeric values are exact rationals: integers or strings \"n\", \"n/d\", \"0.5\" (JSON floats are rejected). Track 0 is the bottom video layer. Clips on a track may not overlap except by a dissolve's duration. Build and change timelines with edit_apply ops rather than editing this JSON by hand.",
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "output": {
                "type": "object",
                "properties": {
                    "width": { "type": "integer", "minimum": 1 },
                    "height": { "type": "integer", "minimum": 1 },
                    "fps": rational("frames per second, e.g. 24 or \"30000/1001\""),
                    "gop": { "type": "integer", "minimum": 1, "default": 24, "description": "frames per closed GOP" },
                    "gops_per_chunk": { "type": "integer", "minimum": 1, "default": 1, "description": "render chunk size in GOPs" },
                    "duration": { "anyOf": [ { "type": "null" }, rational("explicit output length (default: end of the last clip)") ] }
                },
                "required": ["width", "height", "fps"], "additionalProperties": false
            },
            "tracks": {
                "type": "array", "minItems": 1,
                "description": "video tracks, bottom (0) to top",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "unique (edit ops and duck keys refer to it)" },
                        "audio": bus(),
                        "matte": matte(),
                        "effects": video_effects(),
                        "clips": { "type": "array", "items": video_clip() }
                    },
                    "required": ["clips"], "additionalProperties": false
                }
            },
            "audio_tracks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "bus": bus(),
                        "clips": { "type": "array", "items": audio_clip() }
                    },
                    "required": ["clips"], "additionalProperties": false
                }
            },
            "audio": {
                "type": "object",
                "properties": {
                    "sample_rate": { "type": "integer", "minimum": 8000, "maximum": 192000, "default": 48000 },
                    "master_gain_db": animatable("master gain, dB (timeline time)"),
                    "loudness": {
                        "anyOf": [
                            { "type": "null" },
                            {
                                "type": "object",
                                "description": "two-pass loudness normalization + true-peak limiter",
                                "properties": {
                                    "target_lufs": rational("integrated loudness target, [-70, 0): -23 broadcast (EBU R128), -14 streaming"),
                                    "true_peak_dbtp": rational("true-peak ceiling, [-20, 0], default -1")
                                },
                                "required": ["target_lufs"], "additionalProperties": false
                            }
                        ]
                    }
                },
                "additionalProperties": false
            },
            "camera": camera(),
            "motion_blur": motion_blur(),
            "markers": markers("timeline time, >= 0 (does not move with ripple edits)")
        },
        "required": ["output", "tracks"],
        "additionalProperties": false
    })
}

pub fn timeline_schema() -> Value {
    object(
        json!({
            "part": {
                "enum": ["all", "timeline", "edit_ops", "params", "guide"],
                "default": "all",
                "description": "timeline: JSON Schema of the file; edit_ops: schema of edit_apply ops; params: every settable parameter (name, kind, unit, range, default, time base); guide: concise authoring guide (markdown)"
            }
        }),
        &[],
    )
}

pub fn markers_list() -> Value {
    object(json!({ "timeline": path(TL) }), &["timeline"])
}

pub fn media_status() -> Value {
    object(
        json!({
            "timeline": path(TL),
            "proxies": { "type": "boolean", "default": true, "description": "also look up each online video file's proxy (hashes the files)" }
        }),
        &["timeline"],
    )
}

pub fn proxy_generate() -> Value {
    object(
        json!({
            "timeline": path("make proxies of every video source of this timeline (nested comps followed)"),
            "media": { "type": "array", "items": path("media file"), "description": "media files to make proxies of" },
            "force": { "type": "boolean", "default": false, "description": "re-make existing proxies" }
        }),
        &[],
    )
}

pub fn media_probe() -> Value {
    object(
        json!({ "path": path("media file (absolute, or relative to the server's working directory)") }),
        &["path"],
    )
}

pub fn index_media() -> Value {
    let b = |d: bool, desc: &str| json!({ "type": "boolean", "default": d, "description": desc });
    object(
        json!({
            "media": path("media file to index"),
            "transcribe": b(true, "build the whisper.cpp transcript (word times)"),
            "shots": b(true, "detect shot boundaries (when the detector is available)"),
            "force": b(false, "rebuild even if a cached index exists"),
            "cpu": b(false, "transcribe on the CPU instead of the GPU"),
            "segments": b(true, "include the transcript segments (start, end, text) in the result"),
        }),
        &["media"],
    )
}

pub fn transcript_search() -> Value {
    object(
        json!({
            "media": path("media file (indexed on first use)"),
            "query": { "type": "string", "minLength": 1, "description": "words to find (case and punctuation are ignored; near matches are scored)" },
            "max_results": { "type": "integer", "minimum": 1, "maximum": 100, "default": 5 },
            "pad": rational("handle added before and after each hit for cut_in/cut_out, seconds (default 1/4)"),
        }),
        &["media", "query"],
    )
}

pub fn shots_list() -> Value {
    object(
        json!({
            "media": path("media file (indexed on first use)"),
            "start": rational("only boundaries at or after this source time"),
            "end": rational("only boundaries before this source time"),
        }),
        &["media"],
    )
}

/// `expect_audio` for render(check) / quality_check.
pub fn expect_audio() -> Value {
    json!({
        "enum": ["auto", "yes", "no"], "default": "auto",
        "description": "audio expectation for the check: auto = audio iff the timeline has any (a silent timeline doesn't fail missing_audio)"
    })
}
