//! Selected-range render (PP-165/169): an exact half-open interval of
//! timeline frames rendered into a master that starts at 0. Its frames and
//! PCM equal the same frames/samples of a full master; interior chunks reuse
//! the full render's cache; the report keeps source and output intervals.
//! Pure rules always run; the render test skips without a GPU adapter.

use std::path::Path;

use ferrocut_audio::Stereo;
use ferrocut_core::{
    AdapterPreference, CancelToken, GpuContext, Rational, RationalTime, SharedGpu,
};
use ferrocut_engine::audio::frame_sample;
use ferrocut_engine::captions::{CaptionCue, clip_to_range};
use ferrocut_engine::media::concat::{ConcatAudio, StereoFeed, concat};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::media::inspect::decode_frames;
use ferrocut_engine::render::{ChunkStatus, FrameRange, RenderOptions};
use ferrocut_engine::{Timeline, compile, render};
use ffmpeg_next::{format, media};

const W: u32 = 64;
const H: u32 = 48;

fn t(s: &str) -> RationalTime {
    RationalTime(s.parse::<Rational>().unwrap())
}

#[test]
fn frame_and_time_ranges_are_half_open_and_exact() {
    let ntsc: Rational = "24000/1001".parse().unwrap();
    assert_eq!(
        FrameRange::parse("17..41", false, ntsc).unwrap(),
        FrameRange { start: 17, end: 41 }
    );
    // Frame i is in [t0, t1) iff t0 <= i * 1001/24000 < t1.
    // 1 s -> 23.976 frames: first frame starting at or after 1 s is 24.
    let r = FrameRange::parse("1..2", true, ntsc).unwrap();
    assert_eq!((r.start, r.end), (24, 48));
    // A time exactly on a frame start includes that frame and excludes the end one.
    let r = FrameRange::parse("1001/24000..3003/24000", true, ntsc).unwrap();
    assert_eq!((r.start, r.end), (1, 3));
    for bad in ["5..5", "6..2", "-1..3", "a..3", "3", "1.."] {
        assert!(FrameRange::parse(bad, false, ntsc).is_err(), "{bad}");
    }
    // An interval holding no frame start is refused, not rounded to one.
    assert!(FrameRange::parse("1/100..2/100", true, ntsc).is_err());
    assert!(FrameRange::frames(0, 10).unwrap().check(9).is_err());
    assert!(FrameRange::frames(0, 9).unwrap().check(9).is_ok());
}

#[test]
fn caption_cues_are_clipped_and_shifted_to_the_range() {
    let cue = |id: &str, a: &str, b: &str| CaptionCue {
        id: id.into(),
        start: t(a),
        end: t(b),
        text: id.into(),
    };
    let cues = [
        cue("before", "0", "1"),
        cue("straddle_in", "1/2", "3/2"),
        cue("inside", "3/2", "2"),
        cue("straddle_out", "2", "4"),
        cue("after", "3", "4"),
    ];
    let out = clip_to_range(&cues, t("1"), t("3")).unwrap();
    let got: Vec<(&str, RationalTime, RationalTime)> = out
        .iter()
        .map(|c| (c.id.as_str(), c.start, c.end))
        .collect();
    assert_eq!(
        got,
        [
            ("straddle_in", t("0"), t("1/2")),
            ("inside", t("1/2"), t("1")),
            ("straddle_out", t("1"), t("2")),
        ]
    );
    assert!(clip_to_range(&cues, t("2"), t("2")).is_err());
}

fn gpu_or_skip() -> Option<SharedGpu> {
    match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => Some(SharedGpu::new(g)),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            None
        }
    }
}

/// 3 s of moving 24 fps video with 48 kHz stereo impulses every 1/4 s.
fn av_source(dir: &Path, frames: i64) -> std::path::PathBuf {
    let fps = Rational::from_int(24);
    let v = dir.join("src.video.mkv");
    let s = EncodeSettings {
        width: W,
        height: H,
        fps,
        gop: 6,
    };
    let mut e = ChunkEncoder::create(&v, &s).unwrap();
    for f in 0..frames {
        let px: Vec<u8> = (0..W * H * 4)
            .map(|i| (i as i64 * 7 + f * 3) as u8 | 0x10)
            .collect();
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
    let n = (frames * 2000) as usize;
    let mut a = Stereo::silence(n);
    for i in (0..n).step_by(12_000) {
        a.l[i] = 0.5;
        a.r[i] = 0.5;
    }
    let out = dir.join("src.mkv");
    let fs = |i: i64| i * 2000;
    concat(
        &[v.as_path()],
        &[0],
        fps,
        &out,
        Some(&mut ConcatAudio {
            rate: 48_000,
            feed: &mut StereoFeed(&a),
            frame_sample: &fs,
            total: n as i64,
        }),
    )
    .unwrap();
    out
}

fn read_pcm(path: &Path) -> Vec<f32> {
    ffmpeg_next::init().unwrap();
    let mut ictx = format::input(path).unwrap();
    let idx = ictx.streams().best(media::Type::Audio).unwrap().index();
    let mut out = Vec::new();
    for (s, p) in ictx.packets() {
        if s.index() == idx {
            out.extend(
                p.data()
                    .unwrap()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c)),
            );
        }
    }
    out
}

#[test]
fn range_master_equals_the_same_frames_and_samples_of_the_full_master() {
    let Some(gpu) = gpu_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    av_source(d, 3 * 24);
    // 23.976 fps (non-integer samples per frame), 12-frame chunks.
    let tl_path = d.join("tl.json");
    std::fs::write(
        &tl_path,
        format!(
            r#"{{ "output": {{ "width": {W}, "height": {H}, "fps": "24000/1001", "gop": 6, "gops_per_chunk": 2 }},
                 "tracks": [ {{ "name": "V1", "clips": [
                   {{ "id": "a", "source": "src.mkv", "start": 0, "duration": "2" }} ]}}] }}"#
        ),
    )
    .unwrap();
    let tl = Timeline::load(&tl_path).unwrap();
    let c = compile(&tl).unwrap();
    let total = tl.frame_count();
    let run = |name: &str, range: Option<FrameRange>| {
        render(
            &tl,
            &c,
            &gpu,
            &d.join(name),
            &RenderOptions {
                jobs: 2,
                range,
                ..RenderOptions::new(d.join("cache"))
            },
        )
        .unwrap()
    };

    let full = run("full.mkv", None);
    assert!(full.range.is_none(), "legacy no-range report has no range");
    assert!(
        full.chunks
            .iter()
            .all(|c| c.plan.source_start_frame.is_none())
    );
    assert_eq!(full.total_frames, total);

    // 17..41: neither end on a chunk boundary (12, 24, 36).
    let r = FrameRange::frames(17, 41).unwrap();
    let part = run("part.mkv", Some(r));
    assert_eq!(part.total_frames, 24);
    let rr = part.range.as_ref().unwrap();
    assert_eq!(rr.source_frames, [17, 41]);
    assert_eq!(rr.output_frames, [0, 24]);
    assert_eq!(rr.timeline_frames, total);
    let layout: Vec<(i64, i64, Option<i64>, ChunkStatus)> = part
        .chunks
        .iter()
        .map(|c| {
            (
                c.plan.start_frame,
                c.plan.frames,
                c.plan.source_start_frame,
                c.status,
            )
        })
        .collect();
    assert_eq!(
        layout,
        [
            (0, 7, Some(17), ChunkStatus::Rendered),  // cut edge: own key
            (7, 12, Some(24), ChunkStatus::Reused),   // full render's chunk 2
            (19, 5, Some(36), ChunkStatus::Rendered), // cut edge
        ]
    );
    assert_eq!(part.chunks[1].plan.key, full.chunks[2].plan.key);

    // Pixels: output frame k == full frame 17 + k, decoded from both files.
    let probe = [0i64, 6, 7, 18, 19, 23];
    let src: Vec<i64> = probe.iter().map(|k| k + 17).collect();
    let (_, a) = decode_frames(&d.join("part.mkv"), &probe, &CancelToken::new()).unwrap();
    let (_, b) = decode_frames(&d.join("full.mkv"), &src, &CancelToken::new()).unwrap();
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(
            x.rgba, y.rgba,
            "output frame {} != source frame {}",
            x.index, y.index
        );
    }
    let (s, _) = decode_frames(&d.join("part.mkv"), &[-1], &CancelToken::new()).unwrap();
    assert_eq!(s.frame_count, Some(24));

    // PCM: exactly program samples [fs(17), fs(41)), no re-mastering.
    let (s0, s1) = (frame_sample(&tl, 17), frame_sample(&tl, 41));
    assert_eq!(rr.source_samples, Some([s0, s1]));
    assert_eq!(rr.output_samples, Some([0, s1 - s0]));
    let pf = read_pcm(&d.join("full.mkv"));
    let pp = read_pcm(&d.join("part.mkv"));
    assert_eq!(pp.len() as i64, (s1 - s0) * 2);
    assert_eq!(pp, pf[(s0 * 2) as usize..(s1 * 2) as usize]);
    assert_eq!(part.audio.as_ref().unwrap().samples, s1 - s0);

    // Out of bounds is refused before rendering.
    let err = render(
        &tl,
        &c,
        &gpu,
        &d.join("bad.mkv"),
        &RenderOptions {
            range: Some(FrameRange {
                start: 10,
                end: total + 1,
            }),
            ..RenderOptions::new(d.join("cache"))
        },
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("ends after the timeline"));
}
