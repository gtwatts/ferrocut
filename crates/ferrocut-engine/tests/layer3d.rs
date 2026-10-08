//! 3D layers, camera and motion blur: the multi-sample projective kernel
//! against a CPU reference on the default adapter and lavapipe, frame keys
//! (2D timelines and blur-off keep theirs), blur coverage, a default-camera
//! 3D layer matching the 2D layer, the painter's sort, the near plane, and
//! validation / edit ops. GPU tests skip with a note when an adapter is
//! unavailable.

use std::sync::{Arc, OnceLock};

use ferrocut_core::{
    AdapterPreference, CancelToken, ColorSpace, CpuFrame, Frame, FrameStorage, GpuContext,
    PixelRect, Rational, RationalTime, RenderCtx, WorkerState,
};
use ferrocut_engine::Timeline;
use ferrocut_engine::compile::compile;
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::graph::FrameCache;
use ferrocut_engine::layer3d::{
    CameraSpec, Mat3, MultiSetup, affine_homography, layer_homography, plan_multi,
};
use ferrocut_engine::transform::TransformAt;
use half::f16;

const W: u32 = 48;
const H: u32 = 32;

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

fn ctx_parts(gpu: &GpuContext) -> (Arc<Compositor>, WorkerState) {
    let comp = Arc::new(Compositor::new(gpu));
    let mut w = WorkerState::default();
    w.slot(compositor_slot(), || Ok(comp.clone())).unwrap();
    (comp, w)
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

/// Premultiplied test pattern, exact in f16.
fn pattern(x: i32, y: i32) -> [f32; 4] {
    let a = if (x + y) % 3 == 0 { 0.5 } else { 1.0 };
    [
        a * ((x * 7 + y) % 16) as f32 / 16.0,
        a * ((y * 5 + x * 3) % 16) as f32 / 16.0,
        a * ((x ^ y) % 8) as f32 / 8.0,
        a,
    ]
}

fn source() -> Frame {
    let mut px = Vec::new();
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            px.extend(pattern(x, y).map(f16::from_f32));
        }
    }
    Frame::from_cpu(&CpuFrame::new(W, H, ColorSpace::acescg(), px))
}

/// Display-window pixels (outside the data window = 0).
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

fn mitchell(x: f64) -> f64 {
    let (b, c) = (0.0, 0.5);
    let x = x.abs();
    if x < 1.0 {
        ((12.0 - 9.0 * b - 6.0 * c) * x * x * x
            + (-18.0 + 12.0 * b + 6.0 * c) * x * x
            + (6.0 - 2.0 * b))
            / 6.0
    } else if x < 2.0 {
        ((-b - 6.0 * c) * x * x * x
            + (6.0 * b + 30.0 * c) * x * x
            + (-12.0 * b - 48.0 * c) * x
            + (8.0 * b + 24.0 * c))
            / 6.0
    } else {
        0.0
    }
}

/// CPU reference of `transform_ms.wgsl` (mip 0) at output pixel (x, y).
fn reference(k: &MultiSetup, x: u32, y: u32) -> [f32; 4] {
    let p = [x as f64 + 0.5, y as f64 + 0.5, 1.0];
    let mut sum = [0.0f64; 4];
    for g in &k.inverses {
        let Some(g) = g else { continue };
        let d = |r: usize| g[r][0] * p[0] + g[r][1] * p[1] + g[r][2];
        let w = d(2);
        if w <= 0.0 || w > 1.0 {
            continue;
        }
        let s = [d(0) / w, d(1) / w];
        let jac = |r: usize| {
            let a = (g[r][0] - s[r] * g[2][0]) / w;
            let b = (g[r][1] - s[r] * g[2][1]) / w;
            (a * a + b * b).sqrt().clamp(1.0, 8.0)
        };
        let fs = [jac(0), jac(1)];
        let rad = [(2.0 * fs[0]).ceil() as i32, (2.0 * fs[1]).ceil() as i32];
        let c = [s[0] - 0.5, s[1] - 0.5];
        let base = [c[0].floor() as i32, c[1].floor() as i32];
        let (mut acc, mut ws) = ([0.0f64; 4], 0.0);
        for j in 1 - rad[1]..=rad[1] {
            let qy = base[1] + j;
            let wy = mitchell((qy as f64 - c[1]) / fs[1]);
            for i in 1 - rad[0]..=rad[0] {
                let qx = base[0] + i;
                let wt = wy * mitchell((qx as f64 - c[0]) / fs[0]);
                ws += wt;
                if (0..W as i32).contains(&qx) && (0..H as i32).contains(&qy) {
                    let v = pattern(qx, qy);
                    for ch in 0..4 {
                        acc[ch] += wt * v[ch] as f64;
                    }
                }
            }
        }
        let o = acc.map(|v| if ws != 0.0 { v / ws } else { 0.0 });
        for ch in 0..3 {
            sum[ch] += o[ch].max(0.0);
        }
        sum[3] += o[3].clamp(0.0, 1.0);
    }
    let n = k.inverses.len() as f64;
    sum.map(|v| f16::from_f64(v / n).to_f32())
}

fn at(pos: [f64; 2], scale: f64, rot: f64) -> TransformAt {
    TransformAt {
        position: pos,
        anchor: [W as f64 / 2.0, H as f64 / 2.0],
        scale: [scale, scale],
        rotation_deg: rot,
    }
}

fn camera(json: &str) -> CameraSpec {
    let c: CameraSpec = serde_json::from_str(json).unwrap();
    c.validate().unwrap();
    c
}

fn cases() -> Vec<(&'static str, Vec<Mat3>)> {
    let z = RationalTime::ZERO;
    let l3 = |t: &TransformAt, d3: [f64; 7], cam: Option<&CameraSpec>| {
        layer_homography(t, d3, cam, z, W, H, 1.0).0
    };
    let cam = camera(
        r#"{"position": ["30", "10", "-50"], "point_of_interest": ["20", "18", "10"], "fov_deg": "70"}"#,
    );
    vec![
        (
            "3D Y/X rotation and depth, default camera",
            vec![l3(
                &at([24.0, 16.0], 0.8, 10.0),
                [30.0, 0.0, 0.0, 0.0, 0.0, 15.0, 35.0],
                None,
            )],
        ),
        (
            "3D through a moved fov camera, partly off frame",
            vec![l3(
                &at([24.0, 16.0], 1.0, 0.0),
                [0.0, 0.0, 0.0, 20.0, 0.0, 0.0, 0.0],
                Some(&cam),
            )],
        ),
        (
            "2D motion blur, 6 samples",
            (0..6)
                .map(|i| {
                    affine_homography(&at([18.0 + 2.5 * i as f64, 16.0], 0.9, 4.0 * i as f64), 1.0)
                })
                .collect(),
        ),
        (
            "3D motion blur in depth",
            (0..4)
                .map(|i| {
                    l3(
                        &at([24.0, 16.0], 1.0, 0.0),
                        [
                            -10.0 + 8.0 * i as f64,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            5.0 * i as f64,
                        ],
                        None,
                    )
                })
                .collect(),
        ),
    ]
}

#[test]
fn kernel_matches_the_cpu_reference_on_every_adapter() {
    let ads = adapters();
    let src = source();
    let mut worst = (0.0f32, 0.0f32);
    for (name, fwds) in cases() {
        let k = plan_multi(&fwds, PixelRect::full(W, H), W, H).unwrap();
        assert_eq!(k.mip, [0, 0], "{name}");
        let want: Vec<[f32; 4]> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .map(|(x, y)| reference(&k, x, y))
            .collect();
        assert!(want.iter().any(|p| p[3] > 0.0), "{name}: visible");
        let mut outs = Vec::new();
        for (an, gpu) in &ads {
            let (comp, mut w) = ctx_parts(gpu);
            let cancel = CancelToken::new();
            let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
            let f = comp
                .transform_multi(&mut ctx, &src.to_gpu(gpu), &k)
                .unwrap();
            ctx.flush();
            let got = display(gpu, &f);
            let (d, i) = max_codes(&got, &want);
            assert!(
                d <= 2.0,
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
            assert!(d <= 2.0, "{name}: adapters differ by {d:.3} codes at {i}");
        }
    }
    eprintln!(
        "max difference: {:.4} codes vs the CPU reference, {:.4} between adapters",
        worst.0, worst.1
    );
}

/// A gray background and a small orange card moving right for 1 s, then
/// holding until 2 s. `extra` is spliced into the timeline object and
/// `clip_extra` into the card.
fn tl(extra: &str, clip_extra: &str) -> Timeline {
    Timeline::from_json(&format!(
        r#"{{
  "output": {{ "width": 48, "height": 32, "fps": "24", "gop": 12 }}{extra},
  "tracks": [
    {{ "name": "V1", "clips": [
      {{ "id": "bg", "start": 0, "duration": "2",
        "generator": {{ "type": "solid", "color": ["0.2", "0.2", "0.2"] }} }}
    ]}},
    {{ "name": "V2", "clips": [
      {{ "id": "card", "start": 0, "duration": "2",
        "generator": {{ "type": "solid", "color": ["1", "0.5", "0"] }},
        "transform": {{ "scale": "1/4",
          "position": [ {{ "keyframes": [ {{ "t": "0", "v": "10" }}, {{ "t": "1", "v": "38" }} ] }}, "16" ] }}{clip_extra} }}
    ]}}
  ]
}}"#
    ))
    .unwrap()
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

fn render(gpu: &GpuContext, tl: &Timeline, frame: i64) -> Vec<[f32; 4]> {
    let c = compile(tl).unwrap();
    let (_, mut w) = ctx_parts(gpu);
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut w, &cancel, None);
    let mut cache = FrameCache::new(8);
    let t = RationalTime::from_frames(frame, tl.output.fps);
    let f = c.graph.evaluate(c.output, t, &mut ctx, &mut cache).unwrap();
    ctx.flush();
    display(gpu, &f)
}

const BLUR: &str =
    r#", "motion_blur": { "shutter_angle": 180, "shutter_phase": -90, "samples": "8" }"#;

#[test]
fn blur_switches_keep_keys_until_both_are_on_and_still_frames_keep_theirs() {
    let base = keys(&tl("", ""));
    // Either switch alone is off (After Effects: composition and layer).
    assert_eq!(keys(&tl(BLUR, "")), base);
    assert_eq!(keys(&tl("", r#", "motion_blur": true"#)), base);
    let blurred = keys(&tl(BLUR, r#", "motion_blur": true"#));
    for i in 0..base.len() {
        // The shutter spans t +- 1/96 s: frames up to 24 (1 s) see motion.
        assert_eq!(blurred[i] == base[i], i > 24, "frame {i}");
    }
}

#[test]
fn motion_blur_smears_and_keeps_coverage() {
    let still = tl("", "");
    let blur = tl(BLUR, r#", "motion_blur": true"#);
    for (an, gpu) in adapters() {
        let a = render(gpu, &still, 12);
        let b = render(gpu, &blur, 12);
        // Orange coverage (red minus the background) is conserved.
        let cov = |v: &[[f32; 4]]| {
            v.iter()
                .map(|p| p[0] as f64 - 0.2f64.powf(2.2))
                .sum::<f64>()
        };
        let (ca, cb) = (cov(&a), cov(&b));
        assert!((ca - cb).abs() / ca < 0.02, "{an}: coverage {ca} vs {cb}");
        // The card moves 28 px/s: 7/12 px per shutter at 180 degrees, so
        // the blurred row has partial values where the sharp one is flat.
        let row = |v: &[[f32; 4]], x: u32| v[(16 * W + x) as usize][0];
        let partial = (0..W)
            .filter(|&x| (row(&b, x) - row(&a, x)).abs() > 0.01)
            .count();
        assert!(partial >= 2, "{an}: blur changes the edges ({partial})");
        // Held frames render exactly like the unblurred timeline.
        assert_eq!(render(gpu, &still, 40), render(gpu, &blur, 40), "{an}");
    }
}

#[test]
fn default_camera_3d_layer_looks_like_the_2d_layer() {
    let flat = tl("", "");
    let card = tl("", r#", "three_d": true"#);
    assert_ne!(keys(&card), keys(&flat));
    for (an, gpu) in adapters() {
        for frame in [0, 7, 30] {
            let (d, i) = max_codes(&render(gpu, &card, frame), &render(gpu, &flat, frame));
            assert!(d <= 1.0, "{an} frame {frame}: {d:.3} codes at {i}");
        }
    }
}

fn stack(layers: &[(&str, &str)]) -> Timeline {
    let tracks: Vec<String> = layers
        .iter()
        .enumerate()
        .map(|(i, (color, extra))| {
            format!(
                r#"{{ "name": "V{i}", "clips": [ {{ "id": "c{i}", "start": 0, "duration": "1",
                  "generator": {{ "type": "solid", "color": {color} }}{extra} }} ] }}"#
            )
        })
        .collect();
    Timeline::from_json(&format!(
        r#"{{ "output": {{ "width": 48, "height": 32, "fps": "24" }}, "tracks": [{}] }}"#,
        tracks.join(",")
    ))
    .unwrap()
}

const BLUE: &str = r#"["0", "0", "1"]"#;
const RED: &str = r#"["1", "0", "0"]"#;
const GREEN: &str = r#"["0", "1", "0"]"#;

fn center(gpu: &GpuContext, tl: &Timeline, frame: i64) -> [f32; 4] {
    render(gpu, tl, frame)[(16 * W + 24) as usize]
}

fn dominant(p: [f32; 4]) -> usize {
    (0..3).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap()
}

#[test]
fn painters_sort_draws_far_layers_first_and_2d_layers_split_runs() {
    // Bottom track moves from in front of the top track to behind it.
    let moving = r#", "three_d": true, "transform": { "scale": "1/2",
        "position_z": { "keyframes": [ { "t": "0", "v": "-30" }, { "t": "23/24", "v": "30" } ] } }"#;
    let fixed = r#", "three_d": true, "transform": { "scale": "1/2" }"#;
    let sorted = stack(&[(BLUE, moving), (RED, fixed)]);
    // A 2D layer between them splits the run: track order again.
    let split = stack(&[
        (BLUE, moving),
        (GREEN, r#", "transform": { "scale": "1/8" }"#),
        (RED, fixed),
    ]);
    let k = keys(&sorted);
    assert_ne!(k[0], k[23]);
    for (an, gpu) in adapters() {
        assert_eq!(dominant(center(gpu, &sorted, 0)), 2, "{an}: blue in front");
        assert_eq!(dominant(center(gpu, &sorted, 23)), 0, "{an}: red in front");
        assert_eq!(
            dominant(center(gpu, &split, 0)),
            0,
            "{an}: 2D splits the run"
        );
    }
}

#[test]
fn layers_behind_the_camera_are_not_drawn() {
    // The default camera sits at z = -48*50/36 = -66.7.
    let behind = stack(&[(
        RED,
        r#", "three_d": true, "transform": { "position_z": "-100" }"#,
    )]);
    let near = stack(&[(
        RED,
        r#", "three_d": true, "transform": { "position_z": "-40", "rotation_x": "60" }"#,
    )]);
    for (an, gpu) in adapters() {
        assert!(render(gpu, &behind, 0).iter().all(|p| p[3] == 0.0), "{an}");
        let n = render(gpu, &near, 0);
        assert!(n.iter().all(|p| p.iter().all(|v| v.is_finite())), "{an}");
        assert!(
            n.iter().any(|p| p[3] > 0.0),
            "{an}: the part in front is drawn"
        );
    }
}

fn run(base: &Timeline, ops: &str) -> anyhow::Result<Timeline> {
    let ops = parse_ops(ops)?;
    apply(base, &ops, &mut MediaLengths::unbounded()).map(|(tl, _)| tl)
}

#[test]
fn validation_and_edit_ops() {
    let bad = |extra: &str, clip_extra: &str| {
        let text = serde_json::to_string(&tl("", "")).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let e: serde_json::Value = serde_json::from_str(&format!("{{{extra}}}")).unwrap();
        for (k, x) in e.as_object().unwrap() {
            v[k] = x.clone();
        }
        let c: serde_json::Value = serde_json::from_str(&format!("{{{clip_extra}}}")).unwrap();
        for (k, x) in c.as_object().unwrap() {
            v["tracks"][1]["clips"][0][k] = x.clone();
        }
        format!("{:#}", Timeline::from_json(&v.to_string()).unwrap_err())
    };
    assert!(bad("", r#""transform": {"rotation_y": "10"}"#).contains("three_d"));
    assert!(bad(r#""camera": {"zoom": "50", "fov_deg": "40"}"#, "").contains("not both"));
    assert!(bad(r#""camera": {"zoom": "0"}"#, "").contains("> 0"));
    assert!(bad(r#""motion_blur": {"samples": 1}"#, "").contains("samples"));
    assert!(bad(r#""motion_blur": {"shutter_angle": 800}"#, "").contains("shutter_angle"));

    let base = tl("", "");
    let t = run(
        &base,
        r#"[
          {"op": "set_param", "clip": "card", "param": "three_d", "value": true},
          {"op": "set_keyframes", "clip": "card", "param": "transform.rotation_y",
           "keyframes": [{"t": "0", "v": "0"}, {"t": "1", "v": "60"}]},
          {"op": "set_param", "clip": "card", "param": "transform.orientation.z", "value": "15"},
          {"op": "set_param", "param": "camera.position.x", "value": "10"},
          {"op": "set_keyframes", "param": "camera.zoom",
           "keyframes": [{"t": "0", "v": "66"}, {"t": "2", "v": "40"}]},
          {"op": "set_param", "param": "motion_blur", "value": {}},
          {"op": "set_param", "param": "motion_blur.samples", "value": "4"},
          {"op": "set_param", "clip": "card", "param": "motion_blur", "value": true}
        ]"#,
    )
    .unwrap();
    let c = &t.tracks[1].clips[0];
    assert!(c.three_d && c.motion_blur);
    let spec = c.transform.as_ref().unwrap();
    assert_eq!(spec.at_3d(RationalTime::new(1, 2))[6], 30.0);
    assert_eq!(spec.at_3d(RationalTime::ZERO)[4], 15.0);
    let cam = t
        .camera
        .as_ref()
        .unwrap()
        .at(RationalTime::ZERO, 48.0, 32.0);
    // One component set: the others come from the default camera.
    assert_eq!(cam.position, [10.0, 16.0, -48.0 * 50.0 / 36.0]);
    assert_eq!(cam.zoom, 66.0);
    assert_eq!(t.motion_blur.unwrap().samples, 4);
    assert_eq!(
        t.motion_blur.unwrap().shutter_angle,
        Rational::from_int(180)
    );
    // Round trip, and 2D timelines serialize without the new fields.
    let text = serde_json::to_string(&t).unwrap();
    Timeline::from_json(&text).unwrap();
    let flat = serde_json::to_string(&base).unwrap();
    for f in ["three_d", "camera", "motion_blur"] {
        assert!(!flat.contains(f), "{f}: {flat}");
    }
    // 3D fields without the switch are rejected by the op.
    let e = run(
        &base,
        r#"[{"op": "set_param", "clip": "card", "param": "transform.position_z", "value": "5"}]"#,
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("three_d"), "{e:#}");
}
