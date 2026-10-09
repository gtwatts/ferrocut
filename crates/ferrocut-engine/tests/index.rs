//! Media index: whisper JSON parsing, transcript search, padded cut ranges,
//! the content-hashed cache (with a fake whisper-cli), GPU->CPU fallback, the
//! shot-detection hook, and (when whisper.cpp, its model and the eval clip
//! are present) a real transcription of a Sintel line.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use ferrocut_core::{BoundaryKind, Rational, RationalTime, ShotBoundary, TimeRange};
use ferrocut_engine::index::{
    self, IndexOptions, Part, Transcript, WhisperConfig, padded_range, search, shots, whisper,
};

fn rt(n: i64, d: i64) -> RationalTime {
    RationalTime::new(n, d)
}

const WHISPER_JSON: &str = r#"{
  "transcription": [
    { "offsets": { "from": 0, "to": 2000 }, "text": " So, what brings you?",
      "tokens": [
        { "text": "[_BEG_]", "offsets": { "from": 0, "to": 0 }, "p": 0.9 },
        { "text": " So", "offsets": { "from": 120, "to": 300 }, "p": 0.9 },
        { "text": ",", "offsets": { "from": 300, "to": 310 }, "p": 0.8 },
        { "text": " what", "offsets": { "from": 400, "to": 600 }, "p": 0.95 },
        { "text": " br", "offsets": { "from": 600, "to": 700 }, "p": 0.7 },
        { "text": "ings", "offsets": { "from": 700, "to": 900 }, "p": 0.99 },
        { "text": " you", "offsets": { "from": 900, "to": 1100 }, "p": 0.9 },
        { "text": "?", "offsets": { "from": 1100, "to": 1110 }, "p": 0.9 },
        { "text": "[_TT_55]", "offsets": { "from": 2000, "to": 2000 }, "p": 0.1 }
      ] },
    { "offsets": { "from": 3000, "to": 4000 }, "text": " You're lucky.",
      "tokens": [
        { "text": " You", "offsets": { "from": 3000, "to": 3200 }, "p": 0.9 },
        { "text": "'re", "offsets": { "from": 3200, "to": 3300 }, "p": 0.9 },
        { "text": " lucky", "offsets": { "from": 3300, "to": 3800 }, "p": 0.9 },
        { "text": ".", "offsets": { "from": 3800, "to": 3810 }, "p": 0.9 }
      ] }
  ]
}"#;

#[test]
fn parses_whisper_tokens_into_words() {
    let segs = whisper::parse(WHISPER_JSON).unwrap();
    assert_eq!(segs.len(), 2);
    let w: Vec<(&str, RationalTime, RationalTime)> = segs[0]
        .words
        .iter()
        .map(|w| (w.text.as_str(), w.start, w.end))
        .collect();
    assert_eq!(
        w,
        [
            ("So,", rt(3, 25), rt(31, 100)),
            ("what", rt(2, 5), rt(3, 5)),
            ("brings", rt(3, 5), rt(9, 10)),
            ("you?", rt(9, 10), rt(111, 100)),
        ]
    );
    assert_eq!(segs[0].words[2].p, 0.7, "lowest token probability");
    assert_eq!((segs[1].start, segs[1].end), (rt(3, 1), rt(4, 1)));
    assert_eq!(segs[1].words[0].text, "You're");
    assert_eq!(segs[0].text, "So, what brings you?");
}

fn transcript() -> Transcript {
    Transcript {
        engine: "whisper.cpp".into(),
        model: "m".into(),
        model_blake3: "x".into(),
        language: "en".into(),
        device: "cpu".into(),
        segments: whisper::parse(WHISPER_JSON).unwrap(),
    }
}

#[test]
fn search_finds_phrases_exactly_and_approximately() {
    let t = transcript();
    assert_eq!(index::search::tokens("You're LUCKY!"), ["youre", "lucky"]);
    let h = search(&t, "so what brings you", 5);
    assert_eq!(h[0].score, 1.0);
    assert!(h[0].exact);
    assert_eq!((h[0].start, h[0].end), (rt(3, 25), rt(111, 100)));
    assert_eq!(h[0].text, "So, what brings you?");
    assert_eq!(h[0].segment.index, 0);
    // Punctuation and case don't matter; one wrong word still matches (3/4).
    let h = search(&t, "So... what BRINGS them", 5);
    assert!(!h[0].exact && (h[0].score - 0.75).abs() < 1e-6, "{h:?}");
    assert_eq!(h[0].start, rt(3, 25));
    // Across segments: the hit spans both.
    let h = search(&t, "brings you you're lucky", 1);
    assert_eq!((h[0].start, h[0].end), (rt(3, 5), rt(381, 100)));
    assert_eq!(h[0].text, "brings you? You're lucky.");
    // Hits never overlap; nothing below the threshold.
    let h = search(&t, "you", 10);
    assert!(h.len() == 1 && h[0].text == "you?", "{h:?}");
    assert!(search(&t, "dragon hunter quest", 5).is_empty());
    assert!(search(&t, "  ", 5).is_empty());
}

#[test]
fn padded_ranges_snap_out_to_frames_and_clamp() {
    let r = Rational::from_int(24);
    let (a, b) = padded_range(rt(10, 1), rt(12, 1), rt(1, 4), Some(r), Some(rt(24, 1)));
    assert_eq!((a, b), (RationalTime::from_frames(234, r), rt(147, 12)));
    assert_eq!(b, rt(49, 4));
    // Clamped to the media.
    let (a, b) = padded_range(rt(1, 10), rt(23, 1), rt(1, 2), Some(r), Some(rt(23, 1)));
    assert_eq!((a, b), (RationalTime::ZERO, rt(23, 1)));
    // No frame rate: exact.
    let (a, b) = padded_range(rt(1, 1), rt(2, 1), rt(1, 3), None, None);
    assert_eq!((a, b), (rt(2, 3), rt(7, 3)));
}

fn write_tone(path: &Path, secs: f32) {
    let n = (secs * 16000.0) as usize;
    let s: Vec<f32> = (0..n).map(|i| 0.1 * (i as f32 * 0.05).sin()).collect();
    whisper::write_wav(path, &s, 16000).unwrap();
}

/// A whisper-cli stand-in: writes WHISPER_JSON to `<-of>.json`, counts its
/// runs, and fails without `-ng` while `gpu-broken` exists next to it.
fn fake_whisper(dir: &Path) -> PathBuf {
    let json = dir.join("fixture.json");
    std::fs::write(&json, WHISPER_JSON).unwrap();
    let p = dir.join("whisper-cli");
    std::fs::write(
        &p,
        format!(
            r#"#!/bin/sh
cpu=0
while [ $# -gt 0 ]; do
  case "$1" in -of) of=$2; shift;; -ng) cpu=1;; esac
  shift
done
echo run >> "{d}/runs"
if [ -e "{d}/gpu-broken" ] && [ $cpu = 0 ]; then echo "no CUDA device" >&2; exit 1; fi
cp "{j}" "$of.json"
"#,
            d = dir.display(),
            j = json.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn runs(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("runs"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn index_is_cached_by_content_and_falls_back_to_cpu() {
    let d = tempfile::tempdir().unwrap();
    let tools = d.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let cli = fake_whisper(&tools);
    let model = tools.join("ggml-test.bin");
    std::fs::write(&model, b"model v1").unwrap();
    let media = d.path().join("talk.wav");
    write_tone(&media, 2.0);
    let opts = |force: bool| IndexOptions {
        transcribe: true,
        shots: true,
        force,
        cached_only: false,
        whisper: WhisperConfig {
            cli: Some(cli.clone()),
            model: Some(model.clone()),
            ..Default::default()
        },
    };
    let (ix, info) = index::index_media(&media, &opts(false)).unwrap();
    assert!(!info.cached);
    assert!(
        info.index_path
            .starts_with(d.path().join(".ferrocut-index"))
    );
    let t = ix.transcript.done().expect("transcript");
    assert_eq!((t.device.as_str(), t.words()), ("gpu", 6));
    assert_eq!(ix.media.duration, Some(rt(2, 1)));
    // Audio only: no shots to detect.
    assert_eq!(
        ix.shots,
        Part::Unavailable {
            reason: "no video stream".into()
        }
    );
    // Same content: read back, whisper not run again.
    let (ix2, info2) = index::index_media(&media, &opts(false)).unwrap();
    assert!(info2.cached && ix2 == ix && runs(&tools) == 1);
    // force rebuilds; a new model or new media bytes change the key.
    index::index_media(&media, &opts(true)).unwrap();
    assert_eq!(runs(&tools), 2);
    std::fs::write(&model, b"model v2").unwrap();
    let (ix3, info3) = index::index_media(&media, &opts(false)).unwrap();
    assert!(!info3.cached && ix3.key != ix.key && runs(&tools) == 3);
    write_tone(&media, 3.0);
    let (ix4, _) = index::index_media(&media, &opts(false)).unwrap();
    assert!(ix4.key != ix3.key && ix4.media.duration == Some(rt(3, 1)));
    // GPU failure: retried on the CPU, and recorded.
    std::fs::write(tools.join("gpu-broken"), b"").unwrap();
    let (ix5, _) = index::index_media(&media, &opts(true)).unwrap();
    assert_eq!(ix5.transcript.done().unwrap().device, "cpu");
    // cached_only never builds.
    let other = d.path().join("other.wav");
    write_tone(&other, 1.0);
    let e = index::index_media(
        &other,
        &IndexOptions {
            cached_only: true,
            ..opts(false)
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("no index yet"), "{e}");
    // No whisper at all: the transcript is unavailable with the reason.
    let (ix6, _) = index::index_media(
        &other,
        &IndexOptions {
            whisper: WhisperConfig {
                cli: Some(cli.clone()),
                model: Some(tools.join("missing.bin")),
                ..Default::default()
            },
            ..opts(false)
        },
    )
    .unwrap();
    let Part::Unavailable { reason } = &ix6.transcript else {
        panic!("{:?}", ix6.transcript)
    };
    assert!(reason.contains("missing.bin"), "{reason}");
}

fn fake_detector(
    _: &Path,
    range: Option<TimeRange>,
) -> Result<Vec<ShotBoundary>, ferrocut_core::NodeError> {
    let all = vec![
        ShotBoundary {
            at: rt(2, 1),
            span: None,
            kind: BoundaryKind::Cut,
            confidence: 0.9,
        },
        ShotBoundary {
            at: rt(5, 1),
            span: Some((rt(9, 2), rt(11, 2))),
            kind: BoundaryKind::Dissolve,
            confidence: 0.6,
        },
    ];
    Ok(all
        .into_iter()
        .filter(|b| range.is_none_or(|r| r.contains(b.at)))
        .collect())
}

#[test]
fn shot_hook_uses_the_registered_detector() {
    // (This test binary's only registration.)
    assert!(shots::register("fake", fake_detector));
    assert!(!shots::register("again", fake_detector), "first wins");
    assert_eq!(shots::detector_id().as_deref(), Some("in-process:fake"));
    let b = shots::detect_shots(Path::new("x.mkv"), None, &shots::ShotOptions::default()).unwrap();
    assert_eq!(b.len(), 2);
    assert_eq!(b[1].span, Some((rt(9, 2), rt(11, 2))));
    let b = shots::detect_shots(
        Path::new("x.mkv"),
        Some(TimeRange::new(rt(4, 1), rt(2, 1))),
        &shots::ShotOptions::default(),
    )
    .unwrap();
    assert_eq!(b.len(), 1);
    // The subprocess protocol's JSON (array or {"boundaries": [...]}).
    let j = r#"{"boundaries":[{"at":"5/2","span":null,"kind":"fade_in","confidence":1.0}]}"#;
    assert_eq!(
        shots::parse_boundaries(j).unwrap()[0].kind,
        BoundaryKind::FadeIn
    );
    assert!(shots::parse_boundaries("[]").unwrap().is_empty());
    assert!(shots::parse_boundaries(r#"{"x":1}"#).is_err());
}

#[test]
fn real_whisper_finds_the_gatekeepers_line() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let clip = repo.join("eval/media/clips/d1.mkv");
    let cfg = WhisperConfig::default();
    let (Ok(_), Ok(_), true) = (
        whisper::find_cli(&cfg),
        whisper::find_model(&cfg),
        clip.is_file(),
    ) else {
        eprintln!(
            "SKIP: needs whisper.cpp (scripts/build-whisper.sh), a model and eval/media/clips/d1.mkv"
        );
        return;
    };
    let d = tempfile::tempdir().unwrap();
    let media = d.path().join("d1.mkv");
    std::fs::copy(&clip, &media).unwrap();
    let (ix, _) = index::index_media(
        &media,
        &IndexOptions {
            transcribe: true,
            ..Default::default()
        },
    )
    .unwrap();
    let t = ix.transcript.done().expect("transcript");
    let h = search(t, "So, what brings you to the land of the gatekeepers?", 1);
    assert!(h[0].score >= 0.9, "{h:?}");
    // Speech energy puts the line at ~10.8..14.85 s; whisper's token times
    // run early at the start.
    let (a, b) = (h[0].start.seconds().to_f64(), h[0].end.seconds().to_f64());
    assert!(
        (9.8..=10.9).contains(&a) && (14.5..=15.3).contains(&b),
        "{a}..{b}"
    );
}

/// A whisper-cli stand-in whose behavior follows `<dir>/mode`: `fail` exits 1
/// (GPU and CPU), `garbage` writes unparsable output, `silent` writes an
/// empty transcription (no speech), otherwise WHISPER_JSON. Counts runs.
fn moody_whisper(dir: &Path) -> PathBuf {
    let json = dir.join("fixture.json");
    std::fs::write(&json, WHISPER_JSON).unwrap();
    let p = dir.join("whisper-cli");
    std::fs::write(
        &p,
        format!(
            r#"#!/bin/sh
while [ $# -gt 0 ]; do
  case "$1" in -of) of=$2; shift;; esac
  shift
done
echo run >> "{d}/runs"
case "$(cat "{d}/mode" 2>/dev/null)" in
  fail) echo "model load failed" >&2; exit 1;;
  garbage) echo "not json" > "$of.json";;
  silent) echo '{{"transcription":[]}}' > "$of.json";;
  *) cp "{j}" "$of.json";;
esac
"#,
            d = dir.display(),
            j = json.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn index_files(media: &Path) -> usize {
    std::fs::read_dir(media.parent().unwrap().join(".ferrocut-index"))
        .map(|d| d.count())
        .unwrap_or(0)
}

#[test]
fn unavailable_transcripts_recover_without_deleting_the_cache() {
    // Seen on an installed build indexing narration: it found neither whisper-cli
    // nor a model and cached that; after only the CLI was fixed it still
    // answered "whisper-cli not found", from the cache.
    let d = tempfile::tempdir().unwrap();
    let tools = d.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let cli = moody_whisper(&tools);
    let model = tools.join("ggml-test.bin");
    let media = d.path().join("vo.wav");
    write_tone(&media, 1.0);
    let opts = |cli: &Path, model: &Path| IndexOptions {
        transcribe: true,
        shots: false,
        whisper: WhisperConfig {
            cli: Some(cli.into()),
            model: Some(model.into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let reason = |ix: &index::MediaIndex| match &ix.transcript {
        Part::Unavailable { reason } => reason.clone(),
        other => panic!("{other:?}"),
    };
    let missing_cli = tools.join("no-such-whisper-cli");
    // Neither exists: unavailable, naming the CLI.
    let (ix, _) = index::index_media(&media, &opts(&missing_cli, &model)).unwrap();
    assert!(
        reason(&ix).contains("no-such-whisper-cli does not exist"),
        "{}",
        reason(&ix)
    );
    // CLI fixed, model still missing: same key (cached file), but the reason
    // is today's, not the saved one.
    let (ix, info) = index::index_media(&media, &opts(&cli, &model)).unwrap();
    assert!(info.cached);
    assert!(
        reason(&ix).contains("ggml-test.bin does not exist"),
        "{}",
        reason(&ix)
    );
    // Both fixed: transcribed, no cache deletion needed.
    std::fs::write(&model, b"model v1").unwrap();
    let (ix, info) = index::index_media(&media, &opts(&cli, &model)).unwrap();
    assert!(!info.cached && runs(&tools) == 1);
    assert_eq!(ix.transcript.done().unwrap().words(), 6);
    // A successful transcript is reused as before.
    let (ix2, info) = index::index_media(&media, &opts(&cli, &model)).unwrap();
    assert!(info.cached && ix2 == ix && runs(&tools) == 1);
}

#[test]
fn failed_or_unparsable_transcription_is_retried_and_no_speech_is_kept() {
    let d = tempfile::tempdir().unwrap();
    let tools = d.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let cli = moody_whisper(&tools);
    let model = tools.join("ggml-test.bin");
    std::fs::write(&model, b"model v1").unwrap();
    let opts = IndexOptions {
        transcribe: true,
        shots: false,
        whisper: WhisperConfig {
            cli: Some(cli),
            model: Some(model),
            ..Default::default()
        },
        ..Default::default()
    };
    let mode = |m: &str| std::fs::write(tools.join("mode"), m).unwrap();
    for (bad, want) in [("fail", "whisper-cli failed"), ("garbage", "whisper JSON")] {
        let media = d.path().join(format!("{bad}.wav"));
        write_tone(&media, 1.0 + runs(&tools) as f32 / 10.0);
        mode(bad);
        let before = runs(&tools);
        let (ix, info) = index::index_media(&media, &opts).unwrap();
        let Part::Unavailable { reason } = &ix.transcript else {
            panic!("{bad}: {:?}", ix.transcript)
        };
        assert!(reason.contains(want), "{bad}: {reason}");
        assert!(!info.cached && index_files(&media) == 0, "{bad}: cached");
        // The next request runs whisper again; once it works, it is kept.
        mode("ok");
        let (ix, info) = index::index_media(&media, &opts).unwrap();
        assert!(!info.cached && ix.transcript.done().is_some(), "{bad}");
        assert!(runs(&tools) > before + 1);
        std::fs::remove_dir_all(d.path().join(".ferrocut-index")).unwrap();
    }
    // No speech is a transcript (empty), not an error, and it is cached.
    let media = d.path().join("room-tone.wav");
    write_tone(&media, 0.5);
    mode("silent");
    let (ix, _) = index::index_media(&media, &opts).unwrap();
    let t = ix.transcript.done().expect("an empty transcript");
    assert_eq!(t.words(), 0);
    let n = runs(&tools);
    let (ix2, info) = index::index_media(&media, &opts).unwrap();
    assert!(info.cached && ix2 == ix && runs(&tools) == n);
}

#[test]
fn legacy_cached_transcription_errors_are_retried() {
    // Before this fix a parse or decode error was cached under the model's
    // key and returned on every later request.
    let d = tempfile::tempdir().unwrap();
    let tools = d.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let cli = moody_whisper(&tools);
    let model = tools.join("ggml-test.bin");
    std::fs::write(&model, b"model v1").unwrap();
    let media = d.path().join("vo.wav");
    write_tone(&media, 1.0);
    let opts = |cached_only: bool| IndexOptions {
        transcribe: true,
        shots: true,
        cached_only,
        whisper: WhisperConfig {
            cli: Some(cli.clone()),
            model: Some(model.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let (good, info) = index::index_media(&media, &opts(false)).unwrap();
    // Rewrite it as the old code left it: same key, unavailable transcript.
    // The shots reason marks whether the cached shots are reused.
    let mut legacy = serde_json::to_value(&good).unwrap();
    legacy["transcript"] =
        serde_json::json!({"status":"unavailable","reason":"whisper JSON: expected value"});
    legacy["shots"] = serde_json::json!({"status":"unavailable","reason":"cached shots"});
    std::fs::write(&info.index_path, legacy.to_string()).unwrap();
    // cached_only reads it as it is and never runs whisper.
    let (ix, info) = index::index_media(&media, &opts(true)).unwrap();
    assert!(info.cached && ix.transcript.done().is_none() && runs(&tools) == 1);
    // A normal request retries the transcript and keeps the cached shots.
    let (ix, info) = index::index_media(&media, &opts(false)).unwrap();
    assert!(!info.cached && runs(&tools) == 2);
    assert_eq!(ix.transcript, good.transcript);
    assert_eq!(
        ix.shots,
        Part::Unavailable {
            reason: "cached shots".into()
        }
    );
    // Rewritten: the next request is a plain cache hit.
    let (ix2, info) = index::index_media(&media, &opts(false)).unwrap();
    assert!(info.cached && ix2 == ix && runs(&tools) == 2);
}
