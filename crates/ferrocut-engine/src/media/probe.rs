//! Media probe for agents and edit ops: duration, frame rate, size and the
//! audio/video streams of a file, with exact rational times.
//!
//! Durations follow [`super::media_duration`] (source time 0 = the video
//! stream's first timestamp, like the decoders), so `duration` is the longest
//! `source_in + duration` a clip on this file can use.

use std::path::Path;

use anyhow::Context as _;
use ferrocut_core::{Rational, RationalTime};
use ffmpeg_next::media::Type;
use serde::Serialize;

use super::{init, to_core};

/// One stream of a media file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StreamInfo {
    pub index: usize,
    /// `video`, `audio`, `subtitle`, `data` or `other`.
    pub kind: &'static str,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<RationalTime>,
    /// Video: pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Video: average frame rate (exact rational, e.g. `"24"`, `"30000/1001"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<Rational>,
    /// Video: frame count when the container stores it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<i64>,
    /// Audio: Hz.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    /// Picked by FFmpeg as the best stream of its kind (what the engine uses).
    pub default: bool,
}

/// What [`probe`] reports about a file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MediaInfo {
    pub path: std::path::PathBuf,
    pub container: String,
    /// Usable source length (see the module docs); `None` if unknown.
    pub duration: Option<RationalTime>,
    pub has_video: bool,
    pub has_audio: bool,
    /// The engine's video stream: size and frame rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<Rational>,
    /// The engine's audio stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    pub streams: Vec<StreamInfo>,
}

fn kind(t: Type) -> &'static str {
    match t {
        Type::Video => "video",
        Type::Audio => "audio",
        Type::Subtitle => "subtitle",
        Type::Data => "data",
        _ => "other",
    }
}

/// Probe `path` (opens the container and reads stream headers; no decoding).
pub fn probe(path: &Path) -> anyhow::Result<MediaInfo> {
    init();
    let ictx =
        ffmpeg_next::format::input(path).with_context(|| format!("opening {}", path.display()))?;
    let nopts = ffmpeg_next::ffi::AV_NOPTS_VALUE;
    let container = ictx.format().name().to_string();
    let best_v = ictx.streams().best(Type::Video).map(|s| s.index());
    let best_a = ictx.streams().best(Type::Audio).map(|s| s.index());
    let mut streams = Vec::new();
    for s in ictx.streams() {
        let par = s.parameters();
        let medium = par.medium();
        // SAFETY: `par` borrows the stream's AVCodecParameters, valid while
        // `ictx` lives; we only read plain fields.
        let raw = unsafe { &*par.as_ptr() };
        let duration = (s.duration() != nopts && s.duration() > 0)
            .then(|| RationalTime::from_pts(s.duration(), to_core(s.time_base())));
        let mut si = StreamInfo {
            index: s.index(),
            kind: kind(medium),
            codec: par.id().name().to_string(),
            duration,
            width: None,
            height: None,
            fps: None,
            frames: None,
            sample_rate: None,
            channels: None,
            default: Some(s.index()) == best_v || Some(s.index()) == best_a,
        };
        match medium {
            Type::Video => {
                si.width = Some(raw.width.max(0) as u32);
                si.height = Some(raw.height.max(0) as u32);
                si.fps = super::stream_rate(&s);
                si.frames = (s.frames() > 0).then(|| s.frames());
            }
            Type::Audio => {
                si.sample_rate = Some(raw.sample_rate.max(0) as u32);
                si.channels = Some(raw.ch_layout.nb_channels.max(0) as u32);
            }
            _ => {}
        }
        streams.push(si);
    }
    let v = best_v
        .and_then(|i| streams.iter().find(|s| s.index == i))
        .cloned();
    let a = best_a
        .and_then(|i| streams.iter().find(|s| s.index == i))
        .cloned();
    drop(ictx);
    Ok(MediaInfo {
        path: path.to_path_buf(),
        container,
        duration: super::media_duration(path)?,
        has_video: v.is_some(),
        has_audio: a.is_some(),
        width: v.as_ref().and_then(|s| s.width),
        height: v.as_ref().and_then(|s| s.height),
        fps: v.as_ref().and_then(|s| s.fps),
        sample_rate: a.as_ref().and_then(|s| s.sample_rate),
        channels: a.as_ref().and_then(|s| s.channels),
        streams,
    })
}
