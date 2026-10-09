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

// --- Frame-grid caption timing (captions import --snap/--close-gaps/--min-duration) ---

fn t(n: i64, d: i64) -> RationalTime {
    RationalTime(Rational::new(n, d))
}
fn cue(id: &str, start: RationalTime, end: RationalTime) -> CaptionCue {
    CaptionCue {
        id: id.into(),
        start,
        end,
        text: id.to_uppercase(),
    }
}
/// Frames (at `fps`) that show no cue, between the first cue's start and
/// the last cue's end, using the compositor's rule start <= n/fps < end.
fn blank_frames(cues: &[CaptionCue], fps: Rational) -> Vec<i64> {
    let first = cues[0].start.frame_ceil(fps);
    let last = cues.last().unwrap().end.frame_ceil(fps);
    (first..last)
        .filter(|&n| {
            let at = RationalTime::from_frames(n, fps);
            !cues.iter().any(|c| c.start <= at && at < c.end)
        })
        .collect()
}
fn shown(c: &CaptionCue, fps: Rational) -> std::ops::Range<i64> {
    c.start.frame_ceil(fps)..c.end.frame_ceil(fps)
}
const RATES: [(i64, i64); 4] = [(24, 1), (25, 1), (30, 1), (30000, 1001)];

#[test]
fn explainer_cue_gap_blinks_raw_and_is_closed_by_default() {
    // explainer-16x9 authoring/captions.srt cues 1-2 at 30 fps: frame 72
    // (2.4 s) falls in the 33 ms gap and showed no caption in the master.
    let srt = "1\n00:00:00,589 --> 00:00:02,384\nAsk an agent for\na video today,\n\n2\n00:00:02,417 --> 00:00:03,965\nand it glues one together.\n";
    let cues = captions::parse(srt, CaptionFormat::Srt).unwrap();
    let fps = Rational::from_int(30);
    assert_eq!(blank_frames(&cues, fps), vec![72]);

    let (exact, report) = captions::retime(
        &cues,
        fps,
        RationalTime::ZERO,
        &captions::CaptionTiming::exact(),
    )
    .unwrap();
    assert_eq!(exact, cues);
    assert!(report.changed.is_empty());
    assert_eq!(report.gaps_kept[0].frames, [72, 72]);
    assert!(
        report.warnings[0].contains("blink"),
        "{:?}",
        report.warnings
    );

    let (fixed, report) = captions::retime(
        &cues,
        fps,
        RationalTime::ZERO,
        &captions::CaptionTiming::default(),
    )
    .unwrap();
    assert!(blank_frames(&fixed, fps).is_empty());
    assert_eq!(fixed[0].start, t(18, 30)); // 0.589 s -> frame 18 (0.6 s)
    assert_eq!(fixed[0].end, fixed[1].start);
    assert_eq!(fixed[1].start, t(73, 30));
    assert_eq!(fixed[1].end, t(119, 30));
    assert_eq!(report.changed[0].reasons, ["snapped", "closed_gap"]);
    assert_eq!(report.changed[0].frames, [18, 72]);
    assert_eq!(report.changed[0].from, [cues[0].start, cues[0].end]);
    assert!(report.gaps_kept.is_empty() && report.warnings.is_empty());
    // The source cues are an input, not something retime rewrites.
    assert_eq!(cues[0].end, t(2384, 1000));
}

#[test]
fn snapping_never_changes_a_displayed_frame() {
    let cues = vec![
        cue("a", t(0, 1), t(967, 1000)),
        cue("b", t(1, 1), t(2, 1)),
        cue("c", t(2033, 1000), t(2967, 1000)),
        cue("d", t(1001 * 100, 30000), t(1001 * 130, 30000)),
    ];
    let timing = captions::CaptionTiming {
        close_gaps: RationalTime::ZERO,
        ..Default::default()
    };
    for (n, d) in RATES {
        let fps = Rational::new(n, d);
        let (snapped, _) = captions::retime(&cues, fps, RationalTime::ZERO, &timing).unwrap();
        for (s, c) in snapped.iter().zip(&cues) {
            assert_eq!(shown(s, fps), shown(c, fps), "{} at {fps}", c.id);
            assert_eq!(
                s.start,
                RationalTime::from_frames(s.start.frame_ceil(fps), fps)
            );
            assert_eq!(s.end, RationalTime::from_frames(s.end.frame_ceil(fps), fps));
        }
    }
}

#[test]
fn one_frame_gap_depends_on_rate_and_only_real_blinks_are_reported() {
    // A=[0, 0.967), B=[1, 2): no uncaptioned frame at 24/25/30, frame 29 at
    // 30000/1001.
    let cues = vec![cue("a", t(0, 1), t(967, 1000)), cue("b", t(1, 1), t(2, 1))];
    for (n, d) in RATES {
        let fps = Rational::new(n, d);
        let (_, r) = captions::retime(
            &cues,
            fps,
            RationalTime::ZERO,
            &captions::CaptionTiming::exact(),
        )
        .unwrap();
        let expect: Vec<[i64; 2]> = if d == 1001 { vec![[29, 29]] } else { vec![] };
        assert_eq!(
            r.gaps_kept.iter().map(|g| g.frames).collect::<Vec<_>>(),
            expect
        );
        let (fixed, _) = captions::retime(
            &cues,
            fps,
            RationalTime::ZERO,
            &captions::CaptionTiming::default(),
        )
        .unwrap();
        assert!(blank_frames(&fixed, fps).is_empty(), "{fps}");
    }
}

#[test]
fn deliberate_pauses_stay_and_are_listed() {
    let cues = vec![
        cue("a", t(1, 1), t(2, 1)),
        cue("b", t(2367, 1000), t(3, 1)), // 0.367 s pause: frames 60..=71
        cue("c", t(3100, 1000), t(4, 1)), // exactly 0.1 s: closed (inclusive)
        cue("d", t(4133, 1000), t(5, 1)), // 4 frames (explainer pause): kept
    ];
    let fps = Rational::from_int(30);
    let (out, r) = captions::retime(
        &cues,
        fps,
        RationalTime::ZERO,
        &captions::CaptionTiming::default(),
    )
    .unwrap();
    assert_eq!(out[0].end, t(2, 1));
    assert_eq!(out[1].start, t(72, 30));
    assert_eq!(out[1].end, out[2].start);
    assert_eq!(out[2].end, t(4, 1));
    assert_eq!(r.gaps_kept.len(), 2);
    assert_eq!(r.gaps_kept[0].frames, [60, 71]);
    assert_eq!(r.gaps_kept[0].duration, t(12, 30));
    assert_eq!(r.gaps_kept[1].frames, [120, 123]);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
}

#[test]
fn turning_snap_off_still_closes_gaps_on_exact_times() {
    let cues = vec![
        cue("a", t(0, 1), t(2384, 1000)),
        cue("b", t(2417, 1000), t(3, 1)),
    ];
    let timing = captions::CaptionTiming {
        snap: false,
        ..Default::default()
    };
    let (out, r) =
        captions::retime(&cues, Rational::from_int(30), RationalTime::ZERO, &timing).unwrap();
    assert_eq!(out[0].end, t(2417, 1000));
    assert_eq!(out[1], cues[1]);
    assert_eq!(r.changed[0].reasons, ["closed_gap"]);
}

#[test]
fn cues_that_show_no_frame_get_one_or_are_refused() {
    let fps = Rational::from_int(30);
    // [1.010, 1.020) contains no frame time at 30 fps.
    let lone = vec![
        cue("a", t(1010, 1000), t(1020, 1000)),
        cue("b", t(2, 1), t(3, 1)),
    ];
    let (out, r) = captions::retime(
        &lone,
        fps,
        RationalTime::ZERO,
        &captions::CaptionTiming::default(),
    )
    .unwrap();
    assert_eq!((out[0].start, out[0].end), (t(31, 30), t(32, 30)));
    assert_eq!(r.changed[0].reasons, ["snapped", "one_frame"]);
    assert!(
        r.warnings[0].contains("showed no frame"),
        "{:?}",
        r.warnings
    );
    // Exact timing keeps it but says it is invisible.
    let (out, r) = captions::retime(
        &lone,
        fps,
        RationalTime::ZERO,
        &captions::CaptionTiming::exact(),
    )
    .unwrap();
    assert_eq!(out, lone);
    assert!(r.warnings[0].contains("shows no frame"));
    // The next cue starts on the same frame: no room, so it is an error
    // rather than a dropped cue or an overlap.
    let crowded = vec![
        cue("a", t(1010, 1000), t(1020, 1000)),
        cue("b", t(1025, 1000), t(2, 1)),
    ];
    let err = captions::retime(
        &crowded,
        fps,
        RationalTime::ZERO,
        &captions::CaptionTiming::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("cue a") && err.contains("same frame"), "{err}");
    // Overlaps are still refused.
    let overlap = vec![cue("a", t(0, 1), t(2, 1)), cue("b", t(1, 1), t(3, 1))];
    assert!(
        captions::retime(&overlap, fps, RationalTime::ZERO, &Default::default())
            .unwrap_err()
            .to_string()
            .contains("overlapping")
    );
}

#[test]
fn min_duration_extends_into_the_gap_only_and_never_lengthens_the_program() {
    let fps = Rational::new(30000, 1001);
    let cues = vec![
        cue("a", t(0, 1), t(1, 2)), // room: extends to the grid at/after 5/6 s
        cue("b", t(2, 1), t(2200, 1000)), // next starts at 2.4: only partial room
        cue("c", t(2400, 1000), t(3, 1)), // last: bounded by the program end
    ];
    let timing = captions::CaptionTiming {
        close_gaps: RationalTime::ZERO,
        min_duration: Some(t(5, 6)),
        ..Default::default()
    };
    let program_end = t(3, 1);
    let (out, r) = captions::retime(&cues, fps, program_end, &timing).unwrap();
    // ceil(5/6 * 30000/1001) = 25 -> 25 * 1001/30000 s.
    assert_eq!(out[0].end, RationalTime::from_frames(25, fps));
    assert!(out[0].end - out[0].start >= t(5, 6));
    assert_eq!(out[1].end, out[2].start);
    assert!(r.changed[1].reasons.contains(&"extended"));
    // Snapping moves c's end to frame 90 (3.003 s) without showing another
    // frame; min_duration adds nothing past the program end.
    assert_eq!(out[2].end, RationalTime::from_frames(90, fps));
    assert!(!r.changed[2].reasons.contains(&"extended"));
    for w in ["cue b", "cue c"] {
        assert!(
            r.warnings.iter().any(|x| x.contains(w)),
            "{w}: {:?}",
            r.warnings
        );
    }
    assert!(out.windows(2).all(|p| p[0].end <= p[1].start));
}

#[test]
fn timing_options_are_bounded() {
    let cues = vec![cue("a", t(0, 1), t(1, 1))];
    for timing in [
        captions::CaptionTiming {
            close_gaps: t(3, 1),
            ..Default::default()
        },
        captions::CaptionTiming {
            close_gaps: t(-1, 10),
            ..Default::default()
        },
        captions::CaptionTiming {
            min_duration: Some(RationalTime::ZERO),
            ..Default::default()
        },
    ] {
        assert!(
            captions::retime(&cues, Rational::from_int(30), RationalTime::ZERO, &timing).is_err()
        );
    }
    let parsed: captions::CaptionTiming =
        serde_json::from_value(json!({"close_gaps": "0"})).unwrap();
    assert!(parsed.snap && parsed.close_gaps == RationalTime::ZERO);
    assert!(serde_json::from_value::<captions::CaptionTiming>(json!({"snapp": true})).is_err());
}

#[test]
fn caption_cli_imports_frame_snapped_cues_and_reports_changes() {
    let dir = tempfile::tempdir().unwrap();
    let ntsc30 = Timeline::from_json(
        r#"{"output":{"width":320,"height":180,"fps":"30"},"tracks":[{"name":"Picture","clips":[]}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("t.json"),
        serde_json::to_vec_pretty(&ntsc30).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("cues.srt"),
        "1\n00:00:00,589 --> 00:00:02,384\nOne\n\n2\n00:00:02,417 --> 00:00:03,965\nTwo\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("style.json"),
        serde_json::to_vec(&style()).unwrap(),
    )
    .unwrap();
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_ferrocut"))
            .current_dir(dir.path())
            // A bare style file name (no directory) used to fail with an
            // unnamed "No such file or directory".
            .args([
                "captions",
                "import",
                "t.json",
                "cues.srt",
                "--style",
                "style.json",
            ])
            .args(extra)
            .output()
            .unwrap()
    };
    let exact = run(&["--exact-timing", "--dry-run"]);
    assert!(
        exact.status.success(),
        "{}",
        String::from_utf8_lossy(&exact.stderr)
    );
    assert!(String::from_utf8_lossy(&exact.stderr).contains("frame(s) 72..=72"));
    assert!(
        !run(&["--exact-timing", "--close-gaps", "0"])
            .status
            .success()
    );

    let out = run(&[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["caption_timing"]["changed"][0]["reasons"],
        json!(["snapped", "closed_gap"])
    );
    let tl = Timeline::load(dir.path().join("t.json")).unwrap();
    let cues = captions::from_track(&tl, "Captions").unwrap();
    assert!(blank_frames(&cues, tl.output.fps).is_empty());
    assert_eq!(cues[0].end, t(73, 30));
}
