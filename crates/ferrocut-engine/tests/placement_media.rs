//! Actual decode/compile/render placement: a known round shape must stay round.
use ferrocut_core::{AdapterPreference, GpuContext, Rational, RationalTime, SharedGpu};
use ferrocut_engine::media::{
    decode::Decoder,
    encode::{ChunkEncoder, EncodeSettings},
    proxy,
};
use ferrocut_engine::{RenderOptions, Timeline, compile, render};
use serde_json::json;
use std::path::Path;

fn source(path: &Path) {
    let mut encoder = ChunkEncoder::create(
        path,
        &EncodeSettings {
            width: 64,
            height: 32,
            fps: Rational::from_int(24),
            gop: 1,
        },
    )
    .unwrap();
    let mut bgra = vec![0; 64 * 32 * 4];
    for y in 0..32i32 {
        for x in 0..64i32 {
            let inside = (2 * x + 1 - 64).pow(2) + (2 * y + 1 - 32).pow(2) < 16 * 16;
            let i = ((y * 64 + x) * 4) as usize;
            bgra[i..i + 4].copy_from_slice(&[
                if inside { 255 } else { 0 },
                if inside { 255 } else { 0 },
                if inside { 255 } else { 0 },
                255,
            ]);
        }
    }
    for _ in 0..2 {
        encoder.push_bgra(&bgra).unwrap();
    }
    encoder.finish().unwrap();
}

fn white_bounds(path: &Path) -> [u32; 4] {
    let mut decoder = Decoder::open(path, 64, 64).unwrap();
    let rgba = decoder.frame_at(RationalTime::ZERO).unwrap();
    let mut bounds = [64, 64, 0, 0];
    for y in 0..64 {
        for x in 0..64 {
            let i = ((y * 64 + x) * 4) as usize;
            if rgba[i..i + 3].iter().all(|v| *v > 128) {
                bounds[0] = bounds[0].min(x);
                bounds[1] = bounds[1].min(y);
                bounds[2] = bounds[2].max(x + 1);
                bounds[3] = bounds[3].max(y + 1);
            }
        }
    }
    bounds
}

#[test]
fn round_source_stays_round_through_fit_proxy_and_native_mask() {
    let Some(gpu) = GpuContext::new(AdapterPreference::default())
        .map(SharedGpu::new)
        .map_err(|e| eprintln!("SKIP: no GPU ({e})"))
        .ok()
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    source(&d.join("circle.mkv"));
    let mut value = json!({"output":{"width":64,"height":64,"fps":24,"gop":1},"tracks":[{"clips":[{"id":"circle","source":d.join("circle.mkv"),"start":0,"duration":"1/12"}]}]});
    let run = |tl: &Timeline, name: &str, draft: bool, jobs| {
        let graph = if draft {
            ferrocut_engine::compile::compile_proxies(tl).unwrap().0
        } else {
            compile(tl).unwrap()
        };
        let out = d.join(format!("{name}.mkv"));
        let report = render(
            tl,
            &graph,
            &gpu,
            &out,
            &RenderOptions {
                jobs,
                ..RenderOptions::new(d.join(name))
            },
        )
        .unwrap();
        (report, white_bounds(&out))
    };
    let mut contain_hash = String::new();
    for (fit, expected) in [
        ("contain", [24, 24, 40, 40]),
        ("none", [24, 24, 40, 40]),
        ("cover", [16, 16, 48, 48]),
        ("stretch", [24, 16, 40, 48]),
    ] {
        value["tracks"][0]["clips"][0]["fit"] = json!(fit);
        let tl = Timeline::from_json(&value.to_string()).unwrap();
        let (report, bounds) = run(&tl, fit, false, 1);
        assert_eq!(bounds, expected, "{fit}");
        assert_eq!(report.warnings.is_empty(), fit != "stretch");
        if fit == "contain" {
            contain_hash = report.video_blake3;
        }
    }
    value["tracks"][0]["clips"][0]["fit"] = json!("contain");
    let tl = Timeline::from_json(&value.to_string()).unwrap();
    assert_eq!(run(&tl, "repeat-j2", false, 2).0.video_blake3, contain_hash);
    proxy::generate(&d.join("circle.mkv"), false).unwrap();
    assert_eq!(run(&tl, "proxy", true, 1).1, [24, 24, 40, 40]);
    assert_eq!(run(&tl, "final", false, 1).0.video_blake3, contain_hash);
    // An inner 128x32 canvas is fitted as one native layer in the 64x64
    // parent. Its centered source circle becomes an 8x8 circle after the
    // outer 1/2 fit; the parent still has a 64x64 display.
    let mut inner = value.clone();
    inner["output"]["width"] = json!(128);
    inner["output"]["height"] = json!(32);
    std::fs::write(d.join("inner.json"), inner.to_string()).unwrap();
    let mut nested = value.clone();
    nested["tracks"][0]["clips"][0]["source"] = json!(d.join("inner.json"));
    let nested = Timeline::from_json(&nested.to_string()).unwrap();
    assert_eq!(run(&nested, "nested", false, 1).1, [28, 28, 36, 36]);
    // The mask is expressed in the 64x32 native image, before placement.
    value["tracks"][0]["clips"][0]["masks"] =
        json!([{"geometry":{"type":"rectangle","x":32,"y":0,"width":32,"height":32}}]);
    let tl = Timeline::from_json(&value.to_string()).unwrap();
    assert_eq!(run(&tl, "mask", false, 1).1, [32, 24, 40, 40]);
}
