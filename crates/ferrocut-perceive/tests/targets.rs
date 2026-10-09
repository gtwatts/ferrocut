//! Check threshold provenance: flag > supplied config key > the render's
//! recorded loudness target/ceiling > the timeline (only when the render
//! report lacks them) > default, and how the audio is graded against them.
//! Pure: synthetic render reports, timelines and loudness figures; no GPU,
//! decode or render.
//!
//! Before this, a master the engine normalized to its authored -16 LUFS
//! failed `loudness_off_target` against the fixed -14 default, and an
//! authored -1.5 dBTP ceiling was graded at -1.

use std::path::{Path, PathBuf};

use ferrocut_core::{Rational, RationalTime};
use ferrocut_perceive::audio::{AudioReport, Loudness};
use ferrocut_perceive::check::{grade_audio, reason};
use ferrocut_perceive::input::{RenderReport, Timeline};
use ferrocut_perceive::targets::{Authored, Recorded, Resolved, ThresholdSource};
use serde_json::{Value, json};

use ThresholdSource::*;

/// A render report whose audio analysis is `analysis` (`None`: no analysis
/// object, as older reports).
fn render(analysis: Option<Value>) -> RenderReport {
    let mut audio = json!({"sample_rate": 48000, "channels": 2, "samples": 96000, "blake3": "x"});
    if let Some(a) = analysis {
        audio["analysis"] = a;
    }
    serde_json::from_value(
        json!({"total_frames": 48, "chunk_frames": 24, "chunks": [], "audio": audio}),
    )
    .unwrap()
}
fn timeline(loudness: Option<Value>) -> Timeline {
    let mut t = json!({"output": {"width": 16, "height": 16, "fps": "24"}, "tracks": []});
    if let Some(l) = loudness {
        t["audio"] = json!({"sample_rate": 48000, "loudness": l});
    }
    serde_json::from_value(t).unwrap()
}
fn rendered(target: Value, ceiling: Value) -> RenderReport {
    render(Some(
        json!({"ducks": [], "target_lufs": target, "true_peak_ceiling_dbtp": ceiling,
                       "norm_gain_db": 0.0, "limiter_max_reduction_db": 0.0, "passes": 1}),
    ))
}
fn resolve(
    rr: &RenderReport,
    tl: &Timeline,
    config: Option<&Path>,
    flags: &[(&str, Value)],
) -> Resolved {
    Resolved::resolve(&Authored::new(rr, tl).unwrap(), config, flags).unwrap()
}
/// (target, tolerance, ceiling) values and sources.
fn audio_keys(r: &Resolved) -> [(f64, ThresholdSource); 3] {
    let t = r.loudness_target();
    [
        (t.target_lufs.value, t.target_lufs.source),
        (t.tolerance_lu.value, t.tolerance_lu.source),
        (t.true_peak_max_dbtp.value, t.true_peak_max_dbtp.source),
    ]
}
fn config(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("check.json");
    std::fs::write(&p, body).unwrap();
    p
}
fn master(integrated: Option<f64>, true_peak: Option<f64>) -> AudioReport {
    AudioReport {
        source: "master.mkv".into(),
        sample_rate: 48000,
        channels: 2,
        duration: RationalTime(Rational::from_int(2)),
        loudness: Loudness {
            integrated_lufs: integrated,
            loudness_range_lu: None,
            momentary_max_lufs: None,
            short_term_max_lufs: None,
            true_peak_dbtp: true_peak,
            sample_peak_dbfs: None,
            silence: vec![],
            clipping: vec![],
        },
        delta_ebu_r128_lu: None,
        delta_streaming_lu: None,
        short_term_per_second: vec![],
        engine: None,
        join_mismatch_chunks: vec![],
    }
}
fn reasons(list: &[ferrocut_perceive::check::Problem]) -> Vec<&str> {
    list.iter().map(|p| p.reason.as_str()).collect()
}
fn fps() -> Rational {
    Rational::from_int(24)
}

#[test]
fn the_render_recorded_target_and_ceiling_are_the_default() {
    // R1: the authored -16 LUFS / -1.5 dBTP the engine normalized to.
    let r = resolve(
        &rendered(json!(-16.0), json!(-1.5)),
        &timeline(Some(
            json!({"target_lufs": "-16", "true_peak_dbtp": "-3/2"}),
        )),
        None,
        &[],
    );
    assert_eq!(
        audio_keys(&r),
        [(-16.0, Render), (1.0, Default), (-1.5, Render)]
    );
    assert!(r.mismatches.is_empty());
    // Measured -16.0 passes; before, the fixed -14 default failed it.
    let (p, w) = grade_audio(Some(&master(Some(-16.0), Some(-1.51))), &[], 48, fps(), &r);
    assert!(p.is_empty(), "{p:?}");
    assert!(w.is_empty());
    let old = Resolved::defaults(ferrocut_perceive::CheckThresholds::default());
    let (p, _) = grade_audio(
        Some(&master(Some(-16.0), Some(-1.51))),
        &[],
        48,
        fps(),
        &old,
    );
    assert_eq!(reasons(&p), [reason::LOUDNESS_OFF_TARGET]);
    // The authored ceiling is enforced: -1.2 dBTP passed the old -1 default.
    let (p, _) = grade_audio(Some(&master(Some(-16.0), Some(-1.2))), &[], 48, fps(), &r);
    assert_eq!(reasons(&p), [reason::TRUE_PEAK_OVER]);
    assert_eq!(p[0].threshold, Some(-1.5));
    assert_eq!(
        p[0].threshold_sources.as_ref().unwrap()["true_peak_max_dbtp"],
        Render
    );
    let (p, _) = grade_audio(Some(&master(Some(-16.0), Some(-1.2))), &[], 48, fps(), &old);
    // The old -1 dBTP default lets -1.2 through (only the loudness fails).
    assert_eq!(reasons(&p), [reason::LOUDNESS_OFF_TARGET], "{p:?}");
}

#[test]
fn flags_and_supplied_config_keys_override_per_key() {
    let d = tempfile::tempdir().unwrap();
    let rr = rendered(json!(-16.0), json!(-1.5));
    let tl = timeline(Some(
        json!({"target_lufs": "-16", "true_peak_dbtp": "-3/2"}),
    ));
    // R2: a delivery spec flag wins for its key only (mixed provenance).
    let r = resolve(&rr, &tl, None, &[("loudness_target_lufs", json!(-14.0))]);
    assert_eq!(
        audio_keys(&r),
        [(-14.0, Flag), (1.0, Default), (-1.5, Render)]
    );
    let (p, _) = grade_audio(Some(&master(Some(-16.0), Some(-1.51))), &[], 48, fps(), &r);
    assert_eq!(
        reasons(&p),
        [reason::LOUDNESS_OFF_TARGET],
        "an explicit spec is still enforced"
    );
    let srcs = p[0].threshold_sources.as_ref().unwrap();
    assert_eq!(
        (srcs["loudness_target_lufs"], srcs["loudness_tolerance_lu"]),
        (Flag, Default)
    );
    // R3: config key, then a flag over it.
    let c = config(d.path(), r#"{"loudness_target_lufs": -18}"#);
    let r = resolve(&rr, &tl, Some(&c), &[]);
    assert_eq!(audio_keys(&r)[0], (-18.0, Config));
    let r = resolve(
        &rr,
        &tl,
        Some(&c),
        &[("loudness_target_lufs", json!(-14.0))],
    );
    assert_eq!(audio_keys(&r)[0], (-14.0, Flag));
    // R3b: a config naming only another key keeps the render's values.
    let c = config(d.path(), r#"{"cut_tolerance_frames": 2}"#);
    let r = resolve(&rr, &tl, Some(&c), &[]);
    assert_eq!(
        audio_keys(&r),
        [(-16.0, Render), (1.0, Default), (-1.5, Render)]
    );
    assert_eq!(
        (
            r.thresholds.cut_tolerance_frames,
            r.source("cut_tolerance_frames")
        ),
        (2, Config)
    );
    // Unknown config keys are still rejected.
    let c = config(d.path(), r#"{"loudness": 1}"#);
    assert!(Resolved::resolve(&Authored::new(&rr, &tl).unwrap(), Some(&c), &[]).is_err());
}

#[test]
fn missing_metadata_falls_back_to_the_timeline_but_null_means_no_target() {
    let tl16 = timeline(Some(
        json!({"target_lufs": "-16", "true_peak_dbtp": "-3/2"}),
    ));
    // R4: no analysis object (older report): the timeline.
    let r = resolve(&render(None), &tl16, None, &[]);
    assert_eq!(
        audio_keys(&r),
        [(-16.0, Timeline), (1.0, Default), (-1.5, Timeline)]
    );
    assert!(r.mismatches.is_empty());
    // An analysis object without those keys counts as older too.
    let a = Authored::new(&render(Some(json!({"ducks": []}))), &tl16).unwrap();
    assert_eq!(
        (a.render_target, a.render_ceiling),
        (Recorded::Absent, Recorded::Absent)
    );
    // The ceiling defaults to -1 when the timeline names only a target.
    let r = resolve(
        &render(None),
        &timeline(Some(json!({"target_lufs": -20}))),
        None,
        &[],
    );
    assert_eq!(
        audio_keys(&r),
        [(-20.0, Timeline), (1.0, Default), (-1.0, Timeline)]
    );
    // R5: nothing authored anywhere: defaults.
    let r = resolve(&render(None), &timeline(None), None, &[]);
    assert_eq!(
        audio_keys(&r),
        [(-14.0, Default), (1.0, Default), (-1.0, Default)]
    );
    // R6b: rendered without a target (explicit null) while the timeline now
    // has one: the defaults, NOT the timeline, and a mismatch warning.
    let r = resolve(&rendered(Value::Null, Value::Null), &tl16, None, &[]);
    assert_eq!(
        audio_keys(&r),
        [(-14.0, Default), (1.0, Default), (-1.0, Default)]
    );
    assert_eq!(r.mismatches.len(), 2);
    let (_, w) = grade_audio(Some(&master(Some(-14.0), Some(-2.0))), &[], 48, fps(), &r);
    assert_eq!(
        reasons(&w),
        [
            reason::LOUDNESS_TARGET_MISMATCH,
            reason::LOUDNESS_TARGET_MISMATCH
        ]
    );
    assert!(
        w[0].message.contains("none") && w[0].message.contains("-16"),
        "{}",
        w[0].message
    );
}

#[test]
fn a_stale_render_is_graded_as_rendered_and_reported() {
    // R6: render -16, timeline edited to -12 afterwards.
    let r = resolve(
        &rendered(json!(-16.0), json!(-1.5)),
        &timeline(Some(
            json!({"target_lufs": "-12", "true_peak_dbtp": "-3/2"}),
        )),
        None,
        &[],
    );
    assert_eq!(audio_keys(&r)[0], (-16.0, Render));
    let (p, w) = grade_audio(Some(&master(Some(-16.0), Some(-2.0))), &[], 48, fps(), &r);
    assert!(p.is_empty(), "graded against the render's target: {p:?}");
    assert_eq!(reasons(&w), [reason::LOUDNESS_TARGET_MISMATCH]);
    assert_eq!((w[0].measured, w[0].threshold), (Some(-16.0), Some(-12.0)));
    // R6c: the timeline no longer normalizes at all.
    let r = resolve(
        &rendered(json!(-16.0), json!(-1.5)),
        &timeline(None),
        None,
        &[],
    );
    assert_eq!(audio_keys(&r)[0], (-16.0, Render));
    assert_eq!(r.mismatches.len(), 2);
}

#[test]
fn silent_streams_and_missing_streams_stay_distinct() {
    let r = resolve(
        &rendered(json!(-16.0), json!(-1.5)),
        &timeline(Some(json!({"target_lufs": "-16"}))),
        None,
        &[],
    );
    // R8: an existing but silent stream still fails loudness_off_target.
    let (p, _) = grade_audio(Some(&master(None, None)), &[], 48, fps(), &r);
    assert_eq!(reasons(&p), [reason::LOUDNESS_OFF_TARGET]);
    assert_eq!((p[0].measured, p[0].threshold), (None, Some(-16.0)));
    assert!(p[0].message.contains("silent"));
    // No stream: missing_audio only, no loudness problem or mismatch, unless
    // audio isn't required.
    let (p, w) = grade_audio(None, &[], 48, fps(), &r);
    assert_eq!(reasons(&p), [reason::MISSING_AUDIO]);
    assert!(w.is_empty());
    let r = resolve(
        &rendered(json!(-16.0), json!(-1.5)),
        &timeline(None),
        None,
        &[("require_audio", json!(false))],
    );
    let (p, w) = grade_audio(None, &[], 48, fps(), &r);
    assert!(p.is_empty() && w.is_empty(), "{p:?} {w:?}");
}

#[test]
fn the_report_line_names_each_source() {
    let r = resolve(
        &rendered(json!(-16.0), json!(-1.5)),
        &timeline(None),
        None,
        &[("loudness_target_lufs", json!(-14.0))],
    );
    assert_eq!(
        r.loudness_target().line(),
        "loudness target -14 LUFS (flag) ±1 LU (default), true peak ≤ -1.5 dBTP (render)"
    );
    let j = serde_json::to_value(r.loudness_target()).unwrap();
    assert_eq!(
        j["true_peak_max_dbtp"],
        json!({"value": -1.5, "source": "render"})
    );
}

#[test]
fn a_malformed_recorded_value_is_an_error_not_a_timeline_fallback() {
    // A present target or ceiling must be a number or null; falling back to
    // a (possibly edited) timeline would grade the wrong intent silently.
    let tl = timeline(Some(json!({"target_lufs": "-12"})));
    for (target, ceiling, field) in [
        (json!(true), json!(-1.5), "target_lufs"),
        (json!("bogus"), json!(-1.5), "target_lufs"),
        (json!(-16.0), json!({"db": -1}), "true_peak_ceiling_dbtp"),
    ] {
        let e = Authored::new(&rendered(target, ceiling), &tl)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains(&format!("audio.analysis.{field} must be a number or null")),
            "{e}"
        );
    }
}
