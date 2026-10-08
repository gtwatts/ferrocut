//! Native graphics schemas and project-root asset integration.
//! Uses the repository's small JSON Schema harness; engine checks cover
//! semantic constraints (UTF-8 byte limits and path command ordering).

#[path = "support/mini_schema.rs"]
mod mini_schema;

use std::path::{Path, PathBuf};

use ferrocut_engine::Timeline;
use ferrocut_engine::text::{MAX_TEXT_BYTES, TextSpec};
use ferrocut_mcp::{Ctx, Root, call};
use serde_json::{Value, json};

fn timeline(generator: Value) -> Value {
    json!({
        "output":{"width":160,"height":80,"fps":24,"gop":12},
        "tracks":[{"name":"graphics","clips":[
            {"id":"graphic","start":0,"duration":1,"generator":generator}
        ]}]
    })
}

fn text(value: Value) -> Value {
    json!({"type":"text","text":value})
}

fn shape(value: Value) -> Value {
    json!({"type":"shape","shape":value})
}

fn check_round_trip(value: Value) {
    let schema = ferrocut_mcp::schema::timeline();
    let errors = mini_schema::validate(&schema, &value);
    assert!(errors.is_empty(), "authored graphics: {errors:#?}");
    let parsed = Timeline::from_json(&value.to_string()).unwrap();
    let output = serde_json::to_value(parsed).unwrap();
    let errors = mini_schema::validate(&schema, &output);
    assert!(errors.is_empty(), "re-serialized graphics: {errors:#?}");
}

#[test]
fn native_text_and_shape_timelines_validate_before_and_after_serialization() {
    for t in [
        json!({"content":"Hello","font":"fonts/main.ttf"}),
        json!({
            "content":"Ferrocut\nمرحبا שלום","font":"fonts/main.ttf","font_index":0,
            "fallback_fonts":["fonts/arabic.ttf","fonts/hebrew.ttf"],
            "font_size":{"keyframes":[{"t":0,"v":24},{"t":1,"v":36,"interp":"easy_ease"}]},
            "line_height":44,"tracking":"1/2","position":[4,6],"box_size":[150,70],
            "align":"justified","vertical_align":"center","wrap":"word_or_glyph",
            "fill":[1,"1/2",0,1],"stroke":{"color":[0,0,0],"width":2},"opacity":"3/4",
            "animators":[
                {"selector":{"unit":"characters","start":0,"end":4},"position":[2,0],"opacity":"1/2","fill":[0,0,1]},
                {"selector":{"unit":"words","end":2},"fill":null},
                {"selector":{"unit":"lines","start":1,"end":2}}
            ]
        }),
        json!({
            "content":"","font":"fonts/main.ttf","line_height":null,"box_size":null,
            "stroke":null,"align":"right","vertical_align":"bottom","wrap":"none"
        }),
    ] {
        check_round_trip(timeline(text(t)));
    }

    let stops = json!([
        {"offset":0,"color":[1,0,0,1]},
        {"offset":"1/2","color":[0,1,0,"1/2"]},
        {"offset":1,"color":[0,0,1,0]}
    ]);
    for s in [
        json!({"geometry":{"type":"rectangle","width":120,"height":40}}),
        json!({
            "geometry":{"type":"rectangle","x":4,"y":6,"width":{"expression":"100 + time * 10"},"height":40,"radius":8},
            "fill":{"type":"linear_gradient","start":[0,0],"end":[120,40],"stops":stops,"interpolation":"linear"},
            "stroke":{"paint":{"type":"solid","color":[1,1,1]},"width":3,"cap":"square","join":"bevel","miter_limit":4,"dashes":[6,4],"dash_offset":2},
            "fill_rule":"even_odd"
        }),
        json!({
            "geometry":{"type":"ellipse","center":[80,40],"radius":[40,20]},
            "fill":{"type":"radial_gradient","center":[80,40],"radius":40,"stops":stops},
            "stroke":null
        }),
        json!({
            "geometry":{"type":"path","commands":[
                {"type":"move_to","point":[4,4]},
                {"type":"line_to","point":[80,4]},
                {"type":"quad_to","control":[100,10],"to":[120,40]},
                {"type":"cubic_to","control1":[100,70],"control2":[20,70],"to":[4,40]},
                {"type":"close"}
            ]},
            "fill":null,
            "stroke":{"paint":{"type":"linear_gradient","start":[0,0],"end":[120,0],"stops":stops},"cap":"round","join":"round"}
        }),
    ] {
        check_round_trip(timeline(shape(s)));
    }
}

fn schema_and_engine_reject(generator: Value) {
    let value = timeline(generator);
    let errors = mini_schema::validate(&ferrocut_mcp::schema::timeline(), &value);
    assert!(
        !errors.is_empty(),
        "schema accepted malformed generator {}",
        value["tracks"][0]["clips"][0]["generator"]
    );
    assert!(
        Timeline::from_json(&value.to_string()).is_err(),
        "engine accepted malformed generator"
    );
}

#[test]
fn schemas_reject_missing_misspelled_or_mistyped_native_fields() {
    for t in [
        json!({"font":"a.ttf"}),
        json!({"content":"Hi"}),
        json!({"content":"Hi","font":""}),
        json!({"content":"Hi","font":"a.ttf","font_size":2.5}),
        json!({"content":"Hi","font":"a.ttf","font_sze":24}),
        json!({"content":"Hi","font":"a.ttf","fill":null}),
        json!({"content":"Hi","font":"a.ttf","font_index":-1}),
        json!({"content":"Hi","font":"a.ttf","font_index":4294967296u64}),
        json!({"content":"Hi","font":"a.ttf","align":"justify"}),
        json!({"content":"Hi","font":"a.ttf","vertical_align":"baseline"}),
        json!({"content":"Hi","font":"a.ttf","wrap":"character"}),
        json!({"content":"Hi","font":"a.ttf","fallback_fonts":[""]}),
        json!({"content":"Hi","font":"a.ttf","fallback_fonts":vec!["b.ttf";32]}),
        json!({"content":"Hi","font":"a.ttf","box_size":[1,2,3]}),
        json!({"content":"Hi","font":"a.ttf","stroke":{"color":[1,1,1]}}),
        json!({"content":"Hi","font":"a.ttf","animators":[{"selector":{"start":0}}]}),
        json!({"content":"Hi","font":"a.ttf","animators":[{"selector":{"end":3,"unit":"glyphs"}}]}),
        json!({"content":"Hi","font":"a.ttf","animators":vec![json!({"selector":{"end":1}});129]}),
        json!({"content":"Hi","font":"a.ttf","opacity":{"keyframes":[]}}),
    ] {
        schema_and_engine_reject(text(t));
    }
    let stop = json!({"offset":0,"color":[1,1,1]});
    for s in [
        json!({}),
        json!({"geometry":{"type":"star","points":5}}),
        json!({"geometry":{"type":"rectangle","width":20}}),
        json!({"geometry":{"type":"rectangle","width":20,"height":10,"height_px":10}}),
        json!({"geometry":{"type":"ellipse","center":[0,0],"radius":5}}),
        json!({"geometry":{"type":"path","commands":[
            {"type":"move_to","point":[0,0]},{"type":"cubic_to","control1":[1,1],"to":[2,2]}
        ]}}),
        json!({"geometry":{"type":"path","commands":[
            {"type":"move_to","point":[0,0]},{"type":"close","point":[0,0]}
        ]}}),
        json!({"geometry":{"type":"rectangle","width":20,"height":10},"fill":{"type":"solid","color":[1,1]}}),
        json!({"geometry":{"type":"rectangle","width":20,"height":10},"fill":{"type":"linear_gradient","start":[0,0],"end":[20,0],"stops":[stop]}}),
        json!({"geometry":{"type":"rectangle","width":20,"height":10},"fill":{"type":"radial_gradient","center":[0,0],"radius":5,"stops":vec![stop.clone();257]}}),
        json!({"geometry":{"type":"rectangle","width":20,"height":10},"stroke":{"paint":{"type":"solid","color":[1,1,1]},"cap":"triangle"}}),
        json!({"geometry":{"type":"rectangle","width":20,"height":10},"stroke":{"paint":{"type":"solid","color":[1,1,1]},"dashes":[1,2,3]}}),
        json!({"geometry":{"type":"rectangle","width":20,"height":10},"fill_rule":"positive"}),
    ] {
        schema_and_engine_reject(shape(s));
    }
}

#[test]
fn text_content_byte_limit_and_schema_count_limits_are_explicit() {
    let schema = ferrocut_mcp::native_schema::text();
    assert_eq!(schema["properties"]["content"]["maxLength"], MAX_TEXT_BYTES);
    assert_eq!(schema["properties"]["fallback_fonts"]["maxItems"], 31);
    assert_eq!(schema["properties"]["animators"]["maxItems"], 128);
    // JSON Schema counts characters, while the engine also bounds actual UTF-8
    // storage. Test the documented byte rule with multibyte text.
    let mut value = json!({"content":"é".repeat(MAX_TEXT_BYTES/2),"font":"font.ttf"});
    let t: TextSpec = serde_json::from_value(value.clone()).unwrap();
    t.validate().unwrap();
    value["content"] = json!("é".repeat(MAX_TEXT_BYTES / 2 + 1));
    let t: TextSpec = serde_json::from_value(value).unwrap();
    assert!(t.validate().unwrap_err().contains("bytes"));
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(root.join("fonts")).unwrap();
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let font = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ferrocut-engine/tests/data/text/NotoSans-Regular.ttf");
    assert!(
        font.is_file(),
        "missing explicit font fixture {}",
        font.display()
    );
    for target in [
        root.join("fonts/main.ttf"),
        root.join("fonts/fallback.ttf"),
        outside.join("secret.ttf"),
    ] {
        std::fs::copy(&font, target).unwrap();
    }
    std::fs::write(
        root.join("tl.json"),
        timeline(text(json!({
            "content":"Ferrocut","font":"fonts/main.ttf","font_size":24,
            "fallback_fonts":["fonts/fallback.ttf"]
        })))
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside/secret.ttf", root.join("escape.ttf")).unwrap();
        std::os::unix::fs::symlink("fonts/main.ttf", root.join("inside.ttf")).unwrap();
    }
    Fixture {
        _temp: temp,
        root,
        outside,
    }
}

fn run(cx: &Ctx, tool: &str, args: Value) -> Result<Value, String> {
    call(cx, tool, args)
        .expect("known tool")
        .map_err(|e| format!("{e:#}"))
}

fn rejected(cx: &Ctx, tool: &str, args: Value) {
    let error =
        run(cx, tool, args).expect_err("outside font should fail before any rendering/probing");
    assert!(
        error.contains("outside the project root") || error.contains("dangling"),
        "{error}"
    );
    assert!(error.contains("generator asset"), "{error}");
}

#[test]
fn font_assets_and_fallbacks_obey_root_checks_including_nested_compositions() {
    let f = fixture();
    let cx = Ctx::new(Root::new(&f.root).unwrap());
    // A plan compiles a real node from explicit in-root font bytes without a GPU.
    run(&cx, "plan", json!({"timeline":"tl.json"})).unwrap();
    for (i, font) in [
        Value::String("../outside/secret.ttf".into()),
        json!(f.outside.join("secret.ttf")),
        Value::String("escape.ttf".into()),
    ]
    .into_iter()
    .enumerate()
    {
        if i == 2 && !cfg!(unix) {
            continue;
        }
        let path = format!("bad-{i}.json");
        std::fs::write(
            f.root.join(&path),
            timeline(text(json!({"content":"Hi","font":font}))).to_string(),
        )
        .unwrap();
        rejected(&cx, "plan", json!({"timeline":path}));
    }
    let bad_fallback = timeline(text(
        json!({"content":"Hi","font":"fonts/main.ttf","fallback_fonts":["../outside/secret.ttf"]}),
    ));
    std::fs::write(f.root.join("bad-fallback.json"), bad_fallback.to_string()).unwrap();
    rejected(&cx, "plan", json!({"timeline":"bad-fallback.json"}));

    let inner = timeline(text(
        json!({"content":"Hi","font":"../../outside/secret.ttf"}),
    ));
    std::fs::write(f.root.join("nested/inner.json"), inner.to_string()).unwrap();
    let outer = json!({"output":{"width":160,"height":80,"fps":24},"tracks":[{"name":"outer","clips":[
        {"id":"nested","start":0,"duration":1,"source":"nested/inner.json"}
    ]}]});
    std::fs::write(f.root.join("nested.json"), outer.to_string()).unwrap();
    rejected(&cx, "plan", json!({"timeline":"nested.json"}));

    #[cfg(unix)]
    {
        let inside = timeline(text(json!({"content":"Hi","font":"inside.ttf"})));
        std::fs::write(f.root.join("inside.json"), inside.to_string()).unwrap();
        run(&cx, "plan", json!({"timeline":"inside.json"})).unwrap();
    }
}

#[test]
fn dry_run_and_committed_edits_check_new_font_assets_before_writing() {
    let f = fixture();
    let cx = Ctx::new(Root::new(&f.root).unwrap());
    let before = std::fs::read(f.root.join("tl.json")).unwrap();
    for (param, value) in [
        ("generator.text.font", json!("../outside/secret.ttf")),
        (
            "generator.text.fallback_fonts",
            json!(["../outside/secret.ttf"]),
        ),
        ("generator.text.font", json!("escape.ttf")),
    ] {
        if value == json!("escape.ttf") && !cfg!(unix) {
            continue;
        }
        for dry_run in [true, false] {
            rejected(
                &cx,
                "edit_apply",
                json!({"timeline":"tl.json","dry_run":dry_run,
                "ops":[{"op":"set_param","clip":"graphic","param":param,"value":value}]}),
            );
            assert_eq!(std::fs::read(f.root.join("tl.json")).unwrap(), before);
            assert!(!f.root.join("tl.journal.jsonl").exists());
        }
    }
    for dry_run in [true, false] {
        rejected(
            &cx,
            "edit_apply",
            json!({"timeline":"tl.json","dry_run":dry_run,
            "ops":[{"op":"add_clip","track":"graphics","id":"bad","duration":1,
                "generator":{"type":"text","text":{"content":"Hi","font":"../outside/secret.ttf"}}}]}),
        );
    }
    let good = json!([{"op":"set_param","clip":"graphic","param":"generator.text.font","value":"fonts/fallback.ttf"}]);
    let dry = run(
        &cx,
        "edit_apply",
        json!({"timeline":"tl.json","ops":good,"dry_run":true,"plan":true,"return_timeline":true}),
    )
    .unwrap();
    assert_eq!(dry["written"], false);
    assert_eq!(
        dry["timeline"]["tracks"][0]["clips"][0]["generator"]["text"]["font"],
        "fonts/fallback.ttf"
    );
    assert_eq!(std::fs::read(f.root.join("tl.json")).unwrap(), before);
    assert!(!f.root.join("tl.journal.jsonl").exists());
    let written = run(
        &cx,
        "edit_apply",
        json!({"timeline":"tl.json","ops":good,"return_timeline":true}),
    )
    .unwrap();
    assert_eq!(written["written"], true);
    assert_eq!(
        written["timeline"]["tracks"][0]["clips"][0]["generator"]["text"]["font"],
        "fonts/fallback.ttf"
    );
    assert_ne!(std::fs::read(f.root.join("tl.json")).unwrap(), before);
}
