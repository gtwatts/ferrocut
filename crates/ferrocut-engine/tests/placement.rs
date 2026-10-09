//! Placement API, inheritance, source coordinates, and frame-local cache impact.
use ferrocut_core::{Rational, RationalTime, RenderNode};
use ferrocut_engine::{
    Timeline,
    compile::compile_with,
    edit::{MediaFacts, MediaLengths, apply, parse_ops},
    nodes::SourceNode,
    placement::Fit,
};
use serde_json::json;

fn timeline() -> Timeline {
    Timeline::from_json(
        &json!({
            "output":{"width":64,"height":64,"fps":24,"gop":12},
            "tracks":[{"name":"V1","clips":[
                {"id":"a","source":"wide.mkv","start":0,"duration":1},
                {"id":"b","source":"wide.mkv","start":1,"duration":1,"fit":"none"},
                {"id":"c","source":"square.mkv","start":2,"duration":1}
            ]}]
        })
        .to_string(),
    )
    .unwrap()
}

fn compiled(tl: &Timeline) -> ferrocut_engine::compile::Compiled {
    compile_with(tl, |p| {
        Ok(SourceNode {
            path: p.clone(),
            file_hash: *blake3::hash(p.to_string_lossy().as_bytes()).as_bytes(),
            width: 64,
            height: if p.ends_with("wide.mkv") { 32 } else { 64 },
            fps: Some(Rational::from_int(24)),
        })
    })
    .unwrap()
}

#[test]
fn default_fit_is_inherited_and_only_visible_changed_placements_rekey() {
    let tl = timeline();
    let before = compiled(&tl);
    assert_eq!(before.placements[0].fit, Fit::Contain);
    let mut changed = tl.clone();
    changed.output.fit = Some(Fit::Cover);
    let after = compiled(&changed);
    for frame in 0..72 {
        let t = RationalTime::from_frames(frame, tl.output.fps);
        assert_eq!(
            before.graph.frame_key(before.output, t) != after.graph.frame_key(after.output, t),
            frame < 24,
            "frame {frame}"
        );
    }
    changed.output.fit = None;
    changed.tracks[0].clips[1].fit = Some(Fit::Stretch);
    let after = compiled(&changed);
    for frame in 0..72 {
        let t = RationalTime::from_frames(frame, tl.output.fps);
        assert_eq!(
            before.graph.frame_key(before.output, t) != after.graph.frame_key(after.output, t),
            (24..48).contains(&frame),
            "frame {frame}"
        );
    }
    assert_eq!(after.warnings.len(), 1);
    assert!(after.warnings[0].contains("clip b"));
    assert_eq!(after.max_layer_size, (64, 64));
}

#[test]
fn edits_use_source_anchor_defaults_and_null_restores_fit_inheritance() {
    let mut media = MediaLengths::unbounded().with_info(|_| {
        Ok(MediaFacts {
            duration: Some(RationalTime::new(10, 1)),
            has_video: true,
            has_audio: false,
            size: Some((64, 32)),
        })
    });
    let ops = parse_ops(&json!([
        {"op":"set_param","param":"output.fit","value":"cover"},
        {"op":"set_param","clip":"a","param":"fit","value":"stretch"},
        {"op":"set_param","clip":"a","param":"fit","value":null},
        {"op":"set_param","clip":"a","param":"transform.anchor.x","value":10},
        {"op":"add_clip","track":"V1","source":"wide.mkv","id":"new","start":3,"duration":1,"fit":"none"}
    ]).to_string()).unwrap();
    let (tl, _) = apply(&timeline(), &ops, &mut media).unwrap();
    let a = &tl.tracks[0].clips[0];
    assert_eq!(a.effective_fit(&tl.output), Fit::Cover);
    let anchor = a.transform.as_ref().unwrap().anchor.as_ref().unwrap();
    assert_eq!(anchor[0].eval(RationalTime::ZERO), 10.0);
    assert_eq!(anchor[1].eval(RationalTime::ZERO), 16.0);
    assert_eq!(tl.tracks[0].clips[3].fit, Some(Fit::Native));
    let op =
        parse_ops(r#"[{"op":"set_param","clip":"a","param":"transform.anchor.x","value":10}]"#)
            .unwrap();
    let err = apply(&timeline(), &op, &mut MediaLengths::unbounded()).unwrap_err();
    assert!(format!("{err:#}").contains("set both components"));
}

#[test]
fn invalid_fit_and_generator_fit_are_rejected() {
    for clip in [
        json!({"id":"bad","source":"wide.mkv","start":0,"duration":1,"fit":"fill"}),
        json!({"id":"bad","generator":{"type":"solid","color":[1,0,0]},"start":0,"duration":1,"fit":"contain"}),
        json!({"id":"bad","adjustment":true,"start":0,"duration":1,"fit":"cover"}),
    ] {
        let value = json!({"output":{"width":64,"height":32,"fps":24},"tracks":[{"clips":[clip]}]});
        assert!(Timeline::from_json(&value.to_string()).is_err());
    }
}

#[test]
fn native_decode_requests_its_texture_and_upload_limits() {
    let source = SourceNode {
        path: "large.mkv".into(),
        file_hash: [0; 32],
        width: 12_001,
        height: 8_000,
        fps: None,
    };
    let limits = source.gpu_requirements().limits.unwrap();
    assert!(limits.max_texture_dimension_2d >= 12_001);
    assert!(limits.max_buffer_size >= 48_128 * 8_000);
}

#[test]
fn native_anchor_expressions_require_explicit_values_without_probing() {
    let mut value = serde_json::to_value(timeline()).unwrap();
    let transform = &mut value["tracks"][0]["clips"][0]["transform"];
    *transform = json!({"anchor":[{"expression":"value + 1"},16]});
    let error = Timeline::from_json(&value.to_string()).unwrap_err();
    assert!(format!("{error:#}").contains("explicit pre-expression value"));
    value["tracks"][0]["clips"][0]["transform"]["anchor"][0]["value"] = json!(32);
    let tl = Timeline::from_json(&value.to_string()).unwrap();
    let baked = ferrocut_engine::expr::bake(&tl).unwrap();
    assert_eq!(
        baked.tracks[0].clips[0]
            .transform
            .as_ref()
            .unwrap()
            .anchor
            .as_ref()
            .unwrap()[0]
            .eval(RationalTime::ZERO),
        33.0
    );
    value["tracks"][0]["clips"][0]["transform"] =
        json!({"position":[{"expression":"param(\"transform.anchor.x\")"},32]});
    let error = Timeline::from_json(&value.to_string()).unwrap_err();
    assert!(format!("{error:#}").contains("set transform.anchor explicitly"));
}
