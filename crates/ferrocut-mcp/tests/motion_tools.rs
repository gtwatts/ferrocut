//! Agent controls, strict schemas, source binding and ordinary undoable edits.

#[path = "support/mini_schema.rs"]
mod mini_schema;

use ferrocut_core::Rational;
use ferrocut_engine::{
    Timeline,
    media::encode::{ChunkEncoder, EncodeSettings},
};
use ferrocut_mcp::{Ctx, Root, call};
use serde_json::{Value, json};
use std::path::PathBuf;

fn run(cx: &Ctx, name: &str, args: Value) -> Result<Value, String> {
    call(cx, name, args)
        .expect("registered tool")
        .map_err(|e| format!("{e:#}"))
}
fn settings() -> Value {
    json!({"start":0,"fps":8,"width":96,"height":64,"frame_count":8,
        "points":[{"id":"feature","center":["49/2","49/2"],"feature_size":[15,15],"search_size":[35,35]}],
        "min_confidence":80,"subpixel":false})
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    cx: Ctx,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let mut encoder = ChunkEncoder::create(
            &root.join("motion.mkv"),
            &EncodeSettings {
                width: 96,
                height: 64,
                fps: Rational::from_int(8),
                gop: 4,
            },
        )
        .unwrap();
        for frame in 0..8i32 {
            let mut rgba = vec![0u8; 96 * 64 * 4];
            for y in 0..64i32 {
                for x in 0..96i32 {
                    let sx = x - frame * 2;
                    let sy = y - frame;
                    let value = if sx >= 0 && sy >= 0 {
                        let seed = (sx as u32)
                            .wrapping_mul(7_471)
                            .wrapping_add((sy as u32).wrapping_mul(19_927));
                        ((seed ^ seed.wrapping_mul(73).rotate_left(13)) % 206 + 25) as u8
                    } else {
                        0
                    };
                    let i = (y as usize * 96 + x as usize) * 4;
                    rgba[i..i + 4].copy_from_slice(&[value, value, value, 255]);
                }
            }
            encoder.push_bgra(&rgba).unwrap();
        }
        encoder.finish().unwrap();
        let tl = json!({"output":{"width":96,"height":64,"fps":8,"duration":1,"gop":4},
        "tracks":[{"name":"Source","clips":[{"id":"source","source":"motion.mkv","start":0,"duration":1}]},
        {"name":"Overlay","clips":[{"id":"overlay","start":0,"duration":1,"generator":{"type":"shape","shape":{
            "geometry":{"type":"ellipse","center":["49/2","49/2"],"radius":[5,5]},"fill":null,
            "stroke":{"paint":{"type":"solid","color":[1,0,0,1]},"width":1}
        }}}]}]});
        std::fs::write(root.join("tl.json"), serde_json::to_vec(&tl).unwrap()).unwrap();
        let cx = Ctx::new(Root::new(&root).unwrap());
        Self {
            _temp: temp,
            root,
            cx,
        }
    }
    fn analyze(&self) -> Value {
        run(
            &self.cx,
            "tracking_analyze",
            json!({"path":"motion.mkv","output":"analysis.json","settings":settings()}),
        )
        .unwrap()
    }
    fn keys(&self, mode: &str) -> Result<Value, String> {
        let mut options =
            json!({"point":"feature","source_clip":"source","target_clip":"overlay","mode":mode});
        if mode == "stabilize" {
            options["target_clip"] = json!("source");
            options["stabilization"] = json!({"mode":"lock","smoothness":"1/2"});
        }
        run(
            &self.cx,
            "tracking_keyframes",
            json!({"analysis":"analysis.json","timeline":"tl.json","options":options}),
        )
    }
}

#[test]
fn tracking_measurements_become_real_editable_keys_and_undo_restores_timeline() {
    let f = Fixture::new();
    let report = f.analyze();
    assert_eq!(report["completion"], "completed");
    assert_eq!(report["tracks"][0]["measured_frames"], 7);
    let before = std::fs::read(f.root.join("tl.json")).unwrap();
    let planned = f.keys("attach").unwrap();
    assert_eq!(planned["sample_count"], 8);
    assert_eq!(
        std::fs::read(f.root.join("tl.json")).unwrap(),
        before,
        "planning is read only"
    );
    run(
        &f.cx,
        "edit_apply",
        json!({"timeline":"tl.json","ops":planned["ops"]}),
    )
    .unwrap();
    let tl = Timeline::load(&f.root.join("tl.json")).unwrap();
    let position = tl.tracks[1].clips[0]
        .transform
        .as_ref()
        .unwrap()
        .position
        .as_ref()
        .unwrap();
    let at = ferrocut_core::RationalTime::new(7, 8);
    assert!((position[0].eval(at) - 62.0).abs() < 0.01);
    assert!((position[1].eval(at) - 39.0).abs() < 0.01);
    run(&f.cx, "undo", json!({"timeline":"tl.json"})).unwrap();
    let restored = ferrocut_engine::project::read_timeline(&f.root.join("tl.json")).unwrap();
    let original: Timeline = serde_json::from_slice(&before).unwrap();
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    let stabilized = f.keys("stabilize").unwrap();
    assert_eq!(stabilized["ops"][0]["clip"], "source");
    let first: Rational =
        serde_json::from_value(stabilized["ops"][0]["keyframes"][0]["v"].clone()).unwrap();
    let last: Rational =
        serde_json::from_value(stabilized["ops"][0]["keyframes"][7]["v"].clone()).unwrap();
    assert!((first.to_f64() - last.to_f64() - 14.0).abs() < 0.01);
}

#[test]
fn tracking_displacement_uses_fit_factors_for_mismatched_sources() {
    let f = Fixture::new();
    f.analyze();
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(f.root.join("tl.json")).unwrap()).unwrap();
    value["output"]["width"] = json!(192);
    value["output"]["height"] = json!(192);
    for (fit, factors, position) in [
        ("contain", json!(["2", "2"]), [124.0, 110.0]),
        ("none", json!(["1", "1"]), [110.0, 103.0]),
        ("stretch", json!(["2", "3"]), [124.0, 117.0]),
    ] {
        value["tracks"][0]["clips"][0]["fit"] = json!(fit);
        std::fs::write(f.root.join("tl.json"), value.to_string()).unwrap();
        let planned = f.keys("attach").unwrap();
        assert_eq!(planned["placement"]["fit_scale"], factors);
        assert_eq!(
            planned["warnings"].as_array().unwrap().is_empty(),
            fit != "stretch"
        );
        for (axis, expected) in position.into_iter().enumerate() {
            let keys = planned["ops"][axis]["keyframes"].as_array().unwrap();
            let last: Rational = serde_json::from_value(keys.last().unwrap()["v"].clone()).unwrap();
            assert!(
                (last.to_f64() - expected).abs() < 0.01,
                "{fit} axis {axis}: {last}"
            );
        }
    }
}

#[test]
fn analysis_io_is_guarded_strict_and_does_not_overwrite() {
    let f = Fixture::new();
    let outside = f._temp.path().join("outside.mkv");
    std::fs::copy(f.root.join("motion.mkv"), &outside).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, f.root.join("escape.mkv")).unwrap();
    for path in [
        outside.to_string_lossy().to_string(),
        "../outside.mkv".to_string(),
        "escape.mkv".to_string(),
    ] {
        assert!(
            run(
                &f.cx,
                "tracking_analyze",
                json!({"path":path,"output":"new.json","settings":settings()})
            )
            .is_err()
        );
        assert!(!f.root.join("new.json").exists());
    }
    assert!(
        run(
            &f.cx,
            "tracking_analyze",
            json!({"path":"motion.mkv","output":"../outside.json","settings":settings()})
        )
        .is_err()
    );
    assert!(run(&f.cx,"tracking_analyze",json!({"path":"motion.mkv","output":"analysis.json","settings":settings(),"timeout_seconds":0})).is_err());
    assert!(run(&f.cx,"tracking_analyze",json!({"path":"motion.mkv","output":"analysis.json","settings":settings(),"unexpected":true})).is_err());
    f.analyze();
    let before = std::fs::read(f.root.join("analysis.json")).unwrap();
    assert!(
        run(
            &f.cx,
            "tracking_analyze",
            json!({"path":"motion.mkv","output":"analysis.json","settings":settings()})
        )
        .is_err()
    );
    assert_eq!(std::fs::read(f.root.join("analysis.json")).unwrap(), before);
}

#[test]
fn tracking_keys_reject_wrong_source_partial_tracks_and_ambiguous_source_timing() {
    let f = Fixture::new();
    f.analyze();
    let path = f.root.join("tl.json");
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    std::fs::copy(f.root.join("motion.mkv"), f.root.join("other.mkv")).unwrap();
    for (property, value) in [
        ("source", json!("other.mkv")),
        ("speed", json!({"expression":"time + 1","value":1})),
        (
            "time_remap",
            json!({"keyframes":[{"t":0,"v":0},{"t":1,"v":1}]}),
        ),
        ("speed", json!(0)),
        ("transform", json!({"rotation":30})),
    ] {
        let mut changed = original.clone();
        changed["tracks"][0]["clips"][0][property] = value;
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(f.keys("attach").is_err(), "accepted ambiguous {property}");
    }
    std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    let analysis_path = f.root.join("analysis.json");
    let mut analysis: Value =
        serde_json::from_slice(&std::fs::read(&analysis_path).unwrap()).unwrap();
    analysis["tracks"][0]["samples"][3]["confidence"] = json!(0);
    std::fs::write(&analysis_path, serde_json::to_vec(&analysis).unwrap()).unwrap();
    assert!(f.keys("attach").is_err());
}

#[test]
fn saved_analysis_is_bound_to_current_source_bytes_and_root() {
    use std::io::Write;
    let f = Fixture::new();
    f.analyze();
    let path = f.root.join("analysis.json");
    let original = std::fs::read(&path).unwrap();
    let outside = f._temp.path().join("outside.mkv");
    std::fs::copy(f.root.join("motion.mkv"), &outside).unwrap();
    let mut changed: Value = serde_json::from_slice(&original).unwrap();
    changed["source"]["path"] = json!(outside);
    std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        f.keys("attach").is_err(),
        "outside recorded source accepted"
    );
    std::fs::write(&path, original).unwrap();
    let mut media = std::fs::OpenOptions::new()
        .append(true)
        .open(f.root.join("motion.mkv"))
        .unwrap();
    media.write_all(b"changed media identity").unwrap();
    drop(media);
    assert!(f.keys("attach").unwrap_err().contains("content changed"));
}

#[test]
fn reverse_speed_and_source_canvas_conversion_generate_sorted_exact_local_times() {
    let f = Fixture::new();
    f.analyze();
    let path = f.root.join("tl.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["output"]["width"] = json!(192);
    document["output"]["height"] = json!(128);
    document["output"]["duration"] = json!(2);
    let source = &mut document["tracks"][0]["clips"][0];
    source["start"] = json!("1/4");
    source["source_in"] = json!("7/8");
    source["duration"] = json!("7/16");
    source["speed"] = json!(-2);
    document["tracks"][1]["clips"][0]["start"] = json!("1/4");
    document["tracks"][1]["clips"][0]["duration"] = json!("1/2");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let result = f.keys("attach").unwrap();
    let x = result["ops"][0]["keyframes"].as_array().unwrap();
    assert_eq!(x.len(), 7);
    assert_eq!(x[0]["t"], "0");
    assert_eq!(x[6]["t"], "3/8");
    let first: Rational = serde_json::from_value(x[0]["v"].clone()).unwrap();
    let last: Rational = serde_json::from_value(x[6]["v"].clone()).unwrap();
    assert!((first.to_f64() - 124.0).abs() < 0.02);
    assert!((last.to_f64() - 100.0).abs() < 0.02);
    run(
        &f.cx,
        "edit_apply",
        json!({"timeline":"tl.json","ops":result["ops"]}),
    )
    .unwrap();
}

#[test]
fn masks_and_nested_repeater_schemas_round_trip_and_reject_misspellings() {
    let value = json!({"output":{"width":96,"height":64,"fps":8,"gop":4},"tracks":[{"clips":[{
        "id":"g","start":0,"duration":1,
        "generator":{"type":"vector_group","group":{"items":[{"type":"group","group":{"items":[{"type":"shape","shape":{
            "geometry":{"type":"rectangle","width":8,"height":8}
        }}],"repeat":{"copies":"5/2","position":[12,0]}}}],"transform":{"position":[4,8]}}},
        "masks":[{"geometry":{"type":"ellipse","center":[48,32],"radius":[40,24]},"feather":2,"mode":"intersect"}]
    }]}]});
    for current in [
        value.clone(),
        serde_json::to_value(Timeline::from_json(&value.to_string()).unwrap()).unwrap(),
    ] {
        let errors = mini_schema::validate(&ferrocut_mcp::schema::timeline(), &current);
        assert!(errors.is_empty(), "{errors:#?}");
    }
    for (path, extra) in [
        (
            "masks",
            json!([{"geometry":{"type":"ellipse","center":[48,32],"radius":[40,24]},"feathre":2}]),
        ),
        (
            "generator",
            json!({"type":"vector_group","group":{"items":[],"repeet":{"copies":2}}}),
        ),
    ] {
        let mut bad = value.clone();
        bad["tracks"][0]["clips"][0][path] = extra;
        assert!(!mini_schema::validate(&ferrocut_mcp::schema::timeline(), &bad).is_empty());
        assert!(Timeline::from_json(&bad.to_string()).is_err());
    }
}

#[test]
fn new_tracking_tool_schemas_match_real_strict_arguments() {
    let tools = ferrocut_mcp::tools();
    for (name, args) in [
        (
            "tracking_analyze",
            json!({"path":"motion.mkv","output":"analysis.json","settings":settings()}),
        ),
        (
            "tracking_keyframes",
            json!({"analysis":"analysis.json","timeline":"tl.json","options":{
            "point":"feature","source_clip":"source","target_clip":"overlay","mode":"attach"}}),
        ),
    ] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let schema = Value::Object((*tool.input_schema).clone());
        assert!(mini_schema::validate(&schema, &args).is_empty());
        let mut bad = args;
        bad["extra"] = json!(true);
        assert!(!mini_schema::validate(&schema, &bad).is_empty());
    }
}
