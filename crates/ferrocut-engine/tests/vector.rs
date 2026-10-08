//! Native vector output tests: coverage, contour topology, stroke geometry,
//! gradients/color precision, animation, hashes, and timeline/matte integration.
//! CPU raster tests always execute; the final graph test uses an available GPU.

use ferrocut_core::{Animatable, CpuFrame, Rational, RationalTime, RenderNode};
use ferrocut_engine::generator::GeneratorAt;
use ferrocut_engine::vector::{VectorNode, VectorSpec};
use serde_json::{Value, json};

fn time(s: &str) -> RationalTime {
    RationalTime(s.parse::<Rational>().unwrap())
}

fn spec(value: Value) -> VectorSpec {
    let s: VectorSpec = serde_json::from_value(value).unwrap();
    s.validate().unwrap();
    s
}

fn rectangle(width: u32, height: u32) -> Value {
    json!({"geometry":{"type":"rectangle","width":width,"height":height}})
}

fn frame(value: Value, at: &str, width: u32, height: u32) -> CpuFrame {
    spec(value).rasterize(time(at), width, height).unwrap()
}

fn pixel(f: &CpuFrame, x: u32, y: u32) -> [f32; 4] {
    let i = (y * f.width + x) as usize * 4;
    [0, 1, 2, 3].map(|k| f.image.pixels[i + k].to_f32())
}

fn alpha_sum(f: &CpuFrame) -> f32 {
    f.image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3].to_f32())
        .sum()
}

fn close(a: [f32; 4], b: [f32; 4]) {
    for k in 0..4 {
        assert!((a[k] - b[k]).abs() < 0.001, "channel {k}: {a:?} vs {b:?}");
    }
}

fn working(c: [f64; 4]) -> [f32; 4] {
    GeneratorAt {
        kind: 0,
        c0: c,
        c1: [0.0; 4],
        geo: [0.0; 4],
        linear: false,
    }
    .pixel(0, 0)
}

#[test]
fn rectangle_coverage_is_exact_and_subpixels_are_antialiased() {
    let f = frame(
        json!({"geometry":{"type":"rectangle","x":4,"y":3,"width":12,"height":10}}),
        "0",
        24,
        18,
    );
    assert_eq!(pixel(&f, 4, 3)[3], 1.0);
    assert_eq!(pixel(&f, 15, 12)[3], 1.0);
    assert_eq!(pixel(&f, 3, 3), [0.0; 4]);
    assert_eq!(pixel(&f, 16, 13), [0.0; 4]);
    assert_eq!(alpha_sum(&f), 120.0);
    let f = frame(
        json!({"geometry":{"type":"rectangle","x":"4.5","y":"3.5","width":5,"height":5}}),
        "0",
        16,
        16,
    );
    assert!((pixel(&f, 4, 3)[3] - 0.25).abs() < 0.02);
    assert_eq!(pixel(&f, 5, 4)[3], 1.0);
    assert!((alpha_sum(&f) - 25.0).abs() < 0.1);
}

#[test]
fn rounded_rectangles_clamp_radius_and_ellipses_keep_correct_area() {
    let f = frame(
        json!({"geometry":{"type":"rectangle","x":3,"y":3,"width":20,"height":12,"radius":100}}),
        "0",
        28,
        20,
    );
    assert_eq!(pixel(&f, 3, 3)[3], 0.0);
    assert_eq!(pixel(&f, 12, 8)[3], 1.0);
    let rounded_area = 20.0 * 12.0 - (4.0 - std::f32::consts::PI) * 36.0;
    assert!((alpha_sum(&f) - rounded_area).abs() < 2.0);
    let e = frame(
        json!({"geometry":{"type":"ellipse","center":[12,10],"radius":[8,6]}}),
        "0",
        24,
        20,
    );
    assert_eq!(pixel(&e, 12, 10)[3], 1.0);
    assert_eq!(pixel(&e, 3, 10)[3], 0.0);
    assert!(
        (alpha_sum(&e) - std::f32::consts::PI * 48.0).abs() < 2.0,
        "ellipse coverage area {} vs {}",
        alpha_sum(&e),
        std::f32::consts::PI * 48.0
    );
    assert!(
        e.image
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3].to_f32() > 0.0 && p[3].to_f32() < 1.0)
    );
}

#[test]
fn cubic_paths_and_even_odd_holes_are_real_geometry() {
    let cubic = frame(
        json!({"geometry":{"type":"path","commands":[
            {"type":"move_to","point":[2,18]},
            {"type":"cubic_to","control1":[2,2],"control2":[18,2],"to":[18,18]},
            {"type":"close"}
        ]}}),
        "0",
        22,
        22,
    );
    assert_eq!(pixel(&cubic, 10, 10)[3], 1.0);
    assert_eq!(pixel(&cubic, 10, 4)[3], 0.0);
    assert_eq!(pixel(&cubic, 10, 19)[3], 0.0);
    let mut donut = json!({"geometry":{"type":"path","commands":[
        {"type":"move_to","point":[2,2]},{"type":"line_to","point":[18,2]},
        {"type":"line_to","point":[18,18]},{"type":"line_to","point":[2,18]},{"type":"close"},
        {"type":"move_to","point":[6,6]},{"type":"line_to","point":[14,6]},
        {"type":"line_to","point":[14,14]},{"type":"line_to","point":[6,14]},{"type":"close"}
    ]},"fill_rule":"even_odd"});
    let hole = frame(donut.clone(), "0", 20, 20);
    assert_eq!(pixel(&hole, 10, 10), [0.0; 4]);
    assert_eq!(pixel(&hole, 3, 3)[3], 1.0);
    assert_eq!(alpha_sum(&hole), 192.0);
    donut["fill_rule"] = json!("nonzero");
    assert_eq!(pixel(&frame(donut, "0", 20, 20), 10, 10)[3], 1.0);
}

fn line(cap: &str, dashes: Value, offset: Value) -> Value {
    json!({"geometry":{"type":"path","commands":[
        {"type":"move_to","point":[4,8]},{"type":"line_to","point":[28,8]}
    ]},"fill":null,"stroke":{"paint":{"type":"solid","color":[1,1,1]},
        "width":4,"cap":cap,"dashes":dashes,"dash_offset":offset}})
}

#[test]
fn stroke_caps_dashes_and_animated_offsets_change_coverage() {
    let butt = frame(line("butt", json!([]), json!(0)), "0", 34, 16);
    let round = frame(line("round", json!([]), json!(0)), "0", 34, 16);
    let square = frame(line("square", json!([]), json!(0)), "0", 34, 16);
    assert_eq!(pixel(&butt, 2, 8)[3], 0.0);
    assert!(pixel(&round, 2, 8)[3] > 0.9);
    assert_eq!(pixel(&square, 2, 6)[3], 1.0);
    assert!(pixel(&round, 2, 6)[3] < 0.7);
    let offset = json!({"keyframes":[{"t":0,"v":0},{"t":2,"v":4}]});
    let dashed = line("butt", json!([4, 4]), offset);
    let first = frame(dashed.clone(), "0", 34, 16);
    let shifted = frame(dashed, "1", 34, 16);
    assert_eq!(pixel(&first, 6, 8)[3], 1.0);
    assert_eq!(pixel(&first, 9, 8)[3], 0.0);
    assert_eq!(pixel(&shifted, 6, 8)[3], 0.0);
    let invisible = frame(line("butt", json!([0, 0]), json!(0)), "0", 34, 16);
    assert_eq!(alpha_sum(&invisible), 0.0);
    // A short path can lie completely within one long gap as the phase moves.
    let gap = frame(line("butt", json!([1, 100]), json!(2)), "0", 34, 16);
    assert_eq!(alpha_sum(&gap), 0.0);
    let excessive = spec(line("butt", json!(["0.000001", "0.000001"]), json!(0)))
        .rasterize(time("0"), 34, 16)
        .unwrap_err();
    assert!(excessive.contains("resource limit"), "{excessive}");
}

#[test]
fn stroke_joins_and_animated_miter_limit_change_corner_geometry() {
    let shape = |join: &str, miter_limit: Value| {
        json!({"geometry":{"type":"path","commands":[
            {"type":"move_to","point":[8,20]},
            {"type":"line_to","point":[8,8]},
            {"type":"line_to","point":[20,8]}
        ]},"fill":null,"stroke":{"paint":{"type":"solid","color":[1,1,1]},
            "width":6,"join":join,"miter_limit":miter_limit}})
    };
    let miter = frame(shape("miter", json!(4)), "0", 26, 26);
    let round = frame(shape("round", json!(4)), "0", 26, 26);
    let bevel = frame(shape("bevel", json!(4)), "0", 26, 26);
    assert_eq!(pixel(&miter, 5, 5)[3], 1.0);
    assert_eq!(pixel(&bevel, 5, 5)[3], 0.0);
    assert!(alpha_sum(&miter) > alpha_sum(&round) + 1.0);
    assert!(alpha_sum(&round) > alpha_sum(&bevel) + 1.0);
    let animated = shape("miter", json!({"keyframes":[{"t":0,"v":1},{"t":1,"v":4}]}));
    let limited = frame(animated.clone(), "0", 26, 26);
    let extended = frame(animated, "1", 26, 26);
    assert_eq!(limited.image.pixels, bevel.image.pixels);
    assert_eq!(extended.image.pixels, miter.image.pixels);
}

#[test]
fn fill_and_stroke_composite_in_linear_premultiplied_working_space() {
    let f = frame(
        json!({"geometry":{"type":"rectangle","x":4,"y":4,"width":12,"height":12},
            "fill":{"type":"solid","color":[1,0,0,"1/2"]},
            "stroke":{"paint":{"type":"solid","color":[0,0,1,"1/2"]},"width":4}
        }),
        "0",
        22,
        22,
    );
    let red = working([1.0, 0.0, 0.0, 0.5]);
    let blue = working([0.0, 0.0, 1.0, 0.5]);
    close(pixel(&f, 8, 8), red);
    close(
        pixel(&f, 4, 5),
        [0, 1, 2, 3].map(|k| blue[k] + red[k] * 0.5),
    );
    close(pixel(&f, 2, 5), blue);
    assert_eq!(pixel(&f, 1, 5), [0.0; 4]);
    assert_eq!(pixel(&f, 4, 5)[3], 0.75);
}

#[test]
fn vector_color_is_not_quantized_to_rgba8() {
    let mut shape = rectangle(8, 8);
    shape["fill"] = json!({"type":"solid","color":["0.12345","0.45678","0.78901","0.23456"]});
    let f = frame(shape, "0", 8, 8);
    close(
        pixel(&f, 3, 3),
        working([0.12345, 0.45678, 0.78901, 0.23456]),
    );
    assert!((pixel(&f, 3, 3)[3] - 0.23456).abs() < 0.0001);
    let quantized = (0.23456_f32 * 255.0).round() / 255.0;
    assert!((pixel(&f, 3, 3)[3] - quantized).abs() > 0.0005);
}

#[test]
fn gradients_match_existing_generator_color_and_alpha_semantics() {
    for linear in [false, true] {
        let paint = json!({"type":"linear_gradient","start":[2,1],"end":[30,14],
            "interpolation":if linear {"linear"} else {"display"},
            "stops":[{"offset":0,"color":[1,"0.25",0,1]},{"offset":1,"color":[0,"0.5",1,"0.2"]}]});
        let mut shape = rectangle(32, 16);
        shape["fill"] = paint;
        let f = frame(shape, "0", 32, 16);
        let reference = GeneratorAt {
            kind: 1,
            c0: [1.0, 0.25, 0.0, 1.0],
            c1: [0.0, 0.5, 1.0, 0.2],
            geo: [2.0, 1.0, 30.0, 14.0],
            linear,
        };
        for y in 0..16 {
            for x in 0..32 {
                close(pixel(&f, x, y), reference.pixel(x, y));
            }
        }
    }
}

#[test]
fn multiple_gradient_stops_hard_edges_and_radial_alpha_are_supported() {
    let mut shape = rectangle(18, 18);
    shape["fill"] = json!({"type":"linear_gradient","start":["0.5",0],"end":["16.5",0],"stops":[
        {"offset":0,"color":[1,0,0]},{"offset":"1/2","color":[0,1,0]},{"offset":1,"color":[0,0,1]}
    ]});
    let f = frame(shape.clone(), "0", 18, 18);
    close(pixel(&f, 0, 4), working([1.0, 0.0, 0.0, 1.0]));
    close(pixel(&f, 8, 4), working([0.0, 1.0, 0.0, 1.0]));
    close(pixel(&f, 16, 4), working([0.0, 0.0, 1.0, 1.0]));
    shape["fill"]["stops"] = json!([
        {"offset":0,"color":[1,0,0]},{"offset":"1/2","color":[1,0,0]},
        {"offset":"1/2","color":[0,0,1]},{"offset":1,"color":[0,0,1]}
    ]);
    let f = frame(shape.clone(), "0", 18, 18);
    close(pixel(&f, 7, 4), working([1.0, 0.0, 0.0, 1.0]));
    close(pixel(&f, 8, 4), working([0.0, 0.0, 1.0, 1.0]));
    shape["fill"] = json!({"type":"radial_gradient","center":["8.5","8.5"],"radius":4,"stops":[
        {"offset":0,"color":[1,1,1,1]},{"offset":1,"color":[0,0,1,0]}
    ]});
    let f = frame(shape.clone(), "0", 18, 18);
    assert_eq!(pixel(&f, 8, 8)[3], 1.0);
    assert_eq!(pixel(&f, 12, 8), [0.0; 4]);
    // Hidden blue at a transparent stop must not contaminate the visible edge.
    let p = pixel(&f, 10, 8);
    assert!((p[0] - p[1]).abs() < 0.001 && (p[1] - p[2]).abs() < 0.001);
    shape["fill"]["radius"] = json!(0);
    assert_eq!(alpha_sum(&frame(shape, "0", 18, 18)), 0.0);
}

#[test]
fn geometry_and_style_animation_evaluate_at_source_time() {
    let shape = json!({"geometry":{"type":"rectangle","x":4,"y":4,"width":{"keyframes":[
        {"t":0,"v":8},{"t":2,"v":16}
    ]},"height":12},"fill":{"type":"solid","color":[1,1,1,{"keyframes":[
        {"t":0,"v":1},{"t":2,"v":"1/2"}
    ]}]}});
    let s = spec(shape.clone());
    assert!(s.is_animated());
    let first = s.rasterize(time("0"), 26, 22).unwrap();
    let middle = s.rasterize(time("1"), 26, 22).unwrap();
    let last = s.rasterize(time("2"), 26, 22).unwrap();
    assert_eq!(pixel(&first, 14, 8), [0.0; 4]);
    assert_eq!(pixel(&middle, 14, 8)[3], 0.75);
    assert_eq!(pixel(&last, 18, 8)[3], 0.5);
    assert!(
        s.all()
            .iter()
            .any(|(name, a)| name == "geometry.width" && a.is_animated())
    );
    assert!(
        s.all()
            .iter()
            .any(|(name, a)| name == "fill.color.a" && a.is_animated())
    );
    let round_trip: VectorSpec = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(round_trip, s);
}

#[test]
fn path_control_points_and_gradient_stops_can_be_animated() {
    let shape = json!({"geometry":{"type":"path","commands":[
        {"type":"move_to","point":[2,18]},
        {"type":"quad_to","control":[10,{"keyframes":[{"t":0,"v":2},{"t":1,"v":14}]}],"to":[18,18]},
        {"type":"close"}
    ]},"fill":{"type":"linear_gradient","start":[0,0],"end":[20,0],"stops":[
        {"offset":0,"color":[1,0,0]},
        {"offset":{"keyframes":[{"t":0,"v":"1/4"},{"t":1,"v":"3/4"}]},"color":[0,0,1]}
    ]}});
    let s = spec(shape);
    let a = s.rasterize(time("0"), 22, 22).unwrap();
    let b = s.rasterize(time("1"), 22, 22).unwrap();
    assert!(alpha_sum(&a) > alpha_sum(&b) + 20.0);
    assert_ne!(pixel(&a, 10, 17), pixel(&b, 10, 17));
    assert!(
        s.all()
            .iter()
            .any(|(p, a)| p == "geometry.commands.1.control.y" && a.is_animated())
    );
    assert!(
        s.all()
            .iter()
            .any(|(p, a)| p == "fill.stops.1.offset" && a.is_animated())
    );
}

#[test]
fn hashes_sample_values_and_preserve_geometry_style_and_dimensions() {
    let held = spec(json!({"geometry":{"type":"rectangle","width":{"keyframes":[
        {"t":0,"v":8,"interp":"hold"},{"t":2,"v":16}
    ]},"height":12}}));
    let node = VectorNode {
        spec: held,
        width: 24,
        height: 20,
    };
    assert_eq!(
        node.content_hash_at(time("0")),
        node.content_hash_at(time("1"))
    );
    assert_ne!(
        node.content_hash_at(time("1")),
        node.content_hash_at(time("2"))
    );
    let constant = VectorNode {
        spec: spec(rectangle(8, 12)),
        width: 24,
        height: 20,
    };
    assert_eq!(
        node.content_hash_at(time("0")),
        constant.content_hash_at(time("0"))
    );
    assert_ne!(node.content_hash(), constant.content_hash());
    let different_size = VectorNode {
        spec: constant.spec.clone(),
        width: 25,
        height: 20,
    };
    assert_ne!(
        constant.content_hash_at(time("0")),
        different_size.content_hash_at(time("0"))
    );
    assert!(node.pulls(time("1")).is_empty());
}

#[test]
fn malformed_shape_requests_fail_before_rasterization() {
    for value in [
        json!({"geometry":{"type":"rectangle","width":2,"height":2,"unexpected":1}}),
        json!({"geometry":{"type":"ellipse","center":[0,0],"radius":[2,2]},"fill_rule":"xor"}),
        json!({"geometry":{"type":"path","commands":[{"type":"line_to","point":[1,1]}]}}),
        json!({"geometry":{"type":"path","commands":[{"type":"move_to","point":[0,0]},{"type":"close"}]}}),
        json!({"geometry":{"type":"rectangle","width":2,"height":2},"fill":{"type":"solid","color":[1,1]}}),
        json!({"geometry":{"type":"rectangle","width":2,"height":2},"fill":{"type":"linear_gradient","start":[0,0],"end":[2,0],"stops":[{"offset":0,"color":[1,1,1]}]}}),
        json!({"geometry":{"type":"rectangle","width":2,"height":2},"stroke":{"paint":{"type":"solid","color":[1,1,1]},"dashes":[1,2,3]}}),
        json!({"geometry":{"type":"rectangle","width":{"keyframes":[]},"height":2}}),
    ] {
        let result = serde_json::from_value::<VectorSpec>(value.clone());
        assert!(
            result.is_err() || result.unwrap().validate().is_err(),
            "accepted {value}"
        );
    }
    let s = spec(rectangle(2, 2));
    assert!(s.rasterize(time("0"), 0, 10).is_err());
    assert!(s.rasterize(time("0"), u32::MAX, u32::MAX).is_err());
}

#[test]
fn empty_clipped_and_zero_width_geometry_is_transparent() {
    let f = frame(
        json!({"geometry":{"type":"rectangle","x":100,"y":100,"width":8,"height":8}}),
        "0",
        16,
        16,
    );
    assert_eq!(alpha_sum(&f), 0.0);
    let f = frame(rectangle(0, 8), "0", 16, 16);
    assert_eq!(alpha_sum(&f), 0.0);
    let mut l = line("round", json!([]), json!(0));
    l["stroke"]["width"] = json!(0);
    assert_eq!(alpha_sum(&frame(l, "0", 34, 16)), 0.0);
    let s = spec(rectangle(8, 8));
    let defaults: Vec<_> = s
        .all()
        .iter()
        .map(|(p, a)| (p.clone(), (*a).clone()))
        .collect();
    assert!(
        defaults
            .iter()
            .any(|(p, a)| p == "geometry.radius" && *a == Animatable::constant(Rational::ZERO))
    );
}

#[test]
fn shape_timelines_bake_expressions_and_supply_animated_alpha_track_mattes() {
    use ferrocut_core::{AdapterPreference, CancelToken, GpuContext, RenderCtx, WorkerState};
    use ferrocut_engine::Timeline;
    use ferrocut_engine::compile::compile;
    use ferrocut_engine::compositor::{Compositor, compositor_slot};
    use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
    use ferrocut_engine::generator::GeneratorSpec;
    use ferrocut_engine::graph::FrameCache;
    use std::sync::Arc;

    let value = json!({
        "output":{"width":40,"height":24,"fps":24,"gop":12},
        "tracks":[
            {"name":"picture","matte":{"mode":"alpha"},"clips":[
                {"id":"red","start":0,"duration":2,"generator":{"type":"solid","color":[1,0,0]}}
            ]},
            {"name":"mask","clips":[
                {"id":"shape","start":0,"source_in":"1/2","duration":2,"generator":{"type":"shape","shape":{
                    "geometry":{"type":"rectangle","x":{"expression":"4 + time * 4"},"y":4,"width":12,"height":12,"radius":3}
                }}}
            ]}
        ]
    });
    let timeline = Timeline::from_json(&value.to_string()).unwrap();
    let baked = ferrocut_engine::expr::bake(&timeline).unwrap();
    let GeneratorSpec::Shape { shape } = baked.tracks[1].clips[0].generator.as_ref().unwrap()
    else {
        panic!("shape source")
    };
    // Source time includes source_in; neither expression nor geometry is keyed
    // to the picture clip's or composition's zero.
    let a = shape.rasterize(time("1/2"), 40, 24).unwrap();
    let b = shape.rasterize(time("1"), 40, 24).unwrap();
    assert_eq!(pixel(&a, 7, 10)[3], 1.0);
    assert_eq!(pixel(&b, 7, 10)[3], 0.0);
    let compiled = compile(&timeline).unwrap();
    let ops = parse_ops(r#"[{"op":"set_param","clip":"shape","param":"generator.shape.geometry.width","value":16}]"#).unwrap();
    let mut media = MediaLengths::new(".", |_| None);
    let (edited, _) = apply(&timeline, &ops, &mut media).unwrap();
    let edited_graph = compile(&edited).unwrap();
    assert_ne!(
        compiled.graph.frame_key(compiled.output, time("0")),
        edited_graph.graph.frame_key(edited_graph.output, time("0"))
    );
    let edit = |ops: &str| {
        apply(
            &timeline,
            &parse_ops(ops).unwrap(),
            &mut MediaLengths::unbounded(),
        )
        .unwrap()
        .0
    };
    let split = edit(r#"[{"op":"split","clip":"shape","at":1}]"#);
    let split_graph = compile(&split).unwrap();
    for f in 0..48 {
        let t = RationalTime::from_frames(f, timeline.output.fps);
        assert_eq!(
            compiled.graph.frame_key(compiled.output, t),
            split_graph.graph.frame_key(split_graph.output, t),
            "split changed source-time vector frame {f}"
        );
    }
    let unfilled =
        edit(r#"[{"op":"set_param","clip":"shape","param":"generator.shape.fill","value":null}]"#);
    let GeneratorSpec::Shape { shape: empty } =
        unfilled.tracks[1].clips[0].generator.as_ref().unwrap()
    else {
        panic!("shape source")
    };
    assert!(empty.fill.is_none());
    assert_eq!(
        alpha_sum(&empty.rasterize(time("1/2"), 40, 24).unwrap()),
        0.0
    );
    let empty_graph = compile(&unfilled).unwrap();
    let sped = edit(r#"[{"op":"set_speed","clip":"shape","speed":2}]"#);
    assert_eq!(sped.tracks[1].clips[0].source_in, time("1/2"));
    assert_eq!(sped.tracks[1].clips[0].duration, time("1"));
    let sped_graph = compile(&sped).unwrap();

    let gpu = match GpuContext::new(AdapterPreference::Cpu) {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("SKIP GPU vector matte integration: no software adapter ({e})");
            return;
        }
    };
    let compositor = Arc::new(Compositor::new(&gpu));
    let mut worker = WorkerState::default();
    worker
        .slot(compositor_slot(), || Ok(compositor.clone()))
        .unwrap();
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
    let mut cache = FrameCache::new(8);
    let red = working([1.0, 0.0, 0.0, 1.0]);
    for at in ["0", "1/2", "1"] {
        let mask = shape.rasterize(time(at) + time("1/2"), 40, 24).unwrap();
        let output = compiled
            .graph
            .evaluate(compiled.output, time(at), &mut ctx, &mut cache)
            .unwrap();
        ctx.flush();
        let actual = output.to_cpu_frame(&gpu).unwrap();
        for y in 0..24 {
            for x in 0..40 {
                let alpha = pixel(&mask, x, y)[3];
                close(pixel(&actual, x, y), red.map(|c| c * alpha));
            }
        }
    }
    for (fast, original) in [("0", "0"), ("1/4", "1/2"), ("3/4", "3/2")] {
        let reference = compiled
            .graph
            .evaluate(compiled.output, time(original), &mut ctx, &mut cache)
            .unwrap();
        let output = sped_graph
            .graph
            .evaluate(sped_graph.output, time(fast), &mut ctx, &mut cache)
            .unwrap();
        ctx.flush();
        let reference = reference.to_cpu_frame(&gpu).unwrap();
        let output = output.to_cpu_frame(&gpu).unwrap();
        for (i, (a, b)) in reference
            .image
            .pixels
            .iter()
            .zip(&output.image.pixels)
            .enumerate()
        {
            assert_eq!(
                a,
                b,
                "retimed vector source {fast} != original {original} at pixel ({}, {}) channel {}",
                i / 4 % 40,
                i / 4 / 40,
                i % 4
            );
        }
    }
    let cleared = empty_graph
        .graph
        .evaluate(empty_graph.output, time("0"), &mut ctx, &mut cache)
        .unwrap();
    ctx.flush();
    assert_eq!(alpha_sum(&cleared.to_cpu_frame(&gpu).unwrap()), 0.0);
}
