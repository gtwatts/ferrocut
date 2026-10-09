//! Public synthetic regression for geometry lost before the clip transform.
//! CPU checks use signed source coordinates; graph checks skip without an adapter.

use std::sync::Arc;

use ferrocut_core::{
    AdapterPreference, CancelToken, CpuFrame, GpuContext, PixelRect, RationalTime, RenderCtx,
    WorkerState,
};
use ferrocut_engine::{
    Timeline,
    compile::{Compiled, compile},
    compositor::{Compositor, compositor_slot},
    edit::parse_ops,
    graph::FrameCache,
    project::{self, EditOptions},
    vector::{VectorNode, VectorSpec},
    vector_instances::VectorGroup,
};
use serde_json::{Value, json};

fn pixel(f: &CpuFrame, x: i32, y: i32) -> [f32; 4] {
    let w = f.data_window;
    if x < w.x || y < w.y || i64::from(x) >= w.right() || i64::from(y) >= w.bottom() {
        return [0.0; 4];
    }
    let i = ((y - w.y) as usize * w.width as usize + (x - w.x) as usize) * 4;
    std::array::from_fn(|k| f.image.pixels[i + k].to_f32())
}

fn shape(value: Value) -> VectorSpec {
    serde_json::from_value(value).unwrap()
}

fn rect(x: i32, y: i32, width: i32, height: i32) -> Value {
    json!({"geometry":{"type":"rectangle","x":x,"y":y,"width":width,"height":height}})
}

#[test]
fn source_bounds_preserve_pixels_on_every_side_without_resizing_the_display() {
    for (x, y) in [(-12, 4), (28, 4), (4, -12), (4, 28)] {
        let s = shape(rect(x, y, 8, 8));
        let f = s.rasterize_source(RationalTime::ZERO, 24, 24).unwrap();
        assert_eq!((f.width, f.height), (24, 24));
        assert_eq!(pixel(&f, x + 2, y + 2)[3], 1.0);
        assert_eq!(pixel(&f, x - 1, y + 2)[3], 0.0);
        // Viewport helpers deliberately remain clipped and indexed by display width.
        let viewport = s.rasterize(RationalTime::ZERO, 24, 24).unwrap();
        assert_eq!(viewport.data_window, PixelRect::full(24, 24));
        assert_eq!(pixel(&viewport, x + 2, y + 2)[3], 0.0);
    }
    let s = shape(rect(4, 4, 8, 8));
    let source = s.rasterize_source(RationalTime::ZERO, 24, 24).unwrap();
    let viewport = s.rasterize(RationalTime::ZERO, 24, 24).unwrap();
    assert_eq!(source.data_window, viewport.data_window);
    assert_eq!(source.image.pixels, viewport.image.pixels);
    let empty = shape(rect(-12, -12, 0, 0))
        .rasterize_source(RationalTime::ZERO, 24, 24)
        .unwrap();
    assert_eq!(empty.data_window, PixelRect::full(24, 24));
    assert!(empty.image.pixels.iter().all(|v| v.to_f32() == 0.0));
    assert!(
        shape(rect(-1_000_000, -1_000_000, 1, 1))
            .rasterize_source(RationalTime::ZERO, 24, 24)
            .is_err()
    );
}

#[test]
fn stroke_outline_and_gradient_keep_source_coordinates_in_negative_storage() {
    let make = |dx: i32| {
        shape(json!({
            "geometry":{"type":"path","commands":[
                {"type":"move_to","point":[dx-12,8]},
                {"type":"line_to","point":[dx-4,8]}]},
            "fill":null,
            "stroke":{"width":6,"cap":"square","paint":{
                "type":"linear_gradient","start":[dx-16,8],"end":[dx,8],
                "stops":[{"offset":0,"color":[1,0,0,"1/2"]},
                         {"offset":1,"color":[0,0,1,"1/2"]}]}}
        }))
    };
    let source = make(0)
        .rasterize_source(RationalTime::ZERO, 24, 24)
        .unwrap();
    let reference = make(16).rasterize(RationalTime::ZERO, 24, 24).unwrap();
    assert_eq!(pixel(&source, -14, 6)[3], 0.5, "square cap was cropped");
    for y in 0..24 {
        for x in -16..8 {
            assert_eq!(pixel(&source, x, y), pixel(&reference, x + 16, y));
        }
    }
}

#[test]
fn miter_and_subpixel_curve_fringe_match_an_uncropped_viewport_reference() {
    let make = |dx, dy, curve| {
        if curve {
            shape(json!({"geometry":{"type":"ellipse",
                "center":[format!("{}/2", dx * 2 - 3), format!("{}/2",dy * 2 - 5)],
                "radius":["9/4","7/4"]}}))
        } else {
            shape(json!({"geometry":{"type":"path","commands":[
                {"type":"move_to","point":[dx-14,dy-2]},
                {"type":"line_to","point":[dx-8,dy+12]},
                {"type":"line_to","point":[dx-2,dy-2]}]},
                "fill":null,"stroke":{"width":6,"join":"miter","miter_limit":8,
                    "paint":{"type":"solid","color":[1,1,1,1]}}}))
        }
    };
    for curve in [false, true] {
        let source = make(0, 0, curve)
            .rasterize_source(RationalTime::ZERO, 32, 24)
            .unwrap();
        let reference = make(32, 16, curve)
            .rasterize(RationalTime::ZERO, 64, 48)
            .unwrap();
        assert!(source.data_window.x < 0 && source.data_window.y < 0);
        for y in -16..32 {
            for x in -32..32 {
                assert_eq!(
                    pixel(&source, x, y),
                    pixel(&reference, x + 32, y + 16),
                    "fringe mismatch at {x},{y}; curve={curve}"
                );
            }
        }
    }
}

#[test]
fn repeated_group_retains_post_group_bounds_and_local_gradient_samples() {
    let make = |dx| -> VectorGroup {
        serde_json::from_value(json!({
            "transform":{"position":[dx,0]},
            "repeat":{"copies":2,"position":[-12,0]},
            "items":[{"type":"shape","shape":{
                "geometry":{"type":"rectangle","x":2,"y":4,"width":8,"height":8},
                "fill":{"type":"linear_gradient","start":[2,4],"end":[10,4],
                    "stops":[{"offset":0,"color":[1,0,0,"1/2"]},
                             {"offset":1,"color":[0,0,1,"1/2"]}]}
            }}]
        }))
        .unwrap()
    };
    let source = make(0)
        .rasterize_source(RationalTime::ZERO, 32, 24)
        .unwrap();
    let reference = make(16).rasterize(RationalTime::ZERO, 48, 24).unwrap();
    assert_eq!((source.width, source.height), (32, 24));
    assert_eq!(pixel(&source, -8, 6)[3], 0.5);
    for y in 0..24 {
        for x in -12..32 {
            assert_eq!(pixel(&source, x, y), pixel(&reference, x + 16, y));
        }
    }
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../examples/vector-source-bounds.json")).unwrap()
}

fn gpu() -> Option<GpuContext> {
    GpuContext::new(AdapterPreference::Cpu)
        .map_err(|e| eprintln!("SKIP vector source-bounds GPU check: {e}"))
        .ok()
}

fn evaluate(gpu: &GpuContext, graph: &Compiled, t: RationalTime) -> CpuFrame {
    let mut worker = WorkerState::default();
    let compositor = Arc::new(Compositor::new(gpu));
    worker.slot(compositor_slot(), || Ok(compositor)).unwrap();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
    let frame = graph
        .graph
        .evaluate(graph.output, t, &mut ctx, &mut FrameCache::new(8))
        .unwrap();
    ctx.flush();
    frame.to_cpu_frame(gpu).unwrap()
}

#[test]
fn translated_and_rotated_negative_shape_reaches_all_expected_output_pixels() {
    let Some(gpu) = gpu() else { return };
    let mut value = fixture();
    value["tracks"].as_array_mut().unwrap().remove(0);
    for clip in value["tracks"][0]["clips"].as_array_mut().unwrap() {
        clip["generator"]["shape"]["fill"]["color"] = json!([1, 1, 1, "1/2"]);
    }
    let timeline = Timeline::from_json(&value.to_string()).unwrap();
    let graph = compile(&timeline).unwrap();
    for (frame, rotated) in [(0, false), (23, false), (24, true), (47, true)] {
        let actual = evaluate(
            &gpu,
            &graph,
            RationalTime::from_frames(frame, timeline.output.fps),
        );
        assert_eq!((actual.width, actual.height), (128, 96));
        for y in 0..96 {
            for x in 0..128 {
                let inside = if rotated {
                    ((96..104).contains(&x) && (32..64).contains(&y))
                        || ((72..96).contains(&x) && (32..44).contains(&y))
                } else {
                    ((16..48).contains(&x) && (16..24).contains(&y))
                        || ((16..28).contains(&x) && (24..48).contains(&y))
                };
                let expected = if inside { 0.5 } else { 0.0 };
                for channel in pixel(&actual, x, y) {
                    assert!(
                        (channel - expected).abs() < 0.001,
                        "frame {frame} pixel ({x},{y}): {channel} != {expected}"
                    );
                }
            }
        }
    }
}

#[test]
fn oversized_source_is_rejected_before_gpu_upload() {
    use ferrocut_core::RenderNode;
    let Some(gpu) = gpu() else { return };
    let limit = gpu.device.limits().max_texture_dimension_2d;
    let node = VectorNode {
        spec: shape(rect(-(limit as i32), 0, 1, 1)),
        width: 1,
        height: 1,
    };
    let mut worker = WorkerState::default();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
    let error = node.render(&mut ctx, RationalTime::ZERO, &[]).unwrap_err();
    assert!(
        error.message.contains("texture dimension limit"),
        "{error:?}"
    );
}

#[test]
fn journaled_geometry_and_transform_edits_rekey_only_the_edited_shot_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    let timeline = Timeline::from_json(&fixture().to_string()).unwrap();
    std::fs::write(&path, project::timeline_text(&timeline).unwrap()).unwrap();
    let before_bytes = std::fs::read(&path).unwrap();
    let before = compile(&timeline).unwrap();
    let ops = parse_ops(&json!([
        {"op":"set_param","clip":"translated","param":"transform.position.x","value":132},
        {"op":"set_param","clip":"translated","param":"generator.shape.geometry.commands.0.point.x","value":-50}
    ]).to_string()).unwrap();
    project::edit_file(
        &path,
        &ops,
        &EditOptions {
            plan: true,
            ..Default::default()
        },
    )
    .unwrap();
    let after = compile(&Timeline::load(&path).unwrap()).unwrap();
    for frame in 0..48 {
        let t = RationalTime::from_frames(frame, timeline.output.fps);
        assert_eq!(
            before.graph.frame_key(before.output, t) != after.graph.frame_key(after.output, t),
            frame < 24,
            "unexpected dirty frame {frame}"
        );
    }
    project::undo(&path, false).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before_bytes);
    let restored = compile(&Timeline::load(&path).unwrap()).unwrap();
    assert_eq!(
        before.graph.frame_key(before.output, RationalTime::ZERO),
        restored
            .graph
            .frame_key(restored.output, RationalTime::ZERO)
    );
}

#[test]
fn negative_source_mask_and_adjacent_matte_keep_the_recovered_geometry() {
    let Some(gpu) = gpu() else { return };
    let mut value = fixture();
    value["tracks"][0]["matte"] = json!({"mode":"alpha"});
    value["tracks"][0]["clips"][0]["generator"]["color"] = json!([1, 1, 1, 1]);
    value["tracks"][1]["clips"][0]["masks"] = json!([
        {"geometry":{"type":"rectangle","x":-48,"y":16,"width":32,"height":8}}
    ]);
    let timeline = Timeline::from_json(&value.to_string()).unwrap();
    let frame = evaluate(&gpu, &compile(&timeline).unwrap(), RationalTime::ZERO);
    assert_eq!(pixel(&frame, 20, 20)[3], 1.0);
    assert_eq!(
        pixel(&frame, 20, 32)[3],
        0.0,
        "source mask did not cut the lower leg"
    );
    assert_eq!(
        pixel(&frame, 60, 20)[3],
        0.0,
        "matte source leaked outside geometry"
    );
}

#[test]
fn nested_fit_and_default_anchor_use_inner_display_not_extended_storage() {
    let Some(gpu) = gpu() else { return };
    let dir = tempfile::tempdir().unwrap();
    let inner = json!({"output":{"width":64,"height":48,"fps":24,"duration":1,"gop":12},
        "tracks":[{"clips":[{"id":"shape","start":0,"duration":1,
            "generator":{"type":"shape","shape":rect(-8,8,8,8)},
            "transform":{"position":[48,24]}}]}]});
    let path = dir.path().join("inner.json");
    std::fs::write(&path, inner.to_string()).unwrap();
    let outer = Timeline::from_json(
        &json!({
            "output":{"width":128,"height":96,"fps":24,"duration":1,"gop":12},
            "tracks":[{"clips":[{"id":"nested","source":path,"start":0,"duration":1}]}]
        })
        .to_string(),
    )
    .unwrap();
    let graph = compile(&outer).unwrap();
    assert_eq!(graph.placements[0].native, (64, 48));
    assert_eq!(graph.placements[0].output, (128, 96));
    let frame = evaluate(&gpu, &graph, RationalTime::ZERO);
    assert_eq!((frame.width, frame.height), (128, 96));
    assert!((pixel(&frame, 24, 24)[3] - 1.0).abs() < 0.001);
    assert_eq!(pixel(&frame, 40, 24)[3], 0.0);
    assert_eq!(pixel(&frame, 24, 40)[3], 0.0);
}
