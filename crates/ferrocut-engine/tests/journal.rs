//! Project journal (edit/undo/log/branches) and structured diffs.

use std::path::{Path, PathBuf};

use ferrocut_core::Rational;
use ferrocut_engine::Timeline;
use ferrocut_engine::diff::{diff, diff_files};
use ferrocut_engine::edit::parse_ops;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::project::{
    self, EditOptions, Entry, Journal, read_timeline, timeline_hash, timeline_text,
};

const BASE: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [
      { "id": "a", "source": "a.mov", "start": 0, "duration": "2" },
      { "id": "b", "source": "b.mov", "start": "2", "source_in": "1", "duration": "2" },
      { "id": "c", "source": "c.mov", "start": "4", "source_in": "1/2", "duration": "1",
        "opacity": { "keyframes": [ { "t": "0", "v": "0" }, { "t": "1/2", "v": "1" } ] } }
    ]},
    { "name": "V2", "clips": [ { "id": "t", "source": "t.mov", "start": "6", "duration": "1" } ] }
  ],
  "audio_tracks": [ { "name": "music", "clips": [ { "id": "m", "source": "m.flac", "start": 0, "duration": "7" } ] } ]
}"#;

const SCRIPTS: [&str; 3] = [
    r#"[{ "op": "slip", "clip": "b", "delta": "1/4" }]"#,
    r#"{ "ops": [ { "op": "split", "clip": "a", "at": "1", "new_id": "a2" },
                  { "op": "trim", "clip": "c", "edge": "out", "delta": "-1/4" } ] }"#,
    r#"[{ "op": "move", "clip": "t", "to": "13/2" }]"#,
];

fn no_probe() -> EditOptions {
    EditOptions {
        probe: false,
        ..EditOptions::default()
    }
}

/// A project dir with `tl.json` in ferrocut's on-disk form.
fn setup() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("tl.json");
    std::fs::write(
        &p,
        timeline_text(&Timeline::from_json(BASE).unwrap()).unwrap(),
    )
    .unwrap();
    (dir, p)
}

fn edit(p: &Path, script: &str) -> project::EditOutcome {
    project::edit_file(p, &parse_ops(script).unwrap(), &no_probe()).unwrap()
}

fn file_hash(p: &Path) -> String {
    timeline_hash(&read_timeline(p).unwrap())
}

#[test]
fn journal_is_append_only_chained_and_deterministic() {
    let run = || {
        let (dir, p) = setup();
        let h0 = file_hash(&p);
        let mut prev = h0.clone();
        let mut lens = Vec::new();
        for (i, s) in SCRIPTS.iter().enumerate() {
            let r = edit(&p, s);
            assert!(r.written);
            assert_eq!(r.journal_seq, Some(i as u64 + 1));
            assert_eq!(r.before, prev, "entries chain");
            prev = r.after.clone();
            assert_eq!(file_hash(&p), r.after);
            lens.push(std::fs::read(p.with_extension("journal.jsonl")).unwrap());
        }
        // Append-only: each journal is a prefix of the next.
        for w in lens.windows(2) {
            assert!(w[1].starts_with(&w[0]));
        }
        let entries = Journal::for_timeline(&p).entries().unwrap();
        assert_eq!(entries.len(), 3);
        match &entries[1] {
            Entry::Edit { ops, changes, .. } => {
                assert_eq!(ops.len(), 2);
                assert_eq!(changes.len(), 2);
                assert_eq!(ops[0].kind(), "split");
            }
            e => panic!("{e:?}"),
        }
        let journal = std::fs::read_to_string(p.with_extension("journal.jsonl")).unwrap();
        assert!(!journal.contains("time"), "no timestamps");
        (journal, std::fs::read(&p).unwrap(), dir)
    };
    let (j1, f1, _d1) = run();
    let (j2, f2, _d2) = run();
    assert_eq!(j1, j2, "same edits -> byte-identical journal");
    assert_eq!(f1, f2);
}

#[test]
fn canonical_hash_ignores_formatting_and_key_order() {
    let (_d, p) = setup();
    let h = file_hash(&p);
    let v: serde_json::Value = serde_json::from_str(BASE).unwrap();
    std::fs::write(&p, serde_json::to_string(&v).unwrap()).unwrap();
    assert_eq!(file_hash(&p), h);
    let shuffled = BASE.replacen(
        r#""width": 64, "height": 32"#,
        r#""height": 32, "width": 64"#,
        1,
    );
    std::fs::write(&p, shuffled).unwrap();
    assert_eq!(file_hash(&p), h);
}

#[test]
fn dry_run_writes_nothing() {
    let (dir, p) = setup();
    let before = std::fs::read(&p).unwrap();
    let r = project::edit_file(
        &p,
        &parse_ops(SCRIPTS[0]).unwrap(),
        &EditOptions {
            dry_run: true,
            ..no_probe()
        },
    )
    .unwrap();
    assert!(!r.written && r.dry_run);
    assert_ne!(r.before, r.after);
    assert_eq!(r.changes.len(), 1);
    assert_eq!(std::fs::read(&p).unwrap(), before);
    assert!(!p.with_extension("journal.jsonl").exists());
    assert!(!dir.path().join(".ferrocut").exists());
}

#[test]
fn undo_restores_each_state_exactly() {
    let (_d, p) = setup();
    let mut states = vec![std::fs::read(&p).unwrap()];
    for s in SCRIPTS {
        edit(&p, s);
        states.push(std::fs::read(&p).unwrap());
    }
    for k in (0..3).rev() {
        let u = project::undo(&p, false).unwrap();
        assert_eq!(u.undid, k as u64 + 1);
        assert_eq!(
            std::fs::read(&p).unwrap(),
            states[k],
            "byte-exact after undo"
        );
    }
    assert!(
        project::undo(&p, false)
            .unwrap_err()
            .to_string()
            .contains("nothing to undo")
    );
    let log = project::log(&p).unwrap();
    assert_eq!(log.entries.len(), 6);
    assert!(log.clean);
    assert_eq!(log.entries.iter().filter(|e| e.undone).count(), 3);

    // A new edit after undos, then an outside change: undo refuses unless forced.
    edit(&p, SCRIPTS[2]);
    let mut tl = read_timeline(&p).unwrap();
    tl.name = "hand-edited".into();
    std::fs::write(&p, timeline_text(&tl).unwrap()).unwrap();
    assert!(!project::log(&p).unwrap().clean);
    let e = project::undo(&p, false).unwrap_err().to_string();
    assert!(e.contains("changed since journal entry"), "{e}");
    let u = project::undo(&p, true).unwrap();
    assert!(u.forced);
    assert_eq!(std::fs::read(&p).unwrap(), states[0]);
    // The discarded hand edit is still recoverable as a snapshot.
    assert!(Journal::for_timeline(&p).load_snapshot(&u.before).is_ok());
}

#[test]
fn branches_checkout_and_merge() {
    let (_d, p) = setup();
    let h0 = file_hash(&p);
    project::branch(&p, "alt").unwrap();
    assert!(project::branch(&p, "alt").is_err());
    assert!(project::branch(&p, "bad name").is_err());
    project::checkout(&p, "alt", false).unwrap();
    let alt1 = edit(&p, SCRIPTS[0]).after;
    let alt2 = edit(&p, SCRIPTS[2]).after;
    assert_eq!(project::log(&p).unwrap().branch, "alt");

    project::checkout(&p, "main", false).unwrap();
    assert_eq!(file_hash(&p), h0, "checkout restores main's tip");
    // Undo on main has nothing (alt's edits aren't main's).
    assert!(project::undo(&p, false).is_err());

    // Main diverges with a commuting edit, then merges alt (replays both ops).
    edit(
        &p,
        r#"[{ "op": "trim", "clip": "a", "edge": "out", "delta": "-1/2" }]"#,
    );
    let m = project::merge(&p, "alt", &no_probe()).unwrap();
    assert_eq!(m.changes.len(), 2);
    let tl = read_timeline(&p).unwrap();
    let b = tl.tracks[0].clips.iter().find(|c| c.id == "b").unwrap();
    assert_eq!(b.source_in.to_string(), "5/4s");
    assert_eq!(tl.tracks[1].clips[0].start.to_string(), "13/2s");
    let log = project::log(&p).unwrap();
    assert!(matches!(
        log.entries.last().unwrap().entry,
        Entry::Merge { .. }
    ));
    assert_eq!(log.tips["alt"], alt2);
    assert_ne!(alt1, alt2);
    // The merge undoes as one step.
    project::undo(&p, false).unwrap();
    assert_eq!(
        read_timeline(&p).unwrap().tracks[1].clips[0]
            .start
            .to_string(),
        "6s"
    );

    // Conflict: alt edits a clip main deleted -> error, nothing written.
    project::checkout(&p, "alt", false).unwrap();
    edit(&p, r#"[{ "op": "slip", "clip": "c", "delta": "1/8" }]"#);
    project::checkout(&p, "main", false).unwrap();
    edit(&p, r#"[{ "op": "ripple_delete", "clip": "c" }]"#);
    let (bytes, jlen) = (
        std::fs::read(&p).unwrap(),
        std::fs::read(p.with_extension("journal.jsonl"))
            .unwrap()
            .len(),
    );
    assert!(project::merge(&p, "alt", &no_probe()).is_err());
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
    assert_eq!(
        std::fs::read(p.with_extension("journal.jsonl"))
            .unwrap()
            .len(),
        jlen
    );
}

#[test]
fn output_elsewhere_journals_beside_output() {
    let (dir, p) = setup();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let out = out_dir.join("cut.json");
    let r = project::edit_file(
        &p,
        &parse_ops(SCRIPTS[0]).unwrap(),
        &EditOptions {
            output: Some(out.clone()),
            ..no_probe()
        },
    )
    .unwrap();
    assert!(r.sources_absolutized);
    assert!(out.with_extension("journal.jsonl").exists());
    assert!(!p.with_extension("journal.jsonl").exists());
    let tl = read_timeline(&out).unwrap();
    assert!(tl.tracks[0].clips[0].source.is_absolute());
    // Undo on the output restores the input's state (with absolute sources).
    project::undo(&out, false).unwrap();
    let d = diff_files(&p, &out, false).unwrap();
    assert!(d.identical, "{d:?}");
}

fn tl(ops: &str) -> Timeline {
    let base = Timeline::from_json(BASE).unwrap();
    let ops = parse_ops(ops).unwrap();
    ferrocut_engine::edit::apply(
        &base,
        &ops,
        &mut ferrocut_engine::edit::MediaLengths::unbounded(),
    )
    .unwrap()
    .0
}

fn tags(d: &ferrocut_engine::diff::TimelineDiff, id: &str) -> Vec<&'static str> {
    d.clips
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no change for {id}: {d:#?}"))
        .tags
        .clone()
}

#[test]
fn diff_classifies_changes() {
    let base = Timeline::from_json(BASE).unwrap();
    assert!(diff(&base, &base).identical);

    let d = diff(
        &base,
        &tl(r#"[{ "op": "slip", "clip": "b", "delta": "1/4" }]"#),
    );
    assert_eq!(tags(&d, "b"), ["slipped"]);
    assert_eq!(d.summary.slipped, 1);

    let d = diff(&base, &tl(r#"[{ "op": "move", "clip": "t", "to": "8" }]"#));
    assert_eq!(tags(&d, "t"), ["moved"]);

    let d = diff(
        &base,
        &tl(r#"[{ "op": "roll", "clip": "a", "delta": "1/2" }]"#),
    );
    assert_eq!(tags(&d, "a"), ["trimmed_out"]);
    assert_eq!(tags(&d, "b"), ["trimmed_in"]);
    assert_eq!(d.summary.trimmed, 2);

    let d = diff(
        &base,
        &tl(r#"[{ "op": "move", "clip": "t", "to": "8", "track": "V1" }]"#),
    );
    assert!(tags(&d, "t").contains(&"track_changed"));
    assert_eq!(d.clips[0].from_track.as_deref(), Some("V2"));

    let d = diff(
        &base,
        &tl(r#"[{ "op": "split", "clip": "a", "at": "1", "new_id": "a2" }]"#),
    );
    let a2 = d.clips.iter().find(|c| c.id == "a2").unwrap();
    assert_eq!(a2.change, "added");
    assert_eq!(tags(&d, "a"), ["trimmed_out"]);

    let d = diff(&base, &tl(r#"[{ "op": "ripple_delete", "clip": "b" }]"#));
    assert_eq!(d.summary.clips_removed, 1);
    assert_eq!(tags(&d, "c"), ["moved"]);
    assert_eq!(d.affected.len(), 1);

    // Keyframes: change one value, add one key.
    let mut k = base.clone();
    let c = k.tracks[0].clips.iter_mut().find(|c| c.id == "c").unwrap();
    c.opacity = serde_json::from_str(
        r#"{ "keyframes": [ { "t": "0", "v": "1/4" }, { "t": "1/2", "v": "1" }, { "t": "1", "v": "0" } ] }"#,
    )
    .unwrap();
    let d = diff(&base, &k);
    assert_eq!(tags(&d, "c"), ["opacity_changed", "keyframes_changed"]);
    let kf = d.clips[0].fields[0].keyframes.as_ref().unwrap();
    assert_eq!(
        (kf.added.len(), kf.removed.len(), kf.changed.len()),
        (1, 0, 1)
    );
    assert_eq!(kf.changed[0].t, serde_json::json!("0"));
    assert_eq!(d.affected.len(), 1);
    assert_eq!(d.affected[0].0.to_string(), "4s");

    // Settings change affects everything.
    let mut s = base.clone();
    s.output.width = 128;
    let d = diff(&base, &s);
    assert_eq!(d.settings[0].path, "output.width");
    assert_eq!(d.affected[0].1, base.duration());
}

fn synth(path: &Path, frames: i64, seed: u8) {
    let s = EncodeSettings {
        width: 64,
        height: 32,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..frames {
        let px: Vec<u8> = (0..64 * 32 * 4)
            .map(|i| {
                if i % 4 == 3 {
                    255
                } else {
                    (i as i64 + f * 7) as u8 ^ seed
                }
            })
            .collect();
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

#[test]
fn diff_reports_chunks_to_rerender() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for (n, seed) in [("a.mkv", 1u8), ("b.mkv", 2), ("c.mkv", 3)] {
        synth(&d.join(n), 5 * 24, seed);
    }
    let base = r#"{
      "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
      "tracks": [ { "name": "V1", "clips": [
        { "id": "a", "source": "a.mkv", "start": 0, "source_in": "1", "duration": "2" },
        { "id": "b", "source": "b.mkv", "start": "2", "source_in": "1", "duration": "2" },
        { "id": "c", "source": "c.mkv", "start": "4", "source_in": "1", "duration": "2" } ]}]
    }"#;
    let p = d.join("tl.json");
    std::fs::write(&p, base).unwrap();
    let orig = d.join("orig.json");
    std::fs::write(&orig, base).unwrap();
    let r = project::edit_file(
        &p,
        &parse_ops(r#"[{ "op": "slip", "clip": "b", "delta": "1/2" }]"#).unwrap(),
        &EditOptions {
            plan: true,
            ..EditOptions::default()
        },
    )
    .unwrap();
    let imp = r.render.expect("render impact");
    assert_eq!(imp.dirty_chunks, [4, 5, 6, 7]);
    let df = diff_files(&orig, &p, true).unwrap();
    let ri = df.render.unwrap();
    assert_eq!(ri.dirty_chunks, [4, 5, 6, 7]);
    assert_eq!(
        (ri.total_chunks, ri.reused_chunks, ri.dirty_frames),
        (12, 8, 48)
    );
    assert_eq!(ri.ranges.len(), 1);
    assert_eq!((ri.ranges[0].start_frame, ri.ranges[0].end_frame), (48, 96));
    assert_eq!(ri.ranges[0].start.to_string(), "2s");
    // Missing media: structural diff still works, render impact explains.
    std::fs::remove_file(d.join("c.mkv")).unwrap();
    let df = diff_files(&orig, &p, true).unwrap();
    assert!(df.render.is_none() && df.render_error.is_some());
    assert_eq!(df.summary.slipped, 1);
}
