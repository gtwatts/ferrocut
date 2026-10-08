//! Timeline audio: resolve the timeline into a `ferrocut_audio::Program`,
//! decode sources, run the analysis pass, and render per-chunk sample ranges.
//!
//! Sample positions come from exact rational times: sample `n` of time `t`
//! is `round(t · rate)` with halves away from zero ([`ferrocut_audio::sample_at`]),
//! the same rule the video side uses for frames and pts. A chunk of video
//! frames `[f0, f1)` carries audio samples `[S(f0/fps), S(f1/fps))`, and each
//! video frame `i` gets the packet `[S(i/fps), S((i+1)/fps))`, so non-integer
//! samples per frame (48000 / 23.976 = 2002.002) partition exactly with no
//! drift, and chunk boundaries line up with GOP boundaries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, bail};
use ferrocut_audio::{
    AnalysisReport, ClipProg, Control, Duck, Fade, LoudnessTarget, Measurement, Program,
    SourceAudio, Stereo, TrackProg, analyze, render_range, sample_at,
};
use ferrocut_core::{Rational, RationalTime};
use serde::Serialize;

use crate::media::audio::decode_audio;
use crate::timeline::{BusSpec, ClipAudio, Timeline};

/// Output channel count of the master (stereo).
pub const CHANNELS: usize = 2;

#[derive(Clone, Debug, Serialize)]
pub struct SourceInfo {
    pub path: PathBuf,
    pub codec: String,
    pub source_rate: u32,
    pub source_channels: u16,
    pub samples: usize,
}

/// A resolved, decoded and analyzed timeline mix.
pub struct AudioPlan {
    pub program: Program,
    pub sources: Vec<SourceAudio>,
    pub info: Vec<SourceInfo>,
    pub control: Control,
    pub analysis: AnalysisReport,
    pub decode_ms: u128,
    pub analysis_ms: u128,
}

#[derive(Clone, Debug, Serialize)]
pub struct AudioReport {
    pub codec: &'static str,
    pub sample_rate: u32,
    pub channels: usize,
    pub samples: i64,
    pub sources: Vec<SourceInfo>,
    pub analysis: AnalysisReport,
    /// Measured on the rendered (chunked) master.
    pub output: Option<Measurement>,
    /// blake3 of the master's interleaved f32le samples (= the PCM stream bytes).
    pub blake3: String,
    pub decode_ms: u128,
    pub analysis_ms: u128,
    pub render_ms: u128,
}

/// Sample index of output frame `i`'s start.
pub fn frame_sample(tl: &Timeline, i: i64) -> i64 {
    sample_at(
        RationalTime::from_frames(i, tl.output.fps),
        tl.audio.sample_rate,
    )
}

/// Output length in samples: through the end of the last frame.
pub fn total_samples(tl: &Timeline) -> i64 {
    frame_sample(tl, tl.frame_count())
}

fn rat(r: Rational) -> f64 {
    r.to_f64()
}

/// A resolved program, its decoded sources and their paths.
pub type Resolved = (Program, Vec<SourceAudio>, Vec<PathBuf>);

/// Build the program. `load` returns a source's audio (`None`: no audio stream).
/// Returns `None` if nothing on the timeline has audio.
pub fn resolve(
    tl: &Timeline,
    load: &mut dyn FnMut(&Path) -> anyhow::Result<Option<SourceAudio>>,
) -> anyhow::Result<Option<Resolved>> {
    let rate = tl.audio.sample_rate;
    let s = |t: RationalTime| sample_at(t, rate);
    let mut sources: Vec<SourceAudio> = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut index: HashMap<PathBuf, Option<usize>> = HashMap::new();
    let mut source_of = |path: &Path| -> anyhow::Result<Option<usize>> {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = index.get(&key) {
            return Ok(*i);
        }
        let i = match load(path).with_context(|| format!("audio of {}", path.display()))? {
            Some(a) if !a.is_empty() => {
                sources.push(a);
                paths.push(path.to_path_buf());
                Some(sources.len() - 1)
            }
            _ => None,
        };
        index.insert(key, i);
        Ok(i)
    };
    struct Item<'a> {
        id: &'a str,
        source: &'a Path,
        start: RationalTime,
        source_in: RationalTime,
        duration: RationalTime,
        audio: &'a ClipAudio,
        required: bool,
    }
    let mut tracks_in: Vec<(&str, &BusSpec, Vec<Item>)> = Vec::new();
    for t in &tl.tracks {
        let items = t
            .clips
            .iter()
            .map(|c| Item {
                id: &c.id,
                source: &c.source,
                start: c.start,
                source_in: c.source_in,
                duration: c.duration,
                audio: &c.audio,
                required: false,
            })
            .collect();
        tracks_in.push((&t.name, &t.audio, items));
    }
    for t in &tl.audio_tracks {
        let items = t
            .clips
            .iter()
            .map(|c| Item {
                id: &c.id,
                source: &c.source,
                start: c.start,
                source_in: c.source_in,
                duration: c.duration,
                audio: &c.audio,
                required: true,
            })
            .collect();
        tracks_in.push((&t.name, &t.bus, items));
    }
    let mut any = false;
    let mut tracks = Vec::new();
    for (name, bus, items) in &tracks_in {
        let mut clips: Vec<ClipProg> = Vec::new();
        // (index into `clips` of the crossfading clip, crossfade end time, fade)
        let mut xfades: Vec<(RationalTime, Fade)> = Vec::new();
        let mut ends: Vec<RationalTime> = Vec::new();
        for it in items {
            if it.audio.mute {
                continue;
            }
            let Some(src) = source_of(it.source)? else {
                if it.required {
                    bail!(
                        "clip {}: {} has no audio stream",
                        it.id,
                        it.source.display()
                    );
                }
                continue;
            };
            any = true;
            let (a0, a1) = crate::timeline::audio_region(it.start, it.duration, it.audio);
            let mut fades = Vec::new();
            let mk = |t0: RationalTime, t1: RationalTime, curve, fade_in| Fade {
                start: s(t0),
                len: s(t1) - s(t0),
                curve,
                fade_in,
            };
            if let Some(f) = &it.audio.fade_in {
                fades.push(mk(a0, a0 + f.duration, f.curve, true));
            }
            if let Some(f) = &it.audio.fade_out {
                fades.push(mk(a1 - f.duration, a1, f.curve, false));
            }
            if let Some(x) = &it.audio.crossfade_in {
                fades.push(mk(a0, a0 + x.duration, x.curve, true));
                xfades.push((a0 + x.duration, mk(a0, a0 + x.duration, x.curve, false)));
            }
            clips.push(ClipProg {
                id: it.id.to_string(),
                source: src,
                start: s(a0),
                end: s(a1),
                src_offset: ((it.source_in - it.start).seconds() * Rational::from_int(rate as i64))
                    .round(),
                origin: it.start.seconds(),
                gain_db: it.audio.gain_db.clone(),
                pan: it.audio.pan.clone(),
                fades,
            });
            ends.push(a1);
        }
        // The outgoing side of each crossfade: the clip whose audio ends where it ends.
        for (end, fade) in xfades {
            if let Some(i) = ends.iter().position(|e| *e == end) {
                clips[i].fades.push(fade);
            }
        }
        tracks.push(TrackProg {
            name: name.to_string(),
            clips,
            gain_db: bus.gain_db.clone(),
            pan: bus.pan.clone(),
            mute: bus.mute,
            duck: None,
        });
    }
    if !any {
        return Ok(None);
    }
    for (ti, (name, bus, _)) in tracks_in.iter().enumerate() {
        let Some(d) = &bus.duck else { continue };
        let keys = d
            .key
            .iter()
            .map(|k| {
                tracks_in
                    .iter()
                    .position(|(n, _, _)| n == k)
                    .with_context(|| format!("track {name:?}: duck key {k:?} not found"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        tracks[ti].duck = Some(Duck {
            keys,
            threshold_db: d.threshold_db.clone(),
            ratio: d.ratio.clone(),
            attack_ms: d.attack_ms.clone(),
            release_ms: d.release_ms.clone(),
            range_db: d.range_db.clone(),
        });
    }
    let program = Program {
        rate,
        total: total_samples(tl),
        tracks,
        master_gain_db: tl.audio.master_gain_db.clone(),
        loudness: tl.audio.loudness.as_ref().map(|l| LoudnessTarget {
            target_lufs: rat(l.target_lufs),
            true_peak_dbtp: rat(l.true_peak_dbtp),
        }),
    };
    Ok(Some((program, sources, paths)))
}

/// Decode, resolve and analyze the timeline's audio (`None`: no audio).
pub fn prepare(tl: &Timeline) -> anyhow::Result<Option<AudioPlan>> {
    let t0 = Instant::now();
    let rate = tl.audio.sample_rate;
    let mut info = Vec::new();
    let resolved = resolve(tl, &mut |p| {
        Ok(decode_audio(p, rate)?.map(|d| {
            info.push(SourceInfo {
                path: p.to_path_buf(),
                codec: d.codec,
                source_rate: d.source_rate,
                source_channels: d.source_channels,
                samples: d.audio.len(),
            });
            d.audio
        }))
    })?;
    let Some((program, sources, _paths)) = resolved else {
        return Ok(None);
    };
    let decode_ms = t0.elapsed().as_millis();
    let t1 = Instant::now();
    let (control, analysis) = analyze(&program, &sources).map_err(anyhow::Error::msg)?;
    Ok(Some(AudioPlan {
        program,
        sources,
        info,
        control,
        analysis,
        decode_ms,
        analysis_ms: t1.elapsed().as_millis(),
    }))
}

impl AudioPlan {
    /// The master for output frames `[start_frame, start_frame + frames)`.
    pub fn render_frames(&self, tl: &Timeline, start_frame: i64, frames: i64) -> Stereo {
        let a = frame_sample(tl, start_frame).min(self.program.total);
        let b = frame_sample(tl, start_frame + frames).min(self.program.total);
        render_range(&self.program, &self.sources, &self.control, a, b)
    }
}
