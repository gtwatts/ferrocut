//! `ferrocut-perceive check` through the CLI: pass, fail, threshold
//! overrides (flags and config file), brief cuts and errors. The output is
//! deserialized with the field names of the engine's grader hook
//! (`ferrocut_engine::perceive`, 60b5652) and run through the hook's own
//! `interpret`, so the contract is checked end to end. `demo_av_*` renders
//! the repo's demo and checks the agreed loudness known answer.
//! Skips (passes with a note) without a GPU adapter or the demo media.

#[path = "support/schema_check.rs"]
mod schema_check;
#[path = "support/synth.rs"]
mod synth;
#[path = "support/wav.rs"]
mod wav;

use std::path::{Path, PathBuf};
use std::process::Command;

use ferrocut_core::{AdapterPreference, GpuContext, RationalTime, SharedGpu};
use ferrocut_engine::perceive::{CheckStatus, interpret};
use ferrocut_engine::render::RenderOptions;
use ferrocut_engine::{Timeline as EngineTimeline, compile, render};
use serde::Deserialize;
use synth::{H, W, synth};

/// The contract, exactly as agreed with the engine's hook.
#[derive(Debug, Deserialize)]
struct HookReport {
    schema_version: String,
    pass: bool,
    problems: Vec<HookProblem>,
}

#[derive(Debug, Deserialize)]
struct HookProblem {
    reason: String,
    range: [RationalTime; 2],
    measured: Option<f64>,
    threshold: Option<f64>,
}

const CONTRACT: [&str; 7] = [
    "missed_cut",
    "extra_cut",
    "black_frames",
    "frozen_frames",
    "flash",
    "loudness_off_target",
    "true_peak_over",
];
const ADDITIVE: [&str; 2] = ["missing_audio", "audio_join_mismatch"];

fn gpu() -> Option<SharedGpu> {
    match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => Some(SharedGpu::new(g)),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            None
        }
    }
}

/// Render `tl` (written to `<dir>/<name>.json`) to `<dir>/<name>.mkv` with the
/// report next to it, as the engine CLI does.
fn render_to(gpu: &SharedGpu, tl_path: &Path, out: &Path, cache: &Path) {
    let etl = EngineTimeline::load(tl_path).unwrap();
    let c = compile(&etl).unwrap();
    let opts = RenderOptions {
        jobs: 2,
        ..RenderOptions::new(cache.to_path_buf())
    };
    let r = render(&etl, &c, gpu, out, &opts).unwrap();
    std::fs::write(
        out.with_extension("report.json"),
        serde_json::to_string_pretty(&r).unwrap(),
    )
    .unwrap();
}

struct Out {
    code: i32,
    stdout: String,
    report: Option<HookReport>,
    json: serde_json::Value,
}

fn check(render: &Path, tl: &Path, extra: &[&str]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_ferrocut-perceive"))
        .arg("check")
        .arg(render)
        .arg("--timeline")
        .arg(tl)
        .arg("--json")
        .args(extra)
        .output()
        .unwrap();
    let stdout = String::from_utf8(o.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    let code = o.status.code().unwrap();
    eprintln!("check {extra:?} -> exit {code}\n{stdout}{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(json["schema_version"], "ferrocut.perceive.check/1");
    let schema: serde_json::Value = serde_json::from_str(ferrocut_perceive::CHECK_SCHEMA).unwrap();
    let errs = schema_check::validate(&schema, &json);
    assert!(
        errs.is_empty(),
        "check schema violations:\n{}",
        errs.join("\n")
    );
    let report: Option<HookReport> = (code != 2).then(|| {
        let r: HookReport = serde_json::from_value(json.clone()).expect("hook field names");
        assert_eq!(r.schema_version, "ferrocut.perceive.check/1");
        assert_eq!(r.pass, code == 0, "exit code agrees with pass");
        assert_eq!(r.pass, r.problems.is_empty());
        for p in &r.problems {
            assert!(
                CONTRACT.contains(&p.reason.as_str()) || ADDITIVE.contains(&p.reason.as_str()),
                "unknown reason {}",
                p.reason
            );
            assert!(p.range[0] <= p.range[1]);
        }
        r
    });
    // The engine hook's own parser agrees.
    let hook = interpret(Some(code), &stdout, &stderr);
    let want = match code {
        0 => CheckStatus::Pass,
        1 => CheckStatus::Fail,
        _ => CheckStatus::Error,
    };
    assert_eq!(hook.status, want, "{hook:?}");
    if let Some(r) = &report {
        assert_eq!(hook.problems.len(), r.problems.len());
    }
    Out {
        code,
        stdout,
        report,
        json,
    }
}

fn reasons(o: &Out) -> Vec<String> {
    let mut v: Vec<String> = o
        .report
        .as_ref()
        .map(|r| r.problems.iter().map(|p| p.reason.clone()).collect())
        .unwrap_or_default();
    v.sort();
    v.dedup();
    v
}

fn problem<'a>(o: &'a Out, reason: &str) -> &'a HookProblem {
    o.report
        .as_ref()
        .unwrap()
        .problems
        .iter()
        .find(|p| p.reason == reason)
        .unwrap_or_else(|| panic!("no {reason} in {}", o.stdout))
}

fn timeline(audio: &str, clips: &str) -> String {
    format!(
        r#"{{ "name": "check", "output": {{ "width": {W}, "height": {H}, "fps": "24", "gop": 12 }},
  "tracks": [ {{ "clips": [ {clips} ] }} ],
  "audio_tracks": [ {{ "name": "a", "clips": [ {{ "id": "s", "source": "{audio}", "start": 0, "duration": "4" }} ] }} ] }}"#
    )
}

const CLEAN: &str = r#"{ "id": "a", "source": "A.mkv", "start": 0, "duration": "2" },
  { "id": "b", "source": "B.mkv", "start": "2", "duration": "2" }"#;

/// A flash (W at frame 24 between two A shots) and 6 black frames mid-program.
const FAULTY: &str = r#"{ "id": "a", "source": "A.mkv", "start": 0, "duration": "1" },
  { "id": "f", "source": "W.mkv", "start": "1", "duration": "1/24" },
  { "id": "a2", "source": "A.mkv", "start": "25/24", "source_in": "25/24", "duration": "23/24" },
  { "id": "k", "source": "K.mkv", "start": "2", "duration": "1/4" },
  { "id": "b", "source": "B.mkv", "start": "9/4", "duration": "7/4" }"#;

fn setup(dir: &Path) {
    synth(&dir.join("A.mkv"), 72, [200, 80, 60], true);
    synth(&dir.join("B.mkv"), 72, [40, 90, 200], true);
    synth(&dir.join("W.mkv"), 24, [235, 235, 225], false);
    synth(&dir.join("K.mkv"), 24, [0, 0, 0], false);
    for (name, dbfs) in [("on_target", -14.0), ("quiet", -23.0), ("hot", -0.5)] {
        wav::write_wav(
            &dir.join(format!("{name}.wav")),
            48_000,
            2,
            &wav::sine_stereo(48_000, 1000.0, dbfs, 4.0),
        );
    }
}

fn job(gpu: &SharedGpu, dir: &Path, name: &str, audio: &str, clips: &str) -> (PathBuf, PathBuf) {
    let tl = dir.join(format!("{name}.json"));
    std::fs::write(&tl, timeline(audio, clips)).unwrap();
    let out = dir.join(format!("{name}.mkv"));
    render_to(gpu, &tl, &out, &dir.join("cache"));
    (out, tl)
}

#[test]
fn check_pass_fail_overrides_and_errors() {
    let Some(gpu) = gpu() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    setup(dir);

    // --- Pass: two clean shots, a hard cut where the timeline has it, -14 LUFS.
    let (clean, clean_tl) = job(&gpu, dir, "clean", "on_target.wav", CLEAN);
    let o = check(&clean, &clean_tl, &[]);
    assert_eq!(o.code, 0, "{}", o.stdout);
    assert_eq!(o.json["measured"]["detected_cuts"], serde_json::json!([48]));
    let i = o.json["measured"]["integrated_lufs"].as_f64().unwrap();
    assert!((i + 14.0).abs() <= 0.1, "{i}");
    // Human output (no --json) and the render report as <render>.
    let h = Command::new(env!("CARGO_BIN_EXE_ferrocut-perceive"))
        .arg("check")
        .arg(clean.with_extension("report.json"))
        .arg("--timeline")
        .arg(&clean_tl)
        .output()
        .unwrap();
    assert_eq!(h.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&h.stdout).starts_with("PASS"));

    // --- Brief cuts: the brief wants a cut at 1 s, the render cuts at 2 s.
    let brief = dir.join("brief.json");
    std::fs::write(&brief, r#"{ "cuts": ["1"] }"#).unwrap();
    let o = check(
        &clean,
        &clean_tl,
        &["--brief-cuts", brief.to_str().unwrap()],
    );
    assert_eq!(o.code, 1);
    assert_eq!(reasons(&o), ["extra_cut", "missed_cut"]);
    assert_eq!(
        problem(&o, "missed_cut").range,
        [RationalTime::new(1, 1), RationalTime::new(25, 24)]
    );
    assert_eq!(problem(&o, "extra_cut").range[0], RationalTime::new(2, 1));

    // --- Fail: flash, mid-program black, quiet audio.
    let (bad, bad_tl) = job(&gpu, dir, "faulty", "quiet.wav", FAULTY);
    let o = check(&bad, &bad_tl, &[]);
    assert_eq!(o.code, 1);
    assert_eq!(
        reasons(&o),
        ["black_frames", "flash", "loudness_off_target"]
    );
    let f = problem(&o, "flash");
    assert_eq!(
        f.range,
        [RationalTime::new(1, 1), RationalTime::new(25, 24)]
    );
    assert_eq!((f.measured, f.threshold), (Some(1.0), Some(0.0)));
    let b = problem(&o, "black_frames");
    assert_eq!(b.range, [RationalTime::new(2, 1), RationalTime::new(9, 4)]);
    assert_eq!((b.measured, b.threshold), (Some(6.0), Some(0.0)));
    let l = problem(&o, "loudness_off_target");
    assert!((l.measured.unwrap() + 23.0).abs() <= 0.1);
    assert_eq!(l.threshold, Some(-14.0));
    assert_eq!(l.range, [RationalTime::new(0, 1), RationalTime::new(4, 1)]);
    // Rational serde form on the wire, as ferrocut-types writes it.
    let wire = o.json["problems"].as_array().unwrap();
    let flash = wire.iter().find(|p| p["reason"] == "flash").unwrap();
    assert_eq!(flash["range"], serde_json::json!(["1", "25/24"]));

    // --- The same render passes with thresholds from flags, and from a config file.
    let o = check(
        &bad,
        &bad_tl,
        &[
            "--max-flash-frames",
            "1",
            "--max-black-frames",
            "6",
            "--loudness-target",
            "-23",
        ],
    );
    assert_eq!(o.code, 0, "{}", o.stdout);
    assert!(!o.json["warnings"].as_array().unwrap().is_empty());
    let cfg = dir.join("check.json");
    std::fs::write(
        &cfg,
        r#"{ "loudness_target_lufs": -23, "max_black_frames": 6, "max_flash_frames": 1 }"#,
    )
    .unwrap();
    let o = check(&bad, &bad_tl, &["--config", cfg.to_str().unwrap()]);
    assert_eq!(o.code, 0, "{}", o.stdout);
    // Flags override the file.
    let o = check(
        &bad,
        &bad_tl,
        &[
            "--config",
            cfg.to_str().unwrap(),
            "--loudness-target",
            "-14",
        ],
    );
    assert_eq!(reasons(&o), ["loudness_off_target"]);

    // --- True peak over -1 dBTP (and far too loud).
    let (hot, hot_tl) = job(&gpu, dir, "hot", "hot.wav", CLEAN);
    let o = check(&hot, &hot_tl, &[]);
    assert_eq!(reasons(&o), ["loudness_off_target", "true_peak_over"]);
    let t = problem(&o, "true_peak_over");
    assert!(t.measured.unwrap() > -1.0 && t.threshold == Some(-1.0));

    // --- Audio chunk cache entries match their schema.
    let schema: serde_json::Value =
        serde_json::from_str(ferrocut_perceive::AUDIO_CHUNK_SCHEMA).unwrap();
    let adir = dir.join("cache/perceive/v1/audio");
    let mut n = 0;
    for e in std::fs::read_dir(&adir).unwrap() {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(e.unwrap().path()).unwrap()).unwrap();
        let errs = schema_check::validate(&schema, &v);
        assert!(
            errs.is_empty(),
            "audio chunk schema violations:\n{}",
            errs.join("\n")
        );
        n += 1;
    }
    assert!(n >= 8, "{n} audio chunk entries");

    // --- Errors: exit 2, JSON with pass=false and the error.
    let o = check(&dir.join("missing.mkv"), &clean_tl, &[]);
    assert_eq!(o.code, 2);
    assert_eq!(o.json["pass"], false);
    assert!(
        o.json["error"]
            .as_str()
            .unwrap()
            .contains("missing.report.json")
    );
    std::fs::write(&cfg, r#"{ "loudness": -14 }"#).unwrap();
    assert_eq!(
        check(&clean, &clean_tl, &["--config", cfg.to_str().unwrap()]).code,
        2
    );
}

/// Agreed known answer: demo-av's master measures -14.00 LUFS integrated
/// and -2.18 dBTP, so it passes the loudness checks.
#[test]
fn demo_av_passes_loudness_with_the_known_answer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // The media is generated, not committed: <repo>/media, or FERROCUT_TEST_MEDIA.
    let media = std::env::var_os("FERROCUT_TEST_MEDIA")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("media"));
    if !media.join("cam_a.mov").exists() || !media.join("dialogue.wav").exists() {
        eprintln!("SKIP: demo media not generated (scripts/gen-test-media.sh)");
        return;
    }
    let Some(gpu) = gpu() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let media = std::fs::canonicalize(media).unwrap();
    let tl = tmp.path().join("demo-av.json");
    std::fs::write(
        &tl,
        std::fs::read_to_string(root.join("examples/demo-av.json"))
            .unwrap()
            .replace("\"../media/", &format!("\"{}/", media.display())),
    )
    .unwrap();
    let out = tmp.path().join("demo-av.mkv");
    render_to(&gpu, &tl, &out, &tmp.path().join("cache"));
    let o = check(&out, &tl, &[]);
    let m = &o.json["measured"];
    let (i, tp) = (
        m["integrated_lufs"].as_f64().unwrap(),
        m["true_peak_dbtp"].as_f64().unwrap(),
    );
    eprintln!("demo-av: {i} LUFS, {tp} dBTP; problems {:?}", reasons(&o));
    assert!((i + 14.00).abs() <= 0.01, "integrated {i}");
    assert!((tp + 2.18).abs() <= 0.01, "true peak {tp}");
    let r = reasons(&o);
    assert!(
        !r.iter()
            .any(|x| x == "loudness_off_target" || x == "true_peak_over"),
        "{r:?}"
    );
    assert!(!r.iter().any(|x| x == "audio_join_mismatch"), "{r:?}");
    // The logo's opacity fades are transitions, not cuts; the demo is a clean edit.
    assert_eq!(o.code, 0, "{}", o.stdout);
    assert_eq!(m["expected_cuts"], serde_json::json!([120, 240]));
}
