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
//!
//! GPU faults: each frame runs inside a [`GpuContext::error_scope`], so wgpu
//! out-of-memory surfaces as a `Retryable` [`GpuFault::OutOfMemory`] error (the
//! pool is trimmed and the frame retried) instead of panicking. A device-lost
//! fault (reported by a node via `NodeError::from_gpu`/`device_lost`, or seen
//! through wgpu's device-lost callback) is not retried per frame: the chunk is
//! abandoned, the shared [`SharedGpu`] recreates the device, queue and texture
//! pool on the same adapter, and the chunk re-renders from its first frame on
//! a fresh worker, at most `max_chunk_restarts` times. Chunks finished on the
//! old device are only committed if it was still alive at the end of the chunk.
//!
//! Sequential nodes: if any node feeding the output is
//! [`AccessPattern::Sequential`], the chunks to render are split into at most
//! `jobs` contiguous runs, one per worker, each rendered in increasing time
//! order on one persistent [`WorkerState`]. A sequential node then pre-rolls
//! once per worker instead of once per chunk, and the graph resets it if a
//! retry ever asks for an earlier time.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use ferrocut_core::{
    AccessPattern, CancelToken, ErrorKind, FrameKey, GpuContext, GpuFault, NodeError, RationalTime,
    RenderCtx, SharedGpu, WorkerState,
};
use rayon::prelude::*;
use serde::Serialize;

use crate::compile::Compiled;
use crate::compositor::{Compositor, ReadbackRing, compositor_slot};
use crate::graph::FrameCache;
use crate::media::concat::concat;
use crate::media::encode::{ChunkEncoder, EncodeSettings};
use crate::timeline::Timeline;

/// Part of every chunk key (with [`ferrocut_colorspace::VERSION`]): bump when
/// the engine's output for the same inputs changes.
/// v2: color transforms from `ferrocut-colorspace` (OCIO 2.5 matrices).
pub const ENGINE_VERSION: &str =
    concat!("ferrocut-engine ", env!("CARGO_PKG_VERSION"), " render.v2");

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
    /// Some node feeding the output is `Sequential`: contiguous chunk runs per worker.
    pub sequential: bool,
    /// Worker tasks: one per chunk, or one contiguous run per worker if sequential.
    pub worker_tasks: usize,
    /// Backward seeks that reset a sequential node.
    pub sequential_resets: u64,
    /// Chunks restarted from their first frame after a GPU fault.
    pub chunk_restarts: u64,
    /// GPU contexts recreated after device loss.
    pub gpu_recreations: u64,
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
    /// Restarts per chunk after a GPU fault (device lost: on a recreated device).
    pub max_chunk_restarts: u32,
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
            max_chunk_restarts: 2,
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
/// backs off 10 ms, 20 ms, 40 ms... between attempts. Device-lost errors are
/// returned at once: retrying on the dead device is pointless; the chunk
/// scheduler recreates the device instead.
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
            Err(e) if e.is_retryable() && !e.is_device_lost() && attempt < max_retries => {
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
        h.update(b"ferrocut.chunk.v1\0");
        h.update(ENGINE_VERSION.as_bytes());
        h.update(ferrocut_colorspace::VERSION.as_bytes());
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

/// Split `chunks` (in order) into at most `n` contiguous runs of roughly equal
/// frame counts.
pub fn contiguous_runs<'a>(chunks: &[&'a ChunkPlan], n: usize) -> Vec<Vec<&'a ChunkPlan>> {
    let n = n.clamp(1, chunks.len().max(1));
    let total: i64 = chunks.iter().map(|c| c.frames).sum();
    let mut runs: Vec<Vec<&ChunkPlan>> = vec![Vec::new(); n];
    let mut before = 0i64;
    for c in chunks {
        // Run of the chunk's midpoint; monotonic in `before`, hence contiguous.
        let g = if total > 0 {
            ((2 * before + c.frames) as i128 * n as i128 / (2 * total) as i128) as usize
        } else {
            0
        };
        runs[g.min(n - 1)].push(c);
        before += c.frames;
    }
    runs.retain(|r| !r.is_empty());
    runs
}

/// The compositor for the current device (rebuilt after device recreation).
#[derive(Default)]
struct CompositorCache(Mutex<Option<(u64, Arc<Compositor>)>>);

impl CompositorCache {
    fn get(&self, gpu: &GpuContext) -> Arc<Compositor> {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((id, c)) = m.as_ref()
            && *id == gpu.id()
        {
            return c.clone();
        }
        let c = Arc::new(Compositor::new(gpu));
        *m = Some((gpu.id(), c.clone()));
        c
    }
}

struct ChunkEnv<'a> {
    tl: &'a Timeline,
    c: &'a Compiled,
    gpu: &'a SharedGpu,
    comps: &'a CompositorCache,
    cancel: &'a CancelToken,
    deadline: Option<Instant>,
    max_retries: u32,
    max_chunk_restarts: u32,
    retries: &'a AtomicU64,
    submissions: &'a AtomicU64,
    resets: &'a AtomicU64,
    restarts: &'a AtomicU64,
}

/// One render worker: a device snapshot plus its per-worker node state.
struct Worker {
    gpu: Arc<GpuContext>,
    comp: Arc<Compositor>,
    state: WorkerState,
}

impl Worker {
    fn new(env: &ChunkEnv<'_>, gpu: Arc<GpuContext>) -> anyhow::Result<Worker> {
        let comp = env.comps.get(&gpu);
        let mut state = WorkerState::default();
        state.slot(compositor_slot(), || Ok(comp.clone()))?;
        Ok(Worker { gpu, comp, state })
    }
}

/// The GPU fault behind a failed chunk, if any.
fn gpu_fault(e: &anyhow::Error, gpu: &GpuContext) -> Option<GpuFault> {
    if gpu.is_lost() {
        return Some(GpuFault::DeviceLost);
    }
    node_error(e).and_then(|n| n.gpu_fault)
}

/// Render `chunks` in order on one worker (one chunk, or a sequential run).
fn render_run(
    env: &ChunkEnv<'_>,
    chunks: &[&ChunkPlan],
    path_of: &(dyn Fn(&ChunkPlan) -> PathBuf + Sync),
) -> anyhow::Result<Vec<(usize, u128)>> {
    let mut w = Worker::new(env, env.gpu.get())?;
    let mut out = Vec::with_capacity(chunks.len());
    for p in chunks {
        let s = Instant::now();
        let mut restarts = 0;
        loop {
            let (sub0, res0) = (w.state.submissions, w.state.sequential_resets);
            let r = render_chunk(env, &mut w, p, &path_of(p));
            env.submissions
                .fetch_add(w.state.submissions - sub0, Ordering::Relaxed);
            env.resets
                .fetch_add(w.state.sequential_resets - res0, Ordering::Relaxed);
            let Err(e) = r else { break };
            match gpu_fault(&e, &w.gpu) {
                Some(fault)
                    if restarts < env.max_chunk_restarts
                        && check_cancel(env.cancel, env.deadline).is_ok() =>
                {
                    restarts += 1;
                    env.restarts.fetch_add(1, Ordering::Relaxed);
                    let gpu = match fault {
                        GpuFault::DeviceLost => env
                            .gpu
                            .recover(&w.gpu)
                            .map_err(NodeError::from)
                            .with_context(|| {
                                format!("chunk {}: recreating the GPU device", p.index)
                            })?,
                        GpuFault::OutOfMemory => {
                            w.gpu.trim_pool();
                            w.gpu.clone()
                        }
                    };
                    // Fresh worker state: nothing from the failed attempt (or the
                    // old device) leaks into the retry; sequential nodes pre-roll.
                    w = Worker::new(env, gpu)?;
                }
                _ => return Err(e.context(format!("chunk {}", p.index))),
            }
        }
        out.push((p.index, s.elapsed().as_millis()));
    }
    Ok(out)
}

fn render_chunk(
    env: &ChunkEnv<'_>,
    w: &mut Worker,
    chunk: &ChunkPlan,
    path: &Path,
) -> anyhow::Result<()> {
    let tmp = path.with_extension(format!("tmp{}.mkv", std::process::id()));
    let r = render_chunk_to(env, w, chunk, &tmp);
    if r.is_err() {
        std::fs::remove_file(&tmp).ok();
        return r;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn render_chunk_to(
    env: &ChunkEnv<'_>,
    w: &mut Worker,
    chunk: &ChunkPlan,
    tmp: &Path,
) -> anyhow::Result<()> {
    let (tl, c) = (env.tl, env.c);
    let gpu = w.gpu.clone();
    let gpu: &GpuContext = &gpu;
    // Frame keys include `t`, so cross-frame hits are rare (reuse across edits
    // happens at chunk level). The cache serves DAG sharing within a frame and
    // keeps finished sub-results across a retry; it's cleared after each frame
    // so its textures go back to the pool for the next one.
    let mut cache = FrameCache::new(16);
    let mut enc = ChunkEncoder::create(tmp, &encode_settings(tl))?;
    let mut ring = ReadbackRing::new(gpu, tl.output.width, tl.output.height, READBACK_DEPTH);
    let mut sink = |rows: &[u8], stride: usize| enc.push_bgra_strided(rows, stride);
    for i in chunk.start_frame..chunk.start_frame + chunk.frames {
        let t = RationalTime::from_frames(i, tl.output.fps);
        let frame = with_retries(
            env.max_retries,
            env.cancel,
            env.deadline,
            env.retries,
            |_| {
                let r = gpu.scoped(|| {
                    let mut ctx = RenderCtx::new(gpu, &mut w.state, env.cancel, env.deadline);
                    c.graph.evaluate(c.output, t, &mut ctx, &mut cache)
                });
                if r.as_ref().is_err_and(NodeError::is_gpu_out_of_memory) {
                    cache.clear();
                    gpu.trim_pool();
                }
                r
            },
        )
        .map_err(anyhow::Error::new)
        .with_context(|| format!("frame {i}"))?;
        let scope = gpu.error_scope();
        let pushed = {
            let mut ctx = RenderCtx::new(gpu, &mut w.state, env.cancel, env.deadline);
            ring.push(&w.comp, &mut ctx, &frame, &mut sink)
        };
        if let Some(e) = scope.finish() {
            return Err(anyhow::Error::new(e).context(format!("frame {i}: output")));
        }
        pushed.with_context(|| format!("frame {i}: output"))?;
        cache.clear();
    }
    let scope = gpu.error_scope();
    let drained = ring.drain(gpu, &mut sink);
    if let Some(e) = scope.finish() {
        return Err(anyhow::Error::new(e).context("output"));
    }
    drained?;
    // Never commit a chunk whose frames may come from a dying device.
    gpu.check_lost().map_err(anyhow::Error::new)?;
    enc.finish()?;
    Ok(())
}

/// Render `tl` to `out`. `gpu` is the shared context; after device loss it
/// holds the recreated one (see the module docs).
pub fn render(
    tl: &Timeline,
    c: &Compiled,
    gpu: &SharedGpu,
    out: &Path,
    opts: &RenderOptions,
) -> anyhow::Result<RenderReport> {
    let t0 = Instant::now();
    let chunk_dir = opts.cache_dir.join("chunks");
    std::fs::create_dir_all(&chunk_dir)
        .with_context(|| format!("creating {}", chunk_dir.display()))?;
    let plans = plan(tl, c);
    let gpu0 = gpu.get();
    let pool_before = gpu0.pool_stats();
    let recreations_before = gpu.recreations();
    let comps = CompositorCache::default();
    let run_cancel = opts.cancel.child();
    let (retries, submissions) = (AtomicU64::new(0), AtomicU64::new(0));
    let (resets, restarts) = (AtomicU64::new(0), AtomicU64::new(0));
    let env = ChunkEnv {
        tl,
        c,
        gpu,
        comps: &comps,
        cancel: &run_cancel,
        deadline: opts.deadline,
        max_retries: opts.max_retries,
        max_chunk_restarts: opts.max_chunk_restarts,
        retries: &retries,
        submissions: &submissions,
        resets: &resets,
        restarts: &restarts,
    };
    let path_of = |p: &ChunkPlan| chunk_dir.join(format!("{}.mkv", p.key));

    let todo: Vec<&ChunkPlan> = plans
        .iter()
        .filter(|p| opts.force || !path_of(p).exists())
        .collect();
    let sequential = c.graph.access_pattern(c.output) == AccessPattern::Sequential;
    let runs: Vec<Vec<&ChunkPlan>> = if sequential {
        contiguous_runs(&todo, opts.jobs)
    } else {
        todo.iter().map(|p| vec![*p]).collect()
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.jobs.max(1))
        .build()?;
    let t_render = Instant::now();
    let timings: Vec<anyhow::Result<Vec<(usize, u128)>>> = pool.install(|| {
        runs.par_iter()
            .map(|run| render_run(&env, run, &path_of).inspect_err(|_| run_cancel.cancel()))
            .collect()
    });
    let render_wall_ms = t_render.elapsed().as_millis();
    let mut ms = vec![None; plans.len()];
    let mut errors = Vec::new();
    for r in timings {
        match r {
            Ok(v) => v.into_iter().for_each(|(i, t)| ms[i] = Some(t)),
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
    let gpu_end = gpu.get();
    // Pool counters of the context that finished the render (a recreated
    // device starts a fresh pool).
    let (tex_alloc, tex_reused) = {
        let a = gpu_end.pool_stats();
        if gpu_end.id() == gpu0.id() {
            (
                a.allocated - pool_before.allocated,
                a.reused - pool_before.reused,
            )
        } else {
            (a.allocated, a.reused)
        }
    };

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
        adapter: gpu_end.describe(),
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
        textures_allocated: tex_alloc,
        textures_reused: tex_reused,
        sequential,
        worker_tasks: runs.len(),
        sequential_resets: resets.load(Ordering::Relaxed),
        chunk_restarts: restarts.load(Ordering::Relaxed),
        gpu_recreations: gpu.recreations() - recreations_before,
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
    fn device_lost_is_not_retried_per_frame() {
        let c = CancelToken::new();
        let (r, calls, retries) = run(5, &c, None, |_| Some(NodeError::device_lost("gone")));
        assert!(r.unwrap_err().is_device_lost());
        assert_eq!((calls, retries), (1, 0));
        // Out of memory is retried like any Retryable error.
        let (r, calls, _) = run(2, &c, None, |a| {
            (a == 0).then(|| NodeError::gpu_out_of_memory("alloc"))
        });
        assert_eq!((r.unwrap(), calls), (1, 2));
    }

    fn plans(frames: &[i64]) -> Vec<ChunkPlan> {
        let mut start = 0;
        frames
            .iter()
            .enumerate()
            .map(|(index, &f)| {
                let p = ChunkPlan {
                    index,
                    start_frame: start,
                    frames: f,
                    key: String::new(),
                };
                start += f;
                p
            })
            .collect()
    }

    fn idx(runs: &[Vec<&ChunkPlan>]) -> Vec<Vec<usize>> {
        runs.iter()
            .map(|r| r.iter().map(|p| p.index).collect())
            .collect()
    }

    #[test]
    fn contiguous_runs_are_ordered_and_balanced() {
        let p = plans(&[12; 6]);
        let all: Vec<&ChunkPlan> = p.iter().collect();
        assert_eq!(
            idx(&contiguous_runs(&all, 3)),
            vec![vec![0, 1], vec![2, 3], vec![4, 5]]
        );
        assert_eq!(idx(&contiguous_runs(&all, 1)), vec![vec![0, 1, 2, 3, 4, 5]]);
        // More workers than chunks: one chunk each.
        assert_eq!(idx(&contiguous_runs(&all, 12)).len(), 6);
        // Gaps (reused chunks) and a short tail chunk keep order.
        let some = vec![&p[0], &p[2], &p[3], &p[5]];
        let r = contiguous_runs(&some, 2);
        assert_eq!(idx(&r), vec![vec![0, 2], vec![3, 5]]);
        let q = plans(&[12, 12, 12, 2]);
        let all: Vec<&ChunkPlan> = q.iter().collect();
        let r = idx(&contiguous_runs(&all, 2));
        assert_eq!(r.concat(), vec![0, 1, 2, 3]);
        assert!(contiguous_runs(&[], 4).is_empty());
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
