//! MCP captions_import: frame-grid timing, journaled edits and root checks.

use std::path::{Path, PathBuf};

use ferrocut_core::RationalTime;
use ferrocut_engine::{Timeline, captions};
use ferrocut_mcp::{Ctx, Root, call};
use serde_json::{Value, json};

fn run(cx: &Ctx, args: Value) -> Result<Value, String> {
    call(cx, "captions_import", args)
        .expect("registered tool")
        .map_err(|e| format!("{e:#}"))
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    cx: Ctx,
}

// explainer-16x9 cues 1-2: at 30 fps frame 72 falls in the 33 ms gap.
const SRT: &str = "1\n00:00:00,589 --> 00:00:02,384\nAsk an agent\n\n2\n00:00:02,417 --> 00:00:03,965\nand it glues one together.\n";

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let outside = temp.path().join("outside");
        for d in [&root, &outside, &root.join("fonts")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let font = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ferrocut-engine/tests/data/text/NotoSans-Regular.ttf");
        std::fs::copy(&font, root.join("fonts/caption.ttf")).unwrap();
        std::fs::copy(&font, outside.join("caption.ttf")).unwrap();
        std::fs::write(
            root.join("tl.json"),
            r#"{"output":{"width":320,"height":180,"fps":"30"},"tracks":[{"name":"Picture","clips":[]}]}"#,
        )
        .unwrap();
        std::fs::write(root.join("cues.srt"), SRT).unwrap();
        std::fs::write(outside.join("cues.srt"), SRT).unwrap();
        let cx = Ctx::new(Root::new(&root).unwrap());
        Fixture {
            _temp: temp,
            root,
            outside,
            cx,
        }
    }
    fn tl(&self) -> Timeline {
        Timeline::load(&self.root.join("tl.json")).unwrap()
    }
}

fn style(font: &str) -> Value {
    json!({"content":"", "font":font, "font_size":"24", "align":"center"})
}

fn blank_frames(cues: &[captions::CaptionCue], fps: ferrocut_core::Rational) -> Vec<i64> {
    (cues[0].start.frame_ceil(fps)..cues.last().unwrap().end.frame_ceil(fps))
        .filter(|&n| {
            let at = RationalTime::from_frames(n, fps);
            !cues.iter().any(|c| c.start <= at && at < c.end)
        })
        .collect()
}

#[test]
fn imports_gap_free_cues_with_a_report_through_the_journal() {
    let f = Fixture::new();
    let before = std::fs::read(f.root.join("tl.json")).unwrap();
    let args =
        json!({"timeline":"tl.json","subtitles":"cues.srt","style":style("fonts/caption.ttf")});

    let mut dry = args.clone();
    dry["dry_run"] = json!(true);
    let v = run(&f.cx, dry).unwrap();
    assert_eq!(v["written"], json!(false));
    assert_eq!(std::fs::read(f.root.join("tl.json")).unwrap(), before);
    let changed = &v["caption_timing"]["changed"];
    assert_eq!(changed[0]["reasons"], json!(["snapped", "closed_gap"]));
    assert_eq!(changed[0]["frames"], json!([18, 72]));
    assert_eq!(changed[0]["from"], json!(["589/1000", "298/125"]));

    let v = run(&f.cx, args).unwrap();
    assert_eq!(v["written"], json!(true));
    let tl = f.tl();
    let cues = captions::from_track(&tl, "Captions").unwrap();
    assert!(blank_frames(&cues, tl.output.fps).is_empty());
    assert_eq!(cues[0].end, RationalTime::new(73, 30));
    // Fonts stay as given: relative to the timeline.
    let clip = serde_json::to_value(&tl.tracks[1].clips[0]).unwrap();
    assert_eq!(
        clip["generator"]["text"]["font"],
        json!("fonts/caption.ttf")
    );

    let undone = call(&f.cx, "undo", json!({"timeline":"tl.json"})).unwrap();
    assert!(undone.is_ok(), "{undone:?}");
    assert_eq!(f.tl().tracks.len(), 1);
}

#[test]
fn exact_timing_keeps_the_blink_and_says_so() {
    let f = Fixture::new();
    let v = run(
        &f.cx,
        json!({"timeline":"tl.json","subtitles":"cues.srt","style":style("fonts/caption.ttf"),
            "timing":{"snap":false,"close_gaps":0}}),
    )
    .unwrap();
    let report = &v["caption_timing"];
    assert_eq!(report["changed"], json!([]));
    assert_eq!(report["gaps_kept"][0]["frames"], json!([72, 72]));
    assert!(report["warnings"][0].as_str().unwrap().contains("blink"));
    let tl = f.tl();
    let cues = captions::from_track(&tl, "Captions").unwrap();
    assert_eq!(blank_frames(&cues, tl.output.fps), vec![72]);
}

#[test]
fn paths_outside_the_root_are_refused_and_nothing_is_written() {
    let f = Fixture::new();
    let before = std::fs::read(f.root.join("tl.json")).unwrap();
    let outside_srt = f.outside.join("cues.srt");
    let outside_font = f.outside.join("caption.ttf");
    for (args, what) in [
        (
            json!({"timeline":"tl.json","subtitles":outside_srt,"style":style("fonts/caption.ttf")}),
            "outside the project root",
        ),
        (
            json!({"timeline":"tl.json","subtitles":"../outside/cues.srt","style":style("fonts/caption.ttf")}),
            "outside the project root",
        ),
        (
            json!({"timeline":"tl.json","subtitles":"cues.srt","style":style(outside_font.to_str().unwrap())}),
            "outside the project root",
        ),
        (
            json!({"timeline":"tl.json","subtitles":"cues.srt","style":style("../outside/caption.ttf")}),
            "outside the project root",
        ),
        (
            json!({"timeline":"tl.json","subtitles":"cues.srt","style":style("fonts/caption.ttf"),"output":outside_srt.with_extension("json")}),
            "outside the project root",
        ),
        (
            json!({"timeline":"tl.json","subtitles":"cues.srt","style":style("fonts/caption.ttf"),"timing":{"close_gaps":"5"}}),
            "close_gaps",
        ),
        (
            json!({"timeline":"tl.json","subtitles":"cues.srt","style":style("fonts/caption.ttf"),"timing":{"snapp":true}}),
            "invalid arguments",
        ),
    ] {
        let err = run(&f.cx, args.clone()).unwrap_err();
        assert!(err.contains(what), "{args}: {err}");
        assert_eq!(std::fs::read(f.root.join("tl.json")).unwrap(), before);
    }
    assert!(!f.outside.join("cues.json").exists());
}
