//! Timeline-building ops: add_track, add_clip (probed), add_transition,
//! set_param, set_keyframes, so agents never hand-edit the JSON.

use ferrocut_core::{Rational, RationalTime};
use ferrocut_engine::Timeline;
use ferrocut_engine::edit::{MediaFacts, MediaLengths, apply, parse_ops};

fn t(s: &str) -> RationalTime {
    serde_json::from_value(serde_json::Value::from(s)).unwrap()
}

/// Fake media: `v*.mkv` 6 s video+audio, `pic*.mkv` 6 s picture only,
/// `music*.wav` 30 s audio only; anything else doesn't exist.
fn media() -> MediaLengths<'static> {
    let facts = |p: &std::path::Path| -> Option<MediaFacts> {
        let n = p.file_name()?.to_str()?;
        if n.starts_with('v') {
            Some(MediaFacts {
                duration: Some(t("6")),
                has_video: true,
                has_audio: true,
            })
        } else if n.starts_with("pic") {
            Some(MediaFacts {
                duration: Some(t("6")),
                has_video: true,
                has_audio: false,
            })
        } else if n.starts_with("music") {
            Some(MediaFacts {
                duration: Some(t("30")),
                has_video: false,
                has_audio: true,
            })
        } else {
            None
        }
    };
    MediaLengths::new(".", move |p| facts(p).and_then(|f| f.duration)).with_info(move |p| {
        facts(p).ok_or_else(|| anyhow::anyhow!("opening {}: no such file", p.display()))
    })
}

fn empty() -> Timeline {
    Timeline::from_json(
        r#"{"output":{"width":64,"height":32,"fps":"24"},"tracks":[{"name":"V1","clips":[]}]}"#,
    )
    .unwrap()
}

fn run(tl: &Timeline, ops: &str) -> anyhow::Result<Timeline> {
    Ok(apply(tl, &parse_ops(ops)?, &mut media())?.0)
}

fn err(tl: &Timeline, ops: &str) -> String {
    format!("{:#}", run(tl, ops).unwrap_err())
}

#[test]
fn builds_a_sequence_from_nothing() {
    let tl = run(
        &empty(),
        r#"[
          {"op":"add_clip","track":"V1","source":"media/v1.mkv","source_in":"1","duration":"4"},
          {"op":"add_clip","track":"V1","source":"media/v2.mkv","source_in":"1/2"},
          {"op":"add_clip","track":"V1","source":"media/v1.mkv","id":"again","duration":"2"},
          {"op":"add_track","kind":"audio","name":"Music"},
          {"op":"add_clip","track":"Music","source":"media/music.wav","start":"0","duration":"10"},
          {"op":"add_track","kind":"video","name":"Titles"},
          {"op":"add_track","kind":"video","name":"Under","index":0}
        ]"#,
    )
    .unwrap();
    let v1 = &tl.tracks[1].clips;
    assert_eq!(
        tl.tracks
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["Under", "V1", "Titles"]
    );
    // Defaults: id from the file stem (unique), start = end of track, duration = rest of the media.
    assert_eq!(
        v1.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["v1", "v2", "again"]
    );
    assert_eq!((v1[1].start, v1[1].duration), (t("4"), t("11/2")));
    assert_eq!(v1[2].start, t("19/2"));
    assert_eq!(tl.audio_tracks[0].clips[0].id, "music");
    assert_eq!(tl.duration(), t("23/2"));
}

#[test]
fn add_clip_checks_streams_files_overlap_and_names() {
    let tl = empty();
    assert!(
        err(
            &tl,
            r#"[{"op":"add_clip","track":"V1","source":"music.wav"}]"#
        )
        .contains("no video stream")
    );
    assert!(
        err(
            &tl,
            r#"[{"op":"add_clip","track":"V1","source":"nope.mkv"}]"#
        )
        .contains("no such file")
    );
    assert!(err(&tl, r#"[{"op":"add_clip","track":"V2","source":"v.mkv"}]"#).contains("add_track"));
    assert!(
        err(&tl, r#"[{"op":"add_track","kind":"audio","name":"A"},{"op":"add_clip","track":"A","source":"pic.mkv"}]"#)
            .contains("no audio stream")
    );
    let e = err(
        &tl,
        r#"[{"op":"add_clip","track":"V1","source":"v.mkv"},{"op":"add_clip","track":"V1","source":"v.mkv","start":"5"}]"#,
    );
    assert!(
        e.contains("overlaps clip v") && e.contains("ripple_insert"),
        "{e}"
    );
    // Media bounds still apply.
    assert!(
        err(
            &tl,
            r#"[{"op":"add_clip","track":"V1","source":"v.mkv","source_in":"2","duration":"5"}]"#
        )
        .contains("6s long")
    );
    assert!(
        err(&tl, r#"[{"op":"add_track","kind":"video","name":"V1"}]"#).contains("already exists")
    );
}

fn two_shots() -> Timeline {
    run(
        &empty(),
        r#"[{"op":"add_clip","track":"V1","source":"pic5.mkv","id":"a","source_in":"1/2","duration":"5"},
            {"op":"add_clip","track":"V1","source":"pic6.mkv","id":"b","source_in":"1/2","duration":"5"}]"#,
    )
    .unwrap()
}

#[test]
fn add_transition_uses_handles_and_moves_nothing_else() {
    // The eval's tos-dissolve-fade edit: 1 s dissolve centred on the cut at 5 s.
    let tl = run(
        &two_shots(),
        r#"[{"op":"add_transition","clip":"b","duration":"1"}]"#,
    )
    .unwrap();
    let c = &tl.tracks[0].clips;
    assert_eq!((c[0].start, c[0].end()), (t("0"), t("11/2")));
    assert_eq!(
        (c[1].start, c[1].source_in, c[1].end()),
        (t("9/2"), t("0"), t("10"))
    );
    assert_eq!(c[1].dissolve(), Some(t("1")));
    assert_eq!(
        c[1].audio.crossfade_in.as_ref().map(|f| f.duration),
        Some(t("1"))
    );
    assert_eq!(tl.duration(), t("10"));
    // start / end alignment
    let s = run(
        &two_shots(),
        r#"[{"op":"add_transition","clip":"b","duration":"1/2","align":"start"}]"#,
    )
    .unwrap();
    assert_eq!(
        (s.tracks[0].clips[0].end(), s.tracks[0].clips[1].start),
        (t("11/2"), t("5"))
    );
    let e = run(
        &two_shots(),
        r#"[{"op":"add_transition","clip":"b","duration":"1/2","align":"end"}]"#,
    )
    .unwrap();
    assert_eq!(
        (e.tracks[0].clips[0].end(), e.tracks[0].clips[1].start),
        (t("5"), t("9/2"))
    );
    // No handle: b starts at source 1/2, so a 2 s end-aligned dissolve can't fit.
    let x = err(
        &two_shots(),
        r#"[{"op":"add_transition","clip":"b","duration":"2","align":"end"}]"#,
    );
    assert!(x.contains("source_in would be"), "{x}");
    assert!(
        err(
            &two_shots(),
            r#"[{"op":"add_transition","clip":"a","duration":"1"}]"#
        )
        .contains("first clip")
    );
}

#[test]
fn set_param_and_keyframes_reach_every_parameter() {
    let tl = run(
        &two_shots(),
        r#"[
          {"op":"add_transition","clip":"b","duration":"1"},
          {"op":"set_keyframes","clip":"a","param":"opacity","keyframes":[{"t":"0","v":"0"},{"t":"1","v":"1"}]},
          {"op":"set_keyframes","clip":"b","param":"opacity","timeline_time":true,
           "keyframes":[{"t":"9","v":"1"},{"t":"10","v":"0","interp":"ease_in"}]},
          {"op":"set_param","clip":"a","param":"transform.scale","value":"1/2"},
          {"op":"set_param","clip":"a","param":"transform.position.x","value":"10"},
          {"op":"set_keyframes","clip":"a","param":"transform.rotation","keyframes":[{"t":"0","v":"0"},{"t":"5","v":"90"}]},
          {"op":"set_param","clip":"b","param":"audio.gain_db","value":"-6"},
          {"op":"set_param","clip":"b","param":"audio.fade_out","value":{"duration":"1/2"}},
          {"op":"add_track","kind":"audio","name":"Music"},
          {"op":"add_clip","track":"Music","source":"music.wav","start":"0","duration":"10"},
          {"op":"set_param","track":"Music","param":"bus.duck","value":{"key":["V1"]}},
          {"op":"set_keyframes","track":"Music","param":"bus.duck.range_db","keyframes":[{"t":"0","v":"6"},{"t":"10","v":"18"}]},
          {"op":"set_param","param":"audio.loudness","value":{"target_lufs":"-23","true_peak_dbtp":"-2"}},
          {"op":"set_keyframes","param":"audio.master_gain_db","keyframes":[{"t":"0","v":"0"},{"t":"10","v":"-3"}]}
        ]"#,
    )
    .unwrap();
    let v = serde_json::to_value(&tl).unwrap();
    let a = &v["tracks"][0]["clips"][0];
    let b = &v["tracks"][0]["clips"][1];
    assert_eq!(a["opacity"]["keyframes"][1]["v"], "1");
    // Timeline 9..10 -> b's local time (b starts at 9/2).
    assert_eq!(b["opacity"]["keyframes"][0]["t"], "9/2");
    assert_eq!(b["opacity"]["keyframes"][1]["t"], "11/2");
    assert_eq!(a["transform"]["scale"], "1/2");
    assert_eq!(a["transform"]["position"], serde_json::json!(["10", "16"]));
    assert_eq!(b["audio"]["gain_db"], "-6");
    assert_eq!(
        v["audio_tracks"][0]["bus"]["duck"]["range_db"]["keyframes"][1]["v"],
        "18"
    );
    assert_eq!(v["audio"]["loudness"]["target_lufs"], "-23");
    assert_eq!(
        tl.audio.master_gain_db.key_range().0,
        Rational::from_int(-3)
    );
}

#[test]
fn set_param_errors_name_the_problem() {
    let tl = two_shots();
    let e = err(
        &tl,
        r#"[{"op":"set_param","clip":"a","param":"opacty","value":"1"}]"#,
    );
    assert!(
        e.contains("unknown parameter") && e.contains("opacity"),
        "{e}"
    );
    assert!(
        err(
            &tl,
            r#"[{"op":"set_param","clip":"a","param":"opacity","value":"2"}]"#
        )
        .contains("maximum")
    );
    assert!(
        err(
            &tl,
            r#"[{"op":"set_param","clip":"a","param":"opacity","value":0.5}]"#
        )
        .contains("expected a rational")
    );
    assert!(err(&tl, r#"[{"op":"set_keyframes","clip":"a","param":"audio.mute","keyframes":[{"t":"0","v":"1"}]}]"#)
        .contains("not animatable"));
    assert!(
        err(
            &tl,
            r#"[{"op":"set_param","track":"V1","param":"bus.duck","value":{"key":["nope"]}}]"#
        )
        .contains("not a track name")
    );
    assert!(
        err(
            &tl,
            r#"[{"op":"set_param","param":"audio.loudness","value":{"target_lufs":"5"}}]"#
        )
        .contains("target_lufs")
    );
    // Nothing written on error: the input is untouched.
    assert!(tl.tracks[0].clips[0].transform.is_none());
}
