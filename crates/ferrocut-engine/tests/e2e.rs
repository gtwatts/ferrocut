//! End-to-end: synthesize sources, render on the GPU twice, check determinism,
//! then edit one clip and check only the affected chunks re-render.
//! Skips (passes with a note) when no GPU adapter is available.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ferrocut_core::{
    AdapterPreference, ErrorKind, Frame, GpuContext, NodeError, NodeHash, Pull, Rational,
    RationalTime, RenderCtx, RenderNode,
};
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
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            return;
        }
    };
    eprintln!("adapter: {}", gpu.describe());
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
        Ok(g) => g,
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
