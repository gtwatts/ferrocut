use std::io::Write as _;

use anyhow::Result;
use ferrocut_core::{CancelToken, Rational, RationalTime};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::tracking::{
    PointSettings, SampleStatus, StabilizationMode, StabilizationSettings, TrackFailure,
    TrackOutcome, TrackingAnalysis, TrackingCompletion, TrackingSettings, hash_source,
    stabilization, track_file, track_frames,
};

const W: u32 = 128;
const H: u32 = 96;

fn settings(frames: u32) -> TrackingSettings {
    TrackingSettings {
        start: RationalTime::ZERO,
        fps: Rational::from_int(24),
        width: W,
        height: H,
        frame_count: frames,
        points: vec![PointSettings {
            id: "sign-corner".into(),
            center: [Rational::new(97, 2), Rational::new(95, 2)],
            feature_size: [15, 15],
            search_size: [47, 47],
        }],
        min_confidence: Rational::from_int(75),
        subpixel: true,
    }
}

// Original deterministic fixture: an infinite, non-periodic luminance field.
// Shifting this field creates real image motion, without shipping sample media.
fn noise(x: i32, y: i32) -> f64 {
    let mut n =
        (x as u32).wrapping_mul(0x6c8e_9cf5) ^ (y as u32).wrapping_mul(0xa529_d147) ^ 0x41f3_29a7;
    n ^= n >> 15;
    n = n.wrapping_mul(0x85eb_ca6b);
    n ^= n >> 13;
    35.0 + f64::from(n & 0xff) * 0.72
}

fn luminance(x: i32, y: i32) -> f64 {
    // Band-limit the field so a quarter-pixel shift can be measured instead of
    // confusing interpolation aliasing with a tracking failure.
    let weights = [1.0, 2.0, 1.0];
    let mut value = 0.0;
    for (j, wy) in weights.into_iter().enumerate() {
        for (i, wx) in weights.into_iter().enumerate() {
            value += wx * wy * noise(x + i as i32 - 1, y + j as i32 - 1);
        }
    }
    value / 16.0
}

fn moving_frame(dx: f64, dy: f64) -> Vec<u8> {
    let mut rgba = vec![0; W as usize * H as usize * 4];
    for y in 0..H {
        for x in 0..W {
            let sx = f64::from(x) - dx;
            let sy = f64::from(y) - dy;
            let ix = sx.floor() as i32;
            let iy = sy.floor() as i32;
            let fx = sx - f64::from(ix);
            let fy = sy - f64::from(iy);
            let value = ((1.0 - fx) * (1.0 - fy) * luminance(ix, iy)
                + fx * (1.0 - fy) * luminance(ix + 1, iy)
                + (1.0 - fx) * fy * luminance(ix, iy + 1)
                + fx * fy * luminance(ix + 1, iy + 1))
            .round() as u8;
            let i = (y as usize * W as usize + x as usize) * 4;
            rgba[i..i + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
    rgba
}

fn frame_index(t: RationalTime, s: &TrackingSettings) -> usize {
    ((t.seconds() - s.start.seconds()) * s.fps).round() as usize
}

fn run(motion: &[[f64; 2]]) -> Result<TrackingAnalysis> {
    let s = settings(motion.len() as u32);
    track_frames(
        &s,
        &CancelToken::new(),
        |t| {
            let [x, y] = motion[frame_index(t, &s)];
            Ok(moving_frame(x, y))
        },
        |_| {},
    )
}

#[test]
fn actual_tracker_follows_integer_translation_and_reports_measured_confidence() -> Result<()> {
    let motion = [[0.0, 0.0], [3.0, 2.0], [6.0, 4.0], [4.0, 3.0]];
    let a = run(&motion)?;
    assert_eq!(a.completion, TrackingCompletion::Completed);
    assert_eq!(a.tracks[0].outcome, TrackOutcome::Tracked);
    assert_eq!(a.processed_frames, 4);
    let seed = &a.tracks[0].samples[0];
    assert_eq!(seed.status, SampleStatus::Seeded);
    assert!(seed.confidence.is_none());
    for (sample, delta) in a.tracks[0].samples.iter().zip(motion) {
        let p = sample.position.unwrap();
        assert!((p[0].to_f64() - 48.5 - delta[0]).abs() < 0.06, "{sample:?}");
        assert!((p[1].to_f64() - 47.5 - delta[1]).abs() < 0.06, "{sample:?}");
        if sample.frame > 0 {
            assert!(sample.confidence.unwrap() > Rational::from_int(99));
        }
    }
    a.validate()?;
    Ok(())
}

#[test]
fn subpixel_refinement_measures_fractional_image_motion() -> Result<()> {
    let a = run(&[[0.0, 0.0], [0.5, 0.25], [1.0, 0.5]])?;
    assert_eq!(a.completion, TrackingCompletion::Completed);
    let sample = &a.tracks[0].samples[1];
    let p = sample.position.unwrap();
    assert!((p[0].to_f64() - 49.0).abs() < 0.16, "{sample:?}");
    assert!((p[1].to_f64() - 47.75).abs() < 0.16, "{sample:?}");
    Ok(())
}

#[test]
fn occlusion_stops_point_without_reporting_previous_position_as_a_match() -> Result<()> {
    let s = settings(6);
    let a = track_frames(
        &s,
        &CancelToken::new(),
        |t| {
            if frame_index(t, &s) < 2 {
                Ok(moving_frame(2.0 * frame_index(t, &s) as f64, 0.0))
            } else {
                Ok(vec![0; W as usize * H as usize * 4])
            }
        },
        |_| {},
    )?;
    assert_eq!(a.completion, TrackingCompletion::AllPointsLost);
    assert_eq!(a.processed_frames, 3);
    assert_eq!(a.tracks[0].outcome, TrackOutcome::Lost);
    let lost = &a.tracks[0].samples[2];
    assert_eq!(lost.status, SampleStatus::Failed);
    assert_eq!(lost.failure, Some(TrackFailure::LowConfidence));
    assert!(lost.position.is_none());
    assert_eq!(lost.confidence, Some(Rational::ZERO));
    assert!(stabilization(&a, "sign-corner", &lock()).is_err());
    Ok(())
}

#[test]
fn lost_point_gets_inactive_samples_while_other_point_continues() -> Result<()> {
    let mut s = settings(4);
    s.points.push(PointSettings {
        id: "left-corner".into(),
        center: [Rational::new(31, 2), Rational::new(31, 2)],
        feature_size: [9, 9],
        search_size: [25, 25],
    });
    let a = track_frames(
        &s,
        &CancelToken::new(),
        |t| {
            let index = frame_index(t, &s);
            let mut rgba = moving_frame(index as f64, 0.0);
            if index > 0 {
                // Cover the first point's entire search region, preserve the other.
                for y in 21..76usize {
                    for x in 25..78usize {
                        let i = (y * W as usize + x) * 4;
                        rgba[i..i + 4].copy_from_slice(&[80, 80, 80, 255]);
                    }
                }
            }
            Ok(rgba)
        },
        |_| {},
    )?;
    assert_eq!(a.completion, TrackingCompletion::Completed);
    assert_eq!(a.tracks[0].outcome, TrackOutcome::Lost);
    assert_eq!(a.tracks[0].samples[2].status, SampleStatus::Inactive);
    assert!(a.tracks[0].samples[2].position.is_none());
    assert!(a.tracks[0].samples[2].confidence.is_none());
    assert_eq!(a.tracks[1].outcome, TrackOutcome::Tracked);
    a.validate()?;
    assert!(stabilization(&a, "sign-corner", &lock()).is_err());
    stabilization(&a, "left-corner", &lock())?;
    Ok(())
}

#[test]
fn blank_seed_has_explicit_no_texture_outcome_and_no_confidence() -> Result<()> {
    let a = track_frames(
        &settings(4),
        &CancelToken::new(),
        |_| Ok(vec![128; W as usize * H as usize * 4]),
        |_| {},
    )?;
    assert_eq!(a.completion, TrackingCompletion::AllPointsLost);
    assert_eq!(a.processed_frames, 1);
    assert_eq!(
        a.tracks[0].samples[0].failure,
        Some(TrackFailure::NoTexture)
    );
    assert!(a.tracks[0].samples[0].confidence.is_none());
    assert!(a.tracks[0].samples[0].position.is_none());
    Ok(())
}

#[test]
fn seed_only_does_not_claim_measured_tracking() -> Result<()> {
    let a = run(&[[0.0, 0.0]])?;
    assert_eq!(a.completion, TrackingCompletion::SeedOnly);
    assert_eq!(a.tracks[0].outcome, TrackOutcome::SeedOnly);
    assert!(stabilization(&a, "sign-corner", &lock()).is_err());
    Ok(())
}

#[test]
fn exact_ntsc_source_grid_and_serialized_results_are_repeatable() -> Result<()> {
    let mut s = settings(3);
    s.start = RationalTime::new(3, 2);
    s.fps = Rational::new(30_000, 1001);
    let make = || {
        track_frames(
            &s,
            &CancelToken::new(),
            |t| Ok(moving_frame(frame_index(t, &s) as f64, 0.0)),
            |_| {},
        )
    };
    let first = make()?;
    let second = make()?;
    assert_eq!(serde_json::to_vec(&first)?, serde_json::to_vec(&second)?);
    assert_eq!(
        first.tracks[0].samples[2].source_time,
        RationalTime::new(23_501, 15_000)
    );
    let reloaded: TrackingAnalysis = serde_json::from_slice(&serde_json::to_vec(&first)?)?;
    reloaded.validate()?;
    assert_eq!(first, reloaded);
    Ok(())
}

#[test]
fn cancellation_returns_only_complete_committed_frames_with_progress() -> Result<()> {
    let s = settings(8);
    let cancel = CancelToken::new();
    let mut events = Vec::new();
    let a = track_frames(
        &s,
        &cancel,
        |t| Ok(moving_frame(frame_index(t, &s) as f64, 0.0)),
        |p| {
            if p.completed_frames == 2 {
                cancel.cancel();
            }
            events.push(p);
        },
    )?;
    assert_eq!(a.completion, TrackingCompletion::Cancelled);
    assert_eq!(a.processed_frames, 2);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].active_points, 1);
    assert_eq!(a.tracks[0].samples.len(), 2);
    assert_eq!(a.tracks[0].outcome, TrackOutcome::Cancelled);
    assert!(stabilization(&a, "sign-corner", &lock()).is_err());
    a.validate()?;
    Ok(())
}

#[test]
fn already_cancelled_does_not_call_frame_provider() -> Result<()> {
    let cancel = CancelToken::new();
    cancel.cancel();
    let a = track_frames(
        &settings(4),
        &cancel,
        |_| panic!("provider must not run"),
        |_| {},
    )?;
    assert_eq!(a.processed_frames, 0);
    assert!(a.tracks[0].samples.is_empty());
    assert_eq!(a.completion, TrackingCompletion::Cancelled);
    a.validate()?;
    Ok(())
}

fn lock() -> StabilizationSettings {
    StabilizationSettings {
        mode: StabilizationMode::Lock,
        smoothness: Rational::from_int(50),
    }
}

#[test]
fn upstream_lock_corrections_hold_selected_point_at_middle_frame() -> Result<()> {
    let a = run(&[[0.0, 0.0], [3.0, 1.0], [1.0, -2.0], [4.0, 2.0], [2.0, 0.0]])?;
    let result = stabilization(&a, "sign-corner", &lock())?;
    assert_eq!(
        result.reference_time,
        Some(a.tracks[0].samples[2].source_time)
    );
    assert_eq!(result.keys.len(), 5);
    let reference = a.tracks[0].samples[2].position.unwrap();
    for (key, sample) in result.keys.iter().zip(&a.tracks[0].samples) {
        assert_eq!(key.source_time, sample.source_time);
        let position = sample.position.unwrap();
        for axis in 0..2 {
            assert!(
                (position[axis].to_f64() + key.offset[axis].to_f64() - reference[axis].to_f64())
                    .abs()
                    < 0.000_003
            );
        }
    }
    Ok(())
}

#[test]
fn upstream_gaussian_corrections_reduce_jitter_without_changing_time_grid() -> Result<()> {
    let motion: Vec<_> = (0..12)
        .map(|i| [i as f64 * 0.2 + if i % 2 == 0 { 2.0 } else { -2.0 }, 0.0])
        .collect();
    // Seed describes the point on frame zero, so subtract the initial displacement.
    let origin = motion[0][0];
    let motion: Vec<_> = motion.into_iter().map(|p| [p[0] - origin, p[1]]).collect();
    let a = run(&motion)?;
    let result = stabilization(
        &a,
        "sign-corner",
        &StabilizationSettings {
            mode: StabilizationMode::Smooth,
            smoothness: Rational::from_int(50),
        },
    )?;
    assert!(result.reference_time.is_none());
    let before: Vec<_> = a.tracks[0]
        .samples
        .iter()
        .map(|s| s.position.unwrap()[0].to_f64())
        .collect();
    let after: Vec<_> = before
        .iter()
        .zip(&result.keys)
        .map(|(x, k)| x + k.offset[0].to_f64())
        .collect();
    let roughness = |v: &[f64]| {
        v.windows(3)
            .map(|w| (w[0] - 2.0 * w[1] + w[2]).powi(2))
            .sum::<f64>()
    };
    assert!(roughness(&after) < roughness(&before) * 0.1);
    for (key, sample) in result.keys.iter().zip(&a.tracks[0].samples) {
        assert_eq!(key.source_time, sample.source_time);
    }
    Ok(())
}

#[test]
fn structural_limits_and_malformed_frames_fail_before_unsafe_upstream_inputs() -> Result<()> {
    let base = settings(2);
    let mut s = base.clone();
    s.width = 0;
    assert!(s.validate().is_err());
    let mut s = base.clone();
    s.fps = Rational::ZERO;
    assert!(s.validate().is_err());
    let mut s = base.clone();
    s.frame_count = 2401;
    assert!(s.validate().is_err());
    let mut s = base.clone();
    s.points[0].center[0] = Rational::ONE;
    assert!(s.validate().is_err());
    let mut s = base.clone();
    s.points[0].search_size = [192, 192];
    assert!(s.validate().is_err());
    let mut s = base.clone();
    s.points.push(s.points[0].clone());
    assert!(s.validate().is_err());
    let mut s = base.clone();
    s.min_confidence = Rational::from_int(101);
    assert!(s.validate().is_err());
    let mut s = base.clone();
    s.start = RationalTime(Rational::from_int(i64::MAX));
    assert!(s.validate().is_err());
    assert!(track_frames(&base, &CancelToken::new(), |_| Ok(vec![0; 3]), |_| {}).is_err());
    let mut json = serde_json::to_value(base)?;
    json["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<TrackingSettings>(json).is_err());
    Ok(())
}

#[test]
fn loaded_results_reject_forged_success_gaps_confidence_and_identity() -> Result<()> {
    let good = run(&[[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]])?;
    let mut bad = good.clone();
    bad.tracks[0].samples.pop();
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.tracks[0].samples[1].status = SampleStatus::Inactive;
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.tracks[0].samples[1].source_time = RationalTime::ZERO;
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.tracks[0].samples[1].confidence = Some(Rational::from_int(101));
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.tracks[0].samples[0].confidence = Some(Rational::from_int(100));
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.tracks[0].samples[1].position = Some([Rational::from_int(-1); 2]);
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.algorithm = "imaginary tracker".into();
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.tracks[0].id = "replacement".into();
    assert!(bad.validate().is_err());
    let mut bad = good;
    bad.tracks[0].samples[1].frame = 2;
    assert!(stabilization(&bad, "sign-corner", &lock()).is_err());
    Ok(())
}

#[test]
fn file_decoder_tracks_real_lossless_media_and_rejects_resize_or_end_hold() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("motion.mkv");
    let s = settings(5);
    let mut encoder = ChunkEncoder::create(
        &path,
        &EncodeSettings {
            width: W,
            height: H,
            fps: s.fps,
            gop: 1,
        },
    )?;
    for i in 0..s.frame_count {
        encoder.push_bgra(&moving_frame(i as f64 * 2.0, 0.0))?;
    }
    encoder.finish()?;
    let a = track_file(&path, &s, &CancelToken::new(), |_| {})?;
    assert_eq!(a.completion, TrackingCompletion::Completed);
    assert_eq!(a.source.as_ref().unwrap().width, W);
    assert_eq!(a.source.as_ref().unwrap().path, path);
    assert_eq!(
        a.source.as_ref().unwrap().full_file_hash,
        blake3::hash(&std::fs::read(&path)?).to_hex().to_string()
    );
    assert!((a.tracks[0].samples[4].position.unwrap()[0].to_f64() - 56.5).abs() < 0.06);
    a.validate()?;
    let mut resize = s.clone();
    resize.width += 1;
    assert!(track_file(&path, &resize, &CancelToken::new(), |_| {}).is_err());
    let mut overrun = s;
    overrun.frame_count = 7;
    assert!(track_file(&path, &overrun, &CancelToken::new(), |_| {}).is_err());
    assert!(
        track_file(
            std::path::Path::new("https://example.invalid/video.mkv"),
            &settings(2),
            &CancelToken::new(),
            |_| {}
        )
        .is_err()
    );
    let changed = track_file(&path, &settings(5), &CancelToken::new(), |progress| {
        if progress.completed_frames == 5 {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(b"changed after decoded frames").unwrap();
        }
    });
    assert!(
        changed
            .unwrap_err()
            .to_string()
            .contains("changed during analysis")
    );
    Ok(())
}

#[test]
fn full_file_hash_streams_all_bytes_and_honors_cancellation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("source.bytes");
    let bytes: Vec<u8> = (0..2_400_007usize)
        .map(|i| (i.wrapping_mul(83) ^ (i >> 7)) as u8)
        .collect();
    std::fs::write(&path, &bytes)?;
    assert_eq!(
        hash_source(&path, &CancelToken::new())?,
        blake3::hash(&bytes).to_hex().to_string()
    );
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(hash_source(&path, &cancel).is_err());
    assert!(hash_source(dir.path(), &CancelToken::new()).is_err());
    Ok(())
}
