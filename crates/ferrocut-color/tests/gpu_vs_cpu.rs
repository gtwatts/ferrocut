//! GPU (OCIO GLSL -> naga -> WGSL -> wgpu) vs OCIO's CPU processor.
//!
//! These tests need a GPU adapter (any wgpu backend; lavapipe works too).
//! Set FERROCUT_ADAPTER=<name substring> to pick one. Run with `--nocapture`
//! to see the error statistics.
#![cfg(not(ferrocut_no_ocio))]

use std::sync::{Arc, OnceLock};

use ferrocut_color::{Config, GpuShaderOptions, OcioTransformNode};
use ferrocut_core::{AdapterPreference, CancelToken, ColorSpace, ErrorKind, CpuImage, Frame, FrameStorage, GpuContext, GpuRequirements, PixelRect, Rational, RenderCtx, RenderNode, RationalTime, WorkerState};
use half::f16;

fn gpu() -> &'static GpuContext {
    static G: OnceLock<GpuContext> = OnceLock::new();
    G.get_or_init(|| {
        let g = GpuContext::with_requirements(AdapterPreference::default(), &GpuRequirements::optional(wgpu::Features::FLOAT32_FILTERABLE)).expect("a wgpu adapter is required for these tests");
        eprintln!("[gpu] {} | FLOAT32_FILTERABLE={}", g.describe(), g.device.features().contains(wgpu::Features::FLOAT32_FILTERABLE));
        g
    })
}

const W: usize = 67; // deliberately not a multiple of the 8x8 workgroup
const H: usize = 45;

/// Premultiplied ACEScg test pattern, quantized to f16 like a real working frame:
/// hue sweep across x, exposure ramp (incl. small negatives and >1 highlights)
/// down y, alpha cycling through 1, .75, .5, .25, 0.
fn test_pattern() -> Vec<f16> {
    let mut px = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            let h = x as f32 / W as f32 * 6.0;
            let e = -0.05 + 4.05 * (y as f32 / (H - 1) as f32).powf(2.0);
            let (r, g, b) = (
                (1.0 - (h - 0.0).abs().min((h - 6.0).abs()).min(2.0) / 2.0).max(0.05),
                (1.0 - (h - 2.0).abs().min(2.0) / 2.0).max(0.05),
                (1.0 - (h - 4.0).abs().min(2.0) / 2.0).max(0.05),
            );
            let a = [1.0, 0.75, 0.5, 0.25, 0.0][(x + y) % 5];
            for v in [r * e * a, g * e * a, b * e * a, a] {
                px.push(f16::from_f32(v));
            }
        }
    }
    px
}

fn cpu_frame(px: Vec<f16>, cs: &str) -> Frame {
    Frame {
        width: W as u32,
        height: H as u32,
        data_window: PixelRect::full(W as u32, H as u32),
        pixel_aspect: Rational::ONE,
        color_space: ColorSpace::new(cs),
        alpha: Default::default(),
        storage: FrameStorage::Cpu(Arc::new(CpuImage { pixels: px })),
    }
}

fn cpu_pixels(f: &Frame) -> Vec<f32> {
    let c = f.to_cpu(gpu()).expect("readback");
    match &c.storage {
        FrameStorage::Cpu(img) => img.pixels.iter().map(|v| v.to_f32()).collect(),
        FrameStorage::Gpu(_) => unreachable!(),
    }
}

struct Stats {
    max_abs: f32,
    mean_abs: f32,
    /// max |gpu-cpu| / max(1, |cpu|): absolute below 1.0, relative above (f16 precision is relative).
    max_rel: f32,
    worst_idx: usize,
}

fn compare(gpu_px: &[f32], cpu_px: &[f32]) -> Stats {
    assert_eq!(gpu_px.len(), cpu_px.len());
    let (mut max_abs, mut max_rel, mut sum, mut worst_idx) = (0f32, 0f32, 0f64, 0);
    for (i, (g, c)) in gpu_px.iter().zip(cpu_px).enumerate() {
        assert!(g.is_finite(), "non-finite GPU value at {i}: {g}");
        let d = (g - c).abs();
        sum += d as f64;
        max_rel = max_rel.max(d / c.abs().max(1.0));
        if d > max_abs {
            max_abs = d;
            worst_idx = i;
        }
    }
    Stats { max_abs, mean_abs: (sum / gpu_px.len() as f64) as f32, max_rel, worst_idx }
}

/// Run `node` on GPU and CPU over the test pattern, check alpha handling, return stats.
fn gpu_vs_cpu(node: &OcioTransformNode, src: Vec<f16>) -> Stats {
    let input = cpu_frame(src.clone(), node.src_space().name());
    let out = node.apply(gpu(), &input).expect("gpu render");
    assert_eq!(out.color_space, *node.out_space());
    let g = cpu_pixels(&out);

    let mut c: Vec<f32> = src.iter().map(|v| v.to_f32()).collect();
    node.apply_cpu_premultiplied(&mut c, W, H).unwrap();
    // The GPU result is stored as f16 (working format); quantize the reference the same way.
    let c: Vec<f32> = c.iter().map(|&v| f16::from_f32(v).to_f32()).collect();

    for (i, (gp, sp)) in g.as_chunks::<4>().0.iter().zip(src.as_chunks::<4>().0.iter()).enumerate() {
        assert_eq!(gp[3], sp[3].to_f32(), "alpha must pass through unchanged (pixel {i})");
        if sp[3].to_f32() == 0.0 {
            assert_eq!(&gp[..3], &[0.0, 0.0, 0.0], "alpha=0 pixel must stay black when premultiplied (pixel {i})");
        }
    }
    let s = compare(&g, &c);
    eprintln!(
        "[{}] max |gpu-cpu| = {:.3e}, max rel = {:.3e}, mean = {:.3e} (worst at px {} ch {}: gpu {} cpu {}); LUT textures: {}",
        node.out_space().name(),
        s.max_abs,
        s.max_rel,
        s.mean_abs,
        s.worst_idx / 4,
        s.worst_idx % 4,
        g[s.worst_idx],
        c[s.worst_idx],
        node.shader().textures.len()
    );
    s
}

#[test]
fn translates_builtin_aces_display_view_to_wgsl() {
    let cfg = Config::builtin_default().unwrap();
    let info = cfg.info().unwrap();
    eprintln!("OCIO {} config {} -> {} / {}", ferrocut_color::ocio_version(), info.name, info.default_display, info.default_view);
    let node = OcioTransformNode::display_view(&cfg, "ACEScg", &info.default_display, &info.default_view).unwrap();
    let t = node.translated();
    assert!(t.wgsl.contains("@compute"), "WGSL entry point");
    assert!(t.wgsl.contains("textureSampleLevel"), "LUT lookups survived translation");
    assert!(!node.shader().textures.is_empty(), "ACES 2.0 output transform uses LUT textures");
    eprintln!("GLSL {} bytes -> WGSL {} bytes", t.glsl.len(), t.wgsl.len());
}

#[test]
fn gpu_matches_cpu_acescg_to_srgb_display() {
    let cfg = Config::builtin_default().unwrap();
    let info = cfg.info().unwrap();
    let node = OcioTransformNode::display_view(&cfg, "ACEScg", &info.default_display, &info.default_view).unwrap();
    let s = gpu_vs_cpu(&node, test_pattern());
    // Display-referred output in [0,1]; f16 storage alone is ~5e-4 at 1.0.
    assert!(s.max_abs < 2e-3, "max abs error {} too large", s.max_abs);
    assert!(s.max_rel < 2e-3, "max error {} too large", s.max_rel);
}

#[test]
fn gpu_matches_cpu_acescg_to_srgb_texture_colorspace() {
    let cfg = Config::builtin_default().unwrap();
    let node = OcioTransformNode::colorspace(&cfg, "ACEScg", "sRGB - Texture").unwrap();
    let mut src = test_pattern();
    for v in src.iter_mut() {
        *v = f16::from_f32(v.to_f32().min(1.0)); // keep the encoded output in a sane range
    }
    let s = gpu_vs_cpu(&node, src);
    assert!(s.max_rel < 2e-3, "max error {} too large", s.max_rel);
}

#[test]
fn gpu_matches_cpu_user_lut3d_tetrahedral() {
    // A 17^3 "look": mild saturation + gamma, the kind of LUT an agent might hand us.
    let n = 17u32;
    let mut rgb = Vec::with_capacity((n * n * n * 3) as usize);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let (r, g, b) = (r as f32 / (n - 1) as f32, g as f32 / (n - 1) as f32, b as f32 / (n - 1) as f32);
                let l = 0.2722 * r + 0.6741 * g + 0.0537 * b;
                let s = |c: f32| (l + 1.3 * (c - l)).clamp(0.0, 1.0).powf(0.85);
                rgb.extend([s(r), s(g), s(b)]);
            }
        }
    }
    let cfg = Config::builtin_default().unwrap();
    let p = cfg.lut3d_processor(n, &rgb, true).unwrap();
    let node = OcioTransformNode::new(p, ColorSpace::new("ACEScg"), ColorSpace::new("ACEScg"), GpuShaderOptions::default()).unwrap();
    assert!(node.shader().textures.iter().any(|t| t.dimensions == 3), "expected a 3D LUT texture");
    let mut src = test_pattern();
    for v in src.iter_mut() {
        *v = f16::from_f32(v.to_f32().clamp(0.0, 1.0));
    }
    let s = gpu_vs_cpu(&node, src);
    assert!(s.max_rel < 2e-3, "max error {} too large", s.max_rel);
}

#[test]
fn render_node_contract() {
    let cfg = Config::builtin_default().unwrap();
    let info = cfg.info().unwrap();
    let a = OcioTransformNode::display_view(&cfg, "ACEScg", &info.default_display, &info.default_view).unwrap();
    let b = OcioTransformNode::display_view(&cfg, "ACEScg", &info.default_display, &info.default_view).unwrap();
    let c = OcioTransformNode::colorspace(&cfg, "ACEScg", "sRGB - Texture").unwrap();
    assert_eq!(a.content_hash(), b.content_hash(), "same transform => same cache key");
    assert_ne!(a.content_hash(), c.content_hash(), "different transform => different cache key");
    let t = RationalTime::new(1001, 24000);
    assert_eq!(a.pulls(t).len(), 1);
    assert_eq!(a.pulls(t)[0].time, t);

    // Through the trait, twice: deterministic bit-identical output on one device.
    let input = Arc::new(cpu_frame(test_pattern(), "ACEScg"));
    let mut worker = WorkerState::default();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu(), &mut worker, &cancel, None);
    let o1 = a.render(&mut ctx, t, std::slice::from_ref(&input)).unwrap();
    let o2 = b.render(&mut ctx, t, std::slice::from_ref(&input)).unwrap();
    let (p1, p2) = (cpu_pixels(&o1), cpu_pixels(&o2));
    assert!(p1.iter().zip(&p2).all(|(x, y)| x.to_bits() == y.to_bits()), "two renders must be bit-identical");

    // Wrong input color space is a clean node error.
    let wrong = Arc::new(cpu_frame(test_pattern(), "Linear Rec.709 (sRGB)"));
    let err = a.render(&mut ctx, t, &[wrong]).expect_err("must reject mismatched color space");
    eprintln!("mismatch error: {err}");
}

#[test]
fn frame_geometry_is_preserved_and_partial_windows_rejected() {
    let cfg = Config::builtin_default().unwrap();
    let node = OcioTransformNode::colorspace(&cfg, "ACEScg", "sRGB - Texture").unwrap();
    // Anamorphic input: the output keeps its pixel aspect.
    let mut f = cpu_frame(test_pattern(), "ACEScg");
    f.pixel_aspect = Rational::new(2, 1);
    let out = node.apply(gpu(), &f).unwrap();
    assert_eq!(out.pixel_aspect, Rational::new(2, 1));
    assert_eq!((out.width, out.height, out.data_window), (W as u32, H as u32, PixelRect::full(W as u32, H as u32)));
    // A partial data window must be reframed by the caller (the engine does it in graphs).
    let mut p = cpu_frame(test_pattern(), "ACEScg");
    p.data_window = PixelRect::new(0, 0, (W / 2) as u32, H as u32);
    let err = node.apply(gpu(), &p).expect_err("partial window");
    assert_eq!(err.kind, ErrorKind::Permanent);
    assert!(!node.supports_data_window());
}

#[test]
fn pipeline_is_rebuilt_for_a_recreated_device() {
    // After device loss the engine replaces the shared GpuContext with
    // `recreate()` (new device, new id). A pipeline cached from the old device
    // must not be used on the new one.
    let cfg = Config::builtin_default().unwrap();
    let node = OcioTransformNode::colorspace(&cfg, "ACEScg", "sRGB - Texture").unwrap();
    let a = gpu();
    let b = a.recreate().expect("recreate device");
    assert_ne!(a.id(), b.id());
    let read = |g: &GpuContext, f: &Frame| -> Vec<u16> {
        match &f.to_cpu(g).expect("readback").storage {
            FrameStorage::Cpu(img) => img.pixels.iter().map(|v| v.to_bits()).collect(),
            FrameStorage::Gpu(_) => unreachable!(),
        }
    };
    let src = cpu_frame(test_pattern(), "ACEScg");
    let out_a = read(a, &node.apply(a, &src.to_gpu(a)).unwrap());
    let out_b = read(&b, &node.apply(&b, &src.to_gpu(&b)).unwrap());
    assert_eq!(node.cached_devices(), 2, "one pipeline per device");
    assert!(!Arc::ptr_eq(&node.prepare(a).unwrap(), &node.prepare(&b).unwrap()));
    assert!(out_a == out_b, "same transform on a rebuilt device must be bit-identical");
    // Again on the old one: still cached, no rebuild.
    assert!(Arc::ptr_eq(&node.prepare(a).unwrap(), &node.prepare(a).unwrap()));
    assert_eq!(node.cached_devices(), 2);
}
