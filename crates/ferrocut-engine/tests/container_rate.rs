//! Chunks, concatenated masters and proxies declare their exact frame rate:
//! through the Matroska DefaultDuration when readers derive the rate back
//! exactly (av_reduce(1e9, ns, 30000)), and always through the
//! FERROCUT_FRAME_RATE stream tag, which Ferrocut's probe prefers. With
//! millisecond timestamps alone, 30 fps used to read back as 30000/1001.

use std::path::Path;

use ferrocut_core::Rational;
use ferrocut_engine::media::concat::concat;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::media::{FRAME_RATE_TAG, declared_rate_round_trips, probe, proxy};

/// What libavformat itself reports: the average rate and our tag.
fn demuxed(path: &Path) -> (Rational, Option<String>) {
    let ictx = ffmpeg_next::format::input(path).unwrap();
    let st = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .unwrap();
    let r = st.avg_frame_rate();
    (
        Rational::new(r.numerator() as i64, r.denominator().max(1) as i64),
        st.metadata().get(FRAME_RATE_TAG).map(str::to_string),
    )
}

fn write_chunk(path: &Path, fps: Rational, frames: usize, w: u32, h: u32) {
    let s = EncodeSettings {
        width: w,
        height: h,
        fps,
        gop: 4,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for _ in 0..frames {
        e.push_bgra(&vec![128u8; (w * h * 4) as usize]).unwrap();
    }
    e.finish().unwrap();
}

fn assert_rate(path: &Path, fps: Rational, what: &str) {
    let (rate, tag) = demuxed(path);
    assert_eq!(
        tag.as_deref(),
        Some(fps.to_string().as_str()),
        "{what} tag at {fps}"
    );
    if declared_rate_round_trips(fps) {
        assert_eq!(rate, fps, "{what} declared rate at {fps}");
    }
    assert_eq!(probe(path).unwrap().fps, Some(fps), "{what} probe at {fps}");
}

#[test]
fn masters_declare_their_frame_rate() {
    let dir = tempfile::tempdir().unwrap();
    let rates = [
        (30, 1),
        (30000, 1001),
        (24, 1),
        (24000, 1001),
        (25, 1),
        (60, 1),
        (60000, 1001),
    ];
    for (i, (n, d)) in rates.into_iter().enumerate() {
        let fps = Rational::new(n, d);
        let mut chunks = Vec::new();
        for c in 0..2 {
            let p = dir.path().join(format!("c{i}-{c}.mkv"));
            write_chunk(&p, fps, 4, 32, 16);
            assert_rate(&p, fps, "chunk");
            chunks.push(p);
        }
        let out = dir.path().join(format!("m{i}.mkv"));
        let refs: Vec<&Path> = chunks.iter().map(|p| p.as_path()).collect();
        concat(&refs, &[0, 4], fps, &out, None).unwrap();
        assert_rate(&out, fps, "master");
    }
    // 59.94p cannot be declared through the container: no misleading rate.
    let (rate, _) = demuxed(&dir.path().join("m6.mkv"));
    assert_ne!(rate, Rational::new(19001, 317));
}

#[test]
fn proxies_keep_the_exact_source_rate() {
    let dir = tempfile::tempdir().unwrap();
    let fps = Rational::new(60000, 1001);
    let src = dir.path().join("src.mkv");
    write_chunk(&src, fps, 8, 64, 32);
    let r = proxy::generate(&src, false).unwrap();
    assert_rate(&r.proxy, fps, "proxy");
    // Its last frame ends exactly where the source ends (8 frames).
    let (src_d, proxy_d) = (
        probe(&src).unwrap().duration,
        probe(&r.proxy).unwrap().duration,
    );
    assert_eq!(proxy_d, src_d);
}
