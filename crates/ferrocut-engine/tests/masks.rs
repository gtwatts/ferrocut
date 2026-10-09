//! Synthetic, GPU-free pixel oracles and hostile-input mask regressions.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use ferrocut_core::{
    AdapterPreference, AlphaMode, Animatable, CancelToken, ColorSpace, CpuFrame, CpuImage,
    ErrorKind, GpuContext, NodeError, PixelRect, Rational, RationalTime, RenderCtx, WorkerState,
};
use ferrocut_engine::masks::{MAX_MASK_COMMANDS, MAX_MASKS, MaskSpec, MaskStack};
use ferrocut_engine::{
    Timeline,
    compile::{Compiled, compile},
    edit::{MediaLengths, apply, parse_ops},
    expr::bake,
};
use half::f16;
use serde_json::{Value, json};

fn time(s: &str) -> RationalTime {
    RationalTime(s.parse().unwrap())
}
fn mask(v: Value) -> MaskSpec {
    serde_json::from_value(v).unwrap()
}
fn stack(masks: Vec<MaskSpec>) -> MaskStack {
    MaskStack { masks }
}
fn rect(x: &str, y: &str, width: &str, height: &str) -> MaskSpec {
    mask(json!({"geometry":{"type":"rectangle","x":x,"y":y,"width":width,"height":height}}))
}
fn constant(s: &str) -> Animatable {
    Animatable::constant(s.parse().unwrap())
}
fn close(a: f32, b: f64, epsilon: f64) {
    assert!((a as f64 - b).abs() <= epsilon, "{a} vs {b}");
}

fn falloff(sd: f64, feather: f64) -> f64 {
    let u = (sd / feather.max(1.0) + 0.5).clamp(0.0, 1.0);
    u * (1.0 - feather.min(1.0)) + (3.0 * u * u - 2.0 * u * u * u) * feather.min(1.0)
}
/// Independent analytic signed distance to an axis-aligned box.
fn rectangle_sd(p: [f64; 2], r: [f64; 4]) -> f64 {
    let qx = (p[0] - r[0] - r[2] * 0.5).abs() - r[2] * 0.5;
    let qy = (p[1] - r[1] - r[3] * 0.5).abs() - r[3] * 0.5;
    -(qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0))
}
fn combine(mode: &str, a: f64, m: f64) -> f64 {
    match mode {
        "none" => a,
        "add" => a + m * (1.0 - a),
        "subtract" => a * (1.0 - m),
        "intersect" => a * m,
        "lighten" => a.max(m),
        "darken" => a.min(m),
        "difference" => a * (1.0 - m) + m * (1.0 - a),
        _ => panic!("test mode"),
    }
}

#[test]
fn subpixel_rectangle_coverage_matches_analytic_distance_in_negative_overscan() {
    let shape = rect("13/4", "11/4", "21/2", "33/4");
    let region = PixelRect::new(-4, -3, 28, 22);
    let coverage = stack(vec![shape])
        .coverage(time("0"), 20, 16, region)
        .unwrap();
    assert!(coverage.active);
    for (i, c) in coverage.values.iter().enumerate() {
        let p = [
            region.x as f64 + (i % region.width as usize) as f64 + 0.5,
            region.y as f64 + (i / region.width as usize) as f64 + 0.5,
        ];
        close(
            *c,
            falloff(rectangle_sd(p, [3.25, 2.75, 10.5, 8.25]), 0.0),
            1e-6,
        );
    }
}

#[test]
fn feather_expansion_inversion_and_opacity_match_independent_pixel_math() {
    for feather in ["0", "1/2", "4"] {
        for expansion in ["-3/2", "0", "3/2"] {
            for inverted in [false, true] {
                let mut shape = rect("4", "3", "10", "8");
                shape.feather = constant(feather);
                shape.expansion = constant(expansion);
                shape.opacity = constant("3/8");
                shape.inverted = inverted;
                let coverage = stack(vec![shape])
                    .coverage(time("0"), 20, 16, PixelRect::full(20, 16))
                    .unwrap();
                let f: Rational = feather.parse().unwrap();
                let e: Rational = expansion.parse().unwrap();
                for (i, actual) in coverage.values.iter().enumerate() {
                    let p = [(i % 20) as f64 + 0.5, (i / 20) as f64 + 0.5];
                    let c = falloff(
                        rectangle_sd(p, [4.0, 3.0, 10.0, 8.0]) + e.to_f64(),
                        f.to_f64(),
                    );
                    close(*actual, 0.375 * if inverted { 1.0 - c } else { c }, 1e-6);
                }
            }
        }
    }
}

#[test]
fn horizontal_spans_preserve_large_feather_expansion_and_negative_roi_pixels() {
    let region = PixelRect::new(-100, -80, 320, 240);
    for expansion in ["-32", "32"] {
        let mut shape = rect("81/4", "63/4", "61/2", "89/4");
        shape.feather = constant("64");
        shape.expansion = constant(expansion);
        let cov = stack(vec![shape])
            .coverage(time("0"), 96, 64, region)
            .unwrap();
        let e: Rational = expansion.parse().unwrap();
        for (i, actual) in cov.values.iter().enumerate() {
            let p = [
                -100.0 + (i % 320) as f64 + 0.5,
                -80.0 + (i / 320) as f64 + 0.5,
            ];
            close(
                *actual,
                falloff(
                    rectangle_sd(p, [20.25, 15.75, 30.5, 22.25]) + e.to_f64(),
                    64.0,
                ),
                1e-6,
            );
        }
    }
    let circle = stack(vec![mask(
        json!({"geometry":{"type":"ellipse","center":[320,180],"radius":[100,100]},
        "feather":128,"expansion":64}),
    )]);
    let roi = PixelRect::new(-20, -20, 680, 400);
    let cov = circle.coverage(time("0"), 640, 360, roi).unwrap();
    for (i, actual) in cov.values.iter().enumerate() {
        let p = [
            -20.0 + (i % 680) as f64 + 0.5,
            -20.0 + (i / 680) as f64 + 0.5,
        ];
        close(
            *actual,
            falloff(164.0 - (p[0] - 320.0).hypot(p[1] - 180.0), 128.0),
            0.001,
        );
    }
}

#[test]
fn useful_uhd_ellipse_feather_stays_inside_the_row_work_budget() {
    let masks = stack(vec![mask(
        json!({"geometry":{"type":"ellipse","center":[1920,1080],"radius":[1600,900]},
        "feather":64,"expansion":32}),
    )]);
    let cov = masks
        .coverage(time("0"), 3840, 2160, PixelRect::full(3840, 2160))
        .unwrap();
    assert_eq!(cov.values.len(), 3840 * 2160);
    assert_eq!(cov.values[1080 * 3840 + 1920], 1.0);
    assert_eq!(cov.values[0], 0.0);
    close(cov.values[1080 * 3840 + 3550], falloff(1.5, 64.0), 0.002);
}

#[test]
fn every_combine_pair_uses_correct_first_mask_initialization_and_fractional_coverage() {
    let modes = [
        "none",
        "add",
        "subtract",
        "intersect",
        "lighten",
        "darken",
        "difference",
    ];
    for a_mode in modes {
        for b_mode in modes {
            let a = mask(
                json!({"geometry":{"type":"rectangle","x":1,"y":1,"width":8,"height":8},
                "mode":a_mode,"opacity":"3/4","feather":2}),
            );
            let b = mask(
                json!({"geometry":{"type":"rectangle","x":4,"y":0,"width":6,"height":7},
                "mode":b_mode,"opacity":"1/2","feather":3,"inverted":true}),
            );
            let coverage = stack(vec![a, b])
                .coverage(time("0"), 12, 10, PixelRect::full(12, 10))
                .unwrap();
            let first = if a_mode != "none" { a_mode } else { b_mode };
            for (i, actual) in coverage.values.iter().enumerate() {
                if first == "none" {
                    close(*actual, 1.0, 0.0);
                    continue;
                }
                let p = [(i % 12) as f64 + 0.5, (i / 12) as f64 + 0.5];
                let ac = falloff(rectangle_sd(p, [1.0, 1.0, 8.0, 8.0]), 2.0) * 0.75;
                let bc = (1.0 - falloff(rectangle_sd(p, [4.0, 0.0, 6.0, 7.0]), 3.0)) * 0.5;
                let initial = if ["subtract", "intersect", "darken"].contains(&first) {
                    1.0
                } else {
                    0.0
                };
                let expected = combine(b_mode, combine(a_mode, initial, ac), bc);
                close(*actual, expected, 2e-6);
            }
            assert_eq!(coverage.active, first != "none");
        }
    }
}

fn contour(x0: i32, y0: i32, x1: i32, y1: i32, reverse: bool) -> Vec<Value> {
    let mut p = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
    if reverse {
        p.reverse();
    }
    let mut result = vec![json!({"type":"move_to","point":p[0]})];
    result.extend(p[1..].iter().map(|p| json!({"type":"line_to","point":p})));
    result.push(json!({"type":"close"}));
    result
}

#[test]
fn compound_contours_holes_and_open_paths_respect_fill_rules() {
    let mut same = contour(1, 1, 13, 13, false);
    same.extend(contour(4, 4, 10, 10, false));
    let mut reversed = contour(1, 1, 13, 13, false);
    reversed.extend(contour(4, 4, 10, 10, true));
    for (commands, rule, middle) in [
        (same.clone(), "nonzero", 1.0),
        (same, "even_odd", 0.0),
        (reversed, "nonzero", 0.0),
    ] {
        let coverage = stack(vec![mask(
            json!({"geometry":{"type":"path","commands":commands},"fill_rule":rule}),
        )])
        .coverage(time("0"), 16, 16, PixelRect::full(16, 16))
        .unwrap();
        close(coverage.values[7 * 16 + 7], middle, 0.0);
        close(coverage.values[2 * 16 + 2], 1.0, 0.0);
        close(coverage.values[15 * 16 + 15], 0.0, 0.0);
    }
    let mut open = contour(2, 3, 10, 11, false);
    open.pop();
    let a = stack(vec![mask(
        json!({"geometry":{"type":"path","commands":open}}),
    )]);
    let b = stack(vec![rect("2", "3", "8", "8")]);
    assert_eq!(
        a.coverage(time("0"), 16, 16, PixelRect::full(16, 16))
            .unwrap()
            .values,
        b.coverage(time("0"), 16, 16, PixelRect::full(16, 16))
            .unwrap()
            .values
    );
}

/// Independent brute-force winding and closest segment distance; no row bands.
fn polygon_sd(p: [f64; 2], points: &[[f64; 2]]) -> f64 {
    let mut inside = false;
    let mut distance = f64::INFINITY;
    for (a, b) in points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
    {
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
        let d = [b[0] - a[0], b[1] - a[1]];
        let len2 = d[0] * d[0] + d[1] * d[1];
        let t = if len2 == 0.0 {
            0.0
        } else {
            ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len2
        }
        .clamp(0.0, 1.0);
        distance = distance.min((p[0] - a[0] - t * d[0]).hypot(p[1] - a[1] - t * d[1]));
    }
    if inside { distance } else { -distance }
}

#[test]
fn cubic_bezier_pixels_match_a_dense_independent_curve_reference() {
    let shape = mask(json!({"geometry":{"type":"path","commands":[
        {"type":"move_to","point":[2,10]},
        {"type":"cubic_to","control1":[2,0],"control2":[14,0],"to":[14,10]},
        {"type":"line_to","point":[14,14]}, {"type":"line_to","point":[2,14]}, {"type":"close"}
    ]},"feather":4,"expansion":"1/2"}));
    let mut points = Vec::new();
    for i in 0..=4096 {
        let t = i as f64 / 4096.0;
        let u = 1.0 - t;
        points.push([
            2.0 * u * u * u + 6.0 * u * u * t + 42.0 * u * t * t + 14.0 * t * t * t,
            10.0 * u * u * u + 10.0 * t * t * t,
        ]);
    }
    points.extend([[14.0, 14.0], [2.0, 14.0]]);
    let coverage = stack(vec![shape])
        .coverage(time("0"), 18, 18, PixelRect::full(18, 18))
        .unwrap();
    for (i, actual) in coverage.values.iter().enumerate() {
        let p = [(i % 18) as f64 + 0.5, (i / 18) as f64 + 0.5];
        close(*actual, falloff(polygon_sd(p, &points) + 0.5, 4.0), 0.02);
    }
}

#[test]
fn ellipse_rounded_rectangle_polygon_star_and_quadratics_render_native_curves() {
    let circle =
        mask(json!({"geometry":{"type":"ellipse","center":[10,10],"radius":[6,6]},"feather":2}));
    let coverage = stack(vec![circle])
        .coverage(time("0"), 20, 20, PixelRect::full(20, 20))
        .unwrap();
    for (i, c) in coverage.values.iter().enumerate() {
        let distance = 6.0 - ((i % 20) as f64 + 0.5 - 10.0).hypot((i / 20) as f64 + 0.5 - 10.0);
        // 0.05px flatten tolerance × at most 0.75 coverage/px, plus
        // the four-cubic circle approximation, bounds this comparison.
        close(*c, falloff(distance, 2.0), 0.04);
    }
    let rounded = mask(
        json!({"geometry":{"type":"rectangle","x":2,"y":2,"width":16,"height":12,"radius":3},"feather":2}),
    );
    let coverage = stack(vec![rounded])
        .coverage(time("0"), 20, 16, PixelRect::full(20, 16))
        .unwrap();
    for (i, c) in coverage.values.iter().enumerate() {
        let qx = ((i % 20) as f64 + 0.5 - 10.0).abs() - 5.0;
        let qy = ((i / 20) as f64 + 0.5 - 8.0).abs() - 3.0;
        let sd = 3.0 - qx.max(0.0).hypot(qy.max(0.0)) - qx.max(qy).min(0.0);
        close(*c, falloff(sd, 2.0), 0.03);
    }
    for geometry in [
        json!({"type":"polygon","center":[10,10],"points":5,"radius":7,"roundness":25}),
        json!({"type":"star","center":[10,10],"points":5,"inner_radius":3,"outer_radius":8,"outer_roundness":15}),
        json!({"type":"path","commands":[{"type":"move_to","point":[2,12]},
            {"type":"quad_to","control":[10,0],"to":[18,12]},
            {"type":"line_to","point":[18,18]}, {"type":"line_to","point":[2,18]}, {"type":"close"}]}),
    ] {
        let shape = stack(vec![mask(json!({"geometry":geometry}))]);
        let coverage = shape
            .coverage(time("0"), 20, 20, PixelRect::full(20, 20))
            .unwrap();
        assert!(coverage.values.iter().any(|v| *v > 0.99));
        assert!(coverage.values.contains(&0.0));
        assert_eq!(
            shape.hash_bytes_at(time("0")).unwrap(),
            shape.hash_bytes_at(time("9223372036854775807")).unwrap()
        );
    }
}

#[test]
fn rational_animation_and_safe_property_visitors_produce_deterministic_evaluated_cache_bytes() {
    let mut shape = rect("4", "2", "6", "8");
    shape.geometry = serde_json::from_value(json!({"type":"rectangle","x":{"keyframes":[
        {"t":"1/3","v":4},{"t":"5/3","v":12}]},"y":2,"width":6,"height":8}))
    .unwrap();
    let animated = stack(vec![shape]);
    let fixed = stack(vec![rect("6", "2", "6", "8")]);
    assert_eq!(
        animated.hash_bytes_at(time("2/3")).unwrap(),
        fixed.hash_bytes_at(time("0")).unwrap()
    );
    assert_eq!(
        animated
            .coverage(time("2/3"), 24, 12, PixelRect::full(24, 12))
            .unwrap()
            .values,
        fixed
            .coverage(time("0"), 24, 12, PixelRect::full(24, 12))
            .unwrap()
            .values
    );
    assert_ne!(
        animated.hash_bytes_at(time("1/3")).unwrap(),
        animated.hash_bytes_at(time("5/3")).unwrap()
    );
    let serialized = serde_json::to_string(&animated).unwrap();
    assert!(serialized.starts_with('['));
    let mut restored: MaskStack = serde_json::from_str(&serialized).unwrap();
    assert_eq!(
        animated.hash_bytes().unwrap(),
        restored.hash_bytes().unwrap()
    );
    let immutable: Vec<_> = restored.all().into_iter().map(|(name, _)| name).collect();
    let mutable: Vec<_> = restored
        .all_mut()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(immutable, mutable);
    assert_eq!(
        immutable.len(),
        immutable.iter().collect::<HashSet<_>>().len()
    );
    for (name, value) in restored.all_mut() {
        if name == "0.opacity" {
            *value = constant("1/2");
        }
    }
    assert_ne!(
        animated.hash_bytes_at(time("2/3")).unwrap(),
        restored.hash_bytes_at(time("2/3")).unwrap()
    );
    let aliases: MaskStack =
        serde_json::from_str(&serialized.replace("\"1/3\"", "\"2/6\"")).unwrap();
    assert_eq!(
        animated.hash_bytes().unwrap(),
        aliases.hash_bytes().unwrap()
    );
}

#[test]
fn reverse_source_clock_split_and_trim_preserve_exact_mask_animation_without_key_shifts() {
    let tl=Timeline::from_json(&json!({"output":{"width":24,"height":16,"fps":"24"},
        "tracks":[{"name":"V1","clips":[{"id":"masked","start":1,"duration":2,"source_in":"10/3","speed":"-1/2",
            "generator":{"type":"solid","color":[1,1,1]},"masks":[{"geometry":{"type":"rectangle",
                "x":{"keyframes":[{"t":2,"v":2},{"t":4,"v":14}]},"y":2,"width":8,"height":10}}]}]}]}).to_string()).unwrap();
    let split = apply(
        &tl,
        &parse_ops(r#"[{"op":"split","clip":"masked","at":2}]"#).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0;
    let trim = apply(
        &tl,
        &parse_ops(r#"[{"op":"trim","clip":"masked","edge":"in","delta":"1/2"}]"#).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0;
    let clips = [
        &tl.tracks[0].clips[0],
        &split.tracks[0].clips[1],
        &trim.tracks[0].clips[0],
    ];
    let fixed = stack(vec![rect("11/2", "2", "8", "10")]);
    for clip in clips {
        let source = clip.time_map().source_at(time("5/2") - clip.start);
        assert_eq!(source, time("31/12"));
        assert_eq!(clip.masks, tl.tracks[0].clips[0].masks);
        assert_eq!(
            clip.masks.hash_bytes_at(source).unwrap(),
            fixed.hash_bytes_at(time("0")).unwrap()
        );
    }
}

#[test]
fn no_active_masks_and_empty_regions_are_pass_through_with_metadata_preserved() {
    let mut disabled = rect("1", "1", "2", "2");
    disabled.enabled = false;
    let mut none = disabled.clone();
    none.enabled = true;
    none.mode = ferrocut_engine::masks::MaskMode::None;
    for masks in [MaskStack::default(), stack(vec![disabled, none])] {
        let coverage = masks
            .coverage(time("0"), 4, 4, PixelRect::new(-1, -1, 4, 4))
            .unwrap();
        assert!(!coverage.active);
        assert!(coverage.values.iter().all(|v| *v == 1.0));
    }
    let active = stack(vec![rect("0", "0", "4", "4")]);
    let empty = active
        .coverage(time("0"), 4, 4, PixelRect::new(-2, -2, 0, 3))
        .unwrap();
    assert!(empty.active && empty.values.is_empty());
    let input = CpuFrame {
        width: 4,
        height: 4,
        data_window: PixelRect::new(-1, -1, 4, 4),
        pixel_aspect: Rational::new(4, 3),
        color_space: ColorSpace::acescg(),
        alpha: AlphaMode::Premultiplied,
        image: Arc::new(CpuImage {
            pixels: [-0.25, 2.0, 0.5, 0.5].map(f16::from_f32).repeat(16),
        }),
    };
    let passthrough = MaskStack::default().apply_cpu(&input, time("0")).unwrap();
    assert!(Arc::ptr_eq(&input.image, &passthrough.image));
    let mut full = rect("-2", "-2", "8", "8");
    full.opacity = constant("1/2");
    let out = stack(vec![full]).apply_cpu(&input, time("0")).unwrap();
    assert_eq!(out.pixel_aspect, input.pixel_aspect);
    assert_eq!(out.color_space, input.color_space);
    assert_eq!(out.alpha, input.alpha);
    assert_eq!(out.data_window, input.data_window);
    for p in out.image.pixels.as_chunks::<4>().0 {
        assert_eq!(
            *p,
            [
                f16::from_f32(-0.125),
                f16::ONE,
                f16::from_f32(0.25),
                f16::from_f32(0.25)
            ]
        );
    }
}

#[test]
fn cancellation_kind_is_preserved_before_planning_during_rows_and_after_render() {
    let masks = stack(vec![rect("2", "2", "10", "8")]);
    let height = 16;
    for stop in [0, height / 2, height + 4, height * 2 + 3] {
        let calls = std::cell::Cell::new(0);
        let error = masks
            .coverage_checked(time("0"), 20, height, PixelRect::full(20, height), || {
                let call = calls.get();
                calls.set(call + 1);
                if call >= stop {
                    Err(NodeError::cancelled("mask cancellation marker"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(error.message, "mask cancellation marker");
    }
}

#[test]
fn hostile_region_geometry_animation_expressions_and_work_fail_with_useful_errors() {
    let simple = stack(vec![rect("0", "0", "10", "10")]);
    for (w, h, region) in [
        (0, 1, PixelRect::full(1, 1)),
        (u32::MAX, 1, PixelRect::full(1, 1)),
        (1, 1, PixelRect::new(i32::MIN, 0, 1, 1)),
        (1, 1, PixelRect::new(0, 0, u32::MAX, u32::MAX)),
        (8192, 8192, PixelRect::full(1, 1)),
    ] {
        assert!(simple.coverage(time("0"), w, h, region).is_err());
    }
    assert!(
        stack(vec![rect("0", "0", "1", "1"); MAX_MASKS + 1])
            .validate()
            .is_err()
    );
    let commands = vec![json!({"type":"move_to","point":[0,0]}); MAX_MASK_COMMANDS + 1];
    assert!(
        stack(vec![mask(
            json!({"geometry":{"type":"path","commands":commands}})
        )])
        .validate()
        .is_err()
    );
    for value in [
        json!({"geometry":{"type":"path","commands":[{"type":"line_to","point":[1,1]}]}}),
        json!({"geometry":{"type":"rectangle","width":-1,"height":2}}),
        json!({"geometry":{"type":"star","center":[0,0],"points":10000,"inner_radius":1,"outer_radius":2}}),
        json!({"geometry":{"type":"rectangle","width":1,"height":2},"opacity":2}),
    ] {
        assert!(stack(vec![mask(value)]).validate().is_err());
    }
    let expression = stack(vec![mask(
        json!({"geometry":{"type":"rectangle","width":{"expression":"10"},"height":2}}),
    )]);
    expression.validate().unwrap();
    assert!(
        expression
            .hash_bytes_at(time("0"))
            .unwrap_err()
            .contains("baked")
    );
    let overshoot = stack(vec![mask(
        json!({"geometry":{"type":"rectangle","width":8,"height":8},
        "opacity":{"keyframes":[{"t":0,"v":0,"interp":{"bezier":[0,4,1,4]}},{"t":1,"v":1}]}}),
    )]);
    overshoot.validate().unwrap();
    assert!(
        overshoot
            .coverage(time("1/2"), 16, 16, PixelRect::full(16, 16))
            .unwrap_err()
            .contains("opacity")
    );
    let hostile_time = stack(vec![mask(
        json!({"geometry":{"type":"rectangle","width":8,"height":8,
        "x":{"keyframes":[{"t":"1/9223372036854775807","v":0},{"t":"1/9223372036854775805","v":1}]}}}),
    )]);
    hostile_time.validate().unwrap();
    assert!(
        hostile_time
            .hash_bytes_at(time("1/9223372036854775806"))
            .unwrap_err()
            .contains("time arithmetic")
    );
    hostile_time
        .hash_bytes_at(time("1/9223372036854775807"))
        .unwrap();
    let large_curve = stack(vec![mask(json!({"geometry":{"type":"path","commands":[
        {"type":"move_to","point":[0,0]}, {"type":"cubic_to","control1":[1000000,1000000],"control2":[-1000000,1000000],"to":[0,10]}, {"type":"close"}]}}))]);
    assert!(
        large_curve
            .coverage(time("0"), 16, 16, PixelRect::full(16, 16))
            .unwrap_err()
            .contains("accuracy budget")
    );
    let expensive = stack(vec![rect("0", "0", "3840", "2160"); MAX_MASKS]);
    assert!(
        expensive
            .coverage(time("0"), 3840, 2160, PixelRect::full(3840, 2160))
            .unwrap_err()
            .contains("workload")
    );
}

#[test]
fn strict_serialization_and_cpu_storage_reject_malformed_values() {
    for bad in [
        json!({"geometry":{"type":"rectangle","width":1,"height":2},"unknown":true}),
        json!({"geometry":{"type":"rectangle","width":1,"height":2},"mode":"xor"}),
        json!({"geometry":{"type":"rectangle","width":1.25,"height":2}}),
        json!({"geometry":{"type":"rectangle","width":1,"height":2},"opacity":null}),
    ] {
        assert!(serde_json::from_value::<MaskSpec>(bad).is_err());
    }
    let masks = stack(vec![rect("0", "0", "4", "4")]);
    let mut cov = masks
        .coverage(time("0"), 4, 4, PixelRect::full(4, 4))
        .unwrap();
    let input = CpuFrame::new(4, 4, ColorSpace::acescg(), vec![f16::ONE; 64]);
    cov.values[0] = f32::NAN;
    assert!(cov.apply_cpu(&input).unwrap_err().contains("finite"));
    cov.values[0] = 1.0;
    let mut invalid = input.clone();
    invalid.image = Arc::new(CpuImage {
        pixels: vec![f16::ONE; 4],
    });
    assert!(cov.apply_cpu(&invalid).unwrap_err().contains("storage"));
    invalid = input.clone();
    invalid.image = Arc::new(CpuImage {
        pixels: vec![f16::from_f32(2.0); 64],
    });
    assert!(cov.apply_cpu(&invalid).unwrap_err().contains("alpha"));
}

fn adapters() -> Vec<(&'static str, &'static GpuContext)> {
    static DEFAULT: OnceLock<Option<GpuContext>> = OnceLock::new();
    static CPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    [
        ("default", &DEFAULT, AdapterPreference::default()),
        ("cpu", &CPU, AdapterPreference::Cpu),
    ]
    .into_iter()
    .filter_map(|(name, cell, preference)| {
        cell.get_or_init(|| match GpuContext::new(preference) {
            Ok(gpu) => {
                eprintln!(
                    "Mask adapter {name}: {:?} {:?} {:?}",
                    gpu.info.name, gpu.info.backend, gpu.info.device_type
                );
                Some(gpu)
            }
            Err(error) => {
                eprintln!("SKIP mask adapter {name}: {error}");
                None
            }
        })
        .as_ref()
        .map(|gpu| (name, gpu))
    })
    .collect()
}
fn worker(gpu: &GpuContext) -> WorkerState {
    let mut worker = WorkerState::default();
    worker
        .slot(ferrocut_engine::compositor::compositor_slot(), || {
            Ok(Arc::new(ferrocut_engine::compositor::Compositor::new(gpu)))
        })
        .unwrap();
    worker
}
fn render(gpu: &GpuContext, compiled: &Compiled, at: RationalTime) -> CpuFrame {
    let mut worker = worker(gpu);
    let cancel = CancelToken::new();
    let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
    let mut cache = ferrocut_engine::graph::FrameCache::new(16);
    let frame = compiled
        .graph
        .evaluate(compiled.output, at, &mut ctx, &mut cache)
        .unwrap();
    ctx.flush();
    frame.to_cpu_frame(gpu).unwrap()
}

#[test]
fn compiled_reverse_masks_preserve_frame_keys_and_premultiplied_gpu_pixels_after_split_trim() {
    let tl=Timeline::from_json(&json!({"output":{"width":24,"height":16,"fps":"24"},
        "tracks":[{"name":"V1","clips":[{"id":"masked","start":1,"duration":2,
            "source_in":"10/3","speed":"-1/2","opacity":"3/4",
            "generator":{"type":"solid","color":[1,1,1,"1/2"]},
            "masks":[{"geometry":{"type":"rectangle","x":{"keyframes":[{"t":2,"v":2},{"t":4,"v":14}]},
                "y":2,"width":8,"height":10},"opacity":"1/2","feather":3,"expansion":"1/2"}]}]}]}).to_string()).unwrap();
    let split = apply(
        &tl,
        &parse_ops(r#"[{"op":"split","clip":"masked","at":2}]"#).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0;
    let trim = apply(
        &tl,
        &parse_ops(r#"[{"op":"trim","clip":"masked","edge":"in","delta":"1/2"}]"#).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0;
    let compiled = [
        compile(&tl).unwrap(),
        compile(&split).unwrap(),
        compile(&trim).unwrap(),
    ];
    let at = time("5/2");
    let key = compiled[0].graph.frame_key(compiled[0].output, at);
    for c in &compiled[1..] {
        assert_eq!(key, c.graph.frame_key(c.output, at));
    }
    for (name, gpu) in adapters() {
        for c in &compiled {
            let frame = render(gpu, c, at);
            assert_eq!(frame.alpha, AlphaMode::Premultiplied);
            assert_eq!(frame.data_window, PixelRect::full(24, 16));
            for (i, pixel) in frame.image.pixels.as_chunks::<4>().0.iter().enumerate() {
                let p = [(i % 24) as f64 + 0.5, (i / 24) as f64 + 0.5];
                let expected =
                    0.375 * 0.5 * falloff(rectangle_sd(p, [5.5, 2.0, 8.0, 10.0]) + 0.5, 3.0);
                for actual in pixel {
                    assert!(
                        (actual.to_f64() - expected).abs() < 0.001,
                        "{name} pixel{i}: {} vs{expected}",
                        actual.to_f32()
                    );
                }
            }
        }
    }
}

#[test]
fn native_masks_precede_blur_and_follow_the_layer_transform_on_gpu() {
    let tl=Timeline::from_json(&json!({"output":{"width":32,"height":24,"fps":"24"},
        "tracks":[{"name":"V1","clips":[{"id":"masked","start":0,"duration":1,"opacity":"3/4",
            "generator":{"type":"solid","color":[1,1,1,"1/2"]},
            "masks":[{"geometry":{"type":"rectangle","x":6,"y":6,"width":8,"height":8},"opacity":"1/2"}],
            "effects":[{"type":"gaussian_blur","sigma":1}],
            "transform":{"position":[20,14]}}]}]}).to_string()).unwrap();
    let compiled = compile(&tl).unwrap();
    let raw: Vec<f64> = (-3i32..=3)
        .map(|k| (-(k as f64).powi(2) * 0.5).exp())
        .collect();
    let total: f64 = raw.iter().sum();
    let weights: Vec<f64> = raw.into_iter().map(|v| v / total).collect();
    for (name, gpu) in adapters() {
        let frame = render(gpu, &compiled, time("1/2"));
        for (i, pixel) in frame.image.pixels.as_chunks::<4>().0.iter().enumerate() {
            let x = (i % 32) as i32 - 4;
            let y = (i / 32) as i32 - 2;
            let mut alpha = 0.0;
            for dy in -3..=3 {
                for dx in -3..=3 {
                    if (6..14).contains(&(x + dx)) && (6..14).contains(&(y + dy)) {
                        alpha += 0.1875 * weights[(dx + 3) as usize] * weights[(dy + 3) as usize];
                    }
                }
            }
            for actual in pixel {
                assert!(
                    (actual.to_f64() - alpha).abs() < 0.001,
                    "{name} pixel{i}: {} vs{alpha}",
                    actual.to_f32()
                );
            }
        }
        // Outside the translated hard mask, blur must still expose a soft edge.
        assert!(frame.image.pixels[(10 * 32 + 9) * 4 + 3].to_f32() > 0.01);
        assert_eq!(frame.image.pixels[(3 * 32 + 3) * 4 + 3], f16::ZERO);
    }
}

#[test]
fn indexed_mask_edits_and_source_expressions_bake_into_the_same_native_clock() {
    let tl=Timeline::from_json(&json!({"output":{"width":24,"height":16,"fps":"24"},
        "tracks":[{"name":"V1","clips":[{"id":"masked","start":1,"duration":2,"source_in":1,"speed":2,
            "generator":{"type":"solid","color":[1,1,1]},
            "masks":[{"geometry":{"type":"rectangle","x":4,"y":2,"width":8,"height":10}}]}]}]}).to_string()).unwrap();
    let ops=parse_ops(&json!([
        {"op":"set_param","clip":"masked","param":"masks.0.geometry.x","value":{"expression":"time * 2"}},
        {"op":"set_param","clip":"masked","param":"masks.0.opacity","value":{"expression":"0.5"}}
    ]).to_string()).unwrap();
    let (edited, _) = apply(&tl, &ops, &mut MediaLengths::unbounded()).unwrap();
    let baked = bake(&edited).unwrap();
    let mut fixed = rect("4", "2", "8", "10");
    fixed.opacity = constant("1/2");
    assert_eq!(
        baked.tracks[0].clips[0]
            .masks
            .hash_bytes_at(time("2"))
            .unwrap(),
        stack(vec![fixed]).hash_bytes_at(time("2")).unwrap()
    );
    let mut changed = tl.clone();
    let replace = parse_ops(
        &json!([{"op":"set_param","clip":"masked","param":"masks.0.geometry",
        "value":{"type":"ellipse","center":[12,8],"radius":[4,3]}}])
        .to_string(),
    )
    .unwrap();
    changed = apply(&changed, &replace, &mut MediaLengths::unbounded())
        .unwrap()
        .0;
    assert_ne!(
        tl.tracks[0].clips[0]
            .masks
            .hash_bytes_at(time("2"))
            .unwrap(),
        changed.tracks[0].clips[0]
            .masks
            .hash_bytes_at(time("2"))
            .unwrap()
    );
    let sparse =
        parse_ops(r#"[{"op":"set_param","clip":"masked","param":"masks.9.opacity","value":1}]"#)
            .unwrap();
    assert!(apply(&tl, &sparse, &mut MediaLengths::unbounded()).is_err());
    assert_eq!(tl.tracks[0].clips[0].masks.masks.len(), 1);
}

#[test]
fn invalid_sampled_mask_has_a_distinct_key_and_cannot_reuse_a_warm_valid_gpu_frame() {
    let good = Timeline::from_json(
        &json!({"output":{"width":16,"height":16,"fps":"24"},
        "tracks":[{"name":"V1","clips":[{"id":"masked","start":0,"duration":1,
            "generator":{"type":"solid","color":[1,1,1]},
            "masks":[{"geometry":{"type":"rectangle","x":2,"y":2,"width":10,"height":10}}]}]}]})
        .to_string(),
    )
    .unwrap();
    let mut invalid = good.clone();
    invalid.tracks[0].clips[0].masks.masks[0].opacity =
        serde_json::from_value(json!({"keyframes":[
        {"t":0,"v":0,"interp":{"bezier":[0,4,1,4]}},{"t":1,"v":1}]}))
        .unwrap();
    let good = compile(&good).unwrap();
    let invalid = compile(&invalid).unwrap();
    let at = time("1/2");
    assert_ne!(
        good.graph.frame_key(good.output, at),
        invalid.graph.frame_key(invalid.output, at)
    );
    for (name, gpu) in adapters() {
        let mut worker = worker(gpu);
        let cancel = CancelToken::new();
        let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
        let mut cache = ferrocut_engine::graph::FrameCache::new(8);
        let valid = good
            .graph
            .evaluate(good.output, at, &mut ctx, &mut cache)
            .unwrap();
        let error = invalid
            .graph
            .evaluate(invalid.output, at, &mut ctx, &mut cache)
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Permanent, "{name}");
        assert!(error.message.contains("opacity"), "{error}");
        let again = good
            .graph
            .evaluate(good.output, at, &mut ctx, &mut cache)
            .unwrap();
        assert!(Arc::ptr_eq(&valid, &again));
    }
}
