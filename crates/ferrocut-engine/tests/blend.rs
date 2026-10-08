//! Blend modes and track mattes: GPU kernels against the CPU reference on
//! the default adapter and on lavapipe (`AdapterPreference::Cpu`), adapters
//! against each other (within one 10-bit code value), and cache keys (normal
//! is hash-identical to plain over). GPU tests skip with a note when an
//! adapter is unavailable.

use std::sync::{Arc, OnceLock};

use ferrocut_core::{
    AdapterPreference, CancelToken, ColorSpace, CpuFrame, Frame, FrameStorage, GpuContext,
    PixelRect, Rational, RationalTime, RenderCtx, WorkerState,
};
use ferrocut_engine::Timeline;
use ferrocut_engine::blend::{BlendMode, MatteMode, blend_px, matte_px};
use ferrocut_engine::compile::compile_with;
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::nodes::SourceNode;
use half::f16;

const W: u32 = 24;
const H: u32 = 20;

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
        Ok(g) => {
            eprintln!("{label}: {}", g.adapter.get_info().name);
            Some(g)
        }
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

/// Background: opaque and translucent, some HDR (> 1) values.
fn bg_px(x: i32, y: i32) -> [f32; 4] {
    let a = [1.0, 0.75, 0.5, 0.0][((x / 3 + y) % 4) as usize];
    let c = |k: i32| q(a * ((x * k + y * 3) % 13) as f32 / 9.0);
    [c(5), c(7), c(11), a]
}

/// Foreground: every channel level and alpha, including fully transparent.
fn fg_px(x: i32, y: i32) -> [f32; 4] {
    let a = [1.0, 0.5, 0.25, 0.0, 0.875][((x + y / 2) % 5) as usize];
    let c = |k: i32| q(a * ((x * 3 + y * k) % 11) as f32 / 10.0);
    [c(2), c(5), c(9), a]
}

fn frame(window: PixelRect, f: impl Fn(i32, i32) -> [f32; 4]) -> Frame {
    let mut px = Vec::new();
    for y in 0..window.height as i32 {
        for x in 0..window.width as i32 {
            px.extend(f(x + window.x, y + window.y).map(f16::from_f32));
        }
    }
    let mut c = CpuFrame::new(window.width, window.height, ColorSpace::acescg(), px);
    c.width = W;
    c.height = H;
    c.data_window = window;
    Frame::from_cpu(&c)
}

fn display(gpu: &GpuContext, f: &Frame) -> Vec<[f32; 4]> {
    let c = f.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(img) = &c.storage else {
        unreachable!()
    };
    let dw = c.data_window;
    let mut out = vec![[0.0; 4]; (W * H) as usize];
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            let (qx, qy) = (x - dw.x, y - dw.y);
            if qx >= 0 && qy >= 0 && (qx as u32) < dw.width && (qy as u32) < dw.height {
                let i = ((qy as u32 * dw.width + qx as u32) * 4) as usize;
                out[(y as u32 * W + x as u32) as usize] =
                    [0, 1, 2, 3].map(|k| img.pixels[i + k].to_f32());
            }
        }
    }
    out
}

/// Inputs in display space (outside a data window = transparent).
fn inputs() -> (Frame, Frame, Vec<[f32; 4]>, Vec<[f32; 4]>) {
    let fg_win = PixelRect::new(5, 3, 14, 15);
    let bg_win = PixelRect::new(0, 0, 20, H);
    let inside = |w: PixelRect, x: i32, y: i32| {
        x >= w.x && y >= w.y && x < w.x + w.width as i32 && y < w.y + w.height as i32
    };
    let mut fg = Vec::new();
    let mut bg = Vec::new();
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            fg.push(if inside(fg_win, x, y) {
                fg_px(x, y)
            } else {
                [0.0; 4]
            });
            bg.push(if inside(bg_win, x, y) {
                bg_px(x, y)
            } else {
                [0.0; 4]
            });
        }
    }
    (frame(fg_win, fg_px), frame(bg_win, bg_px), fg, bg)
}

enum Op {
    Blend(BlendMode),
    Matte(MatteMode),
}

fn run(gpu: &GpuContext, op: &Op) -> Vec<[f32; 4]> {
    let comp = Arc::new(Compositor::new(gpu));
    let mut w = WorkerState::default();
    w.slot(compositor_slot(), || Ok(comp.clone())).unwrap();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let (fg, bg, _, _) = inputs();
    let (fg, bg) = (fg.to_gpu(gpu), bg.to_gpu(gpu));
    let out = match op {
        Op::Blend(m) => comp.blend(&mut ctx, &fg, &bg, *m).unwrap(),
        Op::Matte(m) => comp.matte(&mut ctx, &fg, &bg, *m).unwrap(),
    };
    ctx.flush();
    display(gpu, &out)
}

/// Difference in 10-bit code values (relative above 1.0 for HDR values).
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

const MATTES: [MatteMode; 4] = [
    MatteMode::Alpha,
    MatteMode::AlphaInverted,
    MatteMode::Luma,
    MatteMode::LumaInverted,
];

fn all_ops() -> Vec<(String, Op)> {
    let mut v: Vec<(String, Op)> = BlendMode::ALL
        .iter()
        .map(|m| (m.name().to_string(), Op::Blend(*m)))
        .collect();
    v.extend(
        MATTES
            .iter()
            .map(|m| (format!("matte {}", m.name()), Op::Matte(*m))),
    );
    v
}

#[test]
fn kernels_match_the_cpu_reference_on_every_adapter() {
    let (_, _, fg, bg) = inputs();
    let ads = adapters();
    let mut worst = (0.0f32, 0.0f32);
    for (name, op) in all_ops() {
        let want: Vec<[f32; 4]> = fg
            .iter()
            .zip(&bg)
            .map(|(f, b)| match &op {
                Op::Blend(m) => blend_px(*m, *f, *b).map(q),
                Op::Matte(m) => matte_px(*m, *f, *b).map(q),
            })
            .collect();
        let mut outs = Vec::new();
        for (an, gpu) in &ads {
            let got = run(gpu, &op);
            let (d, i) = max_codes(&got, &want);
            assert!(
                d <= 1.0,
                "{name} on {an}: {d:.3} codes at ({}, {}): got {:?} want {:?} (fg {:?} bg {:?})",
                i as u32 % W,
                i as u32 / W,
                got[i],
                want[i],
                fg[i],
                bg[i]
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

#[test]
fn normal_blend_is_bit_identical_to_over() {
    for (an, gpu) in adapters() {
        let comp = Arc::new(Compositor::new(gpu));
        let mut w = WorkerState::default();
        w.slot(compositor_slot(), || Ok(comp.clone())).unwrap();
        let cancel = CancelToken::new();
        let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
        let (fg, bg, _, _) = inputs();
        let (fg, bg) = (fg.to_gpu(gpu), bg.to_gpu(gpu));
        let a = comp.blend(&mut ctx, &fg, &bg, BlendMode::Normal).unwrap();
        let b = comp.over(&mut ctx, &fg, &bg).unwrap();
        ctx.flush();
        assert_eq!(a.data_window, b.data_window, "{an}");
        let (a, b) = (display(gpu, &a), display(gpu, &b));
        assert!(
            a.iter()
                .zip(&b)
                .all(|(p, r)| p.map(f32::to_bits) == r.map(f32::to_bits)),
            "{an}"
        );
    }
}

// ---------------------------------------------------------------- timeline

const BASE: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [ { "id": "a", "source": "a.mov", "start": 0, "duration": "3" } ] },
    { "name": "V2", "clips": [
      { "id": "t", "source": "t.mov", "start": "1/2", "duration": "1" },
      { "id": "u", "source": "u.mov", "start": "2", "duration": "1/2" }
    ]}
  ]
}"#;

fn run_ops(ops: &str) -> anyhow::Result<Timeline> {
    let ops = parse_ops(ops)?;
    let mut media = MediaLengths::new(".", |_| Some(RationalTime::new(10, 1)));
    apply(&Timeline::from_json(BASE).unwrap(), &ops, &mut media).map(|(tl, _)| tl)
}

fn keys(tl: &Timeline) -> Vec<ferrocut_core::FrameKey> {
    let c = compile_with(tl, |p, w, h| {
        Ok(SourceNode {
            path: p.clone(),
            file_hash: *blake3::hash(p.to_string_lossy().as_bytes()).as_bytes(),
            width: w,
            height: h,
            fps: Some(Rational::from_int(24)),
        })
    })
    .unwrap();
    (0..tl.frame_count())
        .map(|i| {
            c.graph
                .frame_key(c.output, RationalTime::from_frames(i, tl.output.fps))
        })
        .collect()
}

#[test]
fn blend_mode_changes_only_the_clips_frames() {
    let base = keys(&run_ops("[]").unwrap());
    let normal =
        run_ops(r#"[{"op": "set_param", "clip": "t", "param": "blend_mode", "value": "normal"}]"#)
            .unwrap();
    assert!(
        !serde_json::to_string(&normal)
            .unwrap()
            .contains("blend_mode")
    );
    assert_eq!(keys(&normal), base);
    let tl = run_ops(
        r#"[{"op": "set_param", "clip": "t", "param": "blend_mode", "value": "multiply"}]"#,
    )
    .unwrap();
    let k = keys(&tl);
    for (i, (a, b)) in k.iter().zip(&base).enumerate() {
        // t covers frames 12..36.
        assert_eq!(a != b, (12..36).contains(&i), "frame {i}");
    }
    let screen = keys(
        &run_ops(r#"[{"op": "set_param", "clip": "t", "param": "blend_mode", "value": "screen"}]"#)
            .unwrap(),
    );
    assert_ne!(screen[20], k[20]);
    assert_eq!(screen[40], k[40]);
}

#[test]
fn blend_mode_and_matte_validation() {
    let err = |ops: &str| format!("{:#}", run_ops(ops).unwrap_err());
    assert!(
        err(r#"[{"op": "set_param", "clip": "t", "param": "blend_mode", "value": "vivid"}]"#)
            .contains("blend_mode")
    );
    assert!(
        err(
            r#"[{"op": "set_param", "track": "V2", "param": "matte", "value": {"mode": "alpha"}}]"#
        )
        .contains("needs a video track above")
    );
    let tl = run_ops(
        r#"[{"op": "set_param", "track": "V1", "param": "matte", "value": {"mode": "luma_inverted"}}]"#,
    )
    .unwrap();
    assert!(
        serde_json::to_string(&tl)
            .unwrap()
            .contains("luma_inverted")
    );
    assert!(
        err(r#"[{"op": "add_track", "kind": "video", "name": "V3"},
                {"op": "set_param", "track": "V1", "param": "matte", "value": {"mode": "alpha"}},
                {"op": "set_param", "track": "V2", "param": "matte", "value": {"mode": "alpha"}}]"#)
        .contains("cannot have a matte itself")
    );
    assert!(
        err(r#"[{"op": "add_track", "kind": "audio", "name": "A1"},
                {"op": "set_param", "track": "A1", "param": "matte", "value": {"mode": "alpha"}}]"#)
        .contains("video tracks only")
    );
    assert!(
        err(r#"[{"op": "set_param", "track": "V1", "param": "matte", "value": {"mode": "alpha", "source": "mask"}}]"#)
            .contains("matte")
    );
    // A matte changes every frame's key (V2 becomes the matte, not a layer).
    assert_ne!(keys(&tl)[0], keys(&run_ops("[]").unwrap())[0]);
}
