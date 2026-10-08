//! Markers (timeline and clip, names/colors, source-time anchoring) and the
//! relink op. Markers never touch frame keys.

use ferrocut_core::RationalTime;
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::markers::{MarkerColor, list};
use ferrocut_engine::{Timeline, compile};

fn t(s: &str) -> RationalTime {
    serde_json::from_value(serde_json::Value::from(s)).unwrap()
}

const BASE: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24" },
  "tracks": [ { "name": "V1", "clips": [
    { "id": "a", "source": "v.mkv", "start": "0", "source_in": "1", "duration": "4" },
    { "id": "b", "source": "w.mkv", "start": "4", "source_in": "0", "duration": "2" } ] } ],
  "audio_tracks": [ { "name": "A1", "clips": [
    { "id": "m", "source": "music.wav", "start": "0", "source_in": "10", "duration": "6" } ] } ]
}"#;

fn run(tl: &Timeline, ops: &str) -> anyhow::Result<Timeline> {
    Ok(apply(tl, &parse_ops(ops)?, &mut MediaLengths::unbounded())?.0)
}

fn err(tl: &Timeline, ops: &str) -> String {
    format!("{:#}", run(tl, ops).unwrap_err())
}

#[test]
fn marker_ops_store_timeline_and_source_times() {
    let base = Timeline::from_json(BASE).unwrap();
    let tl = run(
        &base,
        r#"[
        {"op": "add_marker", "time": "2", "name": "beat", "color": "red"},
        {"op": "add_marker", "time": "5", "duration": "1/2", "name": "outro"},
        {"op": "add_marker", "clip": "a", "time": "3", "timeline_time": true, "name": "smile", "color": "yellow"},
        {"op": "add_marker", "clip": "a", "time": "2", "id": "x", "comment": "soft focus"},
        {"op": "add_marker", "clip": "m", "time": "12", "name": "drop", "color": "purple"}
    ]"#,
    )
    .unwrap();
    assert_eq!(tl.markers.len(), 2);
    assert_eq!(tl.markers[0].id, "m1");
    assert_eq!(tl.markers[1].id, "m2");
    assert_eq!(tl.markers[0].color, MarkerColor::Red);
    assert_eq!(tl.markers[1].duration, t("1/2"));
    let a = &tl.tracks[0].clips[0];
    // Kept sorted by time. Timeline 3 on a clip starting at 0 with
    // source_in 1 -> source 4.
    assert_eq!(a.markers[0].id, "x");
    assert_eq!(a.markers[0].time, t("2"));
    assert_eq!(a.markers[1].id, "m1");
    assert_eq!(a.markers[1].time, t("4"));
    assert_eq!(tl.audio_tracks[0].clips[0].markers[0].time, t("12"));

    // Round trip through JSON (and validation on load).
    let back = Timeline::from_json(&serde_json::to_string(&tl).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&back).unwrap(),
        serde_json::to_value(&tl).unwrap()
    );

    // Listing: timeline time for everything.
    let l = list(&tl);
    let got: Vec<(&str, Option<&str>, &str, Option<RationalTime>)> = l
        .iter()
        .map(|m| (m.scope, m.clip.as_deref(), m.id.as_str(), m.time))
        .collect();
    assert_eq!(
        got,
        [
            ("timeline", None, "m1", Some(t("2"))),
            ("timeline", None, "m2", Some(t("5"))),
            ("clip", Some("a"), "x", Some(t("1"))),
            ("clip", Some("a"), "m1", Some(t("3"))),
            ("clip", Some("m"), "m1", Some(t("2"))),
        ]
    );
    assert_eq!(l[3].source_time, Some(t("4")));
    assert_eq!(l[3].track.as_deref(), Some("V1"));

    // Update and remove.
    let tl2 = run(
        &tl,
        r#"[
        {"op": "update_marker", "id": "m1", "color": "green", "comment": "ok"},
        {"op": "update_marker", "clip": "a", "id": "m1", "time": "1", "timeline_time": true},
        {"op": "remove_marker", "clip": "a", "id": "x"}
    ]"#,
    )
    .unwrap();
    assert_eq!(tl2.markers[0].color, MarkerColor::Green);
    assert_eq!(tl2.markers[0].comment, "ok");
    assert_eq!(tl2.markers[0].name, "beat");
    let a2 = &tl2.tracks[0].clips[0];
    assert_eq!(a2.markers.len(), 1);
    assert_eq!(a2.markers[0].time, t("2"));

    // Errors name the problem and change nothing.
    let e = err(
        &tl,
        r#"[{"op": "add_marker", "clip": "a", "time": "1", "id": "x"}]"#,
    );
    assert!(e.contains("\"x\" is already used"), "{e}");
    let e = err(
        &tl,
        r#"[{"op": "add_marker", "clip": "a", "time": "9/2", "timeline_time": true}]"#,
    );
    assert!(e.contains("outside the clip"), "{e}");
    let e = err(&tl, r#"[{"op": "remove_marker", "id": "nope"}]"#);
    assert!(e.contains("nope"), "{e}");
    let e = err(&tl, r#"[{"op": "add_marker", "time": "-1"}]"#);
    assert!(e.contains(">= 0"), "{e}");
    assert!(parse_ops(r#"[{"op": "add_marker", "time": "1", "color": "pink"}]"#).is_err());
}

#[test]
fn clip_markers_follow_their_frame_through_edits() {
    let base = Timeline::from_json(BASE).unwrap();
    let tl = run(
        &base,
        r#"[
        {"op": "add_marker", "clip": "a", "time": "4", "name": "late"},
        {"op": "add_marker", "clip": "a", "time": "3/2", "name": "early"},
        {"op": "add_marker", "time": "4", "name": "stays"}
    ]"#,
    )
    .unwrap();
    // Split a at 2: both halves keep the list; each lists the markers inside it.
    let s = run(&tl, r#"[{"op": "split", "clip": "a", "at": "2"}]"#).unwrap();
    let l = list(&s);
    let shown: Vec<(&str, &str, Option<RationalTime>)> = l
        .iter()
        .filter(|m| m.scope == "clip")
        .map(|m| (m.clip.as_deref().unwrap(), m.name.as_str(), m.time))
        .collect();
    assert_eq!(
        shown,
        [
            ("a", "early", Some(t("1/2"))),
            ("a", "late", None),
            ("a.2", "late", Some(t("3"))),
            ("a.2", "early", None),
        ]
    );
    // A slip moves the marked frame on the timeline
    // with the content; the timeline marker stays put.
    let sl = run(&tl, r#"[{"op": "slip", "clip": "a", "delta": "1/2"}]"#).unwrap();
    let l = list(&sl);
    assert_eq!(l[0].time, Some(t("4")));
    let late = l.iter().find(|m| m.name == "late").unwrap();
    assert_eq!(late.time, Some(t("5/2")));
    // A move carries clip markers along.
    let mv = run(
        &tl,
        r#"[{"op": "ripple_delete", "clip": "b"}, {"op": "move", "clip": "a", "to": "2"}]"#,
    )
    .unwrap();
    let late = list(&mv).into_iter().find(|m| m.name == "late").unwrap();
    assert_eq!(late.time, Some(t("5")));
    // Twice as fast: source 4 is (4 - 1) / 2 = 3/2 after the start.
    let sp = run(&tl, r#"[{"op": "set_speed", "clip": "a", "speed": "2"}]"#).unwrap();
    let late = list(&sp).into_iter().find(|m| m.name == "late").unwrap();
    assert_eq!(late.time, Some(t("3/2")));
}

const GEN: &str = r#"{
  "output": { "width": 32, "height": 16, "fps": "24", "gop": 12 },
  "tracks": [ { "name": "V1", "clips": [
    { "id": "g", "generator": { "type": "solid", "color": ["1/2", "1/4", "1"] }, "start": "0", "duration": "2" } ] } ]
}"#;

#[test]
fn markers_never_change_frame_keys_or_rerender() {
    let base = Timeline::from_json(GEN).unwrap();
    let marked = run(
        &base,
        r#"[{"op": "add_marker", "time": "1", "name": "n"},
            {"op": "add_marker", "clip": "g", "time": "1/2", "color": "blue"}]"#,
    )
    .unwrap();
    let keys = |tl: &Timeline| {
        let c = compile(tl).unwrap();
        (0..tl.frame_count())
            .map(|i| {
                c.graph
                    .frame_key(c.output, RationalTime::from_frames(i, tl.output.fps))
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&base), keys(&marked));
    let d = ferrocut_engine::diff::diff(&base, &marked);
    assert!(!d.identical);
    assert!(d.affected.is_empty(), "{:?}", d.affected);
    let c = d.clips.iter().find(|c| c.id == "g").unwrap();
    assert!(c.tags.contains(&"markers_changed"), "{:?}", c.tags);
    // The op itself reports no span either.
    let (_, ch) = apply(
        &base,
        &parse_ops(r#"[{"op": "add_marker", "time": "1"}]"#).unwrap(),
        &mut MediaLengths::unbounded(),
    )
    .unwrap();
    assert_eq!(ch[0].span.0, ch[0].span.1);
}

/// Real files: relink checks that new paths exist.
fn files(dir: &std::path::Path, names: &[&str]) {
    for n in names {
        let p = dir.join(n);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }
}

fn media(dir: &std::path::Path) -> MediaLengths<'static> {
    MediaLengths::new(dir.to_path_buf(), |_| Some(t("20")))
}

#[test]
fn relink_by_clip_by_prefix_and_by_search() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    files(
        d,
        &[
            "new/v.mkv",
            "moved/deep/w.mkv",
            "moved/music.wav",
            "dup1/v.mkv",
            "dup2/v.mkv",
        ],
    );
    let base = Timeline::from_json(BASE).unwrap();
    let go = |tl: &Timeline, ops: &str| apply(tl, &parse_ops(ops).unwrap(), &mut media(d));
    // clip + to.
    let (tl, ch) = go(
        &base,
        r#"[{"op": "relink", "clip": "a", "to": "new/v.mkv"}]"#,
    )
    .unwrap();
    assert_eq!(
        tl.tracks[0].clips[0].source,
        std::path::Path::new("new/v.mkv")
    );
    assert_eq!(tl.tracks[0].clips[1].source, std::path::Path::new("w.mkv"));
    assert!(
        ch[0].summary.contains("v.mkv -> new/v.mkv"),
        "{}",
        ch[0].summary
    );
    // from + to: a whole directory.
    let (tl2, _) = go(&tl, r#"[{"op": "relink", "from": "new", "to": "dup1"}]"#).unwrap();
    assert_eq!(
        tl2.tracks[0].clips[0].source,
        std::path::Path::new("dup1/v.mkv")
    );
    // from + to: one file.
    let (tl3, _) = go(
        &base,
        r#"[{"op": "relink", "from": "w.mkv", "to": "moved/deep/w.mkv"}]"#,
    )
    .unwrap();
    assert_eq!(
        tl3.tracks[0].clips[1].source,
        std::path::Path::new("moved/deep/w.mkv")
    );
    assert_eq!(tl3.tracks[0].clips[0].source, std::path::Path::new("v.mkv"));
    // search: every offline file found by name (w.mkv, music.wav); v.mkv is ambiguous
    // under the whole dir, so search only where it is unique.
    let (s, ch) = go(&tl, r#"[{"op": "relink", "search": "moved"}]"#).unwrap();
    assert_eq!(
        s.tracks[0].clips[1].source,
        std::path::Path::new("moved/deep/w.mkv")
    );
    assert_eq!(
        s.audio_tracks[0].clips[0].source,
        std::path::Path::new("moved/music.wav")
    );
    assert_eq!(
        s.tracks[0].clips[0].source,
        std::path::Path::new("new/v.mkv")
    );
    assert!(
        ch[0].summary.contains("relink 2 clip(s)"),
        "{}",
        ch[0].summary
    );
    // search with clip: only that clip's source.
    let (s, _) = go(
        &base,
        r#"[{"op": "relink", "search": "moved", "clip": "b"}]"#,
    )
    .unwrap();
    assert_eq!(
        s.tracks[0].clips[1].source,
        std::path::Path::new("moved/deep/w.mkv")
    );
    assert_eq!(
        s.audio_tracks[0].clips[0].source,
        std::path::Path::new("music.wav")
    );
    // Ambiguous names are an error listing the candidates.
    let e = format!(
        "{:#}",
        go(&base, r#"[{"op": "relink", "search": ".", "clip": "a"}]"#).unwrap_err()
    );
    assert!(e.contains("matches several files"), "{e}");
    // Targets must exist; bad combinations are rejected.
    let e = format!(
        "{:#}",
        go(
            &base,
            r#"[{"op": "relink", "clip": "a", "to": "nope.mkv"}]"#
        )
        .unwrap_err()
    );
    assert!(e.contains("does not exist"), "{e}");
    let e = format!(
        "{:#}",
        go(&base, r#"[{"op": "relink", "to": "new/v.mkv"}]"#).unwrap_err()
    );
    assert!(e.contains("give `clip`"), "{e}");
    let e = format!(
        "{:#}",
        go(&base, r#"[{"op": "relink", "from": "zzz", "to": "new"}]"#).unwrap_err()
    );
    assert!(e.contains("no clip source"), "{e}");
}
