//! A selected-range render report is refused before any analysis or cache
//! work: its master starts mid-timeline, so grading it against the whole
//! timeline would misplace cuts and durations. No media is read.

use ferrocut_perceive::input::{RenderReport, Timeline};
use ferrocut_perceive::{Options, Request, analyze};

#[test]
fn range_reports_are_not_graded_against_the_whole_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let rr = RenderReport::from_json(
        r#"{"total_frames":24,"chunk_frames":12,"chunks":[],
            "range":{"source_frames":[17,41],"output_frames":[0,24]}}"#,
    )
    .unwrap();
    let tl = Timeline::from_json(
        r#"{"output":{"width":64,"height":32,"fps":"24","gop":12},"tracks":[]}"#,
    )
    .unwrap();
    let cache = dir.path().join("cache");
    let err = analyze(Request {
        timeline: &tl,
        render: &rr,
        cache_dir: &cache,
        out_dir: &dir.path().join("out"),
        audio: None,
        options: Options::default(),
        gpu: None,
    })
    .err()
    .expect("refused");
    assert!(
        format!("{err:#}").contains("selected-range master"),
        "{err:#}"
    );
    assert!(!cache.exists(), "no cache work before the refusal");

    // A report without `range` is unaffected by the field.
    let plain =
        RenderReport::from_json(r#"{"total_frames":0,"chunk_frames":12,"chunks":[]}"#).unwrap();
    assert!(plain.range.is_none());
}
