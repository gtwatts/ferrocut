//! `render` with `deliver` and the `openh264` tool. Nothing here can download
//! OpenH264: providers point at a temp cache and a dead URL. The encode test
//! runs only with a codec the user already provided (`FERROCUT_OPENH264_LIB`,
//! or a verified copy they enabled); otherwise it prints SKIP.

use std::path::Path;

use ferrocut_core::Rational;
use ferrocut_engine::deliver::openh264;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::{RenderProgress, RenderStage};
use ferrocut_mcp::{Ctx, Root, call, progress_value_with};
use serde_json::{Value, json};

const DEAD_URL: &str = "http://127.0.0.1:9";
const W: u32 = 96;
const H: u32 = 64;

fn run(cx: &Ctx, tool: &str, args: Value) -> Result<Value, String> {
    call(cx, tool, args)
        .expect("known tool")
        .map_err(|e| format!("{e:#}"))
}

fn offline_provider(cache: &Path) -> openh264::Provider {
    let mut p = openh264::Provider::with_cache_dir(cache);
    p.base_url = DEAD_URL.into();
    p
}

/// A project root with a 48-frame clip and a timeline in 12-frame chunks.
fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let proj = std::fs::canonicalize(dir.path()).unwrap().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let s = EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(&proj.join("a.mkv"), &s).unwrap();
    for f in 0..48i64 {
        let px: Vec<u8> = (0..W * H)
            .flat_map(|i| {
                [
                    (i as i64 + f * 7) as u8,
                    (i / W) as u8 * 3,
                    f as u8 * 5,
                    255,
                ]
            })
            .collect();
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
    std::fs::write(
        proj.join("tl.json"),
        format!(
            r#"{{ "output": {{ "width": {W}, "height": {H}, "fps": "24", "gop": 12 }},
                  "tracks": [ {{ "clips": [ {{ "id": "a", "source": "a.mkv", "start": 0, "duration": "2" }} ] }} ] }}"#
        ),
    )
    .unwrap();
    (dir, proj)
}

fn has_gpu() -> bool {
    match ferrocut_core::GpuContext::new(ferrocut_core::AdapterPreference::default()) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            false
        }
    }
}

fn empty(dir: &Path) -> bool {
    !dir.exists() || std::fs::read_dir(dir).unwrap().next().is_none()
}

#[test]
fn openh264_tool_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let mut cx = Ctx::new(Root::new(dir.path()).unwrap());
    cx.openh264 = Some(offline_provider(&cache));

    let s = run(&cx, "openh264", json!({ "action": "status" })).unwrap();
    assert_eq!(s["status"]["choice"], "unset", "{s}");
    assert_eq!(s["status"]["cached"], Value::Null);
    assert_eq!(s["notice"], openh264::NOTICE);
    assert!(empty(&cache), "status must not write anything");

    let l = run(&cx, "openh264", json!({ "action": "license" })).unwrap();
    assert!(l["license"].as_str().unwrap().contains("Cisco"));
    assert_eq!(l["notice"], openh264::NOTICE);

    let d = run(
        &cx,
        "openh264",
        json!({ "action": "disable", "remove": true }),
    )
    .unwrap();
    assert_eq!(d["status"]["choice"], "disabled", "{d}");

    // enable is the only path that downloads; here Cisco's host is a dead port.
    let e = run(&cx, "openh264", json!({ "action": "enable" })).unwrap_err();
    assert!(
        e.contains("127.0.0.1:9") || e.to_lowercase().contains("connect"),
        "{e}"
    );
    let lib = cache.join(openh264::VERSION);
    assert!(empty(&lib), "no library may appear without a download");

    let bad = run(&cx, "openh264", json!({ "action": "install" })).unwrap_err();
    assert!(bad.contains("invalid arguments"), "{bad}");
}

#[test]
fn deliver_progress_has_one_more_step() {
    let p = |stage| RenderProgress {
        stage,
        chunks_done: 4,
        total_chunks: 4,
        frames_done: 48,
        total_frames: 48,
        reused_chunks: 0,
    };
    assert_eq!(
        progress_value_with(&p(RenderStage::Concat), true),
        (50.0, 52.0)
    );
    assert_eq!(
        progress_value_with(&p(RenderStage::Deliver), true),
        (51.0, 52.0)
    );
    assert_eq!(
        progress_value_with(&p(RenderStage::Done), true),
        (52.0, 52.0)
    );
    assert_eq!(
        progress_value_with(&p(RenderStage::Done), false),
        (51.0, 51.0)
    );
}

#[test]
fn render_deliver_outside_root_is_refused_before_rendering() {
    let (_d, proj) = project();
    let cx = Ctx::new(Root::new(&proj).unwrap());
    let e = run(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "out.mkv", "deliver": { "output": "../x.mp4" } }),
    )
    .unwrap_err();
    assert!(e.contains("outside the project root"), "{e}");
    assert!(!proj.join("out.mkv").exists());
}

#[test]
fn render_deliver_without_codec_says_how_to_enable_and_downloads_nothing() {
    if !has_gpu() {
        return;
    }
    let (d, proj) = project();
    let cache = d.path().join("openh264-cache");
    let mut cx = Ctx::new(Root::new(&proj).unwrap());
    cx.openh264 = Some(offline_provider(&cache));
    let e = run(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "out.mkv", "deliver": "mp4" }),
    )
    .unwrap_err();
    assert!(e.contains("delivery failed"), "{e}");
    assert!(e.contains("openh264 enable"), "{e}");
    assert!(proj.join("out.mkv").is_file(), "the master is kept");
    assert!(proj.join("out.report.json").is_file());
    assert!(!proj.join("out.mp4").exists());
    assert!(empty(&cache), "nothing downloaded or recorded");
}

/// A provider for a codec the user already has, or None. Can't download.
fn codec_or_skip() -> Option<openh264::Provider> {
    let mut p = openh264::Provider::from_env().ok()?;
    p.base_url = DEAD_URL.into();
    p.allow_download = false;
    let s = p.status();
    if p.override_lib.is_some()
        || (s.choice == openh264::UserChoice::Enabled && s.cached.is_some() && s.verified)
    {
        return Some(p);
    }
    eprintln!("SKIP: OpenH264 not enabled ({} unset)", openh264::ENV_LIB);
    None
}

#[test]
fn render_with_deliver_mp4() {
    let Some(codec) = codec_or_skip() else { return };
    if !has_gpu() {
        return;
    }
    let (_d, proj) = project();
    let mut cx = Ctx::new(Root::new(&proj).unwrap());
    cx.openh264 = Some(codec);
    let stages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = stages.clone();
    cx.progress = Some(std::sync::Arc::new(move |p: &RenderProgress| {
        sink.lock().unwrap().push(p.stage)
    }));
    let s = run(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "out.mkv", "jobs": 2,
                "deliver": { "format": "mp4", "qp": 24, "jobs": 3 } }),
    )
    .unwrap();
    let dv = &s["deliver"];
    assert_eq!(dv["idr_frames"], json!([0, 12, 24, 36]), "{s}");
    assert_eq!(
        (dv["qp"].as_u64(), dv["jobs"].as_u64()),
        (Some(24), Some(3))
    );
    assert_eq!(dv["notice"], openh264::NOTICE);
    assert!(proj.join("out.mp4").is_file());
    assert!(proj.join("out.deliver.json").is_file());
    let r = run(&cx, "report_read", json!({ "report": "out.report.json" })).unwrap();
    assert_eq!(
        r["summary"]["deliver"]["output_sha256"],
        dv["output_sha256"]
    );
    // The render's own Done is held back: Deliver, then exactly one Done, last.
    let stages = stages.lock().unwrap();
    let n = stages.len();
    assert!(n >= 2, "{stages:?}");
    assert_eq!(&stages[n - 2..], &[RenderStage::Deliver, RenderStage::Done]);
    assert_eq!(
        stages.iter().filter(|s| **s == RenderStage::Done).count(),
        1
    );
}
