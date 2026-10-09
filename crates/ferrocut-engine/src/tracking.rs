//! Bounded CPU point tracking backed by the retained EffectCraft algorithms.
//!
//! Samples use original source pixels and exact source time. A seed is supplied
//! by the caller, not measured, and a stopped match never becomes a position.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use effectcraft_raster::Image;
use effectcraft_track::stabilize::{
    FrameMotion, Framing, Method, StabResult, StabSettings, WarpAnalysis, corrections,
};
use effectcraft_track::{ConfidenceAction, Frame, PointSpec, Status, TrackOptions, Tracker};
use ferrocut_core::{CancelToken, Rational, RationalTime};
use serde::{Deserialize, Serialize};

/// Algorithm identity includes the retained upstream revision and adapter version.
pub const ALGORITHM: &str =
    "effectcraft-6943872cf65b3da1275f1e0f808b60b2e51d84dc/ncc-lk-translation-v1";
pub const MAX_POINTS: usize = 16;
pub const MAX_FRAMES: u32 = 2400;
pub const MAX_SIDE: u32 = 4096;
pub const MAX_PIXELS: u64 = 8_388_608;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointSettings {
    pub id: String,
    /// Source pixels; pixel (i,j) has centre (i+0.5,j+0.5).
    pub center: [Rational; 2],
    pub feature_size: [u32; 2],
    pub search_size: [u32; 2],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingSettings {
    pub start: RationalTime,
    /// Requested sampling rate. Decoding selects the nearest displayed frame.
    pub fps: Rational,
    /// Must match the source, with no coordinate-changing resize.
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
    pub points: Vec<PointSettings>,
    /// Normalized correlation percent, not a calibrated probability.
    pub min_confidence: Rational,
    pub subpixel: bool,
}

impl TrackingSettings {
    /// Structural validation only; no media or font discovery or filesystem I/O.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.width >= 16
                && self.height >= 16
                && self.width <= MAX_SIDE
                && self.height <= MAX_SIDE
                && u64::from(self.width) * u64::from(self.height) <= MAX_PIXELS,
            "tracking dimensions must be 16..={MAX_SIDE} per side, at most {MAX_PIXELS} pixels"
        );
        ensure!(
            self.frame_count > 0 && self.frame_count <= MAX_FRAMES,
            "tracking frame_count must be 1..={MAX_FRAMES}"
        );
        ensure!(
            self.fps > Rational::ZERO && self.fps <= Rational::from_int(240),
            "tracking fps must be positive and at most 240"
        );
        ensure!(
            self.start >= RationalTime::ZERO,
            "tracking start must be nonnegative source time"
        );
        // Also bounds FFmpeg's microsecond conversions and unreasonably long jobs.
        ensure!(
            self.sample_time(self.frame_count - 1)?.seconds() <= Rational::from_int(604_800),
            "tracking sample times must be within the first seven source days"
        );
        ensure!(
            (Rational::ONE..=Rational::from_int(100)).contains(&self.min_confidence),
            "min_confidence must be 1..=100 percent"
        );
        ensure!(
            !self.points.is_empty() && self.points.len() <= MAX_POINTS,
            "tracking requires 1..={MAX_POINTS} points"
        );
        let mut ids = BTreeSet::new();
        for point in &self.points {
            ensure!(
                !point.id.is_empty()
                    && point.id.len() <= 128
                    && !point.id.chars().any(char::is_control),
                "point id must contain 1..=128 bytes without control characters"
            );
            ensure!(
                ids.insert(&point.id),
                "duplicate tracking point id {:?}",
                point.id
            );
            for axis in 0..2 {
                ensure!(
                    (5..=63).contains(&point.feature_size[axis]),
                    "point {:?} feature_size must be 5..=63 pixels",
                    point.id
                );
                ensure!(
                    point.search_size[axis] >= point.feature_size[axis] + 4
                        && point.search_size[axis] <= 191,
                    "point {:?} search_size must be feature_size+4..=191 pixels",
                    point.id
                );
            }
            ensure!(
                feature_inside(point.center.map(Rational::to_f64), point, self),
                "point {:?} seed feature must fit inside the source with a one-pixel margin",
                point.id
            );
        }
        Ok(())
    }

    /// Exact source timestamp, checked before entering the native decoder.
    pub fn sample_time(&self, frame: u32) -> Result<RationalTime> {
        ensure!(
            frame < self.frame_count,
            "tracking sample index exceeds frame_count"
        );
        let offset = Rational::from_int(i64::from(frame)).checked_div(self.fps)?;
        Ok(RationalTime(self.start.seconds().checked_add(offset)?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleStatus {
    Seeded,
    Tracked,
    Failed,
    Inactive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackFailure {
    NoTexture,
    LowConfidence,
    OutOfBounds,
    NonFinite,
    AlreadyLost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackOutcome {
    Tracked,
    SeedOnly,
    Lost,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingCompletion {
    Completed,
    SeedOnly,
    Cancelled,
    AllPointsLost,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingSample {
    pub frame: u32,
    pub source_time: RationalTime,
    pub status: SampleStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[Rational; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Rational>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<TrackFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointTrack {
    pub id: String,
    pub outcome: TrackOutcome,
    pub samples: Vec<TrackingSample>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingSource {
    pub path: PathBuf,
    /// Streamed Blake3 of the complete local media file, checked before/after tracking.
    pub full_file_hash: String,
    pub width: u32,
    pub height: u32,
    pub fps: Option<Rational>,
    pub duration: RationalTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingAnalysis {
    pub version: u32,
    pub algorithm: String,
    pub settings: TrackingSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<TrackingSource>,
    pub completion: TrackingCompletion,
    pub processed_frames: u32,
    /// Blake3 of dimensions, committed sample times and RGBA pixels; not a file hash.
    pub sampled_frames_hash: String,
    pub tracks: Vec<PointTrack>,
}

impl TrackingAnalysis {
    /// Validate loaded JSON before deriving edits. This checks consistency, not
    /// authenticity: callers must guard source paths and establish provenance.
    pub fn validate(&self) -> Result<()> {
        self.settings.validate()?;
        ensure!(
            self.version == 1 && self.algorithm == ALGORITHM,
            "unsupported tracking analysis identity"
        );
        ensure!(
            self.processed_frames <= self.settings.frame_count,
            "tracking processed_frames exceeds requested frames"
        );
        ensure!(
            self.sampled_frames_hash.len() == 64
                && self
                    .sampled_frames_hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "tracking sampled_frames_hash must be a lowercase Blake3 hex digest"
        );
        ensure!(
            self.tracks.len() == self.settings.points.len(),
            "tracking point count mismatch"
        );
        let mut active_points = 0usize;
        for (track, point) in self.tracks.iter().zip(&self.settings.points) {
            ensure!(
                track.id == point.id && track.samples.len() == self.processed_frames as usize,
                "tracking point identity or committed sample count mismatch"
            );
            let mut lost = false;
            let mut tracked = false;
            for (index, measured) in track.samples.iter().enumerate() {
                ensure!(
                    measured.frame == index as u32
                        && measured.source_time == self.settings.sample_time(index as u32)?,
                    "tracking samples have a gap or changed source time"
                );
                if let Some(confidence) = measured.confidence {
                    ensure!(
                        (Rational::ZERO..=Rational::from_int(100)).contains(&confidence),
                        "tracking confidence must be 0..=100"
                    );
                }
                match measured.status {
                    SampleStatus::Seeded => {
                        ensure!(
                            index == 0
                                && !lost
                                && measured.position == Some(point.center)
                                && measured.confidence.is_none()
                                && measured.failure.is_none(),
                            "tracking seed was modified, repeated, or given invented confidence"
                        );
                    }
                    SampleStatus::Tracked => {
                        ensure!(
                            index > 0 && !lost && measured.failure.is_none(),
                            "tracked sample cannot precede seed or follow a lost point"
                        );
                        let position = measured
                            .position
                            .context("measured tracking position missing")?;
                        ensure!(
                            feature_inside(position.map(Rational::to_f64), point, &self.settings),
                            "measured tracking position outside source"
                        );
                        ensure!(
                            measured.confidence.is_some_and(|c| confidence_at_least(
                                c,
                                self.settings.min_confidence
                            )),
                            "tracked sample has missing or low confidence"
                        );
                        tracked = true;
                    }
                    SampleStatus::Failed => {
                        ensure!(
                            !lost && measured.position.is_none(),
                            "failed point has a position or was already lost"
                        );
                        let reason = measured.failure.context("failed sample has no reason")?;
                        match reason {
                            TrackFailure::NoTexture => ensure!(
                                index == 0 && measured.confidence.is_none(),
                                "no-texture seed cannot have invented confidence"
                            ),
                            TrackFailure::LowConfidence => ensure!(
                                index > 0
                                    && measured.confidence.is_some_and(|c| c.to_f64()
                                        <= self.settings.min_confidence.to_f64() + 0.000_000_5),
                                "low-confidence failure has missing or high confidence"
                            ),
                            TrackFailure::OutOfBounds => ensure!(
                                index > 0
                                    && measured.confidence.is_some_and(|c| confidence_at_least(
                                        c,
                                        self.settings.min_confidence
                                    )),
                                "out-of-bounds failure has missing or low confidence"
                            ),
                            TrackFailure::NonFinite => {
                                ensure!(index > 0, "seed cannot have a non-finite measurement")
                            }
                            TrackFailure::AlreadyLost => {
                                bail!("already_lost must be an inactive sample")
                            }
                        }
                        lost = true;
                    }
                    SampleStatus::Inactive => ensure!(
                        lost && measured.position.is_none()
                            && measured.confidence.is_none()
                            && measured.failure == Some(TrackFailure::AlreadyLost),
                        "inactive sample must follow a failure and contain no measurement"
                    ),
                }
                ensure!(
                    index > 0
                        || matches!(measured.status, SampleStatus::Seeded | SampleStatus::Failed),
                    "first tracking sample must be a seed or explicit seed failure"
                );
            }
            let expected = if lost {
                TrackOutcome::Lost
            } else if self.completion == TrackingCompletion::Cancelled {
                TrackOutcome::Cancelled
            } else if tracked {
                TrackOutcome::Tracked
            } else {
                TrackOutcome::SeedOnly
            };
            ensure!(
                track.outcome == expected,
                "tracking outcome disagrees with sample history"
            );
            if !lost {
                active_points += 1;
            }
        }
        match self.completion {
            TrackingCompletion::Completed => ensure!(
                self.processed_frames == self.settings.frame_count
                    && self.processed_frames >= 2
                    && active_points > 0,
                "completed analysis has missing frames or no active point"
            ),
            TrackingCompletion::SeedOnly => ensure!(
                self.processed_frames == 1 && self.settings.frame_count == 1 && active_points > 0,
                "seed-only analysis has invalid counts or no active seed"
            ),
            TrackingCompletion::Cancelled => ensure!(
                self.processed_frames < self.settings.frame_count && active_points > 0,
                "cancelled analysis has invalid counts or completed loss"
            ),
            TrackingCompletion::AllPointsLost => ensure!(
                self.processed_frames > 0 && active_points == 0,
                "all-points-lost analysis still has an active point"
            ),
        }
        if let Some(source) = &self.source {
            ensure!(
                source.full_file_hash.len() == 64
                    && source
                        .full_file_hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "tracking source full_file_hash must be a lowercase Blake3 hex digest"
            );
            ensure!(
                !source.path.as_os_str().is_empty()
                    && source.width == self.settings.width
                    && source.height == self.settings.height,
                "tracking source identity or dimensions mismatch"
            );
            ensure!(
                source.duration > self.settings.sample_time(self.settings.frame_count - 1)?,
                "tracking source extent does not cover requested samples"
            );
            ensure!(
                source.fps.is_none_or(|fps| fps > Rational::ZERO),
                "invalid tracking source fps"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingProgress {
    pub completed_frames: u32,
    pub total_frames: u32,
    pub source_time: RationalTime,
    pub active_points: usize,
}

fn feature_inside(center: [f64; 2], point: &PointSettings, settings: &TrackingSettings) -> bool {
    [settings.width, settings.height]
        .into_iter()
        .enumerate()
        .all(|(axis, extent)| {
            // Matches upstream's rounded half-width patch, plus bilinear/LK margin.
            let radius = (f64::from(point.feature_size[axis]) / 2.0).round().max(2.0);
            center[axis].is_finite()
                && center[axis] - radius >= 1.5
                && center[axis] + radius <= f64::from(extent) - 1.5
        })
}

fn has_texture(rgba: &[u8], point: &PointSettings, width: u32) -> bool {
    let c = point.center.map(Rational::to_f64);
    let r = point
        .feature_size
        .map(|v| (f64::from(v) / 2.0).round() as i32);
    let mut sum = 0.0;
    let mut sum2 = 0.0;
    let mut count = 0u32;
    for y in -r[1]..=r[1] {
        for x in -r[0]..=r[0] {
            let xx = (c[0] - 0.5).floor() as i32 + x;
            let yy = (c[1] - 0.5).floor() as i32 + y;
            let i = (yy as usize * width as usize + xx as usize) * 4;
            let a = f64::from(rgba[i + 3]) / 255.0;
            let luma = (0.2126 * f64::from(rgba[i])
                + 0.7152 * f64::from(rgba[i + 1])
                + 0.0722 * f64::from(rgba[i + 2]))
                / 255.0
                * a;
            sum += luma;
            sum2 += luma * luma;
            count += 1;
        }
    }
    sum2 / f64::from(count) - (sum / f64::from(count)).powi(2) > 1e-6
}

/// Retained floating point spatial estimates are rounded to 1/1,000,000 pixel.
/// Timestamp arithmetic never passes through this conversion.
fn spatial_rational(value: f64) -> Result<Rational> {
    ensure!(
        value.is_finite() && value.abs() < 1_000_000_000.0,
        "invalid tracking spatial value"
    );
    Rational::try_new((value * 1_000_000.0).round() as i128, 1_000_000).map_err(Into::into)
}

fn confidence_at_least(measured: Rational, threshold: Rational) -> bool {
    // Upstream makes its stop decision before six-decimal serialization.
    measured.to_f64() + 0.000_000_5 >= threshold.to_f64()
}

fn sample(
    frame: u32,
    source_time: RationalTime,
    status: SampleStatus,
    position: Option<[Rational; 2]>,
    confidence: Option<Rational>,
    failure: Option<TrackFailure>,
) -> TrackingSample {
    TrackingSample {
        frame,
        source_time,
        status,
        position,
        confidence,
        failure,
    }
}

/// Track callback-provided tightly packed, straight-alpha RGBA8 source frames.
///
/// The provider owns source extent/identity validation. Errors abort the call;
/// cancellation instead returns a partial analysis of fully committed frames.
/// Cancellation is observed before/after decoding and between bounded point calls.
pub fn track_frames<F, P>(
    settings: &TrackingSettings,
    cancel: &CancelToken,
    mut frame_at: F,
    mut progress: P,
) -> Result<TrackingAnalysis>
where
    F: FnMut(RationalTime) -> Result<Vec<u8>>,
    P: FnMut(TrackingProgress),
{
    settings.validate()?;
    let mut analysis = TrackingAnalysis {
        version: 1,
        algorithm: ALGORITHM.to_owned(),
        settings: settings.clone(),
        source: None,
        completion: TrackingCompletion::Cancelled,
        processed_frames: 0,
        sampled_frames_hash: String::new(),
        tracks: settings
            .points
            .iter()
            .map(|p| PointTrack {
                id: p.id.clone(),
                outcome: TrackOutcome::Cancelled,
                samples: Vec::new(),
            })
            .collect(),
    };
    let mut hash = blake3::Hasher::new();
    hash.update(b"ferrocut-tracking-rgba-v1\0");
    hash.update(&settings.width.to_le_bytes());
    hash.update(&settings.height.to_le_bytes());
    let options = TrackOptions {
        subpixel: settings.subpixel,
        threshold: settings.min_confidence.to_f64(),
        action: ConfidenceAction::Stop,
        track_shape: false,
        adapt_every_frame: false,
        ..TrackOptions::default()
    };
    let mut trackers: Vec<Option<Tracker>> = (0..settings.points.len()).map(|_| None).collect();
    let expected_len = settings.width as usize * settings.height as usize * 4;
    for frame in 0..settings.frame_count {
        if cancel.is_cancelled() {
            break;
        }
        let source_time = settings.sample_time(frame)?;
        let rgba = frame_at(source_time)
            .with_context(|| format!("tracking frame {frame} at {source_time}"))?;
        ensure!(
            rgba.len() == expected_len,
            "tracking RGBA frame size mismatch: expected {expected_len}, got {}",
            rgba.len()
        );
        if cancel.is_cancelled() {
            break;
        }
        let image = Image::from_rgba8(settings.width, settings.height, &rgba);
        let input = Frame::new(&image);
        let mut samples = Vec::with_capacity(settings.points.len());
        for (index, point) in settings.points.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            if frame == 0 {
                if !has_texture(&rgba, point, settings.width) {
                    samples.push(sample(
                        frame,
                        source_time,
                        SampleStatus::Failed,
                        None,
                        None,
                        Some(TrackFailure::NoTexture),
                    ));
                    continue;
                }
                let spec = PointSpec {
                    center: point.center.map(Rational::to_f64),
                    feature_size: point.feature_size.map(f64::from),
                    search_offset: [0.0; 2],
                    search_size: point.search_size.map(f64::from),
                };
                trackers[index] = Some(Tracker::new(options.clone(), &[spec], &input));
                samples.push(sample(
                    frame,
                    source_time,
                    SampleStatus::Seeded,
                    Some(point.center),
                    None,
                    None,
                ));
                continue;
            }
            let Some(tracker) = &mut trackers[index] else {
                samples.push(sample(
                    frame,
                    source_time,
                    SampleStatus::Inactive,
                    None,
                    None,
                    Some(TrackFailure::AlreadyLost),
                ));
                continue;
            };
            let measured = tracker
                .step(&input)
                .into_iter()
                .next()
                .context("upstream tracker returned no point")?;
            let confidence = spatial_rational(measured.confidence).ok();
            let failure = if !measured.confidence.is_finite()
                || measured.center.iter().any(|v| !v.is_finite())
            {
                Some(TrackFailure::NonFinite)
            } else if measured.status != Status::Tracked
                || measured.confidence < settings.min_confidence.to_f64()
            {
                Some(TrackFailure::LowConfidence)
            } else if !feature_inside(measured.center, point, settings) {
                Some(TrackFailure::OutOfBounds)
            } else {
                None
            };
            if let Some(reason) = failure {
                trackers[index] = None;
                samples.push(sample(
                    frame,
                    source_time,
                    SampleStatus::Failed,
                    None,
                    confidence,
                    Some(reason),
                ));
            } else {
                let position = [
                    spatial_rational(measured.center[0])?,
                    spatial_rational(measured.center[1])?,
                ];
                samples.push(sample(
                    frame,
                    source_time,
                    SampleStatus::Tracked,
                    Some(position),
                    confidence,
                    None,
                ));
            }
        }
        if samples.len() != settings.points.len() || cancel.is_cancelled() {
            break;
        }
        // Commit only a complete frame, never a partially processed point set.
        hash.update(&source_time.hash_bytes());
        hash.update(&rgba);
        for (track, measured) in analysis.tracks.iter_mut().zip(samples) {
            track.samples.push(measured);
        }
        analysis.processed_frames = frame + 1;
        let active_points = trackers.iter().filter(|v| v.is_some()).count();
        progress(TrackingProgress {
            completed_frames: frame + 1,
            total_frames: settings.frame_count,
            source_time,
            active_points,
        });
        if active_points == 0 {
            analysis.completion = TrackingCompletion::AllPointsLost;
            break;
        }
        if frame + 1 == settings.frame_count {
            analysis.completion = if frame == 0 {
                TrackingCompletion::SeedOnly
            } else {
                TrackingCompletion::Completed
            };
        }
    }
    for track in &mut analysis.tracks {
        track.outcome = if track
            .samples
            .iter()
            .any(|s| s.status == SampleStatus::Failed)
        {
            TrackOutcome::Lost
        } else if analysis.completion == TrackingCompletion::Cancelled {
            TrackOutcome::Cancelled
        } else if track
            .samples
            .iter()
            .any(|s| s.status == SampleStatus::Tracked)
        {
            TrackOutcome::Tracked
        } else {
            TrackOutcome::SeedOnly
        };
    }
    analysis.sampled_frames_hash = hash.finalize().to_hex().to_string();
    analysis.validate()?;
    Ok(analysis)
}

/// Decode a local regular file with Ferrocut's existing FFmpeg CPU decoder.
/// Callers still enforce their sandbox before invoking this function.
pub fn track_file<P>(
    path: &Path,
    settings: &TrackingSettings,
    cancel: &CancelToken,
    progress: P,
) -> Result<TrackingAnalysis>
where
    P: FnMut(TrackingProgress),
{
    settings.validate()?;
    let path_text = path.to_str().context("tracking file path must be UTF-8")?;
    ensure!(
        !path_text.chars().any(char::is_control),
        "tracking file path contains control characters"
    );
    // Reject FFmpeg protocol syntax before any media opens. Colons in ordinary
    // absolute Unix filenames are safe: only an initial URI scheme is rejected.
    if let Some((scheme, _)) = path_text.split_once(':')
        && !scheme.is_empty()
        && scheme
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        bail!("tracking requires a local filesystem path, not a URI");
    }
    ensure!(
        std::fs::metadata(path)?.is_file(),
        "tracking source must be a regular file"
    );
    let full_file_hash = hash_source(path, cancel)?;
    let info = crate::media::probe(path)?;
    ensure!(info.has_video, "tracking source has no video stream");
    ensure!(
        info.width == Some(settings.width) && info.height == Some(settings.height),
        "tracking dimensions must match original source dimensions {:?}x{:?}; resizing is not implicit",
        info.width,
        info.height
    );
    let duration = info
        .duration
        .context("tracking source duration is unknown; cannot guard decoder end hold")?;
    ensure!(
        settings.sample_time(settings.frame_count - 1)? < duration,
        "tracking samples reach or exceed source duration {duration}"
    );
    let mut decoder = crate::media::decode::Decoder::open(path, settings.width, settings.height)?;
    let mut analysis = track_frames(
        settings,
        cancel,
        |time| Ok(decoder.frame_at(time)?.to_vec()),
        progress,
    )?;
    ensure!(
        hash_source(path, cancel)? == full_file_hash,
        "tracking source changed during analysis"
    );
    analysis.source = Some(TrackingSource {
        path: path.to_path_buf(),
        full_file_hash,
        width: settings.width,
        height: settings.height,
        fps: info.fps,
        duration,
    });
    analysis.validate()?;
    Ok(analysis)
}

/// Stream a local regular file in 1 MiB chunks, observing cooperative cancellation.
/// Sandboxing is the caller's responsibility; this does not invoke FFmpeg protocols.
pub fn hash_source(path: &Path, cancel: &CancelToken) -> Result<String> {
    ensure!(
        !cancel.is_cancelled(),
        "tracking cancelled during source hashing"
    );
    ensure!(
        std::fs::metadata(path)?.is_file(),
        "tracking source must be a regular file"
    );
    let mut file = std::fs::File::open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "tracking source must be a regular file"
    );
    let mut hasher = blake3::Hasher::new();
    let mut chunk = vec![0u8; 1024 * 1024];
    loop {
        ensure!(
            !cancel.is_cancelled(),
            "tracking cancelled during source hashing"
        );
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
    }
    ensure!(
        !cancel.is_cancelled(),
        "tracking cancelled during source hashing"
    );
    Ok(hasher.finalize().to_hex().to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StabilizationMode {
    Smooth,
    Lock,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StabilizationSettings {
    pub mode: StabilizationMode,
    pub smoothness: Rational,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationKey {
    pub source_time: RationalTime,
    /// Add to source pixels before the clip's output-space transform.
    pub offset: [Rational; 2],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StabilizationResult {
    pub algorithm: String,
    pub point_id: String,
    pub settings: StabilizationSettings,
    pub source_width: u32,
    pub source_height: u32,
    /// Lock mode maps every frame to the middle sampled frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_time: Option<RationalTime>,
    pub keys: Vec<TranslationKey>,
}

/// Translation correction from one complete, contiguous measured point track.
/// Does not crop, synthesize borders, estimate camera motion, or modify a timeline.
pub fn stabilization(
    analysis: &TrackingAnalysis,
    point_id: &str,
    settings: &StabilizationSettings,
) -> Result<StabilizationResult> {
    analysis.validate()?;
    ensure!(
        analysis.version == 1 && analysis.algorithm == ALGORITHM,
        "unsupported tracking analysis identity"
    );
    ensure!(
        analysis.completion == TrackingCompletion::Completed
            && analysis.processed_frames == analysis.settings.frame_count
            && analysis.processed_frames >= 2,
        "stabilization requires a completed analysis with at least two frames"
    );
    ensure!(
        (Rational::ZERO..=Rational::from_int(100)).contains(&settings.smoothness),
        "stabilization smoothness must be 0..=100"
    );
    let track = analysis
        .tracks
        .iter()
        .find(|t| t.id == point_id)
        .context("tracking point not found")?;
    ensure!(
        track.outcome == TrackOutcome::Tracked
            && track.samples.len() == analysis.processed_frames as usize,
        "stabilization point has lost, incomplete or seed-only samples"
    );
    let mut frames = Vec::with_capacity(track.samples.len());
    let mut previous = None;
    for measured in &track.samples {
        let position = measured.position.context("tracking position missing")?;
        let now = position.map(Rational::to_f64);
        let delta = previous.map_or([0.0; 2], |p: [f64; 2]| [now[0] - p[0], now[1] - p[1]]);
        previous = Some(now);
        frames.push(FrameMotion {
            t: delta,
            ..FrameMotion::default()
        });
    }
    let upstream = WarpAnalysis {
        version: 1,
        start: 0.0,
        frame_duration: Rational::ONE.checked_div(analysis.settings.fps)?.to_f64(),
        size: [
            f64::from(analysis.settings.width),
            f64::from(analysis.settings.height),
        ],
        frames,
        ..WarpAnalysis::default()
    };
    let opts = StabSettings {
        result: match settings.mode {
            StabilizationMode::Smooth => StabResult::SmoothMotion,
            StabilizationMode::Lock => StabResult::NoMotion,
        },
        smoothness: settings.smoothness.to_f64(),
        method: Method::Position,
        framing: Framing::StabilizeOnly,
        fps: analysis.settings.fps.to_f64(),
        ..StabSettings::default()
    };
    let correction = corrections(&upstream, &opts);
    ensure!(
        correction.len() == track.samples.len(),
        "stabilization correction count mismatch"
    );
    let keys = correction
        .iter()
        .zip(&track.samples)
        .map(|(h, measured)| {
            Ok(TranslationKey {
                source_time: measured.source_time,
                offset: [spatial_rational(h.0[0][2])?, spatial_rational(h.0[1][2])?],
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(StabilizationResult {
        algorithm: format!("{ALGORITHM}/gaussian-translation-v1"),
        point_id: point_id.to_owned(),
        settings: settings.clone(),
        source_width: analysis.settings.width,
        source_height: analysis.settings.height,
        reference_time: (settings.mode == StabilizationMode::Lock)
            .then(|| track.samples[track.samples.len() / 2].source_time),
        keys,
    })
}
