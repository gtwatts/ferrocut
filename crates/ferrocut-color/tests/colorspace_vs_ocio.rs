//! `ferrocut-colorspace` (pure Rust, no OCIO) against OCIO 2.5's built-in
//! config, plus its WGSL snippet on the GPU against its own f64 math.
//! Run with `--nocapture` to see the measured errors.
//!
//! The reference is OCIO's *lossless* CPU processor
//! ([`Processor::apply_cpu_rgba_precise`](ferrocut_color::Processor)); OCIO's
//! default CPU processor uses fast pow approximations that are off by up to
//! ~2.5e-5 (also reported below). Remaining differences:
//! - matrices and pure gamma curves: f32 rounding (< 3e-7);
//! - sRGB: OCIO models it as a continuous curve (breakpoint 0.03929, toe
//!   slope 12.923) rather than the IEC 61966-2-1 piecewise constants (0.04045,
//!   12.92) used here, by browsers and by ThorVG. The toes differ by 0.025%:
//!   < 8e-7 in linear light, < 1e-5 in encoded values near black.
//!
//! ACEScg → encoded results are compared in linear light (both decoded with
//! the exact curve): pure power curves have infinite slope at 0, so an f32
//! residue of 1e-8 where the exact answer is 0 encodes to 3e-4.
#![cfg(not(ferrocut_no_ocio))]

use std::sync::mpsc;

use ferrocut_color::Config;
use ferrocut_colorspace::{Space, Transfer, convert_rgb, pixels};
use ferrocut_core::{AdapterPreference, GpuContext};
use half::f16;

const NAMED: [Space; 5] = [Space::ACESCG, Space::LINEAR_REC709, Space::SRGB, Space::REC709_GAMMA22, Space::REC709_GAMMA24];

/// 17^3 grid over [0, 1] plus the piecewise breakpoints.
fn grid() -> Vec<[f64; 3]> {
    let mut v = Vec::new();
    for r in 0..17 {
        for g in 0..17 {
            for b in 0..17 {
                v.push([r as f64 / 16.0, g as f64 / 16.0, b as f64 / 16.0]);
            }
        }
    }
    for x in [0.0031308, 0.04045, 0.018, 0.081, 1e-4, 0.999] {
        v.push([x, x / 2.0, (x * 3.0).min(1.0)]);
    }
    v
}

/// OCIO's lossless CPU processor on straight RGB triples (alpha 1).
fn ocio(src: &str, dst: &str, rgb: &[[f64; 3]]) -> Vec<[f64; 3]> {
    ocio_with(src, dst, rgb, true)
}

fn ocio_with(src: &str, dst: &str, rgb: &[[f64; 3]], precise: bool) -> Vec<[f64; 3]> {
    let p = Config::builtin_default().unwrap().colorspace_processor(src, dst).unwrap_or_else(|e| panic!("{src} -> {dst}: {e}"));
    let mut px: Vec<f32> = rgb.iter().flat_map(|c| [c[0] as f32, c[1] as f32, c[2] as f32, 1.0]).collect();
    if precise {
        p.apply_cpu_rgba_precise(&mut px, rgb.len(), 1).unwrap();
    } else {
        p.apply_cpu_rgba(&mut px, rgb.len(), 1).unwrap();
    }
    px.chunks(4).map(|c| [c[0] as f64, c[1] as f64, c[2] as f64]).collect()
}

fn max_err(a: &[[f64; 3]], b: &[[f64; 3]]) -> f64 {
    a.iter().zip(b).flat_map(|(x, y)| (0..3).map(move |i| (x[i] - y[i]).abs())).fold(0.0, f64::max)
}

/// Max abs error (linear light) allowed against OCIO; see the module docs.
const OCIO_TOL: f64 = 1e-6;

#[test]
fn every_named_space_matches_ocio_both_ways() {
    let g = grid();
    for space in NAMED {
        let name = space.ocio_name().unwrap();
        // space -> ACEScg
        let ours: Vec<_> = g.iter().map(|c| convert_rgb(space, Space::ACESCG, *c)).collect();
        let e1 = max_err(&ours, &ocio(name, "ACEScg", &g));
        // ACEScg -> space, on ACEScg values of in-gamut Rec.709 colors
        let aces: Vec<_> = g.iter().map(|c| convert_rgb(Space::LINEAR_REC709, Space::ACESCG, *c)).collect();
        let ours: Vec<_> = aces.iter().map(|c| convert_rgb(Space::ACESCG, space, *c)).collect();
        let theirs = ocio("ACEScg", name, &aces);
        let decode = |v: &[[f64; 3]]| v.iter().map(|c| c.map(|x| space.transfer.to_linear(x))).collect::<Vec<_>>();
        let e2 = max_err(&decode(&ours), &decode(&theirs));
        let fast = max_err(&g.iter().map(|c| convert_rgb(space, Space::ACESCG, *c)).collect::<Vec<_>>(), &ocio_with(name, "ACEScg", &g, false));
        eprintln!("{name:28} -> ACEScg max err {e1:.2e}; ACEScg -> {name} (linear light): {e2:.2e} (OCIO default CPU: {fast:.2e})");
        assert!(e1 < OCIO_TOL && e2 < OCIO_TOL, "{name}: {e1:e} / {e2:e}");
    }
}

#[test]
fn matrices_match_ocio_out_of_range_too() {
    // Linear transforms: HDR and negative values must match as well.
    let vals: Vec<[f64; 3]> = grid().iter().map(|c| [c[0] * 16.0 - 2.0, c[1] * -0.5, c[2] * 100.0]).collect();
    let ours: Vec<_> = vals.iter().map(|c| convert_rgb(Space::LINEAR_REC709, Space::ACESCG, *c)).collect();
    let theirs = ocio("Linear Rec.709 (sRGB)", "ACEScg", &vals);
    let rel = ours.iter().zip(&theirs).flat_map(|(a, b)| (0..3).map(move |i| (a[i] - b[i]).abs() / b[i].abs().max(1.0))).fold(0.0, f64::max);
    let ours: Vec<_> = vals.iter().map(|c| convert_rgb(Space::ACESCG, Space::LINEAR_REC709, *c)).collect();
    let theirs = ocio("ACEScg", "Linear Rec.709 (sRGB)", &vals);
    let rel2 = ours.iter().zip(&theirs).flat_map(|(a, b)| (0..3).map(move |i| (a[i] - b[i]).abs() / b[i].abs().max(1.0))).fold(0.0, f64::max);
    eprintln!("matrices on [-2, 100]: relative err {rel:.2e} / {rel2:.2e}");
    assert!(rel < 1e-6 && rel2 < 1e-6, "{rel:e} {rel2:e}");
}

#[test]
fn eight_bit_layer_path_matches_ocio_to_f16_precision() {
    // What ferrocut-lottie / ferrocut-html produce, against OCIO on the
    // unpremultiplied values, re-premultiplied in f64.
    let mut worst = 0f64;
    for a in (1..=255u8).step_by(2) {
        let cs: Vec<u8> = (0..=a).step_by(3).collect();
        let px: Vec<[u8; 4]> = cs.iter().map(|&c| [c, a - c / 2, c / 3, a]).collect();
        let mut out = Vec::new();
        pixels::srgb8_premul_to_acescg_f16(px.iter().copied(), &mut out);
        let straight: Vec<[f64; 3]> = px.iter().map(|p| [0, 1, 2].map(|i| p[i] as f64 / a as f64)).collect();
        let want = ocio("sRGB Encoded Rec.709 (sRGB)", "ACEScg", &straight);
        let alpha = a as f64 / 255.0;
        for (k, w) in want.iter().enumerate() {
            for i in 0..3 {
                let (got, w) = (out[k * 4 + i].to_f64(), w[i] * alpha);
                // f16 rounding (half an ulp: 2^-11 relative) plus OCIO_TOL.
                let tol = w.abs() * 2f64.powi(-11) + OCIO_TOL;
                worst = worst.max((got - w).abs() / tol);
                assert!((got - w).abs() <= tol, "a={a} px={:?} ch{i}: {got} vs {w}", px[k]);
            }
            assert_eq!(out[k * 4 + 3], f16::from_f64(alpha));
        }
    }
    eprintln!("8-bit path: worst error {worst:.2} of the f16 tolerance");
}

#[test]
fn wgsl_snippet_on_gpu_matches_cpu() {
    let Ok(gpu) = GpuContext::new(AdapterPreference::default()) else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    const FUNCS: [&str; 10] = [
        "fc_rec709_to_acescg",
        "fc_acescg_to_rec709",
        "fc_srgb_to_linear",
        "fc_linear_to_srgb",
        "fc_bt709_to_linear",
        "fc_linear_to_bt709",
        "fc_gamma22_to_linear",
        "fc_linear_to_gamma22",
        "fc_gamma24_to_linear",
        "fc_linear_to_gamma24",
    ];
    let cpu = |f: usize, c: [f64; 3]| -> [f64; 3] {
        use ferrocut_colorspace::{ACESCG_TO_REC709, REC709_TO_ACESCG, apply};
        let t = [Transfer::Srgb, Transfer::Bt709, Transfer::Gamma22, Transfer::Gamma24];
        match f {
            0 => apply(&REC709_TO_ACESCG, c),
            1 => apply(&ACESCG_TO_REC709, c),
            f if f % 2 == 0 => c.map(|v| t[f / 2 - 1].to_linear(v)),
            f => c.map(|v| t[f / 2 - 1].from_linear(v)),
        }
    };
    let input: Vec<[f64; 3]> = grid();
    let n = input.len();
    let mut body = String::new();
    for (k, f) in FUNCS.iter().enumerate() {
        body += &format!("    outp[i * {}u + {k}u] = vec4<f32>({f}(c), 0.0);\n", FUNCS.len());
    }
    let src = format!(
        "{}\n@group(0) @binding(0) var<storage, read> inp: array<vec4<f32>>;\n@group(0) @binding(1) var<storage, read_write> outp: array<vec4<f32>>;\n@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) {{\n    let i = id.x;\n    if (i >= arrayLength(&inp)) {{ return; }}\n    let c = inp[i].xyz;\n{body}}}\n",
        ferrocut_colorspace::wgsl()
    );
    let dev = &gpu.device;
    let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("colorspace-test"), source: wgpu::ShaderSource::Wgsl(src.into()) });
    let pipeline = dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let in_data: Vec<f32> = input.iter().flat_map(|c| [c[0] as f32, c[1] as f32, c[2] as f32, 0.0]).collect();
    let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
    let inb = dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: (in_data.len() * 4) as u64, usage, mapped_at_creation: false });
    gpu.queue.write_buffer(&inb, 0, bytemuck::cast_slice(&in_data));
    let out_len = (n * FUNCS.len() * 16) as u64;
    let outb = dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: out_len, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let read = dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: out_len, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let bg = dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: inb.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: outb.as_entire_binding() },
        ],
    });
    let mut enc = dev.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(n.div_ceil(64) as u32, 1, 1);
    }
    enc.copy_buffer_to_buffer(&outb, 0, &read, 0, out_len);
    let idx = gpu.queue.submit([enc.finish()]);
    let (tx, rx) = mpsc::channel();
    read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    dev.poll(wgpu::PollType::Wait { submission_index: Some(idx), timeout: None }).unwrap();
    rx.recv().unwrap().unwrap();
    let got: Vec<f32> = {
        let view = read.slice(..).get_mapped_range().expect("mapped");
        view.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
    };
    let mut worst = [0f64; FUNCS.len()];
    for (i, c) in input.iter().enumerate() {
        // The GPU sees the f32-rounded input; evaluate the reference on that.
        let c32 = c.map(|v| v as f32 as f64);
        for (f, w) in worst.iter_mut().enumerate() {
            // BT.709's rounded constants make the curve jump by 2.4e-4 at its
            // breakpoints; exactly there, f32 vs f64 comparisons may pick
            // different sides. Skip only those samples.
            let at_break = |b: f64| c32.iter().any(|v| (v - b).abs() < 1e-7);
            if (f == 4 && at_break(0.081)) || (f == 5 && at_break(0.018)) {
                continue;
            }
            let want = cpu(f, c32);
            let o = (i * FUNCS.len() + f) * 4;
            for ch in 0..3 {
                *w = w.max((got[o + ch] as f64 - want[ch]).abs());
            }
        }
    }
    eprintln!("[gpu] {}", gpu.describe());
    for (f, w) in FUNCS.iter().zip(worst) {
        eprintln!("  {f:22} max abs err {w:.2e}");
        assert!(w < 1e-5, "{f}: {w:e}");
    }
}

/// `ferrocut_colorspace::named` (what the engine uses, keyed by frame color
/// space names): every name and alias must exist in OCIO 2.5's built-in CG
/// config (or, for `Camera Rec.709`, the studio config) and `matrix(name,
/// ACEScg) · decode` must match OCIO's name -> ACEScg processor; the reverse
/// is compared in linear light.
#[test]
fn named_api_matches_ocio_for_every_name_and_alias() {
    use ferrocut_colorspace::named::{self, names};
    let cg = Config::builtin_default().unwrap();
    let studio = Config::load("ocio://studio-config-v4.0.0_aces-v2.0_ocio-v2.5").unwrap();
    let rgb = grid();
    let run = |p: ferrocut_color::Processor, rgb: &[[f64; 3]]| -> Vec<[f64; 3]> {
        let mut px: Vec<f32> = rgb.iter().flat_map(|c| [c[0] as f32, c[1] as f32, c[2] as f32, 1.0]).collect();
        p.apply_cpu_rgba_precise(&mut px, rgb.len(), 1).unwrap();
        px.chunks(4).map(|c| [c[0] as f64, c[1] as f64, c[2] as f64]).collect()
    };
    let mut worst: Vec<(String, f64, f64, f64)> = Vec::new();
    for (name, space) in named::all() {
        let (cfg, which) = match cg.colorspace_processor(name, names::ACESCG) {
            Ok(_) => (&cg, "cg"),
            Err(_) => (&studio, "studio"),
        };
        let to_ap1 = cfg.colorspace_processor(name, names::ACESCG).unwrap_or_else(|e| panic!("{name} not in OCIO ({which}): {e}"));
        let from_ap1 = cfg.colorspace_processor(names::ACESCG, name).unwrap();
        let m = named::matrix(name, names::ACESCG).unwrap();
        let mi = named::matrix(names::ACESCG, name).unwrap();
        let t = named::transfer(name).unwrap();
        assert_eq!(t, space.transfer);
        let mul = |m: &[[f32; 3]; 3], c: [f32; 3]| -> [f64; 3] {
            std::array::from_fn(|i| (0..3).map(|j| m[i][j] as f64 * c[j] as f64).sum())
        };
        // name -> ACEScg
        let ours: Vec<[f64; 3]> = rgb.iter().map(|c| mul(&m, t.decode(c.map(|v| v as f32)))).collect();
        let fwd = run(to_ap1, &rgb)
            .iter()
            .zip(&ours)
            .flat_map(|(a, b)| (0..3).map(move |i| (a[i] - b[i]).abs()))
            .fold(0.0, f64::max);
        // ACEScg -> name, compared in linear light, on ACEScg values of in-gamut
        // Rec.709 colors (negative results take each curve's extension, where
        // OCIO and this crate differ; reported, not asserted).
        let rev_err = |ap1: &[[f64; 3]]| -> f64 {
            let theirs = run(cfg.colorspace_processor(names::ACESCG, name).unwrap(), ap1);
            ap1.iter()
                .zip(&theirs)
                .flat_map(|(c, o)| {
                    let enc = t.encode(mul(&mi, c.map(|v| v as f32)).map(|v| v as f32));
                    (0..3).map(move |i| (t.to_linear(o[i]) - t.to_linear(enc[i] as f64)).abs())
                })
                .fold(0.0, f64::max)
        };
        let in_gamut: Vec<[f64; 3]> = rgb.iter().map(|c| convert_rgb(Space::LINEAR_REC709, Space::ACESCG, *c)).collect();
        let rev = rev_err(&in_gamut);
        let out_of_gamut = rev_err(&rgb.iter().map(|c| c.map(|v| v * 0.8)).collect::<Vec<_>>());
        drop(from_ap1);
        worst.push((format!("{name} [{which}]"), fwd, rev, out_of_gamut));
        // BT.709: spec constants (here, the engine, FFmpeg) vs OCIO's
        // continuous ExponentWithLinear form; the toes differ slightly.
        let tol = if t == Transfer::Bt709 { 1e-4 } else if t == Transfer::Srgb { 1e-6 } else { 5e-7 };
        assert!(fwd < tol && rev < tol, "{name}: name->ACEScg {fwd:.2e}, ACEScg->name {rev:.2e} (tol {tol:.0e})");
    }
    for (n, f, r, o) in &worst {
        eprintln!("named {n:48} ->ACEScg {f:.2e}  ACEScg-> {r:.2e}  (out-of-gamut ACEScg-> {o:.2e})");
    }
}
