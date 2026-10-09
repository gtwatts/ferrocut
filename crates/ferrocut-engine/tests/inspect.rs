//! media::inspect: exact frames of an encoded file by presentation ordinal,
//! with their own timestamps. Fixtures are tiny FFV1 files from the engine's
//! own encoder (lossless bgr0, so pixels round-trip exactly); the irregular
//! fixture is concatenated with the engine's own muxer at chosen frames.

use std::path::Path;

use ferrocut_core::{CancelToken, Rational, RationalTime};
use ferrocut_engine::media::concat::concat;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::media::inspect::{
    Limits, MAX_INSPECT_FRAMES, decode_frames, inspect_file, inspect_with_limits,
};

const W: u32 = 32;
const H: u32 = 16;

fn settings() -> EncodeSettings {
    EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(24),
        gop: 6,
    }
}

/// `n` frames at 24 fps; frame f is solid blue = (first + f) * 20.
fn synth_from(path: &Path, n: u32, first: u32) {
    let mut e = ChunkEncoder::create(path, &settings()).unwrap();
    for f in first..first + n {
        let mut px = vec![0u8; (W * H * 4) as usize];
        for p in px.chunks_mut(4) {
            p.copy_from_slice(&[(f * 20) as u8, 7, 3, 255]); // BGRA
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

fn synth(path: &Path, n: u32) {
    synth_from(path, n, 0)
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
    assert_eq!(stream.time_base, "1/1000", "Matroska milliseconds");
    assert_eq!(stream.start_pts, Some(0));
    let idx: Vec<u64> = frames.iter().map(|f| f.index).collect();
    assert_eq!(idx, [4, 0, 9, 4], "request order, duplicates kept");
    // Frame 4 of a 24 fps file is stored at 167 ms (4/24 s rounded), and its
    // time is that stored value, not 4/24.
    assert_eq!(frames[0].pts, Some(167));
    assert_eq!(frames[0].time, Some(RationalTime(Rational::new(167, 1000))));
    assert_eq!(frames[2].pts, Some(375));
    for f in &frames {
        assert_eq!((f.width, f.height), (W, H));
        assert_eq!(blue(&f.rgba), (f.index * 20) as u8, "frame {}", f.index);
        assert!(!f.alpha);
        assert_eq!(f.conversion.applied_matrix, "rgb (no matrix)");
        assert!(f.rgba.chunks(4).all(|p| p[3] == 255));
    }
}

/// Irregular timestamps with a nonzero origin: two 2-frame chunks muxed at
/// output frames 5..7 and 15..17. Exact values from the muxer's rule
/// (round(frame * 1000 / 24)): 208, 250, 625, 667 ms; ordinals stay 0..3.
#[test]
fn irregular_pts_and_nonzero_origin_are_reported_as_stored() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.mkv"), dir.path().join("b.mkv"));
    synth_from(&a, 2, 0);
    synth_from(&b, 2, 2);
    let out = dir.path().join("gappy.mkv");
    concat(
        &[a.as_path(), b.as_path()],
        &[5, 15],
        Rational::from_int(24),
        &out,
        None,
    )
    .unwrap();
    let (stream, frames) = decode_frames(&out, &[0, 1, 2, 3], &CancelToken::new()).unwrap();
    assert_eq!(stream.start_pts, Some(208));
    let pts: Vec<Option<i64>> = frames.iter().map(|f| f.pts).collect();
    assert_eq!(pts, [Some(208), Some(250), Some(625), Some(667)]);
    let times: Vec<Option<RationalTime>> = frames.iter().map(|f| f.time).collect();
    let ms = |v: i64| Some(RationalTime(Rational::new(v, 1000)));
    assert_eq!(times, [ms(0), ms(42), ms(417), ms(459)]);
    let blues: Vec<u8> = frames.iter().map(|f| blue(&f.rgba)).collect();
    assert_eq!(
        blues,
        [0, 20, 40, 60],
        "ordinal order is presentation order"
    );
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

/// Oversized frames and the retained-bytes budget are refused, for frames
/// that are returned and for frames only held for end-relative indices.
#[test]
fn frame_size_and_retained_budget_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ten.mkv");
    synth(&file, 10);
    let c = CancelToken::new();
    let run = |r: &[i64], l: Limits| {
        format!(
            "{:#}",
            inspect_with_limits(&file, r, &c, None, l).unwrap_err()
        )
    };
    let px = (W * H) as u64;
    let e = run(
        &[0],
        Limits {
            max_frame_pixels: px - 1,
            max_retained_bytes: 1 << 30,
        },
    );
    assert!(e.contains("max") && e.contains("pixels"), "{e}");
    // Two returned RGBA frames need 2 * W * H * 4 bytes.
    let e = run(
        &[0, 1],
        Limits {
            max_frame_pixels: px,
            max_retained_bytes: px * 4 + 1,
        },
    );
    assert!(e.contains("would hold more than"), "{e}");
    // Holding the last 8 decoded frames for -8 exceeds a budget of one frame.
    let e = run(
        &[-8],
        Limits {
            max_frame_pixels: px,
            max_retained_bytes: px * 4,
        },
    );
    assert!(e.contains("would hold more than"), "{e}");
    assert!(inspect_with_limits(&file, &[0], &c, None, Limits::default()).is_ok());
}

#[test]
fn cancellation_stops_before_and_during_work() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ten.mkv");
    synth(&file, 10);
    let c = CancelToken::new();
    c.cancel();
    let e = decode_frames(&file, &[0], &c).unwrap_err();
    assert!(format!("{e:#}").contains("cancelled"));
    // Cancelled after the first decoded frame: the decode loop stops.
    let c = CancelToken::new();
    let c2 = c.clone();
    let mut hook = move || c2.cancel();
    let e = inspect_file(&file, &[9], &c, Some(&mut hook)).unwrap_err();
    assert!(format!("{e:#}").contains("cancelled"), "{e:#}");
}

/// The file is hashed before and after decoding through one handle; a write
/// in between is an error, not pixels labelled with the old hash.
#[test]
fn a_file_changed_during_decoding_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ten.mkv");
    synth(&file, 10);
    let c = CancelToken::new();
    let ok = inspect_file(&file, &[0], &c, None).unwrap();
    assert_eq!(ok.identity.kind, "observed_recheck");
    assert_eq!(
        ok.identity.blake3,
        ferrocut_engine::index::blake3_file(&file).unwrap()
    );
    let target = file.clone();
    let mut mutate = move || {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&target)
            .unwrap();
        f.write_all(b"appended").unwrap();
    };
    let e = inspect_file(&file, &[2], &c, Some(&mut mutate)).unwrap_err();
    assert!(
        format!("{e:#}").contains("changed while it was decoded"),
        "{e:#}"
    );
}

/// Only self-contained containers open, and nothing but the one handle is
/// read: playlists and concat lists that name other files (outside or inside
/// the directory) are refused at open.
#[test]
fn secondary_files_are_never_opened() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    synth(&outside.join("clip.mkv"), 4);
    synth(&root.join("inside.mkv"), 4);
    let c = CancelToken::new();
    for (name, body) in [
        (
            "out.m3u8",
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\n../outside/clip.mkv\n#EXT-X-ENDLIST\n",
        ),
        (
            "in.m3u8",
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\ninside.mkv\n#EXT-X-ENDLIST\n",
        ),
        ("list.ffconcat", "ffconcat version 1.0\nfile 'inside.mkv'\n"),
    ] {
        let p = root.join(name);
        std::fs::write(&p, body).unwrap();
        let e = decode_frames(&p, &[0], &c).unwrap_err();
        assert!(format!("{e:#}").contains("self-contained"), "{name}: {e:#}");
    }
    assert!(decode_frames(&root.join("inside.mkv"), &[0], &c).is_ok());
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
