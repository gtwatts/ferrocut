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

/// The checker child runs with this process's cwd, which need not be the root.
/// Path values in `args` must reach it as the root-checked absolute paths, in
/// both syntaxes; otherwise `--out review` would be checked as <root>/review
/// but written to <cwd>/review. A fake checker records what it was given; no
/// render or decode happens.
#[cfg(unix)]
#[test]
fn forwarded_checker_paths_are_the_checked_absolute_paths() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let proj = base.join("proj");
    let chunks = proj.join(".ferrocut-cache/chunks/t");
    std::fs::create_dir_all(&chunks).unwrap();
    std::fs::write(proj.join("tl.json"), TL).unwrap();
    std::fs::write(proj.join("out.mkv"), b"not a video").unwrap();
    std::fs::write(
        proj.join("out.report.json"),
        serde_json::to_string(&json!({
            "total_frames": 1, "chunk_frames": 1, "chunks": [],
            "chunk_dir": chunks.display().to_string(), "output": "out.mkv",
        }))
        .unwrap(),
    )
    .unwrap();
    let log = base.join("child-args.txt");
    let fake = base.join("fake-perceive");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\ncase \"$*\" in *--help*) echo '--expect-audio'; exit 0;; esac\n\
             printf '%s\\n' \"$@\" > '{}'\n\
             echo '{{\"schema_version\":\"ferrocut.perceive.check/1\",\"pass\":true,\"problems\":[],\"warnings\":[]}}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // This test binary's cwd is the crate directory, not `proj`.
    assert_ne!(std::env::current_dir().unwrap(), proj);
    // SAFETY: the only test in this binary that runs the checker.
    unsafe { std::env::set_var("FERROCUT_PERCEIVE", &fake) };
    let cx = Ctx::new(Root::new(&proj).unwrap());
    let result = call(
        &cx,
        "quality_check",
        json!({ "render": "out.mkv", "timeline": "tl.json",
                "args": ["--out", "review", "--config=cfg.json"] }),
    )
    .expect("known tool")
    .unwrap();
    assert_eq!(result["status"], "pass", "{result}");
    let given: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let i = given.iter().position(|a| a == "--out").unwrap();
    assert_eq!(given[i + 1], proj.join("review").display().to_string());
    assert!(
        given.contains(&format!("--config={}", proj.join("cfg.json").display())),
        "{given:?}"
    );
    // The normalization alone, without a child.
    let args = ferrocut_mcp::normalize_checker_args(
        &Root::new(&proj).unwrap(),
        &[
            "--brief-cuts".into(),
            "cuts.json".into(),
            "--loudness-target".into(),
            "-14".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        args,
        vec![
            "--brief-cuts".to_string(),
            proj.join("cuts.json").display().to_string(),
            "--loudness-target".to_string(),
            "-14".to_string()
        ]
    );
}

const KEY: &str = "0123456789abcdef";

/// A project with a timeline, a master and a render report whose one chunk
/// lives in `proj/.ferrocut-cache/chunks/t`; `edit` adjusts the report JSON.
fn project_with_report(
    edit: impl FnOnce(&mut serde_json::Value),
) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let proj = base.join("proj");
    let chunks = proj.join(".ferrocut-cache/chunks/t");
    std::fs::create_dir_all(&chunks).unwrap();
    std::fs::create_dir_all(base.join("outside")).unwrap();
    std::fs::write(proj.join("tl.json"), TL).unwrap();
    std::fs::write(proj.join("out.mkv"), b"not a video").unwrap();
    std::fs::write(chunks.join(format!("{KEY}.mkv")), b"not a video").unwrap();
    let mut rr = json!({
        "total_frames": 1, "chunk_frames": 1,
        "chunks": [ { "index": 0, "start_frame": 0, "frames": 1, "key": KEY } ],
        "chunk_dir": chunks.display().to_string(), "output": "out.mkv",
    });
    edit(&mut rr);
    std::fs::write(
        proj.join("out.report.json"),
        serde_json::to_string(&rr).unwrap(),
    )
    .unwrap();
    (dir, base, proj)
}

fn refused(proj: &std::path::Path, args: serde_json::Value, render: &str) -> String {
    let cx = Ctx::new(Root::new(proj).unwrap());
    let err = call(
        &cx,
        "quality_check",
        json!({ "render": render, "timeline": "tl.json", "args": args }),
    )
    .expect("known tool")
    .expect_err("refused before the checker runs");
    format!("{err:#}")
}

/// P2. Each path the checker would use is contained, not only the directory:
/// a symlinked report, a chunk key that is a path, a hash-named chunk that is
/// a symlink out, the report's audio master and the derived write cache. No
/// checker is reached: each call is refused while preparing its paths.
#[cfg(unix)]
#[test]
fn quality_check_refuses_each_escaping_checker_path() {
    use std::os::unix::fs::symlink;

    // Implicit report is a symlink to a report outside the root.
    let (_d, base, proj) = project_with_report(|_| {});
    let outside_report = base.join("outside/out.report.json");
    std::fs::rename(proj.join("out.report.json"), &outside_report).unwrap();
    symlink(&outside_report, proj.join("out.report.json")).unwrap();
    let msg = refused(&proj, json!([]), "out.mkv");
    assert!(msg.contains("render report"), "report symlink: {msg}");
    assert!(
        msg.contains("outside the project root"),
        "report symlink: {msg}"
    );

    // A chunk key that is a path is refused before it is joined.
    let (_d, _base, proj) =
        project_with_report(|rr| rr["chunks"][0]["key"] = json!("../../outside/x"));
    let msg = refused(&proj, json!([]), "out.mkv");
    assert!(msg.contains("not a hex chunk key"), "traversal key: {msg}");
    let (_d, _base, proj) = project_with_report(|rr| rr["chunks"][0]["key"] = json!("/etc/passwd"));
    let msg = refused(&proj, json!([]), "out.mkv");
    assert!(msg.contains("not a hex chunk key"), "absolute key: {msg}");

    // A correctly named chunk file that is a symlink out of the root.
    let (_d, base, proj) = project_with_report(|_| {});
    let leaf = proj.join(format!(".ferrocut-cache/chunks/t/{KEY}.mkv"));
    std::fs::remove_file(&leaf).unwrap();
    std::fs::write(base.join("outside/real.mkv"), b"x").unwrap();
    symlink(base.join("outside/real.mkv"), &leaf).unwrap();
    let msg = refused(&proj, json!([]), "out.mkv");
    assert!(msg.contains("chunk file"), "chunk symlink: {msg}");
    assert!(
        msg.contains("outside the project root"),
        "chunk symlink: {msg}"
    );

    // Checking through the report itself uses its `output` as the audio
    // master; an output outside the root is refused.
    let (_d, base, proj) = project_with_report(|rr| {
        rr["audio"] = json!({ "sample_rate": 48000, "channels": 2, "samples": 0, "blake3": "00" });
    });
    let mut rr: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(proj.join("out.report.json")).unwrap())
            .unwrap();
    rr["output"] = json!(base.join("outside/master.mkv").display().to_string());
    std::fs::write(
        proj.join("out.report.json"),
        serde_json::to_string(&rr).unwrap(),
    )
    .unwrap();
    let msg = refused(&proj, json!([]), "out.report.json");
    assert!(msg.contains("audio master"), "audio: {msg}");

    // The derived write cache: the engine cache holding the chunk dir is a
    // symlink out of the root, so perceive/v1 would be written outside.
    let (_d, base, proj) = project_with_report(|_| {});
    let real_cache = base.join("outside/cache");
    std::fs::rename(proj.join(".ferrocut-cache"), &real_cache).unwrap();
    symlink(&real_cache, proj.join(".ferrocut-cache")).unwrap();
    let mut rr: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(proj.join("out.report.json")).unwrap())
            .unwrap();
    rr["chunk_dir"] = json!(".ferrocut-cache/chunks/t");
    std::fs::write(
        proj.join("out.report.json"),
        serde_json::to_string(&rr).unwrap(),
    )
    .unwrap();
    let msg = refused(&proj, json!([]), "out.mkv");
    assert!(
        msg.contains("outside the project root"),
        "cache symlink: {msg}"
    );
}

/// P2 (re-review). Existing symlinks BELOW the directories the checker writes
/// into are refused: `--out review` with `review/perceive.json` or
/// `review/sheets` pointing outside, a cached analysis leaf under
/// `perceive/v1`, and an in-root symlinked directory hiding an outside leaf.
/// No checker is reached.
#[cfg(unix)]
#[test]
fn quality_check_refuses_escaping_symlinks_below_write_dirs() {
    use std::os::unix::fs::symlink;

    let (_d, base, proj) = project_with_report(|_| {});
    std::fs::create_dir_all(proj.join("review")).unwrap();
    symlink(
        base.join("outside/victim"),
        proj.join("review/perceive.json"),
    )
    .unwrap();
    let msg = refused(&proj, json!(["--out", "review"]), "out.mkv");
    assert!(msg.contains("perceive.json"), "out leaf: {msg}");
    assert!(msg.contains("outside the project root"), "out leaf: {msg}");

    let (_d, base, proj) = project_with_report(|_| {});
    std::fs::create_dir_all(proj.join("review")).unwrap();
    symlink(base.join("outside"), proj.join("review/sheets")).unwrap();
    let msg = refused(&proj, json!(["--out=review"]), "out.mkv");
    assert!(msg.contains("sheets"), "out dir: {msg}");

    // A cached analysis JSON in the derived cache.
    let (_d, base, proj) = project_with_report(|_| {});
    let v1 = proj.join(".ferrocut-cache/perceive/v1");
    std::fs::create_dir_all(&v1).unwrap();
    symlink(base.join("outside/a.json"), v1.join("0123.json")).unwrap();
    let msg = refused(&proj, json!([]), "out.mkv");
    assert!(msg.contains("0123.json"), "cache leaf: {msg}");

    // An in-root symlinked directory is walked: its outside leaf is refused.
    let (_d, base, proj) = project_with_report(|_| {});
    std::fs::create_dir_all(proj.join("elsewhere")).unwrap();
    symlink(base.join("outside/t.png"), proj.join("elsewhere/t.png")).unwrap();
    std::fs::create_dir_all(proj.join("review")).unwrap();
    symlink(proj.join("elsewhere"), proj.join("review/scopes")).unwrap();
    let msg = refused(&proj, json!(["--out", "review"]), "out.mkv");
    assert!(msg.contains("t.png"), "nested leaf: {msg}");
}
