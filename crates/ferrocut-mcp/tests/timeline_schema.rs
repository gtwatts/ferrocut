//! The published timeline JSON Schema matches what the engine reads and
//! writes, the guide/resources are served, and media_probe works.

#[path = "support/mini_schema.rs"]
mod mini_schema;

use std::path::{Path, PathBuf};

use ferrocut_engine::Timeline;
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_mcp::{Ctx, Root, call, read_doc, resources};
use serde_json::{Value, json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn timelines() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for dir in ["examples", "eval/tasks"] {
        let d = repo().join(dir);
        let mut stack = vec![d];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "json")
                    && (dir == "examples" || p.file_name().is_some_and(|n| n == "start.json"))
                {
                    v.push(p);
                }
            }
        }
    }
    v.sort();
    v
}

#[test]
fn every_example_and_task_timeline_validates_and_engine_output_too() {
    let schema = ferrocut_mcp::schema::timeline();
    // The published form (docs://timeline/schema.json) must accept the same files.
    let published = ferrocut_mcp::compact::compact(schema.clone());
    ferrocut_mcp::compact::refs_are_well_placed(&published).unwrap();
    let files = timelines();
    assert!(files.len() >= 8, "{files:?}");
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        let errs = mini_schema::validate(&schema, &v);
        assert!(errs.is_empty(), "{}: {errs:#?}", f.display());
        let errs = mini_schema::validate(&published, &v);
        assert!(errs.is_empty(), "{} (published): {errs:#?}", f.display());
        // What the engine writes back (all defaults made explicit) validates too.
        let tl = Timeline::from_json(&text).unwrap();
        let out = serde_json::to_value(&tl).unwrap();
        let errs = mini_schema::validate(&schema, &out);
        assert!(
            errs.is_empty(),
            "{} (re-serialized): {errs:#?}",
            f.display()
        );
    }
}

#[test]
fn schema_rejects_what_the_engine_rejects() {
    let schema = ferrocut_mcp::schema::timeline();
    let base = json!({"output": {"width": 64, "height": 32, "fps": "24"},
                      "tracks": [{"name": "V1", "clips": [{"id": "a", "source": "a.mkv", "start": "0", "duration": "1"}]}]});
    assert!(mini_schema::validate(&schema, &base).is_empty());
    for (path, bad) in [
        ("/tracks/0/clips/0/duration", json!(2.5)),
        ("/tracks/0/clips/0/bogus", json!(1)),
        ("/output/fps", json!("24fps")),
        ("/output/fit", json!("fill")),
        ("/tracks/0/clips/0/fit", json!("fill")),
        ("/tracks/0/clips/0/opacity", json!({"keyframes": []})),
    ] {
        let mut v = base.clone();
        let (parent, key) = path.rsplit_once('/').unwrap();
        v.pointer_mut(parent).unwrap()[key] = bad.clone();
        assert!(
            !mini_schema::validate(&schema, &v).is_empty(),
            "{path} = {bad}"
        );
        assert!(
            Timeline::from_json(&v.to_string()).is_err(),
            "engine accepts {path} = {bad}"
        );
    }
}

#[test]
fn timelines_built_with_every_op_validate() {
    let tl = Timeline::from_json(
        r#"{"output":{"width":64,"height":32,"fps":"24"},"tracks":[{"name":"V1","clips":[]}]}"#,
    )
    .unwrap();
    let ops = parse_ops(
        r#"[
      {"op":"add_clip","track":"V1","source":"a.mkv","id":"a","duration":"5","fit":"contain"},
      {"op":"add_clip","track":"V1","source":"b.mkv","id":"b","source_in":"1","duration":"5"},
      {"op":"add_transition","clip":"b","duration":"1"},
      {"op":"set_keyframes","clip":"a","param":"opacity","keyframes":[{"t":"0","v":"0"},{"t":"1","v":"1","interp":"ease_out"}]},
      {"op":"set_param","clip":"a","param":"transform.scale.x","value":"1/2"},
      {"op":"set_param","clip":"b","param":"audio.fade_out","value":{"duration":"1"}},
      {"op":"add_track","kind":"audio","name":"M"},
      {"op":"add_clip","track":"M","source":"m.wav","start":"0","duration":"9"},
      {"op":"set_param","track":"M","param":"bus.duck","value":{"key":["V1"],"ratio":"6"}},
      {"op":"set_param","param":"audio.loudness","value":{"target_lufs":"-23"}},
      {"op":"split","clip":"b","at":"7"},
      {"op":"add_clip","track":"V1","generator":{"type":"radial_gradient","radius":"10","end_color":["0","0","1","1/2"]},"duration":"2"},
      {"op":"set_keyframes","clip":"radial_gradient","param":"generator.center.x","keyframes":[{"t":"0","v":"0"},{"t":"2","v":"64"}]},
      {"op":"add_clip","track":"V1","generator":{"type":"solid","color":["1","1","1"]},"duration":"1"},
      {"op":"set_param","clip":"solid","param":"three_d","value":true},
      {"op":"set_param","clip":"solid","param":"motion_blur","value":true},
      {"op":"set_keyframes","clip":"solid","param":"transform.rotation_y","keyframes":[{"t":"0","v":"0"},{"t":"1","v":"90"}]},
      {"op":"set_param","clip":"solid","param":"transform.orientation","value":["10","0","0"]},
      {"op":"set_param","clip":"solid","param":"transform.position_z","value":"-20"},
      {"op":"set_param","param":"camera.point_of_interest.z","value":"50"},
      {"op":"set_param","param":"camera.fov_deg","value":"60"},
      {"op":"set_param","param":"motion_blur","value":{"samples":"8","shutter_angle":"270"}}
    ]"#,
    )
    .unwrap();
    let (out, _) = apply(&tl, &ops, &mut MediaLengths::unbounded()).unwrap();
    let errs = mini_schema::validate(
        &ferrocut_mcp::schema::timeline(),
        &serde_json::to_value(&out).unwrap(),
    );
    assert!(errs.is_empty(), "{errs:#?}");
}

#[test]
fn schema_tool_and_resources() {
    let d = tempfile::tempdir().unwrap();
    let cx = Ctx::new(Root::new(d.path()).unwrap());
    let all = call(&cx, "timeline_schema", json!({})).unwrap().unwrap();
    for k in ["timeline", "edit_ops", "params", "guide", "resources"] {
        assert!(all.get(k).is_some(), "{k}");
    }
    assert!(
        all["params"]["video_clip"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "opacity")
    );
    let g = call(&cx, "timeline_schema", json!({"part": "guide"}))
        .unwrap()
        .unwrap();
    assert!(g["guide"].as_str().unwrap().contains("add_transition"));
    assert!(
        call(&cx, "timeline_schema", json!({"part": "nope"}))
            .unwrap()
            .is_err()
    );
    for r in resources() {
        let text = read_doc(r.uri).unwrap_or_else(|| panic!("{}", r.uri));
        if r.mime.contains("json") {
            serde_json::from_str::<Value>(&text).unwrap_or_else(|e| panic!("{}: {e}", r.uri));
        }
    }
    assert!(read_doc("docs://nope").is_none());
}

#[test]
fn media_probe_reports_streams_inside_the_root_only() {
    let d = tempfile::tempdir().unwrap();
    let cx = Ctx::new(Root::new(d.path()).unwrap());
    // A tiny WAV: 0.5 s of mono 8 kHz silence.
    let n = 4000u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + n * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(n * 2).to_le_bytes());
    wav.resize(wav.len() + (n * 2) as usize, 0);
    std::fs::write(d.path().join("s.wav"), wav).unwrap();
    let v = call(&cx, "media_probe", json!({"path": "s.wav"}))
        .unwrap()
        .unwrap();
    assert_eq!(v["has_audio"], true);
    assert_eq!(v["has_video"], false);
    assert_eq!(v["duration"], "1/2");
    assert_eq!(v["sample_rate"], 8000);
    assert_eq!(v["channels"], 1);
    assert_eq!(v["path"], "s.wav");
    let e = call(&cx, "media_probe", json!({"path": "/etc/hostname"}))
        .unwrap()
        .unwrap_err();
    assert!(format!("{e:#}").contains("root"), "{e:#}");
}
