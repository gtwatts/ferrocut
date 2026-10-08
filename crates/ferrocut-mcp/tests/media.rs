//! Markers, media status, relink and proxies through the MCP tools.

use std::path::Path;

use ferrocut_core::{AdapterPreference, GpuContext, Rational};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_mcp::{Ctx, Root, call};
use serde_json::{Value, json};

fn synth(path: &Path, w: u32, h: u32, frames: i64) {
    let s = EncodeSettings {
        width: w,
        height: h,
        fps: Rational::from_int(24),
        gop: 12,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    let mut px = vec![0u8; (w * h * 4) as usize];
    for f in 0..frames {
        for (i, p) in px.iter_mut().enumerate() {
            *p = ((i / 4) as i64 % w as i64 + f * 3) as u8 | 0x20;
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

fn run(cx: &Ctx, tool: &str, args: Value) -> Value {
    call(cx, tool, args.clone())
        .expect("known tool")
        .unwrap_or_else(|e| panic!("{tool} {args}: {e:#}"))
}

const TL: &str = r#"{
  "output": { "width": 512, "height": 256, "fps": "24", "gop": 12 },
  "tracks": [ { "name": "V1", "clips": [
    { "id": "a", "source": "old/clip.mkv", "start": "0", "source_in": "0", "duration": "1" } ] } ]
}"#;

#[test]
fn markers_relink_status_and_draft_render() {
    let dir = tempfile::tempdir().unwrap();
    let d = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(d.join("media/day1")).unwrap();
    synth(&d.join("media/day1/clip.mkv"), 512, 256, 30);
    std::fs::write(d.join("tl.json"), TL).unwrap();
    let cx = Ctx::new(Root::new(&d).unwrap());

    // Offline at first.
    let st = run(&cx, "media_status", json!({ "timeline": "tl.json" }));
    assert_eq!(st["offline"], json!(["old/clip.mkv"]));
    assert_eq!(st["sources"][0]["online"], false);
    assert_eq!(st["sources"][0]["clips"], json!(["a"]));

    // Relink by search, and annotate, in one edit.
    let r = run(
        &cx,
        "edit_apply",
        json!({ "timeline": "tl.json", "ops": [
            { "op": "relink", "search": "media" },
            { "op": "add_marker", "time": "1/2", "name": "check focus", "color": "red" },
            { "op": "add_marker", "clip": "a", "time": "1/4", "name": "slate" }
        ]}),
    );
    assert!(
        r["changes"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("old/clip.mkv -> media/day1/clip.mkv"),
        "{r}"
    );
    let st = run(&cx, "media_status", json!({ "timeline": "tl.json" }));
    assert_eq!(st["offline"], json!([]));
    assert_eq!(st["sources"][0]["path"], "media/day1/clip.mkv");
    assert!(st["sources"][0].get("proxy").is_none());

    let m = run(&cx, "markers_list", json!({ "timeline": "tl.json" }));
    assert_eq!(m["count"], 2);
    assert_eq!(m["markers"][0]["name"], "check focus");
    assert_eq!(m["markers"][0]["color"], "red");
    assert_eq!(m["markers"][1]["scope"], "clip");
    assert_eq!(m["markers"][1]["time"], "1/4");

    // Proxies.
    let p = run(&cx, "proxy_generate", json!({ "timeline": "tl.json" }));
    assert_eq!(p["made"], 1, "{p}");
    assert_eq!(p["proxies"][0]["codec"], "dnxhr_lb");
    assert_eq!(p["proxies"][0]["source"], "media/day1/clip.mkv");
    let p = run(
        &cx,
        "proxy_generate",
        json!({ "media": ["media/day1/clip.mkv"] }),
    );
    assert_eq!(p["kept"], 1, "{p}");
    let st = run(&cx, "media_status", json!({ "timeline": "tl.json" }));
    assert!(
        st["sources"][0]["proxy"]
            .as_str()
            .unwrap()
            .starts_with("media/day1/.ferrocut-proxies/clip."),
        "{st}"
    );
    assert_eq!(st["proxied"], 1);
    assert!(call(&cx, "proxy_generate", json!({})).unwrap().is_err());

    if let Err(e) = GpuContext::new(AdapterPreference::default()) {
        eprintln!("SKIP renders: no GPU ({e})");
        return;
    }
    let full = run(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "full.mkv", "jobs": 2 }),
    );
    assert!(full.get("draft").is_none(), "{full}");
    let draft = run(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "draft.mkv", "jobs": 2, "proxies": true }),
    );
    assert_eq!(draft["draft"], true, "{draft}");
    assert_eq!(draft["proxies"].as_array().unwrap().len(), 1);
    assert_ne!(draft["video_blake3"], full["video_blake3"]);
    assert_eq!(draft["chunks"]["rendered"], 2);
    // Back to full resolution: all reused from the first render.
    let fin = run(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "final.mkv", "jobs": 2 }),
    );
    assert_eq!(fin["chunks"]["reused"], 2);
    assert_eq!(fin["video_blake3"], full["video_blake3"]);
    let rr = run(&cx, "report_read", json!({ "report": "draft.report.json" }));
    assert_eq!(rr["summary"]["draft"], true);
}
