//! Determinism and correctness of the Lottie layer. CPU-only except
//! `render_node_through_core_on_gpu`, which skips without a GPU adapter.
#![cfg(not(cutline_no_thorvg))]

use std::sync::Arc;

use cutline_core::{
    AdapterPreference, CancelToken, ErrorKind, GpuContext, RationalTime, RenderCtx, RenderNode, WorkerState,
};
use cutline_lottie::{EndBehavior, Fit, LottieNode, LottieParams, OutputEncoding};
use half::f16;

const DOC: &[u8] = include_bytes!("data/cutline_test.json");
const SOLID: &[u8] = include_bytes!("data/solid_orange.json");

fn params(end: EndBehavior) -> LottieParams {
    LottieParams { width: 320, height: 240, fit: Fit::Contain, end, encoding: OutputEncoding::AcesCg }
}

fn node(end: EndBehavior) -> LottieNode {
    LottieNode::from_json(DOC, params(end)).unwrap()
}

/// Graph at 24 fps sampling the 30 fps Lottie (so frame times are fractional).
fn graph_t(i: i64) -> RationalTime {
    RationalTime::new(i, 24)
}

fn bits(px: &[f16]) -> Vec<u16> {
    px.iter().map(|v| v.to_bits()).collect()
}

fn fnv(px: &[u16]) -> u64 {
    px.iter().fold(0xcbf29ce484222325u64, |h, v| (h ^ *v as u64).wrapping_mul(0x100000001b3))
}

fn render(n: &LottieNode, r: &mut cutline_lottie::LottieRenderer, t: RationalTime) -> Vec<u16> {
    bits(&n.render_cpu(r, t).unwrap().image.pixels)
}

#[test]
fn same_time_renders_identical_bytes_twice() {
    let n = node(EndBehavior::Hold);
    let t = graph_t(29); // 1.2083 s = Lottie frame 36.25
    let mut r = n.new_renderer().unwrap();
    let a = render(&n, &mut r, t);
    let b = render(&n, &mut r, t); // same frame again (ThorVG "no change" path)
    render(&n, &mut r, graph_t(70)); // move elsewhere...
    let c = render(&n, &mut r, t); // ...and come back
    let mut fresh = n.new_renderer().unwrap();
    let d = render(&n, &mut fresh, t); // brand-new ThorVG instance
    assert!(a.iter().any(|v| *v != 0), "frame is empty");
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(a, d);
    eprintln!("t={t:?}: 4 renders byte-identical, fnv64={:016x}, {} bytes", fnv(&a), a.len() * 2);
}

#[test]
fn chunk_boundary_matches_sequential() {
    let n = node(EndBehavior::Hold);
    const FRAMES: i64 = 72; // 3 s at 24 fps; Lottie is 90 frames at 30 fps
    let mut seq = n.new_renderer().unwrap();
    let sequential: Vec<Vec<u16>> = (0..FRAMES).map(|i| render(&n, &mut seq, graph_t(i))).collect();

    // Distinct frames really differ (the animation, including expressions, moves).
    assert_ne!(sequential[10], sequential[11]);

    // Three chunks, each on a fresh renderer (a different worker), rendered in
    // reverse chunk order to scramble any shared state.
    for (start, end) in [(48, 72), (24, 48), (0, 24)] {
        let mut worker = n.new_renderer().unwrap();
        for i in start..end {
            assert_eq!(render(&n, &mut worker, graph_t(i)), sequential[i as usize], "frame {i} (chunk {start}..{end})");
        }
    }
    // Starting mid-sequence exactly at the boundary, and going backwards.
    let mut mid = n.new_renderer().unwrap();
    for i in (20..30).rev() {
        assert_eq!(render(&n, &mut mid, graph_t(i)), sequential[i as usize], "frame {i} (reverse)");
    }
    eprintln!(
        "{FRAMES} frames: chunked == sequential; boundary frames 23/24 fnv64 {:016x}/{:016x}",
        fnv(&sequential[23]),
        fnv(&sequential[24])
    );
}

#[test]
fn expressions_are_a_pure_function_of_time() {
    // random(), Math.random() and wiggle() drive the "dot" layer. With libc
    // rand() these would depend on everything rendered before in the process.
    let n = node(EndBehavior::Hold);
    let mut warm = n.new_renderer().unwrap();
    for i in 0..40 {
        render(&n, &mut warm, graph_t(i));
    }
    let after_history = render(&n, &mut warm, graph_t(17));
    let mut cold = n.new_renderer().unwrap();
    let cold17 = render(&n, &mut cold, graph_t(17));
    assert_eq!(after_history, cold17);
}

#[test]
fn concurrent_workers_match_sequential() {
    let n = Arc::new(node(EndBehavior::Hold));
    let mut seq = n.new_renderer().unwrap();
    let sequential: Vec<Vec<u16>> = (0..48).map(|i| render(&n, &mut seq, graph_t(i))).collect();
    let sequential = Arc::new(sequential);
    std::thread::scope(|s| {
        for w in 0..4i64 {
            let (n, sequential) = (n.clone(), sequential.clone());
            s.spawn(move || {
                let mut r = n.new_renderer().unwrap();
                for i in (w..48).step_by(4) {
                    assert_eq!(render(&n, &mut r, graph_t(i)), sequential[i as usize], "worker {w} frame {i}");
                }
            });
        }
    });
}

#[test]
fn end_behaviors() {
    let t_end = RationalTime::new(3, 1); // exactly op (90 frames @ 30)
    let hold = node(EndBehavior::Hold);
    let mut r = hold.new_renderer().unwrap();
    let last = render(&hold, &mut r, RationalTime::new(89, 30));
    assert_eq!(render(&hold, &mut r, t_end), last);
    assert_eq!(render(&hold, &mut r, RationalTime::new(100, 1)), last);

    let lp = node(EndBehavior::Loop);
    let mut r = lp.new_renderer().unwrap();
    let first = render(&lp, &mut r, RationalTime::new(0, 1));
    assert_eq!(render(&lp, &mut r, t_end), first);
    assert_eq!(render(&lp, &mut r, RationalTime::new(31, 10)), render(&lp, &mut r, RationalTime::new(1, 10)));

    let tr = node(EndBehavior::Transparent);
    let mut r = tr.new_renderer().unwrap();
    assert!(render(&tr, &mut r, t_end).iter().all(|v| *v == 0));
    assert!(render(&tr, &mut r, RationalTime::new(-1, 24)).iter().all(|v| *v == 0));
    assert_eq!(render(&tr, &mut r, RationalTime::new(1, 2)), {
        let mut h = hold.new_renderer().unwrap();
        render(&hold, &mut h, RationalTime::new(1, 2))
    });
}

#[test]
fn in_point_offset_maps_time_zero_to_ip() {
    // Shift the whole document by ip=30: graph t=0 must equal the original at 1 s.
    let shifted = String::from_utf8(DOC.to_vec()).unwrap().replacen("\"ip\":0,\"op\":90,\"w\"", "\"ip\":30,\"op\":120,\"w\"", 1);
    assert!(shifted.contains("\"ip\":30,\"op\":120"));
    // Layer in/out points are absolute, so shift those too for an exact match.
    let v: serde_json::Value = serde_json::from_str(&shifted).unwrap();
    let mut v = v;
    for l in v["layers"].as_array_mut().unwrap() {
        for k in ["ip", "op", "st"] {
            let x = l[k].as_f64().unwrap() + 30.0;
            l[k] = serde_json::json!(x);
        }
    }
    let shifted = serde_json::to_vec(&v).unwrap();
    let a = LottieNode::from_json(shifted, params(EndBehavior::Hold)).unwrap();
    let b = node(EndBehavior::Hold);
    let (mut ra, mut rb) = (a.new_renderer().unwrap(), b.new_renderer().unwrap());
    // The yellow "late" square is visible on Lottie frames [ip+30, ip+60) only.
    // Local t = 1 s is frame ip+30 in both documents; if ThorVG treated frame
    // numbers as absolute, the shifted doc would show nothing there.
    let late_px = |px: &[u16]| {
        let s = 240.0 / 256.0; // Contain-fit 256x256 into 320x240
        let (x, y) = ((320.0 - 256.0 * s) / 2.0 + 220.0 * s, 40.0 * s);
        px[(y as usize * 320 + x as usize) * 4 + 3] != 0
    };
    for (n, r) in [(&b, &mut rb), (&a, &mut ra)] {
        assert!(!late_px(&render(n, r, RationalTime::new(29, 30))), "visible too early");
        assert!(late_px(&render(n, r, RationalTime::new(1, 1))), "not visible at its in point");
        assert!(late_px(&render(n, r, RationalTime::new(59, 30))), "not visible before its out point");
        assert!(!late_px(&render(n, r, RationalTime::new(2, 1))), "visible after its out point");
    }
}

#[test]
fn color_conversion_and_tags() {
    let mk = |enc| {
        LottieNode::from_json(SOLID, LottieParams { width: 16, height: 16, fit: Fit::Stretch, end: EndBehavior::Hold, encoding: enc })
            .unwrap()
    };
    let srgb = mk(OutputEncoding::SrgbEncoded);
    let mut r = srgb.new_renderer().unwrap();
    let f = srgb.render_cpu(&mut r, RationalTime::new(0, 1)).unwrap();
    assert_eq!(f.color_space.name(), "sRGB Encoded Rec.709 (sRGB)");
    let px = &f.image.pixels[8 * 16 * 4 + 8 * 4..][..4];
    assert_eq!(px.iter().map(|v| v.to_f32()).collect::<Vec<_>>(), [1.0, f16::from_f32(128.0 / 255.0).to_f32(), 0.0, 1.0]);

    let aces = mk(OutputEncoding::AcesCg);
    let mut r = aces.new_renderer().unwrap();
    let f = aces.render_cpu(&mut r, RationalTime::new(0, 1)).unwrap();
    assert_eq!(f.color_space.name(), "ACEScg");
    let px = &f.image.pixels[8 * 16 * 4 + 8 * 4..][..4];
    // OCIO 2.5 cg-config: sRGB Encoded Rec.709 (1, 128/255, 0) -> ACEScg
    let g = cutline_lottie::color::srgb_eotf(128.0 / 255.0);
    let m = cutline_lottie::color::REC709_TO_ACESCG;
    for c in 0..3 {
        let want = m[c][0] as f64 + m[c][1] as f64 * g;
        assert!((px[c].to_f64() - want).abs() < want * 1e-3 + 1e-4, "ch{c}: {} vs {want}", px[c]);
    }
    assert_eq!(px[3].to_f32(), 1.0);
}

#[test]
fn bad_document_is_a_permanent_node_error() {
    for bad in [&b"{\"not\":\"lottie\"}"[..], &b"{\"v\":\"5.7\",\"fr\":30,"[..], &b"\x00\x01"[..]] {
        let e = LottieNode::from_json(bad, params(EndBehavior::Hold)).err().expect("must reject");
        assert_eq!(e.kind, ErrorKind::Permanent, "{e}");
    }
    let zero = LottieParams { width: 0, ..params(EndBehavior::Hold) };
    assert_eq!(LottieNode::from_json(DOC, zero).err().map(|e| e.kind), Some(ErrorKind::Permanent));
}

#[test]
fn many_short_lived_worker_threads() {
    // Regression: ThorVG keys JerryScript contexts by thread id; with reused ids
    // on fresh threads it used to segfault. All ThorVG calls now run on one thread.
    let n = Arc::new(node(EndBehavior::Hold));
    let mut seq = n.new_renderer().unwrap();
    let want = render(&n, &mut seq, graph_t(13));
    for round in 0..16 {
        let n = n.clone();
        let got = std::thread::spawn(move || {
            let mut r = n.new_renderer().unwrap();
            render(&n, &mut r, graph_t(13))
        })
        .join()
        .unwrap();
        assert_eq!(got, want, "round {round}");
    }
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
    let n = node(EndBehavior::Hold);
    let mut worker = WorkerState::default();
    let cancel = CancelToken::new();
    let t = graph_t(31);
    let out = {
        let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
        n.render(&mut ctx, t, &[]).unwrap()
    };
    assert!(out.gpu().is_some(), "node should hand the engine a GPU frame");
    let back = out.to_cpu_frame(&gpu).unwrap();
    let mut r = n.new_renderer().unwrap();
    assert_eq!(bits(&back.image.pixels), render(&n, &mut r, t), "GPU upload round trip changed bytes");
    assert_eq!(back.color_space.name(), "ACEScg");

    cancel.cancel();
    let mut ctx = RenderCtx::new(&gpu, &mut worker, &cancel, None);
    assert_eq!(n.render(&mut ctx, t, &[]).err().map(|e| e.kind), Some(ErrorKind::Cancelled));
}
