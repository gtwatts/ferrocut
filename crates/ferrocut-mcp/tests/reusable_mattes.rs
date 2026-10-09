//! Reusable matte discovery, real reversible edits, keys and root checks.
#[path = "support/mini_schema.rs"]
mod mini_schema;

use ferrocut_engine::{Timeline, project::timeline_text};
use ferrocut_mcp::{Ctx, Root, call};
use serde_json::{Value, json};

fn document() -> Value {
    json!({"output":{"width":32,"height":16,"fps":24,"gop":12,"duration":2},"tracks":[
        {"name":"Stencil","clips":[{"id":"s","generator":{"type":"solid","color":[1,1,1,"1/2"]},"start":"1/2","duration":1}]},
        {"name":"Spacer","visible":false,"clips":[]},
        {"name":"Panel A","matte":{"mode":"alpha","source":{"track":"Stencil"}},"clips":[{"id":"a","generator":{"type":"solid","color":[1,0,0]},"start":0,"duration":2}]},
        {"name":"Panel B","matte":{"mode":"alpha","source":{"track":"Stencil"}},"clips":[{"id":"b","generator":{"type":"solid","color":[0,0,1]},"start":0,"duration":2}]}
    ],"audio_tracks":[{"name":"Music","clips":[]}]})
}
fn run(cx: &Ctx, tool: &str, args: Value) -> Result<Value, String> {
    call(cx, tool, args)
        .expect("registered tool")
        .map_err(|e| format!("{e:#}"))
}
fn fixture() -> (tempfile::TempDir, Ctx, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let tl = Timeline::from_json(&document().to_string()).unwrap();
    // Undo preserves the canonical timeline representation, not arbitrary
    // whitespace in an imported hand-authored document.
    let text = timeline_text(&tl).unwrap();
    std::fs::write(dir.path().join("tl.json"), &text).unwrap();
    std::fs::write(dir.path().join("before.json"), &text).unwrap();
    let cx = Ctx::new(Root::new(dir.path()).unwrap());
    (dir, cx, text.into_bytes())
}

#[test]
fn schema_round_trips_named_mattes_and_rejects_malformed_reference_shapes() {
    let schema = ferrocut_mcp::schema::timeline();
    let v = document();
    assert!(mini_schema::validate(&schema, &v).is_empty());
    let tl = Timeline::from_json(&v.to_string()).unwrap();
    assert!(mini_schema::validate(&schema, &serde_json::to_value(tl).unwrap()).is_empty());
    for value in [
        json!({"track":17}),
        json!({"track":"Stencil","index":0}),
        json!({"index":0}),
    ] {
        let mut bad = v.clone();
        bad["tracks"][2]["matte"]["source"] = value;
        assert!(!mini_schema::validate(&schema, &bad).is_empty());
        assert!(Timeline::from_json(&bad.to_string()).is_err());
    }
    let (dir, cx, _) = fixture();
    let meta = run(&cx, "timeline_schema", json!({"part":"params"})).unwrap();
    for name in ["visible", "matte"] {
        assert!(
            meta["params"]["track"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == name)
        );
    }
    assert!(!dir.path().join("tl.journal.jsonl").exists());
}

#[test]
fn matte_and_visibility_edits_dry_run_diff_journal_and_undo() {
    let (dir, cx, before) = fixture();
    let original_plan = run(&cx, "plan", json!({"timeline":"tl.json"})).unwrap();
    let ops = json!([
        {"op":"set_param","track":"Panel A","param":"matte","value":{"mode":"luma","source":{"track":"Stencil"}}},
        {"op":"set_param","track":"Panel B","param":"matte","value":{"mode":"luma_inverted","source":{"track":"Stencil"}}},
        {"op":"set_param","track":"Stencil","param":"visible","value":false}
    ]);
    let dry = run(
        &cx,
        "edit_apply",
        json!({"timeline":"tl.json","ops":ops,"dry_run":true,"plan":true}),
    )
    .unwrap();
    assert_eq!(dry["written"], false);
    assert!(dry["render_error"].is_null());
    assert_eq!(std::fs::read(dir.path().join("tl.json")).unwrap(), before);
    assert!(!dir.path().join("tl.journal.jsonl").exists());
    let applied = run(
        &cx,
        "edit_apply",
        json!({"timeline":"tl.json","ops":ops,"plan":true}),
    )
    .unwrap();
    assert_eq!(applied["written"], true);
    assert!(applied["journal_seq"].as_u64().is_some());
    assert!(dir.path().join("tl.journal.jsonl").is_file());
    let diff = run(&cx, "diff", json!({"a":"before.json","b":"tl.json"})).unwrap();
    assert!(
        !diff["render"]["dirty_chunks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let after_plan = run(&cx, "plan", json!({"timeline":"tl.json"})).unwrap();
    assert_ne!(original_plan["chunks"], after_plan["chunks"]);
    run(&cx, "undo", json!({"timeline":"tl.json"})).unwrap();
    assert_eq!(std::fs::read(dir.path().join("tl.json")).unwrap(), before);
    assert_eq!(
        original_plan["chunks"],
        run(&cx, "plan", json!({"timeline":"tl.json"})).unwrap()["chunks"]
    );
}

#[test]
fn source_edit_invalidates_only_chunks_that_pull_it() {
    let (dir, cx, before) = fixture();
    let changed = run(
        &cx,
        "edit_apply",
        json!({"timeline":"tl.json","plan":true,
        "ops":[{"op":"set_param","clip":"s","param":"generator.color.r","value":0}]}),
    )
    .unwrap();
    assert!(changed["render_error"].is_null());
    assert_eq!(changed["render"]["dirty_chunks"], json!([1, 2]));
    let diff = run(&cx, "diff", json!({"a":"before.json","b":"tl.json"})).unwrap();
    assert_eq!(diff["render"]["dirty_chunks"], json!([1, 2]));
    run(&cx, "undo", json!({"timeline":"tl.json"})).unwrap();
    assert_eq!(std::fs::read(dir.path().join("tl.json")).unwrap(), before);
}

#[test]
fn invalid_reference_cycle_audio_visibility_and_unsupported_track_ops_are_atomic() {
    let (dir, cx, before) = fixture();
    for (op, expected) in [
        (
            json!({"op":"set_param","track":"Panel A","param":"matte","value":{"mode":"alpha","source":{"track":"Missing"}}}),
            "no video matte source",
        ),
        (
            json!({"op":"set_param","track":"Stencil","param":"matte","value":{"mode":"alpha","source":{"track":"Panel A"}}}),
            "cyclic track matte",
        ),
        (
            json!({"op":"set_param","track":"Music","param":"visible","value":false}),
            "video tracks only",
        ),
        (
            json!({"op":"rename_track","track":"Stencil","name":"New"}),
            "unknown variant",
        ),
        (
            json!({"op":"remove_track","track":"Stencil"}),
            "unknown variant",
        ),
    ] {
        let ops = json!([{"op":"set_param","track":"Stencil","param":"visible","value":false},op]);
        let err = run(&cx, "edit_apply", json!({"timeline":"tl.json","ops":ops})).unwrap_err();
        assert!(err.contains(expected), "{err}");
        assert_eq!(std::fs::read(dir.path().join("tl.json")).unwrap(), before);
        assert!(!dir.path().join("tl.journal.jsonl").exists());
    }
}

#[test]
fn hidden_matte_source_and_nested_assets_still_obey_root_containment() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir(&root).unwrap();
    // Existing bytes ensure a missed root check would advance into probing.
    std::fs::write(
        dir.path().join("outside.mov"),
        b"invalid media, never probe",
    )
    .unwrap();
    let mut v = document();
    v["tracks"][0]["visible"] = json!(false);
    v["tracks"][0]["clips"][0]
        .as_object_mut()
        .unwrap()
        .remove("generator");
    v["tracks"][0]["clips"][0]["source"] = json!("../outside.mov");
    std::fs::write(root.join("inner.json"), v.to_string()).unwrap();
    let cx = Ctx::new(Root::new(&root).unwrap());
    for nested in [false, true] {
        let value = if nested {
            json!({"output":{"width":32,"height":16,"fps":24},"tracks":[
                {"name":"Nested","visible":false,"clips":[{"id":"n","source":"inner.json","start":0,"duration":2}]}
            ]})
        } else {
            v.clone()
        };
        let before = value.to_string();
        std::fs::write(root.join("tl.json"), &before).unwrap();
        let e = run(&cx, "plan", json!({"timeline":"tl.json"})).unwrap_err();
        assert!(e.contains("outside the project root"), "{e}");
        let e = run(&cx, "edit_apply", json!({"timeline":"tl.json","ops":[]})).unwrap_err();
        assert!(e.contains("outside the project root"), "{e}");
        assert_eq!(
            std::fs::read_to_string(root.join("tl.json")).unwrap(),
            before
        );
        assert!(!root.join("tl.journal.jsonl").exists());
    }
}
