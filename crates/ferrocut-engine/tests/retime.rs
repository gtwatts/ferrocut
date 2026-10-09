//! Time remapping: speed / reverse / ramps / remap curves / freeze frames in
//! edit ops, frame keys and linked audio.

use std::path::Path;

use ferrocut_audio::SourceAudio;
use ferrocut_core::{Rational, RationalTime};
use ferrocut_engine::Timeline;
use ferrocut_engine::compile::compile_with;
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::nodes::{ClipNode, SourceNode};
use ferrocut_engine::retime::Sample;

fn t(s: &str) -> RationalTime {
    RationalTime(s.parse().unwrap())
}

const BASE: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [
      { "id": "a", "source": "a.mov", "start": 0, "duration": "2" },
      { "id": "b", "source": "b.mov", "start": "2", "source_in": "1", "duration": "2" },
      { "id": "c", "source": "c.mov", "start": "4", "source_in": "1/2", "duration": "1" }
    ]},
    { "name": "V2", "clips": [ { "id": "t", "source": "t.mov", "start": "6", "duration": "1" } ] }
  ]
}"#;

fn run_on(base: &Timeline, ops: &str) -> anyhow::Result<Timeline> {
    let ops = parse_ops(ops)?;
    let mut media = MediaLengths::new(".", |_| Some(RationalTime::new(10, 1)));
    apply(base, &ops, &mut media).map(|(tl, _)| tl)
}

fn run(ops: &str) -> anyhow::Result<Timeline> {
    run_on(&Timeline::from_json(BASE).unwrap(), ops)
}

fn find<'a>(tl: &'a Timeline, id: &str) -> &'a ferrocut_engine::timeline::Clip {
    tl.tracks
        .iter()
        .flat_map(|tr| &tr.clips)
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no clip {id}"))
}

fn span(tl: &Timeline, id: &str) -> (RationalTime, RationalTime, RationalTime) {
    let c = find(tl, id);
    (c.start, c.source_in, c.duration)
}

#[test]
fn set_speed_keeps_the_source_range() {
    let tl = run(r#"[{"op": "set_speed", "clip": "b", "speed": "2", "ripple": true}]"#).unwrap();
    assert_eq!(span(&tl, "b"), (t("2"), t("1"), t("1")));
    assert_eq!(span(&tl, "c").0, t("3"), "ripple pulls c in");
    assert_eq!(span(&tl, "t").0, t("6"), "other tracks untouched");
    let m = find(&tl, "b").time_map();
    assert_eq!(m.source_at(t("1/2")), t("2"));

    // Reverse: same range, played from its last frame back to its first.
    let tl = run(r#"[{"op": "set_speed", "clip": "b", "speed": "-1"}]"#).unwrap();
    let b = find(&tl, "b");
    assert_eq!((b.duration, b.source_in), (t("2"), t("71/24")));
    assert_eq!(b.time_map().source_at(t("47/24")), t("1"));
    // And back to forward speed: the original clip.
    let tl = run_on(&tl, r#"[{"op": "set_speed", "clip": "b", "speed": "1"}]"#).unwrap();
    assert_eq!(span(&tl, "b"), (t("2"), t("1"), t("2")));

    // Slow motion without ripple would overlap c.
    let e = run(r#"[{"op": "set_speed", "clip": "b", "speed": "1/2"}]"#).unwrap_err();
    assert!(format!("{e:#}").contains("overlap"), "{e:#}");
    let tl = run(r#"[{"op": "set_speed", "clip": "b", "speed": "1/2", "ripple": true, "preserve_pitch": true}]"#)
        .unwrap();
    assert_eq!(span(&tl, "b"), (t("2"), t("1"), t("4")));
    assert_eq!(span(&tl, "c").0, t("6"));
    assert!(find(&tl, "b").audio.preserve_pitch);
    // Past the media end (10 s): 4x on a 10 s range starting at 1 is fine, but
    // slipping a retimed clip past the end is reported.
    let e = run(r#"[{"op": "set_speed", "clip": "b", "speed": "4"},
                    {"op": "slip", "clip": "b", "delta": "8"}]"#)
    .unwrap_err();
    assert!(format!("{e:#}").contains("needs source up to"), "{e:#}");
}

#[test]
fn freeze_frame_hold_and_insert() {
    let tl = run(r#"[{"op": "freeze_frame", "clip": "b", "at": "3"}]"#).unwrap();
    assert_eq!(span(&tl, "b"), (t("2"), t("1"), t("1")));
    let h = find(&tl, "b.hold.2");
    assert_eq!((h.start, h.source_in, h.duration), (t("3"), t("2"), t("1")));
    assert!(h.audio.mute);
    assert_eq!(h.time_map().source_at(t("3/4")), t("2"));

    let tl = run(
        r#"[{"op": "freeze_frame", "clip": "b", "at": "3", "duration": "1", "new_id": "hold"}]"#,
    )
    .unwrap();
    assert_eq!(span(&tl, "b"), (t("2"), t("1"), t("1")));
    assert_eq!(span(&tl, "hold"), (t("3"), t("2"), t("1")));
    assert_eq!(span(&tl, "b.2"), (t("4"), t("2"), t("1")));
    assert_eq!(span(&tl, "c").0, t("5"));
    assert_eq!(span(&tl, "t").0, t("6"), "not all_tracks");
    let tl = run(
        r#"[{"op": "freeze_frame", "clip": "b", "at": "3", "duration": "1", "all_tracks": true}]"#,
    )
    .unwrap();
    assert_eq!(span(&tl, "t").0, t("7"));
}

#[test]
fn ops_on_retimed_clips_keep_content_in_place() {
    // A 1 -> 3 linear ramp over the clip, then split / trim: every part maps
    // timeline time to the same source time as before.
    let tl = run(r#"[{"op": "set_keyframes", "clip": "b", "param": "speed",
                       "keyframes": [{"t": "0", "v": "1"}, {"t": "1", "v": "3"}]},
                     {"op": "set_speed", "clip": "c", "speed": "1/2", "ripple": true}]"#);
    // The ramp needs 1 + 2 + 3 = 5 s of source from 1: inside 10 s.
    let tl = tl.unwrap();
    let before = find(&tl, "b").time_map();
    let src = |m: &ferrocut_engine::retime::TimeMap, start: &str, at: &str| {
        m.source_seconds((t(at) - t(start)).0)
    };
    let tl2 = run_on(
        &tl,
        r#"[{"op": "split", "clip": "b", "at": "5/2"},
                               {"op": "trim", "clip": "b.2", "edge": "in", "delta": "1/4"}]"#,
    )
    .unwrap();
    let (l, r) = (find(&tl2, "b"), find(&tl2, "b.2"));
    for at in ["2", "9/4", "19/8"] {
        assert!((src(&l.time_map(), "2", at) - src(&before, "2", at)).abs() < 1e-6);
    }
    for at in ["11/4", "3", "7/2", "31/8"] {
        let rs = r.start.0.to_string();
        assert!(
            (src(&r.time_map(), &rs, at) - src(&before, "2", at)).abs() < 1e-5,
            "{at}: {} vs {}",
            src(&r.time_map(), &rs, at),
            src(&before, "2", at)
        );
    }
    // Remap curve + slip shifts the curve's values.
    let tl3 = run(
        r#"[{"op": "set_keyframes", "clip": "b", "param": "time_remap",
                        "keyframes": [{"t": "0", "v": "4"}, {"t": "2", "v": "2"}]},
                      {"op": "slip", "clip": "b", "delta": "1"}]"#,
    )
    .unwrap();
    assert_eq!(find(&tl3, "b").time_map().source_at(t("1")), t("4"));
    let e = run(
        r#"[{"op": "set_keyframes", "clip": "b", "param": "time_remap",
                     "keyframes": [{"t": "0", "v": "4"}]},
                    {"op": "set_param", "clip": "b", "param": "speed", "value": "2"}]"#,
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("exclusive"), "{e:#}");
    let e =
        run(r#"[{"op": "set_param", "clip": "b", "param": "sampling", "value": "optical_flow"}]"#)
            .unwrap_err();
    assert!(format!("{e:#}").contains("reserved hook"), "{e:#}");
    run(
        r#"[{"op": "set_param", "clip": "b", "param": "sampling", "value": "frame_blend"},
            {"op": "set_param", "clip": "b", "param": "time_remap", "value": null}]"#,
    )
    .unwrap();
}

fn stub_keys(tl: &Timeline) -> Vec<ferrocut_core::FrameKey> {
    let c = compile_with(tl, |p| {
        let (w, h) = (tl.output.width, tl.output.height);
        Ok(SourceNode {
            path: p.clone(),
            file_hash: *blake3::hash(p.to_string_lossy().as_bytes()).as_bytes(),
            width: w,
            height: h,
            fps: Some(Rational::from_int(24)),
        })
    })
    .unwrap();
    (0..tl.frame_count())
        .map(|i| {
            c.graph
                .frame_key(c.output, RationalTime::from_frames(i, tl.output.fps))
        })
        .collect()
}

#[test]
fn frame_keys_follow_the_time_map() {
    let base = Timeline::from_json(BASE).unwrap();
    let k0 = stub_keys(&base);
    let f = |i: i64| RationalTime::from_frames(i, Rational::from_int(24));
    // A freeze: every held frame pulls the same source frame (decoded once
    // per chunk); frames outside the edited clip keep their keys.
    let tl = run(r#"[{"op": "freeze_frame", "clip": "b", "at": "3"}]"#).unwrap();
    let k = stub_keys(&tl);
    let node = clip_node(&tl, "b.hold.2");
    assert!((72..96).all(|i| node.sample_at(f(i)) == Sample::One(t("2"))));
    assert_eq!(k[..72], k0[..72]);
    assert_eq!(k[96..], k0[96..]);
    // Frame 72 showed source 2 before too: same content, same key.
    assert_eq!(k[72], k0[72]);
    assert!((73..96).all(|i| k[i] != k0[i]));
    // Half speed: nearest repeats each source frame; frame blend mixes the
    // two neighbours on the in-between output frames.
    let tl = run(r#"[{"op": "set_speed", "clip": "c", "speed": "1/2"},
                     {"op": "set_param", "clip": "c", "param": "sampling", "value": "frame_blend"}]"#)
    .unwrap();
    let blend = clip_node(&tl, "c");
    let tl_n = run(r#"[{"op": "set_speed", "clip": "c", "speed": "1/2"}]"#).unwrap();
    let near = clip_node(&tl_n, "c");
    // c: start 4, source_in 1/2 (frame 12). Output frame 97 is source 12.5.
    assert_eq!(near.sample_at(f(96)), Sample::One(f(12)));
    assert_eq!(near.sample_at(f(97)), Sample::One(f(13)));
    assert_eq!(blend.sample_at(f(96)), Sample::One(f(12)));
    assert_eq!(
        blend.sample_at(f(97)),
        Sample::Blend(f(12), f(13), Rational::new(1, 2))
    );
    let (kb, kn) = (stub_keys(&tl), stub_keys(&tl_n));
    assert_eq!(kb[96], kn[96], "whole source frames: same key either way");
    assert_ne!(kb[97], kn[97], "the blended frame is new");
    // Reverse: b's first frame shows its last source frame and vice versa.
    let tl = run(r#"[{"op": "set_speed", "clip": "b", "speed": "-1"}]"#).unwrap();
    let rev = clip_node(&tl, "b");
    assert_eq!(rev.sample_at(f(48)), Sample::One(t("71/24")));
    assert_eq!(rev.sample_at(f(95)), Sample::One(t("1")));
}

fn clip_node(tl: &Timeline, id: &str) -> ClipNode {
    let c = find(tl, id);
    ClipNode {
        start: c.start,
        source_in: c.source_in,
        duration: c.duration,
        opacity: c.opacity.clone(),
        map: c.time_map(),
        sampling: c.sampling,
        source_fps: Some(Rational::from_int(24)),
    }
}

#[test]
fn linked_audio_follows_speed() {
    let json = r#"{
      "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
      "tracks": [ { "name": "V1", "clips": [
        { "id": "a", "source": "a.mov", "start": 0, "source_in": "1", "duration": "1", "speed": "2" },
        { "id": "r", "source": "a.mov", "start": 1, "source_in": "3", "duration": "1", "speed": "-1" }
      ] } ],
      "audio": { "sample_rate": 8000 }
    }"#;
    let tl = Timeline::from_json(json).unwrap();
    // Source sample i has value i / 1e5 (a ramp), 10 s at 8 kHz.
    let ramp = SourceAudio {
        planes: vec![(0..80_000).map(|i| i as f32 / 1e5).collect()],
    };
    let mut load = |_: &Path| Ok(Some(ramp.clone()));
    let (p, sources, _) = ferrocut_engine::audio::resolve(&tl, &mut load)
        .unwrap()
        .unwrap();
    let bus = ferrocut_audio::render_track(&p, &sources, 0, 0, 16_000);
    // Mono source, centre pan: x · cos(π/4).
    let g = std::f64::consts::FRAC_1_SQRT_2;
    let at = |n: usize| bus.l[n] as f64 / g * 1e5;
    // Speed 2 from source 1 s: program sample n plays source 8000 + 2n.
    for n in [10usize, 1000, 7990] {
        assert!(
            (at(n) - (8000.0 + 2.0 * n as f64)).abs() < 0.05,
            "{n}: {}",
            at(n)
        );
    }
    // Reverse from 3 s: program sample 8000 + k plays source 24000 - k.
    for k in [10usize, 4000, 7990] {
        assert!(
            (at(8000 + k) - (24000.0 - k as f64)).abs() < 0.05,
            "{k}: {}",
            at(8000 + k)
        );
    }
}
