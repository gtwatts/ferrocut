//! Nested compositions: composed frame keys (an inner edit changes only the
//! outer frames that show it), cycle and size checks, nest / unnest ops
//! (files, validation, round trip), and nested audio equal to the flat mix.

use std::path::{Path, PathBuf};

use ferrocut_audio::SourceAudio;
use ferrocut_core::{FrameKey, Rational, RationalTime};
use ferrocut_engine::Timeline;
use ferrocut_engine::compile::compile_with;
use ferrocut_engine::nodes::SourceNode;
use ferrocut_engine::project::{EditOptions, edit_file};

fn stub(p: &Path, w: u32, h: u32) -> anyhow::Result<SourceNode> {
    let name = p.file_name().unwrap().to_string_lossy().into_owned();
    Ok(SourceNode {
        path: p.to_path_buf(),
        file_hash: *blake3::hash(name.as_bytes()).as_bytes(),
        width: w,
        height: h,
        fps: Some(Rational::from_int(24)),
    })
}

fn keys(path: &Path) -> anyhow::Result<Vec<FrameKey>> {
    let tl = Timeline::load(path)?;
    let c = compile_with(&tl, |p| stub(p, 64, 32))?;
    Ok((0..tl.frame_count())
        .map(|i| {
            c.graph
                .frame_key(c.output, RationalTime::from_frames(i, tl.output.fps))
        })
        .collect())
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let p = dir.join(name);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(&p, text).unwrap();
    p
}

fn opts() -> EditOptions {
    EditOptions {
        probe: false,
        journal: false,
        ..Default::default()
    }
}

fn edit(tl: &Path, ops: &str) -> anyhow::Result<ferrocut_engine::project::EditOutcome> {
    edit_file(tl, &ferrocut_engine::edit::parse_ops(ops)?, &opts())
}

const INNER: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [ { "name": "V1", "clips": [ { "id": "x", "source": "../x.mov", "start": 0, "duration": "3" } ] } ]
}"#;

const OUTER: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [ { "id": "bg", "source": "bg.mov", "start": 0, "duration": "5" } ] },
    { "name": "V2", "clips": [ { "id": "c", "source": "comps/inner.json", "start": "2", "source_in": "1/2", "duration": "2" } ] }
  ]
}"#;

#[test]
fn inner_edits_change_only_the_outer_frames_that_show_them() {
    let d = tempfile::tempdir().unwrap();
    let inner = write(d.path(), "comps/inner.json", INNER);
    let outer = write(d.path(), "outer.json", OUTER);
    let before = keys(&outer).unwrap();
    // Inner time 2 is outer 2 + (2 - 1/2) = 7/2 s = frame 84; the comp clip ends at frame 96.
    edit(
        &inner,
        r#"[{"op": "split", "clip": "x", "at": "2", "new_id": "y"},
            {"op": "set_param", "clip": "y", "param": "opacity", "value": "1/2"}]"#,
    )
    .unwrap();
    let after = keys(&outer).unwrap();
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        assert_eq!(a != b, (84..96).contains(&i), "frame {i}");
    }
    // Inner changes past the part the clip shows change nothing.
    edit(
        &inner,
        r#"[{"op": "set_param", "clip": "x", "param": "opacity", "value": "1/4"}]"#,
    )
    .unwrap();
    let k2 = keys(&outer).unwrap();
    for i in 0..before.len() {
        let inner_t = Rational::new(i as i64, 24) - Rational::from_int(2) + Rational::new(1, 2);
        let shows_x = (48..96).contains(&i) && inner_t < Rational::from_int(2);
        assert_eq!(k2[i] != after[i], shows_x, "frame {i}");
    }
}

#[test]
fn cycles_fail_and_other_canvas_sizes_are_fitted() {
    let d = tempfile::tempdir().unwrap();
    let a = write(
        d.path(),
        "a.json",
        r#"{ "output": { "width": 64, "height": 32, "fps": "24" },
             "tracks": [ { "name": "V1", "clips": [ { "id": "b", "source": "b.json", "start": 0, "duration": "1" } ] } ] }"#,
    );
    write(
        d.path(),
        "b.json",
        r#"{ "output": { "width": 64, "height": 32, "fps": "24" },
             "tracks": [ { "name": "V1", "clips": [ { "id": "a", "source": "a.json", "start": 0, "duration": "1" } ] } ] }"#,
    );
    let e = format!("{:#}", keys(&a).unwrap_err());
    assert!(e.contains("nested composition cycle"), "{e}");
    let me = write(
        d.path(),
        "self.json",
        r#"{ "output": { "width": 64, "height": 32, "fps": "24" },
             "tracks": [ { "name": "V1", "clips": [ { "id": "s", "source": "self.json", "start": 0, "duration": "1" } ] } ] }"#,
    );
    // The outer file isn't on the stack (compile only sees the timeline), so
    // the cycle is caught one level in.
    assert!(format!("{:#}", keys(&me).unwrap_err()).contains("cycle"));
    write(
        d.path(),
        "big.json",
        r#"{ "output": { "width": 128, "height": 32, "fps": "24" },
             "tracks": [ { "name": "V1", "clips": [ { "id": "x", "source": "x.mov", "start": 0, "duration": "1" } ] } ] }"#,
    );
    let o = write(
        d.path(),
        "o.json",
        r#"{ "output": { "width": 64, "height": 32, "fps": "24" },
             "tracks": [ { "name": "V1", "clips": [ { "id": "c", "source": "big.json", "start": 0, "duration": "1" } ] } ] }"#,
    );
    assert_eq!(keys(&o).unwrap().len(), 24);
    let tl = Timeline::load(&o).unwrap();
    let compiled = compile_with(&tl, |p| stub(p, 64, 32)).unwrap();
    let placed = compiled.placements.iter().find(|p| p.clip == "c").unwrap();
    assert_eq!(placed.native, (128, 32));
    assert_eq!(placed.output, (64, 32));
    assert_eq!(placed.fit_scale, [Rational::new(1, 2); 2]);
    assert!(
        format!(
            "{:#}",
            edit(&o, r#"[{"op":"unnest","clip":"c"}]"#).unwrap_err()
        )
        .contains("unnest would change the picture")
    );
}

const FLAT: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [
    { "name": "V1", "clips": [
      { "id": "a", "source": "a.mov", "start": 0, "duration": "2", "opacity": "3/4" },
      { "id": "b", "source": "b.mov", "start": "3", "duration": "2" } ] },
    { "name": "V2", "clips": [
      { "id": "t", "source": "t.mov", "start": "1", "source_in": "1", "duration": "3/2",
        "transform": { "rotation": "10" } } ] }
  ]
}"#;

#[test]
fn nest_writes_a_comp_and_unnest_restores_the_clips() {
    let d = tempfile::tempdir().unwrap();
    let tl = write(d.path(), "edit.json", FLAT);
    // Dry run: reported, nothing written.
    let r = edit_file(
        &tl,
        &ferrocut_engine::edit::parse_ops(
            r#"[{"op": "nest", "clips": ["a", "t"], "path": "comps/at.json"}]"#,
        )
        .unwrap(),
        &EditOptions {
            dry_run: true,
            ..opts()
        },
    )
    .unwrap();
    assert_eq!(r.new_comps.len(), 1);
    assert!(!d.path().join("comps").exists());
    // comps/ does not exist yet: nest creates it.
    let r = edit(
        &tl,
        r#"[{"op": "nest", "clips": ["a", "t"], "path": "comps/at.json", "id": "at"}]"#,
    )
    .unwrap();
    assert!(r.changes[0].summary.contains("2 clip(s) from 2 track(s)"));
    let comp = Timeline::load(&d.path().join("comps/at.json")).unwrap();
    assert_eq!(comp.tracks.len(), 2);
    assert_eq!(comp.duration(), RationalTime::new(5, 2));
    let t = &comp.tracks[1].clips[0];
    assert_eq!((t.id.as_str(), t.start), ("t", RationalTime::new(1, 1)));
    // Sources were relative to the timeline's directory; the comp lives in
    // comps/, so they are made absolute.
    assert!(t.source.is_absolute() && t.source.ends_with("t.mov"));
    let outer = Timeline::load(&tl).unwrap();
    let ids: Vec<&str> = outer.clip_ids().collect();
    assert_eq!(ids, ["at", "b"]);
    assert_eq!(outer.tracks[0].clips[0].duration, RationalTime::new(5, 2));
    keys(&tl).unwrap();
    // Existing file: refused.
    let e = format!(
        "{:#}",
        edit(
            &tl,
            r#"[{"op": "nest", "clips": ["b"], "path": "comps/at.json"}]"#
        )
        .unwrap_err()
    );
    assert!(e.contains("already exists"), "{e}");
    // Unnest: back where they were, t on a new track right above V1.
    let r = edit(&tl, r#"[{"op": "unnest", "clip": "at"}]"#).unwrap();
    assert!(r.changes[0].summary.contains("2 clip(s)"));
    let back = Timeline::load(&tl).unwrap();
    let flat = Timeline::load(&write(d.path(), "flat.json", FLAT)).unwrap();
    let names: Vec<&str> = back.tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["V1", "V2.2", "V2"]);
    let strip = |c: &ferrocut_engine::timeline::Clip| {
        let mut c = c.clone();
        c.source = PathBuf::from(c.source.file_name().unwrap());
        serde_json::to_value(c).unwrap()
    };
    assert_eq!(
        back.tracks[0].clips.iter().map(strip).collect::<Vec<_>>(),
        flat.tracks[0].clips.iter().map(strip).collect::<Vec<_>>()
    );
    assert_eq!(
        strip(&back.tracks[1].clips[0]),
        strip(&flat.tracks[1].clips[0])
    );
    assert!(back.tracks[2].clips.is_empty());
}

#[test]
fn unnest_trims_to_the_shown_part() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "comps/inner.json", INNER);
    let outer = write(d.path(), "outer.json", OUTER);
    edit(&outer, r#"[{"op": "unnest", "clip": "c"}]"#).unwrap();
    let tl = Timeline::load(&outer).unwrap();
    let x = &tl.tracks[1].clips[0];
    // Shown: inner [1/2, 5/2) at outer [2, 4).
    assert_eq!(x.start, RationalTime::new(2, 1));
    assert_eq!(x.source_in, RationalTime::new(1, 2));
    assert_eq!(x.duration, RationalTime::new(2, 1));
    // A comp clip with its own settings is refused.
    write(d.path(), "outer2.json", OUTER);
    let o2 = d.path().join("outer2.json");
    let e = format!(
        "{:#}",
        edit(
            &o2,
            r#"[{"op": "set_param", "clip": "c", "param": "opacity", "value": "1/2"},
                {"op": "unnest", "clip": "c"}]"#
        )
        .unwrap_err()
    );
    assert!(e.contains("reset them first"), "{e}");
}

#[test]
fn nest_validation() {
    let d = tempfile::tempdir().unwrap();
    let tl = write(
        d.path(),
        "v.json",
        r#"{
  "output": { "width": 64, "height": 32, "fps": "24" },
  "tracks": [ { "name": "V1", "clips": [
      { "id": "a", "source": "a.mov", "start": 0, "source_in": "1", "duration": "2" },
      { "id": "b", "source": "b.mov", "start": "3/2", "source_in": "1", "duration": "2",
        "transition_in": { "kind": "dissolve", "duration": "1/2" } },
      { "id": "c", "source": "c.mov", "start": "4", "source_in": "1", "duration": "1",
        "audio": { "in_offset": "-1/2" } } ] } ],
  "audio_tracks": [ { "name": "A1", "clips": [ { "id": "m", "source": "m.wav", "start": 0, "duration": "1" } ] } ]
}"#,
    );
    let err = |ops: &str| format!("{:#}", edit(&tl, ops).unwrap_err());
    assert!(
        err(r#"[{"op": "nest", "clips": ["b"], "path": "n.json"}]"#).contains("dissolves from a")
    );
    assert!(err(r#"[{"op": "nest", "clips": ["c"], "path": "n.json"}]"#).contains("linked audio"));
    assert!(err(r#"[{"op": "nest", "clips": ["m"], "path": "n.json"}]"#).contains("audio clip"));
    assert!(err(r#"[{"op": "nest", "clips": ["a"], "path": "n.mov"}]"#).contains(".json"));
    assert!(err(r#"[{"op": "nest", "clips": ["a", "a"], "path": "n.json"}]"#).contains("twice"));
    assert!(err(r#"[{"op": "unnest", "clip": "a"}]"#).contains("not a nested composition"));
    // Both sides of the dissolve: fine; the comp lands in the timeline's directory.
    edit(
        &tl,
        r#"[{"op": "nest", "clips": ["a", "b"], "path": "n.json"}]"#,
    )
    .unwrap();
    let comp = Timeline::load(&d.path().join("n.json")).unwrap();
    assert_eq!(
        comp.tracks[0].clips[1].dissolve(),
        Some(RationalTime::new(1, 2))
    );
}

#[test]
fn nested_audio_matches_the_flat_mix() {
    let d = tempfile::tempdir().unwrap();
    let flat = write(
        d.path(),
        "flat.json",
        r#"{
  "output": { "width": 64, "height": 32, "fps": "24" },
  "tracks": [ { "name": "V1", "clips": [
      { "id": "a", "source": "a.mov", "start": "1/2", "source_in": "1", "duration": "1",
        "audio": { "gain_db": "-6", "pan": "1/2", "fade_in": { "duration": "1/4" } } },
      { "id": "b", "source": "b.mov", "start": "2", "duration": "1/2" } ] } ],
  "audio": { "sample_rate": 8000 }
}"#,
    );
    let nested = d.path().join("nested.json");
    std::fs::copy(&flat, &nested).unwrap();
    edit(
        &nested,
        r#"[{"op": "nest", "clips": ["a", "b"], "path": "ab.json"}]"#,
    )
    .unwrap();
    let mut load = |p: &Path| -> anyhow::Result<Option<SourceAudio>> {
        let seed = p.file_name().unwrap().len() as f32;
        let n = 8000 * 4;
        let l = (0..n)
            .map(|i| (i as f32 * 0.01 * seed).sin() * 0.5)
            .collect();
        let r = (0..n)
            .map(|i| (i as f32 * 0.013 * seed).cos() * 0.25)
            .collect();
        Ok(Some(SourceAudio { planes: vec![l, r] }))
    };
    let mix = |path: &Path, load: &mut dyn FnMut(&Path) -> anyhow::Result<Option<SourceAudio>>| {
        let tl = Timeline::load(path).unwrap();
        let (prog, srcs, _) = ferrocut_engine::audio::resolve(&tl, load).unwrap().unwrap();
        let (ctl, _) = ferrocut_audio::analyze(&prog, &srcs).unwrap();
        ferrocut_audio::render_range(&prog, &srcs, &ctl, 0, prog.total)
    };
    let a = mix(&flat, &mut load);
    let b = mix(&nested, &mut load);
    assert_eq!(a.l.len(), b.l.len());
    assert!(a.l.iter().any(|v| *v != 0.0));
    assert_eq!(a.l, b.l);
    assert_eq!(a.r, b.r);
}
