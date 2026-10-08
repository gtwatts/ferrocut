//! Retime regression: source properties must use the same clock as the graph.
use ferrocut_core::{Animatable, Rational, RationalTime};
use ferrocut_engine::{Timeline, expr};
use serde_json::{Value, json};

fn timeline(speed: Value, remap: Option<Value>, expression: &str) -> anyhow::Result<Timeline> {
    let mut clip = json!({"id":"g","generator":{"type":"radial_gradient","radius":{"expression":expression}},"start":"3","source_in":"1","duration":"1","speed":speed,"opacity":{"expression":"0.25+0.1*time"}});
    if let Some(remap) = remap {
        clip["time_remap"] = remap;
    }
    Timeline::from_json(&json!({"output":{"width":32,"height":24,"fps":"24"},"tracks":[{"name":"V1","clips":[clip]}]}).to_string())
}
fn value(t: &Timeline, source: RationalTime) -> f64 {
    let baked = expr::bake(t).unwrap();
    let v = serde_json::to_value(&*baked).unwrap();
    serde_json::from_value::<Animatable>(
        v.pointer("/tracks/0/clips/0/generator/radius")
            .unwrap()
            .clone(),
    )
    .unwrap()
    .eval(source)
}

#[test]
fn source_time_comp_time_and_cross_clock_param_follow_forward_reverse_and_remap() {
    let script = "10.0+time+comp_time+param(\"opacity\")";
    let remap = json!({"keyframes":[{"t":"0","v":"1","interp":"ease_in"},{"t":"1","v":"4"}]});
    for t in [
        timeline(json!("2"), None, script).unwrap(),
        timeline(json!("-1"), None, script).unwrap(),
        timeline(json!("1"), Some(remap), script).unwrap(),
    ] {
        let map = t.tracks[0].clips[0].time_map();
        for i in 0..24 {
            let local = RationalTime::from_frames(i, t.output.fps);
            let source = map.source_at(local);
            let expected =
                10.0 + source.0.to_f64() + 3.0 + local.0.to_f64() + 0.25 + 0.1 * local.0.to_f64();
            assert!(
                (value(&t, source) - expected).abs() < 1e-8,
                "frame {i}: got {}, expected {expected}",
                value(&t, source)
            );
        }
    }
}

#[test]
fn frozen_source_expression_can_hold_a_source_function_but_not_changing_timeline_state() {
    let t = timeline(json!("0"), None, "10+time").unwrap();
    assert_eq!(value(&t, RationalTime(Rational::ONE)), 11.0);
    let error = timeline(json!("0"), None, "10+comp_time")
        .unwrap_err()
        .to_string();
    assert!(error.contains("different expression values"), "{error}");
}

#[test]
fn ambiguous_repeated_source_and_expression_timing_combinations_fail_with_guidance() {
    let remap = json!({"keyframes":[{"t":"0","v":"1"},{"t":"1/2","v":"2"},{"t":"1","v":"1"}]});
    let error = timeline(json!("1"), Some(remap), "10+time")
        .unwrap_err()
        .to_string();
    assert!(error.contains("monotonic"), "{error}");
    let error = timeline(json!({"expression":"2"}), None, "10+time")
        .unwrap_err()
        .to_string();
    assert!(error.contains("bake the timing curve"), "{error}");
}

#[test]
fn cross_clip_source_reads_reject_unbaked_timing_but_local_reads_remain_valid() {
    for timing in [
        json!({"speed":{"expression":"2"}}),
        json!({"time_remap":{"expression":"2*time"}}),
    ] {
        let mut b = json!({"id":"b","generator":{"type":"radial_gradient","radius":{"keyframes":[{"t":"0","v":10},{"t":"2","v":30}]}},"start":"0","duration":"1","opacity":"0.8"});
        for (name, value) in timing.as_object().unwrap() {
            b[name] = value.clone();
        }
        let mut document = json!({"output":{"width":32,"height":24,"fps":"24"},"tracks":[{"name":"A","clips":[{"id":"a","generator":{"type":"radial_gradient","radius":{"expression":"10+layer(\"b\").param(\"generator.radius\")"}},"start":"0","duration":"1"}]},{"name":"B","clips":[b]}]});
        let error = Timeline::from_json(&document.to_string())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("cannot read source parameter")
                && error.contains("bake the timing curve"),
            "{error}"
        );
        document["tracks"][0]["clips"][0]["generator"]["radius"] =
            json!({"expression":"10+layer(\"b\").param(\"opacity\")"});
        let t = Timeline::from_json(&document.to_string()).unwrap();
        assert!((value(&t, RationalTime(Rational::new(1, 2))) - 10.8).abs() < 1e-8);
    }
}
