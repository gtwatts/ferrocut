//! Generator layers (solid, linear / radial gradient): the kernel against
//! the CPU reference on the default adapter and lavapipe, whole-timeline
//! renders (no media needed), frame keys under keyframes and splits, and the
//! add_clip / set_param / set_keyframes ops. GPU tests skip with a note when
//! an adapter is unavailable.

use std::sync::{Arc, OnceLock};

use ferrocut_core::{
    AdapterPreference, CancelToken, FrameStorage, GpuContext, RationalTime, RenderCtx, WorkerState,
};
use ferrocut_engine::Timeline;
use ferrocut_engine::compile::compile;
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::generator::{GeneratorAt, GeneratorSpec};
use ferrocut_engine::graph::FrameCache;
use half::f16;

const W: u32 = 40;
const H: u32 = 24;

fn adapter(pref: AdapterPreference) -> Option<&'static GpuContext> {
    static GPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    static CPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    let cell = if matches!(pref, AdapterPreference::Cpu) {
        &CPU
    } else {
        &GPU
    };
    let label = format!("{pref:?}");
    cell.get_or_init(|| match GpuContext::new(pref) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("SKIP: no adapter for {label} ({e})");
            None
        }
    })
    .as_ref()
}

fn adapters() -> Vec<(&'static str, &'static GpuContext)> {
    let mut v = Vec::new();
    if let Some(g) = adapter(AdapterPreference::default()) {
        v.push(("default", g));
    }
    if let Some(g) = adapter(AdapterPreference::Cpu) {
        v.push(("lavapipe", g));
    }
    v
}

fn q(v: f32) -> f32 {
    f16::from_f32(v).to_f32()
}

fn codes(a: f32, b: f32) -> f32 {
    (a - b).abs() / a.abs().max(b.abs()).max(1.0) * 1023.0
}

fn max_codes(a: &[[f32; 4]], b: &[[f32; 4]]) -> (f32, usize) {
    let mut worst = (0.0f32, 0);
    for (i, (p, r)) in a.iter().zip(b).enumerate() {
        for k in 0..4 {
            let d = codes(p[k], r[k]);
            if d > worst.0 {
                worst = (d, i);
            }
        }
    }
    worst
}

fn readback(gpu: &GpuContext, f: &ferrocut_core::Frame) -> Vec<[f32; 4]> {
    let c = f.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(img) = &c.storage else {
        unreachable!()
    };
    let dw = c.data_window;
    assert_eq!((dw.width, dw.height, c.width, c.height), (W, H, W, H));
    (0..(W * H) as usize)
        .map(|i| [0, 1, 2, 3].map(|k| img.pixels[i * 4 + k].to_f32()))
        .collect()
}

fn ctx_parts(gpu: &GpuContext) -> (Arc<Compositor>, WorkerState) {
    let comp = Arc::new(Compositor::new(gpu));
    let mut w = WorkerState::default();
    w.slot(compositor_slot(), || Ok(comp.clone())).unwrap();
    (comp, w)
}

fn spec(json: &str) -> GeneratorSpec {
    let s: GeneratorSpec = serde_json::from_str(json).unwrap();
    s.validate().unwrap();
    s
}

fn cases() -> Vec<(&'static str, GeneratorAt)> {
    let at = |j: &str| spec(j).at(RationalTime::ZERO, W, H);
    vec![
        (
            "solid",
            at(r#"{"type": "solid", "color": ["0.8", "0.4", "0.1"]}"#),
        ),
        (
            "solid translucent",
            at(r#"{"type": "solid", "color": ["0.2", "0.9", "0.5", "0.3"]}"#),
        ),
        ("linear default", at(r#"{"type": "linear_gradient"}"#)),
        (
            "linear diagonal alpha",
            at(
                r#"{"type": "linear_gradient", "start": ["5", "3"], "end": ["31", "20"],
                    "start_color": ["1", "0", "0", "1"], "end_color": ["0", "0", "1", "0.25"]}"#,
            ),
        ),
        (
            "linear in linear light",
            at(
                r#"{"type": "linear_gradient", "start": ["0", "0"], "end": ["0", "24"],
                    "start_color": ["0", "0.5", "1", "0"], "end_color": ["1", "1", "0"], "interpolation": "linear"}"#,
            ),
        ),
        (
            "linear degenerate",
            at(r#"{"type": "linear_gradient", "start": ["7", "7"], "end": ["7", "7"]}"#),
        ),
        (
            "radial",
            at(
                r#"{"type": "radial_gradient", "center": ["12.5", "9"], "radius": "15",
                    "start_color": ["1", "1", "0.8"], "end_color": ["0.1", "0", "0.3", "0.6"]}"#,
            ),
        ),
        (
            "radial zero radius",
            at(r#"{"type": "radial_gradient", "radius": "0"}"#),
        ),
    ]
}

#[test]
fn kernel_matches_the_cpu_reference_on_every_adapter() {
    let ads = adapters();
    let mut worst = (0.0f32, 0.0f32);
    for (name, a) in cases() {
        let want: Vec<[f32; 4]> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .map(|(x, y)| a.pixel(x, y).map(q))
            .collect();
        let mut outs = Vec::new();
        for (an, gpu) in &ads {
            let (comp, mut w) = ctx_parts(gpu);
            let cancel = CancelToken::new();
            let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
            let f = comp.generate(&mut ctx, W, H, &a);
            ctx.flush();
            let got = readback(gpu, &f);
            let (d, i) = max_codes(&got, &want);
            assert!(
                d <= 1.0,
                "{name} on {an}: {d:.3} codes at ({}, {}): got {:?} want {:?}",
                i as u32 % W,
                i as u32 / W,
                got[i],
                want[i]
            );
            worst.0 = worst.0.max(d);
            outs.push(got);
        }
        if let [a, b] = &outs[..] {
            let (d, i) = max_codes(a, b);
            worst.1 = worst.1.max(d);
            assert!(d <= 1.0, "{name}: adapters differ by {d:.3} codes at {i}");
        }
    }
    eprintln!(
        "max difference: {:.4} codes vs the CPU reference, {:.4} between adapters",
        worst.0, worst.1
    );
}

const TL: &str = r#"{
  "output": { "width": 40, "height": 24, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [
      { "id": "bg", "start": 0, "duration": "2",
        "generator": { "type": "solid", "color": [
          { "keyframes": [ { "t": "0", "v": "0" }, { "t": "2", "v": "1" } ] }, "0.25", "0.5" ] } }
    ]},
    { "name": "V2", "clips": [
      { "id": "glow", "start": "1/2", "duration": "1", "opacity": "1/2",
        "generator": { "type": "radial_gradient", "radius": "12",
          "start_color": ["1", "1", "1"], "end_color": ["1", "1", "1", "0"] } }
    ]}
  ]
}"#;

#[test]
fn generator_timelines_render_without_media() {
    let tl = Timeline::from_json(TL).unwrap();
    let c = compile(&tl).unwrap();
    let bg = |t: RationalTime| {
        tl.tracks[0].clips[0]
            .generator
            .as_ref()
            .unwrap()
            .at(t, W, H)
    };
    let glow = tl.tracks[1].clips[0]
        .generator
        .as_ref()
        .unwrap()
        .at(RationalTime::ZERO, W, H);
    for (an, gpu) in adapters() {
        let (_, mut w) = ctx_parts(gpu);
        let cancel = CancelToken::new();
        let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
        let mut cache = FrameCache::new(8);
        for frame in [0i64, 18, 30] {
            let t = RationalTime::from_frames(frame, tl.output.fps);
            let f = c.graph.evaluate(c.output, t, &mut ctx, &mut cache).unwrap();
            ctx.flush();
            let got = readback(gpu, &f);
            // The solid's color keys are in source time (= clip-local here).
            let b = bg(t);
            assert_eq!(b.c0[0], frame as f64 / 48.0);
            let want: Vec<[f32; 4]> = (0..H)
                .flat_map(|y| (0..W).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let bp = b.pixel(x, y).map(q);
                    if !(12..36).contains(&frame) {
                        return bp;
                    }
                    let g = glow.pixel(x, y).map(|v| q(q(v) * 0.5));
                    [0, 1, 2, 3].map(|k| g[k] + bp[k] * (1.0 - g[3]))
                })
                .collect();
            let (d, i) = max_codes(&got, &want);
            assert!(
                d <= 1.5,
                "frame {frame} on {an}: {d:.3} codes at {i}: {:?} vs {:?}",
                got[i],
                want[i]
            );
        }
    }
}

fn keys(tl: &Timeline) -> Vec<ferrocut_core::FrameKey> {
    let c = compile(tl).unwrap();
    (0..tl.frame_count())
        .map(|i| {
            c.graph
                .frame_key(c.output, RationalTime::from_frames(i, tl.output.fps))
        })
        .collect()
}

fn run(base: &str, ops: &str) -> anyhow::Result<Timeline> {
    let ops = parse_ops(ops)?;
    apply(
        &Timeline::from_json(base).unwrap(),
        &ops,
        &mut MediaLengths::unbounded(),
    )
    .map(|(tl, _)| tl)
}

#[test]
fn keys_follow_parameters_and_survive_splits_and_trims() {
    let tl = Timeline::from_json(TL).unwrap();
    let base = keys(&tl);
    // The animated solid gives every frame its own key; the constant glow
    // only differs where it is visible.
    assert_ne!(base[1], base[2]);
    // Splitting the animated solid keeps every frame (keys in source time).
    let split = run(TL, r#"[{"op": "split", "clip": "bg", "at": "1"}]"#).unwrap();
    assert_eq!(split.tracks[0].clips.len(), 2);
    assert_eq!(keys(&split), base);
    // Trimming the in point keeps the remaining frames too.
    let trimmed = run(
        TL,
        r#"[{"op": "trim", "clip": "bg", "edge": "in", "delta": "1/4"}]"#,
    )
    .unwrap();
    let k = keys(&trimmed);
    for i in 0..base.len() {
        assert_eq!(k[i] == base[i], i >= 6, "frame {i}");
    }
    // Changing the glow's color touches only its frames.
    let red = run(
        TL,
        r#"[{"op": "set_param", "clip": "glow", "param": "generator.start_color.g", "value": "0"}]"#,
    )
    .unwrap();
    let k = keys(&red);
    for i in 0..base.len() {
        assert_eq!(k[i] != base[i], (12..36).contains(&i), "frame {i}");
    }
}

#[test]
fn add_clip_generators_and_param_ops() {
    let empty = r#"{"output": {"width": 40, "height": 24, "fps": "24"},
                    "tracks": [{"name": "V1", "clips": []}], "audio_tracks": [{"name": "A", "clips": []}]}"#;
    let tl = run(
        empty,
        r#"[
          {"op": "add_clip", "track": "V1", "generator": {"type": "solid", "color": ["1", "0", "0"]}, "duration": "2"},
          {"op": "add_clip", "track": "V1", "generator": {"type": "linear_gradient"}, "duration": "1"},
          {"op": "set_keyframes", "clip": "linear_gradient", "param": "generator.end_color.b",
           "keyframes": [{"t": "2", "v": "1"}, {"t": "3", "v": "0"}], "timeline_time": true},
          {"op": "set_param", "clip": "solid", "param": "generator.color", "value": ["0", "1", "0", "1/2"]}
        ]"#,
    )
    .unwrap();
    let c = &tl.tracks[0].clips;
    assert_eq!(
        (c[0].id.as_str(), c[1].id.as_str()),
        ("solid", "linear_gradient")
    );
    assert_eq!(c[1].start, RationalTime::new(2, 1));
    assert!(c[0].source.as_os_str().is_empty());
    // Timeline keys 2..3 land at source time 0..1 for the clip at 2 s.
    let g = c[1].generator.as_ref().unwrap();
    assert_eq!(g.at(RationalTime::ZERO, W, H).c1[2], 1.0);
    assert_eq!(g.at(RationalTime::new(1, 1), W, H).c1[2], 0.0);
    // Round trip: generator clips serialize without a source.
    let text = serde_json::to_string(&tl).unwrap();
    assert!(!text.contains("\"source\""), "{text}");
    Timeline::from_json(&text).unwrap();
    let err = |ops: &str| format!("{:#}", run(empty, ops).unwrap_err());
    assert!(
        err(r#"[{"op": "add_clip", "track": "V1", "generator": {"type": "solid", "color": ["1", "1", "1"]}}]"#)
            .contains("duration")
    );
    assert!(
        err(r#"[{"op": "add_clip", "track": "A", "generator": {"type": "solid", "color": ["1", "1", "1"]}, "duration": "1"}]"#)
            .contains("video tracks")
    );
    assert!(
        err(r#"[{"op": "add_clip", "track": "V1", "source": "a.mkv", "generator": {"type": "solid", "color": ["1", "1", "1"]}, "duration": "1"}]"#)
            .contains("not both")
    );
    assert!(
        err(
            r#"[{"op": "add_clip", "track": "V1", "generator": {"type": "rect"}, "duration": "1"}]"#
        )
        .contains("generator")
    );
    assert!(
        err(r#"[{"op": "add_clip", "track": "V1", "generator": {"type": "solid", "color": ["1", "1", "3"]}, "duration": "1"}]"#)
            .contains("[0, 1]")
    );
    // The timeline file rejects clips with neither or both.
    let bad = |clip: &str| {
        Timeline::from_json(&format!(
            r#"{{"output": {{"width": 4, "height": 4, "fps": "24"}}, "tracks": [{{"clips": [{clip}]}}]}}"#
        ))
        .unwrap_err()
        .to_string()
    };
    assert!(bad(r#"{"id": "x", "start": 0, "duration": 1}"#).contains("needs a source"));
    assert!(
        bad(r#"{"id": "x", "source": "a.mkv", "start": 0, "duration": 1, "generator": {"type": "linear_gradient"}}"#)
            .contains("not both")
    );
}
