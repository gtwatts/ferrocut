//! Source controls for the opt-in planar scene. GPU runs require an adapter;
//! a SKIP is not pixel acceptance. No encoded-output/installed-route claim here.
use std::sync::{Arc, OnceLock};

use ferrocut_core::{
    AdapterPreference, CancelToken, ColorSpace, CpuFrame, Frame, FrameStorage, GpuContext,
    PixelRect, Rational, RationalTime, RenderCtx, RenderNode, WorkerState,
};
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::depth::{SceneClip, SceneNode, vertices};
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::graph::FrameCache;
use ferrocut_engine::layer3d::{CameraSpec, MotionBlurSpec};
use ferrocut_engine::nodes::ClipRange;
use ferrocut_engine::placement::{Fit, Placement};
use ferrocut_engine::{Timeline, compile};
use half::f16;
use serde_json::{Value, json};

const W: u32 = 160;
const H: u32 = 96;

fn doc() -> Value {
    json!({"output":{"width":W,"height":H,"fps":30,"gop":15,"duration":2},
      "renderer":"depth_layers_v1", "tracks":[
        {"name":"Red", "clips":[{"id":"r","start":0,"duration":1,"three_d":true,
          "generator":{"type":"solid","color":[1,0,0,"1/2"]}}]},
        {"name":"Blue", "clips":[{"id":"b","start":0,"duration":2,"three_d":true,
          "generator":{"type":"solid","color":[0,0,1,"3/4"]}}]}]})
}
fn parse(v: Value) -> Timeline {
    Timeline::from_json(&v.to_string()).unwrap()
}

#[test]
fn rejects_incompatible_scene_combinations_before_render() {
    let mut cases = Vec::new();
    let mut v = doc();
    v["camera"] = json!({"position":[0,0,0],"point_of_interest":[0,0,0]});
    cases.push(v);
    let mut v = doc();
    v["camera"] = json!({"position":[0,0,-600],"point_of_interest":[0,0,0],"reference_up":[0,0,1]});
    cases.push(v);
    let mut v = doc();
    v["camera"] = json!({"near":2,"far":1});
    cases.push(v);
    let mut v = doc();
    v["tracks"][0]["clips"][0]["blend_mode"] = json!("screen");
    cases.push(v);
    let mut v = doc();
    v["tracks"][1]["matte"] = json!({"mode":"alpha","source":{"track":"Red"}});
    cases.push(v);
    let mut v = doc();
    let c = v["tracks"][0]["clips"][0].clone();
    v["tracks"][0]["clips"].as_array_mut().unwrap().push(c);
    v["tracks"][0]["clips"][1]["id"] = json!("overlap");
    cases.push(v);
    let mut v = doc();
    v["motion_blur"] = json!({"samples":2});
    v["tracks"][0]["clips"][0]["motion_blur"] = json!(true);
    cases.push(v);
    let mut v = doc();
    v["renderer"] = json!("legacy");
    v["camera"] = json!({"roll":1});
    cases.push(v);
    for v in cases {
        assert!(Timeline::from_json(&v.to_string()).is_err(), "{v}");
    }
    let mut many = doc();
    many["tracks"] = json!([]);
    for i in 0..17 {
        let mut track = doc()["tracks"][0].clone();
        track["name"] = json!(format!("T{i}"));
        track["clips"][0]["id"] = json!(format!("C{i}"));
        many["tracks"].as_array_mut().unwrap().push(track);
    }
    assert!(
        Timeline::from_json(&many.to_string())
            .unwrap_err()
            .to_string()
            .contains("16")
    );
    let interpolated: CameraSpec = serde_json::from_value(json!({
        "position":[0,0,0],"point_of_interest":[{"keyframes":[{"t":0,"v":1},{"t":2,"v":-1}]},0,0]
    }))
    .unwrap();
    assert!(
        interpolated
            .depth_at(RationalTime::ZERO, 160.0, 96.0)
            .is_ok()
    );
    assert!(
        interpolated
            .depth_at(RationalTime::new(2, 1), 160.0, 96.0)
            .is_ok()
    );
    assert!(
        interpolated
            .depth_at(RationalTime::new(1, 1), 160.0, 96.0)
            .unwrap_err()
            .contains("must differ")
    );
}

#[test]
fn legacy_keys_and_idle_ranges_preserved_and_depth_edits_invalidate() {
    let mut v = doc();
    v.as_object_mut().unwrap().remove("renderer");
    let a = compile(&parse(v.clone())).unwrap();
    v["renderer"] = json!("legacy");
    let b = compile(&parse(v)).unwrap();
    for frame in [0, 15, 30, 59] {
        let t = RationalTime::from_frames(frame, Rational::from_int(30));
        assert_eq!(
            a.graph.frame_key(a.output, t),
            b.graph.frame_key(b.output, t)
        );
    }
    assert_eq!(a.extra_gpu_bytes_per_job, 0);
    let a = compile(&parse(doc())).unwrap();
    let mut v = doc();
    v["tracks"][0]["clips"][0]["transform"] = json!({"position_z":5});
    let b = compile(&parse(v)).unwrap();
    assert_ne!(
        a.graph.frame_key(a.output, RationalTime::ZERO),
        b.graph.frame_key(b.output, RationalTime::ZERO)
    );
    let t = RationalTime::new(3, 2);
    assert_eq!(
        a.graph.frame_key(a.output, t),
        b.graph.frame_key(b.output, t)
    );
    assert!(a.extra_gpu_bytes_per_job >= u64::from(W) * u64::from(H) * 48);
    let mut v = doc();
    v["camera"] = json!({"roll":15});
    let c = compile(&parse(v)).unwrap();
    assert_ne!(
        a.graph.frame_key(a.output, RationalTime::ZERO),
        c.graph.frame_key(c.output, RationalTime::ZERO)
    );
}

#[test]
fn hidden_consumed_matte_tracks_do_not_reset_scene_budget() {
    let mut v = doc();
    v["tracks"] = json!([]);
    for i in 0..17 {
        if i == 8 {
            v["tracks"].as_array_mut().unwrap().extend([
                json!({"name":"hidden recipient","visible":false,"matte":{"mode":"alpha"},"clips":[]}),
                json!({"name":"consumed stencil","clips":[]}),
            ]);
        }
        let mut track = doc()["tracks"][0].clone();
        track["name"] = json!(format!("T{i}"));
        track["clips"][0]["id"] = json!(format!("C{i}"));
        v["tracks"].as_array_mut().unwrap().push(track);
    }
    assert!(
        Timeline::from_json(&v.to_string())
            .unwrap_err()
            .to_string()
            .contains("17 authored surfaces")
    );
    // A genuinely visible 2D recipient delimits the two smaller scenes.
    v["tracks"][8]["visible"] = json!(true);
    assert!(Timeline::from_json(&v.to_string()).is_ok());
}

fn clip(id: &str, angle: i64) -> SceneClip {
    SceneClip {
        id: id.into(),
        range: ClipRange {
            start: RationalTime::ZERO,
            end: RationalTime::new(2, 1),
            dissolve_in: None,
        },
        spec: serde_json::from_value(json!({"scale":2,"rotation_y":angle})).unwrap(),
        placement: Placement {
            native: (W, H),
            output: (W, H),
            fit: Fit::Native,
        },
        motion_blur: false,
    }
}
fn scene(clips: Vec<SceneClip>) -> SceneNode {
    SceneNode {
        clips,
        camera: serde_json::from_value(
            json!({"zoom":600,"position":[80,48,-600],"point_of_interest":[80,48,0]}),
        )
        .unwrap(),
        blur: None,
        fps: Rational::from_int(30),
        width: W,
        height: H,
    }
}

#[test]
fn exact_shutter_visibility_and_edge_content_policy() {
    let mut a = clip("ending", 0);
    a.range.end = RationalTime::new(1, 1);
    let mut b = clip("entering", 0);
    b.range.start = RationalTime::new(1, 1);
    let mut n = scene(vec![a, b]);
    n.blur = Some(MotionBlurSpec {
        samples: 2,
        ..Default::default()
    });
    let t = RationalTime::new(1, 1);
    let times = n.times(t);
    let pulls = n.pulls(t);
    assert_eq!(
        times,
        vec![RationalTime::new(239, 240), RationalTime::new(241, 240)]
    );
    assert_eq!(pulls.len(), 2);
    assert_eq!(pulls[0].time, times[0]);
    assert_eq!(pulls[1].time, t);
    n.blur = None;
    assert_eq!(n.pulls(t).len(), 1);
    assert_eq!(n.pulls(t)[0].input, 1);
}

fn image(color: [f32; 4]) -> Frame {
    Frame::from_cpu(&CpuFrame::new(
        W,
        H,
        ColorSpace::acescg(),
        (0..W * H).flat_map(|_| color.map(f16::from_f32)).collect(),
    ))
}

#[test]
fn signed_storage_window_and_non_square_pixels_project_without_reanchoring() {
    let mut f = image([1.0; 4]);
    f.data_window = PixelRect::new(-20, -10, W, H);
    f.pixel_aspect = Rational::from_int(2);
    let mut c = clip("overscan", 0);
    c.spec = Default::default();
    let cam = CameraSpec::default()
        .depth_at(RationalTime::ZERO, f64::from(W) * 2.0, f64::from(H))
        .unwrap();
    let vs = vertices(&c, &f, RationalTime::ZERO, &cam).unwrap().unwrap();
    let v = vs[0].clip_position;
    let p = [
        (f64::from(v[0] / v[3]) + 1.0) * f64::from(W) / 2.0,
        (1.0 - f64::from(v[1] / v[3])) * f64::from(H) / 2.0,
    ];
    assert!(
        (p[0] + 20.0).abs() < 1e-4 && (p[1] + 10.0).abs() < 1e-4,
        "{p:?}"
    );
    c.spec = serde_json::from_value(json!({"scale":[0,1]})).unwrap();
    assert!(
        vertices(&c, &f, RationalTime::ZERO, &cam)
            .unwrap()
            .is_none()
    );
}

fn adapter() -> Option<&'static GpuContext> {
    static GPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    GPU.get_or_init(|| match GpuContext::new(AdapterPreference::Cpu) {
        Ok(g) => {
            eprintln!("depth GPU control adapter: {:?}", g.info);
            Some(g)
        }
        Err(e) => {
            eprintln!("SKIP: no CPU adapter for depth control: {e}");
            None
        }
    })
    .as_ref()
}
fn draw(gpu: &GpuContext, node: &SceneNode, frames: &[Arc<Frame>]) -> Vec<[f32; 4]> {
    let mut worker = WorkerState::default();
    worker
        .slot(compositor_slot(), || Ok(Arc::new(Compositor::new(gpu))))
        .unwrap();
    let cancel = CancelToken::new();
    let scope = gpu.error_scope();
    let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
    let f = node.render(&mut ctx, RationalTime::ZERO, frames).unwrap();
    ctx.flush();
    assert!(scope.finish().is_none());
    let f = f.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(px) = f.storage else {
        unreachable!()
    };
    px.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p.map(f16::to_f32))
        .collect()
}
fn close(a: [f32; 4], b: [f32; 4]) {
    for i in 0..4 {
        assert!((a[i] - b[i]).abs() < 0.002, "{a:?} != {b:?}");
    }
}

#[test]
fn gpu_crossing_fractional_alpha_reorder_ties_transparent_and_near_clip() {
    let Some(gpu) = adapter() else { return };
    let alpha = 2.0_f32.powi(-20);
    let tiny = draw(
        gpu,
        &scene(vec![clip("tiny", 0)]),
        &[Arc::new(image([alpha, 0.0, 0.0, alpha]))],
    );
    assert_eq!(
        tiny[(48 * W + 80) as usize],
        [alpha, 0.0, 0.0, alpha],
        "positive subnormal alpha must not be discarded"
    );
    let red = Arc::new(image([0.5, 0.0, 0.0, 0.5]));
    let blue = Arc::new(image([0.0, 0.0, 0.75, 0.75]));
    // y rotation -30 => z increases with x: red nearer at screen left.
    // Independent interior oracle: front + (1-front.a)*back, no background.
    let n = scene(vec![clip("red", -30), clip("blue", 30)]);
    let px = draw(gpu, &n, &[red.clone(), blue.clone()]);
    close(px[(48 * W + 40) as usize], [0.5, 0.0, 0.375, 0.875]);
    close(px[(48 * W + 120) as usize], [0.125, 0.0, 0.75, 0.875]);
    let reversed = scene(vec![clip("blue", 30), clip("red", -30)]);
    let other = draw(gpu, &reversed, &[blue.clone(), red.clone()]);
    for x in [40, 120] {
        assert_eq!(px[(48 * W + x) as usize], other[(48 * W + x) as usize]);
    }
    let mut ties = scene(vec![clip("red", 0), clip("blue", 0)]);
    let px = draw(gpu, &ties, &[red.clone(), blue.clone()]);
    close(px[(48 * W + 80) as usize], [0.125, 0.0, 0.75, 0.875]);
    let zero = Arc::new(image([0.0; 4]));
    let px = draw(gpu, &ties, &[red.clone(), zero]);
    close(px[(48 * W + 80) as usize], [0.5, 0.0, 0.0, 0.5]);
    // A real represented near separation overrides authoring priority.
    ties.clips[0].spec = serde_json::from_value(json!({"scale":2,"position_z":"-1/8"})).unwrap();
    let px = draw(gpu, &ties, &[red.clone(), blue.clone()]);
    close(px[(48 * W + 80) as usize], [0.5, 0.0, 0.375, 0.875]);
    ties.camera.near = Some(Rational::from_int(601));
    let px = draw(gpu, &ties, &[red, blue]);
    assert!(px.iter().all(|v| *v == [0.0; 4]));

    // A slanted plane straddles the near plane at camera z=600. The left
    // side is clipped, not the whole card, and the farther right survives.
    let mut partial = scene(vec![clip("straddling", -30)]);
    partial.camera.near = Some(Rational::from_int(600));
    let px = draw(gpu, &partial, &[Arc::new(image([1.0, 0.0, 0.0, 1.0]))]);
    assert_eq!(px[(48 * W + 40) as usize], [0.0; 4]);
    close(px[(48 * W + 120) as usize], [1.0, 0.0, 0.0, 1.0]);
}

#[test]
fn scene_memory_estimate_lowers_jobs_without_changing_legacy_sizing() {
    let free = 1851_u64 << 20;
    let old = ferrocut_engine::vram::jobs_for(free, 1920, 1080, 12);
    let new = ferrocut_engine::vram::jobs_for_with_extra(free, 1920, 1080, 12, 1920 * 1080 * 48);
    assert_eq!(old, 8);
    assert!(new < old);
}

#[test]
fn gpu_shutter_resolves_visibility_before_averaging_and_preserves_storage_origin() {
    let Some(gpu) = adapter() else { return };
    let mut n = scene(vec![clip("moving", 0), clip("fixed", 0)]);
    for c in &mut n.clips {
        c.range.start = RationalTime::new(-1, 1);
    }
    n.clips[0].spec = serde_json::from_value(json!({"scale":2,"position_z":{"keyframes":[
        {"t":"239/240","v":-1},{"t":"241/240","v":1}
    ]}}))
    .unwrap();
    n.blur = Some(MotionBlurSpec {
        samples: 2,
        ..Default::default()
    });
    let px = draw(
        gpu,
        &n,
        &[
            Arc::new(image([0.5, 0.0, 0.0, 0.5])),
            Arc::new(image([0.0, 0.0, 0.75, 0.75])),
        ],
    );
    // Red front at the first sample, blue front at the second; averaging
    // each card first then sorting once cannot produce this interior value.
    close(px[(48 * W + 80) as usize], [0.3125, 0.0, 0.5625, 0.875]);

    let mut f = image([1.0; 4]);
    f.data_window = PixelRect::new(-20, -10, W, H);
    f.pixel_aspect = Rational::from_int(2);
    let mut n = scene(vec![clip("storage", 0)]);
    n.clips[0].spec = Default::default();
    n.camera = Default::default();
    let px = draw(gpu, &n, &[Arc::new(f)]);
    close(px[(40 * W + 120) as usize], [1.0; 4]);
    assert_eq!(px[(40 * W + 145) as usize], [0.0; 4]);

    let mut two = image([1.0; 4]);
    two.pixel_aspect = Rational::from_int(2);
    let mut worker = WorkerState::default();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
    let err = scene(vec![clip("square", 0), clip("wide", 0)])
        .render(
            &mut ctx,
            RationalTime::ZERO,
            &[Arc::new(image([1.0; 4])), Arc::new(two)],
        )
        .unwrap_err();
    assert!(err.to_string().contains("common positive pixel aspect"));
}

#[test]
fn nested_scene_texture_keeps_exact_source_in_speed_and_rejects_lossy_unnest() {
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("inner.json");
    let mut v = doc();
    v["output"]["duration"] = json!(3);
    v["tracks"] = json!([{"name":"steps","clips":(0..3).map(|i| json!({
        "id":format!("step{i}"),"start":i,"duration":1,"three_d":true,
        "generator":{"type":"solid","color":[1,1,1,format!("{}/4",i+1)]}
    })).collect::<Vec<_>>()}]);
    std::fs::write(&inner, serde_json::to_vec(&v).unwrap()).unwrap();
    let tl = parse(
        json!({"output":{"width":W,"height":H,"fps":30,"duration":3},
        "renderer":"depth_layers_v1","tracks":[{"name":"card","clips":[{
            "id":"nested","source":inner,"three_d":true,"start":1,"duration":1,
            "source_in":"1/2","speed":2
        }]}]}),
    );
    let ops = parse_ops(r#"[{"op":"set_param","clip":"nested","param":"speed","value":1},{"op":"unnest","clip":"nested"}]"#).unwrap();
    assert!(
        format!(
            "{:#}",
            apply(&tl, &ops, &mut MediaLengths::unbounded()).unwrap_err()
        )
        .contains("keep this composition nested")
    );
    let c = compile(&tl).unwrap();
    assert_eq!(
        (0..c.graph.len())
            .filter(|&i| c.graph.node(i).kind() == "depth_scene")
            .count(),
        2
    );
    let Some(gpu) = adapter() else { return };
    let mut worker = WorkerState::default();
    worker
        .slot(compositor_slot(), || Ok(Arc::new(Compositor::new(gpu))))
        .unwrap();
    let cancel = CancelToken::new();
    let mut cache = FrameCache::new(32);
    for (t, alpha) in [
        (RationalTime::new(1, 1), 0.25),
        (RationalTime::new(5, 4), 0.5),
        (RationalTime::new(7, 4), 0.75),
    ] {
        let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
        let scope = gpu.error_scope();
        let f = c.graph.evaluate(c.output, t, &mut ctx, &mut cache).unwrap();
        ctx.flush();
        assert!(scope.finish().is_none());
        let f = f.to_cpu(gpu).unwrap();
        let FrameStorage::Cpu(px) = f.storage else {
            unreachable!()
        };
        assert!((px.pixels[((48 * W + 80) * 4 + 3) as usize].to_f32() - alpha).abs() < 0.002);
    }
}
