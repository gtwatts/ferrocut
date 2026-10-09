//! media::inspect: exact frames of an encoded file by presentation ordinal,
//! with their own timestamps. Fixtures are tiny FFV1 files from the engine's
//! own encoder (lossless bgr0, so pixels round-trip exactly).

use std::path::Path;

use ferrocut_core::{CancelToken, Rational, RationalTime};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::media::inspect::{MAX_INSPECT_FRAMES, decode_frames};

const W: u32 = 32;
const H: u32 = 16;

/// `n` frames at 24 fps; frame f is solid blue = f * 20.
fn synth(path: &Path, n: u32) {
    let s = EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..n {
        let mut px = vec![0u8; (W * H * 4) as usize];
        for p in px.chunks_mut(4) {
            p.copy_from_slice(&[(f * 20) as u8, 7, 3, 255]); // BGRA
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

fn blue(rgba: &[u8]) -> u8 {
    rgba[2]
}

#[test]
fn ordinals_from_start_and_end_keep_exact_timestamps() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ten.mkv");
    synth(&file, 10);
    let (stream, frames) = decode_frames(&file, &[4, 0, -1, 4], &CancelToken::new()).unwrap();
    assert_eq!(stream.frame_count, Some(10), "-1 needs the whole stream");
    assert_eq!(stream.frames_decoded, 10);
    let idx: Vec<u64> = frames.iter().map(|f| f.index).collect();
    assert_eq!(idx, [4, 0, 9, 4], "request order, duplicates kept");
    for f in &frames {
        assert_eq!((f.width, f.height), (W, H));
        assert_eq!(blue(&f.rgba), (f.index * 20) as u8, "frame {}", f.index);
        // Time is the container timestamp exactly (Matroska: milliseconds),
        // not index / nominal fps: 4/24 s is stored as 167 ms.
        let pts = f.pts.expect("timestamp");
        let t = f.time.expect("time");
        let tb: Rational = stream.time_base.parse().unwrap();
        assert_eq!(
            t,
            RationalTime(Rational::from_int(pts - stream.start_pts.unwrap_or(0)) * tb)
        );
        let nominal = RationalTime::from_frames(f.index as i64, Rational::from_int(24));
        let d = (t - nominal).seconds();
        let half = Rational::new(1, 48);
        assert!(d <= half && d >= -half, "frame {}", f.index);
        assert_eq!(f.conversion, "rgb source");
    }
}

#[test]
fn only_positive_ordinals_stop_early() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ten.mkv");
    synth(&file, 10);
    let (stream, frames) = decode_frames(&file, &[2], &CancelToken::new()).unwrap();
    assert_eq!(frames[0].index, 2);
    assert_eq!(stream.frame_count, None, "did not read to the end");
    assert!(stream.frames_decoded < 10);
}

#[test]
fn bounds_and_requests_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ten.mkv");
    synth(&file, 10);
    let c = CancelToken::new();
    let err = |r: &[i64]| format!("{:#}", decode_frames(&file, r, &c).unwrap_err());
    assert!(err(&[10]).contains("past the end"), "{}", err(&[10]));
    assert!(err(&[10]).contains("10 frames"));
    assert!(err(&[-11]).contains("before the start"), "{}", err(&[-11]));
    assert!(err(&[]).contains("no frames"));
    let many: Vec<i64> = (0..=MAX_INSPECT_FRAMES as i64).collect();
    assert!(err(&many).contains("max"));
    assert!(err(&[-(MAX_INSPECT_FRAMES as i64) - 1]).contains("at most"));
}

#[test]
fn cancellation_stops_decoding() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ten.mkv");
    synth(&file, 10);
    let c = CancelToken::new();
    c.cancel();
    let e = decode_frames(&file, &[0], &c).unwrap_err();
    assert!(format!("{e:#}").contains("cancelled"));
}

#[test]
fn corrupt_and_truncated_inputs_error_instead_of_inventing_frames() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ten.mkv");
    synth(&file, 10);
    let bytes = std::fs::read(&file).unwrap();

    let junk = dir.path().join("junk.mkv");
    std::fs::write(&junk, b"definitely not a video file").unwrap();
    assert!(decode_frames(&junk, &[0], &CancelToken::new()).is_err());

    // Truncated: the last frame cannot be returned as if it were whole.
    let cut = dir.path().join("cut.mkv");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
    match decode_frames(&cut, &[9], &CancelToken::new()) {
        Err(_) => {}
        Ok((_, f)) => assert!(
            f[0].corrupt,
            "a frame past the cut must not come back clean"
        ),
    }
}
