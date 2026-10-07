//! GPU frame semantics: data windows, pixel aspect, the texture pool, device
//! requirements, and reframing for nodes that don't handle data windows.
//! Skips (passes with a note) when no GPU adapter is available.

use std::sync::{Arc, OnceLock};

use ferrocut_core::{
    AdapterPreference, CancelToken, ColorSpace, CpuFrame, ErrorKind, Frame, FrameStorage,
    GpuContext, GpuError, GpuRequirements, NodeError, NodeHash, PixelRect, Pull, Rational,
    RationalTime, RenderCtx, RenderNode, WorkerState,
};
use ferrocut_engine::compositor::{Compositor, ReadbackRing, compositor_slot};
use ferrocut_engine::graph::{FrameCache, Graph};
use half::f16;

const W: u32 = 8;
const H: u32 = 4;

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

/// Solid premultiplied color over `window` of a W x H display.
fn solid(window: PixelRect, rgba: [f32; 4]) -> Frame {
    let n = (window.width * window.height) as usize;
    let px: Vec<f16> = (0..n).flat_map(|_| rgba.map(f16::from_f32)).collect();
    let mut f = CpuFrame::new(window.width, window.height, ColorSpace::acescg(), px);
    f.width = W;
    f.height = H;
    f.data_window = window;
    Frame::from_cpu(&f)
}

/// Read back as display-window pixels (outside the data window = 0).
fn display_pixels(gpu: &GpuContext, f: &Frame) -> Vec<[f32; 4]> {
    let c = f.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(img) = &c.storage else {
        unreachable!()
    };
    let dw = c.data_window;
    let mut out = vec![[0.0; 4]; (c.width * c.height) as usize];
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

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 2e-3)
}

const LOWER_THIRD: PixelRect = PixelRect::new(2, 1, 4, 2);
const FG: [f32; 4] = [0.5, 0.25, 0.0, 0.5];
const BG: [f32; 4] = [0.2, 0.4, 0.6, 1.0];

fn inside(x: u32, y: u32) -> bool {
    (2..6).contains(&x) && (1..3).contains(&y)
}

#[test]
fn over_and_dissolve_respect_data_windows() {
    let Some(gpu) = gpu() else { return };
    let (mut w, comp) = worker(gpu);
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let fg = solid(LOWER_THIRD, FG).to_gpu(gpu);
    let bg = solid(PixelRect::full(W, H), BG).to_gpu(gpu);
    assert!(!fg.is_full_window() && bg.is_full_window());

    let over = comp.over(&mut ctx, &fg, &bg).unwrap();
    let diss = comp.dissolve(&mut ctx, &fg, &bg, 0.25).unwrap();
    let alone = comp.opacity(&mut ctx, &fg, 0.5).unwrap();
    ctx.flush();
    assert_eq!(over.data_window, PixelRect::full(W, H), "union of windows");
    assert_eq!(alone.data_window, LOWER_THIRD, "unary ops keep the window");

    let (o, d) = (display_pixels(gpu, &over), display_pixels(gpu, &diss));
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as usize;
            let (want_o, want_d) = if inside(x, y) {
                ([0.6, 0.45, 0.3, 1.0], [0.425, 0.2875, 0.15, 0.625])
            } else {
                (BG, BG.map(|v| v * 0.25))
            };
            assert!(
                close(o[i], want_o),
                "over ({x},{y}) {:?} vs {want_o:?}",
                o[i]
            );
            assert!(
                close(d[i], want_d),
                "dissolve ({x},{y}) {:?} vs {want_d:?}",
                d[i]
            );
        }
    }
}

#[test]
fn output_crops_overscan_and_blacks_out_uncovered_pixels() {
    let Some(gpu) = gpu() else { return };
    let (mut w, comp) = worker(gpu);
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let bg = solid(PixelRect::full(W, H), BG).to_gpu(gpu);
    let overscan = comp
        .reframe(&mut ctx, &bg, PixelRect::new(-3, -2, W + 6, H + 5))
        .unwrap();
    let fg = solid(LOWER_THIRD, [1.0, 1.0, 1.0, 1.0]).to_gpu(gpu);

    let mut outs: Vec<Vec<u8>> = Vec::new();
    let mut ring = ReadbackRing::new(gpu, W, H, 2);
    let mut sink = |rows: &[u8], stride: usize| {
        outs.push(
            (0..H as usize)
                .flat_map(|y| rows[y * stride..y * stride + W as usize * 4].to_vec())
                .collect(),
        );
        Ok(())
    };
    for f in [&bg, &overscan, &fg] {
        ring.push(&comp, &mut ctx, f, &mut sink).unwrap();
    }
    ring.drain(gpu, &mut sink).unwrap();
    assert_eq!(outs.len(), 3);
    assert_eq!(
        outs[0], outs[1],
        "overscan crops to the same display pixels"
    );
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let want: &[u8] = if inside(x, y) {
                &[255, 255, 255, 255]
            } else {
                &[0, 0, 0, 255]
            };
            assert_eq!(&outs[2][i..i + 4], want, "({x},{y})");
        }
    }
}

#[test]
fn pixel_aspect_mismatch_is_a_permanent_error() {
    let Some(gpu) = gpu() else { return };
    let (mut w, comp) = worker(gpu);
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let a = solid(PixelRect::full(W, H), BG).to_gpu(gpu);
    let mut b = a.clone();
    b.pixel_aspect = Rational::new(4, 3);
    let e = comp.over(&mut ctx, &a, &b).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent);
    let anamorphic = comp.opacity(&mut ctx, &b, 1.0).unwrap();
    assert_eq!(
        anamorphic.pixel_aspect,
        Rational::new(4, 3),
        "PAR propagates"
    );
}

#[test]
fn pool_reuses_textures_on_the_same_thread() {
    let Some(gpu) = gpu() else { return };
    let before = gpu.pool_stats();
    let f = Frame::new_gpu(gpu, 33, 17, ColorSpace::acescg());
    assert!(f.gpu().unwrap().is_pooled());
    drop(f);
    let g = Frame::new_gpu(gpu, 33, 17, ColorSpace::acescg());
    let after = gpu.pool_stats();
    assert!(after.reused > before.reused, "{before:?} -> {after:?}");
    // Another thread doesn't get this thread's idle textures (see pool docs).
    drop(g);
    let before = gpu.pool_stats();
    std::thread::scope(|s| {
        s.spawn(|| drop(Frame::new_gpu(gpu, 33, 17, ColorSpace::acescg())));
    });
    assert_eq!(gpu.pool_stats().reused, before.reused);
}

#[test]
fn device_requirements_union_and_fallback() {
    let Some(gpu) = gpu() else { return };
    let adapter = gpu.adapter.features();
    let opt = GpuRequirements::optional(wgpu::Features::FLOAT32_FILTERABLE);
    let req = GpuRequirements::required(wgpu::Features::empty()).with_limits(wgpu::Limits {
        max_storage_textures_per_shader_stage: 4,
        ..Default::default()
    });
    let u = opt.union(&req);
    assert_eq!(u.optional_features, wgpu::Features::FLOAT32_FILTERABLE);
    assert_eq!(
        u.limits
            .as_ref()
            .unwrap()
            .max_storage_textures_per_shader_stage,
        4.max(wgpu::Limits::default().max_storage_textures_per_shader_stage)
    );

    // Optional: granted iff the adapter has it; never an error.
    let g = GpuContext::with_requirements(AdapterPreference::default(), &opt).unwrap();
    assert_eq!(
        g.has_features(wgpu::Features::FLOAT32_FILTERABLE),
        adapter.contains(wgpu::Features::FLOAT32_FILTERABLE)
    );

    // Required but unsupported: a clear error instead of a broken device.
    if let Some(missing) = (wgpu::Features::all() - adapter).iter().next() {
        match GpuContext::with_requirements(
            AdapterPreference::default(),
            &GpuRequirements::required(missing),
        ) {
            Err(GpuError::MissingFeatures { missing: m, .. }) => assert_eq!(m, missing),
            Err(e) => panic!("unexpected error {e}"),
            Ok(_) => panic!("device created without required feature {missing:?}"),
        }
    }
}

/// A CPU "source" producing the lower third.
struct LowerThird;
impl RenderNode for LowerThird {
    fn kind(&self) -> &'static str {
        "test.lower_third"
    }
    fn content_hash(&self) -> NodeHash {
        NodeHash::of("test.lower_third", &[])
    }
    fn pulls(&self, _t: RationalTime) -> Vec<Pull> {
        vec![]
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn render(
        &self,
        _: &mut RenderCtx<'_>,
        _: RationalTime,
        _: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        Ok(Arc::new(solid(LOWER_THIRD, FG)))
    }
}

/// A node written before data windows existed (like most third-party nodes).
struct FullWindowOnly(wgpu::Features);
impl RenderNode for FullWindowOnly {
    fn kind(&self) -> &'static str {
        "test.full_window_only"
    }
    fn content_hash(&self) -> NodeHash {
        NodeHash::of("test.full_window_only", &[])
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull { input: 0, time: t }]
    }
    fn gpu_requirements(&self) -> GpuRequirements {
        GpuRequirements::optional(self.0)
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        _: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        if !inputs[0].is_full_window() {
            return Err(NodeError::new("got a partial data window"));
        }
        if ctx.worker.has_pending_gpu_work() {
            return Err(NodeError::new(
                "batched GPU work wasn't flushed before a non-batching node",
            ));
        }
        Ok(inputs[0].clone())
    }
}

#[test]
fn graph_reframes_and_flushes_for_legacy_nodes() {
    let Some(gpu) = gpu() else { return };
    let mut g = Graph::new();
    let src = g.add(Arc::new(LowerThird), vec![]);
    let out = g.add(
        Arc::new(FullWindowOnly(wgpu::Features::FLOAT32_FILTERABLE)),
        vec![src],
    );
    assert_eq!(
        g.gpu_requirements().optional_features,
        wgpu::Features::FLOAT32_FILTERABLE
    );
    let (mut w, _) = worker(gpu);
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let f = g
        .evaluate(out, RationalTime::ZERO, &mut ctx, &mut FrameCache::new(4))
        .unwrap();
    assert!(f.is_full_window());
    let px = display_pixels(gpu, &f);
    assert!(close(px[(W + 2) as usize], FG) && close(px[0], [0.0; 4]));
}
