//! OpenFX host round trip, premultiplication handling and crash isolation.
#![cfg(not(cutline_ofx_no_host))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use cutline_core::{ColorSpace, CpuImage, Frame, FrameStorage, Rational, RationalTime, RenderNode};
use cutline_ofx::{HostConfig, OfxNode, OfxPluginSpec, OfxSession, PREMULTIPLIED, UNPREMULTIPLIED, bundled_plugin_dir};
use half::f16;

const W: u32 = 64;
const H: u32 = 36;
const INVERT: &str = "net.sf.openfx.invertPlugin"; // OpenFX SDK example (BSD-3-Clause)

fn cfg() -> HostConfig {
    HostConfig { plugin_paths: vec![bundled_plugin_dir()], render_timeout: Duration::from_secs(20), ..Default::default() }
}
fn spec(id: &str) -> OfxPluginSpec {
    OfxPluginSpec { plugin_id: id.into(), context: "filter".into(), params: vec![] }
}
fn rate() -> Rational {
    Rational::new(24000, 1001)
}
fn t() -> RationalTime {
    RationalTime::from_frames(17, rate())
}

/// Premultiplied test frame; alpha cycles 1, .5, .25, 0; rows differ so a flip would show.
fn pattern() -> Vec<f32> {
    let mut v = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let a = [1.0, 0.5, 0.25, 0.0][(x % 4) as usize];
            let (r, g, b) = (x as f32 / W as f32, y as f32 / H as f32, 0.3);
            v.extend([r * a, g * a, b * a, a]);
        }
    }
    v
}
fn frame(px: &[f32]) -> Frame {
    Frame {
        width: W,
        height: H,
        color_space: ColorSpace::acescg(),
        alpha: Default::default(),
        storage: FrameStorage::Cpu(Arc::new(CpuImage { pixels: px.iter().map(|&v| f16::from_f32(v)).collect() })),
    }
}
fn pixels(f: &Frame) -> Vec<f32> {
    match &f.storage {
        FrameStorage::Cpu(c) => c.pixels.iter().map(|v| v.to_f32()).collect(),
        _ => panic!("expected CPU frame"),
    }
}
fn f16q(v: f32) -> f32 {
    f16::from_f32(v).to_f32()
}

#[test]
fn sdk_invert_plugin_renders_cpu_round_trip() {
    let mut s = OfxSession::new(cfg(), spec(INVERT));
    let src = pattern();
    let start = Instant::now();
    let (out, reply) = s.render_rgba_f32(t(), rate(), W, H, &src, PREMULTIPLIED).expect("render");
    eprintln!("Invert {W}x{H} via host pid {:?}: {:?} in {:?} (incl. host start)", s.host_pid(), reply, start.elapsed());
    assert_eq!(reply.how, "rendered");
    assert_eq!(reply.output_premult, PREMULTIPLIED, "declared source premult flows to the output clip");
    let max = out.iter().zip(&src).map(|(o, i)| (o - (1.0 - i)).abs()).fold(0f32, f32::max);
    eprintln!("Invert max |out - (1 - in)| = {max:e}");
    assert!(max == 0.0, "SDK Invert must compute 1 - x exactly in float");
    // second render reuses the same process
    let pid = s.host_pid();
    s.render_rgba_f32(t(), rate(), W, H, &src, PREMULTIPLIED).unwrap();
    assert_eq!(pid, s.host_pid());
}

#[test]
fn render_node_contract_and_hash() {
    let a = OfxNode::new(cfg(), spec(INVERT), rate()).unwrap();
    let b = OfxNode::new(cfg(), spec(INVERT), rate()).unwrap();
    let c = OfxNode::new(cfg(), spec("org.cutline.test.Unpremult"), rate()).unwrap();
    assert_eq!(a.content_hash(), b.content_hash());
    assert_ne!(a.content_hash(), c.content_hash());
    assert_eq!(a.pulls(t())[0].time, t());
    eprintln!("node label {:?}, hash {}", a.label(), a.content_hash());
    let mut s = a.session();
    let out = a.render_with(&mut s, None, t(), &frame(&pattern())).unwrap();
    let src = pattern();
    for (o, i) in pixels(&out).iter().zip(&src) {
        assert_eq!(*o, f16q(1.0 - f16q(*i)));
    }
}

#[test]
fn unpremultiplied_output_clip_is_honoured() {
    // The plugin declares kOfxImageUnPreMultiplied on its output clip and writes straight alpha.
    let mut s = OfxSession::new(cfg(), spec("org.cutline.test.Unpremult"));
    let src = pattern();
    let (raw, reply) = s.render_rgba_f32(t(), rate(), W, H, &src, PREMULTIPLIED).unwrap();
    assert_eq!(reply.output_premult, UNPREMULTIPLIED);
    // raw buffer really is straight alpha: rgb = premult / a
    let p = &raw[4..4 * 2]; // pixel x=1, alpha 0.5
    assert!((p[0] - src[4] / 0.5).abs() < 1e-6 && p[3] == 0.5);

    // The node re-premultiplies, so the frame that leaves the node equals the input.
    let node = OfxNode::new(cfg(), spec("org.cutline.test.Unpremult"), rate()).unwrap();
    let mut s2 = node.session();
    let out = pixels(&node.render_with(&mut s2, None, t(), &frame(&src)).unwrap());
    let max = out.iter().zip(&src).map(|(o, i)| (o - f16q(*i)).abs()).fold(0f32, f32::max);
    eprintln!("unpremult->repremult round trip max error {max:e} (f16 frames)");
    assert!(max <= 1e-3);
}

fn expect_node_error(id: &str, cfg: HostConfig, want: &str) -> String {
    let node = OfxNode::new(cfg, spec(id), rate()).unwrap();
    let mut s = node.session();
    let start = Instant::now();
    let err = node.render_with(&mut s, None, t(), &frame(&pattern())).expect_err("plugin must fail");
    let msg = err.to_string();
    eprintln!("{id}: NodeError after {:?}: {msg}", start.elapsed());
    assert!(msg.contains(want), "error should mention {want}: {msg}");
    // The test process is obviously still alive; the failed host is gone, and the
    // next render on the same session starts a fresh host (and fails the same way).
    let err2 = node.render_with(&mut s, None, t(), &frame(&pattern())).expect_err("still failing");
    assert!(err2.to_string().contains(want));
    assert_eq!(s.spawns, 2, "each failure costs exactly one host process");
    msg
}

#[test]
fn segfaulting_plugin_is_a_clean_node_error() {
    expect_node_error("org.cutline.test.Crash", cfg(), "SIGSEGV");
    // ...and a healthy node keeps working in the same process afterwards.
    let mut s = OfxSession::new(cfg(), spec(INVERT));
    s.render_rgba_f32(t(), rate(), W, H, &pattern(), PREMULTIPLIED).unwrap();
}

#[test]
fn aborting_plugin_is_a_clean_node_error() {
    expect_node_error("org.cutline.test.Abort", cfg(), "SIGABRT");
}

#[test]
fn hung_plugin_times_out_and_host_is_killed() {
    let mut c = cfg();
    c.render_timeout = Duration::from_millis(1500);
    let msg = expect_node_error("org.cutline.test.Hang", c, "timed out");
    assert!(msg.contains("killed"));
}

#[test]
fn externally_killed_host_recovers_on_next_render() {
    let mut s = OfxSession::new(cfg(), spec(INVERT));
    s.render_rgba_f32(t(), rate(), W, H, &pattern(), PREMULTIPLIED).unwrap();
    let pid = s.host_pid().unwrap();
    // Simulate the OOM killer.
    std::process::Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let r = s.render_rgba_f32(t(), rate(), W, H, &pattern(), PREMULTIPLIED);
    // Either the render notices the dead host (clean error) or it had already been
    // detected and a fresh host rendered; both are fine, a panic/hang is not.
    match r {
        Err(e) => {
            eprintln!("render after SIGKILL: {e}");
            assert!(e.to_string().contains("SIGKILL"));
            s.render_rgba_f32(t(), rate(), W, H, &pattern(), PREMULTIPLIED).expect("fresh host renders");
        }
        Ok(_) => eprintln!("dead host detected before RENDER; restarted transparently"),
    }
    assert_ne!(s.host_pid(), Some(pid));
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(), "old host reaped");
}

#[test]
fn leaky_plugin_host_is_recycled() {
    let mut c = cfg();
    c.max_rss_bytes = Some(200 << 20);
    let mut s = OfxSession::new(c, spec("org.cutline.test.Leak")); // leaks 64 MiB per render
    let mut pids = std::collections::BTreeSet::new();
    for _ in 0..8 {
        s.render_rgba_f32(t(), rate(), W, H, &pattern(), PREMULTIPLIED).unwrap();
        if let Some(p) = s.host_pid() {
            pids.insert(p);
        }
    }
    eprintln!("leaky plugin: 8 renders used {} host processes", s.spawns);
    assert!(s.spawns >= 2, "host must be recycled once RSS passes the budget");
}

#[test]
fn missing_plugin_fails_at_graph_build_time() {
    let err = OfxNode::new(cfg(), spec("com.example.DoesNotExist"), rate()).err().expect("must fail");
    eprintln!("{err}");
    assert!(err.to_string().contains("not found"));
}
