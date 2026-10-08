//! Native finishing against independent f64 pixel references, then through
//! actual timeline edits/compilation and rendered compositions. GPU cases run
//! on both the default adapter and software Vulkan when available.

use std::sync::{Arc, OnceLock};

use ferrocut_core::effect::{Canvas, EffectRequest, ParamValue, WorkingSpace};
use ferrocut_core::{
    AdapterPreference, AlphaMode, CancelToken, ColorSpace, CpuFrame, CpuImage, ErrorKind, Frame,
    FrameKey, FrameStorage, GpuContext, PixelRect, Rational, RationalTime, RenderCtx, WorkerState,
};
use ferrocut_engine::Timeline;
use ferrocut_engine::compile::compile;
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::fx::{EffectStack, ParsedEffect, VideoEffectSpec};
use ferrocut_engine::graph::FrameCache;
use half::f16;
use serde_json::{Value, json};

const W: u32 = 16;
const H: u32 = 12;
const WIN: PixelRect = PixelRect::new(-3, 2, 12, 7);

fn adapters() -> Vec<(&'static str, &'static GpuContext)> {
    static DEFAULT: OnceLock<Option<GpuContext>> = OnceLock::new();
    static CPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    [
        ("default", &DEFAULT, AdapterPreference::default()),
        ("cpu", &CPU, AdapterPreference::Cpu),
    ]
    .into_iter()
    .filter_map(|(name, cell, pref)| {
        cell.get_or_init(|| match GpuContext::new(pref) {
            Ok(gpu) => {
                eprintln!(
                    "finishing adapter {name}: name={:?}, backend={:?}, type={:?}",
                    gpu.info.name, gpu.info.backend, gpu.info.device_type,
                );
                Some(gpu)
            }
            Err(e) => {
                eprintln!("SKIP finishing adapter {name}: {e}");
                None
            }
        })
        .as_ref()
        .map(|gpu| (name, gpu))
    })
    .collect()
}

fn parsed(value: Value) -> ParsedEffect {
    let spec: VideoEffectSpec = serde_json::from_value(value).unwrap();
    ParsedEffect::parse(&spec).unwrap()
}

fn q(v: f32) -> f32 {
    f16::from_f32(v).to_f32()
}

fn picture(x: i32, y: i32) -> [f32; 4] {
    // Negative/overrange RGB, zero/low alpha, neutral gray, green screen,
    // foreground primaries and a screen-contaminated transition edge.
    const COLORS: [[f32; 4]; 12] = [
        [0.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 0.5],
        [0.9, 0.1, 0.05, 1.0],
        [0.1, 0.3, 0.7, 0.25],
        [0.18, 0.18, 0.18, 1.0],
        [1.5, 0.5, 0.25, 1.0],
        [-0.125, 0.5, 0.1, 0.75],
        [0.0, 0.0, 0.0, 1.0],
        [0.2, 0.75, 0.3, 0.6],
        [0.2, 0.2, 0.2, 0.5],
        [0.8, 0.2, 0.1, 0.0001],
    ];
    let c = COLORS[((x - WIN.x + 3 * (y - WIN.y)) as usize) % COLORS.len()];
    [q(c[0] * c[3]), q(c[1] * c[3]), q(c[2] * c[3]), q(c[3])]
}

fn input(
    gpu: &GpuContext,
    space: &str,
    window: PixelRect,
    mut pixel: impl FnMut(i32, i32) -> [f32; 4],
) -> Frame {
    let mut pixels = Vec::new();
    for y in window.y..window.y + window.height as i32 {
        for x in window.x..window.x + window.width as i32 {
            pixels.extend(pixel(x, y).map(f16::from_f32));
        }
    }
    Frame::from_cpu(&CpuFrame {
        width: W,
        height: H,
        data_window: window,
        pixel_aspect: Rational::new(4, 3),
        color_space: ColorSpace::new(space),
        alpha: AlphaMode::Premultiplied,
        image: Arc::new(CpuImage { pixels }),
    })
    .to_gpu(gpu)
}

fn read(gpu: &GpuContext, frame: &Frame) -> Vec<[f32; 4]> {
    let cpu = frame.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(image) = cpu.storage else {
        unreachable!()
    };
    image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| [p[0].to_f32(), p[1].to_f32(), p[2].to_f32(), p[3].to_f32()])
        .collect()
}

fn request(region: PixelRect) -> EffectRequest {
    EffectRequest {
        time: RationalTime::ZERO,
        param_time: RationalTime::ZERO,
        effect_time: RationalTime::ZERO,
        region,
        canvas: Canvas {
            width: W,
            height: H,
            pixel_aspect: 4.0 / 3.0,
            frame_rate: 30.0,
        },
        label: "test".into(),
    }
}

fn run_direct(gpu: &GpuContext, effect: &ParsedEffect, region: PixelRect) -> Vec<[f32; 4]> {
    let p = effect.sample(RationalTime::ZERO);
    let source = input(gpu, effect.effect.working_space(&p).name(), WIN, picture);
    let mut worker = WorkerState::default();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
    let frame = effect
        .effect
        .render(&mut ctx, &source, &p, &request(region))
        .unwrap();
    ctx.flush();
    assert_eq!(frame.data_window, region);
    assert_eq!((frame.width, frame.height), (W, H));
    assert_eq!(frame.pixel_aspect, source.pixel_aspect);
    assert_eq!(frame.color_space, source.color_space);
    assert_eq!(frame.alpha, AlphaMode::Premultiplied);
    read(gpu, &frame)
}

/// Independent f64 implementation of the documented operations. Pixels enter
/// in the declared working space; this reference deliberately uses no GPU
/// uniforms, shader helpers or implementation functions.
fn reference(effect: &ParsedEffect, pixel: [f32; 4]) -> [f32; 4] {
    if pixel[3] <= 0.0 {
        return [0.0; 4];
    }
    let p = effect.sample(RationalTime::ZERO);
    let mut alpha = f64::from(pixel[3]);
    let mut c = [0, 1, 2].map(|i| f64::from(pixel[i]) / alpha);
    let dot = |c: [f64; 3], w: [f64; 3]| c[0] * w[0] + c[1] * w[1] + c[2] * w[2];
    let smooth = |x: f64, threshold: f64, width: f64| {
        if width == 0.0 {
            return if x > threshold { 1.0 } else { 0.0 };
        }
        let t = ((x - threshold) / width).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let weights = [0.2126, 0.7152, 0.0722];
    let cbcr = |c: [f64; 3], y: f64| [(c[2] - y) / 1.8556, (c[0] - y) / 1.5748];
    match effect.spec.kind.as_str() {
        "exposure" => c = c.map(|v| v * 2.0f64.powf(p.scalar("stops"))),
        "contrast" => {
            c = c.map(|v| p.scalar("pivot") + (v - p.scalar("pivot")) * p.scalar("amount"))
        }
        "saturation" => {
            let y = dot(c, [0.2722287168, 0.6740817658, 0.0536895174]);
            c = c.map(|v| y + p.scalar("amount") * (v - y));
        }
        "lift_gamma_gain" => {
            let lift = p.vec_opt("lift", 3).unwrap();
            let gamma = p.vec_opt("gamma", 3).unwrap();
            let gain = p.vec_opt("gain", 3).unwrap();
            c = [0, 1, 2].map(|i| {
                let v = c[i] + lift[i] * (1.0 - c[i]);
                v.signum() * v.abs().powf(1.0 / gamma[i]) * gain[i]
            });
        }
        "chroma_key" => {
            let key = p.color("key_color");
            let key = [key[0], key[1], key[2]];
            let y = dot(c, weights);
            let uv = cbcr(c, y);
            let key_uv = cbcr(key, dot(key, weights));
            let distance = ((uv[0] - key_uv[0]).powi(2) + (uv[1] - key_uv[1]).powi(2)).sqrt();
            alpha *= smooth(distance, p.scalar("tolerance"), p.scalar("softness"));
            let length2 = key_uv[0].powi(2) + key_uv[1].powi(2);
            if p.scalar("spill") > 0.0 && length2 > 1e-12 {
                let projection = ((uv[0] * key_uv[0] + uv[1] * key_uv[1]) / length2).max(0.0);
                let uv = [0, 1].map(|i| uv[i] - p.scalar("spill") * projection * key_uv[i]);
                let r = y + 1.5748 * uv[1];
                let b = y + 1.8556 * uv[0];
                c = [r, (y - weights[0] * r - weights[2] * b) / weights[1], b];
            }
        }
        "luma_key" => {
            let keep = smooth(dot(c, weights), p.scalar("threshold"), p.scalar("softness"));
            alpha *= if p.bool("invert") { 1.0 - keep } else { keep };
        }
        other => panic!("unexpected finishing effect {other}"),
    }
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    [
        q((c[0] * alpha).clamp(-65504.0, 65504.0) as f32),
        q((c[1] * alpha).clamp(-65504.0, 65504.0) as f32),
        q((c[2] * alpha).clamp(-65504.0, 65504.0) as f32),
        q(alpha as f32),
    ]
}

fn compare(a: &[[f32; 4]], b: &[[f32; 4]], label: &str) {
    assert_eq!(a.len(), b.len(), "{label}");
    let mut worst = 0.0f32;
    for (i, (a, b)) in a.iter().zip(b).enumerate() {
        for j in 0..4 {
            assert!(
                a[j].is_finite() && b[j].is_finite(),
                "{label} nonfinite pixel {i}: {a:?} {b:?}"
            );
            let codes = (a[j] - b[j]).abs() / a[j].abs().max(b[j].abs()).max(1.0) * 1023.0;
            worst = worst.max(codes);
            assert!(
                codes <= 1.5,
                "{label}: {codes} codes at {i} channel {j}: {a:?} vs {b:?}"
            );
        }
    }
    eprintln!("finishing {label}: max {worst:.4} normalized 10-bit codes");
}

fn cases() -> Vec<Value> {
    vec![
        json!({"type":"exposure", "stops": 2}),
        json!({"type":"contrast", "amount": "1.75", "pivot":"0.18"}),
        json!({"type":"saturation", "amount": 0}),
        json!({"type":"saturation", "amount":"1.7"}),
        json!({"type":"lift_gamma_gain", "lift":["0.04","-0.02","0.03"], "gamma":["0.8","1.2","1.1"], "gain":["1.1","0.9",1]}),
        json!({"type":"chroma_key", "tolerance":"0.08", "softness":"0.2", "spill":"0.8"}),
        json!({"type":"chroma_key", "key_color":[0,0,1], "tolerance":"0.08", "softness":0}),
        json!({"type":"chroma_key", "key_color":["0.5","0.5","0.5"], "spill":1}),
        json!({"type":"luma_key", "threshold":"0.1", "softness":"0.2"}),
        json!({"type":"luma_key", "threshold":"0.4", "softness":0, "invert":true}),
    ]
}

#[test]
fn gpu_matches_independent_references_with_alpha_hdr_and_partial_windows() {
    for value in cases() {
        let effect = parsed(value);
        for region in [
            WIN,
            PixelRect::new(0, 3, 4, 3),
            PixelRect::new(-5, 0, 16, 12),
        ] {
            let mut expected = Vec::new();
            for y in region.y..region.y + region.height as i32 {
                for x in region.x..region.x + region.width as i32 {
                    let inside = x >= WIN.x
                        && y >= WIN.y
                        && x < WIN.x + WIN.width as i32
                        && y < WIN.y + WIN.height as i32;
                    expected.push(if inside {
                        reference(&effect, picture(x, y))
                    } else {
                        [0.0; 4]
                    });
                }
            }
            let outputs: Vec<_> = adapters()
                .into_iter()
                .map(|(name, gpu)| {
                    let got = run_direct(gpu, &effect, region);
                    compare(
                        &got,
                        &expected,
                        &format!("{name} {} {region:?}", effect.spec.kind),
                    );
                    got
                })
                .collect();
            if let [a, b] = &outputs[..] {
                compare(a, b, &format!("cross-adapter {}", effect.spec.kind));
            }
        }
    }
}

#[test]
fn keying_repremultiplies_soft_edges_and_despill_preserves_luma() {
    let screen_chroma_length = ((0.7152f32 / 1.8556).powi(2) + (0.7152f32 / 1.5748).powi(2)).sqrt();
    let mix_gray = 0.2 / screen_chroma_length;
    // A gray/green mix exactly 0.2 CbCr units from green. With tolerance
    // 0.1 and softness 0.2 this has half coverage, then original alpha 0.5.
    let soft_edge = [0.25 * mix_gray, 0.5 - 0.25 * mix_gray, 0.25 * mix_gray, 0.5];
    let tests = [
        (json!({"type":"chroma_key"}), [0.0, 0.5, 0.0, 0.5], [0.0; 4]),
        (
            json!({"type":"chroma_key"}),
            [0.5, 0.0, 0.0, 0.5],
            [0.5, 0.0, 0.0, 0.5],
        ),
        (
            json!({"type":"chroma_key", "tolerance":"0.1", "softness":"0.2"}),
            soft_edge,
            soft_edge.map(|v| v * 0.5),
        ),
        (
            json!({"type":"luma_key", "threshold":"0.1", "softness":"0.2"}),
            [0.1, 0.1, 0.1, 0.5],
            [0.05, 0.05, 0.05, 0.25],
        ),
        (
            json!({"type":"luma_key", "threshold":"0.1", "softness":"0.2", "invert":true}),
            [0.1, 0.1, 0.1, 0.5],
            [0.05, 0.05, 0.05, 0.25],
        ),
        (
            json!({"type":"chroma_key", "tolerance":0,"softness":0,"spill":1}),
            [0.3, 0.6, 0.3, 1.0],
            [0.51456, 0.51456, 0.51456, 1.0],
        ),
    ];
    for (spec, pixel, expected) in tests {
        let effect = parsed(spec);
        let p = effect.sample(RationalTime::ZERO);
        for (name, gpu) in adapters() {
            let source = input(
                gpu,
                effect.effect.working_space(&p).name(),
                PixelRect::new(2, 3, 1, 1),
                |_, _| pixel,
            );
            let mut worker = WorkerState::default();
            let cancel = CancelToken::new();
            let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
            let out = effect
                .effect
                .render(&mut ctx, &source, &p, &request(source.data_window))
                .unwrap();
            ctx.flush();
            compare(
                &read(gpu, &out),
                &[expected],
                &format!("{name} semantic {}", effect.spec.kind),
            );
        }
    }
}

#[test]
fn extreme_gamma_and_zero_gain_stay_finite_at_half_float_storage_limits() {
    let effect = parsed(
        json!({"type":"lift_gamma_gain", "gamma":["0.1","0.1","0.1"], "gain":[0,"0.0001",1]}),
    );
    let p = effect.sample(RationalTime::ZERO);
    for (name, gpu) in adapters() {
        let source = input(
            gpu,
            effect.effect.working_space(&p).name(),
            PixelRect::new(0, 0, 1, 1),
            |_, _| [25.0, 25.0, 25.0, 0.5],
        );
        let mut worker = WorkerState::default();
        let cancel = CancelToken::new();
        let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
        let out = effect
            .effect
            .render(&mut ctx, &source, &p, &request(source.data_window))
            .unwrap();
        ctx.flush();
        compare(
            &read(gpu, &out),
            &[[0.0, 65504.0, 65504.0, 0.5]],
            &format!("{name} extreme gamma"),
        );
    }
}

#[test]
fn neutral_grades_and_disabled_effects_return_the_original_frame() {
    let specs: Vec<VideoEffectSpec> = serde_json::from_value(json!([
        {"type":"exposure"}, {"type":"contrast","pivot":"0.25"}, {"type":"saturation"},
        {"type":"lift_gamma_gain"}, {"type":"chroma_key","enabled":false}, {"type":"luma_key","enabled":false}
    ])).unwrap();
    let stack = EffectStack::new("neutral", &specs, RationalTime::ZERO).unwrap();
    assert!(stack.hash_bytes_at(RationalTime::ZERO).is_empty());
    for (_, gpu) in adapters() {
        let frame = Arc::new(input(gpu, ColorSpace::ACESCG, WIN, picture));
        let mut worker = WorkerState::default();
        let cancel = CancelToken::new();
        let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
        let out = stack
            .apply(&mut ctx, frame.clone(), RationalTime::ZERO)
            .unwrap();
        assert!(Arc::ptr_eq(&frame, &out));
        assert!(!ctx.worker.has_pending_gpu_work());
    }
}

#[test]
fn parameter_bounds_working_spaces_and_nonfinite_samples_are_checked() {
    for (value, message) in [
        (json!({"type":"exposure","stops":17}), "stops"),
        (json!({"type":"contrast","amount":-1}), "amount"),
        (json!({"type":"contrast","pivot":0}), "pivot"),
        (json!({"type":"saturation","amount":9}), "amount"),
        (json!({"type":"lift_gamma_gain","gamma":[1,0,1]}), "gamma"),
        (json!({"type":"lift_gamma_gain","gain":[1,1]}), "gain"),
        (json!({"type":"chroma_key","tolerance":-1}), "tolerance"),
        (json!({"type":"chroma_key","spill":2}), "spill"),
        (json!({"type":"luma_key","softness":"1.1"}), "softness"),
        (json!({"type":"exposure","unknown":1}), "unknown"),
    ] {
        let spec: VideoEffectSpec = serde_json::from_value(value).unwrap();
        let err = ParsedEffect::parse(&spec).unwrap_err();
        assert!(err.contains(message), "{err}");
    }
    for value in cases() {
        let effect = parsed(value);
        let mut p = effect.sample(RationalTime::ZERO);
        let expected = match effect.spec.kind.as_str() {
            "exposure" | "contrast" | "saturation" => WorkingSpace::ACESCG,
            _ => WorkingSpace::CAMERA_REC709,
        };
        assert_eq!(effect.effect.working_space(&p), expected);
        let first = effect.effect.params()[0];
        p.insert(first.name, ParamValue::Scalar(f64::NAN));
        assert!(effect.effect.validate(&p).unwrap_err().contains("finite"));
    }
}

fn base() -> Timeline {
    Timeline::from_json(
        &json!({
            "output":{"width":W,"height":H,"fps":24,"gop":12,"gops_per_chunk":1},
            "tracks":[{"name":"V1","clips":[{"id":"base","start":0,"duration":2,
                "generator":{"type":"solid","color":["0.25","0.3","0.4"]}}]}]
        })
        .to_string(),
    )
    .unwrap()
}

fn edit(tl: &Timeline, ops: Value) -> Timeline {
    apply(
        tl,
        &parse_ops(&ops.to_string()).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0
}

fn keys(tl: &Timeline) -> Vec<FrameKey> {
    let c = compile(tl).unwrap();
    (0..tl.frame_count())
        .map(|i| {
            c.graph
                .frame_key(c.output, RationalTime::from_frames(i, tl.output.fps))
        })
        .collect()
}

fn render_timeline(gpu: &GpuContext, tl: &Timeline, frames: &[i64]) -> Vec<Vec<[f32; 4]>> {
    let c = compile(tl).unwrap();
    let mut worker = WorkerState::default();
    worker
        .slot(compositor_slot(), || Ok(Arc::new(Compositor::new(gpu))))
        .unwrap();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
    let mut cache = FrameCache::new(8);
    frames
        .iter()
        .map(|&i| {
            let frame = c
                .graph
                .evaluate(
                    c.output,
                    RationalTime::from_frames(i, tl.output.fps),
                    &mut ctx,
                    &mut cache,
                )
                .unwrap();
            ctx.flush();
            read(gpu, &frame)
        })
        .collect()
}

#[test]
fn animated_edits_invalidate_only_changed_frames_and_render_in_source_order() {
    let neutral = edit(
        &base(),
        json!([{"op":"add_video_effect","clip":"base","effect":{"type":"exposure","id":"grade"}}]),
    );
    let animated = edit(
        &neutral,
        json!([{"op":"set_video_effect_param","clip":"base","effect":"grade","param":"stops",
        "value":{"keyframes":[{"t":0,"v":0,"interp":"hold"},{"t":1,"v":1}]}}]),
    );
    let before = keys(&neutral);
    let after = keys(&animated);
    assert_eq!(&before[..24], &after[..24]);
    assert!(before[24..].iter().zip(&after[24..]).all(|(a, b)| a != b));
    let disabled = edit(
        &animated,
        json!([{"op":"set_video_effect_param","clip":"base","effect":"grade","param":"enabled","value":false}]),
    );
    assert_eq!(keys(&disabled), before);
    for (name, gpu) in adapters() {
        let outputs = render_timeline(gpu, &animated, &[24, 0, 24]);
        assert_eq!(outputs[0], outputs[2], "{name}: random frame order differs");
        let expected: Vec<_> = outputs[1]
            .iter()
            .map(|p| [2.0 * p[0], 2.0 * p[1], 2.0 * p[2], p[3]])
            .collect();
        compare(&outputs[0], &expected, &format!("{name} animated exposure"));
    }
}

#[test]
fn expression_driven_video_grade_survives_split_and_preserves_frame_keys() {
    let grade = edit(
        &base(),
        json!([{"op":"add_video_effect","clip":"base","effect":{"type":"exposure","stops":{"expression":"time"}}}]),
    );
    let split = edit(
        &grade,
        json!([{"op":"split","clip":"base","at":1,"new_id":"tail"}]),
    );
    assert_eq!(keys(&grade), keys(&split));
    assert!(
        serde_json::to_value(&split).unwrap()["tracks"][0]["clips"][1]["effects"][0]["stops"]
            .get("expression")
            .is_some()
    );
}

#[test]
fn clip_track_and_adjustment_finishing_share_the_same_rendered_behavior() {
    let clip = edit(
        &base(),
        json!([{"op":"add_video_effect","clip":"base","effect":{"type":"exposure","stops":1}}]),
    );
    let track = edit(
        &base(),
        json!([{"op":"add_video_effect","track":"V1","effect":{"type":"exposure","stops":1}}]),
    );
    let adjustment = edit(
        &base(),
        json!([
            {"op":"add_track","kind":"video","name":"GRADE"},
            {"op":"add_clip","track":"GRADE","id":"grade","start":0,"duration":2,"adjustment":true},
            {"op":"add_video_effect","clip":"grade","effect":{"type":"exposure","stops":1}}
        ]),
    );
    for (name, gpu) in adapters() {
        let clip = &render_timeline(gpu, &clip, &[12])[0];
        let track = &render_timeline(gpu, &track, &[12])[0];
        let adj = &render_timeline(gpu, &adjustment, &[12])[0];
        compare(clip, track, &format!("{name} clip vs track"));
        compare(clip, adj, &format!("{name} clip vs adjustment"));
    }
}

#[test]
fn keyed_layer_reveals_the_background_through_the_actual_compiler() {
    let keyed = edit(
        &base(),
        json!([
            {"op":"add_track","kind":"video","name":"SCREEN"},
            {"op":"add_clip","track":"SCREEN","id":"screen","start":0,"duration":2,"generator":{"type":"solid","color":[0,1,0]}},
            {"op":"add_video_effect","clip":"screen","effect":{"type":"chroma_key","spill":"0.8"}}
        ]),
    );
    for (name, gpu) in adapters() {
        let expected = &render_timeline(gpu, &base(), &[1])[0];
        let got = &render_timeline(gpu, &keyed, &[1])[0];
        compare(got, expected, &format!("{name} keyed comp"));
    }
}

#[test]
fn encoded_to_linear_stack_conversion_saturates_instead_of_overflowing() {
    // Both effects accept these parameters. The encoded gamma result is
    // finite, but BT.709 decoding can exceed f16 when returning to ACEScg.
    let extreme = edit(
        &base(),
        json!([
            {"op":"add_video_effect","clip":"base","effect":{"type":"exposure","stops":16}},
            {"op":"add_video_effect","clip":"base","effect":{"type":"lift_gamma_gain","gamma":["0.1","0.1","0.1"]}}
        ]),
    );
    for (name, gpu) in adapters() {
        let output = &render_timeline(gpu, &extreme, &[0])[0];
        for pixel in output {
            assert!(pixel.iter().all(|v| v.is_finite()), "{name}: {pixel:?}");
            assert!(
                pixel[..3].iter().all(|v| *v > 30000.0 && *v <= 65504.0),
                "{name}: storage saturation changed to {pixel:?}"
            );
            assert_eq!(pixel[3], 1.0);
        }
    }
}

#[test]
fn worker_pipeline_cache_is_device_specific_and_cancellation_is_respected() {
    let effect = parsed(json!({"type":"exposure","stops":1}));
    let p = effect.sample(RationalTime::ZERO);
    let mut worker = WorkerState::default();
    let cancel = CancelToken::new();
    let mut outputs = Vec::new();
    for (name, gpu) in adapters() {
        // Reusing the same worker across two devices would fail wgpu validation
        // if the pipeline cache omitted GpuContext::id().
        let frame = input(gpu, ColorSpace::ACESCG, WIN, picture);
        let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
        let out = effect
            .effect
            .render(&mut ctx, &frame, &p, &request(WIN))
            .unwrap();
        ctx.flush();
        outputs.push(read(gpu, &out));
        let wrong_space = input(gpu, WorkingSpace::CAMERA_REC709.name(), WIN, picture);
        let error = effect
            .effect
            .render(&mut ctx, &wrong_space, &p, &request(WIN))
            .unwrap_err();
        assert!(error.message.contains("expects ACEScg"), "{name}: {error}");
        let empty = effect
            .effect
            .render(&mut ctx, &frame, &p, &request(PixelRect::default()))
            .unwrap_err();
        assert_eq!(empty.kind, ErrorKind::Permanent);
        let cancelled = CancelToken::new();
        cancelled.cancel();
        let mut ctx = RenderCtx::new(gpu, &mut worker, &cancelled, None);
        let error = effect
            .effect
            .render(&mut ctx, &frame, &p, &request(WIN))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
    }
    if let [a, b] = &outputs[..] {
        compare(a, b, "worker reused across devices");
    }
}
