//! The in-process shot-detector registry, alone in its own test binary.
//!
//! `shots::register` sets a process-global, first-wins detector whose id is
//! part of the media-index cache key. Run in the same process as index cache
//! tests that request shots, a registration landing between an index and its
//! cached lookup changes the key (PR 8 CI run 38009480085:
//! `legacy_cached_transcription_errors_are_retried`). Keep this the only test
//! in this file, and never register a detector in `tests/index.rs`.

use std::path::Path;

use ferrocut_core::{BoundaryKind, RationalTime, ShotBoundary, TimeRange};
use ferrocut_engine::index::shots;

fn rt(n: i64, d: i64) -> RationalTime {
    RationalTime::new(n, d)
}

fn fake_detector(
    _: &Path,
    range: Option<TimeRange>,
) -> Result<Vec<ShotBoundary>, ferrocut_core::NodeError> {
    let all = vec![
        ShotBoundary {
            at: rt(2, 1),
            span: None,
            kind: BoundaryKind::Cut,
            confidence: 0.9,
        },
        ShotBoundary {
            at: rt(5, 1),
            span: Some((rt(9, 2), rt(11, 2))),
            kind: BoundaryKind::Dissolve,
            confidence: 0.6,
        },
    ];
    Ok(all
        .into_iter()
        .filter(|b| range.is_none_or(|r| r.contains(b.at)))
        .collect())
}

#[test]
fn shot_hook_uses_the_registered_detector() {
    // This binary's only registration (see the module docs).
    assert!(shots::register("fake", fake_detector));
    assert!(!shots::register("again", fake_detector), "first wins");
    assert_eq!(shots::detector_id().as_deref(), Some("in-process:fake"));
    let b = shots::detect_shots(Path::new("x.mkv"), None, &shots::ShotOptions::default()).unwrap();
    assert_eq!(b.len(), 2);
    assert_eq!(b[1].span, Some((rt(9, 2), rt(11, 2))));
    let b = shots::detect_shots(
        Path::new("x.mkv"),
        Some(TimeRange::new(rt(4, 1), rt(2, 1))),
        &shots::ShotOptions::default(),
    )
    .unwrap();
    assert_eq!(b.len(), 1);
}
