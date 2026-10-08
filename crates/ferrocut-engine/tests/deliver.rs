//! `render --deliver mp4` plumbing: IDRs on render chunk starts, output
//! bit-identical across delivery jobs, clean refusal without the codec.
//!
//! Never downloads OpenH264. The encode test runs only with a codec the user
//! already provided: `FERROCUT_OPENH264_LIB` (plus `FERROCUT_OPENH264_UNVERIFIED=1`
//! for a non-Cisco build), or a verified cached copy they enabled. Otherwise
//! it prints SKIP and passes (CI, fresh machines).

use std::path::Path;
use std::process::Command;

use ferrocut_core::{AdapterPreference, CancelToken, ErrorKind, GpuContext, Rational, SharedGpu};
use ferrocut_engine::deliver::{self, DeliverRequest, openh264};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::render::RenderOptions;
use ferrocut_engine::{RenderReport, Timeline, compile, render};

const W: u32 = 160;
const H: u32 = 96;
const DEAD_URL: &str = "http://127.0.0.1:9";

fn synth(path: &Path, frames: i64) {
    let s = EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..frames {
        let mut px = vec![0u8; (W * H * 4) as usize];
        for (i, p) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i as i64 % W as i64, i as i64 / W as i64);
            p.copy_from_slice(&[(x * 2 + f * 5) as u8, (y * 3 + f) as u8, (x + y) as u8, 255]);
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

/// A 60-frame render in 12-frame chunks (starts 0, 12, 24, 36, 48), or None
/// without a GPU.
fn render_master(dir: &Path) -> Option<RenderReport> {
    let gpu = match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => SharedGpu::new(g),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            return None;
        }
    };
    synth(&dir.join("a.mkv"), 60);
    let json = format!(
        r#"{{ "output": {{ "width": {W}, "height": {H}, "fps": "24", "gop": 12 }},
              "tracks": [ {{ "clips": [ {{ "id": "a", "source": "a.mkv", "start": 0, "duration": "5/2" }} ] }} ] }}"#
    );
    let p = dir.join("tl.json");
    std::fs::write(&p, json).unwrap();
    let tl = Timeline::load(&p).unwrap();
    let c = compile(&tl).unwrap();
    let opts = RenderOptions {
        jobs: 2,
        ..RenderOptions::new(dir.join("cache"))
    };
    Some(render(&tl, &c, &gpu, &dir.join("master.mkv"), &opts).unwrap())
}

/// A provider for a codec the user already has, or None (SKIP). Can't download.
fn codec_or_skip() -> Option<openh264::Provider> {
    let mut p = openh264::Provider::from_env().ok()?;
    p.base_url = DEAD_URL.into();
    p.allow_download = false;
    let s = p.status();
    let usable = p.override_lib.is_some()
        || (s.choice == openh264::UserChoice::Enabled && s.cached.is_some() && s.verified);
    if !usable {
        eprintln!(
            "SKIP: OpenH264 not enabled (set {} or run `ferrocut-deliver openh264 enable`)",
            openh264::ENV_LIB
        );
        return None;
    }
    Some(p)
}

/// Keyframe packet indices of the first video stream, via the LGPL ffprobe.
fn keyframes(mp4: &Path) -> Option<Vec<i64>> {
    let prefix = std::path::PathBuf::from(std::env::var_os("FERROCUT_LGPL_FFMPEG_PREFIX")?);
    let out = Command::new(prefix.join("bin/ffprobe"))
        .env("LD_LIBRARY_PATH", prefix.join("lib"))
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .args(["packet=flags", "-of", "csv=p=0"])
        .arg(mp4)
        .output()
        .ok()?;
    assert!(out.status.success(), "ffprobe failed");
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains('K'))
            .map(|(i, _)| i as i64)
            .collect(),
    )
}

#[test]
fn without_openh264_delivery_fails_permanently_and_downloads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let Some(r) = render_master(dir.path()) else {
        return;
    };
    let cache = dir.path().join("openh264-cache");
    let mut p = openh264::Provider::with_cache_dir(&cache);
    p.base_url = DEAD_URL.into();
    let e = deliver::deliver(&r, &DeliverRequest::mp4(p), None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent, "{e}");
    assert!(e.message.contains("openh264 enable"), "{e}");
    assert!(!dir.path().join("master.mp4").exists());
    assert!(
        !cache.exists() || std::fs::read_dir(&cache).unwrap().next().is_none(),
        "nothing may be downloaded or recorded"
    );
}

#[test]
fn cancelled_delivery_never_starts() {
    let dir = tempfile::tempdir().unwrap();
    let Some(r) = render_master(dir.path()) else {
        return;
    };
    let cancel = CancelToken::new();
    cancel.cancel();
    let p = openh264::Provider::with_cache_dir(dir.path().join("c"));
    let e = deliver::deliver(&r, &DeliverRequest::mp4(p), Some(&cancel)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled, "{e}");
}

#[test]
fn deterministic_across_jobs_with_idrs_on_render_chunks() {
    let Some(codec) = codec_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let Some(r) = render_master(dir.path()) else {
        return;
    };
    let starts: Vec<i64> = r.chunks.iter().map(|c| c.plan.start_frame).collect();
    assert_eq!(starts, vec![0, 12, 24, 36, 48]);

    let run = |jobs: usize| {
        let req = DeliverRequest {
            jobs,
            output: Some(dir.path().join(format!("j{jobs}.mp4"))),
            ..DeliverRequest::mp4(codec.clone())
        };
        deliver::deliver(&r, &req, None).unwrap().0
    };
    let (a, b) = (run(1), run(4));
    assert_eq!(a.output_sha256, b.output_sha256, "jobs changed the bytes");
    assert_eq!((a.jobs, b.jobs), (1, 4));
    assert_eq!(a.frames, 60);
    assert_eq!(a.idr_frames, starts);
    assert!(a.report.is_file(), "deliver.json written");
    let bytes = std::fs::read(&a.output).unwrap();
    assert_eq!(bytes.len() as u64, a.output_bytes);
    match keyframes(&a.output) {
        Some(k) => assert_eq!(k, starts, "IDRs must sit on render chunk starts"),
        None => eprintln!("note: no LGPL ffprobe; IDR check used the report only"),
    }
    // The summary is what lands in the render report's `deliver` section.
    let mut with = r.clone();
    with.deliver = Some(a.clone());
    let v = serde_json::to_value(&with).unwrap();
    assert_eq!(v["deliver"]["idr_frames"], serde_json::json!(starts));
    assert_eq!(v["deliver"]["format"], "mp4");
}
