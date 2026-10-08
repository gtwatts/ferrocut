//! Shot boundaries in a media file: the result type of shot detection.
//!
//! Agreed Oct 7 2026 between Rusty (engine/index) and SeePlus (detector):
//!
//! ```text
//! // ferrocut-perceive (SeePlus)
//! pub fn detect_shots(path: &Path, range: Option<TimeRange>, opts: &ShotOptions)
//!     -> Result<Vec<ShotBoundary>, NodeError>;
//! ```
//!
//! The types live here so the engine's media index (which ferrocut-perceive
//! depends on, so it cannot depend back) and the detector share them exactly;
//! `ShotOptions` is the detector's own tuning and stays in ferrocut-perceive.
//! Times are source times of the file (the same origin clips' `source_in` use).

use serde::{Deserialize, Serialize};

use crate::time::RationalTime;

/// How one shot turns into the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryKind {
    Cut,
    Dissolve,
    FadeIn,
    FadeOut,
}

/// One boundary between shots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotBoundary {
    /// The cut point, or a transition's midpoint.
    pub at: RationalTime,
    /// `[start, end)` of a gradual transition; `None` for hard cuts.
    pub span: Option<(RationalTime, RationalTime)>,
    pub kind: BoundaryKind,
    /// 0.0..=1.0.
    pub confidence: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape() {
        let b = ShotBoundary {
            at: RationalTime::new(5, 2),
            span: Some((RationalTime::new(2, 1), RationalTime::new(3, 1))),
            kind: BoundaryKind::FadeOut,
            confidence: 0.5,
        };
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "at": "5/2", "span": ["2", "3"], "kind": "fade_out", "confidence": 0.5 })
        );
        assert_eq!(serde_json::from_value::<ShotBoundary>(v).unwrap(), b);
    }
}
