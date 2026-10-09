//! Stills: frame selection, rendered pixels against the generator's encoded
//! color, PNG output and the labeled contact sheet. GPU work runs on the
//! software adapter and skips (with a note) when none is available.

use std::sync::OnceLock;

use ferrocut_core::{AdapterPreference, CancelToken, GpuContext, RationalTime, SharedGpu};
use ferrocut_engine::compile::compile;
use ferrocut_engine::media::decode::Decoder;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::preview::{
    check_prefix, contact_sheet, fit_within, frame_at, parse_time, png_within, render_stills,
    select_frames, spread, write_stills,
};
use ferrocut_engine::{RenderOptions, Timeline, render};

const W: u32 = 64;
const H: u32 = 36;

/// A solid whose red channel ramps 0 -> 1 over the 2 s timeline (source time).
const TL: &str = r#"{
  "output": {"width": 64, "height": 36, "fps": 24, "duration": 2, "gop": 12},
  "tracks": [{"name": "V", "clips": [
    {"id": "bg", "start": 0, "duration": 2, "generator": {"type": "solid",
     "color": [{"keyframes": [{"t": 0, "v": 0}, {"t": 2, "v": 1}]}, "1/2", "1/4", 1]}}
  ]}]
}"#;

fn cpu() -> Option<&'static GpuContext> {
    static G: OnceLock<Option<GpuContext>> = OnceLock::new();
    G.get_or_init(|| match GpuContext::new(AdapterPreference::Cpu) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("SKIP: no software adapter ({e})");
            None
        }
    })
    .as_ref()
}

#[test]
fn frame_selection_is_exact_sorted_and_bounded() {
    let tl = Timeline::from_json(TL).unwrap();
    assert_eq!(tl.frame_count(), 48);
    assert_eq!(spread(&tl, 4), [6, 18, 30, 42]);
    assert_eq!(spread(&tl, 1), [24]);
    // Never more stills than frames: 48 distinct frames, not MAX_STILLS.
    assert_eq!(spread(&tl, 200).len(), 48);
    assert_eq!(frame_at(&tl, RationalTime::new(1, 2)).unwrap(), 12);
    assert_eq!(frame_at(&tl, parse_time("47/24").unwrap()).unwrap(), 47);
    assert!(frame_at(&tl, RationalTime::new(2, 1)).is_err());
    assert!(parse_time("0.5x").is_err());
    let picked = select_frames(&tl, &[parse_time("1.5").unwrap()], &[47, 0, 36], None).unwrap();
    assert_eq!(picked, [0, 36, 47]);
    assert_eq!(select_frames(&tl, &[], &[], None).unwrap().len(), 12);
    assert_eq!(select_frames(&tl, &[], &[5], Some(2)).unwrap(), [5, 12, 36]);
    assert!(select_frames(&tl, &[], &[48], None).is_err());
    assert!(select_frames(&tl, &[], &[], Some(0)).is_err());
}

#[test]
fn stills_hold_the_master_pixels_and_sheets_are_labeled() {
    let Some(gpu) = cpu() else { return };
    let tl = Timeline::from_json(TL).unwrap();
    let c = compile(&tl).unwrap();
    let stills = render_stills(&tl, &c, gpu, &[0, 24, 47], &CancelToken::new()).unwrap();
    assert_eq!(stills.len(), 3);
    for (s, red) in stills.iter().zip([0.0f64, 0.5, 47.0 / 48.0]) {
        assert_eq!(
            (s.width, s.height, s.rgba.len()),
            (W, H, (W * H * 4) as usize)
        );
        // Center pixel: the solid's encoded Rec.709 color round-trips through
        // the linear working space back to 8 bits within one code.
        let i = ((H / 2 * W + W / 2) * 4) as usize;
        let want = [red * 255.0, 127.5, 63.75];
        for (k, want) in want.iter().enumerate() {
            let got = s.rgba[i + k] as f64;
            assert!(
                (got - want).abs() <= 1.0,
                "frame {} channel {k}: {got} vs {want}",
                s.frame
            );
        }
        assert_eq!(s.rgba[i + 3], 255);
    }

    let (sw, sh, sheet) = contact_sheet(&tl, &stills, 2, 32).unwrap();
    // 2 columns of 32 px cells (18 px tall at 16:9) with 4 px gaps, 2 rows.
    assert_eq!((sw, sh), (2 * 32 + 3 * 4, 2 * 18 + 3 * 4));
    assert_eq!(sheet.len(), (sw * sh * 4) as usize);
    // The label box darkens the cell's top-left corner; the cell's lower
    // right keeps the frame's color (frame 24: red 128).
    let px = |x: u32, y: u32| {
        let i = ((y * sw + x) * 4) as usize;
        [sheet[i], sheet[i + 1], sheet[i + 2]]
    };
    let labeled = px(4 + 2, 4 + 2);
    let plain = px(4 + 30, 4 + 16);
    assert!(labeled[1] < plain[1], "{labeled:?} vs {plain:?}");
    assert!((plain[1] as i32 - 128).abs() <= 2, "{plain:?}");

    let dir = tempfile::tempdir().unwrap();
    let r = write_stills(&tl, &stills, dir.path(), "t", true, Some((3, 40))).unwrap();
    assert_eq!(r.frames.len(), 3);
    assert_eq!(r.frames[1].time, "1");
    assert_eq!(r.frames[1].timecode, "0:01.00 f24");
    assert_eq!(r.frames[2].time, "47/24");
    for f in &r.frames {
        let p = f.path.as_ref().unwrap();
        assert_eq!(
            p.file_name().unwrap().to_str().unwrap(),
            format!("t-f{:05}.png", f.frame)
        );
        let pm = tiny_skia::Pixmap::load_png(p).unwrap();
        assert_eq!((pm.width(), pm.height()), (W, H));
    }
    let sheet = tiny_skia::Pixmap::load_png(r.sheet.as_ref().unwrap()).unwrap();
    assert_eq!(
        (sheet.width(), sheet.height()),
        (3 * 40 + 4 * 4, 22 + 2 * 4)
    );

    let (fw, fh, small) = fit_within(&stills[0].rgba, W, H, 32);
    assert_eq!((fw, fh, small.len()), (32, 18, 32 * 18 * 4));
    let (fw, fh, _) = fit_within(&stills[0].rgba, W, H, 1000);
    assert_eq!((fw, fh), (W, H));
}

#[test]
fn prefixes_cannot_escape_and_inline_pngs_respect_a_budget() {
    for ok in ["t", "my-cut.v2", "A_1"] {
        check_prefix(ok).unwrap();
    }
    for bad in ["", ".x", "../x", "/tmp/x", "a/b", "a b", &"x".repeat(65)] {
        assert!(check_prefix(bad).is_err(), "{bad:?}");
    }
    // Noise compresses badly: a 512x512 noise image is far above 4 KB, so
    // it is halved down to the 256 px floor.
    let mut seed = 12345u32;
    let noise: Vec<u8> = (0..512 * 512 * 4)
        .map(|i| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            if i % 4 == 3 { 255 } else { (seed >> 24) as u8 }
        })
        .collect();
    let (w, h, png) = png_within(512, 512, &noise, 4096).unwrap();
    assert_eq!((w, h), (256, 256));
    assert!(
        png.len() > 4096,
        "still over budget at the floor, by design"
    );
    let (w, h, png) = png_within(512, 512, &noise, usize::MAX).unwrap();
    assert_eq!((w, h), (512, 512));
    assert!(png.starts_with(b"\x89PNG"));
}

/// A 50 px wide source: 200-byte rows, padded to 256 in the readback, so the
/// stride handling in the sink is exercised.
fn synth(path: &std::path::Path, frames: i64) {
    let (w, h) = (50u32, 30u32);
    let mut e = ChunkEncoder::create(
        path,
        &EncodeSettings {
            width: w,
            height: h,
            fps: ferrocut_core::Rational::new(24, 1),
            gop: 12,
        },
    )
    .unwrap();
    let mut px = vec![0u8; (w * h * 4) as usize];
    for f in 0..frames {
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                px[i] = (x as i64 * 5 + f * 7) as u8;
                px[i + 1] = (y as i64 * 8 + f * 3) as u8;
                px[i + 2] = ((x / 10 + y / 10) * 40) as u8;
                px[i + 3] = 255;
            }
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

/// Stills of a timeline with media and a composited overlay hold exactly
/// the pixels of the master render at the same frames.
#[test]
fn stills_match_the_master_render_on_media() {
    let Some(gpu) = cpu() else { return };
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    synth(&d.join("src.mkv"), 24);
    let tl = r#"{
      "output": {"width": 50, "height": 30, "fps": 24, "duration": 1, "gop": 12},
      "tracks": [
        {"name": "V", "clips": [{"id": "a", "source": "src.mkv", "start": 0, "duration": 1}]},
        {"name": "Fx", "clips": [{"id": "s", "start": "1/2", "duration": "1/2", "opacity": "1/2",
          "generator": {"type": "solid", "color": ["1", "1/2", "0", 1]}}]}
      ]
    }"#;
    std::fs::write(d.join("tl.json"), tl).unwrap();
    let tl = Timeline::load(&d.join("tl.json")).unwrap();
    let c = compile(&tl).unwrap();
    let frames = [0i64, 11, 12, 23];
    let stills = render_stills(&tl, &c, gpu, &frames, &CancelToken::new()).unwrap();
    assert!(stills[0].rgba != stills[3].rgba, "frames differ");

    let shared = SharedGpu::new(GpuContext::new(AdapterPreference::Cpu).unwrap());
    let master = d.join("master.mkv");
    let opts = RenderOptions {
        jobs: 2,
        ..RenderOptions::new(d.join("cache"))
    };
    render(&tl, &c, &shared, &master, &opts).unwrap();
    let mut dec = Decoder::open(&master, 50, 30).unwrap();
    for s in &stills {
        let t = RationalTime::from_frames(s.frame, tl.output.fps);
        let got = dec.frame_at(t).unwrap();
        assert_eq!(got.len(), s.rgba.len());
        let worst = got
            .iter()
            .zip(&s.rgba)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(worst <= 1, "frame {}: max difference {worst}", s.frame);
    }
    // The overlay is visible in the second half only.
    let mid = ((15 * 50 + 25) * 4) as usize;
    assert_ne!(stills[1].rgba[mid..mid + 3], stills[2].rgba[mid..mid + 3]);
}
