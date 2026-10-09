//! artifact_frames: decoded frames of an encoded file under the root, the
//! file's identity pinned, outputs named by content, paths contained.
//! The fixture is a tiny FFV1 file from the engine's own encoder.

use std::path::Path;

use ferrocut_core::Rational;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_mcp::{Ctx, INLINE_PNG_KEY, Root, call};
use serde_json::{Value, json};

fn synth(path: &Path, n: u32) {
    let s = EncodeSettings {
        width: 32,
        height: 16,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..n {
        let px: Vec<u8> = (0..32 * 16)
            .flat_map(|_| [(f * 20) as u8, 7, 3, 255])
            .collect();
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(t.path()).unwrap();
    let proj = base.join("proj");
    std::fs::create_dir_all(proj.join("out")).unwrap();
    std::fs::create_dir_all(base.join("outside")).unwrap();
    synth(&proj.join("out/cut.mkv"), 10);
    (t, proj)
}

fn run(cx: &Ctx, args: Value) -> Result<Value, String> {
    call(cx, "artifact_frames", args)
        .expect("known tool")
        .map_err(|e| format!("{e:#}"))
}

#[test]
fn encoded_frames_come_back_with_identity_and_files() {
    let (_t, proj) = project();
    let cx = Ctx::new(Root::new(&proj).unwrap());
    let v = run(&cx, json!({ "path": "out/cut.mkv", "frames": [0, 4, -1] })).unwrap();
    let hash = ferrocut_engine::index::blake3_file(&proj.join("out/cut.mkv")).unwrap();
    assert_eq!(v["artifact"]["kind"], "encoded_file");
    assert_eq!(v["artifact"]["blake3"], hash.as_str());
    assert_eq!(v["artifact"]["unchanged_after_decode"], true);
    assert_eq!(v["stream"]["frame_count"], 10);
    let frames = v["frames"].as_array().unwrap();
    let idx: Vec<u64> = frames
        .iter()
        .map(|f| f["index"].as_u64().unwrap())
        .collect();
    assert_eq!(idx, [0, 4, 9]);
    for f in frames {
        let p = proj.join(f["path"].as_str().unwrap());
        assert!(p.starts_with(proj.join("out/inspect")), "{}", p.display());
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with(&format!("cut-{}-i", &hash[..12])),
            "{name}"
        );
        assert_eq!(
            f["png_blake3"],
            ferrocut_engine::index::blake3_file(&p).unwrap().as_str()
        );
        assert!(f["pts"].is_i64() && f["time"].is_string());
    }
    assert!(v["sheet"].is_string());
    assert_eq!(v["inline"]["kind"], "sheet");
    assert!(
        v[INLINE_PNG_KEY]
            .as_str()
            .unwrap()
            .starts_with("iVBORw0KGgo")
    );

    let one = run(
        &cx,
        json!({ "path": "out/cut.mkv", "frames": [3], "each": false }),
    )
    .unwrap();
    assert_eq!(one["inline"]["kind"], "frame");
    assert!(one["frames"][0]["path"].is_null());
    assert!(one["sheet"].is_null());
}

#[test]
fn paths_and_requests_are_checked() {
    let (_t, proj) = project();
    let cx = Ctx::new(Root::new(&proj).unwrap());
    let outside = proj.parent().unwrap().join("outside");
    synth(&outside.join("x.mkv"), 2);
    let e = run(&cx, json!({ "path": outside.join("x.mkv"), "frames": [0] })).unwrap_err();
    assert!(e.contains("outside") || e.contains("root"), "{e}");
    let e = run(
        &cx,
        json!({ "path": "out/cut.mkv", "frames": [0], "output_dir": "../outside" }),
    )
    .unwrap_err();
    assert!(e.contains("outside") || e.contains("root"), "{e}");
    let e = run(
        &cx,
        json!({ "path": "out/cut.mkv", "frames": [0], "prefix": "../x" }),
    )
    .unwrap_err();
    assert!(e.contains("prefix"), "{e}");
    let e = run(&cx, json!({ "path": "out/cut.mkv", "frames": [10] })).unwrap_err();
    assert!(e.contains("past the end"), "{e}");
    let e = run(&cx, json!({ "path": "out", "frames": [0] })).unwrap_err();
    assert!(e.contains("not a file"), "{e}");
}

/// A frame file name that is already a symlink out of the root is refused,
/// not written through.
#[cfg(unix)]
#[test]
fn an_escaping_symlink_at_an_output_name_is_refused() {
    let (_t, proj) = project();
    let cx = Ctx::new(Root::new(&proj).unwrap());
    let hash = ferrocut_engine::index::blake3_file(&proj.join("out/cut.mkv")).unwrap();
    let dir = proj.join("out/inspect");
    std::fs::create_dir_all(&dir).unwrap();
    let victim = proj.parent().unwrap().join("outside/victim.png");
    std::os::unix::fs::symlink(
        &victim,
        dir.join(format!("cut-{}-i000000.png", &hash[..12])),
    )
    .unwrap();
    let e = run(&cx, json!({ "path": "out/cut.mkv", "frames": [0] })).unwrap_err();
    assert!(
        e.contains("outside") || e.contains("root") || e.contains("symlink"),
        "{e}"
    );
    assert!(!victim.exists(), "wrote through the symlink");
}
