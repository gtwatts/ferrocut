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
            }
        ]
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
            "crossfade_in": fade()
        },
        "additionalProperties": false
    })
}

/// A clip for `ripple_insert`: a video clip (on a video track) or an audio
/// clip (on an audio track; only id/source/source_in/duration/audio).
pub fn clip() -> Value {
    let pair =
        |d: &str| json!({ "type": "array", "minItems": 2, "maxItems": 2, "items": animatable(d) });
    json!({
        "description": "Clip object; its start is set to `at`. Audio-track clips accept only id, source, source_in, duration, audio.",
        "type": "object",
        "properties": {
            "id": { "type": "string", "minLength": 1, "description": "unique across the timeline" },
            "source": { "type": "string", "minLength": 1, "description": "media path (relative to the timeline file's directory, or absolute)" },
            "start": rational("ignored: replaced by `at`"),
            "source_in": rational("source time of the clip's first frame (default 0)"),
            "duration": rational("clip length in seconds (> 0)"),
            "opacity": animatable("0..1, default 1"),
            "transform": {
                "type": "object",
                "properties": {
                    "position": pair("pixels [x, y]"),
                    "anchor": pair("pixels [x, y] in the source"),
                    "scale": { "anyOf": [ animatable("uniform scale"), pair("per-axis scale [x, y]") ] },
                    "rotation": animatable("degrees, clockwise")
                },
                "additionalProperties": false
            },
            "transition_in": {
                "type": "object",
                "properties": {
                    "kind": { "const": "dissolve" },
                    "duration": rational("transition length in seconds")
                },
                "required": ["kind", "duration"], "additionalProperties": false
            },
            "audio": clip_audio()
        },
        "required": ["id", "source", "duration"],
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
            "Add a clip from a media file to a track. The file is probed: it must have a video stream for a video track (its audio, if any, plays as linked audio) or an audio stream for an audio track. Defaults: source_in 0, duration = the rest of the media after source_in, start = the end of the track, id = the file stem (made unique). The range must be free (use ripple_insert to push clips right).",
            json!({
                "track": { "type": "string", "minLength": 1, "description": "track name" },
                "source": path("media path, relative to the timeline file's directory (or absolute, inside the project root)"),
                "id": { "type": "string", "minLength": 1, "description": "clip id (unique)" },
                "start": rational("timeline time of the clip's first frame"),
                "source_in": rational("source time of the clip's first frame"),
                "duration": rational("clip length in seconds")
            }),
            &["track", "source"],
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
            "Set one parameter by name on a clip (`clip`), a track's audio bus (`track`) or the timeline (neither). Names: see timeline_schema `params` (e.g. opacity, transform.position, transform.position.x, transform.scale, transform.rotation, audio.gain_db, audio.pan, audio.mute, audio.fade_in, bus.gain_db, bus.duck, bus.duck.ratio, audio.master_gain_db, audio.loudness, audio.loudness.target_lufs, output.duration). Numeric parameters take a constant or {keyframes}; objects take an object, or null to remove.",
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
        "description": "constant rational or {keyframes} (numeric), [x, y] (vectors), true/false, an object (fades, duck, loudness) or null (remove)",
        "anyOf": [
            animatable("numeric value"),
            { "type": "array", "minItems": 2, "maxItems": 2, "items": animatable("component") },
            { "type": "boolean" },
            { "type": "object" },
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
            "deliver": deliver()
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
        "description": "2D layer transform (After Effects convention); key times clip-local. Defaults are the identity: position and anchor at the frame center, scale 1, rotation 0.",
        "type": "object",
        "properties": {
            "position": pair("output pixels [x, y] where the anchor lands"),
            "anchor": pair("source pixels [x, y] of the pivot"),
            "scale": { "anyOf": [ animatable("uniform scale factor (1 = 100 %)"), pair("per-axis scale [x, y]") ] },
            "rotation": animatable("degrees, clockwise")
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

fn video_clip() -> Value {
    json!({
        "description": "A clip on a video track. Its source's audio (if any) plays as linked audio on the track's bus.",
        "type": "object",
        "properties": {
            "id": { "type": "string", "minLength": 1, "description": "unique across the timeline; edit ops refer to it" },
            "source": { "type": "string", "minLength": 1, "description": "media path, relative to the timeline file's directory (or absolute)" },
            "start": rational("timeline time of the first frame, >= 0"),
            "source_in": rational("source time of the first frame, >= 0 (default 0)"),
            "duration": rational("length in seconds, > 0; source_in + duration must not exceed the media"),
            "opacity": animatable("0..1, default 1; keyframes clip-local"),
            "transform": transform(),
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
            "audio": clip_audio()
        },
        "required": ["id", "source", "start", "duration"],
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
            "audio": clip_audio()
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
            }
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

pub fn media_probe() -> Value {
    object(
        json!({ "path": path("media file (absolute, or relative to the server's working directory)") }),
        &["path"],
    )
}

/// `expect_audio` for render(check) / quality_check.
pub fn expect_audio() -> Value {
    json!({
        "enum": ["auto", "yes", "no"], "default": "auto",
        "description": "audio expectation for the check: auto = audio iff the timeline has any (a silent timeline doesn't fail missing_audio)"
    })
}
