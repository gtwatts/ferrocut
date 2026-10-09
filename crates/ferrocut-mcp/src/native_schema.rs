//! Strict schemas for native typography and vector source payloads.
//!
//! These describe the serde structures. Animation domains, UTF-8 byte limits,
//! path command ordering, and font decoding are additionally checked by the
//! engine. Font path permission is enforced by the MCP project-root guard.

use serde_json::{Value, json};

use crate::schema::animatable;

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn default(mut schema: Value, value: Value) -> Value {
    schema["default"] = value;
    schema
}

fn nullable(schema: Value) -> Value {
    json!({"anyOf": [{"type":"null"}, schema]})
}

fn pair(description: &str) -> Value {
    json!({
        "description": description,
        "type": "array", "minItems": 2, "maxItems": 2,
        "items": animatable("component; keyframe time is generator source time")
    })
}

fn color(description: &str) -> Value {
    json!({
        "description": description,
        "type": "array", "minItems": 3, "maxItems": 4,
        "items": animatable("straight display-referred Rec.709 component in [0,1]; generator source time")
    })
}

fn font_path() -> Value {
    json!({
        "type": "string", "minLength": 1,
        "description": "Explicit font asset path, relative to the timeline or absolute within the MCP project root. No system-font lookup."
    })
}

fn selector() -> Value {
    object(
        json!({
            "unit": {"enum":["characters","words","lines"],"default":"characters",
                "description":"Unicode graphemes, Unicode words, or visual lines"},
            "start": default(animatable("zero-based half-open selection start; fractional cluster weighting"),json!(0)),
            "end": animatable("zero-based half-open selection end; must be at least start")
        }),
        &["end"],
    )
}

fn animator() -> Value {
    object(
        json!({
            "selector": selector(),
            "position": default(pair("selected-cluster translation in output pixels"),json!([0,0])),
            "opacity": default(animatable("selected-cluster opacity in [0,1]"),json!(1)),
            "fill": nullable(color("optional selected-cluster fill override"))
        }),
        &["selector"],
    )
}

/// Payload of a text generator. Required content and explicit font.
pub fn text() -> Value {
    let stroke = object(
        json!({
            "color": color("outline color, painted beneath fill"),
            "width": animatable("outline outside extent in pixels, [0,256]")
        }),
        &["color", "width"],
    );
    let mut schema = object(
        json!({
            "content": {"type":"string","maxLength":262144,
                "description":"Editable UTF-8 text; engine enforces at most 262144 UTF-8 bytes and rejects control characters except newline, carriage return, and tab. JSON Schema maxLength counts characters."},
            "font": font_path(),
            "font_index": {"type":"integer","minimum":0,"maximum":4294967295u64,"default":0,
                "description":"Face index in the primary font asset"},
            "fallback_fonts": {"type":"array","maxItems":31,"items":font_path(),"default":[],
                "description":"Ordered explicit fallback assets; no installed-font fallback"},
            "font_size": default(animatable("font size in pixels, greater than zero and at most 4096"),json!(48)),
            "line_height": nullable(animatable("line height in pixels, greater than zero and at most 16384; omitted uses metrics")),
            "tracking": default(animatable("additional inter-cluster space in pixels, [-4096,4096]"),json!(0)),
            "position": default(pair("top-left paragraph/point origin in output pixels; each axis [-1000000,1000000]"),json!([0,0])),
            "box_size": nullable(pair("paragraph box [width,height] in pixels; both greater than zero and at most 1000000")),
            "align": {"enum":["left","center","right","justified"],"default":"left"},
            "vertical_align": {"enum":["top","center","bottom"],"default":"top"},
            "wrap": {"enum":["none","word","glyph","word_or_glyph"],"default":"word_or_glyph"},
            "fill": default(color("base text fill"),json!([1,1,1,1])),
            "stroke": nullable(stroke),
            "opacity": default(animatable("text opacity in [0,1]"),json!(1)),
            "animators": {"type":"array","maxItems":128,"items":animator(),"default":[],
                "description":"Ordered range animators; selection is cluster-safe"}
        }),
        &["content", "font"],
    );
    schema["description"] = json!(
        "Native asset-pinned shaped text. Numeric animation uses generator source time. Rendered output is premultiplied linear ACEScg."
    );
    schema
}

fn command() -> Value {
    let mut variants = Vec::new();
    for kind in ["move_to", "line_to"] {
        variants.push(object(
            json!({
                "type":{"const":kind},
                "point":pair("endpoint [x,y] in output pixels")
            }),
            &["type", "point"],
        ));
    }
    variants.push(object(
        json!({
            "type":{"const":"quad_to"},
            "control":pair("quadratic control point"),
            "to":pair("quadratic endpoint")
        }),
        &["type", "control", "to"],
    ));
    variants.push(object(
        json!({
            "type":{"const":"cubic_to"},
            "control1":pair("first cubic control point"),
            "control2":pair("second cubic control point"),
            "to":pair("cubic endpoint")
        }),
        &["type", "control1", "control2", "to"],
    ));
    variants.push(object(json!({"type":{"const":"close"}}), &["type"]));
    json!({"oneOf":variants})
}

fn geometry() -> Value {
    json!({"oneOf":[
        object(json!({
            "type":{"const":"rectangle"},
            "x":default(animatable("left x in output pixels"),json!(0)),
            "y":default(animatable("top y in output pixels"),json!(0)),
            "width":animatable("width in output pixels; clamps to nonnegative at evaluation"),
            "height":animatable("height in output pixels; clamps to nonnegative at evaluation"),
            "radius":default(animatable("rounded corner radius in pixels; clamps to [0,min(width,height)/2]"),json!(0))
        }), &["type","width","height"]),
        object(json!({
            "type":{"const":"ellipse"},
            "center":pair("ellipse center [x,y] in output pixels"),
            "radius":pair("ellipse radii [rx,ry] in pixels; clamp to nonnegative")
        }), &["type","center","radius"]),
        object(json!({
            "type":{"const":"polygon"},
            "center":pair("polygon center [x,y] in output pixels"),
            "points":animatable("polygon vertices, 3..256"),
            "radius":animatable("outer radius in pixels"),
            "rotation":default(animatable("rotation in degrees"),json!(0)),
            "roundness":default(animatable("corner roundness in percent, 0..100"),json!(0))
        }), &["type","center","points","radius"]),
        object(json!({
            "type":{"const":"star"},
            "center":pair("star center [x,y] in output pixels"),
            "points":animatable("star points, 3..256"),
            "inner_radius":animatable("inner radius in pixels"),
            "outer_radius":animatable("outer radius in pixels"),
            "rotation":default(animatable("rotation in degrees"),json!(0)),
            "inner_roundness":default(animatable("inner roundness in percent, 0..100"),json!(0)),
            "outer_roundness":default(animatable("outer roundness in percent, 0..100"),json!(0))
        }), &["type","center","points","inner_radius","outer_radius"]),
        object(json!({
            "type":{"const":"path"},
            "commands":{"type":"array","minItems":2,"maxItems":100000,"items":command(),
                "description":"Each contour starts with move_to, followed by drawable segments; close requires a segment and the next contour requires move_to. Engine validates ordering. Open contours close for fill only."}
        }), &["type","commands"])
    ]})
}

fn operators() -> Value {
    let a = animatable;
    json!({"type":"array","maxItems":32,"default":[],"items":{"oneOf":[
        object(json!({"type":{"const":"trim"},
            "start":default(a("start percent"),json!(0)),"end":default(a("end percent"),json!(100)),
            "offset":default(a("offset in degrees"),json!(0)),"mode":{"enum":["simultaneous","individual"],"default":"simultaneous"}}), &["type"]),
        object(json!({"type":{"const":"round_corners"},"radius":a("rounding radius in pixels")}), &["type","radius"]),
        object(json!({"type":{"const":"offset"},"amount":a("offset in pixels"),
            "join":{"enum":["miter","round","bevel"],"default":"miter"},
            "miter_limit":default(a("miter limit, at least 1"),json!(4)),
            "copies":default(a("copies, bounded integer count"),json!(1)),
            "copy_offset":default(a("offset copy progression"),json!(0))}), &["type","amount"]),
        object(json!({"type":{"const":"pucker_bloat"},"amount":a("pucker/bloat percent")}), &["type","amount"]),
        object(json!({"type":{"const":"zigzag"},"size":a("displacement in pixels"),
            "ridges":default(a("ridges per segment"),json!(1)),"smooth":{"type":"boolean","default":false}}), &["type","size"]),
        object(json!({"type":{"const":"twist"},"angle":a("twist in degrees"),
            "center":default(pair("twist center [x,y] in pixels"),json!([0,0]))}), &["type","angle"]),
        object(json!({"type":{"const":"wiggle"},"size":a("displacement in pixels"),
            "detail":default(a("path detail"),json!(10)),"smooth":{"type":"boolean","default":true},
            "speed":default(a("evolution per source second"),json!(2)),"correlation":default(a("correlation percent"),json!(50)),
            "phase":default(a("phase in degrees"),json!(0)),"seed":default(a("deterministic seed"),json!(0))}), &["type","size"]),
        object(json!({"type":{"const":"reverse"}}), &["type"]),
        object(json!({"type":{"const":"merge"},"mode":{"enum":["merge","add","subtract","intersect","exclude"],"default":"merge"}}), &["type"])
    ]},"description":"Ordered EffectCraft path operations before fill/stroke. Numeric animation and intrinsic wiggle time use generator source time. Engine bounds geometry expansion."})
}

fn stops() -> Value {
    json!({
        "type":"array","minItems":2,"maxItems":256,
        "items":object(json!({
            "offset":animatable("stop position in [0,1]; clamps and stably sorts each sample, coincident stops form hard edges"),
            "color":color("gradient stop color")
        }), &["offset","color"])
    })
}

fn interpolation() -> Value {
    json!({
        "enum":["display","linear"],"default":"display",
        "description":"Premultiplied interpolation in encoded Rec.709 or linear ACEScg"
    })
}

fn paint() -> Value {
    json!({"oneOf":[
        object(json!({
            "type":{"const":"solid"},
            "color":color("solid fill/stroke color")
        }), &["type","color"]),
        object(json!({
            "type":{"const":"linear_gradient"},
            "start":pair("gradient start point in output pixels"),
            "end":pair("gradient end point in output pixels"),
            "stops":stops(),
            "interpolation":interpolation()
        }), &["type","start","end","stops"]),
        object(json!({
            "type":{"const":"radial_gradient"},
            "center":pair("gradient center in output pixels"),
            "radius":animatable("radial gradient radius in pixels; zero selects final stop"),
            "stops":stops(),
            "interpolation":interpolation()
        }), &["type","center","radius","stops"])
    ]})
}

fn dashes() -> Value {
    // JSON Schema has no modulo predicate for array length. A small bounded
    // union captures exactly the supported empty or even-sized dash arrays.
    let lengths: Vec<_> = (0..=256)
        .step_by(2)
        .map(|len| json!({"minItems":len,"maxItems":len}))
        .collect();
    json!({
        "type":"array","maxItems":256,"anyOf":lengths,
        "items":animatable("dash/gap length in pixels; clamps to nonnegative"),
        "default":[],
        "description":"Empty for solid; otherwise even pairs of on/off lengths, at least two. An all-zero evaluated pattern is invisible."
    })
}

fn stroke() -> Value {
    object(
        json!({
            "paint":paint(),
            "width":default(animatable("stroke width in pixels; zero is invisible"),json!(1)),
            "cap":{"enum":["butt","round","square"],"default":"butt"},
            "join":{"enum":["miter","round","bevel"],"default":"miter"},
            "miter_limit":default(animatable("miter limit, clamps to at least 1"),json!(4)),
            "dashes":dashes(),
            "dash_offset":default(animatable("dash phase offset in pixels"),json!(0))
        }),
        &["paint"],
    )
}

/// Payload of a shape generator. Required native geometry.
pub fn shape() -> Value {
    let mut schema = object(
        json!({
            "geometry":geometry(),
            "operators":operators(),
            "fill":default(nullable(paint()),json!({"type":"solid","color":[1,1,1,1]})),
            "stroke":default(nullable(stroke()),Value::Null),
            "fill_rule":{"enum":["nonzero","even_odd"],"default":"nonzero"}
        }),
        &["geometry"],
    );
    schema["description"] = json!(
        "Native antialiased vector source in output pixels. Every numeric property is animatable in generator source time. Alpha supports existing track mattes. Geometry coverage is 8-bit; color processing and output remain floating point."
    );
    schema
}

pub fn masks() -> Value {
    let mut geometry = geometry();
    geometry["oneOf"][4]["properties"]["commands"]["maxItems"] = json!(256);
    json!({"type":"array","maxItems":64,"default":[],
        "description":"Ordered source-time masks before clip effects and transform. Feather is isotropic signed-distance feather. First subtract/intersect/darken starts opaque; additive modes start transparent. Not supported on adjustment layers.",
        "items":object(json!({
            "geometry":geometry,
            "mode":{"enum":["none","add","subtract","intersect","lighten","darken","difference"],"default":"add"},
            "inverted":{"type":"boolean","default":false},
            "enabled":{"type":"boolean","default":true},
            "opacity":default(animatable("mask opacity fraction 0..1, source time"),json!(1)),
            "feather":default(animatable("isotropic feather width in pixels 0..512, source time"),json!(0)),
            "expansion":default(animatable("signed expansion in pixels -512..512, source time"),json!(0)),
            "fill_rule":{"enum":["nonzero","even_odd"],"default":"nonzero"}
        }), &["geometry"])
    })
}

fn group_transform() -> Value {
    object(
        json!({
            "anchor":default(pair("local vector pivot"),json!([0,0])),
            "position":default(pair("group translation in pixels"),json!([0,0])),
            "scale":default(pair("axis scale percent"),json!([100,100])),
            "rotation":default(animatable("clockwise degrees"),json!(0)),
            "skew":default(animatable("skew degrees -85..85"),json!(0)),
            "skew_axis":default(animatable("skew axis degrees"),json!(0)),
            "opacity":default(animatable("per-descendant opacity percent, 0..100; group is not isolated"),json!(100))
        }),
        &[],
    )
}

fn repeat() -> Value {
    object(
        json!({
            "copies":default(animatable("0..128 copies; fractional final copy fades"),json!(3)),
            "offset":default(animatable("copy transform exponent offset"),json!(0)),
            "anchor":default(pair("copy pivot"),json!([0,0])),
            "position":default(pair("per-copy translation"),json!([100,0])),
            "scale":default(pair("per-copy scale percent, strictly positive"),json!([100,100])),
            "rotation":default(animatable("per-copy clockwise degrees"),json!(0)),
            "start_opacity":default(animatable("first copy opacity percent 0..100"),json!(100)),
            "end_opacity":default(animatable("last copy opacity percent 0..100"),json!(100)),
            "composite":{"enum":["above","below"],"default":"below"}
        }),
        &[],
    )
}

/// Unroll the engine's finite nesting bound; no remote or unresolved schema refs.
pub fn vector_group() -> Value {
    fn group(depth: usize) -> Value {
        let mut items = vec![object(
            json!({"type":{"const":"shape"},"shape":shape()}),
            &["type", "shape"],
        )];
        if depth < 8 {
            items.push(object(
                json!({"type":{"const":"group"},"group":group(depth+1)}),
                &["type", "group"],
            ));
        }
        object(
            json!({
                "items":{"type":"array","maxItems":128,"items":{"oneOf":items}},
                "transform":default(group_transform(),json!({})),
                "repeat":nullable(repeat())
            }),
            &["items"],
        )
    }
    let mut result = group(1);
    result["description"] = json!(
        "Editable native vector groups and repeaters, painted bottom to top. Numeric animation is source time. Bounds across the whole tree: depth 8, 128 nodes, 512 expanded draws and work budgets checked by the engine. Group opacity multiplies descendant paints without isolation."
    );
    result
}

pub fn tracking_settings() -> Value {
    let pixel_pair = json!({"type":"array","minItems":2,"maxItems":2,"items":crate::schema::rational("original decoded source pixels")});
    let size = |min, max| json!({"type":"array","minItems":2,"maxItems":2,"items":{"type":"integer","minimum":min,"maximum":max}});
    object(
        json!({
            "start":crate::schema::rational("nonnegative source seconds"),
            "fps":crate::schema::rational("positive sample rate, at most 240; exact rational"),
            "width":{"type":"integer","minimum":16,"maximum":4096},
            "height":{"type":"integer","minimum":16,"maximum":4096},
            "frame_count":{"type":"integer","minimum":1,"maximum":2400},
            "points":{"type":"array","minItems":1,"maxItems":16,"items":object(json!({
                "id":{"type":"string","minLength":1,"maxLength":128},
                "center":pixel_pair,
                "feature_size":size(5,63),
                "search_size":size(9,191)
            }), &["id","center","feature_size","search_size"])},
            "min_confidence":crate::schema::rational("normalized correlation percent 1..100; not a calibrated probability"),
            "subpixel":{"type":"boolean"}
        }),
        &[
            "start",
            "fps",
            "width",
            "height",
            "frame_count",
            "points",
            "min_confidence",
            "subpixel",
        ],
    )
}

pub fn tracking_keyframe_options() -> Value {
    object(
        json!({
            "point":{"type":"string","minLength":1,"maxLength":128},
            "source_clip":{"type":"string","minLength":1},
            "target_clip":{"type":"string","minLength":1},
            "mode":{"enum":["attach","stabilize"]},
            "stabilization":nullable(object(json!({
                "mode":{"enum":["smooth","lock"]},
                "smoothness":crate::schema::rational("upstream smoothing amount 0..100")
            }), &["mode","smoothness"])),
            "offset":{"type":"array","minItems":2,"maxItems":2,"items":crate::schema::rational("extra output-pixel translation"),"default":[0,0]}
        }),
        &["point", "source_clip", "target_clip", "mode"],
    )
}
