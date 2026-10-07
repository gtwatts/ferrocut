//! Chunk planner + parallel scheduler + on-disk chunk cache.
//!
//! The output is split into fixed chunks of `gop * gops_per_chunk` frames, so
//! every chunk starts on a GOP boundary and is an independent closed-GOP encode.
//! A chunk's cache key is the hash of its frames' Merkle keys plus the encoder
//! fingerprint, so after an edit only chunks containing an affected frame
//! re-render; the rest are reused and the whole is losslessly concatenated.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use cutline_core::{FrameKey, GpuContext, RationalTime, RenderCtx, WorkerState};
use rayon::prelude::*;
use serde::Serialize;

use crate::compile::Compiled;
use crate::compositor::{Compositor, compositor_slot};
use crate::graph::FrameCache;
use crate::media::concat::concat;
use crate::media::encode::{ChunkEncoder, EncodeSettings};
use crate::timeline::Timeline;

pub const ENGINE_VERSION: &str =
    concat!("cutline-engine ", env!("CARGO_PKG_VERSION"), " render.v1");

#[derive(Clone, Debug, Serialize)]
pub struct ChunkPlan {
    pub index: usize,
    pub start_frame: i64,
    pub frames: i64,
    pub key: String,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChunkStatus {
    Reused,
    Rendered,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChunkReport {
    #[serde(flatten)]
    pub plan: ChunkPlan,
    pub status: ChunkStatus,
    pub file_blake3: String,
    pub render_ms: u128,
}

#[derive(Clone, Debug, Serialize)]
pub struct RenderReport {
    pub engine: String,
    pub adapter: String,
    /// FFmpeg version + license of the libavcodec actually loaded.
    pub ffmpeg: String,
    pub output: PathBuf,
    pub total_frames: i64,
    pub chunk_frames: i64,
    pub jobs: usize,
    pub chunks: Vec<ChunkReport>,
    pub rendered_frames: i64,
    pub reused_frames: i64,
    pub render_wall_ms: u128,
    pub concat_ms: u128,
    pub total_ms: u128,
    pub render_fps: f64,
    pub final_blake3: String,
}

pub struct RenderOptions {
    pub cache_dir: PathBuf,
    /// Ignore cached chunks and re-render everything (still refreshes the cache).
    pub force: bool,
    pub jobs: usize,
}

pub fn encode_settings(tl: &Timeline) -> EncodeSettings {
    EncodeSettings {
        width: tl.output.width,
        height: tl.output.height,
        fps: tl.output.fps,
        gop: tl.output.gop,
    }
}

/// Pure planning: frame keys -> chunk keys. No decoding, no GPU.
pub fn plan(tl: &Timeline, c: &Compiled) -> Vec<ChunkPlan> {
    let fps = tl.output.fps;
    let total = tl.frame_count();
    let cf = tl.chunk_frames();
    let fp = encode_settings(tl).fingerprint();
    let mut out = Vec::new();
    let mut start = 0;
    while start < total {
        let frames = cf.min(total - start);
        let mut h = blake3::Hasher::new();
        h.update(b"cutline.chunk.v1\0");
        h.update(ENGINE_VERSION.as_bytes());
        h.update(fp.as_bytes());
        h.update(&frames.to_le_bytes());
        for i in start..start + frames {
            let k: FrameKey = c
                .graph
                .frame_key(c.output, RationalTime::from_frames(i, fps));
            h.update(&k.0);
        }
        out.push(ChunkPlan {
            index: out.len(),
            start_frame: start,
            frames,
            key: h.finalize().to_hex().to_string(),
        });
        start += frames;
    }
    out
}

fn file_blake3(p: &Path) -> anyhow::Result<String> {
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(p)?)?;
    Ok(h.finalize().to_hex().to_string())
}

fn render_chunk(
    tl: &Timeline,
    c: &Compiled,
    gpu: &GpuContext,
    comp: &Arc<Compositor>,
    chunk: &ChunkPlan,
    path: &Path,
) -> anyhow::Result<()> {
    let mut worker = WorkerState::default();
    worker.slot(compositor_slot(), || Ok(comp.clone()))?;
    let mut cache = FrameCache::new(8);
    let tmp = path.with_extension(format!("tmp{}.mkv", std::process::id()));
    let mut enc = ChunkEncoder::create(&tmp, &encode_settings(tl))?;
    for i in chunk.start_frame..chunk.start_frame + chunk.frames {
        let t = RationalTime::from_frames(i, tl.output.fps);
        let mut ctx = RenderCtx {
            gpu,
            worker: &mut worker,
        };
        let frame = c
            .graph
            .evaluate(c.output, t, &mut ctx, &mut cache)
            .map_err(|e| anyhow::anyhow!("frame {i}: {e}"))?;
        let bgra = comp
            .output_bgra(gpu, &frame)
            .map_err(|e| anyhow::anyhow!("frame {i}: {e}"))?;
        enc.push_bgra(&bgra)?;
    }
    enc.finish()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn render(
    tl: &Timeline,
    c: &Compiled,
    gpu: &GpuContext,
    out: &Path,
    opts: &RenderOptions,
) -> anyhow::Result<RenderReport> {
    let t0 = Instant::now();
    let chunk_dir = opts.cache_dir.join("chunks");
    std::fs::create_dir_all(&chunk_dir)
        .with_context(|| format!("creating {}", chunk_dir.display()))?;
    let plans = plan(tl, c);
    let comp = Arc::new(Compositor::new(gpu));
    let path_of = |p: &ChunkPlan| chunk_dir.join(format!("{}.mkv", p.key));

    let todo: Vec<&ChunkPlan> = plans
        .iter()
        .filter(|p| opts.force || !path_of(p).exists())
        .collect();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.jobs.max(1))
        .build()?;
    let t_render = Instant::now();
    let timings: Vec<anyhow::Result<(usize, u128)>> = pool.install(|| {
        todo.par_iter()
            .map(|p| {
                let s = Instant::now();
                render_chunk(tl, c, gpu, &comp, p, &path_of(p))
                    .with_context(|| format!("chunk {}", p.index))?;
                Ok((p.index, s.elapsed().as_millis()))
            })
            .collect()
    });
    let render_wall_ms = t_render.elapsed().as_millis();
    let mut ms = vec![None; plans.len()];
    for r in timings {
        let (i, t) = r?;
        ms[i] = Some(t);
    }

    let t_concat = Instant::now();
    let paths: Vec<PathBuf> = plans.iter().map(path_of).collect();
    let refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
    let starts: Vec<i64> = plans.iter().map(|p| p.start_frame).collect();
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let tmp_out = out.with_extension("partial.mkv");
    concat(&refs, &starts, tl.output.fps, &tmp_out)?;
    std::fs::rename(&tmp_out, out)?;
    let concat_ms = t_concat.elapsed().as_millis();

    let mut chunks = Vec::new();
    let (mut rendered, mut reused) = (0, 0);
    for (p, path) in plans.iter().zip(&paths) {
        let status = if ms[p.index].is_some() {
            ChunkStatus::Rendered
        } else {
            ChunkStatus::Reused
        };
        match status {
            ChunkStatus::Rendered => rendered += p.frames,
            ChunkStatus::Reused => reused += p.frames,
        }
        chunks.push(ChunkReport {
            plan: p.clone(),
            status,
            file_blake3: file_blake3(path)?,
            render_ms: ms[p.index].unwrap_or(0),
        });
    }
    let render_fps = if rendered > 0 && render_wall_ms > 0 {
        rendered as f64 * 1000.0 / render_wall_ms as f64
    } else {
        0.0
    };
    Ok(RenderReport {
        engine: ENGINE_VERSION.to_string(),
        adapter: gpu.describe(),
        ffmpeg: {
            let (v, l, _) = crate::media::ffmpeg_info();
            format!("{v} ({l})")
        },
        output: out.to_path_buf(),
        total_frames: tl.frame_count(),
        chunk_frames: tl.chunk_frames(),
        jobs: opts.jobs,
        chunks,
        rendered_frames: rendered,
        reused_frames: reused,
        render_wall_ms,
        concat_ms,
        total_ms: t0.elapsed().as_millis(),
        render_fps,
        final_blake3: file_blake3(out)?,
    })
}
