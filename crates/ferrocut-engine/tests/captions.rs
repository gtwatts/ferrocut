use std::path::{Path, PathBuf};
use std::process::Command;

use ferrocut_core::{Rational, RationalTime};
use ferrocut_engine::captions::{self, CaptionCue, CaptionFormat};
use ferrocut_engine::{Timeline, project};
use serde_json::json;

fn font() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/text/NotoSans-Regular.ttf")
}
fn style() -> ferrocut_engine::text::TextSpec {
    serde_json::from_value(json!({"content":"", "font":font(), "font_size":"24", "align":"center"}))
        .unwrap()
}
fn empty() -> Timeline {
    Timeline::from_json(r#"{"output":{"width":320,"height":180,"fps":"30000/1001"},"tracks":[{"name":"Picture","clips":[]}]}"#).unwrap()
}

#[test]
fn exact_srt_crlf_bom_unicode_and_multiline_round_trip() {
    let text = "\u{feff}1\r\n00:00:00,001 --> 01:02:03,456\r\nHello café\r\nمرحبا\r\n\r\n2\r\n01:02:03,456 --> 01:02:04,999\r\nEnd\r\n";
    let cues = captions::parse(text, CaptionFormat::Srt).unwrap();
    assert_eq!(cues[0].start, RationalTime(Rational::new(1, 1000)));
    assert_eq!(cues[0].end, RationalTime(Rational::new(3_723_456, 1000)));
    assert_eq!(cues[0].text, "Hello café\nمرحبا");
    assert_eq!(
        captions::parse(
            &captions::write(&cues, CaptionFormat::Srt).unwrap(),
            CaptionFormat::Srt
        )
        .unwrap(),
        cues
    );
}

#[test]
fn webvtt_comments_short_clocks_named_ids_and_entities() {
    let input = "WEBVTT\n\nNOTE production note\nnot a subtitle\n\nopening\n00:01.125 --> 00:02.500\nFish &amp; chips &lt;3 &nbsp;&lrm;&rlm;\n\n00:02.500 --> 00:03.000\nNext\n";
    let cues = captions::parse(input, CaptionFormat::Vtt).unwrap();
    assert_eq!(cues[0].id, "opening");
    assert_eq!(cues[0].text, "Fish & chips <3 \u{a0}\u{200e}\u{200f}");
    assert_eq!(
        captions::parse(
            &captions::write(&cues, CaptionFormat::Vtt).unwrap(),
            CaptionFormat::Vtt
        )
        .unwrap(),
        cues
    );
}

#[test]
fn malformed_or_unsupported_subtitles_report_instead_of_losing_data() {
    for s in [
        "1\n00:00:00,000 --> 00:00:00,000\nzero\n",
        "1\n00:60:00,000 --> 01:01:00,000\nbad minute\n",
        "1\n00:00:00,000 --> 00:00:01,00\nbad precision\n",
        "1\n999999999999999999999999999999999999999:00:00,000 --> 00:00:01,000\noverflow\n",
        "1\n00:00:00,000 --> 00:00:01,000\n<i>rich</i>\n",
        "1\n00:00:00,000 --> 00:00:01,000\nfirst\n\n1\n00:00:02,000 --> 00:00:03,000\nduplicate\n",
    ] {
        assert!(captions::parse(s, CaptionFormat::Srt).is_err(), "{s}");
    }
    for s in [
        "00:01.000 --> 00:02.000\nmissing header\n",
        "WEBVTT\n\nSTYLE\n::cue {color:red}\n",
        "WEBVTT\n\n00:01.000 --> 00:02.000 align:right\npositioned\n",
        "WEBVTT\n\n00:01.000 --> 00:02.000\n<v Alice>voice\n",
    ] {
        assert!(captions::parse(s, CaptionFormat::Vtt).is_err(), "{s}");
    }
}

#[test]
fn export_quantization_is_explicit_and_cannot_collapse_cues() {
    let c = CaptionCue {
        id: "frame".into(),
        start: RationalTime(Rational::new(1001, 30000)),
        end: RationalTime(Rational::new(2002, 30000)),
        text: "frame time".into(),
    };
    let out = captions::write(std::slice::from_ref(&c), CaptionFormat::Srt).unwrap();
    assert!(out.contains("00:00:00,033 --> 00:00:00,067"), "{out}");
    let tiny = CaptionCue {
        start: RationalTime::ZERO,
        end: RationalTime(Rational::new(1, 10000)),
        ..c
    };
    assert!(
        captions::write(&[tiny], CaptionFormat::Srt)
            .unwrap_err()
            .to_string()
            .contains("collapses")
    );
}

#[test]
fn webvtt_reserved_identifiers_cannot_silently_turn_cues_into_comments() {
    for id in ["NOTE", "NOTE a comment", "NOTE\tcomment", "STYLE", "REGION"] {
        let cue = CaptionCue {
            id: id.into(),
            start: RationalTime::ZERO,
            end: RationalTime(Rational::ONE),
            text: "Must survive".into(),
        };
        assert!(
            captions::write(&[cue], CaptionFormat::Vtt)
                .unwrap_err()
                .to_string()
                .contains("reserved")
        );
    }
    let cue = CaptionCue {
        id: "NOTEbook".into(),
        start: RationalTime::ZERO,
        end: RationalTime(Rational::ONE),
        text: "Must survive".into(),
    };
    assert_eq!(
        captions::parse(
            &captions::write(std::slice::from_ref(&cue), CaptionFormat::Vtt).unwrap(),
            CaptionFormat::Vtt
        )
        .unwrap(),
        vec![cue]
    );
}

#[test]
fn overlaps_round_trip_but_single_track_import_refuses_them() {
    let cues = captions::parse(
        "1\n00:00:00,000 --> 00:00:02,000\na\n\n2\n00:00:01,000 --> 00:00:03,000\nb\n",
        CaptionFormat::Srt,
    )
    .unwrap();
    assert_eq!(
        captions::parse(
            &captions::write(&cues, CaptionFormat::Srt).unwrap(),
            CaptionFormat::Srt
        )
        .unwrap(),
        cues
    );
    assert!(
        captions::import_ops(&empty(), &cues, "Captions", &style())
            .unwrap_err()
            .to_string()
            .contains("overlapping")
    );
}

#[test]
fn captions_use_normal_atomic_edits_dry_run_and_undo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&empty()).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let cues = captions::parse(
        "1\n00:00:00,000 --> 00:00:01,000\nHello\n\n2\n00:00:01,000 --> 00:00:02,000\nWorld\n",
        CaptionFormat::Srt,
    )
    .unwrap();
    let ops = captions::import_ops(&empty(), &cues, "Captions", &style()).unwrap();
    let dry = project::edit_file(
        &path,
        &ops,
        &project::EditOptions {
            dry_run: true,
            probe: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!dry.written);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let applied = project::edit_file(
        &path,
        &ops,
        &project::EditOptions {
            probe: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(applied.written && applied.journal_seq.is_some());
    let tl = Timeline::load(&path).unwrap();
    let exported = captions::from_track(&tl, "Captions").unwrap();
    assert_eq!(
        exported
            .iter()
            .map(|c| (&c.text, c.start, c.end))
            .collect::<Vec<_>>(),
        cues.iter()
            .map(|c| (&c.text, c.start, c.end))
            .collect::<Vec<_>>()
    );
    let failed = project::edit_file(
        &path,
        &ops,
        &project::EditOptions {
            probe: false,
            ..Default::default()
        },
    );
    assert!(failed.is_err());
    assert_eq!(
        captions::from_track(&Timeline::load(&path).unwrap(), "Captions").unwrap(),
        exported
    );
    project::undo(&path, false).unwrap();
    assert_eq!(Timeline::load(&path).unwrap().tracks.len(), 1);
}

#[test]
fn caption_cli_import_export_and_existing_output_protection() {
    let dir = tempfile::tempdir().unwrap();
    let timeline = dir.path().join("timeline.json");
    let subtitles = dir.path().join("input.srt");
    let style_path = dir.path().join("style.json");
    let out = dir.path().join("out.vtt");
    std::fs::write(&timeline, serde_json::to_vec_pretty(&empty()).unwrap()).unwrap();
    std::fs::write(
        &subtitles,
        "1\n00:00:00,001 --> 00:00:01,002\nEditable captions\n",
    )
    .unwrap();
    std::fs::write(&style_path, serde_json::to_vec(&style()).unwrap()).unwrap();
    let run = |args: Vec<&std::ffi::OsStr>| {
        Command::new(env!("CARGO_BIN_EXE_ferrocut"))
            .args(args)
            .output()
            .unwrap()
    };
    let import = run(vec![
        "captions".as_ref(),
        "import".as_ref(),
        timeline.as_os_str(),
        subtitles.as_os_str(),
        "--style".as_ref(),
        style_path.as_os_str(),
    ]);
    assert!(
        import.status.success(),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
    let args = vec![
        "captions".as_ref(),
        "export".as_ref(),
        timeline.as_os_str(),
        "--track".as_ref(),
        "Captions".as_ref(),
        "--output".as_ref(),
        out.as_os_str(),
    ];
    let exported = run(args.clone());
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let bytes = std::fs::read(&out).unwrap();
    assert!(!run(args).status.success());
    assert_eq!(std::fs::read(&out).unwrap(), bytes);
    assert_eq!(
        captions::parse(std::str::from_utf8(&bytes).unwrap(), CaptionFormat::Vtt).unwrap()[0].text,
        "Editable captions"
    );
}
