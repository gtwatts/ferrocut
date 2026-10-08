//! Downscales beyond the kernel cap (8x) take the 2x box pre-pass: stripes
//! that used to alias are filtered, constant color and coverage are kept, odd
//! data-window origins work, and the result is deterministic. GPU tests skip
//! (pass with a note) when no adapter is available.

use std::sync::{Arc, OnceLock};

use ferrocut_core::{
    AdapterPreference, CancelToken, ColorSpace, CpuFrame, Frame, FrameStorage, GpuContext,
    PixelRect, Rational, RenderCtx, WorkerState,
};
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::transform::{KernelSetup, MAX_FILTER_SCALE, TransformAt, plan};
use half::f16;

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

fn frame(w: u32, h: u32, window: PixelRect, f: impl Fn(i32, i32) -> [f32; 4]) -> Frame {
    let mut px = Vec::new();
    for y in 0..window.height as i32 {
        for x in 0..window.width as i32 {
            px.extend(f(x + window.x, y + window.y).map(f16::from_f32));
        }
    }
    let mut c = CpuFrame::new(window.width, window.height, ColorSpace::acescg(), px);
    c.width = w;
    c.height = h;
    c.data_window = window;
    Frame::from_cpu(&c)
}

/// Display-window pixels as f32 (outside the data window = 0).
fn display(gpu: &GpuContext, f: &Frame) -> Vec<[f32; 4]> {
    let c = f.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(img) = &c.storage else {
        unreachable!()
    };
    let dw = c.data_window;
    let mut out = vec![[0f32; 4]; (c.width * c.height) as usize];
    for y in 0..c.height as i32 {
        for x in 0..c.width as i32 {
            let (qx, qy) = (x - dw.x, y - dw.y);
            if qx >= 0 && qy >= 0 && (qx as u32) < dw.width && (qy as u32) < dw.height {
                let i = ((qy as u32 * dw.width + qx as u32) * 4) as usize;
                out[(y as u32 * c.width + x as u32) as usize] =
                    [0, 1, 2, 3].map(|k| img.pixels[i + k].to_f32());
            }
        }
    }
    out
}

fn run(gpu: &GpuContext, input: &Frame, k: &KernelSetup) -> Vec<[f32; 4]> {
    let comp = Arc::new(Compositor::new(gpu));
    let mut w = WorkerState::default();
    w.slot(compositor_slot(), || Ok(comp.clone())).unwrap();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let input = input.to_gpu(gpu);
    let out = comp.transform(&mut ctx, &input, k).unwrap();
    ctx.flush();
    display(gpu, &out)
}

fn setup(input: &Frame, scale: [f64; 2]) -> KernelSetup {
    let t = TransformAt {
        scale,
        ..TransformAt::identity(input.width, input.height)
    };
    plan(
        &t.affine(Rational::ONE.to_f64()),
        input.data_window,
        input.width,
        input.height,
    )
    .unwrap()
}

/// What the kernel alone did before the pre-pass existed: scale capped at 8.
fn legacy(k: &KernelSetup) -> KernelSetup {
    let mut l = *k;
    l.mip = [0, 0];
    l.filter_scale = [MAX_FILTER_SCALE, MAX_FILTER_SCALE];
    l.radius = [16, 16];
    l
}

#[test]
fn stripes_past_8x_are_filtered_not_aliased() {
    let Some(gpu) = gpu() else { return };
    // Vertical stripes with a 20-pixel period, shrunk 16x horizontally: far
    // above the output's Nyquist, so the ideal result is flat 50 % grey.
    let (w, h) = (1280, 32);
    let src = frame(w, h, PixelRect::full(w, h), |x, _| {
        let v = if x % 20 < 10 { 1.0 } else { 0.0 };
        [v, v, v, 1.0]
    });
    let k = setup(&src, [1.0 / 16.0, 1.0]);
    assert_eq!(k.mip, [1, 0]);
    let dev = |px: &[[f32; 4]]| {
        // Interior of the 80-pixel-wide result (columns 600..680), mid rows.
        let mut worst = 0f32;
        for y in 8..24 {
            for x in 603..677 {
                let p = px[(y * w + x) as usize];
                assert!((p[3] - 1.0).abs() < 1e-3, "alpha at ({x},{y}) {p:?}");
                worst = worst.max((p[0] - 0.5).abs());
            }
        }
        worst
    };
    let new = dev(&run(gpu, &src, &k));
    let old = dev(&run(gpu, &src, &legacy(&k)));
    eprintln!("max deviation from grey: kernel only {old:.4}, with box pre-pass {new:.4}");
    assert!(old > 0.05, "the capped kernel aliases this pattern ({old})");
    assert!(
        new < 0.03 && new < old / 4.0,
        "pre-pass {new} vs kernel only {old}"
    );
}

#[test]
fn box_pre_pass_keeps_color_coverage_and_is_deterministic() {
    let Some(gpu) = gpu() else { return };
    let (w, h) = (640, 320);
    let c = [0.25, 0.5, 0.125, 0.5];
    // Odd data-window origin: the box levels must align to the even grid.
    let window = PixelRect::new(3, 5, 601, 307);
    let src = frame(w, h, window, |_, _| c);
    let k = setup(&src, [1.0 / 20.0, 1.0 / 10.0]);
    assert_eq!(k.mip, [2, 1]);
    let a = run(gpu, &src, &k);
    let b = run(gpu, &src, &k);
    assert!(a == b, "deterministic");
    let alpha: f64 = a.iter().map(|p| p[3] as f64).sum();
    let want = (window.width * window.height) as f64 * c[3] as f64 / 200.0;
    assert!(
        (alpha - want).abs() < 0.03 * want,
        "alpha {alpha} vs {want}"
    );
    // The center of the shrunken image is the solid color.
    let p = a[(160 * w + 320) as usize];
    for i in 0..4 {
        assert!((p[i] - c[i]).abs() < 2e-3, "{p:?}");
    }
}
