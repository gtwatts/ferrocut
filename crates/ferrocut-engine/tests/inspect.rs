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
    // Duplicate positions are separate copies: [0, 0] needs two frames.
    let e = run(
        &[0, 0],
        Limits {
            max_frame_pixels: px,
            max_retained_bytes: px * 4 * 2 - 1,
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

/// A one-frame file with `codec` in `container`: pixel format `pix`, plane
/// bytes from `fill(plane, x, y)` (per byte of each row), and optional range
/// and matrix tags. Test-only encoder over the LGPL FFmpeg.
fn encode_one(
    path: &Path,
    container: &str,
    codec_name: &str,
    pix: ffmpeg_next::format::Pixel,
    tags: Option<(
        ffmpeg_next::ffi::AVColorRange,
        ffmpeg_next::ffi::AVColorSpace,
    )>,
    fill: impl Fn(usize, usize, usize) -> u8,
) {
    use ffmpeg_next::{Packet, codec, encoder, format, frame};
    ffmpeg_next::init().unwrap();
    let mut octx = format::output_as(path, container).unwrap();
    let c = encoder::find_by_name(codec_name).expect("encoder");
    let mut enc = codec::context::Context::new_with_codec(c)
        .encoder()
        .video()
        .unwrap();
    let tb = ffmpeg_next::Rational::new(1, 24);
    enc.set_width(W);
    enc.set_height(H);
    enc.set_format(pix);
    enc.set_time_base(tb);
    if let Some((range, space)) = tags {
        // SAFETY: plain field writes on an owned, unopened encoder context.
        unsafe {
            (*enc.as_mut_ptr()).color_range = range;
            (*enc.as_mut_ptr()).colorspace = space;
        }
    }
    let mut enc = enc.open().unwrap();
    let mut ost = octx.add_stream(c).unwrap();
    ost.set_parameters(&enc);
    ost.set_time_base(tb);
    octx.write_header().unwrap();
    let ost_tb = octx.stream(0).unwrap().time_base();
    let mut f = frame::Video::new(pix, W, H);
    for p in 0..f.planes() {
        let (stride, rows) = (f.stride(p), f.plane_height(p) as usize);
        let data = f.data_mut(p);
        for y in 0..rows {
            for x in 0..stride {
                data[y * stride + x] = fill(p, x, y);
            }
        }
    }
    if let Some((range, space)) = tags {
        // SAFETY: plain field writes on an owned frame.
        unsafe {
            (*f.as_mut_ptr()).color_range = range;
            (*f.as_mut_ptr()).colorspace = space;
        }
    }
    f.set_pts(Some(0));
    enc.send_frame(&f).unwrap();
    enc.send_eof().unwrap();
    let mut pkt = Packet::empty();
    while enc.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(0);
        pkt.rescale_ts(tb, ost_tb);
        pkt.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
}

fn near(got: &[u8], want: [u8; 3]) -> bool {
    got[..3]
        .iter()
        .zip(want)
        .all(|(&g, w)| (g as i32 - w as i32).abs() <= 1)
}

/// VF1: an encoded straight-alpha source (FFV1 bgra) comes back with its
/// exact RGB and alpha, including a fully transparent pixel.
#[test]
fn encoded_alpha_is_returned_straight_and_exact() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("alpha.mkv");
    // BGRA bytes: x even -> semi-transparent [B16 G32 R64 A128]; odd -> A0.
    encode_one(
        &file,
        "matroska",
        "ffv1",
        ffmpeg_next::format::Pixel::BGRA,
        None,
        |_, x, _| {
            let px = x / 4;
            let c = x % 4;
            if px % 2 == 0 {
                [16, 32, 64, 128][c]
            } else {
                [50, 100, 200, 0][c]
            }
        },
    );
    let (_, f) = decode_frames(&file, &[0], &CancelToken::new()).unwrap();
    let f = &f[0];
    assert!(f.alpha, "bgra carries alpha");
    assert_eq!(
        &f.rgba[..4],
        &[64, 32, 16, 128],
        "straight RGBA, not premultiplied"
    );
    assert_eq!(&f.rgba[4..8], &[200, 100, 50, 0], "alpha 0 keeps its RGB");
    assert_eq!(f.conversion.applied_matrix, "rgb (no matrix)");
}

/// VF2: YUV conversion uses the stated matrix and range, checked against
/// values computed by hand from the standard equations (±1 code value).
/// Source Y=100, Cb=90, Cr=200 everywhere.
#[test]
fn yuv_matrix_and_range_match_the_reported_conversion() {
    use ffmpeg_next::ffi::{AVColorRange, AVColorSpace};
    use ffmpeg_next::format::Pixel;
    let dir = tempfile::tempdir().unwrap();
    let ycc = |p: usize, _: usize, _: usize| [100u8, 90, 200][p];
    let c = CancelToken::new();

    // Full range, matrix unspecified -> bt601 fallback:
    // R = Y + 1.402 (Cr-128), G = Y - 0.344136 (Cb-128) - 0.714136 (Cr-128),
    // B = Y + 1.772 (Cb-128)  ->  [201, 62, 33].
    let a = dir.path().join("full601.mkv");
    encode_one(
        &a,
        "matroska",
        "ffv1",
        Pixel::YUV444P,
        Some((
            AVColorRange::AVCOL_RANGE_JPEG,
            AVColorSpace::AVCOL_SPC_UNSPECIFIED,
        )),
        ycc,
    );
    let (_, f) = decode_frames(&a, &[0], &c).unwrap();
    assert_eq!(f[0].conversion.applied_range, "full (tagged)");
    assert!(f[0].conversion.applied_matrix.contains("fallback"));
    assert!(near(&f[0].rgba, [201, 62, 33]), "{:?}", &f[0].rgba[..4]);

    // Limited range, bt709 tagged: Y' = (Y-16)*255/219, C' = (C-128)*255/224;
    // R = Y' + 1.5748 Cr', G = Y' - 0.1873 Cb' - 0.4681 Cr', B = Y' + 1.8556 Cb'
    // -> [227, 68, 18].
    let b = dir.path().join("lim709.mkv");
    encode_one(
        &b,
        "matroska",
        "ffv1",
        Pixel::YUV444P,
        Some((
            AVColorRange::AVCOL_RANGE_MPEG,
            AVColorSpace::AVCOL_SPC_BT709,
        )),
        ycc,
    );
    let (_, f) = decode_frames(&b, &[0], &c).unwrap();
    assert_eq!(f[0].conversion.applied_range, "limited (tagged)");
    assert_eq!(f[0].conversion.applied_matrix, "bt709 (tagged)");
    assert!(near(&f[0].rgba, [227, 68, 18]), "{:?}", &f[0].rgba[..4]);

    // Packed YUYV in AVI (no tags): treated as YUV, limited assumed, bt601
    // fallback -> [213, 54, 21] by hand. libswscale's packed 4:2:2 path is
    // less precise (focused-run-01 observed [211, 53, 19]), so this case
    // allows +-3: a wrong matrix (bt709 [227,..]), full range ([201, 62, 33])
    // or no YUV conversion ([100, 90, 100]) are all much further away.
    // (Bytes Y0 U Y1 V per pixel pair.)
    let p = dir.path().join("yuyv.avi");
    encode_one(&p, "avi", "rawvideo", Pixel::YUYV422, None, |_, x, _| {
        [100u8, 90, 100, 200][x % 4]
    });
    let (_, f) = decode_frames(&p, &[0], &c).unwrap();
    assert_eq!(f[0].conversion.source_format, "yuyv422");
    assert!(f[0].conversion.applied_range.starts_with("limited"));
    assert!(f[0].conversion.applied_matrix.contains("bt601"));
    let close3 = f[0].rgba[..3]
        .iter()
        .zip([213u8, 54, 21])
        .all(|(&g, w)| (g as i32 - w as i32).abs() <= 3);
    assert!(close3, "{:?}", &f[0].rgba[..4]);

    // A tagged matrix that is not converted correctly here is refused.
    let r = dir.path().join("cl.mkv");
    encode_one(
        &r,
        "matroska",
        "ffv1",
        Pixel::YUV444P,
        Some((
            AVColorRange::AVCOL_RANGE_MPEG,
            AVColorSpace::AVCOL_SPC_BT2020_CL,
        )),
        ycc,
    );
    let e = decode_frames(&r, &[0], &c).unwrap_err();
    assert!(format!("{e:#}").contains("not supported"), "{e:#}");
}
