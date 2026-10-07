//! RationalTime -> pts rounding must match FFmpeg's `av_rescale_rnd(..,
//! AV_ROUND_NEAR_INF)` exactly, and pts -> frame -> pts must round-trip through
//! a real mux + seek. No GPU needed.

use std::path::Path;

use cutline_core::{Rational, RationalTime};
use cutline_engine::media::decode::Decoder;
use cutline_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ffmpeg_next::ffi;

fn near_inf(a: i64, b: i64, c: i64) -> i64 {
    // SAFETY: pure arithmetic.
    unsafe { ffi::av_rescale_rnd(a, b, c, ffi::AVRounding::AV_ROUND_NEAR_INF) }
}

#[test]
fn round_matches_av_rescale_rnd_near_inf() {
    // Exact halves both signs, plus a sweep (c odd and even).
    for (a, b, c) in [
        (1, 1, 2),
        (-1, 1, 2),
        (3, 1, 2),
        (-3, 1, 2),
        (5, 1, 2),
        (-5, 1, 2),
        (125, 1, 2),
        (-125, 1, 2),
    ] {
        assert_eq!(
            Rational::new(a * b, c).round(),
            near_inf(a, b, c),
            "{a}*{b}/{c}"
        );
    }
    for a in -2000..2000 {
        for (b, c) in [
            (1000, 48),
            (1001, 24),
            (1000, 30),
            (90000, 25),
            (1, 2),
            (3, 4),
            (7, 10),
            (1000, 16),
        ] {
            assert_eq!(
                Rational::new(a * b, c).round(),
                near_inf(a, b, c),
                "{a}*{b}/{c}"
            );
        }
    }
}

#[test]
fn to_pts_matches_av_rescale_q_rnd() {
    let q = |n: i32, d: i32| ffi::AVRational { num: n, den: d };
    for (rate, tb) in [
        ((48, 1), (1, 1000)),
        ((24000, 1001), (1, 1000)),
        ((16, 1), (1, 1000)),
        ((30000, 1001), (1, 90000)),
        ((25, 1), (1, 12800)),
    ] {
        for n in -500i64..500 {
            // SAFETY: pure arithmetic.
            let want = unsafe {
                ffi::av_rescale_q_rnd(
                    n,
                    q(rate.1, rate.0),
                    q(tb.0, tb.1),
                    ffi::AVRounding::AV_ROUND_NEAR_INF,
                )
            };
            let got = RationalTime::from_frames(n, Rational::new(rate.0 as i64, rate.1 as i64))
                .to_pts(Rational::new(tb.0 as i64, tb.1 as i64));
            assert_eq!(got, want, "frame {n} rate {rate:?} tb {tb:?}");
        }
    }
}

const W: u32 = 64;
const H: u32 = 32;

/// Gray frames whose level encodes the frame index (4 * n).
fn synth(path: &Path, fps: Rational, frames: i64) {
    let s = EncodeSettings {
        width: W,
        height: H,
        fps,
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for n in 0..frames {
        let v = (n * 4) as u8;
        let mut px = vec![v; (W * H * 4) as usize];
        px.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

fn packet_pts(path: &Path) -> (Vec<i64>, Rational) {
    let mut ictx = ffmpeg_next::format::input(path).unwrap();
    let tb = ictx.stream(0).unwrap().time_base();
    let mut pts: Vec<i64> = ictx.packets().filter_map(|(_, p)| p.pts()).collect();
    pts.sort();
    (
        pts,
        Rational::new(tb.numerator() as i64, tb.denominator() as i64),
    )
}

#[test]
fn mux_pts_and_seeks_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    for (name, fps) in [
        ("48", Rational::from_int(48)),
        ("23976", Rational::new(24000, 1001)),
    ] {
        let path = dir.path().join(format!("idx-{name}.mkv"));
        let frames = 60;
        synth(&path, fps, frames);

        // The muxer's own (NEAR_INF) timestamps equal ours, including the exact
        // half-millisecond cases, and map back to the same frame indices.
        let (pts, tb) = packet_pts(&path);
        assert_eq!(tb, Rational::new(1, 1000));
        assert_eq!(pts.len(), frames as usize);
        let mut halves = 0;
        for (n, &p) in pts.iter().enumerate() {
            let t = RationalTime::from_frames(n as i64, fps);
            if (t.seconds() / tb).den() == 2 {
                halves += 1;
            }
            assert_eq!(p, t.to_pts(tb), "{name}: frame {n}");
            assert_eq!(
                RationalTime::from_pts(p, tb).frame_round(fps),
                n as i64,
                "{name}: frame {n}"
            );
        }
        assert!(halves > 0, "{name}: test should exercise exact halves");

        // Frame-accurate decode in a scrambled order: backwards jumps force
        // keyframe seeks, small forward steps decode forward.
        let mut dec = Decoder::open(&path, W, H).unwrap();
        let order: Vec<i64> = (0..frames)
            .map(|i| (i * 37 + 11) % frames)
            .chain([59, 0, 30, 29, 31, 3, 2, 1])
            .collect();
        for n in order {
            let px = dec.frame_at(RationalTime::from_frames(n, fps)).unwrap();
            assert_eq!(px[0] as i64, n * 4, "{name}: asked for frame {n}");
        }
    }
}
