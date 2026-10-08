//! Edit operations: behavior, clear errors, and proptest invariants.

use ferrocut_core::{Rational, RationalTime};
use ferrocut_engine::Timeline;
use ferrocut_engine::edit::{Edge, EditOp, MediaLengths, apply, parse_ops};
use proptest::prelude::*;

fn t(s: &str) -> RationalTime {
    RationalTime(s.parse().unwrap())
}

const BASE: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [
      { "id": "a", "source": "a.mov", "start": 0, "duration": "2" },
      { "id": "b", "source": "b.mov", "start": "2", "source_in": "1", "duration": "2" },
      { "id": "c", "source": "c.mov", "start": "4", "source_in": "1/2", "duration": "1",
        "audio": { "gain_db": { "keyframes": [ { "t": "0", "v": "-6" }, { "t": "1", "v": "0" } ] } } }
    ]},
    { "name": "V2", "clips": [ { "id": "t", "source": "t.mov", "start": "6", "duration": "1" } ] }
  ],
  "audio_tracks": [ { "name": "music", "clips": [ { "id": "m", "source": "m.flac", "start": 0, "duration": "7" } ] } ]
}"#;

fn base() -> Timeline {
    Timeline::from_json(BASE).unwrap()
}

/// Every source is 10 s long.
fn media() -> MediaLengths<'static> {
    MediaLengths::new(".", |_| {
        Some(RationalTime::from_frames(240, Rational::from_int(24)))
    })
}

fn run(ops: &str) -> anyhow::Result<Timeline> {
    let ops = parse_ops(ops)?;
    apply(&base(), &ops, &mut media()).map(|(tl, _)| tl)
}

fn clip(tl: &Timeline, id: &str) -> (RationalTime, RationalTime, RationalTime) {
    for tr in &tl.tracks {
        if let Some(c) = tr.clips.iter().find(|c| c.id == id) {
            return (c.start, c.source_in, c.duration);
        }
    }
    for tr in &tl.audio_tracks {
        if let Some(c) = tr.clips.iter().find(|c| c.id == id) {
            return (c.start, c.source_in, c.duration);
        }
    }
    panic!("no clip {id}")
}

#[test]
fn each_op_does_what_it_says() {
    let tl = run(r#"[{"op": "split", "clip": "b", "at": "3"}]"#).unwrap();
    assert_eq!(clip(&tl, "b"), (t("2"), t("1"), t("1")));
    assert_eq!(clip(&tl, "b.2"), (t("3"), t("2"), t("1")));
    let tl = run(r#"[{"op": "trim", "clip": "b", "edge": "in", "delta": "1/2"}]"#).unwrap();
    assert_eq!(clip(&tl, "b"), (t("5/2"), t("3/2"), t("3/2")));
    let tl = run(r#"[{"op": "trim", "clip": "b", "edge": "out", "delta": "-1/2"}]"#).unwrap();
    assert_eq!(clip(&tl, "b"), (t("2"), t("1"), t("3/2")));
    let tl = run(r#"[{"op": "ripple_delete", "clip": "b"}]"#).unwrap();
    assert_eq!(clip(&tl, "c"), (t("2"), t("1/2"), t("1")));
    assert_eq!(
        clip(&tl, "t"),
        (t("6"), t("0"), t("1")),
        "other tracks untouched"
    );
    let tl = run(r#"[{"op": "ripple_delete", "clip": "b", "all_tracks": true}]"#);
    assert!(
        format!("{:#}", tl.unwrap_err()).contains("spanning"),
        "music spans the removed range"
    );
    let tl = run(r#"[{"op": "ripple_insert", "track": "V1", "at": "2",
                     "clip": {"id": "n", "source": "n.mov", "duration": "3/2"}}]"#)
    .unwrap();
    assert_eq!(clip(&tl, "n"), (t("2"), t("0"), t("3/2")));
    assert_eq!(clip(&tl, "b").0, t("7/2"));
    assert_eq!(clip(&tl, "c").0, t("11/2"));
    let tl = run(r#"[{"op": "roll", "clip": "a", "delta": "1/4"}]"#).unwrap();
    assert_eq!(clip(&tl, "a"), (t("0"), t("0"), t("9/4")));
    assert_eq!(clip(&tl, "b"), (t("9/4"), t("5/4"), t("7/4")));
    let tl = run(r#"[{"op": "slip", "clip": "b", "delta": "3/2"}]"#).unwrap();
    assert_eq!(clip(&tl, "b"), (t("2"), t("5/2"), t("2")));
    let tl = run(r#"[{"op": "slide", "clip": "b", "delta": "1/2"}]"#).unwrap();
    assert_eq!(clip(&tl, "a").2, t("5/2"));
    assert_eq!(clip(&tl, "b"), (t("5/2"), t("1"), t("2")));
    assert_eq!(clip(&tl, "c"), (t("9/2"), t("1"), t("1/2")));
    let tl = run(r#"[{"op": "move", "clip": "t", "to": "5", "track": "V2"}]"#).unwrap();
    assert_eq!(clip(&tl, "t").0, t("5"));
    let tl = run(r#"[{"op": "jl_cut", "clip": "b", "in_offset": "-1/2"},
                     {"op": "jl_cut", "clip": "a", "out_offset": "-1/2"}]"#)
    .unwrap();
    assert_eq!(tl.tracks[0].clips[1].audio.in_offset, t("-1/2"));
    assert_eq!(tl.tracks[0].clips[0].audio.out_offset, t("-1/2"));
}

#[test]
fn keyframes_stay_put_on_the_timeline() {
    // Trim c's in by 1/2: its gain keys (clip-local 0 -> -6, 1 -> 0) shift by
    // -1/2, so the gain at timeline 4.5 is unchanged (-3 dB).
    let before = base();
    let after = run(r#"[{"op": "trim", "clip": "c", "edge": "in", "delta": "1/2"}]"#).unwrap();
    let gain = |tl: &Timeline, at: &str| {
        let c = tl.tracks[0].clips.iter().find(|c| c.id == "c").unwrap();
        c.audio.gain_db.eval(RationalTime(t(at).0 - c.start.0))
    };
    assert_eq!(gain(&before, "9/2"), -3.0);
    assert_eq!(gain(&after, "9/2"), -3.0);
}

#[test]
fn errors_are_clear() {
    let e = |ops: &str| format!("{:#}", run(ops).unwrap_err());
    let s = e(r#"[{"op": "slip", "clip": "b", "delta": "8"}]"#);
    assert!(
        s.contains("op 0 (slip b)") && s.contains("is 10s long") && s.contains("max source_in"),
        "{s}"
    );
    let s = e(r#"[{"op": "slip", "clip": "b", "delta": "-2"}]"#);
    assert!(s.contains("before the start"), "{s}");
    let s = e(r#"[{"op": "split", "clip": "b", "at": "5"}]"#);
    assert!(s.contains("not strictly inside"), "{s}");
    let s = e(r#"[{"op": "roll", "clip": "c", "delta": "1/4"}]"#);
    assert!(s.contains("roll needs a clip after c"), "{s}");
    let s = e(r#"[{"op": "trim", "clip": "a", "edge": "out", "delta": "1"}]"#);
    assert!(s.contains("overlap"), "{s}");
    let s = e(r#"[{"op": "jl_cut", "clip": "a", "in_offset": "-1"}]"#);
    assert!(
        s.contains("J-cuts need source handles") || s.contains("before the start"),
        "{s}"
    );
    let s = e(r#"[{"op": "move", "clip": "m", "to": "1", "track": "V1"}]"#);
    assert!(s.contains("same kind"), "{s}");
    let s = e(
        r#"[{"op": "ripple_insert", "track": "V1", "at": "1", "clip": {"id": "n", "source": "n.mov", "duration": "1"}}]"#,
    );
    assert!(s.contains("split it first"), "{s}");
    let s = e(r#"[{"op": "slip", "clip": "zz", "delta": "1"}]"#);
    assert!(s.contains("no clip with id \"zz\""), "{s}");
    let s = format!(
        "{:#}",
        parse_ops(r#"[{"op": "slip", "clip": "a", "delta": "1", "bogus": 1}]"#).unwrap_err()
    );
    assert!(s.contains("op 0: invalid op"), "{s}");
    // A failing op later in the script: earlier ops don't leak out.
    assert!(run(r#"[{"op": "slip", "clip": "b", "delta": "1"}, {"op": "slip", "clip": "b", "delta": "100"}]"#).is_err());
}

#[test]
fn ops_round_trip_as_json() {
    let ops = vec![
        EditOp::Trim {
            clip: "a".into(),
            edge: Edge::In,
            delta: t("1/2"),
        },
        EditOp::JlCut {
            clip: "b".into(),
            in_offset: Some(t("-1/2")),
            out_offset: None,
        },
    ];
    let s = serde_json::to_string(&ops).unwrap();
    assert_eq!(parse_ops(&s).unwrap(), ops);
}

// ---- property tests -------------------------------------------------------

/// A single-track timeline of `n` clips (cuts or gaps between them, frame
/// aligned at 24 fps), each with 2 s of source handle on both sides.
fn track_strategy() -> impl Strategy<Value = Vec<(i64, i64)>> {
    // (gap before, duration) in frames.
    prop::collection::vec((0i64..12, 6i64..60), 2..7)
}

fn build(spec: &[(i64, i64)]) -> Timeline {
    let mut clips = Vec::new();
    let mut at = 0i64;
    for (i, (gap, dur)) in spec.iter().enumerate() {
        at += gap;
        clips.push(format!(
            r#"{{ "id": "c{i}", "source": "s{i}.mov", "start": "{at}/24", "source_in": "2", "duration": "{dur}/24" }}"#
        ));
        at += dur;
    }
    let json = format!(
        r#"{{ "output": {{ "width": 16, "height": 16, "fps": "24" }},
             "tracks": [ {{ "name": "V1", "clips": [ {} ] }} ] }}"#,
        clips.join(",")
    );
    Timeline::from_json(&json).unwrap()
}

fn spans(tl: &Timeline) -> Vec<(String, RationalTime, RationalTime, RationalTime)> {
    let mut v: Vec<_> = tl.tracks[0]
        .clips
        .iter()
        .map(|c| (c.id.clone(), c.start, c.end(), c.source_in))
        .collect();
    v.sort_by_key(|x| x.1);
    v
}

fn no_overlaps(tl: &Timeline) -> bool {
    spans(tl).windows(2).all(|w| w[0].2 <= w[1].1)
}

fn track_end(tl: &Timeline) -> RationalTime {
    spans(tl).iter().map(|s| s.2).max().unwrap()
}

fn frames(n: i64) -> RationalTime {
    RationalTime::from_frames(n, Rational::from_int(24))
}

fn unbounded() -> MediaLengths<'static> {
    MediaLengths::unbounded()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn ripple_delete_keeps_order_and_no_overlaps(spec in track_strategy(), pick in any::<prop::sample::Index>()) {
        let tl = build(&spec);
        let k = pick.index(spec.len());
        let id = format!("c{k}");
        let (out, _) = apply(&tl, &[EditOp::RippleDelete { clip: id.clone(), all_tracks: false }], &mut unbounded()).unwrap();
        let before = spans(&tl);
        let after = spans(&out);
        prop_assert!(no_overlaps(&out));
        let ids_before: Vec<_> = before.iter().filter(|s| s.0 != id).map(|s| s.0.clone()).collect();
        let ids_after: Vec<_> = after.iter().map(|s| s.0.clone()).collect();
        prop_assert_eq!(ids_before, ids_after, "relative order kept");
        // Upstream untouched; downstream all moved by the same amount (relative spacing kept).
        let shift = frames(spec[k].1);
        for (b, a) in before.iter().filter(|s| s.0 != id).zip(&after) {
            if b.1 < before[k].1 { prop_assert_eq!(b, a); }
            else {
                prop_assert_eq!(b.1 - shift, a.1);
                prop_assert_eq!(b.2 - b.1, a.2 - a.1);
                prop_assert_eq!(b.3, a.3);
            }
        }
    }

    #[test]
    fn ripple_insert_keeps_order_and_no_overlaps(spec in track_strategy(), pick in any::<prop::sample::Index>(), dur in 1i64..48) {
        let tl = build(&spec);
        // Insert at the start of an existing clip (always a legal ripple point).
        let k = pick.index(spec.len());
        let at = spans(&tl)[k].1;
        let op = EditOp::RippleInsert {
            track: "V1".into(),
            at,
            clip: serde_json::json!({"id": "new", "source": "n.mov", "duration": format!("{dur}/24")}),
            all_tracks: false,
        };
        let (out, _) = apply(&tl, &[op], &mut unbounded()).unwrap();
        prop_assert!(no_overlaps(&out));
        let after = spans(&out);
        let before = spans(&tl);
        prop_assert_eq!(&after[k].0, "new");
        for (i, b) in before.iter().enumerate() {
            let a = after.iter().find(|s| s.0 == b.0).unwrap();
            let expect = if i >= k { b.1 + frames(dur) } else { b.1 };
            prop_assert_eq!(a.1, expect);
        }
        prop_assert_eq!(track_end(&out), track_end(&tl) + frames(dur));
    }

    #[test]
    fn roll_preserves_total_duration(spec in track_strategy(), pick in any::<prop::sample::Index>(), d in -5i64..=5) {
        // Make all edits cuts (no gaps) so every clip but the last has a roll partner.
        let spec: Vec<_> = spec.iter().map(|&(_, dur)| (0, dur)).collect();
        let tl = build(&spec);
        let k = pick.index(spec.len() - 1);
        let (out, _) = apply(&tl, &[EditOp::Roll { clip: format!("c{k}"), delta: frames(d) }], &mut unbounded()).unwrap();
        prop_assert_eq!(track_end(&out), track_end(&tl));
        prop_assert!(no_overlaps(&out));
        let (b, a) = (spans(&tl), spans(&out));
        prop_assert_eq!((b[k].2 - b[k].1) + (b[k+1].2 - b[k+1].1), (a[k].2 - a[k].1) + (a[k+1].2 - a[k+1].1));
        prop_assert_eq!(a[k].2, a[k+1].1, "still a cut");
        // The right clip's content stays in place: its source_in moved with its start.
        prop_assert_eq!(a[k+1].3 - b[k+1].3, a[k+1].1 - b[k+1].1);
        for i in (0..spec.len()).filter(|&i| i != k && i != k + 1) { prop_assert_eq!(&b[i], &a[i]); }
    }

    #[test]
    fn slip_preserves_position_and_duration(spec in track_strategy(), pick in any::<prop::sample::Index>(), d in -48i64..=48) {
        let tl = build(&spec);
        let k = pick.index(spec.len());
        let (out, _) = apply(&tl, &[EditOp::Slip { clip: format!("c{k}"), delta: frames(d) }], &mut unbounded()).unwrap();
        let (b, a) = (spans(&tl), spans(&out));
        for i in 0..spec.len() {
            prop_assert_eq!((b[i].1, b[i].2), (a[i].1, a[i].2));
            let ds = if i == k { frames(d) } else { RationalTime::ZERO };
            prop_assert_eq!(a[i].3, b[i].3 + ds);
        }
    }

    #[test]
    fn slide_preserves_track_duration(spec in track_strategy(), pick in any::<prop::sample::Index>(), d in -5i64..=5) {
        let spec: Vec<_> = spec.iter().map(|&(_, dur)| (0, dur)).collect();
        let tl = build(&spec);
        // An interior clip (has both neighbors).
        prop_assume!(spec.len() >= 3);
        let k = 1 + pick.index(spec.len() - 2);
        let (out, _) = apply(&tl, &[EditOp::Slide { clip: format!("c{k}"), delta: frames(d) }], &mut unbounded()).unwrap();
        prop_assert_eq!(track_end(&out), track_end(&tl));
        prop_assert!(no_overlaps(&out));
        let (b, a) = (spans(&tl), spans(&out));
        prop_assert_eq!(b[k].2 - b[k].1, a[k].2 - a[k].1, "slid clip keeps its duration");
        prop_assert_eq!(b[k].3, a[k].3, "and its source window");
        prop_assert_eq!(a[k].1 - b[k].1, frames(d));
    }

    #[test]
    fn invalid_scripts_never_half_apply(spec in track_strategy(), d in 240i64..400) {
        let tl = build(&spec);
        let bounded = &mut MediaLengths::new(".", |_| Some(RationalTime::new(10, 1)));
        let r = apply(&tl, &[EditOp::Slip { clip: "c0".into(), delta: frames(d) }], bounded);
        prop_assert!(r.is_err());
        let msg = format!("{:#}", r.unwrap_err());
        prop_assert!(msg.contains("op 0 (slip c0)"));
    }
}
