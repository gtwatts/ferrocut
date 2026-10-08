//! Chunk and concatenated masters declare their exact frame rate (Matroska
//! DefaultDuration). With millisecond timestamps alone, readers guess:
//! 30 fps used to read back as 30000/1001.

use ferrocut_core::Rational;
use ferrocut_engine::media::concat::concat;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};

fn declared_rate(path: &std::path::Path) -> (i32, i32) {
    let ictx = ffmpeg_next::format::input(path).unwrap();
    let st = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .unwrap();
    let r = st.avg_frame_rate();
    (r.numerator(), r.denominator())
}

#[test]
fn masters_declare_their_frame_rate() {
    let dir = tempfile::tempdir().unwrap();
    for (i, (fps, want)) in [
        (Rational::new(30, 1), (30, 1)),
        (Rational::new(30000, 1001), (30000, 1001)),
        (Rational::new(24, 1), (24, 1)),
    ]
    .into_iter()
    .enumerate()
    {
        let s = EncodeSettings {
            width: 32,
            height: 16,
            fps,
            gop: 4,
        };
        let mut chunks = Vec::new();
        for c in 0..2 {
            let p = dir.path().join(format!("c{i}-{c}.mkv"));
            let mut e = ChunkEncoder::create(&p, &s).unwrap();
            for _ in 0..4 {
                e.push_bgra(&vec![128u8; 32 * 16 * 4]).unwrap();
            }
            e.finish().unwrap();
            assert_eq!(declared_rate(&p), want, "chunk at {fps}");
            chunks.push(p);
        }
        let out = dir.path().join(format!("m{i}.mkv"));
        let refs: Vec<&std::path::Path> = chunks.iter().map(|p| p.as_path()).collect();
        concat(&refs, &[0, 4], fps, &out, None).unwrap();
        assert_eq!(declared_rate(&out), want, "master at {fps}");
    }
}
