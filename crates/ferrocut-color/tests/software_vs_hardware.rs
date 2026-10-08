//! The OCIO node on a software adapter (Mesa lavapipe, core's
//! `AdapterPreference::Cpu`, the path `--cpu` / `FERROCUT_ADAPTER=cpu` takes)
//! vs a hardware GPU (NVIDIA preferred): same transform, same frame, compared
//! as display code values (max level difference at 10 and 8 bits) and by luma
//! SSIM, each also against OCIO's CPU processor.
//!
//! Needs both adapters; skips otherwise (CI with lavapipe only, or
//! `FERROCUT_ADAPTER` set, which pins both to one adapter). `--nocapture`
//! prints the statistics.
#![cfg(not(ferrocut_no_ocio))]

use std::sync::Arc;

use ferrocut_color::{Config, GpuShaderOptions, OcioTransformNode};
use ferrocut_core::{
    AdapterPreference, ColorSpace, CpuImage, Frame, FrameStorage, GpuContext, GpuRequirements,
    PixelRect, Rational,
};
use half::f16;

const W: usize = 640;
const H: usize = 360;

fn adapters() -> Option<(GpuContext, GpuContext)> {
    if std::env::var_os("FERROCUT_ADAPTER").is_some() {
        eprintln!("skipping: FERROCUT_ADAPTER pins every context to one adapter");
        return None;
    }
    let req = GpuRequirements::optional(wgpu::Features::FLOAT32_FILTERABLE);
    let hw = match GpuContext::with_requirements(AdapterPreference::DiscreteNvidia, &req) {
        Ok(g) if g.info.device_type != wgpu::DeviceType::Cpu => g,
        Ok(g) => {
            eprintln!("skipping: no hardware adapter (best is {})", g.describe());
            return None;
        }
        Err(e) => {
            eprintln!("skipping: no GPU adapter ({e})");
            return None;
        }
    };
    let sw = match GpuContext::with_requirements(AdapterPreference::Cpu, &req) {
        Ok(g) if g.info.device_type == wgpu::DeviceType::Cpu => g,
        Ok(g) => {
            eprintln!("skipping: no software adapter (got {})", g.describe());
            return None;
        }
        Err(e) => {
            eprintln!("skipping: no software adapter ({e})");
            return None;
        }
    };
    eprintln!("[hw] {}\n[sw] {}", hw.describe(), sw.describe());
    Some((hw, sw))
}

/// Opaque ACEScg test image with smooth ramps and fine detail (SSIM needs
/// structure): hue sweep across x, exposure ramp down y up to `peak`, and a
/// sine zone plate modulating brightness by +-10%.
fn pattern(peak: f32) -> Vec<f16> {
    let mut px = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            let h = x as f32 / W as f32 * 6.0;
            let e = -0.02 + (peak + 0.02) * (y as f32 / (H - 1) as f32).powf(2.2);
            let (dx, dy) = (x as f32 - W as f32 / 2.0, y as f32 - H as f32 / 2.0);
            let zone = 1.0 + 0.1 * ((dx * dx + dy * dy) * 0.002).sin();
            let (r, g, b) = (
                (1.0 - h.min(6.0 - h).min(2.0) / 2.0).max(0.03),
                (1.0 - (h - 2.0).abs().min(2.0) / 2.0).max(0.03),
                (1.0 - (h - 4.0).abs().min(2.0) / 2.0).max(0.03),
            );
            for v in [r * e * zone, g * e * zone, b * e * zone, 1.0] {
                px.push(f16::from_f32(v));
            }
        }
    }
    px
}

fn frame(px: Vec<f16>, cs: &str) -> Frame {
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

fn render(gpu: &GpuContext, node: &OcioTransformNode, src: &Frame) -> Vec<f32> {
    let out = node.apply(gpu, src).expect("render");
    match &out.to_cpu(gpu).expect("readback").storage {
        FrameStorage::Cpu(img) => img.pixels.iter().map(|v| v.to_f32()).collect(),
        FrameStorage::Gpu(_) => unreachable!(),
    }
}

/// Display code value at `bits` (output clipped to [0, 1], as a display would).
fn code(v: f32, bits: u32) -> i32 {
    (v.clamp(0.0, 1.0) * ((1u32 << bits) - 1) as f32).round() as i32
}

fn max_level_diff(a: &[f32], b: &[f32], bits: u32) -> i32 {
    let rgb = |s: &[f32]| {
        s.as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect::<Vec<_>>()
    };
    rgb(a)
        .iter()
        .zip(rgb(b))
        .map(|(x, y)| (code(*x, bits) - code(y, bits)).abs())
        .max()
        .unwrap_or(0)
}

/// Mean SSIM of Rec.709 luma (of the clipped display values), 8x8 windows on a
/// stride of 4, L = 1.
fn ssim_luma(a: &[f32], b: &[f32]) -> f64 {
    let luma = |s: &[f32]| -> Vec<f64> {
        s.as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let c = |v: f32| v.clamp(0.0, 1.0) as f64;
                0.2126 * c(p[0]) + 0.7152 * c(p[1]) + 0.0722 * c(p[2])
            })
            .collect()
    };
    let (ya, yb) = (luma(a), luma(b));
    let (c1, c2) = (0.01f64.powi(2), 0.03f64.powi(2));
    let (mut sum, mut n) = (0.0, 0usize);
    for y0 in (0..=H - 8).step_by(4) {
        for x0 in (0..=W - 8).step_by(4) {
            let (mut ma, mut mb) = (0.0, 0.0);
            for y in y0..y0 + 8 {
                for x in x0..x0 + 8 {
                    ma += ya[y * W + x];
                    mb += yb[y * W + x];
                }
            }
            ma /= 64.0;
            mb /= 64.0;
            let (mut va, mut vb, mut cov) = (0.0, 0.0, 0.0);
            for y in y0..y0 + 8 {
                for x in x0..x0 + 8 {
                    let (da, db) = (ya[y * W + x] - ma, yb[y * W + x] - mb);
                    va += da * da;
                    vb += db * db;
                    cov += da * db;
                }
            }
            va /= 63.0;
            vb /= 63.0;
            cov /= 63.0;
            sum += ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            n += 1;
        }
    }
    sum / n as f64
}

struct Cmp {
    ssim: f64,
    max10: i32,
    max8: i32,
    max_abs: f32,
    /// Largest distance in f16 units in the last place (RGB, unclipped).
    /// Informational: near black an ulp is ~6e-8, so transcendental differences
    /// show up as hundreds of ulps that are invisible on any display.
    max_ulp: u16,
    /// Fraction of RGB samples whose f16 bits differ at all.
    differ: f64,
}

fn cmp(a: &[f32], b: &[f32]) -> Cmp {
    let max_abs = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x.clamp(0.0, 1.0) - y.clamp(0.0, 1.0)).abs())
        .fold(0.0, f32::max);
    let bits = |v: f32| f16::from_f32(v).to_bits();
    let rgb = |s: &[f32]| {
        s.as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect::<Vec<_>>()
    };
    let (ra, rb) = (rgb(a), rgb(b));
    let ulps: Vec<u16> = ra
        .iter()
        .zip(&rb)
        .map(|(x, y)| {
            let (bx, by) = (bits(*x), bits(*y));
            // Same sign (all outputs here are >= 0 or tiny negatives near 0).
            if (bx ^ by) & 0x8000 == 0 {
                bx.abs_diff(by)
            } else {
                (bx & 0x7fff) + (by & 0x7fff)
            }
        })
        .collect();
    let differ = ulps.iter().filter(|u| **u > 0).count() as f64 / ulps.len() as f64;
    Cmp {
        ssim: ssim_luma(a, b),
        max10: max_level_diff(a, b, 10),
        max8: max_level_diff(a, b, 8),
        max_abs,
        max_ulp: ulps.iter().copied().max().unwrap_or(0),
        differ,
    }
}

/// Renders `src` on both adapters and on OCIO's CPU path; returns hw-vs-sw.
fn compare(
    hw: &GpuContext,
    sw: &GpuContext,
    name: &str,
    node: &OcioTransformNode,
    src: Vec<f16>,
) -> Cmp {
    let input = frame(src.clone(), node.src_space().name());
    let (h, s) = (render(hw, node, &input), render(sw, node, &input));
    let mut c: Vec<f32> = src.iter().map(|v| v.to_f32()).collect();
    node.apply_cpu_premultiplied(&mut c, W, H).unwrap();
    let c: Vec<f32> = c.iter().map(|&v| f16::from_f32(v).to_f32()).collect();
    let (hs, hc, sc) = (cmp(&h, &s), cmp(&h, &c), cmp(&s, &c));
    let luts = |g| {
        node.prepare(g)
            .map(|t| if t.luts_f32 { "f32" } else { "f16" })
            .unwrap_or("?")
    };
    eprintln!(
        "[{name}] {W}x{H}, LUT textures {} (hw {}, sw {})",
        node.shader().textures.len(),
        luts(hw),
        luts(sw),
    );
    for (what, r) in [("hw vs sw ", &hs), ("hw vs cpu", &hc), ("sw vs cpu", &sc)] {
        eprintln!(
            "  {what}: 1-SSIM {:.2e}  max level diff {} @10-bit, {} @8-bit  max |d| {:.2e}  max {} f16 ulp  {:.3}% samples differ",
            1.0 - r.ssim,
            r.max10,
            r.max8,
            r.max_abs,
            r.max_ulp,
            100.0 * r.differ
        );
    }
    // OCIO's CPU processor is the reference for both adapters.
    assert!(
        sc.ssim > 0.9999 && sc.max10 <= 1,
        "[{name}] lavapipe vs OCIO CPU"
    );
    hs
}

#[test]
fn lavapipe_matches_hardware_gpu() {
    let Some((hw, sw)) = adapters() else { return };
    let cfg = Config::builtin_default().unwrap();
    let info = cfg.info().unwrap();

    let dv =
        OcioTransformNode::display_view(&cfg, "ACEScg", &info.default_display, &info.default_view)
            .unwrap();
    let a = compare(&hw, &sw, "ACEScg -> display/view", &dv, pattern(8.0));

    let tex = OcioTransformNode::colorspace(&cfg, "ACEScg", "sRGB - Texture").unwrap();
    let b = compare(&hw, &sw, "ACEScg -> sRGB - Texture", &tex, pattern(1.0));

    let n = 17u32;
    let mut rgb = Vec::with_capacity((n * n * n * 3) as usize);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let f = |i: u32| i as f32 / (n - 1) as f32;
                let (r, g, b) = (f(r), f(g), f(b));
                let l = 0.2722 * r + 0.6741 * g + 0.0537 * b;
                let s = |c: f32| (l + 1.3 * (c - l)).clamp(0.0, 1.0).powf(0.85);
                rgb.extend([s(r), s(g), s(b)]);
            }
        }
    }
    let p = cfg.lut3d_processor(n, &rgb, true).unwrap();
    let lut = OcioTransformNode::new(
        p,
        ColorSpace::new("ACEScg"),
        ColorSpace::new("ACEScg"),
        GpuShaderOptions::default(),
    )
    .unwrap();
    let c = compare(&hw, &sw, "17^3 LUT (tetrahedral)", &lut, pattern(1.0));

    // Measured (RTX 5090 / Vulkan vs lavapipe, Mesa 26.1, f32 LUTs on both):
    // max |d| 4.9e-4 on clipped display values (half a 10-bit code), so at
    // most one 10-bit / 8-bit code value (a rounding-boundary flip); 1-SSIM
    // <= 3e-8; 0.001-1.2% of samples differ in their f16 bits at all. A
    // difference of a whole code value or more is a real divergence.
    for (name, r) in [("display/view", a), ("sRGB - Texture", b), ("3D LUT", c)] {
        assert!(r.ssim > 0.99999, "[{name}] hw vs sw SSIM {}", r.ssim);
        assert!(
            r.max10 <= 1 && r.max8 <= 1,
            "[{name}] hw vs sw max level diff {} @10-bit, {} @8-bit",
            r.max10,
            r.max8
        );
        assert!(
            r.max_abs < 1.0 / 1023.0,
            "[{name}] hw vs sw max |d| {:e}",
            r.max_abs
        );
    }
}
