//! The analysis pipeline: decode each chunk master once, compute scope counts
//! (GPU when available, CPU otherwise; identical numbers), cache the result
//! under the chunk's key, then run timeline-level shot detection, audio
//! loudness, contact sheets and issue flagging.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use ferrocut_core::{GpuContext, Rational, RationalTime};
use serde::{Deserialize, Serialize};

use crate::audio::{self, AudioAnalysis, AudioBuffer};
use crate::gpu::GpuScopes;
use crate::images::{Rgb, contact_sheet, downscale_bgrz, scope_image, thumb_size};
use crate::input::{RenderReport, Timeline};
use crate::report::*;
use crate::scopes::{self, ColorStats, HIST_LEN, Levels, OFF_HIST, OFF_VEC, VEC_LEN, round};
use crate::shots::{self, FrameFeat, Shots, Thresholds};

/// Bump when cached per-chunk analysis would change for the same frames.
pub const ANALYSIS_VERSION: &str = "ferrocut.perceive.chunk/1";

#[derive(Clone, Debug)]
pub struct Options {
    /// Full-scope sampling stride in frames (default: one per second).
    pub sample_every: Option<i64>,
    /// Write a scope PNG per sampled frame.
    pub scope_images: bool,
    pub thumb_width: usize,
    pub contact_sheets: bool,
    /// Max cells on the whole-timeline contact sheet.
    pub timeline_sheet_cells: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            sample_every: None,
            scope_images: false,
            thumb_width: 160,
            contact_sheets: true,
            timeline_sheet_cells: 48,
        }
    }
}

/// Program audio, aligned so sample 0 is timeline time 0. It is analysed
/// per engine chunk (see [`audio`]), cached under
/// `<cache>/perceive/v1/audio/<key>.json`.
pub enum AudioInput {
    /// The engine's master (video + PCM f32 audio): decoded, and every
    /// chunk's PCM is checked against the render report's `audio_blake3`.
    Master(PathBuf),
    /// Any other FFmpeg-readable file (no hash check).
    File(PathBuf),
    /// Samples already in memory (no hash check).
    Buffer(AudioBuffer),
}

impl AudioInput {
    /// The engine's master, when the render report says it carries audio.
    /// A relative `output` path is tried as given, then next to the report.
    pub fn from_render(rr: &RenderReport, report_dir: &Path) -> Option<AudioInput> {
        rr.audio.as_ref()?;
        let out = rr.output.as_ref()?;
        let path = if out.is_absolute() || out.exists() {
            out.clone()
        } else {
            report_dir.join(out.file_name()?)
        };
        Some(AudioInput::Master(path))
    }
}

pub struct Request<'a> {
    pub timeline: &'a Timeline,
    pub render: &'a RenderReport,
    /// The engine's cache dir (`chunks/` lives here; analysis is cached in `perceive/`).
    pub cache_dir: &'a Path,
    /// Where images referenced by the report are written (paths in the report
    /// are relative to it).
    pub out_dir: &'a Path,
    pub audio: Option<AudioInput>,
    pub options: Options,
    pub gpu: Option<&'a GpuContext>,
}

/// Run statistics, kept out of the report so the report stays deterministic.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub chunks_analyzed: usize,
    pub chunks_cached: usize,
    pub frames_decoded: usize,
    pub gpu_frames: usize,
    pub cpu_frames: usize,
    /// Audio chunks analysed vs. reused from the cache.
    pub audio_chunks_analyzed: usize,
    pub audio_chunks_cached: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SampleAnalysis {
    pub offset: i64,
    pub levels: Levels,
    pub color: ColorStats,
}

/// Everything perceive needs from one chunk; cached as JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChunkAnalysis {
    pub version: String,
    pub analysis_key: String,
    pub chunk_key: String,
    pub start_frame: i64,
    pub frames: i64,
    pub width: u32,
    pub height: u32,
    pub feats: Vec<FrameFeat>,
    /// Luma, R, G, B histograms summed over every frame.
    pub hist: Vec<u64>,
    /// Vectorscope summed over sampled frames, sparse `(bin, count)`.
    pub vec_sum: Vec<(u32, u64)>,
    pub samples: Vec<SampleAnalysis>,
    pub scope_images: bool,
}

fn sample_offsets(start: i64, frames: i64, every: i64) -> Vec<i64> {
    (0..frames)
        .filter(|&o| o == 0 || o == frames - 1 || (start + o) % every == 0)
        .collect()
}

fn analysis_key(chunk_key: &str, start: i64, frames: i64, every: i64, o: &Options) -> String {
    let mut h = blake3::Hasher::new();
    for part in [
        ANALYSIS_VERSION,
        chunk_key,
        &start.to_string(),
        &frames.to_string(),
        &every.to_string(),
        &o.scope_images.to_string(),
        &o.thumb_width.to_string(),
    ] {
        h.update(part.as_bytes());
        h.update(&[0]);
    }
    scopes::hex(&h.finalize().as_bytes()[..16])
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

struct Cache {
    dir: PathBuf,
}

impl Cache {
    fn json(&self, akey: &str) -> PathBuf {
        self.dir.join(format!("{akey}.json"))
    }
    fn thumbs(&self, akey: &str) -> PathBuf {
        self.dir.join(format!("{akey}.thumbs.png"))
    }
    fn scope(&self, akey: &str, offset: i64) -> PathBuf {
        self.dir.join(format!("{akey}.scope-{offset:05}.png"))
    }
    fn load(&self, akey: &str, frames: i64, scope_images: bool) -> Option<ChunkAnalysis> {
        let a: ChunkAnalysis =
            serde_json::from_str(&std::fs::read_to_string(self.json(akey)).ok()?).ok()?;
        let ok = a.version == ANALYSIS_VERSION
            && a.analysis_key == akey
            && a.frames == frames
            && a.feats.len() as i64 == frames
            && self.thumbs(akey).is_file()
            && (!scope_images
                || a.samples
                    .iter()
                    .all(|s| self.scope(akey, s.offset).is_file()));
        ok.then_some(a)
    }
}

struct Scopes<'a> {
    gpu: Option<&'a GpuContext>,
    pipe: Option<GpuScopes>,
}

impl Scopes<'_> {
    fn counts(&mut self, px: &[u8], w: u32, h: u32, full: bool, stats: &mut Stats) -> Vec<u32> {
        if let Some(gpu) = self.gpu {
            if self.pipe.as_ref().is_none_or(|p| p.size() != (w, h)) {
                self.pipe = GpuScopes::new(gpu, w, h).ok();
            }
            if let Some(p) = &self.pipe
                && let Ok(c) = p.counts(gpu, px, full)
            {
                stats.gpu_frames += 1;
                return c;
            }
        }
        stats.cpu_frames += 1;
        scopes::counts_cpu(px, w as usize, h as usize, full)
    }
}

#[allow(clippy::too_many_arguments)] // one call site; a context struct would only rename the arguments
fn analyze_chunk(
    path: &Path,
    akey: &str,
    c: &crate::input::ChunkRef,
    every: i64,
    o: &Options,
    cache: &Cache,
    sc: &mut Scopes,
    stats: &mut Stats,
) -> anyhow::Result<ChunkAnalysis> {
    let offsets = sample_offsets(c.start_frame, c.frames, every);
    let mut feats = Vec::with_capacity(c.frames as usize);
    let mut hist = vec![0u64; HIST_LEN];
    let mut vec_sum = vec![0u64; VEC_LEN];
    let mut samples = Vec::new();
    let mut thumbs: Vec<Rgb> = Vec::new();
    let mut cell_px: Option<(u32, u32, Vec<u32>)> = None;
    let mut size = (0, 0);
    let n = crate::media::for_each_frame(path, |i, px, w, h| {
        size = (w, h);
        let full = offsets.binary_search(&(i as i64)).is_ok();
        let counts = sc.counts(px, w, h, full, stats);
        if cell_px
            .as_ref()
            .is_none_or(|(cw, ch, _)| (*cw, *ch) != (w, h))
        {
            cell_px = Some((w, h, scopes::thumb_cell_pixels(w as usize, h as usize)));
        }
        let sig = scopes::signature(&counts, &cell_px.as_ref().unwrap().2, px);
        feats.push(FrameFeat::from_signature(&sig));
        let fh: Vec<u64> = counts[OFF_HIST..OFF_HIST + HIST_LEN]
            .iter()
            .map(|&v| v as u64)
            .collect();
        for (a, b) in hist.iter_mut().zip(&fh) {
            *a += b;
        }
        if full {
            let vec = &counts[OFF_VEC..OFF_VEC + VEC_LEN];
            for (a, &b) in vec_sum.iter_mut().zip(vec) {
                *a += b as u64;
            }
            samples.push(SampleAnalysis {
                offset: i as i64,
                levels: scopes::levels(&fh),
                color: scopes::color_stats(vec),
            });
            let (tw, th) = thumb_size(w, h, o.thumb_width);
            thumbs.push(downscale_bgrz(px, w as usize, h as usize, tw, th));
            if o.scope_images {
                scope_image(&counts).write_png(&cache.scope(akey, i as i64))?;
            }
        }
        Ok(())
    })?;
    stats.frames_decoded += n;
    if n as i64 != c.frames {
        bail!(
            "chunk {} ({}): decoded {n} frames, render report says {}",
            c.index,
            path.display(),
            c.frames
        );
    }
    // Thumbnails stacked vertically.
    let (tw, th) = (thumbs[0].w, thumbs[0].h);
    let mut strip = Rgb::new(tw, th * thumbs.len(), [0; 3]);
    for (k, t) in thumbs.iter().enumerate() {
        strip.blit(t, 0, k * th);
    }
    strip.write_png(&cache.thumbs(akey))?;
    let a = ChunkAnalysis {
        version: ANALYSIS_VERSION.into(),
        analysis_key: akey.into(),
        chunk_key: c.key.clone(),
        start_frame: c.start_frame,
        frames: c.frames,
        width: size.0,
        height: size.1,
        feats,
        hist,
        vec_sum: vec_sum
            .iter()
            .enumerate()
            .filter(|(_, v)| **v > 0)
            .map(|(i, &v)| (i as u32, v))
            .collect(),
        samples,
        scope_images: o.scope_images,
    };
    write_atomic(&cache.json(akey), &serde_json::to_vec(&a)?)?;
    Ok(a)
}

fn dense_vec(sparse: &[(u32, u64)], into: &mut [u64]) {
    for &(i, v) in sparse {
        into[i as usize] += v;
    }
}

fn color_of(vec: &[u64]) -> ColorStats {
    // color_stats takes u32 bins; scale down if a long program overflows them.
    let max = vec.iter().copied().max().unwrap_or(0);
    let shift = (64 - max.leading_zeros()).saturating_sub(32);
    let v: Vec<u32> = vec.iter().map(|&x| (x >> shift) as u32).collect();
    scopes::color_stats(&v)
}

/// Sample range `[start, end)` of each engine chunk at `rate`: the engine's
/// `frame_sample` rule (exact rational time × rate, halves away from zero),
/// clamped to the decoded length; the last chunk runs to the end.
pub fn chunk_sample_ranges(
    rr: &RenderReport,
    fps: Rational,
    rate: u32,
    total: u64,
) -> Vec<(u64, u64)> {
    let at = |f: i64| -> u64 {
        let s = RationalTime::from_frames(f, fps).frame_round(Rational::from_int(rate as i64));
        (s.max(0) as u64).min(total)
    };
    let mut v: Vec<(u64, u64)> = rr
        .chunks
        .iter()
        .map(|c| (at(c.start_frame), at(c.start_frame + c.frames)))
        .collect();
    match v.last_mut() {
        Some(l) => l.1 = total,
        None => v.push((0, total)),
    }
    v
}

struct AudioResult {
    analysis: AudioAnalysis,
    /// Chunk indices whose decoded PCM differs from the engine's hash.
    mismatches: Vec<usize>,
}

fn audio_analysis(
    input: AudioInput,
    rr: &RenderReport,
    fps: Rational,
    cache_dir: &Path,
    stats: &mut Stats,
) -> anyhow::Result<AudioResult> {
    let (buf, check) = match input {
        AudioInput::Master(p) => (crate::media::decode_audio(&p)?, true),
        AudioInput::File(p) => (crate::media::decode_audio(&p)?, false),
        AudioInput::Buffer(b) => (b, false),
    };
    let ranges = chunk_sample_ranges(rr, fps, buf.sample_rate, buf.frames() as u64);
    let expected: Vec<Option<String>> = if check {
        rr.chunks.iter().map(|c| c.audio_blake3.clone()).collect()
    } else {
        Vec::new()
    };
    let r = audio::analyze_cached(&buf, &ranges, &expected, &cache_dir.join("audio"))?;
    stats.audio_chunks_analyzed += r.analyzed.len();
    stats.audio_chunks_cached += ranges.len() - r.analyzed.len();
    Ok(AudioResult {
        analysis: r.analysis,
        mismatches: r
            .mismatches
            .iter()
            .filter_map(|&i| rr.chunks.get(i).map(|c| c.index))
            .collect(),
    })
}

/// Where a chunk master lives: the report's `chunk_dir` (per-adapter since
/// engine 7de57ff), else the same adapter dir under `cache_dir`, else the
/// older flat `cache_dir/chunks/`.
fn find_chunk(rr: &RenderReport, cache_dir: &Path, key: &str) -> PathBuf {
    let name = format!("{key}.mkv");
    let mut cands = Vec::new();
    if let Some(d) = &rr.chunk_dir {
        cands.push(d.join(&name));
        if let Some(tag) = d.file_name() {
            cands.push(cache_dir.join("chunks").join(tag).join(&name));
        }
    }
    cands.push(cache_dir.join("chunks").join(&name));
    cands
        .iter()
        .find(|p| p.exists())
        .unwrap_or(&cands[0])
        .clone()
}

pub fn analyze(req: Request) -> anyhow::Result<(Report, Stats)> {
    let tl = req.timeline;
    let rr = req.render;
    let o = &req.options;
    let fps = tl.output.fps;
    let fps_f = fps.num() as f64 / fps.den() as f64;
    let every = o.sample_every.unwrap_or_else(|| fps.round()).max(1);
    let cache = Cache {
        dir: req.cache_dir.join("perceive").join("v1"),
    };
    std::fs::create_dir_all(&cache.dir)
        .with_context(|| format!("creating {}", cache.dir.display()))?;
    let mut stats = Stats::default();
    let mut sc = Scopes {
        gpu: req.gpu,
        pipe: None,
    };

    // 1. Per-chunk analysis (cached by chunk key).
    let mut chunks: Vec<(crate::input::ChunkRef, ChunkAnalysis)> = Vec::new();
    let mut expect = 0;
    for c in &rr.chunks {
        if c.start_frame != expect {
            bail!(
                "render report chunks are not contiguous at chunk {}",
                c.index
            );
        }
        expect += c.frames;
        let akey = analysis_key(&c.key, c.start_frame, c.frames, every, o);
        let a = match cache.load(&akey, c.frames, o.scope_images) {
            Some(a) => {
                stats.chunks_cached += 1;
                a
            }
            None => {
                stats.chunks_analyzed += 1;
                let path = find_chunk(rr, req.cache_dir, &c.key);
                analyze_chunk(&path, &akey, c, every, o, &cache, &mut sc, &mut stats)
                    .with_context(|| format!("analyzing chunk {}", c.index))?
            }
        };
        chunks.push((c.clone(), a));
    }
    let total = expect;
    if total != rr.total_frames {
        bail!(
            "chunks cover {total} frames, render report says {}",
            rr.total_frames
        );
    }

    // 2. Shots over the whole timeline.
    let feats: Vec<FrameFeat> = chunks
        .iter()
        .flat_map(|(_, a)| a.feats.iter().cloned())
        .collect();
    let th = Thresholds::for_fps(fps_f);
    let intended = tl.intended(total);
    let shots = shots::detect(&feats, &intended, &th);

    // 3. Audio.
    let audio_r = match req.audio {
        Some(input) => Some(
            audio_analysis(input, rr, fps, &cache.dir, &mut stats).context("analysing audio")?,
        ),
        None => None,
    };
    let audio_a = audio_r.as_ref().map(|r| &r.analysis);
    let audio_report = audio_r.as_ref().map(|r| {
        let mut a = audio::summarize(&r.analysis);
        a.join_mismatch_chunks = r.mismatches.clone();
        a.engine = rr.audio.as_ref().and_then(|e| e.output.as_ref()).map(|m| {
            let d = |ours: Option<f64>, theirs: Option<f64>| match (ours, theirs) {
                (Some(a), Some(b)) => Some(round(a - b, 2)),
                _ => None,
            };
            let (i, tp) = (
                m.integrated_lufs.map(|v| round(v, 2)),
                m.true_peak_dbtp.map(|v| round(v, 2)),
            );
            audio::EngineCrossCheck {
                integrated_lufs: i,
                true_peak_dbtp: tp,
                delta_integrated_lu: d(a.loudness.integrated_lufs, i),
                delta_true_peak_db: d(a.loudness.true_peak_dbtp, tp),
            }
        });
        a
    });

    // 4. Images.
    std::fs::create_dir_all(req.out_dir)?;
    let tc = |f: i64| timecode(f, fps);
    let time = |f: i64| RationalTime::from_frames(f, fps);
    let mut all_thumbs: Vec<(Rgb, String)> = Vec::new();
    let mut chunk_reports = Vec::new();
    let mut total_hist = vec![0u64; HIST_LEN];
    let mut total_vec = vec![0u64; VEC_LEN];
    let mut sampled = 0;
    for (c, a) in &chunks {
        for (x, y) in total_hist.iter_mut().zip(&a.hist) {
            *x += y;
        }
        let mut vec = vec![0u64; VEC_LEN];
        dense_vec(&a.vec_sum, &mut vec);
        for (x, y) in total_vec.iter_mut().zip(&vec) {
            *x += y;
        }
        sampled += a.samples.len();
        let thumbs = Rgb::read_png(&cache.thumbs(&a.analysis_key))?;
        let th_h = thumbs.h / a.samples.len().max(1);
        let cells: Vec<(Rgb, String)> = a
            .samples
            .iter()
            .enumerate()
            .map(|(k, s)| {
                let mut cell = Rgb::new(thumbs.w, th_h, [0; 3]);
                cell.px.copy_from_slice(
                    &thumbs.px[k * th_h * thumbs.w * 3..(k + 1) * th_h * thumbs.w * 3],
                );
                (cell, tc(c.start_frame + s.offset))
            })
            .collect();
        let sheet = if o.contact_sheets {
            let rel = format!("sheets/chunk-{:04}.png", c.index);
            std::fs::create_dir_all(req.out_dir.join("sheets"))?;
            let refs: Vec<(&Rgb, String)> = cells.iter().map(|(r, l)| (r, l.clone())).collect();
            contact_sheet(&refs, 6).write_png(&req.out_dir.join(&rel))?;
            Some(rel)
        } else {
            None
        };
        all_thumbs.extend(cells);
        let mut samples = Vec::new();
        for s in &a.samples {
            let f = c.start_frame + s.offset;
            let scope = if o.scope_images {
                let rel = format!("scopes/frame-{f:06}.png");
                std::fs::create_dir_all(req.out_dir.join("scopes"))?;
                std::fs::copy(
                    cache.scope(&a.analysis_key, s.offset),
                    req.out_dir.join(&rel),
                )?;
                Some(rel)
            } else {
                None
            };
            samples.push(Sample {
                frame: f,
                time: time(f),
                timecode: tc(f),
                levels: s.levels.clone(),
                color: s.color.clone(),
                scope_image: scope,
            });
        }
        let (s0, s1) = (c.start_frame, c.start_frame + c.frames);
        let in_chunk = |f: i64| f >= s0 && f < s1;
        let motion = if a.feats.len() > 1 {
            a.feats
                .windows(2)
                .map(|w| shots::distance(&w[0], &w[1]))
                .sum::<f64>()
                / (a.feats.len() - 1) as f64
        } else {
            0.0
        };
        let overlap = |s: i64, e: i64| (e.min(s1) - s.max(s0)).max(0);
        chunk_reports.push(ChunkReport {
            index: c.index,
            start_frame: s0,
            frames: c.frames,
            start: time(s0),
            end: time(s1),
            timecode: tc(s0),
            key: c.key.clone(),
            analysis_key: a.analysis_key.clone(),
            levels: scopes::levels(&a.hist),
            color: color_of(&vec),
            motion: round(motion, 4),
            black_frames: a.feats.iter().filter(|f| f.is_black()).count() as i64,
            frozen_frames: shots.frozen.iter().map(|s| overlap(s.start, s.end)).sum(),
            samples,
            events: events(&shots, &intended_missed(&shots), in_chunk),
            audio: audio_a.map(|aa| {
                let (b0, b1) = audio::block_range(aa, time(s0), time(s1));
                audio::loudness(aa, b0, b1)
            }),
            contact_sheet: sheet,
        });
    }
    let timeline_sheet = if o.contact_sheets && !all_thumbs.is_empty() {
        let n = all_thumbs.len();
        let k = o.timeline_sheet_cells.clamp(1, n);
        let pick: Vec<(&Rgb, String)> = (0..k)
            .map(|i| i * n / k)
            .map(|i| (&all_thumbs[i].0, all_thumbs[i].1.clone()))
            .collect();
        let rel = "sheets/timeline.png".to_string();
        contact_sheet(&pick, 8).write_png(&req.out_dir.join(&rel))?;
        Some(rel)
    } else {
        None
    };

    // 5. Issues and summary.
    let issues = issues(
        &chunk_reports,
        &shots,
        &th,
        audio_report.as_ref(),
        total,
        fps,
    );
    let mut counts = IssueCounts::default();
    for i in &issues {
        match i.severity {
            Severity::Error => counts.error += 1,
            Severity::Warning => counts.warning += 1,
            Severity::Info => counts.info += 1,
        }
    }
    let span_len = |v: &[shots::Span]| v.iter().map(|s| s.end - s.start).sum::<i64>();
    let summary = Summary {
        frames: total,
        sampled_frames: sampled,
        levels: scopes::levels(&total_hist),
        color: color_of(&total_vec),
        shots: shots.shots.len(),
        cuts: shots.cuts.len(),
        dissolves: shots.dissolves.len(),
        missed_cuts: shots.missed_cuts.len(),
        unexpected_cuts: shots.unexpected_cuts.len(),
        flash_frames: span_len(&shots.flash_frames),
        black_frames: span_len(&shots.black),
        frozen_frames: span_len(&shots.frozen),
        integrated_lufs: audio_report
            .as_ref()
            .and_then(|a| a.loudness.integrated_lufs),
        true_peak_dbtp: audio_report
            .as_ref()
            .and_then(|a| a.loudness.true_peak_dbtp),
        issues: counts,
    };
    let report = Report {
        schema_version: SCHEMA_VERSION.into(),
        generator: concat!("ferrocut-perceive ", env!("CARGO_PKG_VERSION")).into(),
        timeline: TimelineInfo {
            name: tl.name.clone(),
            width: tl.output.width,
            height: tl.output.height,
            fps,
            total_frames: total,
            chunk_frames: rr.chunk_frames,
            duration: tl.duration(),
        },
        settings: Settings {
            sample_every: every,
            scope_images: o.scope_images,
            thumb_width: o.thumb_width,
            thresholds: th,
        },
        summary,
        issues,
        shots,
        audio: audio_report,
        contact_sheet: timeline_sheet,
        chunks: chunk_reports,
    };
    Ok((report, stats))
}

fn intended_missed(s: &Shots) -> Vec<i64> {
    s.missed_cuts.iter().map(|m| m.frame).collect()
}

fn events(s: &Shots, missed: &[i64], in_chunk: impl Fn(i64) -> bool) -> Vec<Event> {
    let mut v = Vec::new();
    let mut push = |kind, frame: i64, end: i64| {
        if in_chunk(frame) {
            v.push(Event {
                kind,
                frame,
                end_frame: end,
            });
        }
    };
    for c in &s.cuts {
        push(EventKind::Cut, c.frame, c.frame + 1);
    }
    for &f in &s.unexpected_cuts {
        push(EventKind::UnexpectedCut, f, f + 1);
    }
    for &f in missed {
        push(EventKind::MissedCut, f, f + 1);
    }
    for d in &s.dissolves {
        push(EventKind::Dissolve, d.start, d.end);
    }
    for x in &s.missed_dissolves {
        push(EventKind::MissedDissolve, x.start, x.end);
    }
    for x in &s.flash_frames {
        push(EventKind::Flash, x.start, x.end);
    }
    for x in &s.black {
        push(EventKind::Black, x.start, x.end);
    }
    for x in &s.frozen {
        push(EventKind::Frozen, x.start, x.end);
    }
    v.sort();
    v
}

fn issues(
    chunks: &[ChunkReport],
    s: &Shots,
    th: &Thresholds,
    audio: Option<&audio::AudioReport>,
    total: i64,
    fps: Rational,
) -> Vec<Issue> {
    let chunk_of = |f: i64| {
        chunks
            .iter()
            .find(|c| f >= c.start_frame && f < c.start_frame + c.frames)
            .map(|c| c.index)
    };
    let mut v = Vec::new();
    let mut add = |severity, kind: &str, frame: Option<i64>, end: Option<i64>, message: String| {
        v.push(Issue {
            severity,
            kind: kind.into(),
            frame,
            end_frame: end,
            timecode: frame.map(|f| timecode(f, fps)),
            chunk: frame.and_then(chunk_of),
            message,
        });
    };
    use Severity::*;
    for c in chunks {
        let (f, e) = (Some(c.start_frame), Some(c.start_frame + c.frames));
        if c.levels.luma_clipped_pct > 1.0 {
            add(
                Warning,
                "clipped_highlights",
                f,
                e,
                format!(
                    "{}% of pixels at Y' >= 253 (white clipped)",
                    c.levels.luma_clipped_pct
                ),
            );
        }
        if c.levels.luma_crushed_pct > 5.0 {
            add(
                Warning,
                "crushed_shadows",
                f,
                e,
                format!(
                    "{}% of pixels at Y' <= 2 (blacks crushed)",
                    c.levels.luma_crushed_pct
                ),
            );
        }
        if let Some(dev) = c.color.skin_line_dev_deg
            && c.color.skin_pct > 2.0
            && dev.abs() > 10.0
        {
            add(
                Info,
                "skin_tone_off_line",
                f,
                e,
                format!(
                    "skin-tone pixels ({}%) sit {dev}° off the skin-tone line",
                    c.color.skin_pct
                ),
            );
        }
    }
    let flash_cut = |f: i64| s.flash_frames.iter().any(|x| f == x.start || f == x.end);
    for x in &s.flash_frames {
        add(
            Warning,
            "flash_frame",
            Some(x.start),
            Some(x.end),
            format!(
                "{}-frame flash: a shot this short between matching shots is usually an edit error",
                x.end - x.start
            ),
        );
    }
    for m in &s.missed_cuts {
        let why = if m.distance < th.cut_min / 2.0 {
            "the two sides look alike (continuous footage?)"
        } else {
            "the picture changes less than a cut threshold"
        };
        add(
            Warning,
            "missed_cut",
            Some(m.frame),
            Some(m.frame + 1),
            format!(
                "timeline cuts here but no cut is visible (distance {}): {why}",
                m.distance
            ),
        );
    }
    for &f in &s.unexpected_cuts {
        if !flash_cut(f) {
            add(
                Warning,
                "unexpected_cut",
                Some(f),
                Some(f + 1),
                "visible cut the timeline doesn't explain (source footage cut or glitch)".into(),
            );
        }
    }
    for x in &s.missed_dissolves {
        add(
            Warning,
            "missed_dissolve",
            Some(x.start),
            Some(x.end),
            "timeline dissolves here but no transition is visible".into(),
        );
    }
    for x in &s.unexpected_dissolves {
        add(
            Info,
            "unexpected_dissolve",
            Some(x.start),
            Some(x.end),
            "gradual transition the timeline doesn't explain (fade in the source?)".into(),
        );
    }
    for x in &s.black {
        let edge = x.start == 0 || x.end == total;
        add(
            if edge { Info } else { Warning },
            "black_frames",
            Some(x.start),
            Some(x.end),
            format!(
                "{} black frame(s){}",
                x.end - x.start,
                if edge {
                    " at the start/end"
                } else {
                    " mid-program"
                }
            ),
        );
    }
    for x in &s.frozen {
        add(
            Info,
            "frozen_frames",
            Some(x.start),
            Some(x.end),
            format!(
                "{} identical frames (still image or stalled source)",
                x.end - x.start
            ),
        );
    }
    if let Some(a) = audio {
        let frame_of = |t: RationalTime| t.frame_floor(fps);
        let l = &a.loudness;
        for &i in &a.join_mismatch_chunks {
            if let Some(c) = chunks.iter().find(|c| c.index == i) {
                add(
                    Error,
                    "audio_join_mismatch",
                    Some(c.start_frame),
                    Some(c.start_frame + c.frames),
                    format!(
                        "chunk {i}: audio decoded from the master differs from the engine's mix (audio_blake3)"
                    ),
                );
            }
        }
        if let Some(e) = &a.engine {
            let off_i = e.delta_integrated_lu.is_some_and(|d| d.abs() > 0.1);
            let off_tp = e.delta_true_peak_db.is_some_and(|d| d.abs() > 0.2);
            if off_i || off_tp {
                add(
                    Warning,
                    "audio_measure_disagrees",
                    None,
                    None,
                    format!(
                        "perceive measures {:?} LUFS / {:?} dBTP, the engine reported {:?} LUFS / {:?} dBTP",
                        l.integrated_lufs, l.true_peak_dbtp, e.integrated_lufs, e.true_peak_dbtp
                    ),
                );
            }
        }
        for c in l.clipping.iter().take(50) {
            add(
                Error,
                "audio_clipping",
                Some(frame_of(c.start)),
                Some(frame_of(c.end) + 1),
                format!(
                    "{} clipped samples on channel {}",
                    c.samples.unwrap_or(0),
                    c.channel.unwrap_or(0)
                ),
            );
        }
        if l.clipping.len() > 50 {
            add(
                Error,
                "audio_clipping",
                None,
                None,
                format!("{} more clipping runs not listed", l.clipping.len() - 50),
            );
        }
        if let Some(tp) = l.true_peak_dbtp
            && tp > -1.0
        {
            add(
                Warning,
                "true_peak_over",
                None,
                None,
                format!("true peak {tp} dBTP exceeds -1 dBTP"),
            );
        }
        if let (Some(i), Some(d1), Some(d2)) =
            (l.integrated_lufs, a.delta_ebu_r128_lu, a.delta_streaming_lu)
            && d1.abs() > 1.0
            && d2.abs() > 1.0
        {
            add(
                Info,
                "loudness_off_target",
                None,
                None,
                format!(
                    "integrated {i} LUFS: {d1:+} LU vs EBU R128 (-23), {d2:+} LU vs streaming (-14)"
                ),
            );
        }
        for x in &l.silence {
            add(
                Info,
                "audio_silence",
                Some(frame_of(x.start)),
                Some(frame_of(x.end)),
                "silence (sample peak below -60 dBFS for 0.5 s or more)".into(),
            );
        }
    }
    v.sort_by(|a, b| {
        (a.severity, a.frame.is_none(), a.frame, &a.kind, &a.message).cmp(&(
            b.severity,
            b.frame.is_none(),
            b.frame,
            &b.kind,
            &b.message,
        ))
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_include_chunk_ends() {
        assert_eq!(sample_offsets(0, 12, 24), vec![0, 11]);
        assert_eq!(sample_offsets(12, 24, 24), vec![0, 12, 23]);
        assert_eq!(
            super::timecode(24 * 3661 + 5, Rational::from_int(24)),
            "01:01:01:05"
        );
    }
}
