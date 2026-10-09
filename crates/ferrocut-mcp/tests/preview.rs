//! preview_frames: stills and the contact sheet land under the root, the
//! sheet (or a single frame) comes back inline as a PNG, inputs are checked.

use ferrocut_mcp::{Ctx, INLINE_PNG_KEY, Root, base64, call};
use serde_json::{Value, json};

fn root() -> (tempfile::TempDir, Root) {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("project");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tl.json"),
        json!({
            "output": {"width": 64, "height": 36, "fps": 24, "duration": 1, "gop": 12},
            "tracks": [{"name": "V", "clips": [{"id": "bg", "start": 0, "duration": 1,
                "generator": {"type": "solid", "color": ["1/2", "1/4", "1/8", 1]}}]}]
        })
        .to_string(),
    )
    .unwrap();
    let root = Root::new(&dir).unwrap();
    (temp, root)
}

fn run(cx: &Ctx, args: Value) -> Result<Value, String> {
    call(cx, "preview_frames", args)
        .expect("known tool")
        .map_err(|e| format!("{e:#}"))
}

#[test]
fn base64_is_rfc4648() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(
        base64(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']),
        "iVBORw0KGgo="
    );
}

#[test]
fn stills_land_under_the_root_with_an_inline_sheet() {
    if let Err(e) = ferrocut_core::GpuContext::new(ferrocut_core::AdapterPreference::Cpu) {
        eprintln!("SKIP: no software adapter ({e})");
        return;
    }
    let (_t, root) = root();
    let cx = Ctx::new(root.clone());
    let v = run(
        &cx,
        json!({"timeline": "tl.json", "spread": 3, "each": true, "cpu": true}),
    )
    .unwrap();
    assert_eq!(v["total_frames"], 24);
    assert_eq!(v["fps"], "24");
    let frames = v["frames"].as_array().unwrap();
    assert_eq!(
        frames
            .iter()
            .map(|f| f["frame"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [4, 12, 20]
    );
    assert_eq!(frames[1]["time"], "1/2");
    assert_eq!(frames[1]["timecode"], "0:00.50 f12");
    for f in frames {
        let p = f["path"].as_str().unwrap();
        assert!(p.starts_with("stills/tl-f"), "{p}");
        assert!(root.dir().join(p).is_file(), "{p}");
    }
    assert_eq!(v["sheet"], "stills/tl-sheet.png");
    assert!(root.dir().join("stills/tl-sheet.png").is_file());
    assert_eq!(v["inline"]["kind"], "sheet");
    let b64 = v[INLINE_PNG_KEY].as_str().unwrap();
    assert!(b64.starts_with("iVBORw0KGgo"), "not a PNG");
    assert_eq!(b64.len() % 4, 0);

    // One frame by time: the frame itself comes back inline, no sheet.
    let v = run(
        &cx,
        json!({"timeline": "tl.json", "at": ["1/2"], "sheet": false, "cpu": true,
               "inline_max": 256, "output_dir": "look"}),
    )
    .unwrap();
    let frames = v["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["frame"], 12);
    assert_eq!(frames[0]["path"], "look/tl-f00012.png");
    assert!(root.dir().join("look/tl-f00012.png").is_file());
    assert!(v["sheet"].is_null());
    assert_eq!(v["inline"]["kind"], "frame");
    assert_eq!(v["inline"]["width"], 64);
    assert!(v[INLINE_PNG_KEY].is_string());

    let v = run(
        &cx,
        json!({"timeline": "tl.json", "frames": [0], "inline": false, "cpu": true}),
    )
    .unwrap();
    assert!(v.get(INLINE_PNG_KEY).is_none());
    assert!(v.get("inline").is_none());
}

#[test]
fn bad_inputs_are_rejected_before_any_gpu_work() {
    let (_t, root) = root();
    let cx = Ctx::new(root);
    for args in [
        json!({"timeline": "tl.json", "output_dir": "../outside", "cpu": true}),
        json!({"timeline": "tl.json", "at": ["1"], "cpu": true}),
        json!({"timeline": "tl.json", "frames": [24], "cpu": true}),
        json!({"timeline": "tl.json", "spread": 0, "cpu": true}),
        json!({"timeline": "tl.json", "cols": 0, "cpu": true}),
        json!({"timeline": "tl.json", "at": [0.5], "cpu": true}),
        json!({"timeline": "tl.json", "bogus": 1}),
        json!({"timeline": "tl.json", "prefix": "../escape", "cpu": true}),
        json!({"timeline": "tl.json", "prefix": "/tmp/x", "cpu": true}),
        json!({"timeline": "tl.json", "prefix": ".hidden", "cpu": true}),
    ] {
        assert!(run(&cx, args.clone()).is_err(), "{args}");
    }
}
