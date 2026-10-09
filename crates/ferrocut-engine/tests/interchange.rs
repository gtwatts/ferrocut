//! Original synthetic fixtures exercise the native adapter and real upstream codecs.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ferrocut_core::{Rational, RationalTime};
use ferrocut_engine::interchange::{self, ExportOptions, Format, ImportOptions, SourceMetadata};
use ferrocut_engine::timeline::Timeline;
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

fn t(value: &str) -> RationalTime {
    RationalTime(value.parse().unwrap())
}
fn timeline(value: Value) -> Timeline {
    let tl: Timeline = serde_json::from_value(value).unwrap();
    tl.validate().unwrap();
    tl
}
fn simple() -> Timeline {
    timeline(
        json!({"name":"Original synthetic edit","output":{"width":640,"height":360,"fps":"24","duration":"6"},"tracks":[{"name":"Picture","clips":[
            {"id":"a","source":"media/first.mov","start":"1","source_in":"2","duration":"2"},
            {"id":"b","source":"media/second.mov","start":"4","source_in":"3","duration":"2"}
        ]}]}),
    )
}
fn options(audio: bool) -> ExportOptions {
    let mut media = BTreeMap::new();
    for path in ["media/first.mov", "media/second.mov"] {
        media.insert(
            PathBuf::from(path),
            SourceMetadata {
                duration: t("20"),
                width: Some(640),
                height: Some(360),
                fps: Some(Rational::from_int(24)),
                sample_rate: audio.then_some(48000),
                channels: audio.then_some(2),
            },
        );
    }
    ExportOptions {
        base_dir: Some(PathBuf::from("/original-project")),
        media,
        ..Default::default()
    }
}
fn roundtrip(tl: &Timeline, format: Format, options: &ExportOptions) -> interchange::ImportResult {
    let out = interchange::export_timeline(tl, format, options).unwrap();
    assert!(
        out.report
            .entries
            .iter()
            .all(|e| e.severity != interchange::Severity::Loss || e.feature == "codec_track_name"),
        "unexpected export losses: {:?}",
        out.report.entries
    );
    assert_eq!(
        interchange::detect(&out.bytes, Some(format.extension())),
        Some(format)
    );
    let incoming =
        interchange::import_document(&out.bytes, format, &ImportOptions::default()).unwrap();
    assert!(
        !incoming.report.has_losses(),
        "unexpected import losses: {:?}",
        incoming.report.entries
    );
    incoming.timeline.validate().unwrap();
    incoming
}

#[test]
fn checked_tick_bridge_preserves_broadcast_and_audio_time_and_rejects_precision_overflow() {
    for value in ["0", "1001/24000", "1/48000", "-1/25", "1234567/1000"] {
        assert_eq!(
            interchange::from_tick(interchange::to_tick(t(value)).unwrap()).unwrap(),
            t(value)
        );
    }
    assert_eq!(
        interchange::to_tick(t("1")).unwrap(),
        Tick(TICKS_PER_SECOND)
    );
    assert!(interchange::to_tick(t("1/1000000007")).is_err());
    assert!(interchange::to_tick(t("9223372036854775807")).is_err());
    assert!(interchange::from_tick(Tick(i64::MAX)).is_err());
}

#[test]
fn original_otio_and_fcp7_roundtrip_preserves_gaps_tracks_paths_and_source_edits() {
    for format in [Format::Otio, Format::Fcp7Xml] {
        let original = simple();
        let imported = roundtrip(&original, format, &options(false));
        let tl = imported.timeline;
        assert_eq!(tl.output.fps, original.output.fps);
        assert_eq!(tl.output.duration, Some(t("6")));
        assert_eq!(tl.tracks.len(), 1);
        if format == Format::Otio {
            assert_eq!(tl.tracks[0].name, "Picture");
        }
        for (a, b) in tl.tracks[0].clips.iter().zip(&original.tracks[0].clips) {
            assert_eq!(
                (a.start, a.source_in, a.duration),
                (b.start, b.source_in, b.duration)
            );
            assert_eq!(a.source, Path::new("/original-project").join(&b.source));
        }
        assert_eq!(tl.tracks[0].clips.len(), 2);
    }
}

#[test]
fn native_clip_masks_are_reported_as_interchange_loss_instead_of_silently_dropped() {
    let mut tl = simple();
    tl.tracks[0].clips[0].masks = serde_json::from_value(json!([
        {"geometry":{"type":"ellipse","center":[320,180],"radius":[100,80]},"feather":8}
    ]))
    .unwrap();
    for format in [Format::Otio, Format::Fcp7Xml] {
        let exported = interchange::export_timeline(&tl, format, &options(false)).unwrap();
        assert!(exported.report.has_losses());
        assert!(exported.report.entries.iter().any(|entry|
            entry.feature=="clip_compositing" && entry.message.contains("masks")),
            "mask loss missing: {:?}",exported.report.entries);
    }
}

#[test]
fn opacity_motion_keys_use_source_origin_and_preserve_native_geometry() {
    let mut value = serde_json::to_value(simple()).unwrap();
    value["tracks"][0]["clips"][0]["opacity"] = json!({"keyframes":[{"t":"0","v":"0.25"},{"t":"1","v":"0.75","interp":"hold"},{"t":"2","v":"1"}]});
    value["tracks"][0]["clips"][0]["transform"] = json!({"position":[{"keyframes":[{"t":"0","v":"300"},{"t":"2","v":"400"}]},"180"],"anchor":["100","90"],"scale":["1.5","0.75"],"rotation":{"keyframes":[{"t":"0","v":"-10"},{"t":"2","v":"30"}]}});
    let original = timeline(value);
    assert!(matches!(
        original.tracks[0].clips[0]
            .transform
            .as_ref()
            .unwrap()
            .scale,
        Some(ferrocut_engine::transform::Scale::Xy(_))
    ));
    let reparsed: Timeline =
        serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
    assert_eq!(
        original.tracks[0].clips[0].transform,
        reparsed.tracks[0].clips[0].transform
    );
    let mut opts = options(false);
    opts.media
        .get_mut(Path::new("media/first.mov"))
        .unwrap()
        .width = Some(1280);
    opts.media
        .get_mut(Path::new("media/first.mov"))
        .unwrap()
        .height = Some(720);
    for format in [Format::Otio, Format::Fcp7Xml] {
        let mut fixture = original.clone();
        if format == Format::Fcp7Xml {
            fixture.tracks[0].clips[0].transform.as_mut().unwrap().scale =
                Some(ferrocut_engine::transform::Scale::Uniform(
                    ferrocut_core::Animatable::Constant("1.5".parse().unwrap()),
                ));
        }
        let imported = roundtrip(&fixture, format, &opts);
        let a = &fixture.tracks[0].clips[0];
        let b = &imported.timeline.tracks[0].clips[0];
        for time in ["0", "1/2", "1", "3/2", "2"] {
            let time = t(time);
            assert!((a.opacity.eval(time) - b.opacity.eval(time)).abs() < 1e-10);
            let placement = |fit| ferrocut_engine::placement::Placement {
                native: (1280, 720),
                output: (640, 360),
                fit,
            };
            let at = placement(a.effective_fit(&fixture.output))
                .placed(a.transform.as_ref().unwrap(), time);
            let bt = placement(b.effective_fit(&imported.timeline.output))
                .placed(b.transform.as_ref().unwrap(), time);
            assert!((at.position[0] - bt.position[0]).abs() < 1e-8);
            assert!((at.anchor[0] - bt.anchor[0]).abs() < 1e-8);
            assert!((at.scale[0] - bt.scale[0]).abs() < 1e-8);
            assert!((at.scale[1] - bt.scale[1]).abs() < 1e-8);
            assert!((at.rotation_deg - bt.rotation_deg).abs() < 1e-8);
        }
    }
    let out = interchange::export_timeline(&original, Format::Fcp7Xml, &opts).unwrap();
    assert!(
        out.report
            .entries
            .iter()
            .any(|e| e.feature == "upstream" && e.message.contains("non-uniform scale"))
    );
    assert!(out.report.has_losses());
}

#[test]
fn overlapping_native_dissolve_converts_to_adjacent_edits_and_back() {
    let original = timeline(
        json!({"name":"Dissolve fixture","output":{"width":640,"height":360,"fps":"24","duration":"5"},"tracks":[{"name":"Picture","clips":[
            {"id":"a","source":"media/first.mov","start":"0","source_in":"2","duration":"3"},
            {"id":"b","source":"media/second.mov","start":"2","source_in":"3","duration":"3","transition_in":{"kind":"dissolve","duration":"1"}}
        ]}]}),
    );
    for format in [Format::Otio, Format::Fcp7Xml] {
        let imported = roundtrip(&original, format, &options(false));
        let clips = &imported.timeline.tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].duration, t("3"));
        assert_eq!(
            (clips[1].start, clips[1].source_in, clips[1].duration),
            (t("2"), t("3"), t("3"))
        );
        assert_eq!(clips[1].dissolve(), Some(t("1")));
    }
}

#[test]
fn linked_sound_is_preserved_once_as_independent_native_audio() {
    let original = simple();
    for format in [Format::Otio, Format::Fcp7Xml] {
        let imported = roundtrip(&original, format, &options(true));
        let tl = imported.timeline;
        assert!(tl.tracks[0].clips.iter().all(|c| c.audio.mute));
        assert_eq!(tl.audio_tracks.len(), 1);
        assert_eq!(tl.audio_tracks[0].clips.len(), 2);
        for (a, b) in original.tracks[0]
            .clips
            .iter()
            .zip(&tl.audio_tracks[0].clips)
        {
            assert_eq!(
                (a.start, a.source_in, a.duration),
                (b.start, b.source_in, b.duration)
            );
            assert!(!b.audio.mute);
        }
    }
}

#[test]
fn omitted_generators_retimes_unprobed_media_and_trailing_duration_are_reported() {
    let mut original = simple();
    original.tracks[0].clips[0].speed = ferrocut_core::Animatable::Constant(Rational::from_int(2));
    let out =
        interchange::export_timeline(&original, Format::Otio, &ExportOptions::default()).unwrap();
    assert!(out.report.has_losses());
    assert!(out.report.entries.iter().any(|e| e.feature == "retime"));
    assert!(
        out.report
            .entries
            .iter()
            .any(|e| e.feature == "source_metadata")
    );
    assert!(out.report.ensure_lossless().is_err());
    original = simple();
    original.output.duration = Some(t("8"));
    let out = interchange::export_timeline(&original, Format::Otio, &options(false)).unwrap();
    assert!(
        out.report
            .entries
            .iter()
            .any(|e| e.feature == "explicit_duration")
    );
    original = simple();
    original.tracks[0].clips[0].source = PathBuf::new();
    original.tracks[0].clips[0].generator =
        Some(serde_json::from_value(json!({"type":"solid","color":["1","0","0","1"]})).unwrap());
    let out = interchange::export_timeline(&original, Format::Otio, &options(false)).unwrap();
    assert!(
        out.report
            .entries
            .iter()
            .any(|e| e.feature == "generated_layers")
    );
}

#[test]
fn malformed_oversized_unvalidated_formats_and_sequence_selection_error_cleanly() {
    for (bytes, format) in [
        (b"{broken".as_slice(), Format::Otio),
        (b"<xmeml><sequence>".as_slice(), Format::Fcp7Xml),
        (b"".as_slice(), Format::Otio),
    ] {
        assert!(interchange::import_document(bytes, format, &ImportOptions::default()).is_err());
    }
    assert!(
        interchange::import_document(
            &vec![b' '; interchange::MAX_DOCUMENT_BYTES + 1],
            Format::Otio,
            &ImportOptions::default()
        )
        .is_err()
    );
    assert!(interchange::export_timeline(&simple(), Format::Aaf, &options(false)).is_err());
    let out = interchange::export_timeline(&simple(), Format::Otio, &options(false)).unwrap();
    assert!(
        interchange::import_document(
            &out.bytes,
            Format::Otio,
            &ImportOptions {
                sequence: 1,
                ..Default::default()
            }
        )
        .is_err()
    );
    let mut unsafe_time = simple();
    unsafe_time.tracks[0].clips[0].start = t("9223372036854775807");
    assert!(interchange::export_timeline(&unsafe_time, Format::Otio, &options(false)).is_err());
}

#[test]
fn nested_timeline_resolver_roundtrip_produces_safe_sibling_documents() {
    let inner = simple();
    let mut outer = simple();
    outer.tracks[0].clips.truncate(1);
    outer.tracks[0].clips[0].source = PathBuf::from("inner.json");
    outer.output.duration = Some(t("3"));
    for format in [Format::Otio, Format::Fcp7Xml] {
        let out =
            interchange::export_timeline_with_resolver(&outer, format, &options(false), |path| {
                assert_eq!(path, Path::new("/original-project/inner.json"));
                Ok(Some(inner.clone()))
            })
            .unwrap();
        assert!(
            out.report
                .entries
                .iter()
                .all(|e| e.severity != interchange::Severity::Loss
                    || e.feature == "codec_track_name"),
            "{:?}",
            out.report.entries
        );
        let imported =
            interchange::import_document(&out.bytes, format, &ImportOptions::default()).unwrap();
        assert!(
            !imported.report.has_losses(),
            "{:?}",
            imported.report.entries
        );
        assert_eq!(imported.nested.len(), 1);
        let filename = &imported.nested[0].filename;
        assert_eq!(filename.components().count(), 1);
        assert_eq!(imported.timeline.tracks[0].clips[0].source, *filename);
        assert_eq!(imported.nested[0].timeline.tracks[0].clips.len(), 2);
    }
}

#[test]
fn sequence_and_source_markers_preserve_color_range_comments_and_origin() {
    let mut value = serde_json::to_value(simple()).unwrap();
    value["markers"] = json!([{"id":"note","time":"5/2","duration":"1/2","name":"beat","color":"cyan","comment":"Original fixture comment"}]);
    value["tracks"][0]["clips"][0]["markers"] =
        json!([{"id":"source-note","time":"13/4","name":"source beat","color":"orange"}]);
    let original = timeline(value);
    for format in [Format::Otio, Format::Fcp7Xml] {
        let imported = roundtrip(&original, format, &options(false));
        let m = &imported.timeline.markers[0];
        assert_eq!(
            (m.time, m.duration, m.color),
            (
                t("5/2"),
                t("1/2"),
                ferrocut_engine::markers::MarkerColor::Cyan
            )
        );
        assert_eq!(m.comment, "Original fixture comment");
        let m = &imported.timeline.tracks[0].clips[0].markers[0];
        assert_eq!(m.time, t("13/4"));
        assert_eq!(m.color, ferrocut_engine::markers::MarkerColor::Orange);
    }
}

#[test]
fn otio_preserves_audio_automation_bus_controls_and_additive_overlapping_lanes() {
    let mut value = serde_json::to_value(simple()).unwrap();
    value["audio"] = json!({"sample_rate":48000,"master_gain_db":"-3"});
    value["tracks"][0]["audio"] = json!({"gain_db":"-2","pan":"0.25"});
    value["tracks"][0]["clips"][1]["start"] = json!("2");
    value["tracks"][0]["clips"][1]["duration"] = json!("4");
    value["tracks"][0]["clips"][1]["transition_in"] = json!({"kind":"dissolve","duration":"1"});
    value["tracks"][0]["clips"][0]["audio"] =
        json!({"gain_db":{"keyframes":[{"t":"0","v":"-12"},{"t":"2","v":"0"}]},"pan":"-0.5"});
    let original = timeline(value);
    let imported = roundtrip(&original, Format::Otio, &options(true));
    let tl = &imported.timeline;
    assert_eq!(
        tl.audio.master_gain_db.as_constant(),
        Some(Rational::from_int(-3))
    );
    assert_eq!(tl.audio_tracks.len(), 2);
    for track in &tl.audio_tracks {
        assert_eq!(
            track.bus.gain_db.as_constant(),
            Some(Rational::from_int(-2))
        );
        assert!((track.bus.pan.eval(t("0")) - 0.25).abs() < 1e-10);
    }
    let sound = &tl.audio_tracks[0].clips[0];
    assert!((sound.audio.gain_db.eval(t("1")) + 6.0).abs() < 1e-10);
    assert!((sound.audio.pan.eval(t("1")) + 0.5).abs() < 1e-10);
    let out = interchange::export_timeline(&original, Format::Fcp7Xml, &options(true)).unwrap();
    assert!(out.report.has_losses());
    assert!(
        out.report
            .entries
            .iter()
            .any(|e| e.feature == "fcp7_track_mix")
    );
    assert!(
        out.report
            .entries
            .iter()
            .any(|e| e.feature == "fcp7_master_mix")
    );
}

#[test]
fn otio_preserves_subframe_edits_while_fcp7_reports_rounding() {
    let mut original = simple();
    original.tracks[0].clips[0].start = t("1/48000");
    original.tracks[0].clips[0].source_in = t("1/48000");
    let imported = roundtrip(&original, Format::Otio, &options(false));
    assert_eq!(imported.timeline.tracks[0].clips[0].start, t("1/48000"));
    assert_eq!(imported.timeline.tracks[0].clips[0].source_in, t("1/48000"));
    let out = interchange::export_timeline(&original, Format::Fcp7Xml, &options(false)).unwrap();
    assert!(
        out.report
            .entries
            .iter()
            .any(|e| e.feature == "fcp7_subframe")
    );
    assert!(
        out.report
            .entries
            .iter()
            .any(|e| e.feature == "codec_edit_time")
    );
}

#[test]
fn unresolved_nested_sources_report_loss_and_cycles_error_without_io() {
    let mut outer = simple();
    outer.tracks[0].clips[0].source = PathBuf::from("loop.json");
    let out = interchange::export_timeline(&outer, Format::Otio, &options(false)).unwrap();
    assert!(
        out.report
            .entries
            .iter()
            .any(|e| e.feature == "nested_timeline")
    );
    assert!(
        interchange::export_timeline_with_resolver(&outer, Format::Otio, &options(false), |_| Ok(
            Some(outer.clone())
        ))
        .is_err()
    );
    let mut opts = options(false);
    opts.media
        .get_mut(Path::new("media/first.mov"))
        .unwrap()
        .duration = t("1");
    assert!(interchange::export_timeline(&simple(), Format::Otio, &opts).is_err());
}

#[test]
fn source_reference_protocols_are_checked_before_upstream_path_resolution() {
    let out = interchange::export_timeline(&simple(), Format::Otio, &options(false)).unwrap();
    let original: Value = serde_json::from_slice(&out.bytes).unwrap();
    fn replace(value: &mut Value, url: &str) {
        match value {
            Value::Object(object) => {
                if object.contains_key("target_url") {
                    object.insert("target_url".into(), json!(url));
                }
                for value in object.values_mut() {
                    replace(value, url);
                }
            }
            Value::Array(values) => {
                for value in values {
                    replace(value, url);
                }
            }
            _ => {}
        }
    }
    for url in [
        "https://example.invalid/a.mov",
        "data:video/mp4;base64,AAAA",
        "s3://bucket/a.mov",
        "file://remote.invalid/share/a.mov",
        "file://localhost.evil/share/a.mov",
        "//server/share/a.mov",
        "file:///%00.mov",
    ] {
        let mut value = original.clone();
        replace(&mut value, url);
        let bytes = serde_json::to_vec(&value).unwrap();
        let err = interchange::import_document(
            &bytes,
            Format::Otio,
            &ImportOptions {
                base_dir: Some(PathBuf::from("/safe")),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            !err.to_string().contains("panic guard"),
            "protocol should fail in preflight: {err}"
        );
    }
    for url in [
        "file:///tmp/é🐱.mov",
        "file://localhost/tmp/é🐱.mov",
        "relative/clip.mov",
        "C:/local/clip.mov",
    ] {
        let mut value = original.clone();
        replace(&mut value, url);
        let bytes = serde_json::to_vec(&value).unwrap();
        interchange::import_document(&bytes, Format::Otio, &ImportOptions::default()).unwrap();
    }
    let xml=b"<?xml version=\"1.0\"?><xmeml version=\"5\"><sequence><media><video><track><clipitem><file><pathurl>https://example.invalid/a.mov</pathurl></file></clipitem></track></video></media></sequence></xmeml>";
    let err =
        interchange::import_document(xml, Format::Fcp7Xml, &ImportOptions::default()).unwrap_err();
    assert!(
        err.to_string()
            .contains("unsupported media reference protocol")
    );
    let mut timeline = simple();
    timeline.tracks[0].clips[0].source = PathBuf::from("https://example.invalid/a.mov");
    assert!(interchange::export_timeline(&timeline, Format::Otio, &options(false)).is_err());
}

#[test]
fn image_sequence_reference_alternative_urls_reject_custom_protocols_and_encoded_controls() {
    let out = interchange::export_timeline(&simple(), Format::Otio, &options(false)).unwrap();
    let original: Value = serde_json::from_slice(&out.bytes).unwrap();
    fn mutate(value: &mut Value, field: &str, url: &str) {
        match value {
            Value::Object(object) => {
                if object.contains_key("target_url") {
                    object.insert("OTIO_SCHEMA".into(), json!("ImageSequenceReference.1"));
                    object.remove("target_url");
                    object.insert(field.into(), json!(url));
                }
                for value in object.values_mut() {
                    mutate(value, field, url);
                }
            }
            Value::Array(values) => {
                for value in values {
                    mutate(value, field, url);
                }
            }
            _ => {}
        }
    }
    for field in ["target_url", "target_url_base"] {
        for url in [
            "custom+cloud://storage/frames/",
            "https://example.invalid/frames/",
            "file:///tmp/frame%00.png",
            "file:///tmp/frame%0A.png",
        ] {
            let mut value = original.clone();
            mutate(&mut value, field, url);
            let bytes = serde_json::to_vec(&value).unwrap();
            let err = interchange::import_document(&bytes, Format::Otio, &ImportOptions::default())
                .unwrap_err();
            assert!(!err.to_string().contains("panic guard"));
        }
    }
    let mut value = original;
    mutate(&mut value, "target_url_base", "file:///tmp/frames/");
    let bytes = serde_json::to_vec(&value).unwrap();
    let result =
        interchange::import_document(&bytes, Format::Otio, &ImportOptions::default()).unwrap();
    assert!(
        result
            .report
            .entries
            .iter()
            .any(|e| e.feature == "image_sequence" && e.severity == interchange::Severity::Loss)
    );
}

#[test]
fn broadcast_fractional_frame_rate_roundtrips_keep_exact_native_edit_times() {
    for frame_rate in ["24000/1001", "30000/1001", "60000/1001"] {
        let fps: Rational = frame_rate.parse().unwrap();
        let factor: Rational = "1001/1000".parse().unwrap();
        let mut original = simple();
        original.output.fps = fps;
        original.output.duration = original
            .output
            .duration
            .map(|d| RationalTime(d.0.checked_mul(factor).unwrap()));
        for clip in &mut original.tracks[0].clips {
            clip.start = RationalTime(clip.start.0.checked_mul(factor).unwrap());
            clip.source_in = RationalTime(clip.source_in.0.checked_mul(factor).unwrap());
            clip.duration = RationalTime(clip.duration.0.checked_mul(factor).unwrap());
        }
        let mut opts = options(false);
        for media in opts.media.values_mut() {
            media.fps = Some(fps);
        }
        for format in [Format::Otio, Format::Fcp7Xml] {
            let result = roundtrip(&original, format, &opts);
            assert_eq!(result.timeline.output.fps, fps);
            assert_eq!(result.timeline.output.duration, original.output.duration);
            for (before, after) in original.tracks[0]
                .clips
                .iter()
                .zip(&result.timeline.tracks[0].clips)
            {
                assert_eq!(
                    (before.start, before.source_in, before.duration),
                    (after.start, after.source_in, after.duration)
                );
            }
        }
    }
}

#[test]
fn imported_source_requirements_include_full_dissolve_sound_and_skip_generated_nests() {
    let mut original = simple();
    original.tracks[0].clips[1].start = t("2");
    original.tracks[0].clips[1].duration = t("4");
    original.tracks[0].clips[1].transition_in =
        Some(ferrocut_engine::timeline::Transition::Dissolve { duration: t("1") });
    original.tracks[0].clips[1].source = original.tracks[0].clips[0].source.clone();
    original.tracks[0].clips[1].source_in = t("10");
    let imported = roundtrip(&original, Format::Otio, &options(true));
    let requirements =
        interchange::imported_source_requirements(&imported.timeline, &Default::default()).unwrap();
    assert_eq!(requirements.len(), 1);
    assert_eq!(
        requirements[Path::new("/original-project/media/first.mov")],
        t("14")
    );
    let generated =
        std::collections::BTreeSet::from([PathBuf::from("/original-project/media/first.mov")]);
    assert!(
        interchange::imported_source_requirements(&imported.timeline, &generated)
            .unwrap()
            .is_empty()
    );
    original.tracks[0].clips[0].speed = ferrocut_core::Animatable::Constant(Rational::from_int(2));
    assert!(interchange::imported_source_requirements(&original, &Default::default()).is_err());
}
