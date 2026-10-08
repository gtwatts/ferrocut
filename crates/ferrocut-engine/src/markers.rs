//! Markers: named, colored annotations on the timeline or on clips (Premiere
//! sequence / clip markers). Agents use them to note beats, shots, problems
//! and decisions; they never change the picture or sound, so they are not
//! part of any frame key.
//!
//! - Timeline markers (`timeline.markers`): `time` in timeline seconds. They
//!   stay where they are when clips move (ripple edits don't move them).
//! - Clip markers (`clip.markers`, video and audio clips): `time` in the
//!   clip's **source time** (like generator keyframes), so they stay on the
//!   frame they mark through trims, splits, slips of the neighbours and
//!   moves; after a split both parts keep the list and each shows the
//!   markers inside its own source range.

use ferrocut_core::{Rational, RationalTime};
use serde::{Deserialize, Serialize};

use crate::retime::TimeMap;

/// Premiere's marker colors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerColor {
    #[default]
    Green,
    Red,
    Purple,
    Orange,
    Yellow,
    White,
    Blue,
    Cyan,
}

impl MarkerColor {
    pub const ALL: [&'static str; 8] = [
        "green", "red", "purple", "orange", "yellow", "white", "blue", "cyan",
    ];
    pub fn is_default(&self) -> bool {
        *self == MarkerColor::Green
    }
}

fn is_zero_time(t: &RationalTime) -> bool {
    *t == RationalTime::ZERO
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    /// Unique within its list (the timeline's, or one clip's).
    pub id: String,
    /// Timeline seconds (timeline markers) or source seconds (clip markers).
    pub time: RationalTime,
    /// Length of a range marker; 0 = a point marker.
    #[serde(default, skip_serializing_if = "is_zero_time")]
    pub duration: RationalTime,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "MarkerColor::is_default")]
    pub color: MarkerColor,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
}

/// Check one marker list (`what` names it in errors).
pub fn validate(list: &[Marker], what: &str) -> anyhow::Result<()> {
    let mut ids = std::collections::HashSet::new();
    for m in list {
        anyhow::ensure!(!m.id.is_empty(), "{what}: marker id must not be empty");
        anyhow::ensure!(
            ids.insert(m.id.as_str()),
            "{what}: duplicate marker id {:?}",
            m.id
        );
        anyhow::ensure!(
            m.time >= RationalTime::ZERO,
            "{what}: marker {:?} time must be >= 0",
            m.id
        );
        anyhow::ensure!(
            m.duration >= RationalTime::ZERO,
            "{what}: marker {:?} duration must be >= 0",
            m.id
        );
    }
    Ok(())
}

/// A fresh id `m1`, `m2`, ... not used in `list`.
pub fn fresh_id(list: &[Marker]) -> String {
    (1..)
        .map(|i| format!("m{i}"))
        .find(|id| !list.iter().any(|m| m.id == *id))
        .expect("unbounded")
}

/// Where a clip marker shows on the timeline: the clip's start, source
/// range and time map.
#[derive(Clone, Debug)]
pub struct ClipPlacement {
    pub start: RationalTime,
    pub duration: RationalTime,
    pub map: TimeMap,
}

impl ClipPlacement {
    /// Source time shown at timeline time `t` (which must be in the clip).
    pub fn source_at(&self, t: RationalTime) -> RationalTime {
        self.map.source_at(t - self.start)
    }

    /// Timeline time at which source time `s` shows, when the clip plays at
    /// a constant non-zero speed and `s` is inside the clip (`None`
    /// otherwise: outside the clip's range, frozen or ramped).
    pub fn timeline_at(&self, s: RationalTime) -> Option<RationalTime> {
        let speed = self.map.constant_speed()?;
        if speed == Rational::ZERO {
            return None;
        }
        let s0 = self.map.source_at(RationalTime::ZERO);
        let u = (s.0 - s0.0) / speed;
        (u >= Rational::ZERO && u < self.duration.0).then(|| RationalTime(self.start.0 + u))
    }
}

/// One marker as listed for agents (`markers_list`): timeline time for
/// both kinds.
#[derive(Clone, Debug, Serialize)]
pub struct ListedMarker {
    /// `timeline` or `clip`.
    pub scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    pub id: String,
    pub name: String,
    pub color: MarkerColor,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// Timeline time (`null` for a clip marker outside the clip's range or
    /// on a ramped / frozen clip).
    pub time: Option<RationalTime>,
    pub duration: RationalTime,
    /// Clip markers: the source time stored in the marker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_time: Option<RationalTime>,
}

/// Every marker of the timeline, timeline markers first, then clip markers
/// (tracks bottom to top, clips by start), each sorted by time.
pub fn list(tl: &crate::Timeline) -> Vec<ListedMarker> {
    let mut out: Vec<ListedMarker> = tl
        .markers
        .iter()
        .map(|m| ListedMarker {
            scope: "timeline",
            clip: None,
            track: None,
            id: m.id.clone(),
            name: m.name.clone(),
            color: m.color,
            comment: m.comment.clone(),
            time: Some(m.time),
            duration: m.duration,
            source_time: None,
        })
        .collect();
    out.sort_by_key(|m| m.time);
    let clip =
        |track: &str, id: &str, p: ClipPlacement, ms: &[Marker], out: &mut Vec<ListedMarker>| {
            let mut v: Vec<ListedMarker> = ms
                .iter()
                .map(|m| ListedMarker {
                    scope: "clip",
                    clip: Some(id.to_string()),
                    track: Some(track.to_string()),
                    id: m.id.clone(),
                    name: m.name.clone(),
                    color: m.color,
                    comment: m.comment.clone(),
                    time: p.timeline_at(m.time),
                    duration: m.duration,
                    source_time: Some(m.time),
                })
                .collect();
            v.sort_by_key(|m| (m.time.is_none(), m.time, m.source_time));
            out.extend(v);
        };
    for t in &tl.tracks {
        let mut cs: Vec<_> = t.clips.iter().collect();
        cs.sort_by_key(|c| c.start);
        for c in cs {
            let p = ClipPlacement {
                start: c.start,
                duration: c.duration,
                map: c.time_map(),
            };
            clip(&t.name, &c.id, p, &c.markers, &mut out);
        }
    }
    for t in &tl.audio_tracks {
        let mut cs: Vec<_> = t.clips.iter().collect();
        cs.sort_by_key(|c| c.start);
        for c in cs {
            let p = ClipPlacement {
                start: c.start,
                duration: c.duration,
                map: c.time_map(),
            };
            clip(&t.name, &c.id, p, &c.markers, &mut out);
        }
    }
    out
}
