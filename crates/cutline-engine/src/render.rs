//! Chunk planner + parallel scheduler + on-disk chunk cache.
//!
//! The output is split into fixed chunks of `gop * gops_per_chunk` frames, so
//! every chunk starts on a GOP boundary and is an independent closed-GOP encode.
//! A chunk's cache key is the hash of its frames' Merkle keys plus the encoder
//! fingerprint, so after an edit only chunks containing an affected frame
//! re-render; the rest are reused and the whole is losslessly concatenated.

//!
//! Failures: a node error of kind `Retryable` re-evaluates that frame up to
//! `max_retries` times (sub-frames that already rendered come from the frame
//! cache); `Permanent` and `Cancelled` are never retried. The first failing
//! chunk cancels its siblings through a child of the caller's [`CancelToken`];
//! the caller's token and deadline are checked between frames and handed to
//! nodes in [`RenderCtx`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use cutline_core::{
    CancelToken, ErrorKind, FrameKey, GpuContext, NodeError, RationalTime, RenderCtx, WorkerState,
};
use rayon::prelude::*;
use serde::Serialize;

use crate::compile::Compiled;
use crate::compositor::{Compositor, ReadbackRing, compositor_slot};
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
    /// Frame re-evaluations after `Retryable` node errors.
    pub retries: u64,
    /// `queue.submit` calls made by render workers (≈ one per rendered frame).
    pub gpu_submissions: u64,
    /// Texture pool: textures created vs. reused from the pool.
    pub textures_allocated: u64,
    pub textures_reused: u64,
    pub final_blake3: String,
}

/// Output frames in flight per chunk worker in the readback ring.
pub const READBACK_DEPTH: usize = 3;

pub struct RenderOptions {
    pub cache_dir: PathBuf,
    /// Ignore cached chunks and re-render everything (still refreshes the cache).
    pub force: bool,
    pub jobs: usize,
    /// Cancel from another thread to stop the render (yields a `Cancelled` [`NodeError`]).
    pub cancel: CancelToken,
    /// Give up with a `Cancelled` [`NodeError`] after this instant.
    pub deadline: Option<Instant>,
    /// Retries per frame for `Retryable` node errors.
    pub max_retries: u32,
}

impl RenderOptions {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        RenderOptions {
            cache_dir: cache_dir.into(),
            force: false,
            jobs: 4,
            cancel: CancelToken::new(),
            deadline: None,
            max_retries: 2,
        }
    }
}

/// `Err(Cancelled)` if `deadline` passed or `cancel` fired.
pub fn check_cancel(cancel: &CancelToken, deadline: Option<Instant>) -> Result<(), NodeError> {
    // Deadline first: once it passes, every worker reports it (rather than
    // the sibling cancellation it triggers).
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(NodeError::cancelled("render deadline exceeded"));
    }
    if cancel.is_cancelled() {
        return Err(NodeError::cancelled("render cancelled"));
    }
    Ok(())
}

/// Run `f(attempt)` until it succeeds, fails with a non-retryable error, or has
/// been retried `max_retries` times. Checks cancellation before every attempt;
/// backs off 10 ms, 20 ms, 40 ms... between attempts.
pub fn with_retries<T>(
    max_retries: u32,
    cancel: &CancelToken,
    deadline: Option<Instant>,
    retries: &AtomicU64,
    mut f: impl FnMut(u32) -> Result<T, NodeError>,
) -> Result<T, NodeError> {
    let mut attempt = 0;
    loop {
        check_cancel(cancel, deadline)?;
        match f(attempt) {
            Ok(v) => return Ok(v),
            Err(e) if e.is_retryable() && attempt < max_retries => {
                retries.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(10 << attempt.min(6)));
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The [`NodeError`] inside an engine error chain, if any.
pub fn node_error(e: &anyhow::Error) -> Option<&NodeError> {
    e.chain().find_map(|c| c.downcast_ref::<NodeError>())
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

struct ChunkEnv<'a> {
    tl: &'a Timeline,
    c: &'a Compiled,
    gpu: &'a GpuContext,
    comp: &'a Arc<Compositor>,
    cancel: &'a CancelToken,
    deadline: Option<Instant>,
    max_retries: u32,
    retries: &'a AtomicU64,
    submissions: &'a AtomicU64,
}

fn render_chunk(env: &ChunkEnv<'_>, chunk: &ChunkPlan, path: &Path) -> anyhow::Result<()> {
    let tmp = path.with_extension(format!("tmp{}.mkv", std::process::id()));
    let r = render_chunk_to(env, chunk, &tmp);
    if r.is_err() {
        std::fs::remove_file(&tmp).ok();
        return r;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn render_chunk_to(env: &ChunkEnv<'_>, chunk: &ChunkPlan, tmp: &Path) -> anyhow::Result<()> {
    let (tl, c, gpu) = (env.tl, env.c, env.gpu);
    let mut worker = WorkerState::default();
    worker.slot(compositor_slot(), || Ok(env.comp.clone()))?;
    // Frame keys include `t`, so cross-frame hits are rare (reuse across edits
    // happens at chunk level). The cache serves DAG sharing within a frame and
    // keeps finished sub-results across a retry; it's cleared after each frame
    // so its textures go back to the pool for the next one.
    let mut cache = FrameCache::new(16);
    let mut enc = ChunkEncoder::create(tmp, &encode_settings(tl))?;
    let mut ring = ReadbackRing::new(gpu, tl.output.width, tl.output.height, READBACK_DEPTH);
    let mut sink = |rows: &[u8], stride: usize| enc.push_bgra_strided(rows, stride);
    let result = (|| -> anyhow::Result<()> {
        for i in chunk.start_frame..chunk.start_frame + chunk.frames {
            let t = RationalTime::from_frames(i, tl.output.fps);
            let frame = with_retries(
                env.max_retries,
                env.cancel,
                env.deadline,
                env.retries,
                |_| {
                    let mut ctx = RenderCtx::new(gpu, &mut worker, env.cancel, env.deadline);
                    c.graph.evaluate(c.output, t, &mut ctx, &mut cache)
                },
            )
            .map_err(anyhow::Error::new)
            .with_context(|| format!("frame {i}"))?;
            let mut ctx = RenderCtx::new(gpu, &mut worker, env.cancel, env.deadline);
            ring.push(env.comp, &mut ctx, &frame, &mut sink)
                .with_context(|| format!("frame {i}: output"))?;
            cache.clear();
        }
        ring.drain(gpu, &mut sink)
    })();
    env.submissions
        .fetch_add(worker.submissions, Ordering::Relaxed);
    result?;
    enc.finish()?;
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
    let pool_before = gpu.pool_stats();
    let run_cancel = opts.cancel.child();
    let (retries, submissions) = (AtomicU64::new(0), AtomicU64::new(0));
    let env = ChunkEnv {
        tl,
        c,
        gpu,
        comp: &comp,
        cancel: &run_cancel,
        deadline: opts.deadline,
        max_retries: opts.max_retries,
        retries: &retries,
        submissions: &submissions,
    };
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
                render_chunk(&env, p, &path_of(p))
                    .with_context(|| format!("chunk {}", p.index))
                    .inspect_err(|_| run_cancel.cancel())?;
                Ok((p.index, s.elapsed().as_millis()))
            })
            .collect()
    });
    let render_wall_ms = t_render.elapsed().as_millis();
    let mut ms = vec![None; plans.len()];
    let mut errors = Vec::new();
    for r in timings {
        match r {
            Ok((i, t)) => ms[i] = Some(t),
            Err(e) => errors.push(e),
        }
    }
    // Report the root cause, not the siblings we cancelled because of it.
    if let Some(i) = errors
        .iter()
        .position(|e| node_error(e).is_none_or(|n| n.kind != ErrorKind::Cancelled))
    {
        return Err(errors.swap_remove(i));
    }
    if let Some(e) = errors.into_iter().next() {
        return Err(e);
    }
    let pool_after = gpu.pool_stats();

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
        retries: retries.load(Ordering::Relaxed),
        gpu_submissions: submissions.load(Ordering::Relaxed),
        textures_allocated: pool_after.allocated - pool_before.allocated,
        textures_reused: pool_after.reused - pool_before.reused,
        final_blake3: file_blake3(out)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        max: u32,
        cancel: &CancelToken,
        deadline: Option<Instant>,
        fail: impl Fn(u32) -> Option<NodeError>,
    ) -> (Result<u32, NodeError>, u32, u64) {
        let retries = AtomicU64::new(0);
        let mut calls = 0;
        let r = with_retries(max, cancel, deadline, &retries, |attempt| {
            calls += 1;
            match fail(attempt) {
                Some(e) => Err(e),
                None => Ok(attempt),
            }
        });
        (r, calls, retries.load(Ordering::Relaxed))
    }

    #[test]
    fn retryable_is_retried_until_success() {
        let c = CancelToken::new();
        let (r, calls, retries) = run(2, &c, None, |a| {
            (a < 2).then(|| NodeError::retryable("busy"))
        });
        assert_eq!((r.unwrap(), calls, retries), (2, 3, 2));
    }

    #[test]
    fn retryable_gives_up_after_bound() {
        let c = CancelToken::new();
        let (r, calls, retries) = run(2, &c, None, |_| Some(NodeError::retryable("busy")));
        assert_eq!(r.unwrap_err().kind, ErrorKind::Retryable);
        assert_eq!((calls, retries), (3, 2));
    }

    #[test]
    fn permanent_and_cancelled_are_never_retried() {
        let c = CancelToken::new();
        let (r, calls, _) = run(5, &c, None, |_| Some(NodeError::permanent("bad params")));
        assert_eq!((r.unwrap_err().kind, calls), (ErrorKind::Permanent, 1));
        let (r, calls, _) = run(5, &c, None, |_| Some(NodeError::cancelled("stop")));
        assert_eq!((r.unwrap_err().kind, calls), (ErrorKind::Cancelled, 1));
        // NodeError::new is Permanent.
        assert_eq!(NodeError::new("x").kind, ErrorKind::Permanent);
    }

    #[test]
    fn cancellation_and_deadline_stop_before_the_attempt() {
        let c = CancelToken::new();
        c.cancel();
        let (r, calls, _) = run(2, &c, None, |_| None);
        assert_eq!((r.unwrap_err().kind, calls), (ErrorKind::Cancelled, 0));
        let c = CancelToken::new();
        let (r, calls, _) = run(2, &c, Some(Instant::now()), |_| None);
        assert_eq!((r.unwrap_err().kind, calls), (ErrorKind::Cancelled, 0));
        // A cancel that lands during backoff stops further retries.
        let c = CancelToken::new();
        let c2 = c.clone();
        let (r, calls, _) = run(5, &c, None, move |_| {
            c2.cancel();
            Some(NodeError::retryable("busy"))
        });
        assert_eq!((r.unwrap_err().kind, calls), (ErrorKind::Cancelled, 1));
    }
}
