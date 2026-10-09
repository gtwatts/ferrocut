//! Proxies: half-resolution DNxHR LB / FFV1 copies keyed by content hash,
//! read by draft renders only; final renders always use the original media
//! (and their chunks and hashes don't depend on whether proxies exist).

use std::path::Path;

use ferrocut_core::{AdapterPreference, GpuContext, Rational, RationalTime, SharedGpu};
use ferrocut_engine::compile::compile_proxies;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::media::proxy;
use ferrocut_engine::render::ChunkStatus;
use ferrocut_engine::{RenderOptions, Timeline, compile, render};

fn synth(path: &Path, w: u32, h: u32, frames: i64, seed: u8) {
    let s = EncodeSettings {
        width: w,
        height: h,
        fps: Rational::from_int(24),
        gop: 12,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    let mut px = vec![0u8; (w * h * 4) as usize];
    for f in 0..frames {
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = (x as i64 + f * 4) as u8 ^ seed;
                px[i + 1] = (y as i64 * 2 + f) as u8;
                px[i + 2] = seed.wrapping_add(((x / 8 + y / 8) * 16) as u8);
                px[i + 3] = 255;
            }
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

#[test]
fn proxies_are_half_size_cached_by_content_and_codec_by_size() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    synth(&d.join("big.mkv"), 640, 360, 30, 1);
    synth(&d.join("small.mkv"), 64, 48, 12, 2);

    let big = proxy::generate(&d.join("big.mkv"), false).unwrap();
    assert!(big.created);
    assert_eq!(big.codec, "dnxhr_lb");
    assert_eq!((big.width, big.height), (320, 180));
    assert_eq!(big.frames, Some(30));
    assert!(big.proxy.starts_with(d.join(".ferrocut-proxies")));
    let pi = ferrocut_engine::media::probe(&big.proxy).unwrap();
    assert_eq!(
        pi.duration,
        ferrocut_engine::media::probe(&d.join("big.mkv"))
            .unwrap()
            .duration
    );

    // Proxy under DNxHR's 256x120 minimum: FFV1.
    let small = proxy::generate(&d.join("small.mkv"), false).unwrap();
    assert_eq!(small.codec, "ffv1");
    assert_eq!((small.width, small.height), (32, 24));
    assert_eq!(
        ferrocut_engine::media::probe(&small.proxy)
            .unwrap()
            .duration,
        Some(RationalTime::new(1, 2))
    );

    // Kept unless forced; found by the source's current content.
    let again = proxy::generate(&d.join("big.mkv"), false).unwrap();
    assert!(!again.created);
    assert_eq!(again.proxy, big.proxy);
    assert_eq!(again.codec, "dnxhr_lb");
    assert!(proxy::generate(&d.join("big.mkv"), true).unwrap().created);
    let h = proxy::file_hash(&d.join("big.mkv")).unwrap();
    assert_eq!(proxy::find(&d.join("big.mkv"), &h), Some(big.proxy.clone()));
    // A changed file has no proxy until one is made.
    synth(&d.join("big.mkv"), 640, 360, 30, 9);
    let h2 = proxy::file_hash(&d.join("big.mkv")).unwrap();
    assert_eq!(proxy::find(&d.join("big.mkv"), &h2), None);
    let fresh = proxy::generate(&d.join("big.mkv"), false).unwrap();
    assert!(fresh.created);
    assert_ne!(fresh.proxy, big.proxy);
}

const TL: &str = r#"{
  "output": { "width": 512, "height": 256, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [
      { "id": "a", "source": "big.mkv", "start": "0", "source_in": "0", "duration": "1" },
      { "id": "n", "source": "comp.json", "start": "1", "source_in": "0", "duration": "1/2" } ] },
    { "name": "V2", "clips": [
      { "id": "g", "generator": { "type": "solid", "color": ["1", "0", "0", "1/4"] }, "start": "0", "duration": "3/2" } ] } ],
  "audio_tracks": [ { "name": "A1", "clips": [
    { "id": "m", "source": "gone.wav", "start": "0", "source_in": "0", "duration": "1" } ] } ]
}"#;

#[test]
fn odd_dimension_proxy_placement_uses_the_original_canvas() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("odd.mkv");
    synth(&source, 641, 361, 2, 1);
    let generated = proxy::generate(&source, false).unwrap();
    assert_eq!((generated.width, generated.height), (322, 181));
    let tl = Timeline::from_json(
        &serde_json::json!({
            "output":{"width":1920,"height":1080,"fps":24},
            "tracks":[{"clips":[{"id":"odd","source":source,"start":0,"duration":"1/12"}]}]
        })
        .to_string(),
    )
    .unwrap();
    let final_graph = compile(&tl).unwrap();
    let (draft_graph, used) = compile_proxies(&tl).unwrap();
    assert_eq!(used, [generated.proxy]);
    let final_placement = &final_graph.placements[0];
    assert_eq!(final_placement.native, (641, 361));
    assert_eq!(final_placement.fit_scale, [Rational::new(1080, 361); 2]);
    assert_eq!(
        serde_json::to_value(&draft_graph.placements).unwrap(),
        serde_json::to_value(&final_graph.placements).unwrap()
    );
    assert_ne!(
        draft_graph
            .graph
            .frame_key(draft_graph.output, RationalTime::ZERO),
        final_graph
            .graph
            .frame_key(final_graph.output, RationalTime::ZERO)
    );
}

const COMP: &str = r#"{
  "output": { "width": 512, "height": 256, "fps": "24", "gop": 12 },
  "tracks": [ { "name": "V1", "clips": [
    { "id": "c", "source": "other.mkv", "start": "0", "source_in": "0", "duration": "1/2" } ] } ]
}"#;

#[test]
fn sources_status_and_draft_vs_final_renders() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    synth(&d.join("big.mkv"), 256, 128, 30, 3);
    synth(&d.join("other.mkv"), 512, 256, 18, 4);
    std::fs::write(d.join("comp.json"), COMP).unwrap();
    std::fs::write(d.join("tl.json"), TL).unwrap();
    let tl = Timeline::load(&d.join("tl.json")).unwrap();

    // Video sources follow nested comps; generators and audio are skipped.
    let srcs = proxy::video_sources(&tl).unwrap();
    assert_eq!(srcs, [d.join("big.mkv"), d.join("other.mkv")]);

    let st = proxy::media_status(&tl, true);
    let row = |n: &str| st.iter().find(|s| s.path.ends_with(n)).unwrap();
    assert!(row("big.mkv").online && row("big.mkv").proxy.is_none());
    assert_eq!(row("comp.json").clips, ["n"]);
    assert!(!row("gone.wav").online);
    assert_eq!(row("gone.wav").kind, "audio");

    // The audio file is offline: render the video only.
    let mut tl = tl;
    tl.audio_tracks.clear();
    let keys = |c: &ferrocut_engine::compile::Compiled| {
        (0..tl.frame_count())
            .map(|i| {
                c.graph
                    .frame_key(c.output, RationalTime::from_frames(i, tl.output.fps))
            })
            .collect::<Vec<_>>()
    };
    let k_full = keys(&compile(&tl).unwrap());
    let (c0, used0) = compile_proxies(&tl).unwrap();
    assert!(used0.is_empty());
    assert_eq!(keys(&c0), k_full, "no proxies: a draft is the full render");

    let gpu = match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => Some(SharedGpu::new(g)),
        Err(e) => {
            eprintln!("SKIP renders: no GPU ({e})");
            None
        }
    };
    let opts = || RenderOptions {
        jobs: 2,
        ..RenderOptions::new(d.join("cache"))
    };
    let before = gpu.as_ref().map(|g| {
        render(
            &tl,
            &compile(&tl).unwrap(),
            g,
            &d.join("before.mkv"),
            &opts(),
        )
        .unwrap()
    });

    for s in &srcs {
        let codec = if s.ends_with("big.mkv") {
            "ffv1"
        } else {
            "dnxhr_lb"
        };
        assert_eq!(proxy::generate(s, false).unwrap().codec, codec);
    }
    let st = proxy::media_status(&tl, true);
    assert!(
        st.iter()
            .filter(|s| s.kind == "video" && !s.path.ends_with("comp.json"))
            .all(|s| s.proxy.is_some())
    );

    let (cd, used) = compile_proxies(&tl).unwrap();
    assert_eq!(used.len(), 2, "{used:?}");
    let k_draft = keys(&cd);
    // Frames showing media get new keys; generator-only frames (none here) would not.
    assert!(k_draft.iter().zip(&k_full).all(|(a, b)| a != b));
    assert_eq!(
        keys(&compile(&tl).unwrap()),
        k_full,
        "final keys ignore proxies"
    );

    let Some(gpu) = gpu else { return };
    let before = before.unwrap();
    let draft = render(&tl, &cd, &gpu, &d.join("draft.mkv"), &opts()).unwrap();
    assert!(
        draft
            .chunks
            .iter()
            .all(|c| c.status == ChunkStatus::Rendered)
    );
    assert_ne!(draft.video_blake3, before.video_blake3);
    // The final render swaps back to the originals: every chunk comes from
    // the cache filled before the proxies existed, same output.
    let fin = render(
        &tl,
        &compile(&tl).unwrap(),
        &gpu,
        &d.join("final.mkv"),
        &opts(),
    )
    .unwrap();
    assert!(fin.chunks.iter().all(|c| c.status == ChunkStatus::Reused));
    assert_eq!(fin.video_blake3, before.video_blake3);
    // And the draft still reuses its own chunks.
    let draft2 = render(&tl, &cd, &gpu, &d.join("draft2.mkv"), &opts()).unwrap();
    assert!(
        draft2
            .chunks
            .iter()
            .all(|c| c.status == ChunkStatus::Reused)
    );
    assert_eq!(draft2.video_blake3, draft.video_blake3);
}
