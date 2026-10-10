//! Native schema/typed edit/undo controls; no launched or installed MCP claim.
#[path = "support/mini_schema.rs"]
mod mini_schema;
use ferrocut_engine::{Timeline, project::timeline_text};
use ferrocut_mcp::{Ctx, Root, call};
use serde_json::{Value, json};

fn run(cx: &Ctx, tool: &str, args: Value) -> Result<Value, String> {
    call(cx, tool, args)
        .expect("tool exists")
        .map_err(|e| format!("{e:#}"))
}
fn document() -> Value {
    json!({"output":{"width":32,"height":16,"fps":30,"gop":15},"tracks":[
        {"name":"A","clips":[{"id":"a","start":0,"duration":2,"three_d":true,
          "generator":{"type":"solid","color":[1,0,0,"1/2"]}}]},
        {"name":"B","clips":[{"id":"b","start":0,"duration":2,"three_d":true,
          "generator":{"type":"solid","color":[0,0,1,"3/4"]}}]}]})
}

#[test]
fn depth_mode_camera_fields_discover_and_round_trip() {
    let schema = ferrocut_mcp::schema::timeline();
    let mut v = document();
    v["renderer"] = json!("depth_layers_v1");
    v["camera"] = json!({"reference_up":[0,-1,0],"roll":15,"near":1,"far":10000});
    assert!(mini_schema::validate(&schema, &v).is_empty());
    let t = Timeline::from_json(&v.to_string()).unwrap();
    assert!(mini_schema::validate(&schema, &serde_json::to_value(t).unwrap()).is_empty());
    v["renderer"] = json!("imaginary_depth");
    assert!(!mini_schema::validate(&schema, &v).is_empty());
    assert!(Timeline::from_json(&v.to_string()).is_err());
}

#[test]
fn real_depth_edit_diff_undo_and_degenerate_batch_are_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let text = timeline_text(&Timeline::from_json(&document().to_string()).unwrap()).unwrap();
    for name in ["before.json", "tl.json"] {
        std::fs::write(dir.path().join(name), &text).unwrap();
    }
    let cx = Ctx::new(Root::new(dir.path()).unwrap());
    let before = run(&cx, "plan", json!({"timeline":"tl.json"})).unwrap();
    let meta = run(&cx, "timeline_schema", json!({"part":"params"})).unwrap();
    for name in [
        "renderer",
        "camera.reference_up",
        "camera.roll",
        "camera.near",
        "camera.far",
    ] {
        assert!(
            meta["params"]["timeline"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == name)
        );
    }
    let ops = json!([{ "op":"set_param","param":"renderer","value":"depth_layers_v1"},
        {"op":"set_param","param":"camera.roll","value":15}]);
    let dry = run(
        &cx,
        "edit_apply",
        json!({"timeline":"tl.json","ops":ops,"dry_run":true,"plan":true}),
    )
    .unwrap();
    assert_eq!(dry["written"], false);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tl.json")).unwrap(),
        text
    );
    let applied = run(
        &cx,
        "edit_apply",
        json!({"timeline":"tl.json","ops":ops,"plan":true}),
    )
    .unwrap();
    assert!(applied["render_error"].is_null());
    assert_eq!(applied["written"], true);
    let delta = run(&cx, "diff", json!({"a":"before.json","b":"tl.json"})).unwrap();
    assert!(
        !delta["render"]["dirty_chunks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let bytes = std::fs::read(dir.path().join("tl.json")).unwrap();
    let journal = std::fs::read(dir.path().join("tl.journal.jsonl")).unwrap();
    assert!(run(&cx,"edit_apply",json!({"timeline":"tl.json","ops":[
      {"op":"set_param","param":"camera","value":{"position":[0,0,0],"point_of_interest":[0,0,0]}}
    ]})).is_err());
    assert_eq!(std::fs::read(dir.path().join("tl.json")).unwrap(), bytes);
    assert_eq!(
        std::fs::read(dir.path().join("tl.journal.jsonl")).unwrap(),
        journal
    );
    run(&cx, "undo", json!({"timeline":"tl.json"})).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tl.json")).unwrap(),
        text
    );
    assert_eq!(
        before["chunks"],
        run(&cx, "plan", json!({"timeline":"tl.json"})).unwrap()["chunks"]
    );
    assert!(run(&cx, "plan", json!({"timeline":"../escape.json"})).is_err());
}
