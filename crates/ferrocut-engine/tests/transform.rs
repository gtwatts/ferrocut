//! Layer transform kernel on the GPU: exact integer translation, data-window
//! independence, rotation, minification coverage, pixel aspect, determinism;
//! plus keyframed opacity / transform cache keys. GPU tests skip (pass with a
//! note) when no adapter is available.

use std::sync::{Arc, OnceLock};

use ferrocut_core::{
    AdapterPreference, Animatable, CancelToken, ColorSpace, CpuFrame, Frame, FrameStorage,
    GpuContext, NodeHash, PixelRect, Rational, RationalTime, RenderCtx, RenderNode, WorkerState,
};
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::nodes::{ClipNode, TransformNode};
use ferrocut_engine::transform::{TransformAt, TransformSpec, plan};
use half::f16;

const W: u32 = 32;
const H: u32 = 16;

fn gpu() -> Option<&'static GpuContext> {
    static G: OnceLock<Option<GpuContext>> = OnceLock::new();
    G.get_or_init(|| match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            None
        }
    })
    .as_ref()
}

fn worker(gpu: &GpuContext) -> (WorkerState, Arc<Compositor>) {
    let comp = Arc::new(Compositor::new(gpu));
    let mut w = WorkerState::default();
    w.slot(compositor_slot(), || Ok(comp.clone())).unwrap();
    (w, comp)
}

/// Premultiplied test pattern value at display pixel (x, y), exact in f16.
fn pattern(x: i32, y: i32) -> [f32; 4] {
    let a = if (x + y) % 3 == 0 { 0.5 } else { 1.0 };
    [
        a * ((x * 7 + y) % 16) as f32 / 16.0,
        a * ((y * 5 + x * 3) % 16) as f32 / 16.0,
        a * ((x ^ y) % 8) as f32 / 8.0,
        a,
    ]
}

/// A frame whose storage covers `window` (pattern inside, outside implied transparent).
fn frame(window: PixelRect, par: Rational, f: impl Fn(i32, i32) -> [f32; 4]) -> Frame {
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
    c.pixel_aspect = par;
    Frame::from_cpu(&c)
}

/// Display-window pixels as f16 bit patterns (outside the data window = 0).
fn display_bits(gpu: &GpuContext, f: &Frame) -> Vec<[u16; 4]> {
    let c = f.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(img) = &c.storage else {
        unreachable!()
    };
    let dw = c.data_window;
    let mut out = vec![[0u16; 4]; (c.width * c.height) as usize];
    for y in 0..c.height as i32 {
        for x in 0..c.width as i32 {
            let (qx, qy) = (x - dw.x, y - dw.y);
            if qx >= 0 && qy >= 0 && (qx as u32) < dw.width && (qy as u32) < dw.height {
                let i = ((qy as u32 * dw.width + qx as u32) * 4) as usize;
                out[(y as u32 * c.width + x as u32) as usize] =
                    [0, 1, 2, 3].map(|k| img.pixels[i + k].to_bits());
            }
        }
    }
    out
}

fn to_f32(p: [u16; 4]) -> [f32; 4] {
    p.map(|b| f16::from_bits(b).to_f32())
}

fn run(gpu: &GpuContext, input: &Frame, t: TransformAt) -> (Frame, Vec<[u16; 4]>) {
    let (mut w, comp) = worker(gpu);
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let input = input.to_gpu(gpu);
    let k = plan(
        &t.affine(input.pixel_aspect.to_f64()),
        input.data_window,
        W,
        H,
    )
    .unwrap();
    let out = comp.transform(&mut ctx, &input, &k).unwrap();
    ctx.flush();
    let bits = display_bits(gpu, &out);
    (out, bits)
}

fn at(pos: [f64; 2], anchor: [f64; 2], scale: f64, rot: f64) -> TransformAt {
    TransformAt {
        position: pos,
        anchor,
        scale: [scale, scale],
        rotation_deg: rot,
    }
}

#[test]
fn integer_translation_is_an_exact_copy() {
    let Some(gpu) = gpu() else { return };
    let src = frame(PixelRect::full(W, H), Rational::ONE, pattern);
    let (out, bits) = run(gpu, &src, at([19.0, 6.0], [16.0, 8.0], 1.0, 0.0));
    // +3 in x, -2 in y; the window is the shifted image grown by the kernel radius.
    assert_eq!(out.data_window, PixelRect::new(1, 0, 31, 16));
    let want = display_bits(gpu, &src.to_gpu(gpu));
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            let (sx, sy) = (x - 3, y + 2);
            let expect = if (0..W as i32).contains(&sx) && (0..H as i32).contains(&sy) {
                want[(sy * W as i32 + sx) as usize]
            } else {
                [0; 4]
            };
            assert_eq!(bits[(y * W as i32 + x) as usize], expect, "({x},{y})");
        }
    }
}

#[test]
fn data_window_does_not_change_the_result() {
    let Some(gpu) = gpu() else { return };
    let win = PixelRect::new(5, 3, 17, 9);
    let inside = |x: i32, y: i32| {
        x >= win.x && y >= win.y && (x as i64) < win.right() && (y as i64) < win.bottom()
    };
    let partial = frame(win, Rational::ONE, pattern);
    let full = frame(PixelRect::full(W, H), Rational::ONE, |x, y| {
        if inside(x, y) {
            pattern(x, y)
        } else {
            [0.0; 4]
        }
    });
    let t = at([15.0, 9.5], [13.0, 7.0], 0.7, 30.0);
    let (_, a) = run(gpu, &partial, t);
    let (_, b) = run(gpu, &full, t);
    assert_eq!(a, b);
    assert!(a.iter().any(|p| p[3] != 0), "something is visible");
}

#[test]
fn rotation_by_90_degrees_matches_the_cpu() {
    let Some(gpu) = gpu() else { return };
    let src = frame(PixelRect::full(W, H), Rational::ONE, pattern);
    // Pivot on a pixel corner so centers land on centers: (x, y) -> (c + (cy - y) - 1, ...).
    let (cx, cy) = (16.0, 8.0);
    let (_, bits) = run(gpu, &src, at([cx, cy], [cx, cy], 1.0, 90.0));
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            // Inverse of a clockwise quarter turn about (16, 8) on pixel centers.
            let (sx, sy) = (y - 8 + 16, 16 - x + 8 - 1);
            let want = if (0..W as i32).contains(&sx) && (0..H as i32).contains(&sy) {
                pattern(sx, sy)
            } else {
                [0.0; 4]
            };
            let got = to_f32(bits[(y * W as i32 + x) as usize]);
            for k in 0..4 {
                assert!(
                    (got[k] - want[k]).abs() < 2e-3,
                    "({x},{y}) ch{k}: {got:?} vs {want:?}"
                );
            }
        }
    }
}

#[test]
fn minification_keeps_coverage_and_color() {
    let Some(gpu) = gpu() else { return };
    let c = [0.25, 0.5, 0.75, 1.0];
    let src = frame(PixelRect::full(W, H), Rational::ONE, |_, _| c);
    let (out, bits) = run(gpu, &src, at([16.0, 8.0], [16.0, 8.0], 0.5, 0.0));
    assert_eq!(out.data_window, PixelRect::new(6, 2, 20, 12));
    // Interior of the half-size image is the solid color; outside is empty;
    // total alpha equals the covered area (1/4 of the frame).
    let mut alpha = 0.0f64;
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            let p = to_f32(bits[(y * W as i32 + x) as usize]);
            alpha += p[3] as f64;
            if (10..22).contains(&x) && (6..10).contains(&y) {
                for k in 0..4 {
                    assert!((p[k] - c[k]).abs() < 2e-3, "({x},{y}) {p:?}");
                }
            }
            if !(6..26).contains(&x) || !(2..14).contains(&y) {
                assert_eq!(p, [0.0; 4]);
            }
        }
    }
    let want = (W * H) as f64 / 4.0;
    assert!(
        (alpha - want).abs() < 0.02 * want,
        "alpha {alpha} vs {want}"
    );
}

#[test]
fn pixel_aspect_is_respected() {
    let Some(gpu) = gpu() else { return };
    // PAR 2: a 2x2-pixel block is a 2:1 rectangle on screen (4 x 2 square
    // units). A quarter turn makes it 1:2 on screen: 1 pixel wide, 4 rows tall.
    // (Rotating raw pixels would give a 2x2-pixel block again.)
    let block = |x: i32, y: i32| {
        if (16..18).contains(&x) && (8..10).contains(&y) {
            [1.0; 4]
        } else {
            [0.0; 4]
        }
    };
    let src = frame(PixelRect::full(W, H), Rational::from_int(2), block);
    let (out, bits) = run(gpu, &src, at([16.0, 8.0], [16.0, 8.0], 1.0, 90.0));
    assert_eq!(out.pixel_aspect, Rational::from_int(2));
    let a = |x: i32, y: i32| to_f32(bits[(y * W as i32 + x) as usize])[3];
    for y in 9..11 {
        assert!(a(15, y) > 0.9, "core ({}, {y}) {}", 15, a(15, y));
    }
    assert!(
        a(15, 8) > 0.6 && a(15, 11) > 0.6,
        "{} {}",
        a(15, 8),
        a(15, 11)
    );
    for (x, y) in [(14, 9), (16, 9), (14, 10), (16, 10), (15, 6), (15, 13)] {
        assert!(a(x, y) < 0.1, "outside ({x}, {y}) {}", a(x, y));
    }
}

#[test]
fn transform_is_deterministic() {
    let Some(gpu) = gpu() else { return };
    let src = frame(PixelRect::new(2, 1, 27, 13), Rational::ONE, pattern);
    let t = at([15.3, 7.9], [14.0, 7.0], 1.37, -21.5);
    let (_, a) = run(gpu, &src, t);
    let (_, b) = run(gpu, &src, t);
    assert_eq!(a, b);
}

#[test]
fn keyframed_parameters_drive_frame_keys() {
    let secs = |n: i64, d: i64| RationalTime(Rational::new(n, d));
    // Constant opacity keeps its pre-keyframe hash.
    let clip = |opacity: Animatable| ClipNode {
        start: secs(2, 1),
        source_in: RationalTime::ZERO,
        duration: secs(2, 1),
        opacity,
        map: ferrocut_engine::retime::TimeMap::new(
            RationalTime::ZERO,
            &Animatable::constant(Rational::ONE),
            None,
        ),
        sampling: Default::default(),
        source_fps: None,
    };
    let c = clip(Animatable::constant(Rational::new(1, 2)));
    assert_eq!(
        c.content_hash_at(secs(3, 1)),
        NodeHash::of("clip.at", &[&Rational::new(1, 2).hash_bytes()])
    );
    // Keyed opacity (clip-local times): exact values at keys, hashes follow the value.
    let k: Animatable = serde_json::from_str(
        r#"{ "keyframes": [ { "t": "0", "v": "0", "interp": "ease_out" }, { "t": "1", "v": "1", "interp": "hold" } ] }"#,
    )
    .unwrap();
    let a = clip(k);
    assert_eq!(a.opacity_at(secs(2, 1)), 0.0);
    assert_eq!(a.opacity_at(secs(3, 1)), 1.0);
    assert_eq!(a.opacity_at(secs(7, 2)), 1.0);
    assert_eq!(a.content_hash_at(secs(3, 1)), a.content_hash_at(secs(7, 2)));
    assert_ne!(a.content_hash_at(secs(2, 1)), a.content_hash_at(secs(5, 2)));
    let mut prev = -1.0;
    for i in 0..=24 {
        let v = a.opacity_at(secs(48 + i, 24));
        assert!(v >= prev, "monotonic ease");
        prev = v;
    }
    // Transform: identity at rest, animated hash elsewhere; held pose reuses keys.
    let spec: TransformSpec = serde_json::from_str(
        r#"{ "rotation": { "keyframes": [ { "t": "0", "v": "0", "interp": "easy_ease" }, { "t": "1", "v": "45" } ] } }"#,
    )
    .unwrap();
    let node = TransformNode {
        start: secs(2, 1),
        spec,
        width: W,
        height: H,
        three_d: false,
        camera: None,
        blur: None,
        fps: ferrocut_core::Rational::from_int(24),
    };
    assert_eq!(
        node.content_hash_at(secs(2, 1)),
        NodeHash::of("transform.identity", &[])
    );
    assert_ne!(
        node.content_hash_at(secs(5, 2)),
        node.content_hash_at(secs(2, 1))
    );
    assert_eq!(
        node.content_hash_at(secs(3, 1)),
        node.content_hash_at(secs(4, 1))
    );
    assert_eq!(node.at(secs(3, 1)).rotation_deg, 45.0);
}
