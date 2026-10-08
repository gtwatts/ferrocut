//! End-to-end: synthesize sources, render on the GPU twice, check determinism,
//! then edit one clip and check only the affected chunks re-render.
//! Skips (passes with a note) when no GPU adapter is available.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ferrocut_core::{
    AccessPattern, AdapterPreference, ErrorKind, Frame, GpuContext, NodeError, NodeHash, Pull,
    Rational, RationalTime, RenderCtx, RenderNode, SharedGpu, WorkerState,
};
use ferrocut_engine::compositor::{Compositor, compositor_slot};
use ferrocut_engine::graph::FrameCache;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::render::{ChunkStatus, RenderOptions, node_error};
use ferrocut_engine::{Timeline, compile, render};

const W: u32 = 160;
const H: u32 = 96;

fn synth(path: &Path, frames: i64, seed: u8) {
    let s = EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..frames {
        let mut px = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                px[i] = (x as i64 * 2 + f * 3) as u8 ^ seed;
                px[i + 1] = (y as i64 * 2 + f) as u8;
                px[i + 2] = seed.wrapping_mul(37).wrapping_add((x + y) as u8);
                px[i + 3] = 255;
            }
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

fn timeline(dir: &Path, opacity: &str) -> Timeline {
    let json = format!(
        r#"{{
      "output": {{ "width": {W}, "height": {H}, "fps": "24", "gop": 12 }},
      "tracks": [
        {{ "clips": [
          {{ "id": "a", "source": "a.mkv", "start": 0, "duration": "2" }},
          {{ "id": "b", "source": "b.mkv", "start": "3/2", "source_in": "1/4", "duration": "3/2",
             "transition_in": {{ "kind": "dissolve", "duration": "1/2" }} }}
        ]}},
        {{ "clips": [ {{ "id": "t", "source": "c.mkv", "start": "5/2", "duration": "1/4", "opacity": "{opacity}" }} ] }}
      ]
    }}"#
    );
    let p = dir.join(format!("tl-{}.json", opacity.replace('/', "_")));
    std::fs::write(&p, json).unwrap();
    Timeline::load(&p).unwrap()
}

#[test]
fn deterministic_and_incremental() {
    let gpu = match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => SharedGpu::new(g),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            return;
        }
    };
    eprintln!("adapter: {}", gpu.get().describe());
    let dir = tempfile::tempdir().unwrap();
    synth(&dir.path().join("a.mkv"), 60, 1);
    synth(&dir.path().join("b.mkv"), 60, 2);
    synth(&dir.path().join("c.mkv"), 30, 3);

    let tl = timeline(dir.path(), "1/2");
    let c = compile(&tl).unwrap();
    let run = |name: &str, force: bool, tl: &Timeline, c: &ferrocut_engine::Compiled| {
        let opts = RenderOptions {
            force,
            jobs: 4,
            ..RenderOptions::new(dir.path().join(format!("cache-{name}")))
        };
        render(tl, c, &gpu, &dir.path().join(format!("{name}.mkv")), &opts).unwrap()
    };
    let r1 = run("one", true, &tl, &c);
    let r2 = run("two", true, &tl, &c);
    assert_eq!(r1.total_frames, 72);
    assert_eq!(r1.chunks.len(), 6);
    let h1: Vec<_> = r1.chunks.iter().map(|c| c.file_blake3.clone()).collect();
    let h2: Vec<_> = r2.chunks.iter().map(|c| c.file_blake3.clone()).collect();
    assert_eq!(h1, h2, "per-chunk hashes differ between runs");
    assert_eq!(
        r1.final_blake3, r2.final_blake3,
        "final file differs between runs"
    );

    // Same cache dir as run "one": unchanged timeline reuses everything.
    let r3 = run("one", false, &tl, &c);
    assert!(r3.chunks.iter().all(|c| c.status == ChunkStatus::Reused));
    assert_eq!(r3.final_blake3, r1.final_blake3);

    // Edit the overlay (frames 60..66 -> chunk 5 only).
    let tl2 = timeline(dir.path(), "3/4");
    let c2 = compile(&tl2).unwrap();
    let r4 = run("one", false, &tl2, &c2);
    let rendered: Vec<usize> = r4
        .chunks
        .iter()
        .filter(|c| c.status == ChunkStatus::Rendered)
        .map(|c| c.plan.index)
        .collect();
    assert_eq!(rendered, vec![5]);
    assert_ne!(r4.final_blake3, r1.final_blake3);
    for i in 0..5 {
        assert_eq!(r4.chunks[i].file_blake3, r1.chunks[i].file_blake3);
    }
}

/// Pass-through node that fails at chosen frames, counting attempts per frame.
struct Faulty {
    fail: Box<dyn Fn(i64, u32) -> Option<NodeError> + Send + Sync>,
    attempts: Mutex<HashMap<i64, u32>>,
}

impl Faulty {
    fn new(fail: impl Fn(i64, u32) -> Option<NodeError> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Faulty {
            fail: Box::new(fail),
            attempts: Mutex::default(),
        })
    }
    fn attempts(&self, frame: i64) -> u32 {
        self.attempts
            .lock()
            .unwrap()
            .get(&frame)
            .copied()
            .unwrap_or(0)
    }
}

impl RenderNode for Faulty {
    fn kind(&self) -> &'static str {
        "test.faulty"
    }
    fn content_hash(&self) -> NodeHash {
        NodeHash::of("test.faulty", &[])
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull { input: 0, time: t }]
    }
    fn batches_gpu_work(&self) -> bool {
        true
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn render(
        &self,
        _: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        let n = t.frame_round(Rational::from_int(24));
        let attempt = {
            let mut m = self.attempts.lock().unwrap();
            let a = m.entry(n).or_default();
            *a += 1;
            *a - 1
        };
        match (self.fail)(n, attempt) {
            Some(e) => Err(e),
            None => Ok(inputs[0].clone()),
        }
    }
}

#[test]
fn retries_permanent_errors_and_cancellation() {
    let gpu = match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => SharedGpu::new(g),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    synth(&dir.path().join("a.mkv"), 60, 1);
    synth(&dir.path().join("b.mkv"), 60, 2);
    synth(&dir.path().join("c.mkv"), 30, 3);
    let tl = timeline(dir.path(), "1/2");
    let opts = |name: &str| RenderOptions {
        force: true,
        jobs: 4,
        ..RenderOptions::new(dir.path().join(format!("cache-{name}")))
    };
    let out = |name: &str| dir.path().join(format!("{name}.mkv"));
    let wrapped = |node: Arc<Faulty>| {
        let mut c = compile(&tl).unwrap();
        c.output = c.graph.add(node, vec![c.output]);
        c
    };
    let reference = render(&tl, &compile(&tl).unwrap(), &gpu, &out("ref"), &opts("ref")).unwrap();
    assert_eq!(reference.retries, 0);
    assert_eq!(reference.gpu_submissions, 72, "one submit per frame");

    // Transient failures on every 5th frame's first attempt: retried, same pixels.
    let flaky = Faulty::new(|n, attempt| {
        (n % 5 == 0 && attempt == 0).then(|| NodeError::retryable("host restarting"))
    });
    let r = render(
        &tl,
        &wrapped(flaky.clone()),
        &gpu,
        &out("flaky"),
        &opts("flaky"),
    )
    .unwrap();
    assert_eq!(r.retries, 15);
    assert_eq!(
        (flaky.attempts(0), flaky.attempts(1), flaky.attempts(70)),
        (2, 1, 2)
    );
    assert_eq!(
        r.final_blake3, reference.final_blake3,
        "retries must not change output"
    );

    // Retryable forever: bounded (1 + max_retries attempts), then the error surfaces.
    let stuck = Faulty::new(|n, _| (n == 40).then(|| NodeError::retryable("device lost")));
    let e = render(
        &tl,
        &wrapped(stuck.clone()),
        &gpu,
        &out("stuck"),
        &opts("stuck"),
    )
    .unwrap_err();
    assert_eq!(
        node_error(&e).map(|n| n.kind),
        Some(ErrorKind::Retryable),
        "{e:#}"
    );
    assert_eq!(stuck.attempts(40), 3);

    // Permanent: never retried; siblings are cancelled but the root cause is reported.
    let broken =
        Faulty::new(|n, _| (n == 30).then(|| NodeError::permanent("plugin rejects params")));
    let e = render(
        &tl,
        &wrapped(broken.clone()),
        &gpu,
        &out("broken"),
        &opts("broken"),
    )
    .unwrap_err();
    assert_eq!(
        node_error(&e).map(|n| n.kind),
        Some(ErrorKind::Permanent),
        "{e:#}"
    );
    assert_eq!(broken.attempts(30), 1);
    assert!(!out("broken").exists());

    // Cancelled before start, and a deadline already passed: no frame renders.
    let idle = Faulty::new(|_, _| None);
    let o = opts("cancel");
    o.cancel.cancel();
    let e = render(&tl, &wrapped(idle.clone()), &gpu, &out("cancel"), &o).unwrap_err();
    assert_eq!(
        node_error(&e).map(|n| n.kind),
        Some(ErrorKind::Cancelled),
        "{e:#}"
    );
    let o = RenderOptions {
        deadline: Some(Instant::now()),
        ..opts("deadline")
    };
    let e = render(&tl, &wrapped(idle.clone()), &gpu, &out("deadline"), &o).unwrap_err();
    assert_eq!(
        node_error(&e).map(|n| n.kind),
        Some(ErrorKind::Cancelled),
        "{e:#}"
    );
    assert_eq!(idle.attempts.lock().unwrap().len(), 0);
}

/// Tests that create and destroy several devices run one at a time: the dev
/// box shares its GPU with other processes, so parallel device creation can
/// fail for lack of VRAM.
static MULTI_DEVICE: Mutex<()> = Mutex::new(());

fn multi_device_lock() -> std::sync::MutexGuard<'static, ()> {
    MULTI_DEVICE.lock().unwrap_or_else(|e| e.into_inner())
}

fn gpu_or_skip() -> Option<SharedGpu> {
    match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => Some(SharedGpu::new(g)),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            None
        }
    }
}

fn frame_of(t: RationalTime) -> i64 {
    t.frame_round(Rational::from_int(24))
}

#[derive(Clone, Copy)]
enum Fault {
    /// The node reports device-lost (what a node does after a failed GPU call).
    ReportLost,
    /// The node reports out-of-memory.
    ReportOom,
    /// Really lose the device (`device.destroy()`) and carry on as if nothing
    /// happened: the scheduler must notice by itself.
    DestroyDevice,
}

/// Pass-through that injects a GPU fault at frame `at`: once, or every time.
struct GpuFaultAt {
    at: i64,
    fault: Fault,
    always: bool,
    fired: AtomicBool,
}

impl GpuFaultAt {
    fn once(at: i64, fault: Fault) -> Arc<Self> {
        Arc::new(GpuFaultAt {
            at,
            fault,
            always: false,
            fired: AtomicBool::new(false),
        })
    }
    fn always(at: i64, fault: Fault) -> Arc<Self> {
        Arc::new(GpuFaultAt {
            always: true,
            ..Arc::into_inner(Self::once(at, fault)).unwrap()
        })
    }
}

impl RenderNode for GpuFaultAt {
    fn kind(&self) -> &'static str {
        "test.gpu_fault"
    }
    fn content_hash(&self) -> NodeHash {
        NodeHash::of("test.gpu_fault", &[])
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull { input: 0, time: t }]
    }
    fn batches_gpu_work(&self) -> bool {
        true
    }
    fn supports_data_window(&self) -> bool {
        true
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        if frame_of(t) == self.at && (self.always || !self.fired.swap(true, Ordering::SeqCst)) {
            match self.fault {
                Fault::ReportLost => return Err(NodeError::device_lost("injected device loss")),
                Fault::ReportOom => return Err(NodeError::gpu_out_of_memory("injected OOM")),
                Fault::DestroyDevice => ctx.gpu.device.destroy(),
            }
        }
        Ok(inputs[0].clone())
    }
}

/// Per-worker state of [`SeqProbe`]: like a browser page, it can only move forward.
struct SeqState {
    last: i64,
}

/// Pass-through with sequential per-worker state. Fails (Permanent) when asked
/// for an earlier frame than its last one; counts pre-rolls (fresh states).
struct SeqProbe {
    pattern: AccessPattern,
    /// Also fail on forward gaps (when every chunk is rendered, a contiguous
    /// run is strictly consecutive).
    consecutive: bool,
    prerolls: AtomicU64,
}

impl SeqProbe {
    fn new(pattern: AccessPattern, consecutive: bool) -> Arc<Self> {
        Arc::new(SeqProbe {
            pattern,
            consecutive,
            prerolls: AtomicU64::new(0),
        })
    }
    fn prerolls(&self) -> u64 {
        self.prerolls.load(Ordering::SeqCst)
    }
}

impl RenderNode for SeqProbe {
    fn kind(&self) -> &'static str {
        "test.seq_probe"
    }
    fn content_hash(&self) -> NodeHash {
        NodeHash::of("test.seq_probe", &[])
    }
    fn pulls(&self, t: RationalTime) -> Vec<Pull> {
        vec![Pull { input: 0, time: t }]
    }
    fn access_pattern(&self) -> AccessPattern {
        self.pattern
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        t: RationalTime,
        inputs: &[Arc<Frame>],
    ) -> Result<Arc<Frame>, NodeError> {
        let n = frame_of(t);
        let st = ctx.worker.slot(self.content_hash(), || {
            self.prerolls.fetch_add(1, Ordering::SeqCst);
            Ok(SeqState { last: -1 })
        })?;
        if n < st.last {
            return Err(NodeError::permanent(format!(
                "backward seek {} -> {n}",
                st.last
            )));
        }
        if self.consecutive && st.last >= 0 && n != st.last + 1 {
            return Err(NodeError::permanent(format!("gap {} -> {n}", st.last)));
        }
        st.last = n;
        Ok(inputs[0].clone())
    }
}

#[test]
fn gpu_faults_are_classified_and_recovered() {
    let _one_at_a_time = multi_device_lock();
    let Some(gpu) = gpu_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    synth(&dir.path().join("a.mkv"), 60, 1);
    synth(&dir.path().join("b.mkv"), 60, 2);
    synth(&dir.path().join("c.mkv"), 30, 3);
    let tl = timeline(dir.path(), "1/2");
    let opts = |name: &str| RenderOptions {
        force: true,
        jobs: 3,
        ..RenderOptions::new(dir.path().join(format!("cache-{name}")))
    };
    let out = |name: &str| dir.path().join(format!("{name}.mkv"));
    let wrapped = |nodes: Vec<Arc<dyn RenderNode>>| {
        let mut c = compile(&tl).unwrap();
        for n in nodes {
            c.output = c.graph.add(n, vec![c.output]);
        }
        c
    };
    let reference = render(&tl, &compile(&tl).unwrap(), &gpu, &out("ref"), &opts("ref")).unwrap();
    assert_eq!(
        (reference.chunk_restarts, reference.gpu_recreations),
        (0, 0)
    );

    // Out of memory once: not retried per frame (the frame's objects may be
    // invalid); the chunk restarts with one fewer job in flight.
    let r = render(
        &tl,
        &wrapped(vec![GpuFaultAt::once(30, Fault::ReportOom)]),
        &gpu,
        &out("oom"),
        &opts("oom"),
    )
    .unwrap();
    assert_eq!((r.retries, r.chunk_restarts, r.gpu_recreations), (0, 1, 0));
    assert_eq!(r.oom_backoffs, 1);
    assert!(r.min_jobs_in_flight < 3, "{r:?}");
    assert_eq!(r.final_blake3, reference.final_blake3);

    // A node reports device-lost once: the context is recreated and the chunk re-rendered.
    let first = gpu.get();
    let r = render(
        &tl,
        &wrapped(vec![GpuFaultAt::once(30, Fault::ReportLost)]),
        &gpu,
        &out("lost"),
        &opts("lost"),
    )
    .unwrap();
    assert_eq!(r.gpu_recreations, 1);
    assert!(r.chunk_restarts >= 1);
    assert_eq!(r.retries, 0, "device loss is not retried per frame");
    assert_eq!(r.final_blake3, reference.final_blake3);
    assert!(first.is_lost(), "the replaced context is marked lost");
    assert_ne!(gpu.get().id(), first.id());
    assert!(!gpu.get().is_lost());
    drop(first);

    // A real device loss the node doesn't even report, combined with a
    // sequential node (fresh pre-roll after recovery, never a backward seek).
    let probe = SeqProbe::new(AccessPattern::Sequential, false);
    let r = render(
        &tl,
        &wrapped(vec![
            probe.clone(),
            GpuFaultAt::once(40, Fault::DestroyDevice),
        ]),
        &gpu,
        &out("destroy"),
        &opts("destroy"),
    )
    .unwrap();
    assert_eq!(r.gpu_recreations, 1, "{r:?}");
    assert!(r.chunk_restarts >= 1);
    assert_eq!(r.final_blake3, reference.final_blake3);
    assert!(probe.prerolls() > 3, "restarted workers pre-roll again");

    // Device lost on every attempt: bounded (max_chunk_restarts recreations), then it surfaces.
    let before = gpu.recreations();
    let e = render(
        &tl,
        &wrapped(vec![GpuFaultAt::always(20, Fault::ReportLost)]),
        &gpu,
        &out("dead"),
        &opts("dead"),
    )
    .unwrap_err();
    assert!(
        node_error(&e).is_some_and(NodeError::is_device_lost),
        "{e:#}"
    );
    assert_eq!(gpu.recreations() - before, 2);
    assert!(!out("dead").exists());
}

/// A VRAM budget far below what `jobs` workers need (the `FERROCUT_VRAM_BUDGET_MB`
/// knob, simulating a GPU mostly held by another process): the render backs
/// off to fewer chunks in flight and still produces the identical file; below
/// what even one worker needs it fails with a clear error, not a validation panic.
#[test]
fn out_of_vram_backs_off_jobs_then_fails_clearly() {
    let _one_at_a_time = multi_device_lock();
    let dir = tempfile::tempdir().unwrap();
    synth(&dir.path().join("a.mkv"), 60, 1);
    synth(&dir.path().join("b.mkv"), 60, 2);
    synth(&dir.path().join("c.mkv"), 30, 3);
    let tl = timeline(dir.path(), "1/2");
    let c = compile(&tl).unwrap();
    let opts = |name: &str, jobs: usize| RenderOptions {
        force: true,
        jobs,
        ..RenderOptions::new(dir.path().join(format!("cache-{name}")))
    };
    let out = |name: &str| dir.path().join(format!("{name}.mkv"));
    let fresh = || {
        GpuContext::new(AdapterPreference::default())
            .ok()
            .map(SharedGpu::new)
    };
    let Some(gpu) = fresh() else { return };

    // One worker's working set: the pool's live bytes after a -j 1 render.
    let reference = render(&tl, &c, &gpu, &out("ref"), &opts("ref", 1)).unwrap();
    let per_worker = gpu.get().pool_live_bytes();
    assert!(per_worker > 0);
    assert_eq!(reference.oom_backoffs, 0);

    // Room for about two workers, asked for six.
    drop(gpu);
    let gpu = fresh().unwrap();
    gpu.get().set_memory_budget(Some(per_worker * 5 / 2));
    let r = render(&tl, &c, &gpu, &out("tight"), &opts("tight", 6)).unwrap();
    assert!(r.oom_backoffs >= 1, "{r:?}");
    assert!(r.min_jobs_in_flight <= 2, "{r:?}");
    assert_eq!(r.gpu_recreations, 0);
    assert_eq!(r.final_blake3, reference.final_blake3, "identical output");

    // Not even one worker fits: a clear, actionable error.
    drop(gpu);
    let gpu = fresh().unwrap();
    gpu.get().set_memory_budget(Some(per_worker / 2));
    let e = render(&tl, &c, &gpu, &out("none"), &opts("none", 4)).unwrap_err();
    let msg = format!("{e:#}");
    assert!(
        msg.contains("out of GPU memory even with a single chunk in flight"),
        "{msg}"
    );
    assert!(msg.contains("--cpu"), "{msg}");
    assert!(
        node_error(&e).is_some_and(NodeError::is_gpu_out_of_memory),
        "{msg}"
    );
    assert!(!out("none").exists());
}

#[test]
fn sequential_nodes_get_contiguous_forward_runs() {
    let Some(gpu) = gpu_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    synth(&dir.path().join("a.mkv"), 60, 1);
    synth(&dir.path().join("b.mkv"), 60, 2);
    synth(&dir.path().join("c.mkv"), 30, 3);
    let tl = timeline(dir.path(), "1/2");
    let opts = |name: &str| RenderOptions {
        force: true,
        jobs: 3,
        ..RenderOptions::new(dir.path().join(format!("cache-{name}")))
    };
    let out = |name: &str| dir.path().join(format!("{name}.mkv"));
    let base = compile(&tl).unwrap();
    assert_eq!(
        base.graph.access_pattern(base.output),
        AccessPattern::Random
    );
    let reference = render(&tl, &base, &gpu, &out("ref"), &opts("ref")).unwrap();
    assert!(!reference.sequential);
    assert_eq!(reference.worker_tasks, 6, "one task per chunk");

    let wrapped = |probe: Arc<SeqProbe>| {
        let mut c = compile(&tl).unwrap();
        c.output = c.graph.add(probe, vec![c.output]);
        // Sequential-ness propagates to everything downstream.
        c.output = c.graph.add(Faulty::new(|_, _| None), vec![c.output]);
        c
    };

    // Sequential: 3 contiguous runs of 24 frames, strictly consecutive, one pre-roll each.
    let seq = SeqProbe::new(AccessPattern::Sequential, true);
    let c = wrapped(seq.clone());
    assert_eq!(c.graph.access_pattern(c.output), AccessPattern::Sequential);
    let r = render(&tl, &c, &gpu, &out("seq"), &opts("seq")).unwrap();
    assert!(r.sequential);
    assert_eq!(
        (r.worker_tasks, seq.prerolls(), r.sequential_resets),
        (3, 3, 0)
    );
    assert_eq!(r.final_blake3, reference.final_blake3);

    // The same node declared Random: one fresh worker (and pre-roll) per chunk.
    let rnd = SeqProbe::new(AccessPattern::Random, true);
    let r = render(&tl, &wrapped(rnd.clone()), &gpu, &out("rnd"), &opts("rnd")).unwrap();
    assert!(!r.sequential);
    assert_eq!((r.worker_tasks, rnd.prerolls()), (6, 6));
    assert_eq!(r.final_blake3, reference.final_blake3);

    // Backward seek on one worker: the graph resets the sequential node first...
    let g = gpu.get();
    let comp = Arc::new(Compositor::new(&g));
    let eval = |c: &ferrocut_engine::Compiled, worker: &mut WorkerState, frame: i64| {
        let cancel = ferrocut_core::CancelToken::new();
        let mut cache = FrameCache::new(4);
        let mut ctx = RenderCtx::new(&g, worker, &cancel, None);
        let t = RationalTime::from_frames(frame, Rational::from_int(24));
        let r = c
            .graph
            .evaluate(c.output, t, &mut ctx, &mut cache)
            .map(|_| ());
        ctx.flush();
        r
    };
    let seq = SeqProbe::new(AccessPattern::Sequential, false);
    let c = wrapped(seq.clone());
    let mut worker = WorkerState::default();
    worker.slot(compositor_slot(), || Ok(comp.clone())).unwrap();
    eval(&c, &mut worker, 30).unwrap();
    eval(&c, &mut worker, 31).unwrap();
    eval(&c, &mut worker, 10).unwrap();
    assert_eq!((worker.sequential_resets, seq.prerolls()), (1, 2));
    // ...while a node that (wrongly) claims Random access really would see it and fail.
    let rnd = SeqProbe::new(AccessPattern::Random, false);
    let c = wrapped(rnd.clone());
    let mut worker = WorkerState::default();
    worker.slot(compositor_slot(), || Ok(comp.clone())).unwrap();
    eval(&c, &mut worker, 30).unwrap();
    let e = eval(&c, &mut worker, 10).unwrap_err();
    assert!(e.message.contains("backward seek 30 -> 10"), "{e}");
}
