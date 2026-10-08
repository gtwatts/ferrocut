//! Actual EffectCraft path internals through Ferrocut's native editable source.
//! Geometric references below use analytic area/coverage and source-clock
//! equivalence, rather than asserting only that an operator returned a path.

use ferrocut_core::{Animatable, CpuFrame, Rational, RationalTime, RenderNode};
use ferrocut_engine::vector::{VectorNode, VectorSpec};
use serde_json::{Value, json};

fn t(s: &str) -> RationalTime {
    RationalTime(s.parse::<Rational>().unwrap())
}
fn shape(v: Value) -> VectorSpec {
    let spec: VectorSpec = serde_json::from_value(v).unwrap();
    spec.validate().unwrap();
    spec
}
fn frame(v: Value, at: &str) -> CpuFrame {
    shape(v).rasterize(t(at), 64, 64).unwrap()
}
fn alpha(f: &CpuFrame, x: u32, y: u32) -> f32 {
    f.image.pixels[((y * f.width + x) * 4 + 3) as usize].to_f32()
}
fn area(f: &CpuFrame) -> f64 {
    f.image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3].to_f64())
        .sum()
}
fn rect(operators: Value) -> Value {
    json!({"geometry":{"type":"rectangle","x":16,"y":16,"width":16,"height":16},"operators":operators})
}
fn line(operators: Value) -> Value {
    json!({"geometry":{"type":"path","commands":[{"type":"move_to","point":[8,32]},{"type":"line_to","point":[56,32]}]},"fill":null,"stroke":{"paint":{"type":"solid","color":[1,1,1]},"width":2},"operators":operators})
}
fn two_rects(mode: &str) -> Value {
    json!({"geometry":{"type":"path","commands":[
        {"type":"move_to","point":[8,8]},{"type":"line_to","point":[28,8]},
        {"type":"line_to","point":[28,28]},{"type":"line_to","point":[8,28]},{"type":"close"},
        {"type":"move_to","point":[18,8]},{"type":"line_to","point":[38,8]},
        {"type":"line_to","point":[38,28]},{"type":"line_to","point":[18,28]},{"type":"close"}
    ]},"operators":[{"type":"merge","mode":mode}]})
}

#[test]
fn polygon_and_star_have_analytic_area_and_editable_animated_radii() {
    let polygon = json!({"geometry":{"type":"polygon","center":[32,32],"points":5,"radius":18}});
    let expected = 5.0 / 2.0 * 18.0_f64.powi(2) * (2.0 * std::f64::consts::PI / 5.0).sin();
    assert!((area(&frame(polygon, "0")) - expected).abs() < 3.0);
    let star = json!({"geometry":{"type":"star","center":[32,32],"points":5,"inner_radius":8,"outer_radius":{"keyframes":[{"t":0,"v":12},{"t":1,"v":18}]}}});
    let spec = shape(star.clone());
    assert!(spec.is_animated());
    for (at, radius) in [("0", 12.0), ("1", 18.0)] {
        let rendered = frame(star.clone(), at);
        let expected = 5.0 * radius * 8.0 * (std::f64::consts::PI / 5.0).sin();
        assert!((area(&rendered) - expected).abs() < 3.0, "star at {at}");
        assert_eq!(alpha(&rendered, 32, 32), 1.0);
        assert_eq!(alpha(&rendered, 2, 2), 0.0);
    }
    let mut rounded = star;
    rounded["geometry"]["inner_roundness"] = json!(70);
    rounded["geometry"]["outer_roundness"] = json!(70);
    assert_ne!(
        frame(rounded, "1").image.pixels,
        spec.rasterize(t("1"), 64, 64).unwrap().image.pixels
    );
}

#[test]
fn omitted_operators_preserve_original_pixels_and_serialization() {
    for mut v in [
        rect(json!([])),
        line(json!([])),
        json!({"geometry":{"type":"ellipse","center":[32,32],"radius":[12,8]},"operators":[]}),
    ] {
        let empty = shape(v.clone());
        v.as_object_mut().unwrap().remove("operators");
        let omitted = shape(v);
        assert_eq!(empty, omitted);
        assert!(
            serde_json::to_value(&empty)
                .unwrap()
                .get("operators")
                .is_none()
        );
        assert_eq!(
            empty.rasterize(t("0"), 64, 64).unwrap().image.pixels,
            omitted.rasterize(t("0"), 64, 64).unwrap().image.pixels
        );
    }
    let original = frame(rect(json!([])), "0");
    for ops in [
        json!([{"type":"trim"}]),
        json!([{"type":"round_corners","radius":0}]),
        json!([{"type":"pucker_bloat","amount":0}]),
        json!([{"type":"twist","angle":0}]),
        json!([{"type":"wiggle","size":0}]),
        json!([{"type":"reverse"},{"type":"reverse"}]),
    ] {
        assert_eq!(frame(rect(ops), "0").image.pixels, original.image.pixels);
    }
}

#[test]
fn trim_is_arc_length_based_animated_and_wraps_offset() {
    let value = line(json!([{"type":"trim","end":{"keyframes":[{"t":0,"v":25},{"t":1,"v":75}]}}]));
    let first = frame(value.clone(), "0");
    let last = frame(value, "1");
    assert_eq!(area(&first), 24.0);
    assert_eq!(area(&last), 72.0);
    assert_eq!(alpha(&first, 10, 32), 1.0);
    assert_eq!(alpha(&first, 21, 32), 0.0);
    assert_eq!(alpha(&last, 42, 32), 1.0);
    let wrapped = frame(
        line(json!([{"type":"trim","start":0,"end":50,"offset":270}])),
        "0",
    );
    assert_eq!(alpha(&wrapped, 10, 32), 1.0);
    assert_eq!(alpha(&wrapped, 32, 32), 0.0);
    assert_eq!(alpha(&wrapped, 50, 32), 1.0);
    assert_eq!(
        area(&frame(
            line(json!([{"type":"trim","start":50,"end":50}])),
            "0"
        )),
        0.0
    );
}

#[test]
fn simultaneous_and_individual_trim_respect_contour_order() {
    let mut value = json!({"geometry":{"type":"path","commands":[
        {"type":"move_to","point":[8,16]},{"type":"line_to","point":[40,16]},
        {"type":"move_to","point":[8,32]},{"type":"line_to","point":[40,32]}
    ]},"fill":null,"stroke":{"paint":{"type":"solid","color":[1,1,1]},"width":2},"operators":[{"type":"trim","end":50}]});
    let simultaneous = frame(value.clone(), "0");
    assert_eq!(alpha(&simultaneous, 12, 16), 1.0);
    assert_eq!(alpha(&simultaneous, 12, 32), 1.0);
    assert_eq!(alpha(&simultaneous, 32, 16), 0.0);
    value["operators"][0]["mode"] = json!("individual");
    let individual = frame(value, "0");
    assert_eq!(alpha(&individual, 32, 16), 1.0);
    assert_eq!(alpha(&individual, 12, 32), 0.0);
    assert_eq!(area(&individual), area(&simultaneous));
}

#[test]
fn round_corners_and_offset_have_geometric_coverage_references() {
    let rounded = frame(rect(json!([{"type":"round_corners","radius":4}])), "0");
    let expected = 256.0 - (4.0 - std::f64::consts::PI) * 16.0;
    assert!(
        (area(&rounded) - expected).abs() < 2.0,
        "{} != {expected}",
        area(&rounded)
    );
    assert_eq!(alpha(&rounded, 16, 16), 0.0);
    assert_eq!(alpha(&rounded, 24, 24), 1.0);
    for (amount, expected) in [(4, 576.0), (-4, 64.0)] {
        let expanded = frame(rect(json!([{"type":"offset","amount":amount}])), "0");
        assert!(
            (area(&expanded) - expected).abs() < 0.2,
            "offset {amount}: {}",
            area(&expanded)
        );
        assert_eq!(alpha(&expanded, 24, 24), 1.0);
    }
    let copies = frame(rect(json!([{"type":"offset","amount":4,"copies":2}])), "0");
    assert!((area(&copies) - 1024.0).abs() < 0.2);
    let vanished = frame(rect(json!([{"type":"offset","amount":-20}])), "0");
    assert_eq!(area(&vanished), 0.0);
}

#[test]
fn merge_boolean_modes_match_analytic_overlapping_rectangles() {
    for (mode, expected, left, overlap, right) in [
        ("merge", 600.0, 1.0, 1.0, 1.0),
        ("add", 600.0, 1.0, 1.0, 1.0),
        ("subtract", 200.0, 1.0, 0.0, 0.0),
        ("intersect", 200.0, 0.0, 1.0, 0.0),
        ("exclude", 400.0, 1.0, 0.0, 1.0),
    ] {
        let f = frame(two_rects(mode), "0");
        assert!((area(&f) - expected).abs() < 0.2, "{mode}: {}", area(&f));
        assert_eq!(alpha(&f, 12, 16), left, "{mode}");
        assert_eq!(alpha(&f, 22, 16), overlap, "{mode}");
        assert_eq!(alpha(&f, 34, 16), right, "{mode}");
    }
}

#[test]
fn boolean_empty_operands_keep_subtraction_and_intersection_identity() {
    for (mode, expected) in [("subtract", 0.0), ("intersect", 0.0), ("add", 400.0)] {
        let mut v = two_rects(mode);
        // The first contour is erased, while the second is retained. Its role
        // as the first boolean operand must survive the upstream empty filter.
        v["operators"].as_array_mut().unwrap().insert(
            0,
            json!({"type":"trim","start":50,"end":100,"mode":"individual"}),
        );
        assert!((area(&frame(v, "0")) - expected).abs() < 0.2, "{mode}");
    }
}

#[test]
fn offset_outlines_open_paths_and_preserves_native_fractional_alpha() {
    let mut v = line(json!([{"type":"offset","amount":4,"join":"round"}]));
    v["fill"] = json!({"type":"solid","color":[1,0,0,"1/2"]});
    v["stroke"] = Value::Null;
    let f = frame(v, "0");
    assert!((area(&f) - 192.0).abs() < 0.2);
    assert_eq!(alpha(&f, 32, 32), 0.5);
    assert_eq!(alpha(&f, 32, 27), 0.0);
    assert_eq!(alpha(&f, 7, 32), 0.0);
}

#[test]
fn pucker_bloat_zigzag_twist_and_operator_order_change_native_geometry() {
    let original = frame(rect(json!([])), "0");
    let bloated = frame(rect(json!([{"type":"pucker_bloat","amount":50}])), "0");
    assert_ne!(bloated.image.pixels, original.image.pixels);
    assert!(area(&bloated) > 100.0);
    assert_eq!(alpha(&bloated, 16, 16), 0.0);
    let zigzag = frame(line(json!([{"type":"zigzag","size":4,"ridges":7}])), "0");
    assert!(alpha(&zigzag, 14, 28) > 0.0);
    assert_eq!(alpha(&zigzag, 14, 32), 0.0);
    let smooth = frame(
        line(json!([{"type":"zigzag","size":4,"ridges":7,"smooth":true}])),
        "0",
    );
    assert_ne!(smooth.image.pixels, zigzag.image.pixels);
    let twisted = frame(
        rect(json!([{"type":"twist","angle":90,"center":[24,24]}])),
        "0",
    );
    assert_ne!(twisted.image.pixels, original.image.pixels);
    assert_eq!(alpha(&twisted, 24, 24), 1.0);
    let round_then_trim = frame(
        rect(json!([{"type":"round_corners","radius":6},{"type":"trim","end":25}])),
        "0",
    );
    let trim_then_round = frame(
        rect(json!([{"type":"trim","end":25},{"type":"round_corners","radius":6}])),
        "0",
    );
    assert_ne!(round_then_trim.image.pixels, trim_then_round.image.pixels);
}

#[test]
fn wiggle_uses_source_time_seed_and_intrinsic_cache_discrimination() {
    let value = rect(json!([{"type":"wiggle","size":3,"detail":4,"seed":72,"speed":2}]));
    let spec = shape(value.clone());
    assert!(spec.is_animated());
    let node = VectorNode {
        spec: spec.clone(),
        width: 64,
        height: 64,
    };
    let a = spec.rasterize(t("0"), 64, 64).unwrap();
    let b = spec.rasterize(t("1/3"), 64, 64).unwrap();
    assert_eq!(
        a.image.pixels,
        spec.rasterize(t("0"), 64, 64).unwrap().image.pixels
    );
    assert_ne!(a.image.pixels, b.image.pixels);
    assert_ne!(node.content_hash_at(t("0")), node.content_hash_at(t("1/3")));
    let mut changed = value.clone();
    changed["operators"][0]["seed"] = json!(73);
    assert_ne!(a.image.pixels, frame(changed, "0").image.pixels);
    let mut paused = value.clone();
    paused["operators"][0]["speed"] = json!(0);
    let paused = VectorNode {
        spec: shape(paused),
        width: 64,
        height: 64,
    };
    assert!(!paused.spec.is_animated());
    assert_eq!(
        paused.content_hash_at(t("0")),
        paused.content_hash_at(t("1/3"))
    );
    let mut phased = value;
    phased["operators"][0]["phase"] = json!(240);
    let phased = frame(phased, "0");
    // 2 wiggles/s * 1/3 s = 240 degrees of phase.
    assert_eq!(b.image.pixels, phased.image.pixels);
}

#[test]
fn each_numeric_operator_samples_animation_and_held_values_reuse_keys() {
    for (kind, field, final_value) in [
        ("round_corners", "radius", 6),
        ("offset", "amount", 4),
        ("pucker_bloat", "amount", 50),
        ("zigzag", "size", 4),
        ("twist", "angle", 90),
    ] {
        let mut op = json!({"type":kind});
        op[field] = json!({"keyframes":[{"t":0,"v":0},{"t":1,"v":final_value}]});
        if kind == "twist" {
            op["center"] = json!([24, 24]);
        }
        let spec = shape(rect(json!([op])));
        assert!(spec.is_animated(), "{kind}");
        assert_ne!(
            spec.rasterize(t("0"), 64, 64).unwrap().image.pixels,
            spec.rasterize(t("1"), 64, 64).unwrap().image.pixels,
            "{kind} did not sample animation"
        );
    }
    let held = shape(line(
        json!([{"type":"trim","end":{"keyframes":[{"t":0,"v":25,"interp":"hold"},{"t":1,"v":50}]}}]),
    ));
    let node = VectorNode {
        spec: held,
        width: 64,
        height: 64,
    };
    let constant = VectorNode {
        spec: shape(line(json!([{"type":"trim","end":25}]))),
        width: 64,
        height: 64,
    };
    assert_eq!(node.content_hash_at(t("0")), node.content_hash_at(t("1/2")));
    assert_eq!(
        node.content_hash_at(t("0")),
        constant.content_hash_at(t("0"))
    );
    assert_ne!(node.content_hash_at(t("0")), node.content_hash_at(t("1")));
}

#[test]
fn mutable_numeric_visitor_matches_expression_and_edit_paths() {
    let mut spec = shape(
        json!({"geometry":{"type":"star","center":[32,32],"points":5,"inner_radius":8,"outer_radius":18},"operators":[
        {"type":"trim","end":75},{"type":"round_corners","radius":2},
        {"type":"offset","amount":1},{"type":"pucker_bloat","amount":10},
        {"type":"zigzag","size":1},{"type":"twist","angle":2,"center":[32,32]},
        {"type":"wiggle","size":1},{"type":"reverse"},{"type":"merge"}
    ],"fill":{"type":"linear_gradient","start":[0,0],"end":[64,0],"stops":[{"offset":0,"color":[1,0,0]},{"offset":1,"color":[0,0,1,1]}]},
        "stroke":{"paint":{"type":"radial_gradient","center":[32,32],"radius":16,"stops":[{"offset":0,"color":[1,1,1]},{"offset":1,"color":[0,0,0]}]},"width":2,"dashes":[2,3]}}),
    );
    let before: Vec<_> = spec
        .animatables()
        .into_iter()
        .map(|(p, a)| (p, a.clone()))
        .collect();
    let after: Vec<_> = spec
        .animatables_mut()
        .into_iter()
        .map(|(p, a)| (p, a.clone()))
        .collect();
    assert_eq!(before, after);
    assert!(before.iter().any(|(p, _)| p == "operators.6.correlation"));
    assert!(before.iter().any(|(p, _)| p == "geometry.outer_roundness"));
    for (path, value) in spec.all_mut() {
        if path == "operators.0.end" {
            *value = Animatable::constant(Rational::from_int(50));
        }
    }
    assert_eq!(
        spec.all()
            .iter()
            .find(|(p, _)| p == "operators.0.end")
            .unwrap()
            .1
            .eval(t("0")),
        50.0
    );
}

#[test]
fn invalid_parameters_topology_and_expansion_are_rejected_before_expensive_work() {
    for operators in [
        json!([{"type":"trim","end":101}]),
        json!([{"type":"offset","amount":1,"copies":17}]),
        json!([{"type":"wiggle","size":1,"detail":129}]),
        json!([{"type":"pucker_bloat","amount":101}]),
        json!([{"type":"twist","angle":36001}]),
        json!([{"type":"round_corners","radius":-1}]),
        json!(vec![json!({"type":"reverse"}); 33]),
    ] {
        let value = rect(operators);
        let parsed = serde_json::from_value::<VectorSpec>(value);
        assert!(parsed.is_err() || parsed.unwrap().validate().is_err());
    }
    for op in [
        json!({"type":"trim","surprise":1}),
        json!({"type":"repeater"}),
        json!({"type":"merge","mode":"xor"}),
    ] {
        assert!(serde_json::from_value::<VectorSpec>(rect(json!([op]))).is_err());
    }
    for count in [2, 257, 2147483647] {
        let parsed: VectorSpec = serde_json::from_value(
            json!({"geometry":{"type":"polygon","center":[32,32],"points":count,"radius":18}}),
        )
        .unwrap();
        assert!(parsed.validate().unwrap_err().contains("points"));
    }
    let expanded = shape(rect(
        json!([{"type":"zigzag","size":2,"ridges":128},{"type":"twist","angle":360}]),
    ));
    assert!(
        expanded
            .rasterize(t("0"), 64, 64)
            .unwrap_err()
            .contains("complexity")
    );
    let huge = shape(
        json!({"geometry":{"type":"rectangle","x":1000001,"width":10,"height":10},"operators":[{"type":"reverse"}]}),
    );
    assert!(
        huge.rasterize(t("0"), 64, 64)
            .unwrap_err()
            .contains("coordinates")
    );
    let wiggle = shape(rect(json!([{"type":"wiggle","size":1}])));
    assert!(
        wiggle
            .rasterize(t("1000000000000"), 64, 64)
            .unwrap_err()
            .contains("source clock")
    );
    let complex = shape(
        json!({"geometry":{"type":"star","center":[32,32],"points":256,"inner_radius":8,"outer_radius":18},"operators":[{"type":"offset","amount":1}]}),
    );
    assert!(
        complex
            .rasterize(t("0"), 64, 64)
            .unwrap_err()
            .contains("512-element")
    );
    let unsafe_sample = shape(rect(
        json!([{"type":"zigzag","size":1,"ridges":{"expression":"129","value":129}}]),
    ));
    assert!(
        unsafe_sample
            .rasterize(t("0"), 64, 64)
            .unwrap_err()
            .contains("ridges")
    );
    let zero_wiggle = shape(rect(json!([{"type":"wiggle","size":0}])));
    assert!(zero_wiggle.rasterize(t("1000000000000"), 64, 64).is_ok());
}

#[test]
fn animated_operators_survive_timeline_edits_split_retime_and_cache_reuse() {
    use ferrocut_core::{AdapterPreference, CancelToken, GpuContext, RenderCtx, WorkerState};
    use ferrocut_engine::Timeline;
    use ferrocut_engine::compile::compile;
    use ferrocut_engine::compositor::{Compositor, compositor_slot};
    use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
    use ferrocut_engine::generator::GeneratorSpec;
    use ferrocut_engine::graph::FrameCache;
    use std::sync::Arc;

    let timeline=Timeline::from_json(&json!({
        "output":{"width":64,"height":64,"fps":24,"gop":12},
        "tracks":[{"clips":[{"id":"shape","start":0,"source_in":"1/2","duration":2,
            "generator":{"type":"shape","shape":{
                "geometry":{"type":"star","center":[32,32],"points":5,"inner_radius":10,"outer_radius":22},
                "operators":[{"type":"trim","end":{"expression":"25 + time * 25"}},
                    {"type":"wiggle","size":2,"detail":3,"speed":1,"seed":12}],
                "fill":{"type":"solid","color":[1,1,1,"1/2"]}
            }}}]}]
    }).to_string()).unwrap();
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
    let split = compile(&edit(json!([{"op":"split","clip":"shape","at":1}]))).unwrap();
    for f in 0..48 {
        let at = RationalTime::from_frames(f, timeline.output.fps);
        assert_eq!(
            compiled.graph.frame_key(compiled.output, at),
            split.graph.frame_key(split.output, at),
            "operator split frame {f}"
        );
    }
    let edited=compile(&edit(json!([{"op":"set_param","clip":"shape","param":"generator.shape.operators.0.end","value":50}]))).unwrap();
    assert_ne!(
        compiled.graph.frame_key(compiled.output, t("0")),
        edited.graph.frame_key(edited.output, t("0"))
    );
    let sped = compile(&edit(json!([{"op":"set_speed","clip":"shape","speed":2}]))).unwrap();
    let frozen = compile(&edit(
        json!([{"op":"set_param","clip":"shape","param":"speed","value":0}]),
    ))
    .unwrap();
    let remapped=compile(&edit(json!([{"op":"set_param","clip":"shape","param":"time_remap","value":{"keyframes":[{"t":0,"v":"1/2"},{"t":1,"v":"5/2"}]}}]))).unwrap();
    let baked = ferrocut_engine::expr::bake(&timeline).unwrap();
    let GeneratorSpec::Shape { shape: source } =
        baked.tracks[0].clips[0].generator.as_ref().unwrap()
    else {
        panic!("shape");
    };
    assert_eq!(
        source
            .all()
            .iter()
            .find(|(p, _)| p == "operators.0.end")
            .unwrap()
            .1
            .eval(t("1/2")),
        37.5
    );

    let gpu = match GpuContext::new(AdapterPreference::Cpu) {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("SKIP software GPU operator timeline test: {e}");
            return;
        }
    };
    eprintln!("operator timeline adapter: {:?}", gpu.adapter.get_info());
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
            assert!((a.to_f32() - b.to_f32()).abs() < 0.001);
        }
        let misses = cache.misses;
        let repeated = render(&compiled, t(at), &mut ctx, &mut cache);
        assert_eq!(actual.image.pixels, repeated.image.pixels);
        assert_eq!(
            cache.misses, misses,
            "unchanged operator frame missed cache"
        );
        let split_frame = render(&split, t(at), &mut ctx, &mut cache);
        assert_eq!(actual.image.pixels, split_frame.image.pixels);
    }
    let original = render(&compiled, t("0"), &mut ctx, &mut cache);
    let revised = render(&edited, t("0"), &mut ctx, &mut cache);
    assert_ne!(
        original.image.pixels, revised.image.pixels,
        "edit reused stale shape"
    );
    for (fast, normal) in [("0", "0"), ("1/4", "1/2"), ("3/4", "3/2")] {
        let reference = render(&compiled, t(normal), &mut ctx, &mut cache);
        assert_eq!(
            reference.image.pixels,
            render(&sped, t(fast), &mut ctx, &mut cache).image.pixels,
            "retimed native operator {fast}"
        );
        assert_eq!(
            reference.image.pixels,
            render(&remapped, t(fast), &mut ctx, &mut cache)
                .image
                .pixels,
            "time-remapped native operator {fast}"
        );
    }
    assert_eq!(
        render(&frozen, t("0"), &mut ctx, &mut cache).image.pixels,
        render(&frozen, t("3/2"), &mut ctx, &mut cache).image.pixels
    );
}

#[test]
fn vector_node_honors_cancellation_deadlines_and_preserves_gpu_alpha() {
    use ferrocut_core::{
        AdapterPreference, CancelToken, ErrorKind, GpuContext, RenderCtx, WorkerState,
    };
    use std::time::Instant;
    for preference in [AdapterPreference::default(), AdapterPreference::Cpu] {
        let gpu = match GpuContext::new(preference.clone()) {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("SKIP {preference:?} GPU operator cancellation test: {e}");
                continue;
            }
        };
        eprintln!(
            "operator upload adapter ({preference:?}): {:?}",
            gpu.adapter.get_info()
        );
        let mut value = rect(
            json!([{"type":"round_corners","radius":4},{"type":"offset","amount":2,"join":"round"}]),
        );
        value["fill"] = json!({"type":"solid","color":["0.12345","0.45678","0.78901","1/3"]});
        let node = VectorNode {
            spec: shape(value),
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
        let reference = node.spec.rasterize(t("0"), 64, 64).unwrap();
        assert_eq!(actual.image.pixels, reference.image.pixels);
        assert!((alpha(&actual, 24, 24) - 1.0 / 3.0).abs() < 0.0002);
        assert_eq!(alpha(&actual, 0, 0), 0.0);
    }
}
