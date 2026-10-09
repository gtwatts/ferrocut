//! `quality_check` must refuse a render report whose chunk directory resolves
//! outside the project root. No real render: a fake master plus report JSON.

use ferrocut_mcp::{Ctx, Root, call};
use serde_json::json;

const TL: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [ { "name": "V1", "clips": [
    { "id": "a", "source": "a.mkv", "start": 0, "source_in": "0", "duration": "2" } ]}]
}"#;

fn quality_err(cx: &Ctx, render: &str) -> String {
    let err = call(
        cx,
        "quality_check",
        json!({ "render": render, "timeline": "tl.json" }),
    )
    .expect("known tool")
    .expect_err("chunk dir outside the root");
    format!("{err:#}")
}

#[test]
fn quality_check_rejects_chunk_dir_outside_the_project_root() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let proj = base.join("proj");
    // A unique sibling name, so a relative "../<name>" exists only next to the
    // project, not relative to the test process's cwd (the checker's cwd).
    let outside_name = format!("outside-{}", base.file_name().unwrap().to_string_lossy());
    let outside = base.join(&outside_name);
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(proj.join("tl.json"), TL).unwrap();
    std::fs::write(proj.join("out.mkv"), b"not a video").unwrap();

    let absolute = json!({
        "total_frames": 1,
        "chunk_frames": 1,
        "chunks": [],
        "chunk_dir": outside.display().to_string(),
        "output": "out.mkv",
    });
    std::fs::write(
        proj.join("out.report.json"),
        serde_json::to_string(&absolute).unwrap(),
    )
    .unwrap();

    let cx = Ctx::new(Root::new(&proj).unwrap());
    let msg = quality_err(&cx, "out.mkv");
    assert!(
        msg.contains("outside the project root"),
        "absolute chunk_dir: {msg}"
    );

    // `../<sibling>` from the report location is an existing directory outside
    // the root.
    let traversal = json!({
        "total_frames": 1,
        "chunk_frames": 1,
        "chunks": [],
        "chunk_dir": format!("../{outside_name}"),
        "output": "out.mkv",
    });
    std::fs::write(
        proj.join("out.report.json"),
        serde_json::to_string(&traversal).unwrap(),
    )
    .unwrap();
    let msg = quality_err(&cx, "out.mkv");
    assert!(
        msg.contains("outside the project root"),
        "traversal chunk_dir: {msg}"
    );
}

fn flag_err(cx: &Ctx, args: serde_json::Value) -> String {
    let err = call(
        cx,
        "quality_check",
        json!({ "render": "out.mkv", "timeline": "tl.json", "args": args }),
    )
    .expect("known tool")
    .expect_err("checker flag outside the root");
    format!("{err:#}")
}

/// H'. Checker flags that name paths are sandboxed before the checker runs.
#[test]
fn quality_check_rejects_checker_args_outside_the_project_root() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let proj = base.join("proj");
    let outside = base.join("outside");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(proj.join("tl.json"), TL).unwrap();
    std::fs::write(proj.join("out.mkv"), b"not a video").unwrap();
    std::fs::write(
        proj.join("out.report.json"),
        r#"{"total_frames":1,"chunk_frames":1,"chunks":[],"chunk_dir":".ferrocut-cache/chunks/t","output":"out.mkv"}"#,
    )
    .unwrap();
    let cx = Ctx::new(Root::new(&proj).unwrap());

    let msg = flag_err(&cx, json!(["--cache-dir", outside.display().to_string()]));
    assert!(msg.contains("outside the project root"), "cache-dir: {msg}");
    let msg = flag_err(&cx, json!(["--render-report=../../x.json"]));
    assert!(
        msg.contains("outside the project root"),
        "render-report: {msg}"
    );
}
