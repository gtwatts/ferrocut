//! Video effect stacks and adjustment layers: kernels against CPU
//! references, the default adapter against lavapipe (within 1 code), regions
//! of interest, working-space conversions, error kinds, edit ops, keys, and
//! adjustment layers with opacity and mattes. GPU tests skip with a note
//! when an adapter is unavailable.

use std::sync::{Arc, OnceLock};

use ferrocut_core::effect::{self, EffectParams, EffectRequest, VideoEffect, WorkingSpace};
use ferrocut_core::param::{ParamSpec, TimeBase};
use ferrocut_core::{
    AdapterPreference, AlphaMode, CancelToken, ColorSpace, CpuFrame, CpuImage, ErrorKind, Frame,
    FrameStorage, GpuContext, NodeError, PixelRect, Rational, RationalTime, RenderCtx, WorkerState,
};
use ferrocut_engine::Timeline;
use ferrocut_engine::compile::compile;
use ferrocut_engine::compositor::{Compositor, compositor, compositor_slot};
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::fx::{EffectStack, VideoEffectSpec};
use ferrocut_engine::graph::FrameCache;
use half::f16;

const W: u32 = 48;
const H: u32 = 32;
const WIN: PixelRect = PixelRect::new(8, 6, 24, 16);

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

/// Difference in 10-bit codes relative to the larger magnitude (>= 1).
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

fn worker(gpu: &GpuContext) -> WorkerState {
    let comp = Arc::new(Compositor::new(gpu));
    let mut w = WorkerState::default();
    w.slot(compositor_slot(), || Ok(comp.clone())).unwrap();
    w
}

/// The test picture: smooth premultiplied colors with varying alpha in `WIN`.
fn pattern(x: i32, y: i32) -> [f32; 4] {
    if !(WIN.x..WIN.x + WIN.width as i32).contains(&x)
        || !(WIN.y..WIN.y + WIN.height as i32).contains(&y)
    {
        return [0.0; 4];
    }
    let (u, v) = (
        (x - WIN.x) as f32 / WIN.width as f32,
        (y - WIN.y) as f32 / WIN.height as f32,
    );
    let a = 0.4 + 0.6 * ((u * 7.0).sin() * 0.5 + 0.5);
    let c = [u * 1.5, v, (u * v * 9.0).cos() * 0.5 + 0.5];
    [q(c[0] * a), q(c[1] * a), q(c[2] * a), q(a)]
}

fn input(gpu: &GpuContext) -> Frame {
    let mut px = Vec::new();
    for y in WIN.y..WIN.y + WIN.height as i32 {
        for x in WIN.x..WIN.x + WIN.width as i32 {
            px.extend(pattern(x, y).map(f16::from_f32));
        }
    }
    let f = CpuFrame {
        width: W,
        height: H,
        data_window: WIN,
        pixel_aspect: Rational::ONE,
        color_space: ColorSpace::acescg(),
        alpha: AlphaMode::Premultiplied,
        image: Arc::new(CpuImage { pixels: px }),
    };
    Frame::from_cpu(&f).to_gpu(gpu)
}

fn read(gpu: &GpuContext, f: &Frame) -> (PixelRect, Vec<[f32; 4]>) {
    let c = f.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(img) = &c.storage else {
        unreachable!()
    };
    let n = (c.data_window.width * c.data_window.height) as usize;
    (
        c.data_window,
        (0..n)
            .map(|i| [0, 1, 2, 3].map(|k| img.pixels[i * 4 + k].to_f32()))
            .collect(),
    )
}

fn specs(json: &str) -> Vec<VideoEffectSpec> {
    serde_json::from_str(json).unwrap()
}

/// Run a stack on the test picture.
fn run_stack(gpu: &GpuContext, json: &str) -> Result<(PixelRect, Vec<[f32; 4]>), NodeError> {
    let stack = EffectStack::new("test", &specs(json), RationalTime::ZERO).unwrap();
    let mut w = worker(gpu);
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let f = stack.apply(&mut ctx, Arc::new(input(gpu)), RationalTime::ZERO)?;
    ctx.flush();
    assert_eq!(f.color_space, ColorSpace::acescg());
    Ok(read(gpu, &f))
}

fn weights(sigma: f64) -> Vec<f64> {
    let r = (3.0 * sigma).ceil() as i32;
    let w: Vec<f64> = (-r..=r)
        .map(|k| (-(k as f64).powi(2) / (2.0 * sigma * sigma)).exp())
        .collect();
    let s: f64 = w.iter().sum();
    w.iter().map(|v| v / s).collect()
}

/// Separable Gaussian in f64 (transparent outside `WIN`, or clamped to it).
fn cpu_blur(sigma: [f64; 2], clamp: bool, out: PixelRect) -> Vec<[f32; 4]> {
    let src = |x: i32, y: i32| {
        let (x, y) = if clamp {
            (
                x.clamp(WIN.x, WIN.x + WIN.width as i32 - 1),
                y.clamp(WIN.y, WIN.y + WIN.height as i32 - 1),
            )
        } else {
            (x, y)
        };
        pattern(x, y).map(f64::from)
    };
    let (wx, wy) = (weights(sigma[0].max(1e-9)), weights(sigma[1].max(1e-9)));
    let (rx, ry) = ((wx.len() as i32 - 1) / 2, (wy.len() as i32 - 1) / 2);
    let mut v = Vec::new();
    for y in out.y..out.y + out.height as i32 {
        for x in out.x..out.x + out.width as i32 {
            let mut acc = [0.0f64; 4];
            for ky in -ry..=ry {
                for kx in -rx..=rx {
                    let p = src(x + kx, y + ky);
                    let w = wx[(kx + rx) as usize] * wy[(ky + ry) as usize];
                    for c in 0..4 {
                        acc[c] += w * p[c];
                    }
                }
            }
            v.push(acc.map(|a| a as f32));
        }
    }
    v
}

#[test]
fn gaussian_blur_matches_the_cpu_reference_at_the_edges() {
    for (an, gpu) in adapters() {
        // Transparent edges: the data window grows by ceil(3 sigma) = 5.
        let (win, got) = run_stack(gpu, r#"[{"type": "gaussian_blur", "sigma": "1.5"}]"#).unwrap();
        assert_eq!(win, PixelRect::new(3, 1, 34, 26), "{an}");
        let (d, i) = max_codes(&got, &cpu_blur([1.5, 1.5], false, win));
        assert!(d <= 1.0, "{an}: blur {d:.3} codes at {i}");
        // Repeated edges keep the window.
        let (win, got) = run_stack(
            gpu,
            r#"[{"type": "gaussian_blur", "sigma": 2, "repeat_edges": true}]"#,
        )
        .unwrap();
        assert_eq!(win, WIN);
        let (d, i) = max_codes(&got, &cpu_blur([2.0, 2.0], true, win));
        assert!(d <= 1.0, "{an}: repeat-edge blur {d:.3} codes at {i}");
        // One axis.
        let (win, got) = run_stack(
            gpu,
            r#"[{"type": "gaussian_blur", "sigma": 1, "dimensions": "vertical"}]"#,
        )
        .unwrap();
        assert_eq!(win, PixelRect::new(8, 3, 24, 22));
        let (d, _) = max_codes(&got, &cpu_blur([0.0, 1.0], false, win));
        assert!(d <= 1.0, "{an}: vertical blur {d:.3} codes");
    }
}

const ALL: &[&str] = &[
    r#"[{"type": "gaussian_blur", "sigma": "2.5"}]"#,
    r#"[{"type": "directional_blur", "angle": 30, "length": 9}]"#,
    r#"[{"type": "unsharp_mask", "amount": "1.5", "sigma": "1.2", "threshold": "0.01"}]"#,
    r#"[{"type": "sharpen", "amount": 2}]"#,
    r#"[{"type": "glow", "threshold": "0.3", "sigma": 3, "intensity": "1.5", "color": [1, "0.8", "0.6"]}]"#,
    r#"[{"type": "drop_shadow", "distance": 4, "softness": 2, "opacity": "0.8", "color": ["0.2", 0, "0.4"]}]"#,
    r#"[{"type": "drop_shadow", "distance": 3, "angle": 200, "shadow_only": true}]"#,
    r#"[{"type": "transform", "scale": ["0.7", "1.2"], "rotation": 20, "position": [20, 15], "opacity": "0.75"}]"#,
    r#"[{"type": "crop", "left": "10.5", "top": 8, "right": 18, "bottom": 9, "feather": 3}]"#,
    r#"[{"type": "letterbox", "aspect": "2.39", "color": ["0.1", "0.2", "0.3"], "opacity": "0.9"}]"#,
    r#"[{"type": "letterbox", "aspect": 1, "opacity": 1}]"#,
    r#"[{"type": "gaussian_blur", "sigma": 1}, {"type": "glow", "threshold": "0.2", "sigma": 2},
        {"type": "drop_shadow", "distance": 2, "softness": 1}, {"type": "crop", "left": 12, "feather": 2},
        {"type": "letterbox", "aspect": 2}]"#,
];

#[test]
fn every_effect_matches_across_adapters() {
    let ads = adapters();
    let mut worst: f32 = 0.0;
    for json in ALL {
        let outs: Vec<_> = ads
            .iter()
            .map(|(_, g)| run_stack(g, json).unwrap())
            .collect();
        let base = {
            let gpu = ads[0].1;
            read(gpu, &input(gpu))
        };
        assert!(outs[0] != base, "{json}: no change");
        if let [(wa, a), (wb, b)] = &outs[..] {
            assert_eq!(wa, wb, "{json}: windows differ");
            let (d, i) = max_codes(a, b);
            assert!(
                d <= 1.0,
                "{json}: adapters differ by {d:.3} codes at {i}: {:?} vs {:?}",
                a[i],
                b[i]
            );
            worst = worst.max(d);
        }
    }
    eprintln!("max difference between adapters: {worst:.4} codes");
}

#[test]
fn regions_of_interest_do_not_change_pixels() {
    for (an, gpu) in adapters() {
        // Blur then crop: the stack asks the blur only for the crop rect.
        let (cw, cropped) = run_stack(
            gpu,
            r#"[{"type": "gaussian_blur", "sigma": 2}, {"type": "crop", "left": 20, "top": 10, "right": 14, "bottom": 12}]"#,
        )
        .unwrap();
        assert_eq!(cw, PixelRect::new(20, 10, 14, 10), "{an}");
        let (bw, blurred) = run_stack(gpu, r#"[{"type": "gaussian_blur", "sigma": 2}]"#).unwrap();
        let mut want = Vec::new();
        for y in cw.y..cw.y + cw.height as i32 {
            for x in cw.x..cw.x + cw.width as i32 {
                want.push(blurred[((y - bw.y) * bw.width as i32 + (x - bw.x)) as usize]);
            }
        }
        assert_eq!(cropped, want, "{an}: ROI changed pixels");
        // Disabled and identity effects are skipped entirely.
        let (w0, p0) = run_stack(gpu, r#"[{"type": "gaussian_blur", "sigma": 3, "enabled": false}, {"type": "glow", "intensity": 0}]"#).unwrap();
        assert_eq!((w0, p0), read(gpu, &input(gpu)));
    }
}

// ---------------------------------------------------------------- plug-in API

struct Probe;
const PROBE_PARAMS: &[ParamSpec] = &[ParamSpec::choice(
    "space",
    &["srgb", "rec709", "acescg"],
    "\"srgb\"",
    "",
)];
impl VideoEffect for Probe {
    fn type_name(&self) -> &str {
        "test:probe"
    }
    fn doc(&self) -> &str {
        "returns its input; checks the working space it was given"
    }
    fn params(&self) -> &[ParamSpec] {
        PROBE_PARAMS
    }
    fn working_space(&self, p: &EffectParams) -> WorkingSpace {
        match p.choice("space") {
            Some("srgb") => WorkingSpace::SRGB_REC709,
            Some("rec709") => WorkingSpace::LINEAR_REC709,
            _ => WorkingSpace::ACESCG,
        }
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        let want = self.working_space(p);
        if input.color_space.name() != want.name() {
            return Err(NodeError::permanent(format!(
                "got {}",
                input.color_space.name()
            )));
        }
        let comp = compositor(ctx)?;
        comp.reframe(ctx, input, req.region)
    }
}

struct Fail;
const FAIL_PARAMS: &[ParamSpec] = &[
    ParamSpec::choice(
        "kind",
        &["retryable", "permanent", "window"],
        "\"permanent\"",
        "",
    ),
    ParamSpec::scalar("level", TimeBase::ClipLocal, "", "0", "").range(0.0, 1.0),
];
impl VideoEffect for Fail {
    fn type_name(&self) -> &str {
        "test:fail"
    }
    fn doc(&self) -> &str {
        "fails"
    }
    fn params(&self) -> &[ParamSpec] {
        FAIL_PARAMS
    }
    fn validate(&self, p: &EffectParams) -> Result<(), String> {
        if p.scalar("level") > 0.5 {
            Err("level must be at most 0.5 here".into())
        } else {
            Ok(())
        }
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        match p.choice("kind") {
            Some("retryable") => Err(NodeError::retryable("plugin process crashed")),
            Some("window") => {
                let comp = compositor(ctx)?;
                comp.reframe(ctx, input, PixelRect::new(req.region.x, req.region.y, 1, 1))
            }
            _ => Err(NodeError::permanent("bad pixels")),
        }
    }
}

fn register_test_effects() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        effect::register(Arc::new(Probe)).unwrap();
        effect::register(Arc::new(Fail)).unwrap();
    });
}

#[test]
fn plugin_effects_get_their_working_space_and_errors_keep_their_kind() {
    register_test_effects();
    // Registration rules.
    assert!(
        effect::register(Arc::new(Probe))
            .unwrap_err()
            .contains("already registered")
    );
    struct Reserved;
    impl VideoEffect for Reserved {
        fn type_name(&self) -> &str {
            "test:reserved"
        }
        fn doc(&self) -> &str {
            ""
        }
        fn params(&self) -> &[ParamSpec] {
            const P: &[ParamSpec] = &[ParamSpec::scalar(
                "enabled",
                TimeBase::ClipLocal,
                "",
                "0",
                "",
            )];
            P
        }
        fn render(
            &self,
            _: &mut RenderCtx<'_>,
            _: &Frame,
            _: &EffectParams,
            _: &EffectRequest,
        ) -> Result<Frame, NodeError> {
            unreachable!()
        }
    }
    assert!(
        effect::register(Arc::new(Reserved))
            .unwrap_err()
            .contains("reserved")
    );
    // Validation runs the effect's own checks on constant parameters.
    let e = EffectStack::new(
        "clip c",
        &specs(r#"[{"type": "test:fail", "level": "0.75"}]"#),
        RationalTime::ZERO,
    )
    .unwrap_err();
    assert!(e.to_string().contains("level must be at most 0.5"), "{e}");
    let mut rounds = Vec::new();
    for (an, gpu) in adapters() {
        let base = read(gpu, &input(gpu));
        // sRGB, sRGB (one conversion in), linear Rec.709, ACEScg: round trip.
        let (w, got) = run_stack(
            gpu,
            r#"[{"type": "test:probe"}, {"type": "test:probe"}, {"type": "test:probe", "space": "rec709"}, {"type": "test:probe", "space": "acescg"}]"#,
        )
        .unwrap();
        assert_eq!(w, base.0);
        // f16 holds sRGB-encoded values above 1 in 2^-10 steps, which decode
        // to ~2.5 linear codes; scene-linear spaces round trip within 1.
        let (d, i) = max_codes(&got, &base.1);
        assert!(
            d <= 3.5,
            "{an}: working-space round trip off by {d:.3} codes at {i}: {:?} vs {:?}",
            got[i],
            base.1[i]
        );
        let (_, lin) = run_stack(gpu, r#"[{"type": "test:probe", "space": "rec709"}]"#).unwrap();
        let (d, i) = max_codes(&lin, &base.1);
        assert!(
            d <= 1.0,
            "{an}: linear Rec.709 round trip off by {d:.3} codes at {i}"
        );
        rounds.push(got);
        let e = run_stack(
            gpu,
            r#"[{"type": "test:fail", "id": "boom", "kind": "retryable"}]"#,
        )
        .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Retryable);
        assert_eq!(
            e.message,
            "test: effect \"boom\" (test:fail): plugin process crashed"
        );
        let e =
            run_stack(gpu, r#"[{"type": "gaussian_blur"}, {"type": "test:fail"}]"#).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Permanent);
        assert_eq!(e.message, "test: effect #1 (test:fail): bad pixels");
        let e = run_stack(gpu, r#"[{"type": "test:fail", "kind": "window"}]"#).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Permanent);
        assert!(e.message.contains("asked for"), "{}", e.message);
    }
    if let [a, b] = &rounds[..] {
        let (d, i) = max_codes(a, b);
        assert!(
            d <= 1.0,
            "conversions differ across adapters by {d:.3} codes at {i}"
        );
    }
}

// ---------------------------------------------------------------- timelines

const TL: &str = r#"{
  "output": { "width": 48, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [
      { "id": "bg", "start": 0, "duration": "2",
        "generator": { "type": "linear_gradient", "start": ["0", "0"], "end": ["48", "32"],
          "start_color": ["0.9", "0.2", "0.1"], "end_color": ["0.1", "0.3", "0.9"] } }
    ]},
    { "name": "V2", "clips": [
      { "id": "dot", "start": 0, "duration": "2",
        "generator": { "type": "radial_gradient", "center": ["24", "16"], "radius": "10",
          "start_color": ["1", "1", "0.9"], "end_color": ["1", "1", "1", "0"] } }
    ]}
  ]
}"#;

fn ops(base: &str, ops: &str) -> anyhow::Result<Timeline> {
    let ops = parse_ops(ops)?;
    apply(
        &Timeline::from_json(base).unwrap(),
        &ops,
        &mut MediaLengths::unbounded(),
    )
    .map(|(tl, _)| tl)
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

/// Render frames of a timeline as full display windows.
fn render(gpu: &GpuContext, tl: &Timeline, frames: &[i64]) -> Vec<Vec<[f32; 4]>> {
    let c = compile(tl).unwrap();
    let mut w = worker(gpu);
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let mut cache = FrameCache::new(8);
    frames
        .iter()
        .map(|&i| {
            let f = c
                .graph
                .evaluate(
                    c.output,
                    RationalTime::from_frames(i, tl.output.fps),
                    &mut ctx,
                    &mut cache,
                )
                .unwrap();
            let comp = compositor(&mut ctx).unwrap();
            let f = comp.reframe(&mut ctx, &f, PixelRect::full(W, H)).unwrap();
            ctx.flush();
            read(gpu, &f).1
        })
        .collect()
}

#[test]
fn edit_ops_build_and_change_stacks() {
    let tl = ops(
        TL,
        r#"[
          {"op": "add_video_effect", "clip": "dot", "effect": {"type": "gaussian_blur", "id": "soft", "sigma": 2}},
          {"op": "add_video_effect", "clip": "dot", "effect": {"type": "glow"}, "index": 0},
          {"op": "set_video_effect_param", "clip": "dot", "effect": 0, "param": "color.g", "value": "1/2"},
          {"op": "set_video_effect_param", "clip": "dot", "effect": "soft", "param": "sigma",
           "value": {"keyframes": [{"t": "0", "v": "0"}, {"t": "2", "v": "4"}]}},
          {"op": "move_video_effect", "clip": "dot", "effect": "soft", "to": 0},
          {"op": "add_video_effect", "track": "V1", "effect": {"type": "letterbox", "aspect": 2}},
          {"op": "set_video_effect_param", "track": "V1", "effect": 0, "param": "enabled", "value": false}
        ]"#,
    )
    .unwrap();
    let fx = &tl.tracks[1].clips[0].effects;
    assert_eq!(fx.len(), 2);
    assert_eq!(fx[0].id.as_deref(), Some("soft"));
    assert_eq!(fx[1].kind, "glow");
    assert_eq!(fx[1].params["color"], serde_json::json!([1, "1/2", 1]));
    assert!(!tl.tracks[0].effects[0].enabled);
    let text = serde_json::to_string(&tl).unwrap();
    assert_eq!(
        serde_json::to_string(&Timeline::from_json(&text).unwrap()).unwrap(),
        text
    );
    // Animated sigma: every frame has its own key; a split keeps them.
    let k = keys(&tl);
    assert_ne!(k[10], k[11]);
    let split = ops(&text, r#"[{"op": "split", "clip": "dot", "at": "1"}]"#).unwrap();
    assert_eq!(keys(&split), k);
    let removed = ops(
        &text,
        r#"[{"op": "remove_video_effect", "clip": "dot", "effect": "soft"},
                                 {"op": "remove_video_effect", "clip": "dot", "effect": 0}]"#,
    )
    .unwrap();
    assert!(removed.tracks[1].clips[0].effects.is_empty());
    // Precise errors.
    let err = |o: &str| format!("{:#}", ops(&text, o).unwrap_err());
    let e = err(r#"[{"op": "add_video_effect", "clip": "dot", "effect": {"type": "blur"}}]"#);
    assert!(
        e.contains("unknown video effect type \"blur\"") && e.contains("gaussian_blur"),
        "{e}"
    );
    let e = err(
        r#"[{"op": "add_video_effect", "clip": "dot", "effect": {"type": "gaussian_blur", "radius": 3}}]"#,
    );
    assert!(
        e.contains("unknown parameter \"radius\"") && e.contains("sigma"),
        "{e}"
    );
    let e = err(
        r#"[{"op": "set_video_effect_param", "clip": "dot", "effect": "soft", "param": "sigma", "value": 900}]"#,
    );
    assert!(e.contains("sigma: 900 is above the maximum 500"), "{e}");
    let e = err(
        r#"[{"op": "set_video_effect_param", "clip": "dot", "effect": "nope", "param": "sigma", "value": 1}]"#,
    );
    assert!(
        e.contains("no effect with id \"nope\" (effects: \"soft\", #1)"),
        "{e}"
    );
    let e = err(
        r#"[{"op": "set_video_effect_param", "clip": "dot", "effect": 1, "param": "dimensions", "value": "x"}]"#,
    );
    assert!(e.contains("unknown parameter \"dimensions\""), "{e}");
    let e = err(
        r#"[{"op": "set_video_effect_param", "clip": "dot", "effect": "soft", "param": "dimensions", "value": "diagonal"}]"#,
    );
    assert!(
        e.contains("\"diagonal\" is not one of both, horizontal, vertical"),
        "{e}"
    );
    let e = err(r#"[{"op": "move_video_effect", "clip": "dot", "effect": 0, "to": 2}]"#);
    assert!(e.contains("past the end"), "{e}");
    let e = err(
        r#"[{"op": "add_video_effect", "clip": "dot", "effect": {"type": "glow", "id": "soft"}}]"#,
    );
    assert!(e.contains("\"soft\" is already used"), "{e}");
    let e = err(
        r#"[{"op": "add_video_effect", "clip": "dot", "effect": {"type": "glow", "sigma": 0.5}}]"#,
    );
    assert!(e.contains("rational string"), "{e}");
    // Adjustment layers from ops.
    let adj = ops(
        TL,
        r#"[{"op": "add_track", "kind": "video", "name": "ADJ"},
            {"op": "add_clip", "track": "ADJ", "adjustment": true, "start": "1/2", "duration": "1"},
            {"op": "add_video_effect", "clip": "adjustment", "effect": {"type": "gaussian_blur", "sigma": 3}}]"#,
    )
    .unwrap();
    let c = &adj.tracks[2].clips[0];
    assert!(c.adjustment && c.source.as_os_str().is_empty() && c.effects.len() == 1);
    let e = format!("{:#}", ops(TL, r#"[{"op": "add_clip", "track": "V1", "adjustment": true, "start": "3", "duration": "1"}]"#).unwrap_err());
    assert!(e.contains("need a track of their own"), "{e}");
}

#[test]
fn clip_effects_render_like_the_stack_on_the_clip() {
    // Effects on the top clip, then its (keyframed) opacity.
    let tl = ops(
        TL,
        r#"[{"op": "add_video_effect", "clip": "dot", "effect": {"type": "drop_shadow", "distance": 3, "softness": "1.5", "opacity": 1}},
            {"op": "set_param", "clip": "dot", "param": "opacity", "value": "1/2"}]"#,
    )
    .unwrap();
    let plain = ops(
        TL,
        r#"[{"op": "set_param", "clip": "dot", "param": "opacity", "value": "1/2"}]"#,
    )
    .unwrap();
    let mut outs = Vec::new();
    for (an, gpu) in adapters() {
        let a = &render(gpu, &tl, &[5])[0];
        let b = &render(gpu, &plain, &[5])[0];
        // The shadow darkens pixels down-right of the dot only.
        let px = |v: &Vec<[f32; 4]>, x: usize, y: usize| v[y * W as usize + x];
        assert!(
            px(a, 30, 22)[0] < px(b, 30, 22)[0] - 0.01,
            "{an}: {:?} vs {:?}",
            px(a, 30, 22),
            px(b, 30, 22)
        );
        assert_eq!(px(a, 2, 2), px(b, 2, 2), "{an}");
        outs.push(a.clone());
    }
    if let [a, b] = &outs[..] {
        let (d, _) = max_codes(a, b);
        assert!(d <= 1.0, "adapters differ by {d:.3} codes");
    }
}

const ADJ: &str = r#"{
  "output": { "width": 48, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [
      { "id": "bg", "start": 0, "duration": "2",
        "generator": { "type": "linear_gradient", "start": ["0", "0"], "end": ["48", "32"],
          "start_color": ["0.9", "0.2", "0.1"], "end_color": ["0.1", "0.3", "0.9"] } }
    ]},
    { "name": "V2", "clips": [
      { "id": "dot", "start": 0, "duration": "2",
        "generator": { "type": "radial_gradient", "center": ["24", "16"], "radius": "10",
          "start_color": ["1", "1", "0.9"], "end_color": ["1", "1", "1", "0"] } }
    ]},
    { "name": "ADJ", "clips": [
      { "id": "adj", "adjustment": true, "start": "1/2", "duration": "1", "opacity": "3/4",
        "effects": [ { "type": "gaussian_blur", "sigma": 2, "repeat_edges": true },
                     { "type": "glow", "threshold": "0.5", "sigma": 2 } ] }
    ]}
  ]
}"#;

#[test]
fn adjustment_layers_apply_to_the_composite_below() {
    let tl = Timeline::from_json(ADJ).unwrap();
    let mut below = tl.clone();
    below.tracks.pop();
    let stack = EffectStack::new(
        "adj",
        &tl.tracks[2].clips[0].effects,
        RationalTime::new(1, 2),
    )
    .unwrap();
    let mut outs = Vec::new();
    for (an, gpu) in adapters() {
        let got = render(gpu, &tl, &[0, 11, 12, 30, 35, 36, 47]);
        let base = render(gpu, &below, &[0, 11, 12, 30, 35, 36, 47]);
        // Outside the clip the composite passes through untouched.
        for i in [0, 1, 5, 6] {
            assert_eq!(got[i], base[i], "{an}: frame {i}");
        }
        // Inside: the stack on the composite, mixed by the opacity.
        let comp_below = {
            let c = compile(&below).unwrap();
            let mut w = worker(gpu);
            let cancel = CancelToken::new();
            let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
            let mut cache = FrameCache::new(4);
            let t = RationalTime::from_frames(12, tl.output.fps);
            let f = c.graph.evaluate(c.output, t, &mut ctx, &mut cache).unwrap();
            let fx = stack.apply(&mut ctx, f, t).unwrap();
            let comp = compositor(&mut ctx).unwrap();
            let fx = comp.reframe(&mut ctx, &fx, PixelRect::full(W, H)).unwrap();
            ctx.flush();
            read(gpu, &fx).1
        };
        let want: Vec<[f32; 4]> = base[2]
            .iter()
            .zip(&comp_below)
            .map(|(b, f)| [0, 1, 2, 3].map(|k| q(b[k] + (f[k] - b[k]) * 0.75)))
            .collect();
        let (d, i) = max_codes(&got[2], &want);
        assert!(d <= 1.0, "{an}: adjustment {d:.3} codes at {i}");
        assert!(got[2] != base[2]);
        outs.push(got);
    }
    if let [a, b] = &outs[..] {
        for (fa, fb) in a.iter().zip(b) {
            let (d, _) = max_codes(fa, fb);
            assert!(d <= 1.0, "adapters differ by {d:.3} codes");
        }
    }
    // Keys: frames outside the adjustment clip only depend on what's below.
    let k = keys(&tl);
    let kb = keys(&below);
    let opacity = ops(
        ADJ,
        r#"[{"op": "set_param", "clip": "adj", "param": "opacity", "value": "1/2"}]"#,
    )
    .unwrap();
    let ko = keys(&opacity);
    for i in 0..k.len() {
        assert_eq!(k[i] != ko[i], (12..36).contains(&i), "frame {i}");
    }
    assert_ne!(k[0], kb[0]);
}

#[test]
fn adjustment_layers_follow_their_matte() {
    // The adjustment track takes the track above as an alpha matte: a
    // small solid square. Outside it the composite is untouched.
    let tl = ops(
        ADJ,
        r#"[{"op": "add_track", "kind": "video", "name": "M"},
            {"op": "add_clip", "track": "M", "id": "m", "generator": {"type": "solid", "color": ["1", "1", "1"]}, "duration": "2"},
            {"op": "set_param", "clip": "m", "param": "transform.scale", "value": "1/2"},
            {"op": "set_param", "track": "ADJ", "param": "matte", "value": {"mode": "alpha"}},
            {"op": "set_param", "clip": "adj", "param": "opacity", "value": "1"}]"#,
    )
    .unwrap();
    let mut no_matte = ADJ.replace(r#""opacity": "3/4","#, "");
    no_matte = Timeline::from_json(&no_matte)
        .map(|t| serde_json::to_string(&t).unwrap())
        .unwrap();
    let full = Timeline::from_json(&no_matte).unwrap();
    let mut below = full.clone();
    below.tracks.pop();
    for (an, gpu) in adapters() {
        let m = &render(gpu, &tl, &[20])[0];
        let f = &render(gpu, &full, &[20])[0];
        let b = &render(gpu, &below, &[20])[0];
        let px = |v: &Vec<[f32; 4]>, x: usize, y: usize| v[y * W as usize + x];
        // Square covers x 12..36, y 8..24.
        for (x, y) in [(2, 2), (45, 30), (5, 16), (40, 4)] {
            assert_eq!(
                px(m, x, y),
                px(b, x, y),
                "{an}: outside the matte at ({x}, {y})"
            );
        }
        for (x, y) in [(20, 12), (24, 16), (30, 20)] {
            let (d, _) = max_codes(&[px(m, x, y)], &[px(f, x, y)]);
            assert!(d <= 1.0, "{an}: inside the matte at ({x}, {y})");
        }
    }
    // Validation: adjustment clips need their own track and no transform.
    let bad = ADJ.replace(
        r#""adjustment": true,"#,
        r#""adjustment": true, "transform": {"rotation": "5"},"#,
    );
    let e = Timeline::from_json(&bad).unwrap_err().to_string();
    assert!(
        e.contains("transform is not supported on an adjustment layer"),
        "{e}"
    );
    let e = format!(
        "{:#}",
        ops(ADJ, r#"[{"op": "add_clip", "track": "ADJ", "generator": {"type": "solid", "color": ["1", "1", "1"]}, "start": "3/2", "duration": "1/2"}]"#)
            .unwrap_err()
    );
    assert!(e.contains("need a track of their own"), "{e}");
}
