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
    AnalysisReport, ClipProg, Duck, Fade, LoudnessTarget, Measurement, Program, SourceAudio,
    Stereo, TrackProg, analyze, render_range, sample_at,
};
use ferrocut_core::{Rational, RationalTime};
use serde::Serialize;

use crate::audio_fx::to_effects;
use crate::comp::{CompStack, is_comp};
use crate::media::audio::AUDIO_DECODE_VERSION;
use crate::mixdown::{self, CacheStats, DiskSrc, FinalChunk, FinalReader, PcmMeta, Store};
use crate::retime::TimeMap;
use crate::timeline::{BusSpec, ClipAudio, Timeline};
use ferrocut_audio::retime::StretchBackend;

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

/// A resolved, mixed, normalized and limited timeline mix: the final
/// chunks on disk (see [`crate::mixdown`]).
pub struct AudioPlan {
    pub program: Program,
    pub sources: Vec<DiskSrc>,
    pub info: Vec<SourceInfo>,
    pub analysis: AnalysisReport,
    /// Measured from the final chunks' block-energy records.
    pub output: Measurement,
    pub finals: Vec<FinalChunk>,
    pub cache: CacheStats,
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
    /// Measured on the rendered master (assembled block-energy records of
    /// the final chunks, SeePlus's format: what ferrocut-perceive measures).
    pub output: Option<Measurement>,
    /// Audio chunk cache counters (premix / final chunks, loudness records).
    pub cache: CacheStats,
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

/// Where [`resolve_with`] gets source audio: decoded media, nested comps'
/// mixes and retimed derivations.
pub trait AudioLoader {
    type Src;
    /// A media file's audio (`None`: no audio stream).
    fn media(&mut self, path: &Path) -> anyhow::Result<Option<Self::Src>>;
    /// The mix of a nested comp's resolved program (no loudness target).
    fn comp(&mut self, program: Program, sources: Vec<Self::Src>) -> anyhow::Result<Self::Src>;
    /// `base` rendered along source `positions` (one per output sample).
    fn retimed(
        &mut self,
        base: &Self::Src,
        positions: &[f64],
        rate: u32,
        backend: StretchBackend,
    ) -> anyhow::Result<Self::Src>;
    fn is_empty(&self, src: &Self::Src) -> bool;
}

/// In-memory sources (tests, and the reference for the streaming path).
struct MemLoader<'a> {
    load: &'a mut dyn FnMut(&Path) -> anyhow::Result<Option<SourceAudio>>,
}

impl AudioLoader for MemLoader<'_> {
    type Src = SourceAudio;
    fn media(&mut self, path: &Path) -> anyhow::Result<Option<SourceAudio>> {
        (self.load)(path)
    }
    /// The comp's program analyzed (ducking) and rendered whole.
    fn comp(&mut self, program: Program, sources: Vec<SourceAudio>) -> anyhow::Result<SourceAudio> {
        let (control, _) = analyze(&program, &sources).map_err(anyhow::Error::msg)?;
        let mix = render_range(&program, &sources, &control, 0, program.total);
        Ok(SourceAudio {
            planes: vec![mix.l, mix.r],
        })
    }
    fn retimed(
        &mut self,
        base: &SourceAudio,
        positions: &[f64],
        rate: u32,
        backend: StretchBackend,
    ) -> anyhow::Result<SourceAudio> {
        Ok(ferrocut_audio::retime::render(
            base, positions, rate, backend,
        ))
    }
    fn is_empty(&self, src: &SourceAudio) -> bool {
        src.is_empty()
    }
}

/// Build the program. `load` returns a source's audio (`None`: no audio stream).
/// Returns `None` if nothing on the timeline has audio.
pub fn resolve(
    tl: &Timeline,
    load: &mut dyn FnMut(&Path) -> anyhow::Result<Option<SourceAudio>>,
) -> anyhow::Result<Option<Resolved>> {
    resolve_with(tl, &mut MemLoader { load })
}

/// [`resolve`] with any [`AudioLoader`].
#[allow(clippy::type_complexity)]
pub fn resolve_with<L: AudioLoader>(
    tl: &Timeline,
    loader: &mut L,
) -> anyhow::Result<Option<(Program, Vec<L::Src>, Vec<PathBuf>)>> {
    let baked = crate::expr::bake(tl)?;
    let tl: &Timeline = &baked;
    resolve_in(tl, loader, &mut CompStack::new())
}

/// Disk-backed sources in the mixdown cache: decoded media keyed by the
/// file's blake3 (like video sources), comps and retimed audio by their
/// inputs' keys.
pub struct DiskLoader<'a> {
    pub store: &'a Store,
    pub rate: u32,
    pub info: Vec<SourceInfo>,
}

fn file_blake3(path: &Path) -> anyhow::Result<String> {
    use std::sync::Mutex;
    type Key = (PathBuf, u64, std::time::SystemTime);
    static MEMO: Mutex<Option<HashMap<Key, String>>> = Mutex::new(None);
    let md = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let key = (path.to_path_buf(), md.len(), md.modified()?);
    if let Some(h) = MEMO.lock().unwrap().get_or_insert_default().get(&key) {
        return Ok(h.clone());
    }
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(path)?)?;
    let h = h.finalize().to_hex().to_string();
    MEMO.lock()
        .unwrap()
        .get_or_insert_default()
        .insert(key, h.clone());
    Ok(h)
}

impl AudioLoader for DiskLoader<'_> {
    type Src = DiskSrc;
    fn media(&mut self, path: &Path) -> anyhow::Result<Option<DiskSrc>> {
        let mut h = blake3::Hasher::new();
        h.update(mixdown::MIXDOWN_VERSION.as_bytes());
        h.update(b"\0media\0");
        h.update(AUDIO_DECODE_VERSION.as_bytes());
        h.update(file_blake3(path)?.as_bytes());
        h.update(&self.rate.to_le_bytes());
        let key = h.finalize().to_hex().to_string();
        let file = self.store.source_path(&key);
        let (src, meta) = match DiskSrc::open(&key, &file).filter(|_| !self.store.force) {
            Some(x) => x,
            None => match DiskSrc::write_stream(&key, &file, path, self.rate)? {
                Some(x) => x,
                None => return Ok(None),
            },
        };
        self.info.push(SourceInfo {
            path: path.to_path_buf(),
            codec: meta.codec,
            source_rate: meta.source_rate,
            source_channels: meta.source_channels,
            samples: src.len as usize,
        });
        Ok(Some(src))
    }
    fn comp(&mut self, program: Program, sources: Vec<DiskSrc>) -> anyhow::Result<DiskSrc> {
        let key = mixdown::program_key(&program, &sources);
        let file = self.store.source_path(&key);
        match DiskSrc::open(&key, &file).filter(|_| !self.store.force) {
            Some((s, _)) => Ok(s),
            None => mixdown::write_unnormalized(&program, &sources, &key, &file),
        }
    }
    fn retimed(
        &mut self,
        base: &DiskSrc,
        positions: &[f64],
        rate: u32,
        backend: StretchBackend,
    ) -> anyhow::Result<DiskSrc> {
        let mut h = blake3::Hasher::new();
        h.update(mixdown::MIXDOWN_VERSION.as_bytes());
        h.update(b"\0retime\0");
        h.update(ferrocut_audio::VERSION.as_bytes());
        h.update(base.key.as_bytes());
        h.update(format!("{backend:?}").as_bytes());
        h.update(&rate.to_le_bytes());
        for p in positions {
            h.update(&p.to_bits().to_le_bytes());
        }
        let key = h.finalize().to_hex().to_string();
        let file = self.store.source_path(&key);
        if let Some((s, _)) = DiskSrc::open(&key, &file).filter(|_| !self.store.force) {
            return Ok(s);
        }
        // Only the source span the positions reach is read (not the whole
        // media), so memory follows the clip, not the file.
        let (lo, hi) = ferrocut_audio::retime::source_span(positions, rate, backend);
        let win = base.window(lo, hi)?;
        let derived = ferrocut_audio::retime::render_window(&win, lo, positions, rate, backend);
        DiskSrc::write(&key, &file, &derived, PcmMeta::default())
    }
    fn is_empty(&self, src: &DiskSrc) -> bool {
        src.len == 0
    }
}

#[allow(clippy::type_complexity)]
fn resolve_in<L: AudioLoader>(
    tl: &Timeline,
    loader: &mut L,
    stack: &mut CompStack,
) -> anyhow::Result<Option<(Program, Vec<L::Src>, Vec<PathBuf>)>> {
    let rate = tl.audio.sample_rate;
    let s = |t: RationalTime| sample_at(t, rate);
    let mut sources: Vec<L::Src> = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut index: HashMap<PathBuf, Option<usize>> = HashMap::new();
    let mut source_of = |loader: &mut L,
                         sources: &mut Vec<L::Src>,
                         paths: &mut Vec<PathBuf>,
                         path: &Path|
     -> anyhow::Result<Option<usize>> {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = index.get(&key) {
            return Ok(*i);
        }
        let loaded = if is_comp(path) {
            let (ckey, inner) = stack.load(path)?;
            stack.push(ckey);
            let a = comp_audio(&inner, rate, loader, stack);
            stack.pop();
            a
        } else {
            loader.media(path)
        };
        let i = match loaded.with_context(|| format!("audio of {}", path.display()))? {
            Some(a) if !loader.is_empty(&a) => {
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
        map: TimeMap,
        required: bool,
    }
    let mut tracks_in: Vec<(&str, &BusSpec, Vec<Item>)> = Vec::new();
    for t in &tl.tracks {
        let items = t
            .clips
            .iter()
            .filter(|c| !c.is_generator())
            .map(|c| Item {
                id: &c.id,
                source: &c.source,
                start: c.start,
                source_in: c.source_in,
                duration: c.duration,
                audio: &c.audio,
                map: c.time_map(),
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
                map: c.time_map(),
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
            let Some(mut src) = source_of(loader, &mut sources, &mut paths, it.source)? else {
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
            let mut src_offset =
                ((it.source_in - it.start).seconds() * Rational::from_int(rate as i64)).round();
            if !it.map.is_identity() {
                // Retimed: render the clip's audio region along its time map
                // into a derived source that plays 1:1 from the region start.
                let (p0, p1) = (s(a0), s(a1));
                let pos = retime_positions(&it.map, it.start, p0, p1, rate);
                let backend = if it.audio.preserve_pitch {
                    StretchBackend::Wsola
                } else {
                    StretchBackend::Varispeed
                };
                let derived = loader.retimed(&sources[src], &pos, rate, backend)?;
                sources.push(derived);
                paths.push(it.source.to_path_buf());
                src = sources.len() - 1;
                src_offset = -p0;
            }
            clips.push(ClipProg {
                id: it.id.to_string(),
                source: src,
                start: s(a0),
                end: s(a1),
                src_offset,
                origin: it.start.seconds(),
                gain_db: it.audio.gain_db.clone(),
                pan: it.audio.pan.clone(),
                fades,
                effects: to_effects(&it.audio.effects),
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
            effects: to_effects(&bus.effects),
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

/// Source sample positions for program samples `[p0, p1)` of a clip
/// starting at `start` with time map `map`. The linear map is evaluated per
/// sample; curves every 64 samples with linear interpolation in between
/// (the same function the video side samples per frame).
pub fn retime_positions(
    map: &TimeMap,
    start: RationalTime,
    p0: i64,
    p1: i64,
    rate: u32,
) -> Vec<f64> {
    let n = (p1 - p0).max(0) as usize;
    let r = rate as f64;
    let local = |p: i64| Rational::new(p, rate as i64) - start.0;
    if let TimeMap::Linear { source_in, speed } = map {
        let (si, sp) = (source_in.to_f64(), speed.to_f64());
        let st = start.0.to_f64();
        return (0..n)
            .map(|k| {
                let p = p0 + k as i64;
                (si + sp * (p as f64 / r - st)) * r
            })
            .collect();
    }
    const B: usize = 64;
    let pts: Vec<f64> = (0..=n.div_ceil(B))
        .map(|j| map.source_seconds(local(p0 + (j * B) as i64)) * r)
        .collect();
    (0..n)
        .map(|k| {
            let (j, f) = (k / B, (k % B) as f64 / B as f64);
            pts[j] + (pts[j + 1] - pts[j]) * f
        })
        .collect()
}

/// The mix of nested comp `inner` at `rate` as a stereo source: its program
/// without loudness normalization (the outermost timeline normalizes).
fn comp_audio<L: AudioLoader>(
    inner: &Timeline,
    rate: u32,
    loader: &mut L,
    stack: &mut CompStack,
) -> anyhow::Result<Option<L::Src>> {
    let mut inner = inner.clone();
    inner.audio.sample_rate = rate;
    inner.audio.loudness = None;
    let Some((program, sources, _)) = resolve_in(&inner, loader, stack)? else {
        return Ok(None);
    };
    loader.comp(program, sources).map(Some)
}

/// Resolve the timeline's audio onto disk-backed sources and run the
/// streaming, cached mixdown into final chunks under `cache_dir`
/// (`None`: no audio). `force` ignores cached audio.
pub fn prepare(tl: &Timeline, cache_dir: &Path, force: bool) -> anyhow::Result<Option<AudioPlan>> {
    let t0 = Instant::now();
    let store = Store::new(cache_dir, force);
    let mut loader = DiskLoader {
        store: &store,
        rate: tl.audio.sample_rate,
        info: Vec::new(),
    };
    let Some((program, sources, _paths)) = resolve_with(tl, &mut loader)? else {
        return Ok(None);
    };
    let info = loader.info;
    let decode_ms = t0.elapsed().as_millis();
    let t1 = Instant::now();
    let m = mixdown::run(&program, &sources, &store)?;
    Ok(Some(AudioPlan {
        program,
        sources,
        info,
        analysis: m.analysis,
        output: m.output,
        finals: m.finals,
        cache: m.stats,
        decode_ms,
        analysis_ms: t1.elapsed().as_millis(),
    }))
}

impl AudioPlan {
    /// A sequential reader over the final master.
    pub fn reader(&self) -> FinalReader<'_> {
        FinalReader::new(&self.finals)
    }

    /// The master for output frames `[start_frame, start_frame + frames)`.
    pub fn render_frames(
        &self,
        tl: &Timeline,
        start_frame: i64,
        frames: i64,
    ) -> anyhow::Result<Stereo> {
        let a = frame_sample(tl, start_frame).min(self.program.total);
        let b = frame_sample(tl, start_frame + frames).min(self.program.total);
        let v = self.reader().read(a, b)?;
        let mut s = Stereo::silence(v.len() / 2);
        for i in 0..s.len() {
            s.l[i] = v[2 * i];
            s.r[i] = v[2 * i + 1];
        }
        Ok(s)
    }
}
