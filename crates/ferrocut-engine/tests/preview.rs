//! Stills: frame selection, rendered pixels against the generator's encoded
//! color, PNG output and the labeled contact sheet. GPU work runs on the
//! software adapter and skips (with a note) when none is available.

use std::sync::OnceLock;

use ferrocut_core::{AdapterPreference, CancelToken, GpuContext, RationalTime};
use ferrocut_engine::Timeline;
use ferrocut_engine::compile::compile;
use ferrocut_engine::preview::{
    contact_sheet, fit_within, frame_at, parse_time, render_stills, select_frames, spread,
    write_stills,
};

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

    let (sw, sh, sheet) = contact_sheet(&tl, &stills, 2, 32);
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
