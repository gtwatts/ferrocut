//! Native grouped/repeated geometry, with analytic coverage and composition
//! references rather than only checking that expansion returns a list.

use effectcraft_geom::vec2;
use ferrocut_core::{Animatable, CpuFrame, Rational, RationalTime, RenderNode};
use ferrocut_engine::vector::VectorSpec;
use ferrocut_engine::vector_instances::{VectorGroup, VectorGroupNode};
use serde_json::{Value, json};

fn t(value: &str) -> RationalTime {
    RationalTime(value.parse::<Rational>().unwrap())
}
fn group(value: Value) -> VectorGroup {
    let group: VectorGroup = serde_json::from_value(value).unwrap();
    group.validate().unwrap();
    group
}
fn rectangle(x: i32, y: i32, width: i32, height: i32) -> Value {
    json!({"geometry":{"type":"rectangle","x":x,"y":y,"width":width,"height":height}})
}
fn single(shape: Value) -> Value {
    json!({"items":[{"type":"shape","shape":shape}]})
}
fn frame(value: Value, at: &str) -> CpuFrame {
    group(value).rasterize(t(at), 64, 64).unwrap()
}
fn pixel(frame: &CpuFrame, x: u32, y: u32) -> [f32; 4] {
    let offset = ((y * frame.width + x) * 4) as usize;
    std::array::from_fn(|i| frame.image.pixels[offset + i].to_f32())
}
fn area(frame: &CpuFrame) -> f64 {
    frame
        .image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3].to_f64())
        .sum()
}
fn close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected} within {tolerance}"
    );
}

#[test]
fn default_single_group_preserves_existing_shape_pixels() {
    let mut painted = rectangle(8, 8, 32, 24);
    painted["fill"] = json!({"type":"linear_gradient","start":[8,8],"end":[40,32],"stops":[{"offset":0,"color":[1,0,0,"1/3"]},{"offset":1,"color":[0,0,1,"3/4"]}]});
    painted["stroke"] = json!({"paint":{"type":"solid","color":[0,1,0,"1/2"]},"width":3,"join":"round","cap":"round","dashes":[3,2]});
    for value in [
        rectangle(4, 8, 18, 20),
        json!({"geometry":{"type":"ellipse","center":[32,32],"radius":[18,10]}}),
        json!({"geometry":{"type":"star","center":[32,32],"points":5,"inner_radius":10,"outer_radius":22},"operators":[{"type":"round_corners","radius":2}]}),
        painted,
    ] {
        let legacy: VectorSpec = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            frame(single(value), "0").image.pixels,
            legacy.rasterize(t("0"), 64, 64).unwrap().image.pixels
        );
    }
}

#[test]
fn fractional_copies_and_opacity_ramp_match_analytic_alpha() {
    let mut value = single(rectangle(4, 4, 8, 8));
    value["repeat"] =
        json!({"copies":"5/2","position":[12,0],"start_opacity":100,"end_opacity":50});
    value["transform"] = json!({"opacity":50});
    let rendered = frame(value, "0");
    assert_eq!(pixel(&rendered, 6, 6)[3], 0.5);
    assert_eq!(pixel(&rendered, 18, 6)[3], 0.375);
    assert_eq!(pixel(&rendered, 30, 6)[3], 0.125);
    assert_eq!(pixel(&rendered, 14, 6)[3], 0.0);
    close(area(&rendered), 64.0, 0.001);
}

#[test]
fn zero_and_partial_first_copies_and_empty_groups_are_defined() {
    let mut value = single(rectangle(4, 4, 8, 8));
    value["repeat"] = json!({"copies":0});
    assert_eq!(area(&frame(value.clone(), "0")), 0.0);
    value["repeat"] = json!({"copies":"1/4","start_opacity":80,"end_opacity":0});
    close(pixel(&frame(value, "0"), 6, 6)[3] as f64, 0.2, 0.0001);
    assert_eq!(area(&frame(json!({"items":[]}), "0")), 0.0);
    let mut collapsed = single(rectangle(4, 4, 8, 8));
    collapsed["transform"] = json!({"scale":[0,100]});
    assert_eq!(area(&frame(collapsed, "0")), 0.0);
}

#[test]
fn nested_transform_product_and_opacity_have_exact_area_reference() {
    let value = json!({"transform":{"position":[40,40],"rotation":90,"scale":[200,100],"opacity":50},
        "items":[{"type":"group","group":{"transform":{"position":[2,3],"scale":[100,50],"opacity":50},
            "items":[{"type":"shape","shape":rectangle(1,2,4,6)}]}}]});
    let spec = group(value.clone());
    let draws = spec.instances_at(t("0")).unwrap();
    assert_eq!(draws.len(), 1);
    let center = draws[0].transform.apply(vec2(3.0, 5.0));
    close(center.x, 34.5, 1e-12);
    close(center.y, 50.0, 1e-12);
    assert_eq!(draws[0].opacity, 0.25);
    let rendered = frame(value, "0");
    assert_eq!(pixel(&rendered, 34, 50)[3], 0.25);
    close(area(&rendered), 6.0, 0.05);
}

#[test]
fn transforming_one_subgroup_preserves_all_sibling_pixels() {
    let mut value = json!({"items":[
        {"type":"shape","shape":rectangle(4,4,8,8)},
        {"type":"group","group":{"items":[{"type":"shape","shape":rectangle(4,20,8,8)}],
            "transform":{"position":[20,0]}}}
    ]});
    let before = frame(value.clone(), "0");
    value["items"][1]["group"]["transform"]["position"] = json!([40, 0]);
    let after = frame(value, "0");
    for y in 0..64 {
        for x in 0..16 {
            assert_eq!(pixel(&before, x, y), pixel(&after, x, y));
        }
    }
    close(f64::from(pixel(&before, 28, 24)[3]), 1.0, 0.001);
    close(f64::from(pixel(&after, 28, 24)[3]), 0.0, 0.001);
    close(f64::from(pixel(&after, 48, 24)[3]), 1.0, 0.001);
}

#[test]
fn repeater_anchor_rotation_scale_and_negative_offset_transform_geometry() {
    let mut value = single(rectangle(4, 0, 4, 2));
    value["repeat"] = json!({"copies":2,"position":[32,32],"scale":[200,100],"rotation":90});
    let rendered = frame(value, "0");
    assert_eq!(pixel(&rendered, 6, 1)[3], 1.0);
    assert_eq!(pixel(&rendered, 31, 44)[3], 1.0);
    close(area(&rendered), 24.0, 0.01);

    let mut value = single(rectangle(8, 8, 8, 8));
    value["repeat"] =
        json!({"copies":1,"offset":-1,"anchor":[8,8],"position":[-10,0],"scale":[200,100]});
    let spec = group(value.clone());
    let draws = spec.instances_at(t("0")).unwrap();
    let anchor = draws[0].transform.apply(vec2(8.0, 8.0));
    close(anchor.x, 18.0, 1e-12);
    close(anchor.y, 8.0, 1e-12);
    close(area(&frame(value, "0")), 32.0, 0.01);
}

#[test]
fn above_and_below_copy_order_control_overlapping_gradient_colors() {
    let mut shape = rectangle(0, 8, 16, 16);
    shape["fill"] = json!({"type":"linear_gradient","start":[0,0],"end":[16,0],"interpolation":"linear","stops":[{"offset":0,"color":[1,0,0]},{"offset":1,"color":[0,0,1]}]});
    let mut value = single(shape);
    value["repeat"] = json!({"copies":2,"position":[8,0],"composite":"below"});
    let below = frame(value.clone(), "0");
    value["repeat"]["composite"] = json!("above");
    let above = frame(value, "0");
    let below = pixel(&below, 12, 12);
    let above = pixel(&above, 12, 12);
    assert_eq!(below[3], 1.0);
    assert_eq!(above[3], 1.0);
    assert!(
        above[0] > below[0] + 0.2,
        "above={above:?}, below={below:?}"
    );
    assert!(
        above[2] < below[2] - 0.2,
        "above={above:?}, below={below:?}"
    );
}

#[test]
fn item_order_and_group_opacity_use_linear_premultiplied_paint_composition() {
    let mut red = rectangle(8, 8, 16, 16);
    red["fill"] = json!({"type":"solid","color":[1,0,0]});
    let mut blue = red.clone();
    blue["fill"] = json!({"type":"solid","color":[0,0,1]});
    let value = json!({"transform":{"opacity":50},"items":[{"type":"shape","shape":red},{"type":"shape","shape":blue}]});
    let actual = frame(value, "0");
    let red = pixel(&frame(single(red), "0"), 12, 12);
    let blue = pixel(&frame(single(blue), "0"), 12, 12);
    let expected = [0, 1, 2, 3].map(|i| blue[i] * 0.5 + red[i] * 0.5 * 0.5);
    for (a, b) in pixel(&actual, 12, 12).into_iter().zip(expected) {
        close(a as f64, b as f64, 0.0006);
    }
    assert_eq!(pixel(&actual, 12, 12)[3], 0.75);
}

#[test]
fn radial_gradients_inverse_map_anisotropic_and_rotated_group_coordinates() {
    let mut shape = rectangle(0, 0, 16, 16);
    shape["fill"] = json!({"type":"radial_gradient","center":[8,8],"radius":8,"interpolation":"linear","stops":[{"offset":0,"color":[1,1,1]},{"offset":1,"color":[0,0,0]}]});
    let mut value = single(shape.clone());
    value["transform"] = json!({"position":[8,8],"scale":[200,100]});
    let rendered = frame(value, "0");
    for (x, y) in [(24, 16), (31, 16), (24, 19), (12, 10)] {
        let local = [((x as f64 + 0.5) - 8.0) / 2.0, (y as f64 + 0.5) - 8.0];
        let expected = (1.0 - ((local[0] - 8.0).hypot(local[1] - 8.0) / 8.0)).clamp(0.0, 1.0);
        let p = pixel(&rendered, x, y);
        assert_eq!(p[3], 1.0);
        for channel in &p[..3] {
            close(*channel as f64, expected, 0.001);
        }
    }
    let mut rotated = single(shape);
    rotated["transform"] = json!({"position":[40,8],"rotation":90,"scale":[200,100]});
    let rotated = frame(rotated, "0");
    for (x, y) in [(24, 16), (31, 16), (24, 19)] {
        // Rotate the already tested unrotated screen point about its offset.
        let mapped = (47 - y, x);
        for (a, b) in pixel(&rendered, x, y)
            .into_iter()
            .zip(pixel(&rotated, mapped.0, mapped.1))
        {
            close(a as f64, b as f64, 0.001);
        }
    }
}

#[test]
fn anisotropic_stroke_transforms_outline_instead_of_average_width() {
    let shape = json!({"geometry":{"type":"path","commands":[{"type":"move_to","point":[2,8]},{"type":"line_to","point":[10,8]}]},"fill":null,"stroke":{"paint":{"type":"solid","color":[1,1,1]},"width":4,"cap":"butt"}});
    let mut value = single(shape);
    value["transform"] = json!({"position":[8,8],"scale":[200,50]});
    let rendered = frame(value, "0");
    close(area(&rendered), 32.0, 0.01);
    assert_eq!(pixel(&rendered, 20, 11)[3], 1.0);
    assert_eq!(pixel(&rendered, 20, 10)[3], 0.0);
    assert_eq!(pixel(&rendered, 11, 12)[3], 0.0);
}

#[test]
fn animated_group_and_repeat_controls_sample_exact_source_time() {
    let mut value = single(rectangle(4, 4, 8, 8));
    value["transform"] = json!({"position":[{"keyframes":[{"t":0,"v":0},{"t":"1/3","v":6}]},0]});
    value["repeat"] =
        json!({"copies":{"keyframes":[{"t":0,"v":1},{"t":"1/3","v":"5/2"}]},"position":[12,0]});
    let spec = group(value.clone());
    assert!(spec.is_animated());
    let first = spec.rasterize(t("0"), 64, 64).unwrap();
    let last = spec.rasterize(t("1/3"), 64, 64).unwrap();
    assert_ne!(first.image.pixels, last.image.pixels);
    assert_eq!(pixel(&last, 12, 6)[3], 1.0);
    assert_eq!(pixel(&last, 36, 6)[3], 0.5);
    value["transform"]["position"] = json!([6, 0]);
    value["repeat"]["copies"] = json!("5/2");
    assert_eq!(last.image.pixels, frame(value, "99").image.pixels);
    let serialized = serde_json::to_string(&spec).unwrap();
    assert_eq!(
        serde_json::from_str::<VectorGroup>(&serialized).unwrap(),
        spec
    );
}

#[test]
fn animated_radial_copy_count_and_offset_match_independent_marker_positions() {
    let value = json!({"items":[{"type":"shape","shape":{
        "geometry":{"type":"ellipse","center":[20,0],"radius":[2,2]}
    }}],"transform":{"position":[32,32]},"repeat":{
        "copies":{"keyframes":[{"t":0,"v":1},{"t":1,"v":4}]},
        "offset":{"keyframes":[{"t":0,"v":0},{"t":1,"v":1}]},
        "position":[0,0],"rotation":90
    }});
    let start = frame(value.clone(), "0");
    close(f64::from(pixel(&start, 52, 32)[3]), 1.0, 0.001);
    close(f64::from(pixel(&start, 32, 52)[3]), 0.0, 0.001);
    let middle = frame(value.clone(), "1/2");
    close(f64::from(pixel(&middle, 46, 46)[3]), 1.0, 0.001);
    close(f64::from(pixel(&middle, 17, 17)[3]), 0.5, 0.002);
    close(f64::from(pixel(&middle, 52, 32)[3]), 0.0, 0.001);
    let end = frame(value, "1");
    for (x, y) in [(52, 32), (32, 52), (12, 32), (32, 12)] {
        close(f64::from(pixel(&end, x, y)[3]), 1.0, 0.001);
    }
}

#[test]
fn visitors_match_every_nested_component_and_allow_typed_leaf_edits() {
    let nested = json!({"transform":{"position":[1,2]},"repeat":{"copies":2,"position":[10,0]},"items":[{"type":"shape","shape":rectangle(4,4,8,8)}]});
    let mut spec = group(json!({"items":[{"type":"group","group":nested}]}));
    let immutable: Vec<_> = spec
        .animatables()
        .into_iter()
        .map(|(p, a)| (p, a.clone()))
        .collect();
    let mutable: Vec<_> = spec
        .animatables_mut()
        .into_iter()
        .map(|(p, a)| (p, a.clone()))
        .collect();
    assert_eq!(immutable, mutable);
    for path in [
        "transform.skew_axis",
        "items.0.group.transform.position.x",
        "items.0.group.repeat.end_opacity",
        "items.0.group.items.0.shape.geometry.width",
    ] {
        assert!(immutable.iter().any(|(p, _)| p == path), "missing {path}");
    }
    let before = spec.rasterize(t("0"), 64, 64).unwrap();
    for (path, a) in spec.all_mut() {
        if path == "items.0.group.repeat.position.x" {
            *a = Animatable::constant(Rational::from_int(20));
        }
    }
    assert_ne!(
        before.image.pixels,
        spec.rasterize(t("0"), 64, 64).unwrap().image.pixels
    );
}

#[test]
fn sampled_cache_hashes_reuse_held_values_and_include_intrinsic_wiggle() {
    let mut value = single(rectangle(8, 8, 12, 12));
    value["repeat"] = json!({"copies":{"keyframes":[{"t":0,"v":2,"interp":"hold"},{"t":1,"v":3}]},"position":[16,0]});
    let held = VectorGroupNode {
        group: group(value.clone()),
        width: 64,
        height: 64,
    };
    value["repeat"]["copies"] = json!(2);
    let constant = VectorGroupNode {
        group: group(value),
        width: 64,
        height: 64,
    };
    assert_eq!(held.content_hash_at(t("0")), held.content_hash_at(t("1/2")));
    assert_eq!(
        held.content_hash_at(t("0")),
        constant.content_hash_at(t("99"))
    );
    assert_ne!(held.content_hash(), constant.content_hash());
    assert_ne!(held.content_hash_at(t("0")), held.content_hash_at(t("1")));
    let mut value = single(rectangle(8, 8, 24, 24));
    value["items"][0]["shape"]["operators"] =
        json!([{"type":"wiggle","size":2,"detail":3,"seed":7}]);
    let wiggle = VectorGroupNode {
        group: group(value),
        width: 64,
        height: 64,
    };
    assert!(wiggle.group.is_animated());
    assert_ne!(
        wiggle.content_hash_at(t("0")),
        wiggle.content_hash_at(t("1"))
    );
    assert_ne!(
        wiggle.group.rasterize(t("0"), 64, 64).unwrap().image.pixels,
        wiggle.group.rasterize(t("1"), 64, 64).unwrap().image.pixels
    );
    assert!(wiggle.pulls(t("0")).is_empty());
    assert!(wiggle.supports_data_window());
}

#[test]
fn malformed_numeric_and_recursive_inputs_have_bounded_failures() {
    for extra in [
        json!({"transform":{"opacity":101}}),
        json!({"transform":{"skew":90}}),
        json!({"repeat":{"copies":129}}),
        json!({"repeat":{"copies":-1}}),
        json!({"repeat":{"scale":[0,100]}}),
        json!({"repeat":{"scale":[-100,100]}}),
        json!({"repeat":{"offset":129}}),
    ] {
        let mut value = single(rectangle(4, 4, 8, 8));
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let spec: VectorGroup = serde_json::from_value(value).unwrap();
        assert!(spec.validate().is_err());
    }
    for extra in [
        json!({"surprise":1}),
        json!({"transform":{"surprise":1}}),
        json!({"repeat":{"composite":"random"}}),
        json!({"repeat":{"surprise":1}}),
    ] {
        let mut value = single(rectangle(4, 4, 8, 8));
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(serde_json::from_value::<VectorGroup>(value).is_err());
    }
    let mut nested = single(rectangle(4, 4, 8, 8));
    for _ in 0..8 {
        nested = json!({"items":[{"type":"group","group":nested}]});
    }
    let spec: VectorGroup = serde_json::from_value(nested).unwrap();
    assert!(spec.validate().unwrap_err().contains("depth"));
    let spec: VectorGroup = serde_json::from_value(
        json!({"items":vec![json!({"type":"shape","shape":rectangle(4,4,8,8)});128]}),
    )
    .unwrap();
    assert!(spec.validate().unwrap_err().contains("nodes"));
}

#[test]
fn multiplicative_expansion_pixel_work_and_transform_growth_are_rejected() {
    let mut inner = single(rectangle(4, 4, 8, 8));
    inner["repeat"] = json!({"copies":128,"position":[0,0]});
    let spec = group(
        json!({"repeat":{"copies":128,"position":[0,0]},"items":[{"type":"group","group":inner}]}),
    );
    assert!(
        spec.rasterize(t("0"), u32::MAX, u32::MAX)
            .unwrap_err()
            .contains("instances")
    );
    let spec = group(
        json!({"repeat":{"copies":128,"position":[0,0]},"items":vec![json!({"type":"shape","shape":rectangle(4,4,8,8)});4]}),
    );
    assert!(
        spec.rasterize(t("0"), 1024, 1024)
            .unwrap_err()
            .contains("pixel work")
    );
    let mut value = single(rectangle(4, 4, 8, 8));
    value["repeat"] = json!({"copies":128,"position":[0,0],"scale":[1000,1000]});
    assert!(
        group(value)
            .rasterize(t("0"), 64, 64)
            .unwrap_err()
            .contains("transform")
    );
    let mut value = single(rectangle(1_000_001, 4, 8, 8));
    assert!(
        group(value.clone())
            .rasterize(t("0"), 64, 64)
            .unwrap_err()
            .contains("coordinates")
    );
    value["items"][0]["shape"]["geometry"]["x"] = json!(4);
    value["repeat"] = json!({"copies":{"expression":"129","value":129}});
    assert!(
        group(value)
            .rasterize(t("0"), 64, 64)
            .unwrap_err()
            .contains("copies")
    );
}

#[test]
fn transformed_stroke_geometry_has_a_shared_per_frame_budget() {
    let mut commands = vec![json!({"type":"move_to","point":[1,1]})];
    for i in 1..1000 {
        commands.push(json!({"type":"line_to","point":[1+i%30,1+(i/30)%30]}));
    }
    let spec = group(single(
        json!({"geometry":{"type":"path","commands":commands},"stroke":{"paint":{"type":"solid","color":[1,1,1]},"width":1,"join":"bevel"}}),
    ));
    let mut value = serde_json::to_value(spec).unwrap();
    value["repeat"] = json!({"copies":128,"position":[0,0]});
    let error = group(value).rasterize(t("0"), 1, 1).unwrap_err();
    assert!(error.contains("transformed geometry"), "{error}");
}

#[test]
fn node_upload_preserves_alpha_and_honors_cancellation_and_deadline() {
    use ferrocut_core::{
        AdapterPreference, CancelToken, ErrorKind, GpuContext, RenderCtx, WorkerState,
    };
    use std::time::Instant;
    for preference in [AdapterPreference::default(), AdapterPreference::Cpu] {
        let gpu = match GpuContext::new(preference.clone()) {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("SKIP vector group upload {preference:?}: {e}");
                continue;
            }
        };
        eprintln!(
            "vector group upload adapter ({preference:?}): {:?}",
            gpu.adapter.get_info()
        );
        let mut value = single(rectangle(4, 4, 8, 8));
        value["repeat"] =
            json!({"copies":"5/2","position":[12,0],"start_opacity":50,"end_opacity":25});
        let node = VectorGroupNode {
            group: group(value),
            width: 64,
            height: 64,
        };
        let mut worker = WorkerState::default();
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
        assert_eq!(
            node.render(&mut ctx, t("0"), &[]).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        let cancel = CancelToken::new();
        let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, Some(Instant::now()));
        assert_eq!(
            node.render(&mut ctx, t("0"), &[]).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
        let actual = node
            .render(&mut ctx, t("0"), &[])
            .unwrap()
            .to_cpu_frame(&gpu)
            .unwrap();
        assert_eq!(
            actual.image.pixels,
            node.group.rasterize(t("0"), 64, 64).unwrap().image.pixels
        );
        assert_eq!(pixel(&actual, 6, 6)[3], 0.5);
        assert_eq!(pixel(&actual, 30, 6)[3], 0.125);
    }
}

#[test]
fn native_group_controls_split_trim_retime_and_cache_preserve_source_animation() {
    use ferrocut_core::{AdapterPreference, CancelToken, GpuContext, RenderCtx, WorkerState};
    use ferrocut_engine::Timeline;
    use ferrocut_engine::compile::compile;
    use ferrocut_engine::compositor::{Compositor, compositor_slot};
    use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
    use ferrocut_engine::generator::GeneratorSpec;
    use ferrocut_engine::graph::FrameCache;
    use std::sync::Arc;

    let timeline=Timeline::from_json(&json!({"output":{"width":64,"height":64,"fps":24,"gop":12},
        "tracks":[{"clips":[{"id":"group","start":0,"source_in":"1/2","duration":2,
        "generator":{"type":"vector_group","group":{
            "transform":{"position":[{"expression":"time * 4"},0]},
            "repeat":{"copies":{"expression":"value + time / 4"},"position":[12,0]},
            "items":[{"type":"group","group":{
                "transform":{"position":[0,{"keyframes":[{"t":0,"v":0},{"t":3,"v":6}]}]},
                "items":[{"type":"shape","shape":{
                    "geometry":{"type":"rectangle","x":4,"y":8,"width":8,"height":12},
                    "operators":[{"type":"trim","end":{"expression":"value - time * 10"}},{"type":"wiggle","size":1,"detail":2,"seed":9}],
                    "fill":{"type":"solid","color":[1,1,1,"1/3"]}
                }}]
            }}]
        }}}]}]}).to_string()).unwrap();
    let edit = |ops: Value| {
        apply(
            &timeline,
            &parse_ops(&ops.to_string()).unwrap(),
            &mut MediaLengths::unbounded(),
        )
        .unwrap()
        .0
    };
    let compiled = compile(&timeline).unwrap();
    let split = compile(&edit(json!([{"op":"split","clip":"group","at":1}]))).unwrap();
    let trimmed = compile(&edit(
        json!([{"op":"trim","clip":"group","edge":"in","delta":"1/2"}]),
    ))
    .unwrap();
    for frame in 0..48 {
        let at = RationalTime::from_frames(frame, timeline.output.fps);
        assert_eq!(
            compiled.graph.frame_key(compiled.output, at),
            split.graph.frame_key(split.output, at),
            "group split frame {frame}"
        );
        if frame >= 12 {
            assert_eq!(
                compiled.graph.frame_key(compiled.output, at),
                trimmed.graph.frame_key(trimmed.output, at),
                "group in-trim frame {frame}"
            );
        }
    }
    let revised_timeline = edit(
        json!([{"op":"set_param","clip":"group","param":"generator.group.items.0.group.items.0.shape.geometry.width","value":4}]),
    );
    let revised = compile(&revised_timeline).unwrap();
    let keyed_timeline = edit(
        json!([{"op":"set_keyframes","clip":"group","param":"generator.group.items.0.group.transform.position.y","timeline_time":true,"keyframes":[{"t":0,"v":0},{"t":2,"v":8}]}]),
    );
    let GeneratorSpec::VectorGroup { group: keyed_group } = keyed_timeline.tracks[0].clips[0]
        .generator
        .as_ref()
        .unwrap()
    else {
        panic!("group")
    };
    let keyed = keyed_group
        .all()
        .into_iter()
        .find(|(p, _)| p == "items.0.group.transform.position.y")
        .unwrap()
        .1;
    assert_eq!(keyed.eval(t("1/2")), 0.0);
    assert_eq!(keyed.eval(t("5/2")), 8.0);
    let invalid=apply(&timeline,&parse_ops(&json!([
        {"op":"set_param","clip":"group","param":"generator.group.repeat.copies","value":2},
        {"op":"set_param","clip":"group","param":"generator.group.items.99.group.transform.position.x","value":1}
    ]).to_string()).unwrap(),&mut MediaLengths::unbounded());
    assert!(invalid.is_err());
    assert!(
        timeline.tracks[0].clips[0]
            .generator
            .as_ref()
            .unwrap()
            .is_animated()
    );

    let sped = compile(&edit(json!([{"op":"set_speed","clip":"group","speed":2}]))).unwrap();
    let frozen = compile(&edit(
        json!([{"op":"set_param","clip":"group","param":"speed","value":0}]),
    ))
    .unwrap();
    let remapped=compile(&edit(json!([{"op":"set_param","clip":"group","param":"time_remap","value":{"keyframes":[{"t":0,"v":"1/2"},{"t":1,"v":"5/2"}]}}]))).unwrap();
    let baked = ferrocut_engine::expr::bake(&timeline).unwrap();
    let GeneratorSpec::VectorGroup { group: source } =
        baked.tracks[0].clips[0].generator.as_ref().unwrap()
    else {
        panic!("group")
    };
    assert_eq!(
        source
            .all()
            .into_iter()
            .find(|(p, _)| p == "repeat.copies")
            .unwrap()
            .1
            .eval(t("1/2")),
        3.125,
        "expression value must use the repeat default of 3"
    );
    assert_eq!(
        source
            .all()
            .into_iter()
            .find(|(p, _)| p == "items.0.group.items.0.shape.operators.0.end")
            .unwrap()
            .1
            .eval(t("1/2")),
        95.0,
        "nested shape expression must use trim's default of 100"
    );
    let gpu = match GpuContext::new(AdapterPreference::Cpu) {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("SKIP group timeline GPU comparison: {e}");
            return;
        }
    };
    eprintln!(
        "vector group timeline adapter: {:?}",
        gpu.adapter.get_info()
    );
    let mut worker = WorkerState::default();
    let compositor = Arc::new(Compositor::new(&gpu));
    worker
        .slot(compositor_slot(), || Ok(compositor.clone()))
        .unwrap();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
    let mut cache = FrameCache::new(128);
    let render = |graph: &ferrocut_engine::compile::Compiled,
                  at: RationalTime,
                  ctx: &mut RenderCtx<'_>,
                  cache: &mut FrameCache| {
        let result = graph.graph.evaluate(graph.output, at, ctx, cache).unwrap();
        ctx.flush();
        result.to_cpu_frame(&gpu).unwrap()
    };
    for at in ["0", "1/2", "5/4"] {
        let actual = render(&compiled, t(at), &mut ctx, &mut cache);
        let direct = source.rasterize(t(at) + t("1/2"), 64, 64).unwrap();
        for (a, b) in actual.image.pixels.iter().zip(&direct.image.pixels) {
            close(a.to_f64(), b.to_f64(), 0.001);
        }
        let misses = cache.misses;
        assert_eq!(
            actual.image.pixels,
            render(&compiled, t(at), &mut ctx, &mut cache).image.pixels
        );
        assert_eq!(cache.misses, misses, "group repeated render missed cache");
        assert_eq!(
            actual.image.pixels,
            render(&split, t(at), &mut ctx, &mut cache).image.pixels
        );
        if t(at) >= t("1/2") {
            assert_eq!(
                actual.image.pixels,
                render(&trimmed, t(at), &mut ctx, &mut cache).image.pixels
            );
        }
    }
    assert_ne!(
        render(&compiled, t("0"), &mut ctx, &mut cache).image.pixels,
        render(&revised, t("0"), &mut ctx, &mut cache).image.pixels,
        "nested width edit reused stale output"
    );
    for (fast, normal) in [("0", "0"), ("1/4", "1/2"), ("3/4", "3/2")] {
        let reference = render(&compiled, t(normal), &mut ctx, &mut cache);
        assert_eq!(
            reference.image.pixels,
            render(&sped, t(fast), &mut ctx, &mut cache).image.pixels,
            "group speed at {fast}"
        );
        assert_eq!(
            reference.image.pixels,
            render(&remapped, t(fast), &mut ctx, &mut cache)
                .image
                .pixels,
            "group remap at {fast}"
        );
    }
    assert_eq!(
        render(&frozen, t("0"), &mut ctx, &mut cache).image.pixels,
        render(&frozen, t("3/2"), &mut ctx, &mut cache).image.pixels
    );
}
