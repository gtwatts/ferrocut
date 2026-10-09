//! render.range is validated before any GPU or render work: exactly one of
//! frames/time, half-open, inside the timeline, and not combined with check.

use ferrocut_mcp::{Ctx, Root, call};
use serde_json::{Value, json};

fn project() -> (tempfile::TempDir, Ctx) {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("p");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tl.json"),
        json!({
            "output": {"width": 64, "height": 36, "fps": "24000/1001", "duration": 2, "gop": 12},
            "tracks": [{"name": "V", "clips": [{"id": "bg", "start": 0, "duration": 2,
                "generator": {"type": "solid", "color": ["1/2", "1/4", "1/8", 1]}}]}]
        })
        .to_string(),
    )
    .unwrap();
    let cx = Ctx::new(Root::new(&dir).unwrap());
    (t, cx)
}

fn refused(cx: &Ctx, extra: Value) -> String {
    let mut args = json!({ "timeline": "tl.json", "output": "out.mkv" });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let e = call(cx, "render", args).expect("known tool").unwrap_err();
    format!("{e:#}")
}

#[test]
fn bad_ranges_are_refused_before_rendering() {
    let (_t, cx) = project();
    let e = refused(&cx, json!({ "range": { "frames": [5, 5] } }));
    assert!(e.contains("empty or reversed"), "{e}");
    let e = refused(&cx, json!({ "range": { "frames": [10, 4] } }));
    assert!(e.contains("empty or reversed"), "{e}");
    let e = refused(&cx, json!({ "range": { "frames": [0, 100000] } }));
    assert!(e.contains("ends after the timeline"), "{e}");
    let e = refused(&cx, json!({ "range": {} }));
    assert!(e.contains("exactly one"), "{e}");
    let e = refused(
        &cx,
        json!({ "range": { "frames": [0, 2], "time": ["0", "1"] } }),
    );
    assert!(e.contains("exactly one"), "{e}");
    let e = refused(&cx, json!({ "range": { "time": ["-1/100", "1"] } }));
    assert!(e.contains("negative"), "{e}");
    let e = refused(
        &cx,
        json!({ "range": { "time": ["0", "9223372036854775807"] } }),
    );
    assert!(e.contains("out of range"), "{e}");
    let e = refused(&cx, json!({ "range": { "time": ["1/100", "2/100"] } }));
    assert!(e.contains("no frame start"), "{e}");
    let e = refused(&cx, json!({ "range": { "frames": [0, 2] }, "check": true }));
    assert!(e.contains("selected-range master is not graded"), "{e}");
    assert!(!cx.root.dir().join("out.mkv").exists());
}
