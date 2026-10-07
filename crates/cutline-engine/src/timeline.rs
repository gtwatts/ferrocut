//! Minimal JSON timeline: tracks of clips with rational in/out points.
//!
//! Times are rationals in seconds, written as `"n"` or `"n/d"` strings (or integers).
//! Track 0 is the bottom layer; higher tracks composite over lower ones.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail, ensure};
use cutline_core::{FrameRate, Rational, RationalTime};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    #[serde(default)]
    pub name: String,
    pub output: OutputSpec,
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSpec {
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
    /// Output GOP length in frames; every GOP is closed and starts with a keyframe.
    #[serde(default = "default_gop")]
    pub gop: u32,
    /// Chunk size for parallel rendering, in GOPs.
    #[serde(default = "default_gops_per_chunk")]
    pub gops_per_chunk: u32,
    /// Optional explicit duration; defaults to the end of the last clip.
    #[serde(default)]
    pub duration: Option<RationalTime>,
}

fn default_gop() -> u32 {
    24
}
fn default_gops_per_chunk() -> u32 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    #[serde(default)]
    pub name: String,
    pub clips: Vec<Clip>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: String,
    /// Media path, relative to the timeline file.
    pub source: PathBuf,
    /// Position on the timeline.
    pub start: RationalTime,
    /// In point in the source.
    #[serde(default)]
    pub source_in: RationalTime,
    pub duration: RationalTime,
    #[serde(default = "one")]
    pub opacity: Rational,
    /// Transition from the previous clip on the same track. The previous clip
    /// must overlap this one by at least the transition duration (handles).
    #[serde(default)]
    pub transition_in: Option<Transition>,
}

fn one() -> Rational {
    Rational::ONE
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transition {
    Dissolve { duration: RationalTime },
}

impl Clip {
    pub fn end(&self) -> RationalTime {
        self.start + self.duration
    }
    pub fn dissolve(&self) -> Option<RationalTime> {
        self.transition_in
            .as_ref()
            .map(|Transition::Dissolve { duration }| *duration)
    }
}

impl Timeline {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut tl: Timeline = serde_json::from_str(&text)
            .with_context(|| format!("parsing timeline {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        for t in &mut tl.tracks {
            for c in &mut t.clips {
                if c.source.is_relative() {
                    c.source = base.join(&c.source);
                }
            }
        }
        tl.validate()?;
        Ok(tl)
    }

    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        let tl: Timeline = serde_json::from_str(text)?;
        tl.validate()?;
        Ok(tl)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let o = &self.output;
        ensure!(o.width > 0 && o.height > 0, "output size must be non-zero");
        ensure!(o.fps > Rational::ZERO, "fps must be positive");
        ensure!(
            o.gop > 0 && o.gops_per_chunk > 0,
            "gop and gops_per_chunk must be positive"
        );
        ensure!(!self.tracks.is_empty(), "timeline has no tracks");
        for (ti, track) in self.tracks.iter().enumerate() {
            let mut sorted: Vec<&Clip> = track.clips.iter().collect();
            sorted.sort_by_key(|c| c.start);
            for c in &sorted {
                ensure!(
                    c.duration.seconds() > Rational::ZERO,
                    "clip {}: duration must be positive",
                    c.id
                );
                ensure!(
                    c.start >= RationalTime::ZERO,
                    "clip {}: start must be >= 0",
                    c.id
                );
                ensure!(
                    c.source_in >= RationalTime::ZERO,
                    "clip {}: source_in must be >= 0",
                    c.id
                );
                ensure!(
                    c.opacity >= Rational::ZERO && c.opacity <= Rational::ONE,
                    "clip {}: opacity must be in [0, 1]",
                    c.id
                );
            }
            for pair in sorted.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                match b.dissolve() {
                    Some(d) => {
                        ensure!(
                            d.seconds() > Rational::ZERO,
                            "clip {}: dissolve duration must be positive",
                            b.id
                        );
                        ensure!(
                            d <= b.duration,
                            "clip {}: dissolve longer than the clip",
                            b.id
                        );
                        if b.start + d > a.end() {
                            bail!(
                                "track {ti}: dissolve into {} needs {} of overlap with {} (handles), has {}",
                                b.id,
                                d,
                                a.id,
                                if a.end() > b.start {
                                    a.end() - b.start
                                } else {
                                    RationalTime::ZERO
                                }
                            );
                        }
                    }
                    None => ensure!(
                        b.start >= a.end(),
                        "track {ti}: clips {} and {} overlap without a transition",
                        a.id,
                        b.id
                    ),
                }
            }
        }
        Ok(())
    }

    pub fn duration(&self) -> RationalTime {
        self.output.duration.unwrap_or_else(|| {
            self.tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(Clip::end))
                .max()
                .unwrap_or(RationalTime::ZERO)
        })
    }

    /// Number of output frames: every frame whose start time is before the end.
    pub fn frame_count(&self) -> i64 {
        self.duration().frame_ceil(self.output.fps)
    }

    pub fn chunk_frames(&self) -> i64 {
        self.output.gop as i64 * self.output.gops_per_chunk as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const DEMO: &str = r#"{
      "name": "t",
      "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
      "tracks": [
        { "clips": [
          { "id": "a", "source": "a.mov", "start": 0, "duration": "2" },
          { "id": "b", "source": "b.mov", "start": "3/2", "source_in": "1/2", "duration": "2",
            "transition_in": { "kind": "dissolve", "duration": "1/2" } }
        ]},
        { "clips": [ { "id": "t", "source": "c.mov", "start": 1, "duration": "1/2", "opacity": "1/2" } ] }
      ]
    }"#;

    #[test]
    fn parses_and_measures() {
        let tl = Timeline::from_json(DEMO).unwrap();
        assert_eq!(tl.duration(), RationalTime::new(7, 2));
        assert_eq!(tl.frame_count(), 84);
        assert_eq!(tl.chunk_frames(), 12);
        assert_eq!(
            tl.tracks[0].clips[1].dissolve(),
            Some(RationalTime::new(1, 2))
        );
    }

    #[test]
    fn rejects_dissolve_without_handles() {
        let bad = DEMO.replace(r#""start": "3/2""#, r#""start": "7/4""#);
        let err = Timeline::from_json(&bad).unwrap_err().to_string();
        assert!(err.contains("handles"), "{err}");
    }

    #[test]
    fn rejects_overlap_without_transition() {
        let bad = DEMO.replace(
            r#""transition_in": { "kind": "dissolve", "duration": "1/2" }"#,
            r#""opacity": 1"#,
        );
        assert!(
            Timeline::from_json(&bad)
                .unwrap_err()
                .to_string()
                .contains("overlap")
        );
    }

    #[test]
    fn rejects_unknown_fields_and_float_time() {
        assert!(
            Timeline::from_json(&DEMO.replace(r#""gop": 12"#, r#""gop": 12, "bogus": 1"#)).is_err()
        );
        assert!(
            Timeline::from_json(&DEMO.replace(r#""duration": "2" }"#, r#""duration": 2.5 }"#))
                .is_err()
        );
    }
}
