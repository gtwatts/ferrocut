//! Regression coverage for the familiar editable controls exposed by reused
//! paths. These checks exercise the regular editor and expression compiler.

use ferrocut_core::{Rational, RationalTime};
use ferrocut_engine::Timeline;
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::generator::GeneratorSpec;
use ferrocut_engine::transform::{Scale, TransformSpec};
use ferrocut_engine::vector::VectorSpec;
use serde_json::{Value, json};

fn t(s: &str) -> RationalTime {
    RationalTime(s.parse::<Rational>().unwrap())
}
fn timeline() -> Timeline {
    Timeline::from_json(&json!({
        "output":{"width":64,"height":64,"fps":24,"gop":12},
        "tracks":[{"clips":[{"id":"shape","start":1,"source_in":"1/2","duration":2,
            "generator":{"type":"shape","shape":{
                "geometry":{"type":"polygon","center":[32,32],"points":5,"radius":20},
                "operators":[{"type":"trim"},{"type":"wiggle","size":1},{"type":"twist","angle":0}]
            }}}]}]
    }).to_string()).unwrap()
}
fn edited(tl: &Timeline, value: Value) -> Timeline {
    apply(
        tl,
        &parse_ops(&value.to_string()).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap()
    .0
}
fn spec(tl: &Timeline) -> &VectorSpec {
    let GeneratorSpec::Shape { shape } = tl.tracks[0].clips[0].generator.as_ref().unwrap() else {
        panic!("shape");
    };
    shape
}
fn property<'a>(spec: &'a VectorSpec, path: &str) -> &'a ferrocut_core::Animatable {
    spec.all().into_iter().find(|(p, _)| p == path).unwrap().1
}

#[test]
fn indexed_operator_numeric_and_component_edits_use_existing_typed_slots() {
    let tl = timeline();
    let changed = edited(
        &tl,
        json!([
            {"op":"set_param","clip":"shape","param":"generator.shape.operators.0.end","value":50},
            {"op":"set_param","clip":"shape","param":"generator.shape.operators.2.center.x","value":12},
            {"op":"set_param","clip":"shape","param":"generator.shape.operators.2.center.y","value":24},
            {"op":"set_param","clip":"shape","param":"generator.shape.geometry.points","value":6}
        ]),
    );
    assert_eq!(
        property(spec(&changed), "operators.0.end").eval(t("0")),
        50.0
    );
    assert_eq!(
        property(spec(&changed), "operators.2.center.x").eval(t("0")),
        12.0
    );
    assert_eq!(
        property(spec(&changed), "operators.2.center.y").eval(t("0")),
        24.0
    );
    assert_eq!(
        property(spec(&changed), "geometry.points").eval(t("0")),
        6.0
    );
    assert_ne!(
        spec(&tl).rasterize(t("1/2"), 64, 64).unwrap().image.pixels,
        spec(&changed)
            .rasterize(t("1/2"), 64, 64)
            .unwrap()
            .image
            .pixels
    );
    let restored = edited(
        &changed,
        json!([{"op":"set_param","clip":"shape","param":"generator.shape.operators.0.end","value":100}]),
    );
    assert_eq!(
        property(spec(&restored), "operators.0.end").eval(t("0")),
        100.0
    );
    Timeline::from_json(&serde_json::to_string(&changed).unwrap()).unwrap();
}

#[test]
fn indexed_set_keyframes_converts_timeline_seconds_to_source_seconds() {
    let changed = edited(
        &timeline(),
        json!([{"op":"set_keyframes","clip":"shape","param":"generator.shape.operators.0.end",
        "timeline_time":true,"keyframes":[{"t":1,"v":25},{"t":3,"v":75}]}]),
    );
    let end = property(spec(&changed), "operators.0.end");
    assert_eq!(end.eval(t("1/2")), 25.0);
    assert_eq!(end.eval(t("3/2")), 50.0);
    assert_eq!(end.eval(t("5/2")), 75.0);
    let merged = edited(
        &changed,
        json!([{"op":"set_keyframes","clip":"shape","param":"generator.shape.operators.0.end",
        "mode":"merge","keyframes":[{"t":"3/2","v":60}]}]),
    );
    assert_eq!(
        property(spec(&merged), "operators.0.end").eval(t("3/2")),
        60.0
    );
    assert_eq!(
        property(spec(&merged), "operators.0.end").eval(t("1/2")),
        25.0
    );
}

#[test]
fn operator_expression_value_defaults_and_source_clock_are_preserved() {
    let changed = edited(
        &timeline(),
        json!([
            {"op":"set_param","clip":"shape","param":"generator.shape.operators.0.end","value":{"expression":"value - time * 10"}},
            {"op":"set_param","clip":"shape","param":"generator.shape.operators.1.speed","value":{"expression":"value + time"}},
            {"op":"set_param","clip":"shape","param":"generator.shape.operators.2.center.x","value":{"expression":"value + time"}}
        ]),
    );
    let baked = ferrocut_engine::expr::bake(&changed).unwrap();
    assert_eq!(
        property(spec(&baked), "operators.0.end").eval(t("1/2")),
        95.0
    );
    assert_eq!(
        property(spec(&baked), "operators.1.speed").eval(t("1/2")),
        2.5
    );
    assert_eq!(
        property(spec(&baked), "operators.2.center.x").eval(t("1/2")),
        0.5
    );
    let a = spec(&baked).rasterize(t("1/2"), 64, 64).unwrap();
    let b = spec(&baked).rasterize(t("3/2"), 64, 64).unwrap();
    assert_ne!(a.image.pixels, b.image.pixels);
}

#[test]
fn invalid_operator_edit_is_atomic_and_cannot_create_sparse_or_wrong_fields() {
    let tl = timeline();
    let original = serde_json::to_value(&tl).unwrap();
    for (path, value) in [
        ("generator.shape.operators.31.end", json!(50)),
        ("generator.shape.operators.1000000000.end", json!(50)),
        ("generator.shape.operators.0.detail", json!(4)),
        ("generator.shape.operators.0.mode", json!("add")),
        ("generator.shape.operators.1.size", json!(-1)),
        ("generator.shape.operators.0.end", json!(101)),
    ] {
        let ops=parse_ops(&json!([
            {"op":"set_param","clip":"shape","param":"generator.shape.geometry.points","value":7},
            {"op":"set_param","clip":"shape","param":path,"value":value}
        ]).to_string()).unwrap();
        assert!(
            apply(&tl, &ops, &mut MediaLengths::unbounded()).is_err(),
            "accepted {path}"
        );
        assert_eq!(serde_json::to_value(&tl).unwrap(), original);
    }
}

#[test]
fn anisotropic_scale_arrays_never_deserialize_as_uniform_expressions() {
    for value in [
        json!(["3/2", "3/4"]),
        json!([1, 0]),
        json!([{"keyframes":[{"t":0,"v":1},{"t":2,"v":2}]},"3/4"]),
        json!([{"expression":"1 + time","value":1},{"expression":"2 - time","value":2}]),
    ] {
        let scale: Scale = serde_json::from_value(value.clone()).unwrap();
        assert!(
            matches!(scale, Scale::Xy(_)),
            "misparsed {value}: {scale:?}"
        );
        let reserialized = serde_json::to_value(&scale).unwrap();
        let again: Scale = serde_json::from_value(reserialized).unwrap();
        assert_eq!(scale, again);
    }
    let transform: TransformSpec =
        serde_json::from_value(json!({"anchor":[0,0],"position":[0,0],"scale":["3/2","3/4"]}))
            .unwrap();
    let at = transform.at(t("0"), 64, 64);
    assert_eq!(at.scale, [1.5, 0.75]);
    assert_eq!(at.affine(1.0).apply([8.0, 8.0]), [12.0, 6.0]);
    let mut value = serde_json::to_value(timeline()).unwrap();
    value["tracks"][0]["clips"][0]["transform"] = serde_json::to_value(transform).unwrap();
    let parsed = Timeline::from_json(&value.to_string()).unwrap();
    let parsed = Timeline::from_json(&serde_json::to_string(&parsed).unwrap()).unwrap();
    assert_eq!(
        parsed.tracks[0].clips[0]
            .transform
            .as_ref()
            .unwrap()
            .at(t("0"), 64, 64)
            .scale,
        [1.5, 0.75]
    );
    // Uniform expressions remain accepted as objects, with the ordinary
    // expression language evaluated by the engine later.
    assert!(matches!(
        serde_json::from_value::<Scale>(json!({"expression":"1 + time","value":1})).unwrap(),
        Scale::Uniform(_)
    ));
}

#[test]
fn upstream_file_url_unicode_authorities_never_slice_inside_utf8() {
    // The old pinned helper sliced rest[..9] after a byte-length test. Byte 9
    // lies inside the fifth é here, independently of adapter preflight.
    let result =
        std::panic::catch_unwind(|| filmcraft_interchange::file_url_to_path("file://ééééé"));
    assert_eq!(
        result.expect("UTF-8 URL conversion must not panic"),
        "//ééééé"
    );
    assert_eq!(
        filmcraft_interchange::file_url_to_path("file:///tmp/%C3%A9%20clip.mov"),
        "/tmp/é clip.mov"
    );
    assert_eq!(
        filmcraft_interchange::file_url_to_path("file://localhost/tmp/%C3%A9.mov"),
        "/tmp/é.mov"
    );
}

#[test]
fn native_ffv1_duration_includes_the_last_frame_for_interchange_ranges() {
    use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("one-second-native-master.mkv");
    let mut encoder = ChunkEncoder::create(
        &file,
        &EncodeSettings {
            width: 16,
            height: 16,
            fps: Rational::from_int(24),
            gop: 12,
        },
    )
    .unwrap();
    for _ in 0..24 {
        encoder
            .push_bgra(&[0, 0, 255, 255].repeat(16 * 16))
            .unwrap();
    }
    encoder.finish().unwrap();
    let info = ferrocut_engine::media::probe(&file).unwrap();
    assert_eq!(
        info.duration,
        Some(t("1")),
        "24 frames at24fps must provide the1-second source range used by native timeline export, including the final frame's display interval"
    );
}
