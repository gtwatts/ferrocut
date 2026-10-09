//! Media index: a cached, content-hashed transcript (word-level times) and
//! shot list for one media file (`ferrocut index <media>`, MCP `index_media`,
//! `transcript_search`, `shots_list`).
//!
//! The index is JSON at `<media dir>/.ferrocut-index/<file name>.<key>.json`.
//! The key hashes [`INDEX_VERSION`], the media bytes (blake3), and for each
//! part what produces it: the whisper model's blake3, language and decoding
//! flags; the shot detector's id. Change any of them (or the file) and the
//! index is rebuilt; otherwise it is read back without touching audio or GPU.
//!
//! All times are source times of the file (the origin clips' `source_in`
//! uses), as exact rationals: whisper's millisecond token times map to
//! `n/1000`.
//!
//! Transcripts come from whisper.cpp ([`whisper`]); shots from SeePlus's
//! detector through [`shots`] (a hook until `ferrocut_perceive::detect_shots`
//! lands: the index then records `shots.status = "unavailable"`).

pub mod search;
pub mod shots;
pub mod whisper;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, bail};
use ferrocut_core::{RationalTime, ShotBoundary};
use serde::{Deserialize, Serialize};

pub use search::{Hit, search};
pub use whisper::WhisperConfig;

/// Bump when the index format or how it is built changes.
pub const INDEX_VERSION: &str = "ferrocut.index/1";
/// Sample rate whisper wants.
pub const WHISPER_RATE: u32 = 16_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: RationalTime,
    pub end: RationalTime,
    /// Lowest token probability in the word (0..1).
    pub p: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: RationalTime,
    pub end: RationalTime,
    pub text: String,
    pub words: Vec<Word>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub engine: String,
    /// Model file name and blake3.
    pub model: String,
    pub model_blake3: String,
    pub language: String,
    /// "gpu" or "cpu".
    pub device: String,
    pub segments: Vec<Segment>,
}

impl Transcript {
    pub fn words(&self) -> usize {
        self.segments.iter().map(|s| s.words.len()).sum()
    }
}

/// One part of the index: built, skipped on request, or not available
/// (no audio, no detector, transcription failed) with the reason.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Part<T> {
    Done(T),
    Skipped,
    Unavailable { reason: String },
}

impl<T> Part<T> {
    pub fn done(&self) -> Option<&T> {
        match self {
            Part::Done(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shots {
    pub detector: String,
    pub boundaries: Vec<ShotBoundary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IndexedMedia {
    /// As given (not canonicalized).
    pub path: String,
    pub blake3: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<RationalTime>,
    pub has_audio: bool,
    pub has_video: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaIndex {
    pub schema_version: String,
    pub key: String,
    pub media: IndexedMedia,
    pub transcript: Part<Transcript>,
    pub shots: Part<Shots>,
}

#[derive(Clone, Debug, Default)]
pub struct IndexOptions {
    pub transcribe: bool,
    pub shots: bool,
    /// Rebuild even if a cached index exists.
    pub force: bool,
    /// Only read a cached index; never build (error if missing).
    pub cached_only: bool,
    pub whisper: WhisperConfig,
}

impl IndexOptions {
    pub fn all() -> Self {
        IndexOptions {
            transcribe: true,
            shots: true,
            ..Default::default()
        }
    }
}

/// What [`index_media`] did.
#[derive(Clone, Debug, Serialize)]
pub struct IndexInfo {
    pub index_path: PathBuf,
    pub cached: bool,
    pub elapsed_ms: u128,
}

pub fn blake3_file(path: &Path) -> anyhow::Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex().to_string())
}

/// blake3 of a model file, memoized per (path, size, mtime) for the process
/// (models are hundreds of MB; servers index many files).
fn model_hash(path: &Path) -> anyhow::Result<String> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    type Key = (PathBuf, u64, std::time::SystemTime);
    static MEMO: Mutex<Option<HashMap<Key, String>>> = Mutex::new(None);
    let md = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let key = (path.to_path_buf(), md.len(), md.modified()?);
    if let Some(h) = MEMO.lock().unwrap().get_or_insert_default().get(&key) {
        return Ok(h.clone());
    }
    let h = blake3_file(path)?;
    MEMO.lock()
        .unwrap()
        .get_or_insert_default()
        .insert(key, h.clone());
    Ok(h)
}

/// `[start - pad, end + pad]` widened to whole frames at `fps` (when known)
/// and clamped to `[0, duration]`: an in/out pair to cut a hit with handles.
pub fn padded_range(
    start: RationalTime,
    end: RationalTime,
    pad: RationalTime,
    fps: Option<ferrocut_core::FrameRate>,
    duration: Option<RationalTime>,
) -> (RationalTime, RationalTime) {
    let (mut a, mut b) = (start - pad, end + pad);
    if let Some(r) = fps.filter(|r| *r > ferrocut_core::Rational::ZERO) {
        a = RationalTime::from_frames(a.frame_floor(r), r);
        b = RationalTime::from_frames(b.frame_ceil(r), r);
    }
    a = a.max(RationalTime::ZERO);
    if let Some(d) = duration {
        b = b.min(d);
    }
    (a, b.max(a))
}

/// Where the index of `media` with `key` lives.
pub fn index_path(media: &Path, key: &str) -> PathBuf {
    let dir = media.parent().unwrap_or(Path::new("."));
    let name = media
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media".into());
    dir.join(".ferrocut-index")
        .join(format!("{name}.{}.json", &key[..16]))
}

/// What each requested part would be built with (before doing the work).
struct Plan {
    key: String,
    whisper: Option<(PathBuf, PathBuf, String)>, // cli, model, model blake3
    whisper_err: Option<String>,
    detector: Option<String>,
}

fn plan(media_hash: &str, opts: &IndexOptions) -> Plan {
    let mut k = blake3::Hasher::new();
    k.update(INDEX_VERSION.as_bytes());
    k.update(media_hash.as_bytes());
    let (mut whisper, mut whisper_err) = (None, None);
    if opts.transcribe {
        let found = whisper::find_cli(&opts.whisper).and_then(|cli| {
            let model = whisper::find_model(&opts.whisper)?;
            let mh = model_hash(&model)?;
            Ok((cli, model, mh))
        });
        match found {
            Ok(w) => {
                k.update(b"\0transcript\0whisper.cpp\0");
                k.update(w.2.as_bytes());
                k.update(opts.whisper.language().as_bytes());
                for f in whisper::FLAGS {
                    k.update(f.as_bytes());
                }
                whisper = Some(w);
            }
            Err(e) => {
                k.update(b"\0transcript-unavailable");
                whisper_err = Some(format!("{e:#}"));
            }
        }
    }
    let detector = opts.shots.then(shots::detector_id).flatten();
    if opts.shots {
        k.update(b"\0shots\0");
        k.update(detector.as_deref().unwrap_or("none").as_bytes());
    }
    Plan {
        key: k.finalize().to_hex().to_string(),
        whisper,
        whisper_err,
        detector,
    }
}

/// Index `media` (or read its cached index).
pub fn index_media(media: &Path, opts: &IndexOptions) -> anyhow::Result<(MediaIndex, IndexInfo)> {
    let t0 = Instant::now();
    let media_hash = blake3_file(media)?;
    let p = plan(&media_hash, opts);
    let path = index_path(media, &p.key);
    let cached = (!opts.force)
        .then(|| std::fs::read_to_string(&path).ok())
        .flatten()
        .and_then(|text| serde_json::from_str::<MediaIndex>(&text).ok())
        .filter(|ix| ix.key == p.key);
    // Shots of a cached index rebuilt only to retry its transcript.
    let mut keep_shots = None;
    if let Some(mut ix) = cached {
        let unavailable = ix.media.has_audio && matches!(ix.transcript, Part::Unavailable { .. });
        if unavailable && p.whisper.is_some() && !opts.cached_only {
            // Written before transcription errors stopped being cached: an
            // unavailable transcript under a key whose whisper and model
            // exist. Retry it, keeping the shots.
            keep_shots = Some(ix.shots);
        } else {
            // A missing whisper or model: report why as of now (the CLI may
            // have been fixed but not the model), not a saved reason.
            if unavailable && let Some(err) = &p.whisper_err {
                ix.transcript = Part::Unavailable {
                    reason: err.clone(),
                };
            }
            return Ok((
                ix,
                IndexInfo {
                    index_path: path,
                    cached: true,
                    elapsed_ms: t0.elapsed().as_millis(),
                },
            ));
        }
    }
    if opts.cached_only {
        bail!(
            "{} has no index yet: run index_media (ferrocut index) first",
            media.display()
        );
    }
    let info = crate::media::probe(media)?;
    let mut transcribe_failed = false;
    let transcript = if !opts.transcribe {
        Part::Skipped
    } else if !info.has_audio {
        Part::Unavailable {
            reason: "no audio stream".into(),
        }
    } else if let Some((cli, model, mh)) = &p.whisper {
        match transcribe(media, cli, model, mh, &p.key, &opts.whisper) {
            Ok(t) => Part::Done(t),
            Err(e) => {
                transcribe_failed = true;
                Part::Unavailable {
                    reason: format!("{e:#}"),
                }
            }
        }
    } else {
        Part::Unavailable {
            reason: p.whisper_err.clone().unwrap_or_default(),
        }
    };
    let shots = if let Some(s) = keep_shots {
        s
    } else if !opts.shots {
        Part::Skipped
    } else if !info.has_video {
        Part::Unavailable {
            reason: "no video stream".into(),
        }
    } else {
        match shots::detect_shots(media, None, &shots::ShotOptions::default()) {
            Ok(b) => Part::Done(Shots {
                detector: p.detector.clone().unwrap_or_default(),
                boundaries: b,
            }),
            Err(e) => Part::Unavailable {
                reason: e.to_string(),
            },
        }
    };
    let ix = MediaIndex {
        schema_version: INDEX_VERSION.into(),
        key: p.key.clone(),
        media: IndexedMedia {
            path: media.display().to_string(),
            blake3: media_hash,
            duration: info.duration,
            has_audio: info.has_audio,
            has_video: info.has_video,
        },
        transcript,
        shots,
    };
    // A failed transcription (whisper-cli exit, missing or unparsable
    // output, audio decode) is not cached: it may be transient, and the key
    // (model, flags, bytes) would otherwise pin it. A missing whisper/model
    // is cached under its own key, but its reason is refreshed on read; a
    // transcript, including an empty one (no speech), is cached.
    if !transcribe_failed {
        let dir = path.parent().expect("index dir");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(&ix)?)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok((
        ix,
        IndexInfo {
            index_path: path,
            cached: false,
            elapsed_ms: t0.elapsed().as_millis(),
        },
    ))
}

fn transcribe(
    media: &Path,
    cli: &Path,
    model: &Path,
    model_hash: &str,
    key: &str,
    cfg: &WhisperConfig,
) -> anyhow::Result<Transcript> {
    let audio =
        crate::media::audio::decode_audio(media, WHISPER_RATE)?.context("no audio stream")?;
    let planes = &audio.audio.planes;
    let n = audio.audio.len();
    let k = planes.len().max(1) as f32;
    let mono: Vec<f32> = (0..n)
        .map(|i| planes.iter().map(|p| p[i]).sum::<f32>() / k)
        .collect();
    let base = std::env::temp_dir().join(format!(
        "ferrocut-index-{}-{}",
        std::process::id(),
        &key[..16]
    ));
    let wav = base.with_extension("wav");
    whisper::write_wav(&wav, &mono, WHISPER_RATE)?;
    let r = whisper::transcribe(cli, model, &wav, &base, cfg);
    let _ = std::fs::remove_file(&wav);
    let _ = std::fs::remove_file(base.with_extension("json"));
    let r = r?;
    Ok(Transcript {
        engine: "whisper.cpp".into(),
        model: model
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        model_blake3: model_hash.into(),
        language: cfg.language(),
        device: r.device.into(),
        segments: r.segments,
    })
}
