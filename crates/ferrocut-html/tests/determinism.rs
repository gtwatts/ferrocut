//! Determinism, isolation and core integration of the HTML layer. Each test
//! starts its own Chromium host(s); `render_node_through_core_on_gpu` skips
//! without a GPU adapter.
#![cfg(not(ferrocut_html_no_host))]

use std::path::PathBuf;
use std::time::Duration;

use ferrocut_core::{
    AdapterPreference, CancelToken, ErrorKind, GpuContext, Rational, RationalTime, RenderCtx, RenderNode, WorkerState,
};
use ferrocut_html::{HostConfig, HtmlNode, HtmlParams, HtmlSession, HtmlSource, OutputEncoding};

const W: u32 = 320;
const H: u32 = 320;
const FPS: i64 = 30;

fn page() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/anim.html")
}

fn node() -> HtmlNode {
    let params = HtmlParams { width: W, height: H, fps: Rational::new(FPS, 1), encoding: OutputEncoding::AcesCg };
    HtmlNode::new(HtmlSource::File(page()), params).unwrap()
}

fn fnv64(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &c| (h ^ c as u64).wrapping_mul(0x100_0000_01b3))
}

/// Raw BGRA8 frames `ks` (in order) from one session.
fn frames(s: &mut HtmlSession, ks: impl IntoIterator<Item = i64>) -> Vec<Vec<u8>> {
    ks.into_iter()
        .map(|k| {
            let mut out = Vec::new();
            s.render_frame(k, &|| false, &mut out).unwrap_or_else(|e| panic!("frame {k}: {e}"));
            assert_eq!(out.len(), (W * H * 4) as usize);
            out
        })
        .collect()
}

fn hashes(f: &[Vec<u8>]) -> Vec<u64> {
    f.iter().map(|b| fnv64(b)).collect()
}

/// Max alpha in a pixel rectangle (rows y0..y1, cols x0..x1).
fn max_alpha(f: &[u8], (x0, y0, x1, y1): (u32, u32, u32, u32)) -> u8 {
    let mut m = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            m = m.max(f[((y * W + x) * 4 + 3) as usize]);
        }
    }
    m
}

fn region(f: &[u8], y0: u32, y1: u32) -> &[u8] {
    &f[(y0 * W * 4) as usize..(y1 * W * 4) as usize]
}

const N: i64 = 45; // 1.5 s at 30 fps

#[test]
fn two_runs_render_identical_frames() {
    let n = node();
    let a = hashes(&frames(&mut n.new_session(), 0..N));
    let b = hashes(&frames(&mut n.new_session(), 0..N));
    assert_eq!(a, b, "two independent hosts disagree");
    let mut distinct = a.clone();
    distinct.sort();
    distinct.dedup();
    eprintln!("{N} frames byte-identical across 2 hosts; {} distinct; frame 0 fnv64={:016x}, frame {} fnv64={:016x}", distinct.len(), a[0], N - 1, a[N as usize - 1]);
    assert!(distinct.len() as i64 >= N - 2, "page should animate every frame ({} distinct of {N})", distinct.len());
}

#[test]
fn chunk_boundary_matches_sequential() {
    let n = node();
    let seq = hashes(&frames(&mut n.new_session(), 0..N));
    // Chunk 2 rendered by a fresh host that starts at frame 18 (a chunk boundary).
    let mid = hashes(&frames(&mut n.new_session(), 18..N));
    assert_eq!(&seq[18..], &mid[..], "starting mid-sequence changed frames");
    // Chunks rendered in reverse order on one session (backward seeks respawn).
    let mut s = n.new_session();
    let mut rev = hashes(&frames(&mut s, 30..N));
    rev.splice(0..0, hashes(&frames(&mut s, 15..30)));
    rev.splice(0..0, hashes(&frames(&mut s, 0..15)));
    assert_eq!(seq, rev, "reverse-order chunks changed frames");
    assert_eq!(s.spawns(), 3, "each backward seek should replay in a fresh host");
    eprintln!("frames 18..{N} from a mid-sequence start and 3 reverse-order chunks match the sequential render");
}

#[test]
fn every_clock_follows_graph_time() {
    let f = frames(&mut node().new_session(), 0..=24);
    // CSS animation row (#css, rows 10..50) moves every frame.
    assert_ne!(region(&f[0], 10, 50), region(&f[1], 10, 50), "CSS animation did not advance");
    // requestAnimationFrame row (#raf, rows 130..170) moves every frame.
    assert_ne!(region(&f[0], 130, 170), region(&f[1], 130, 170), "rAF did not advance");
    // setInterval(100ms) row (#timer, rows 70..110) changes only on 100 ms ticks.
    assert_ne!(region(&f[0], 70, 110), region(&f[6], 70, 110), "setInterval did not fire");
    // Date.now() bar (#clock, rows 190..210) grows.
    assert_ne!(region(&f[0], 190, 210), region(&f[12], 190, 210), "Date.now did not advance");
    // setTimeout(500ms) starts #late's animation: invisible at 466 ms, visible by 800 ms.
    let late = (140, 240, 180, 280);
    assert_eq!(max_alpha(&f[14], late), 0, "setTimeout fired early");
    assert!(max_alpha(&f[24], late) > 200, "setTimeout-triggered CSS animation never showed");
    // Transparent page background stays transparent.
    assert_eq!(max_alpha(&f[10], (100, 290, 180, 318)), 0, "background should be transparent");
}

#[test]
fn host_crash_is_a_clean_retryable_node_error_and_recovers() {
    let n = node();
    let reference = frames(&mut n.new_session(), [0, 40]);
    let mut s = n.new_session();
    frames(&mut s, [0]);
    let pid = s.host_pid().expect("host running");
    // SIGKILL the host while it pre-rolls a long way (simulates a crash / OOM kill).
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
    });
    let cancel = CancelToken::new();
    let err = n
        .render_cpu(&mut s, RationalTime::new(600, 1), &cancel, None)
        .expect_err("render across a host kill must fail");
    killer.join().unwrap();
    eprintln!("host kill -> {err}");
    assert_eq!(err.kind, ErrorKind::Retryable, "{err}");
    assert!(err.message.contains("html host died"), "{err}");
    // The next render transparently starts a new host and is still exact.
    let after = frames(&mut s, [40]);
    assert_eq!(fnv64(&after[0]), fnv64(&reference[1]), "frame after recovery differs");
    assert_eq!(s.spawns(), 2);
}

#[test]
fn missing_host_or_bad_page_are_permanent() {
    let cfg = HostConfig { host_exe: "/nonexistent/ferrocut-html-host".into(), ..HostConfig::default() };
    let n = node().with_host_config(cfg);
    let err = n.render_cpu(&mut n.new_session(), RationalTime::new(0, 1), &CancelToken::new(), None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Permanent, "{err}");

    let params = HtmlParams { width: 64, height: 64, fps: Rational::new(30, 1), encoding: OutputEncoding::AcesCg };
    let missing = HtmlNode::new(HtmlSource::Url("file:///nonexistent/ferrocut.html".into()), params).unwrap();
    let err = missing.render_cpu(&mut missing.new_session(), RationalTime::new(0, 1), &CancelToken::new(), None).unwrap_err();
    eprintln!("missing page -> {err}");
    assert_eq!(err.kind, ErrorKind::Permanent, "{err}");

    assert!(HtmlNode::new(HtmlSource::File("/nonexistent.html".into()), params).is_err());
}

#[test]
fn render_node_through_core_on_gpu() {
    let gpu = match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("skipping: no GPU adapter ({e})");
            return;
        }
    };
    let n = node();
    let t = RationalTime::new(1, 2); // frame 15
    let mut worker = WorkerState::default();
    let cancel = CancelToken::new();
    let out = {
        let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
        n.render(&mut ctx, t, &[]).unwrap()
    };
    assert!(out.gpu().is_some(), "node should hand the engine a GPU frame");
    let back = out.to_cpu_frame(&gpu).unwrap();
    let direct = n.render_cpu(&mut n.new_session(), t, &CancelToken::new(), None).unwrap();
    let bits = |p: &[half::f16]| p.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&back.image.pixels), bits(&direct.image.pixels), "GPU upload round trip changed bytes");
    assert_eq!(back.color_space.name(), "ACEScg");

    cancel.cancel();
    let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
    assert_eq!(n.render(&mut ctx, t, &[]).err().map(|e| e.kind), Some(ErrorKind::Cancelled));
}
