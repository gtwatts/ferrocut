//! Expressions: evaluation, helpers (wiggle, loops, interpolation),
//! references and cycles, precise errors, sandbox limits, splits, and cache
//! keys (expressions bake into keyframes that are part of every frame key).

use ferrocut_core::{Animatable, RationalTime};
use ferrocut_engine::Timeline;
use ferrocut_engine::compile::compile;
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::expr::bake;
use serde_json::{Value, json};

const TL: &str = r#"{
  "output": { "width": 48, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [
      { "id": "a", "start": 0, "duration": "2",
        "generator": { "type": "solid", "color": ["0.9", "0.2", "0.1"] } }
    ]},
    { "name": "V2", "clips": [
      { "id": "b", "start": "1/2", "duration": "1",
        "generator": { "type": "radial_gradient", "center": ["24", "16"], "radius": "10",
          "start_color": ["1", "1", "0.9"], "end_color": ["1", "1", "1", "0"] } }
    ]}
  ]
}"#;

fn base() -> Value {
    serde_json::from_str(TL).unwrap()
}

fn set(v: &mut Value, ptr: &str, x: Value) {
    let (parent, key) = ptr.rsplit_once('/').unwrap();
    let p = v.pointer_mut(parent).unwrap();
    match p {
        Value::Object(o) => {
            o.insert(key.to_string(), x);
        }
        Value::Array(a) => a[key.parse::<usize>().unwrap()] = x,
        _ => panic!("{parent}"),
    }
}

fn tl(edits: &[(&str, Value)]) -> anyhow::Result<Timeline> {
    let mut v = base();
    for (p, x) in edits {
        set(&mut v, p, x.clone());
    }
    Timeline::from_json(&v.to_string())
}

fn err(edits: &[(&str, Value)]) -> String {
    format!("{:#}", tl(edits).unwrap_err())
}

/// Baked value at `ptr` (JSON pointer) and parameter time `t` (seconds).
fn at(tl: &Timeline, ptr: &str, t: &str) -> f64 {
    let b = bake(tl).unwrap();
    let v = serde_json::to_value(&*b).unwrap();
    let a: Animatable = serde_json::from_value(v.pointer(ptr).unwrap().clone()).unwrap();
    a.eval(RationalTime(t.parse().unwrap()))
}

const A_OP: &str = "/tracks/0/clips/0/opacity";
const B_OP: &str = "/tracks/1/clips/0/opacity";

#[test]
fn basic_variables_and_interpolation() {
    let t = tl(&[(A_OP, json!({"expression": "linear(time, 0, 1, 0, 1)"}))]).unwrap();
    assert_eq!(at(&t, A_OP, "0"), 0.0);
    assert_eq!(at(&t, A_OP, "1/2"), 0.5);
    assert_eq!(at(&t, A_OP, "3/2"), 1.0);
    // Clip-local time vs comp_time; frame is an integer; ease helpers.
    let t = tl(&[
        (
            B_OP,
            json!({"expression": "if comp_time >= 1.0 { 1 } else { frame / 24.0 }"}),
        ),
        (A_OP, json!({"expression": "ease(time, 0, 2, 0, 1)"})),
    ])
    .unwrap();
    assert_eq!(at(&t, B_OP, "0"), 0.0);
    assert_eq!(at(&t, B_OP, "1/4"), 0.25);
    assert_eq!(at(&t, B_OP, "1/2"), 1.0); // comp time 1
    assert_eq!(at(&t, A_OP, "1"), 0.5);
    assert!((at(&t, A_OP, "1/2") - 0.15625).abs() < 1e-9);
    // `value` defaults to the parameter default (opacity 1) or the given value.
    let t = tl(&[
        (A_OP, json!({"expression": "value * 0.5"})),
        (B_OP, json!({"expression": "value", "value": {"keyframes": [{"t": "0", "v": "0"}, {"t": "1", "v": "1"}]}})),
    ])
    .unwrap();
    assert_eq!(at(&t, A_OP, "1"), 0.5);
    assert_eq!(at(&t, B_OP, "1/2"), 0.5);
    // Vector components and generator params (source time).
    let t = tl(&[
        (
            "/tracks/1/clips/0/transform",
            json!({"position": [{"expression": "value + time * 10"}, "16"]}),
        ),
        (
            "/tracks/1/clips/0/generator/radius",
            json!({"expression": "5 + time * 2"}),
        ),
    ])
    .unwrap();
    assert_eq!(
        at(&t, "/tracks/1/clips/0/transform/position/0", "1/2"),
        29.0
    );
    assert_eq!(at(&t, "/tracks/1/clips/0/generator/radius", "1/2"), 6.0);
}

#[test]
fn wiggle_is_deterministic_bounded_and_seeded_per_parameter() {
    let w = json!({"expression": "wiggle(3, 20)", "value": "100"});
    let t = tl(&[
        (
            "/tracks/0/clips/0/transform",
            json!({"position": [w.clone(), w.clone()]}),
        ),
        (
            "/tracks/1/clips/0/transform",
            json!({"position": [w.clone(), "16"]}),
        ),
    ])
    .unwrap();
    let ax = "/tracks/0/clips/0/transform/position/0";
    let ay = "/tracks/0/clips/0/transform/position/1";
    let bx = "/tracks/1/clips/0/transform/position/0";
    let (mut moved, mut differs) = (false, false);
    for f in 0..24 {
        let s = format!("{f}/24");
        let (x, y, b) = (at(&t, ax, &s), at(&t, ay, &s), at(&t, bx, &s));
        assert!((x - 100.0).abs() <= 20.0 + 1e-6, "{x}");
        moved |= x != 100.0;
        differs |= x != y && x != b;
    }
    assert!(moved && differs);
    // Same timeline, same bake, bit for bit.
    let b1 = serde_json::to_string(&*bake(&t).unwrap()).unwrap();
    let b2 = serde_json::to_string(
        &*bake(
            &tl(&[
                (
                    "/tracks/0/clips/0/transform",
                    json!({"position": [w.clone(), w.clone()]}),
                ),
                (
                    "/tracks/1/clips/0/transform",
                    json!({"position": [w.clone(), "16"]}),
                ),
            ])
            .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(b1, b2);
    // seed_random changes it; random is per frame and in range.
    let t2 = tl(&[(
        "/tracks/0/clips/0/transform",
        json!({"position": [{"expression": "seed_random(7); wiggle(3, 20)", "value": "100"}, "16"]}),
    )])
    .unwrap();
    assert_ne!(at(&t, ax, "1/3"), at(&t2, ax, "1/3"));
    let t3 = tl(&[(A_OP, json!({"expression": "random()"}))]).unwrap();
    let (r0, r1) = (at(&t3, A_OP, "0"), at(&t3, A_OP, "1/24"));
    assert!((0.0..1.0).contains(&r0) && (0.0..1.0).contains(&r1) && r0 != r1);
    let t4 = tl(&[(
        A_OP,
        json!({"expression": "seed_random(1, true); random()"}),
    )])
    .unwrap();
    assert_eq!(at(&t4, A_OP, "0"), at(&t4, A_OP, "1"));
}

#[test]
fn loops_follow_after_effects() {
    let keys = json!({"keyframes": [{"t": "0", "v": "0"}, {"t": "1/2", "v": "10"}]});
    let pos = |e: &str| {
        tl(&[(
            "/tracks/0/clips/0/transform",
            json!({"position": [{"expression": e, "value": keys}, "16"]}),
        )])
        .unwrap()
    };
    let x = "/tracks/0/clips/0/transform/position/0";
    let t = pos("loop_out()");
    assert_eq!(at(&t, x, "1/4"), 5.0);
    assert_eq!(at(&t, x, "3/4"), 5.0);
    assert_eq!(at(&t, x, "13/12"), at(&t, x, "1/12"));
    let t = pos("loop_out(\"pingpong\")");
    assert_eq!(at(&t, x, "5/8"), 7.5);
    let t = pos("loop_out(\"offset\")");
    assert_eq!(at(&t, x, "3/4"), 15.0);
    let t = pos("loop_out(\"continue\")");
    assert!((at(&t, x, "1") - 20.0).abs() < 1e-6);
    let t = pos("value_at_time(time - 0.25)");
    assert_eq!(at(&t, x, "1/2"), 5.0);
    let e = err(&[(
        "/tracks/0/clips/0/transform",
        json!({"position": [{"expression": "loop_out(\"bounce\")", "value": keys}, "16"]}),
    )]);
    assert!(e.contains("loop type \"bounce\""), "{e}");
}

#[test]
fn references_and_cycles() {
    let t = tl(&[
        (A_OP, json!({"expression": "linear(time, 0, 2, 0, 1)"})),
        (B_OP, json!({"expression": "layer(\"a\").param(\"opacity\") * 0.5"})),
        ("/tracks/1/clips/0/transform", json!({"rotation": {"expression": "param(\"transform.position.x\") + param(\"opacity\")"}, "position": ["10", "16"]})),
    ])
    .unwrap();
    // b's local 0 = comp 1/2: a's opacity there is 1/4.
    assert_eq!(at(&t, B_OP, "0"), 0.125);
    assert_eq!(at(&t, "/tracks/1/clips/0/transform/rotation", "0"), 10.125);
    // Video effect params by id, track and timeline references.
    let t = tl(&[
        (
            "/tracks/0/clips/0/effects",
            json!([{"type": "gaussian_blur", "id": "blur", "sigma": {"expression": "1 + time"}}]),
        ),
        (
            B_OP,
            json!({"expression": "layer(\"a\").param(\"effects.blur.sigma\") / 10"}),
        ),
        (
            "/tracks/1/audio",
            json!({"gain_db": {"expression": "-comp_time"}}),
        ),
        (
            "/tracks/0/clips/0/audio",
            json!({"gain_db": {"expression": "track(\"V2\").param(\"audio.gain_db\")"}}),
        ),
    ])
    .unwrap();
    // a's local 1/2 (sigma 1.5) at b's local 0.
    assert_eq!(at(&t, B_OP, "0"), 0.15);
    assert_eq!(at(&t, "/tracks/0/clips/0/audio/gain_db", "1"), -1.0);
    // Cycles name the chain.
    let e = err(&[
        (
            A_OP,
            json!({"expression": "layer(\"b\").param(\"opacity\")"}),
        ),
        (
            B_OP,
            json!({"expression": "layer(\"a\").param(\"opacity\")"}),
        ),
    ]);
    assert!(
        e.contains(
            "expression cycle: clip \"a\": opacity -> clip \"b\": opacity -> clip \"a\": opacity"
        ),
        "{e}"
    );
    let e = err(&[(A_OP, json!({"expression": "param(\"opacity\")"}))]);
    assert!(
        e.contains("expression cycle: clip \"a\": opacity -> clip \"a\": opacity"),
        "{e}"
    );
    let e = err(&[(
        A_OP,
        json!({"expression": "layer(\"zz\").param(\"opacity\")"}),
    )]);
    assert!(
        e.contains("layer(\"zz\"): no such layer (known: a, b)"),
        "{e}"
    );
    let e = err(&[(A_OP, json!({"expression": "param(\"opacty\")"}))]);
    assert!(e.contains("no parameter \"opacty\""), "{e}");
    let e = err(&[(A_OP, json!({"expression": "param(\"generator.color\")"}))]);
    assert!(e.contains("is a vector; pick a component"), "{e}");
}

#[test]
fn errors_are_precise() {
    let e = err(&[(A_OP, json!({"expression": "1 +"}))]);
    assert!(
        e.starts_with("clip \"a\": opacity: expression syntax error at line 1, column"),
        "{e}"
    );
    let e = err(&[(A_OP, json!({"expression": "valu * 2"}))]);
    assert!(
        e.contains("clip \"a\": opacity: expression syntax error") && e.contains("valu"),
        "{e}"
    );
    let e = err(&[(A_OP, json!({"expression": "0.5 +\nwiggel(1, 2)"}))]);
    assert!(
        e.contains(
            "clip \"a\": opacity: expression error at clip time 0 s (frame 0): line 2, column 1"
        ) && e.contains("wiggel"),
        "{e}"
    );
    let e = err(&[(A_OP, json!({"expression": "\"half\""}))]);
    assert!(e.contains("must return a number, got string"), "{e}");
    let e = err(&[(A_OP, json!({"expression": "time"}))]);
    assert!(
        e.contains("clip \"a\": opacity: the expression gives 1.0416666666666667 at clip time 25/24 s (frame 25), above the maximum 1"),
        "{e}"
    );
    let e = err(&[(A_OP, json!({"expression": "1.0 / 0.0"}))]);
    assert!(e.contains("returned inf"), "{e}");
    let e = err(&[(A_OP, json!({"expression": ""}))]);
    assert!(e.contains("opacity: expression is empty"), "{e}");
    let e = err(&[(
        A_OP,
        json!({"expression": "1", "value": {"expression": "2"}}),
    )]);
    assert!(e.contains("not another expression"), "{e}");
    let e = err(&[(A_OP, json!({"expression": "1", "valeu": "1"}))]);
    assert!(
        e.contains("unknown field `valeu`") || e.contains("did not match"),
        "{e}"
    );
    // Not animatable.
    let e = err(&[("/output/gop", json!({"expression": "12"}))]);
    assert!(!e.is_empty());
}

#[test]
fn sandbox_limits_hold() {
    let e = err(&[(A_OP, json!({"expression": "let x = 0; loop { x += 1; } x"}))]);
    assert!(e.contains("Too many operations"), "{e}");
    let e = err(&[(A_OP, json!({"expression": "eval(\"1\")"}))]);
    assert!(e.contains("syntax error"), "{e}");
    let e = err(&[(A_OP, json!({"expression": "import \"fs\" as fs; 1"}))]);
    assert!(
        e.contains("syntax error") || e.contains("expression error"),
        "{e}"
    );
    let e = err(&[(A_OP, json!({"expression": "fn f(x) { f(x) } f(1)"}))]);
    assert!(e.contains("expression error"), "{e}");
    // print is silently discarded.
    let t = tl(&[(A_OP, json!({"expression": "print(\"hi\"); 1"}))]).unwrap();
    assert_eq!(at(&t, A_OP, "0"), 1.0);
}

fn keys(t: &Timeline) -> Vec<ferrocut_core::FrameKey> {
    let c = compile(t).unwrap();
    (0..t.frame_count())
        .map(|i| {
            c.graph
                .frame_key(c.output, RationalTime::from_frames(i, t.output.fps))
        })
        .collect()
}

#[test]
fn expressions_are_part_of_cache_keys() {
    let plain = keys(&tl(&[]).unwrap());
    // An expression that evaluates to the constant: same keys as the constant.
    let same = keys(&tl(&[(A_OP, json!({"expression": "1"}))]).unwrap());
    assert_eq!(plain, same);
    let e1 = tl(&[(B_OP, json!({"expression": "linear(time, 0, 1, 0, 1)"}))]).unwrap();
    let k1 = keys(&e1);
    assert_eq!(&k1[..12], &plain[..12], "before b starts");
    assert_ne!(k1[18], plain[18]);
    // Changing a referenced parameter changes the referencing clip's frames.
    let r1 = tl(&[(
        B_OP,
        json!({"expression": "layer(\"a\").param(\"generator.color.r\")"}),
    )])
    .unwrap();
    let r2 = tl(&[
        (
            B_OP,
            json!({"expression": "layer(\"a\").param(\"generator.color.r\")"}),
        ),
        (
            "/tracks/0/clips/0/generator/color",
            json!(["0.8", "0.2", "0.1"]),
        ),
    ])
    .unwrap();
    let (k1, k2) = (keys(&r1), keys(&r2));
    assert!(k1.iter().zip(&k2).all(|(a, b)| a != b));
    // Same script and value text: same keys (deterministic bake).
    assert_eq!(k1, keys(&r1.clone()));
}

#[test]
fn edits_keep_expressions_in_place() {
    let base_tl = tl(&[(A_OP, json!({"expression": "linear(time, 0, 2, 0, 1)"}))]).unwrap();
    let ops = parse_ops(r#"[{"op": "split", "clip": "a", "at": "1", "new_id": "a2"},
        {"op": "set_param", "clip": "b", "param": "opacity", "value": {"expression": "0.25 + 0.5"}}]"#)
    .unwrap();
    let (t, _) = apply(&base_tl, &ops, &mut MediaLengths::unbounded()).unwrap();
    // The second half (local 0 = comp 1) continues the ramp.
    assert_eq!(at(&t, "/tracks/0/clips/1/opacity", "0"), 0.5);
    assert_eq!(at(&t, "/tracks/0/clips/1/opacity", "1/2"), 0.75);
    assert_eq!(at(&t, B_OP, "0"), 0.75);
    // The edited timeline still stores the expressions (not baked values).
    let v = serde_json::to_value(&t).unwrap();
    assert!(v.pointer("/tracks/0/clips/1/opacity/expression").is_some());
    assert_eq!(
        v.pointer("/tracks/0/clips/1/opacity/time_offset"),
        Some(&json!("1"))
    );
    let ops = parse_ops(
        r#"[{"op": "set_param", "clip": "b", "param": "opacity", "value": {"expression": "2"}}]"#,
    )
    .unwrap();
    let e = format!(
        "{:#}",
        apply(&base_tl, &ops, &mut MediaLengths::unbounded()).unwrap_err()
    );
    assert!(e.contains("above the maximum 1"), "{e}");
}
