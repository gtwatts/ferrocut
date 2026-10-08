//! Pinned upstream execution, independent pixel math, timeline/cache and real
//! worker readback/upload. CPU reference cases execute without a GPU.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use effectcraft_effects as ec;
use effectcraft_keyframe::Value as EcValue;
use ferrocut_core::effect::{Canvas, EffectParams, EffectRequest, ParamValue};
use ferrocut_core::{
    AdapterPreference, AlphaMode, CancelToken, ColorSpace, CpuFrame, CpuImage, Frame, GpuContext,
    PixelRect, Rational, RationalTime, RenderCtx, WorkerState,
};
use ferrocut_engine::fx::effectcraft::{self, ADAPTER_VERSION};
use ferrocut_engine::fx::{EffectStack, ParsedEffect, VideoEffectSpec, set_effect_param};
use ferrocut_engine::{
    Timeline,
    compile::compile,
    edit::{MediaLengths, apply, parse_ops},
    expr::bake,
};
use half::f16;
use serde_json::{Value, json};

const W: u32 = 32;
const H: u32 = 24;

fn time(s: &str) -> RationalTime {
    RationalTime(s.parse().unwrap())
}
fn parsed(v: Value) -> ParsedEffect {
    let s: VideoEffectSpec = serde_json::from_value(v).unwrap();
    ParsedEffect::parse(&s).unwrap()
}
fn picture(w: u32, h: u32, window: PixelRect) -> CpuFrame {
    let colors = [
        [0.0, 0.0, 0.0, 0.0],
        [0.9, 0.1, 0.05, 1.0],
        [0.1, 0.3, 0.7, 0.25],
        [0.18, 0.18, 0.18, 1.0],
        [1.5, 0.5, 0.25, 1.0],
        [-0.125, 0.5, 0.1, 0.75],
        [0.0, 1.0, 0.0, 1.0],
        [0.2, 0.75, 0.3, 0.6],
        [0.8, 0.2, 0.1, 0.0001],
    ];
    let mut pixels = Vec::new();
    for y in 0..window.height {
        for x in 0..window.width {
            let c = colors[((x + 3 * y) as usize) % colors.len()];
            pixels.extend([c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]].map(f16::from_f32));
        }
    }
    CpuFrame {
        width: w,
        height: h,
        data_window: window,
        pixel_aspect: Rational::new(4, 3),
        color_space: ColorSpace::acescg(),
        alpha: AlphaMode::Premultiplied,
        image: Arc::new(CpuImage { pixels }),
    }
}
fn request(w: u32, h: u32, region: PixelRect) -> EffectRequest {
    EffectRequest {
        time: time("7/3"),
        param_time: time("1/3"),
        effect_time: time("1/3"),
        region,
        canvas: Canvas {
            width: w,
            height: h,
            pixel_aspect: 4.0 / 3.0,
            frame_rate: 24.0,
        },
        label: "review".into(),
    }
}
fn run(effect: &ParsedEffect, input: &CpuFrame, req: &EffectRequest) -> CpuFrame {
    effectcraft::render_cpu(
        &effect.spec.kind,
        input,
        &effect.sample(req.param_time),
        req,
        || Ok(()),
    )
    .unwrap()
}

/// Independent adapter reference: call the pinned public upstream function,
/// preserving upstream defaults/typed values and absolute pixel coordinates.
/// It deliberately does not call the production parameter/conversion helpers.
fn upstream(
    id: &str,
    input: &CpuFrame,
    req: &EffectRequest,
    overrides: &[(&str, EcValue)],
    seed: u32,
) -> CpuFrame {
    let spec = ec::find(id).unwrap();
    let size = [input.width as f64, input.height as f64];
    let mut p = ec::Params {
        values: spec
            .params
            .iter()
            .map(|ps| (ps.id.to_owned(), ec::default_value(ps, size)))
            .collect(),
    };
    for (k, v) in overrides {
        p.values.insert((*k).to_owned(), v.clone());
    }
    let normal = input
        .data_window
        .union(&PixelRect::full(input.width, input.height));
    let mut data = vec![[0.0; 4]; normal.width as usize * normal.height as usize];
    for y in 0..input.data_window.height as usize {
        for x in 0..input.data_window.width as usize {
            let i = (y * input.data_window.width as usize + x) * 4;
            let px = [0, 1, 2, 3].map(|k| input.image.pixels[i + k].to_f32());
            let dx = (input.data_window.x as i64 - normal.x as i64 + x as i64) as usize;
            let dy = (input.data_window.y as i64 - normal.y as i64 + y as i64) as usize;
            data[dy * normal.width as usize + dx] = if px[3] == 0.0 { [0.0; 4] } else { px };
        }
    }
    let ctx = ec::EffectCtx {
        params: &p,
        time: req.effect_time.0.to_f64(),
        layer_size: size,
        seed,
        adjustment: false,
        env: ec::EffectEnv {
            comp_time: req.time.0.to_f64(),
            frame_rate: req.canvas.frame_rate,
            working_space: Some(effectcraft_color::ColorSpace::AcesCg),
            working_linear: true,
            ..Default::default()
        },
    };
    let out = ec::apply(
        spec,
        &ctx,
        ec::Buf {
            img: ec::Image {
                width: normal.width,
                height: normal.height,
                data,
            },
            offset: [-(normal.x as f64), -(normal.y as f64)],
            scale: 1.0,
        },
    );
    let mut pixels = Vec::new();
    for y in 0..req.region.height as i64 {
        for x in 0..req.region.width as i64 {
            let sx = req.region.x as i64 + x + out.offset[0] as i64;
            let sy = req.region.y as i64 + y + out.offset[1] as i64;
            let px =
                if sx >= 0 && sy >= 0 && sx < out.img.width as i64 && sy < out.img.height as i64 {
                    out.img.data[sy as usize * out.img.width as usize + sx as usize]
                } else {
                    [0.0; 4]
                };
            let a = px[3].clamp(0.0, 1.0);
            let px = if px[3] <= 0.0 {
                [0.0; 4]
            } else {
                [px[0] * a / px[3], px[1] * a / px[3], px[2] * a / px[3], a]
            };
            pixels.extend(px.map(|v| f16::from_f32(v.clamp(-65504.0, 65504.0))));
        }
    }
    CpuFrame {
        width: input.width,
        height: input.height,
        data_window: req.region,
        pixel_aspect: input.pixel_aspect,
        color_space: ColorSpace::acescg(),
        alpha: AlphaMode::Premultiplied,
        image: Arc::new(CpuImage { pixels }),
    }
}
fn compare(a: &CpuFrame, b: &CpuFrame, label: &str) {
    assert_eq!(a.data_window, b.data_window, "{label}");
    assert_eq!(a.image.pixels.len(), b.image.pixels.len(), "{label}");
    for (i, (a, b)) in a.image.pixels.iter().zip(&b.image.pixels).enumerate() {
        assert!(
            a.is_finite() && b.is_finite(),
            "{label} nonfinite channel {i}"
        );
        let d = (a.to_f32() - b.to_f32()).abs();
        assert!(
            d <= 0.001 * a.to_f32().abs().max(b.to_f32().abs()).max(1.0),
            "{label} {i}: {a} != {b}"
        );
    }
}

#[test]
fn complete_catalogue_is_unique_and_every_connected_default_reaches_upstream() {
    let catalog = effectcraft::catalog();
    assert_eq!(catalog["entry_count"], 306);
    let connected = effectcraft::all();
    assert_eq!(
        connected.len(),
        170,
        "connected count is pinned by the source-linked integration index"
    );
    let ids: HashSet<_> = connected.iter().map(|e| e.type_name()).collect();
    assert_eq!(ids.len(), connected.len());
    let fixture = picture(W, H, PixelRect::full(W, H));
    let req = request(W, H, PixelRect::full(W, H));
    for entry in catalog["entries"].as_array().unwrap() {
        let id = entry["id"].as_str().unwrap();
        if entry["status"] == "connected" {
            assert!(ids.contains(id));
            let s: VideoEffectSpec = serde_json::from_value(json!({"type":id})).unwrap();
            let fx = ParsedEffect::parse(&s).unwrap_or_else(|e| panic!("{id} default parse: {e}"));
            assert_eq!(fx.effect.version(), ADAPTER_VERSION);
            let actual =
                effectcraft::render_cpu(id, &fixture, &fx.sample(req.param_time), &req, || Ok(()))
                    .unwrap_or_else(|e| panic!("{id} default render: {e}"));
            let expected = upstream(id, &fixture, &req, &[], 0);
            compare(&actual, &expected, id);
        } else {
            assert!(!ids.contains(id), "unsupported entry registered: {id}");
            assert!(
                !entry["reason"].as_str().unwrap().is_empty(),
                "missing reason: {id}"
            );
        }
    }
    eprintln!(
        "EffectCraft: {} connected, {} explicitly unsupported",
        connected.len(),
        306 - connected.len()
    );
}

#[test]
fn nondefault_parameters_and_normalized_points_match_upstream_at_two_resolutions() {
    let cases = [
        (
            json!({"type":"ec.blur.gaussian","blurriness":4}),
            vec![("blurriness", EcValue::Scalar(4.0))],
        ),
        (
            json!({"type":"ec.stylize.posterize","level":3}),
            vec![("level", EcValue::Scalar(3.0))],
        ),
        (
            json!({"type":"ec.color.exposure","master/exposure":1,"bypassLinearLight":true}),
            vec![
                ("master/exposure", EcValue::Scalar(1.0)),
                ("bypassLinearLight", EcValue::Bool(true)),
            ],
        ),
        (
            json!({"type":"ec.noise.noise","amount":35,"_seed":19}),
            vec![("amount", EcValue::Scalar(35.0))],
        ),
        (
            json!({"type":"ec.distort.twirl","angle":72,"center":["0.25","0.75"],"radius":12}),
            vec![
                ("angle", EcValue::Scalar(72.0)),
                ("radius", EcValue::Scalar(12.0)),
            ],
        ),
    ];
    for (w, h) in [(W, H), (48, 20)] {
        let input = picture(w, h, PixelRect::new(-3, 2, w - 4, h - 3));
        let req = request(w, h, PixelRect::new(-2, -1, w + 4, h + 2));
        for (v, overrides) in &cases {
            let fx = parsed(v.clone());
            let mut overrides = overrides.clone();
            if fx.spec.kind == "ec.distort.twirl" {
                overrides.push(("center", EcValue::Vec2([w as f64 * 0.25, h as f64 * 0.75])));
            }
            compare(
                &run(&fx, &input, &req),
                &upstream(
                    &fx.spec.kind,
                    &input,
                    &req,
                    &overrides,
                    if fx.spec.kind == "ec.noise.noise" {
                        19
                    } else {
                        0
                    },
                ),
                &fx.spec.kind,
            );
        }
    }
}

#[test]
fn exposure_linear_bypass_preserves_hdr_negative_and_premultiplied_alpha() {
    let input = picture(W, H, PixelRect::full(W, H));
    let req = request(W, H, PixelRect::full(W, H));
    let out = run(
        &parsed(json!({"type":"ec.color.exposure","master/exposure":1,"bypassLinearLight":true})),
        &input,
        &req,
    );
    for (a, b) in out
        .image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(input.image.pixels.as_chunks::<4>().0)
    {
        assert_eq!(a[3], b[3]);
        for k in 0..3 {
            assert!(
                (a[k].to_f32() - b[k].to_f32() * 2.0).abs() < 0.002,
                "{a:?} {b:?}"
            );
        }
        if a[3] == f16::ZERO {
            assert_eq!(a, &[f16::ZERO; 4]);
        }
    }
    assert!(out.image.pixels.iter().any(|p| p.to_f32() > 1.0));
    assert!(out.image.pixels.iter().any(|p| p.to_f32() < 0.0));
}

#[test]
fn every_connected_effect_accepts_a_nondefault_typed_control_and_matches_reference() {
    let input = picture(W, H, PixelRect::full(W, H));
    let req = request(W, H, PixelRect::full(W, H));
    for adapter in effectcraft::all() {
        let id = adapter.type_name();
        let upstream_spec = ec::find(id).unwrap();
        let fx = parsed(json!({"type":id}));
        let p = fx.sample(req.param_time);
        let mut changed = None;
        for ps in &upstream_spec.params {
            let Some(meta) = adapter.params().iter().find(|m| m.name == ps.id) else {
                continue;
            };
            match p.get(ps.id).unwrap() {
                ParamValue::Scalar(d) => {
                    let v = if meta.max.is_none_or(|max| *d + 0.5 <= max) {
                        *d + 0.5
                    } else if meta.min.is_none_or(|min| *d - 0.5 >= min) {
                        *d - 0.5
                    } else {
                        continue;
                    };
                    changed = Some((ps.id, json!(v.to_string()), EcValue::Scalar(v)));
                    break;
                }
                ParamValue::Vec(v) if matches!(ps.default, EcValue::Vec2(_)) => {
                    let mut v = v.clone();
                    v[0] = v[0] * 0.75 + 0.05;
                    let json = json!(v.iter().map(|x| x.to_string()).collect::<Vec<_>>());
                    let point = if matches!(ps.ui, effectcraft_project::ParamUi::Point) {
                        [v[0] * W as f64, v[1] * H as f64]
                    } else {
                        [v[0], v[1]]
                    };
                    changed = Some((ps.id, json, EcValue::Vec2(point)));
                    break;
                }
                ParamValue::Vec(v) if matches!(ps.default, EcValue::Color(_)) => {
                    let mut v = v.clone();
                    v[0] = v[0] * 0.5 + 0.1;
                    changed = Some((
                        ps.id,
                        json!(v.iter().map(|x| x.to_string()).collect::<Vec<_>>()),
                        EcValue::Color([v[0], v[1], v[2], v[3]]),
                    ));
                    break;
                }
                ParamValue::Choice(current) if matches!(ps.default, EcValue::Enum(_)) => {
                    if let Some((index, choice)) = meta
                        .choices
                        .iter()
                        .enumerate()
                        .find(|(_, c)| **c != current.as_str())
                    {
                        changed = Some((ps.id, json!(choice), EcValue::Enum(index as u32)));
                        break;
                    }
                }
                ParamValue::Bool(value) => {
                    changed = Some((ps.id, json!(!value), EcValue::Bool(!value)));
                    break;
                }
                ParamValue::Choice(_) if id == "ec.color.curves" => {
                    changed = Some((ps.id, json!("0,0 1,0.5"), EcValue::Str("0,0 1,0.5".into())));
                    break;
                }
                _ => {}
            }
        }
        let (name, value, expected) =
            changed.unwrap_or_else(|| panic!("no useful change for {id}"));
        let mut spec = fx.spec;
        set_effect_param(&mut spec, name, value).unwrap_or_else(|e| panic!("{id}.{name}: {e}"));
        let fx = ParsedEffect::parse(&spec).unwrap();
        let got = run(&fx, &input, &req);
        compare(
            &got,
            &upstream(id, &input, &req, &[(name, expected)], 0),
            &format!("{id}.{name}"),
        );
    }
}

#[test]
fn curves_and_fixed_enums_render_real_changes_with_clear_malformed_errors() {
    let input = picture(W, H, PixelRect::full(W, H));
    let req = request(W, H, PixelRect::full(W, H));
    let identity = run(&parsed(json!({"type":"ec.color.curves"})), &input, &req);
    compare(&identity, &input, "identity curves");
    let fx = parsed(json!({"type":"ec.color.curves","rgb":"0,0 0.5,0.75 1,1","alpha":"0,0 1,0.5"}));
    let changed = run(&fx, &input, &req);
    assert_ne!(changed.image.pixels, identity.image.pixels);
    compare(
        &changed,
        &upstream(
            "ec.color.curves",
            &input,
            &req,
            &[
                ("rgb", EcValue::Str("0,0 0.5,0.75 1,1".into())),
                ("alpha", EcValue::Str("0,0 1,0.5".into())),
            ],
            0,
        ),
        "curves",
    );
    let fx = parsed(json!({"type":"ec.channel.invert","channel":"Alpha"}));
    assert_ne!(run(&fx, &input, &req).image.pixels, input.image.pixels);
    for v in [
        json!({"type":"ec.channel.invert","channel":"Imaginary"}),
        json!({"type":"ec.channel.invert","channel":{"expression":"1"}}),
        json!({"type":"ec.color.curves","rgb":"bad points"}),
        json!({"type":"ec.color.curves","rgb":"0,0 0,1"}),
        json!({"type":"ec.color.curves","rgb":"0,0 NaN,1"}),
        json!({"type":"ec.color.psarbitrarymap","map":"NaN 0.5"}),
        json!({"type":"ec.color.curves","channel":1}),
        json!({"type":"ec.distort.transform","useCompositionShutterAngle":false,"shutterAngle":180}),
    ] {
        let s: VideoEffectSpec = serde_json::from_value(v.clone()).unwrap();
        assert!(
            ParsedEffect::parse(&s).is_err(),
            "malformed effect accepted: {v}"
        );
    }
}

#[test]
fn padding_prediction_preserves_overscan_for_nondefault_spatial_effects() {
    let input = picture(W, H, PixelRect::new(-4, 2, W + 3, H - 2));
    let mut req = request(W, H, PixelRect::new(-32, -32, W + 64, H + 64));
    let cases = [
        (
            json!({"type":"ec.blur.cccross","radiusX":3,"radiusY":5}),
            vec![
                ("radiusX", EcValue::Scalar(3.0)),
                ("radiusY", EcValue::Scalar(5.0)),
            ],
        ),
        (
            json!({"type":"ec.distort.turbulentdisplace","amount":6,"resizeLayer":true}),
            vec![
                ("amount", EcValue::Scalar(6.0)),
                ("resizeLayer", EcValue::Bool(true)),
            ],
        ),
        (
            json!({"type":"ec.distort.ccslant","slant":20,"height":120}),
            vec![
                ("slant", EcValue::Scalar(20.0)),
                ("height", EcValue::Scalar(120.0)),
            ],
        ),
        (
            json!({"type":"ec.distort.warp","bend":20}),
            vec![("bend", EcValue::Scalar(20.0))],
        ),
        (
            json!({"type":"ec.distort.ccpowerpin","topLeft":["-0.25","-0.25"]}),
            vec![("topLeft", EcValue::Vec2([-8.0, -6.0]))],
        ),
    ];
    for (v, overrides) in cases {
        let fx = parsed(v);
        let p = fx.sample(req.param_time);
        req.region = fx.effect.output_window(input.data_window, &p, req.canvas);
        assert!(
            req.region.x < input.data_window.x || req.region.y < input.data_window.y,
            "{} did not grow",
            fx.spec.kind
        );
        compare(
            &run(&fx, &input, &req),
            &upstream(&fx.spec.kind, &input, &req, &overrides, 0),
            &fx.spec.kind,
        );
    }
}

#[test]
fn hostile_pixels_parameters_bounds_and_workloads_fail_without_inert_fallback() {
    let input = picture(W, H, PixelRect::full(W, H));
    let req = request(W, H, PixelRect::full(W, H));
    let error = |id: &str, input: &CpuFrame, p: &EffectParams, req: &EffectRequest| {
        effectcraft::render_cpu(id, input, p, req, || Ok(()))
            .unwrap_err()
            .message
    };
    assert!(error("ec.time.echo", &input, &EffectParams::new(), &req).contains("unsupported"));
    for (name, value) in [
        ("_seed", ParamValue::Bool(true)),
        ("_seed", ParamValue::Scalar(f64::INFINITY)),
        ("master/exposure", ParamValue::Scalar(f64::NAN)),
        ("misspelled", ParamValue::Scalar(1.0)),
    ] {
        let mut p = EffectParams::new();
        p.insert(name, value);
        assert!(!error("ec.color.exposure", &input, &p, &req).is_empty());
    }
    let mut p = EffectParams::new();
    p.insert("rgb", ParamValue::Choice("0,0 ".repeat(1025)));
    assert!(error("ec.color.curves", &input, &p, &req).contains("4096"));
    let mut p = EffectParams::new();
    p.insert("red", ParamValue::Vec(vec![0.0; 9]));
    assert!(!error("ec.color.exposure", &input, &p, &req).is_empty());
    let mut wrong = input.clone();
    wrong.color_space = ColorSpace::new("sRGB");
    assert!(error("ec.color.exposure", &wrong, &EffectParams::new(), &req).contains("ACEScg"));
    for invalid in [f16::NAN, f16::INFINITY] {
        let mut wrong = input.clone();
        Arc::make_mut(&mut wrong.image).pixels[0] = invalid;
        assert!(
            error("ec.color.exposure", &wrong, &EffectParams::new(), &req).contains("nonfinite")
        );
    }
    let mut wrong = input.clone();
    Arc::make_mut(&mut wrong.image).pixels[3] = f16::from_f32(2.0);
    assert!(error("ec.color.exposure", &wrong, &EffectParams::new(), &req).contains("alpha"));
    let mut wrong = input.clone();
    wrong.data_window.x = i32::MIN;
    assert!(error("ec.color.exposure", &wrong, &EffectParams::new(), &req).contains("coordinates"));
    let mut wrong = input.clone();
    Arc::make_mut(&mut wrong.image).pixels.pop();
    assert!(error("ec.color.exposure", &wrong, &EffectParams::new(), &req).contains("storage"));
    let mut wrong_req = req.clone();
    wrong_req.canvas.frame_rate = 0.0;
    assert!(
        error(
            "ec.color.exposure",
            &input,
            &EffectParams::new(),
            &wrong_req
        )
        .contains("rate")
    );
    let mut wrong_req = req.clone();
    wrong_req.region.width = u32::MAX;
    assert!(
        !error(
            "ec.color.exposure",
            &input,
            &EffectParams::new(),
            &wrong_req
        )
        .is_empty()
    );
    let mut p = EffectParams::new();
    p.insert("radiusX", ParamValue::Scalar(100.0));
    assert!(error("ec.blur.cccross", &input, &p, &req).contains("padding"));
    // These fail before allocating their advertised hostile frame storage.
    let mut huge = input.clone();
    huge.width = 8192;
    huge.height = 8192;
    let huge_req = request(8192, 8192, PixelRect::full(8192, 8192));
    assert!(error("ec.color.exposure", &huge, &EffectParams::new(), &huge_req).contains("canvas"));
    // Real moderate storage, excessive quadratic kernel work.
    let medium = picture(128, 128, PixelRect::full(128, 128));
    let p = parsed(json!({"type":"ec.noise.median","radius":100})).sample(time("0"));
    assert!(
        error(
            "ec.noise.median",
            &medium,
            &p,
            &request(128, 128, PixelRect::full(128, 128))
        )
        .contains("work")
    );
}

#[test]
fn cancellation_is_checked_before_during_copies_and_after_the_upstream_call() {
    let input = picture(W, H, PixelRect::full(W, H));
    let req = request(W, H, PixelRect::full(W, H));
    let p = parsed(json!({"type":"ec.color.exposure"})).sample(time("0"));
    for stop in [0, H + 1, H + 2, H + 3] {
        let count = std::cell::Cell::new(0);
        let e = effectcraft::render_cpu("ec.color.exposure", &input, &p, &req, || {
            let n = count.get();
            count.set(n + 1);
            if n >= stop {
                Err(ferrocut_core::NodeError::cancelled("test cancellation"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(e.kind, ferrocut_core::ErrorKind::Cancelled);
    }
}

#[test]
fn intrinsic_time_seed_animation_frame_rate_and_parameter_edits_change_cache_keys() {
    let spec: VideoEffectSpec =
        serde_json::from_value(json!({"type":"ec.noise.noise","amount":30,"_seed":7})).unwrap();
    let stack = EffectStack::new("noise", std::slice::from_ref(&spec), time("2")).unwrap();
    assert_ne!(
        stack.hash_bytes_at(time("2")),
        stack.hash_bytes_at(time("5/2"))
    );
    let early = EffectStack::new("same", std::slice::from_ref(&spec), time("0")).unwrap();
    assert_eq!(
        stack.hash_bytes_at(time("5/2")),
        early.hash_bytes_at(time("1/2"))
    );
    let changed_rate = stack.clone().with_frame_rate(Rational::from_int(24));
    assert_ne!(
        stack.hash_bytes_at(time("5/2")),
        changed_rate.hash_bytes_at(time("5/2"))
    );
    let mut edited = spec;
    set_effect_param(&mut edited, "_seed", json!(8)).unwrap();
    assert_ne!(
        early.hash_bytes_at(time("1/2")),
        EffectStack::new("same", &[edited], time("0"))
            .unwrap()
            .hash_bytes_at(time("1/2"))
    );
    let pulse = parsed(json!({"type":"ec.distort.ccripplepulse","pulseLevel":100,"amplitude":100}));
    assert!(pulse.effect.time_dependent());
    let pulses = EffectStack::new("pulse", &[pulse.spec], time("0")).unwrap();
    assert_ne!(
        pulses.hash_bytes_at(time("0")),
        pulses.hash_bytes_at(time("1/2"))
    );
    let input = picture(W, H, PixelRect::full(W, H));
    let mut req = request(W, H, PixelRect::full(W, H));
    req.param_time = time("0");
    req.effect_time = time("0");
    let fx = &early.effects[0];
    let a = run(fx, &input, &req);
    req.param_time = time("1/2");
    req.effect_time = time("1/2");
    let b = run(fx, &input, &req);
    assert_ne!(a.image.pixels, b.image.pixels);
    req.label = "renamed".into();
    assert_eq!(run(fx, &input, &req).image.pixels, b.image.pixels);
    let anim = parsed(
        json!({"type":"ec.color.exposure","master/exposure":{"keyframes":[{"t":0,"v":0},{"t":1,"v":2}]},"bypassLinearLight":true}),
    );
    assert_eq!(anim.sample(time("1/2")).scalar("master/exposure"), 1.0);
    let stack = EffectStack::new("grade", std::slice::from_ref(&anim.spec), time("0")).unwrap();
    assert_ne!(
        stack.hash_bytes_at(time("0")),
        stack.hash_bytes_at(time("1/2"))
    );
    let shifted = ParsedEffect::parse(&anim.spec.shifted(Rational::from_int(-1)).unwrap()).unwrap();
    assert_eq!(
        shifted.sample(time("0")).scalar("master/exposure"),
        anim.sample(time("1")).scalar("master/exposure")
    );
}

fn adapters() -> Vec<(&'static str, &'static GpuContext)> {
    static DEFAULT: OnceLock<Option<GpuContext>> = OnceLock::new();
    static CPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    [
        ("default", &DEFAULT, AdapterPreference::default()),
        ("cpu", &CPU, AdapterPreference::Cpu),
    ]
    .into_iter()
    .filter_map(|(name, cell, pref)| {
        cell.get_or_init(|| match GpuContext::new(pref) {
            Ok(gpu) => {
                eprintln!(
                    "EffectCraft adapter {name}: {:?} {:?} {:?}",
                    gpu.info.name, gpu.info.backend, gpu.info.device_type
                );
                Some(gpu)
            }
            Err(e) => {
                eprintln!("SKIP EffectCraft adapter {name}: {e}");
                None
            }
        })
        .as_ref()
        .map(|gpu| (name, gpu))
    })
    .collect()
}

#[test]
fn actual_gpu_readback_upload_and_worker_recreation_match_cpu_pixels() {
    let input = picture(W, H, PixelRect::new(-4, 3, W + 2, H - 4));
    let mut req = request(W, H, PixelRect::new(-8, -8, W + 16, H + 16));
    let fx = parsed(json!({"type":"ec.blur.gaussian","blurriness":4}));
    let expected = run(&fx, &input, &req);
    for (name, gpu) in adapters() {
        req = request(W, H, PixelRect::new(-8, -8, W + 16, H + 16));
        for _ in 0..2 {
            let mut worker = WorkerState::default();
            let cancel = CancelToken::new();
            let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
            let frame = Frame::from_cpu(&input).to_gpu(gpu);
            let result = fx
                .effect
                .render(&mut ctx, &frame, &fx.sample(req.param_time), &req)
                .unwrap();
            ctx.flush();
            let cpu = result.to_cpu_frame(gpu).unwrap();
            compare(&cpu, &expected, name);
            assert_eq!(cpu.pixel_aspect, input.pixel_aspect);
        }
        // Intrinsic animation survives a warm stack/cache, with frame rate
        // and clip-local offset supplied by the ordinary Ferrocut stack.
        let noise = parsed(json!({"type":"ec.noise.noise","amount":30,"_seed":11}));
        let stack = EffectStack::new("clip late", std::slice::from_ref(&noise.spec), time("2"))
            .unwrap()
            .with_frame_rate(Rational::from_int(24));
        let mut worker = WorkerState::default();
        let cancel = CancelToken::new();
        let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
        let source = Arc::new(Frame::from_cpu(&input).to_gpu(gpu));
        let result = stack.apply(&mut ctx, source, time("5/2")).unwrap();
        ctx.flush();
        req = request(W, H, result.data_window);
        req.time = time("5/2");
        req.param_time = time("1/2");
        req.effect_time = time("1/2");
        compare(
            &result.to_cpu_frame(gpu).unwrap(),
            &run(&noise, &input, &req),
            name,
        );
    }
}

#[test]
fn timeline_edit_expressions_retime_and_split_preserve_numeric_effect_clocks() {
    let timeline=Timeline::from_json(&json!({"output":{"width":W,"height":H,"fps":"24"},
        "tracks":[{"name":"V1","clips":[{"id":"title","start":1,"duration":2,"source_in":10,"speed":2,
            "generator":{"type":"solid","color":["0.25","0.125","0.0625"]}}]}]}).to_string()).unwrap();
    let ops=parse_ops(&json!([
        {"op":"add_video_effect","clip":"title","effect":{"type":"ec.color.exposure","id":"grade","bypassLinearLight":true}},
        {"op":"set_video_effect_param","clip":"title","effect":"grade","param":"master/exposure","value":{"expression":"0.5 + time + (comp_time - time - 1.0)"}},
        {"op":"add_video_effect","clip":"title","effect":{"type":"ec.distort.twirl","id":"twirl","angle":0,
            "center":[{"expression":"value + time * 0.1"},"0.5"]}},
    ]).to_string()).unwrap();
    let edited = apply(&timeline, &ops, &mut MediaLengths::unbounded())
        .unwrap()
        .0;
    let baked = bake(&edited).unwrap();
    let grade = ParsedEffect::parse(&baked.tracks[0].clips[0].effects[0]).unwrap();
    assert_eq!(grade.sample(time("1/2")).scalar("master/exposure"), 1.0);
    let twirl = ParsedEffect::parse(&baked.tracks[0].clips[0].effects[1]).unwrap();
    assert!((twirl.sample(time("1/2")).vec2("center").unwrap()[0] - 0.55).abs() < 1e-9);
    let split = apply(
        &edited,
        &parse_ops(r#"[{"op":"split","clip":"title","at":"2"}]"#).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0;
    let baked_split = bake(&split).unwrap();
    let right = ParsedEffect::parse(&baked_split.tracks[0].clips[1].effects[0]).unwrap();
    assert_eq!(
        right.sample(time("1/2")).scalar("master/exposure"),
        grade.sample(time("3/2")).scalar("master/exposure")
    );
    let before = compile(&edited).unwrap();
    let after = compile(&split).unwrap();
    for (name, gpu) in adapters() {
        let mut outputs = Vec::new();
        for compiled in [&before, &after] {
            let mut worker = WorkerState::default();
            worker
                .slot(ferrocut_engine::compositor::compositor_slot(), || {
                    Ok(Arc::new(ferrocut_engine::compositor::Compositor::new(gpu)))
                })
                .unwrap();
            let cancel = CancelToken::new();
            let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
            let mut cache = ferrocut_engine::graph::FrameCache::new(4);
            let f = compiled
                .graph
                .evaluate(compiled.output, time("5/2"), &mut ctx, &mut cache)
                .unwrap();
            ctx.flush();
            outputs.push(f.to_cpu_frame(gpu).unwrap());
        }
        compare(&outputs[0], &outputs[1], name);
    }
}

#[test]
fn split_and_in_trim_preserve_constant_procedural_noise_phase_and_hashes() {
    let tl = Timeline::from_json(
        &json!({"output":{"width":W,"height":H,"fps":"24"},
        "tracks":[{"name":"V1","clips":[{"id":"noise","start":1,"duration":2,
            "source_in":10,"speed":2,"generator":{"type":"solid","color":["0.25","0.125","0.0625"]},
            "effects":[{"type":"ec.noise.noise","amount":30,"_seed":9}]}]}]})
        .to_string(),
    )
    .unwrap();
    let split = apply(
        &tl,
        &parse_ops(r#"[{"op":"split","clip":"noise","at":"2"}]"#).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0;
    let trimmed = apply(
        &tl,
        &parse_ops(r#"[{"op":"trim","clip":"noise","edge":"in","delta":"1/2"}]"#).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0;
    let original = EffectStack::new("original", &tl.tracks[0].clips[0].effects, time("1")).unwrap();
    let right = EffectStack::new("right", &split.tracks[0].clips[1].effects, time("2")).unwrap();
    let cut =
        EffectStack::new("trimmed", &trimmed.tracks[0].clips[0].effects, time("3/2")).unwrap();
    assert_eq!(split.tracks[0].clips[1].effects[0].clock_offset, time("1"));
    assert_eq!(
        trimmed.tracks[0].clips[0].effects[0].clock_offset,
        time("1/2")
    );
    assert_eq!(
        original.hash_bytes_at(time("5/2")),
        right.hash_bytes_at(time("5/2"))
    );
    assert_eq!(
        original.hash_bytes_at(time("5/2")),
        cut.hash_bytes_at(time("5/2"))
    );
    let compiled = [
        compile(&tl).unwrap(),
        compile(&split).unwrap(),
        compile(&trimmed).unwrap(),
    ];
    for (name, gpu) in adapters() {
        let mut frames = Vec::new();
        for c in &compiled {
            let mut worker = WorkerState::default();
            worker
                .slot(ferrocut_engine::compositor::compositor_slot(), || {
                    Ok(Arc::new(ferrocut_engine::compositor::Compositor::new(gpu)))
                })
                .unwrap();
            let cancel = CancelToken::new();
            let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
            let mut cache = ferrocut_engine::graph::FrameCache::new(8);
            // Prime a different intrinsic time first, so stale cache timing is visible.
            c.graph
                .evaluate(c.output, time("9/4"), &mut ctx, &mut cache)
                .unwrap();
            let f = c
                .graph
                .evaluate(c.output, time("5/2"), &mut ctx, &mut cache)
                .unwrap();
            ctx.flush();
            frames.push(f.to_cpu_frame(gpu).unwrap());
        }
        compare(&frames[0], &frames[1], &format!("{name} split phase"));
        compare(&frames[0], &frames[2], &format!("{name} trim phase"));
    }
}

#[test]
fn extreme_clocks_hash_without_panics_and_adapter_time_bounds_fail_cleanly() {
    let mut invalid_hashes = HashSet::new();
    for (offset, at) in [
        ("9223372036854775807", "1"),
        ("9223372036854775807", "2"),
        ("1/9223372036854775807", "1/9223372036854775806"),
    ] {
        let fx = parsed(json!({"type":"ec.noise.noise","amount":30,"clock_offset":offset}));
        let stack = EffectStack::new("hostile clock", &[fx.spec], time("0")).unwrap();
        let hash = stack.hash_bytes_at(time(at));
        assert_eq!(hash, stack.hash_bytes_at(time(at)));
        assert!(
            invalid_hashes.insert(hash),
            "invalid operands must retain distinct keys"
        );
    }
    let valid = parsed(json!({"type":"ec.noise.noise","amount":30}));
    let valid_stack =
        EffectStack::new("valid clock", std::slice::from_ref(&valid.spec), time("0")).unwrap();
    assert!(!invalid_hashes.contains(&valid_stack.hash_bytes_at(time("0"))));

    // Even a static effect must not reuse a valid frame for a clock that its
    // render contract will reject (arithmetic overflow or adapter time bound).
    let grade = parsed(json!({"type":"ec.color.exposure","master/exposure":1,
        "bypassLinearLight":true}));
    let good = EffectStack::new("valid grade", &[grade.spec], time("0")).unwrap();
    let shifted_grade = parsed(json!({"type":"ec.color.exposure","master/exposure":1,
        "bypassLinearLight":true,"clock_offset":"123/2"}));
    let same_pixels =
        EffectStack::new("valid shifted grade", &[shifted_grade.spec], time("0")).unwrap();
    assert_eq!(
        good.hash_bytes_at(time("1")),
        same_pixels.hash_bytes_at(time("1"))
    );
    let hostile = parsed(json!({"type":"ec.color.exposure","master/exposure":1,
        "bypassLinearLight":true,"clock_offset":"9223372036854775807"}));
    let bad = EffectStack::new("invalid grade", &[hostile.spec], time("0")).unwrap();
    for at in ["0", "1"] {
        assert_ne!(good.hash_bytes_at(time(at)), bad.hash_bytes_at(time(at)));
    }

    let input = picture(W, H, PixelRect::full(W, H));
    let params = valid.sample(time("0"));
    let mut req = request(W, H, PixelRect::full(W, H));
    for extreme in ["9223372036854775807", "-9223372036854775808"] {
        req.effect_time = time(extreme);
        assert!(req.effect_time.0.to_f64().is_finite());
        let error = effectcraft::render_cpu("ec.noise.noise", &input, &params, &req, || Ok(()))
            .unwrap_err();
        assert_eq!(error.kind, ferrocut_core::ErrorKind::Permanent);
        assert!(
            error
                .message
                .contains("time exceeds the bounded CPU adapter domain")
        );
    }
    req.effect_time = time("1/9223372036854775807");
    effectcraft::render_cpu("ec.noise.noise", &input, &params, &req, || Ok(())).unwrap();
}

#[test]
fn extreme_clock_shifts_are_fallible_including_i64_min_and_coprime_denominators() {
    let decimal_min = parsed(json!({"type":"ec.noise.noise","amount":30,
        "clock_offset":"-9223372036854775808.0"}));
    assert_eq!(decimal_min.spec.clock_offset, time("-9223372036854775808"));
    assert!(
        serde_json::from_value::<VideoEffectSpec>(json!({"type":"ec.noise.noise",
        "clock_offset":"9223372036854775807.00000000000000000000"}))
        .is_err()
    );
    for (offset, dt) in [
        ("9223372036854775807", "-1"),
        ("-9223372036854775808", "1"),
        ("0", "-9223372036854775808"),
        ("1/9223372036854775807", "1/9223372036854775806"),
    ] {
        let fx = parsed(json!({"type":"ec.noise.noise","amount":30,"clock_offset":offset}));
        let error = fx.spec.shifted(dt.parse().unwrap()).unwrap_err();
        assert!(error.to_string().contains("procedural clock shift"));
        assert_eq!(fx.spec.clock_offset, time(offset));
    }
    // A checked subtraction must not negate i64::MIN in its original width.
    let min = parsed(json!({"type":"ec.noise.noise","amount":30,
        "clock_offset":"-9223372036854775808"}));
    assert_eq!(
        min.spec
            .shifted("-9223372036854775808".parse().unwrap())
            .unwrap()
            .clock_offset,
        time("0")
    );
}

#[test]
fn unrepresentable_effect_clocks_return_contextual_permanent_render_errors() {
    let input = picture(W, H, PixelRect::full(W, H));
    for (name, gpu) in adapters() {
        let source = Arc::new(Frame::from_cpu(&input).to_gpu(gpu));
        for kind in ["ec.noise.noise", "ec.color.exposure"] {
            for (offset, at) in [
                ("9223372036854775807", "1"),
                ("1/9223372036854775807", "1/9223372036854775806"),
            ] {
                let mut spec = json!({"type":kind,"id":"extreme","clock_offset":offset});
                if kind == "ec.noise.noise" {
                    spec["amount"] = json!(30);
                } else {
                    spec["master/exposure"] = json!(1);
                    spec["bypassLinearLight"] = json!(true);
                }
                let fx = parsed(spec);
                let stack = EffectStack::new("hostile clock", &[fx.spec], time("0")).unwrap();
                let mut worker = WorkerState::default();
                let cancel = CancelToken::new();
                let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
                let error = stack.apply(&mut ctx, source.clone(), time(at)).unwrap_err();
                assert_eq!(error.kind, ferrocut_core::ErrorKind::Permanent, "{name}");
                assert!(
                    error.message.contains("hostile clock: effect \"extreme\""),
                    "{error}"
                );
                assert!(error.message.contains("procedural clock"), "{error}");
            }
        }
    }
}

#[test]
fn warm_frame_cache_does_not_hide_static_effect_clock_errors() {
    let tl = Timeline::from_json(
        &json!({"output":{"width":W,"height":H,"fps":"24"},
        "tracks":[{"name":"V1","clips":[{"id":"grade","start":0,"duration":2,
            "generator":{"type":"solid","color":["0.25","0.125","0.0625"]},
            "effects":[{"type":"ec.color.exposure","master/exposure":1,
                "bypassLinearLight":true}]}]}]})
        .to_string(),
    )
    .unwrap();
    let good = compile(&tl).unwrap();
    let mut hostile = tl;
    hostile.tracks[0].clips[0].effects[0].clock_offset = time("9223372036854775807");
    let bad = compile(&hostile).unwrap();
    for (name, gpu) in adapters() {
        let mut worker = WorkerState::default();
        worker
            .slot(ferrocut_engine::compositor::compositor_slot(), || {
                Ok(Arc::new(ferrocut_engine::compositor::Compositor::new(gpu)))
            })
            .unwrap();
        let cancel = CancelToken::new();
        let mut ctx = RenderCtx::new(gpu, &mut worker, &cancel, None);
        let mut cache = ferrocut_engine::graph::FrameCache::new(8);
        for at in ["0", "1"] {
            let valid = good
                .graph
                .evaluate(good.output, time(at), &mut ctx, &mut cache)
                .unwrap();
            let error = bad
                .graph
                .evaluate(bad.output, time(at), &mut ctx, &mut cache)
                .unwrap_err();
            assert_eq!(error.kind, ferrocut_core::ErrorKind::Permanent, "{name}");
            assert!(
                error.message.contains("procedural clock")
                    || error
                        .message
                        .contains("time exceeds the bounded CPU adapter domain"),
                "{error}"
            );
            let again = good
                .graph
                .evaluate(good.output, time(at), &mut ctx, &mut cache)
                .unwrap();
            assert!(Arc::ptr_eq(&valid, &again));
        }
    }
}

#[test]
fn failed_extreme_clock_split_and_trim_leave_the_input_and_prior_edits_untouched() {
    for (offset, delta, duration) in [
        ("9223372036854775807", "1", "2"),
        ("1/9223372036854775807", "1/9223372036854775806", "1"),
    ] {
        let tl = Timeline::from_json(
            &json!({"output":{"width":W,"height":H,"fps":"24"},
            "tracks":[{"name":"V1","clips":[{"id":"noise","start":0,"duration":duration,
                "generator":{"type":"solid","color":["0.25","0.125","0.0625"]},
                "effects":[{"type":"ec.noise.noise","id":"procedural","amount":30,
                    "clock_offset":offset}]}]}]})
            .to_string(),
        )
        .unwrap();
        let original = serde_json::to_value(&tl).unwrap();
        for edit in [
            json!({"op":"split","clip":"noise","at":delta}),
            json!({"op":"trim","clip":"noise","edge":"in","delta":delta}),
        ] {
            let ops = parse_ops(
                &json!([
                    {"op":"set_video_effect_param","clip":"noise","effect":"procedural",
                        "param":"amount","value":45},
                    edit
                ])
                .to_string(),
            )
            .unwrap();
            let error = apply(&tl, &ops, &mut MediaLengths::unbounded()).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("op 1"), "{message}");
            assert!(message.contains("procedural clock shift"), "{message}");
            assert_eq!(serde_json::to_value(&tl).unwrap(), original);
            let (only_first, changes) =
                apply(&tl, &ops[..1], &mut MediaLengths::unbounded()).unwrap();
            assert_eq!(changes.len(), 1);
            assert_ne!(serde_json::to_value(only_first).unwrap(), original);
        }
    }
}
