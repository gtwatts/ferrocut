//! What perceive reads from the engine, mirrored as plain serde types so this
//! crate never depends on `ferrocut-engine` (the engine can then depend on
//! it to run analysis after each render). Unknown fields are ignored on
//! purpose: engine additions don't break perceive.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use ferrocut_core::{FrameRate, RationalTime};
use serde::{Deserialize, Serialize};

/// One rendered chunk: `<chunk_dir>/<key>.mkv`, an FFV1 BGRZ master.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub index: usize,
    pub start_frame: i64,
    pub frames: i64,
    /// The engine's chunk key (hash of the chunk's frame Merkle keys + encoder).
    pub key: String,
    /// blake3 of the chunk's mixed audio, when the timeline has audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_blake3: Option<String>,
}

/// The subset of the engine's render report (`<output>.report.json`) we use.
#[derive(Clone, Debug, Deserialize)]
pub struct RenderReport {
    /// The rendered master (video + PCM audio), as the engine was given it.
    #[serde(default)]
    pub output: Option<PathBuf>,
    pub total_frames: i64,
    pub chunk_frames: i64,
    pub chunks: Vec<ChunkRef>,
    /// Where the engine cached this render's chunk masters (one dir per GPU
    /// adapter); absent in older reports (`<cache>/chunks/`).
    #[serde(default)]
    pub chunk_dir: Option<PathBuf>,
    /// Present when the master carries audio.
    #[serde(default)]
    pub audio: Option<EngineAudio>,
}

/// The engine's audio summary for the master.
#[derive(Clone, Debug, Deserialize)]
pub struct EngineAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: i64,
    /// blake3 of the master's interleaved f32le samples (the PCM stream bytes).
    pub blake3: String,
    /// The engine's own measurement (libebur128 port), for cross-checks.
    #[serde(default)]
    pub output: Option<EngineMeasurement>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EngineMeasurement {
    /// `null` (JSON has no -inf) for silence.
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub sample_peak_dbfs: Option<f64>,
}

impl RenderReport {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let t =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_json(&t).with_context(|| format!("parsing render report {}", path.display()))
    }
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }
}

/// The subset of the engine's timeline JSON we use.
#[derive(Clone, Debug, Deserialize)]
pub struct Timeline {
    #[serde(default)]
    pub name: String,
    pub output: Output,
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Output {
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
    #[serde(default)]
    pub duration: Option<RationalTime>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Track {
    pub clips: Vec<Clip>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Clip {
    pub id: String,
    pub start: RationalTime,
    pub duration: RationalTime,
    #[serde(default)]
    pub transition_in: Option<Transition>,
    /// Constant (`"3/4"`, `0.5`) or `{"keyframes": [{"t", "v", ...}]}` with
    /// `t` relative to the clip start. Only fades from/to 0 at the clip's
    /// edges matter here (they are gradual transitions, not cuts).
    #[serde(default)]
    pub opacity: Option<serde_json::Value>,
}

fn num(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => match s.split_once('/') {
            Some((a, b)) => Some(a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?),
            None => s.trim().parse().ok(),
        },
        _ => None,
    }
}

impl Clip {
    /// Opacity keyframes as `(t, value)`, when animated.
    fn keyframes(&self) -> Vec<(RationalTime, f64)> {
        let Some(kf) = self
            .opacity
            .as_ref()
            .and_then(|o| o.get("keyframes"))
            .and_then(|k| k.as_array())
        else {
            return Vec::new();
        };
        kf.iter()
            .filter_map(|k| {
                let t: RationalTime = serde_json::from_value(k.get("t")?.clone()).ok()?;
                Some((t, num(k.get("v")?)?))
            })
            .collect()
    }
    /// Fade-in from opacity 0 at the clip start: when it reaches non-zero.
    pub fn fade_in(&self) -> Option<RationalTime> {
        let k = self.keyframes();
        let first = k.first()?;
        (first.0 <= RationalTime::default() && first.1 == 0.0)
            .then(|| k.iter().find(|x| x.1 != 0.0).map(|x| x.0))
            .flatten()
    }
    /// Fade-out to opacity 0 at the clip end: when it leaves non-zero.
    pub fn fade_out(&self) -> Option<RationalTime> {
        let k = self.keyframes();
        let last = k.last()?;
        (last.0 >= self.duration && last.1 == 0.0)
            .then(|| k.iter().rev().find(|x| x.1 != 0.0).map(|x| x.0))
            .flatten()
    }
}

/// Any transition kind (today the engine has `dissolve`); every kind is
/// treated as a gradual picture change over `duration`.
#[derive(Clone, Debug, Deserialize)]
pub struct Transition {
    pub kind: String,
    pub duration: RationalTime,
}

impl Timeline {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let t =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_json(&t).with_context(|| format!("parsing timeline {}", path.display()))
    }
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }
    pub fn duration(&self) -> RationalTime {
        self.output.duration.unwrap_or_else(|| {
            self.tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(|c| c.start + c.duration))
                .max()
                .unwrap_or_default()
        })
    }
    /// First frame at or after `t` (the engine renders frame i at i / fps).
    pub fn frame_at(&self, t: RationalTime) -> i64 {
        t.frame_ceil(self.output.fps)
    }

    /// Where the timeline intends the picture to change: hard cuts (first
    /// frame of the new shot) and dissolves (`[start, end)` frames), clipped to
    /// `(0, total)`. A clip boundary covered by an opaque dissolve-in is not
    /// a cut. Boundaries hidden by upper tracks still count (we can't know
    /// what's opaque), so `missed` cuts may be invisible-by-design edits.
    pub fn intended(&self, total: i64) -> Intended {
        let mut cuts = Vec::new();
        let mut dissolves = Vec::new();
        for t in &self.tracks {
            let mut clips: Vec<&Clip> = t.clips.iter().collect();
            clips.sort_by_key(|c| c.start);
            for (i, c) in clips.iter().enumerate() {
                let s = self.frame_at(c.start);
                match (&c.transition_in, c.fade_in()) {
                    (Some(Transition { duration, .. }), _) => {
                        dissolves.push((s, self.frame_at(c.start + *duration)));
                    }
                    (None, Some(t)) => dissolves.push((s, self.frame_at(c.start + t))),
                    (None, None) => cuts.push(s),
                }
                let end = c.start + c.duration;
                if let Some(t) = c.fade_out() {
                    dissolves.push((self.frame_at(c.start + t), self.frame_at(end)));
                    continue;
                }
                let covered = clips.get(i + 1).is_some_and(|n| match &n.transition_in {
                    Some(Transition { duration, .. }) => {
                        n.start <= end && end <= n.start + n.duration && n.start + *duration <= end
                    }
                    None => false,
                });
                if !covered {
                    cuts.push(self.frame_at(end));
                }
            }
        }
        cuts.retain(|&f| f > 0 && f < total);
        cuts.sort();
        cuts.dedup();
        dissolves.retain(|&(s, e)| e > s && s < total);
        dissolves.sort();
        dissolves.dedup();
        // A cut that falls inside an intended dissolve is part of it.
        cuts.retain(|&f| !dissolves.iter().any(|&(s, e)| f >= s && f <= e));
        Intended { cuts, dissolves }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intended {
    pub cuts: Vec<i64>,
    /// `(start, end)` frames, end exclusive.
    pub dissolves: Vec<(i64, i64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intended_cuts_and_dissolves() {
        // Engine's own demo timeline (render.rs tests).
        let tl = Timeline::from_json(
            r#"{ "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
              "tracks": [
                { "clips": [
                  { "id": "a", "source": "a.mov", "start": 0, "duration": "2" },
                  { "id": "b", "source": "b.mov", "start": "3/2", "source_in": "1/2", "duration": "2",
                    "transition_in": { "kind": "dissolve", "duration": "1/2" } } ]},
                { "clips": [ { "id": "t", "source": "c.mov", "start": 1, "duration": "1/2", "opacity": "1/2" } ] }
              ] }"#,
        )
        .unwrap();
        assert_eq!(tl.duration(), RationalTime::new(7, 2));
        let i = tl.intended(84);
        // a ends at 48 inside b's dissolve [36, 48] -> no cut; t: 24..36.
        assert_eq!(i.dissolves, vec![(36, 48)]);
        assert_eq!(i.cuts, vec![24]); // 36 (t's end) is the dissolve start
    }

    #[test]
    fn opacity_fades_are_not_cuts() {
        // demo-av's logo: fades in over 1/2 s and out over the last 1/2 s.
        let tl = Timeline::from_json(
            r#"{ "output": { "width": 64, "height": 32, "fps": "24" },
              "tracks": [
                { "clips": [ { "id": "a", "start": 0, "duration": "13" } ] },
                { "clips": [ { "id": "logo", "start": "6", "duration": "3",
                    "opacity": { "keyframes": [
                      { "t": "0", "v": "0" }, { "t": "1/2", "v": "0.8" },
                      { "t": "5/2", "v": "0.8" }, { "t": "3", "v": 0 } ] } },
                  { "id": "t", "start": "10", "duration": "1", "opacity": "1/2" } ] }
              ] }"#,
        )
        .unwrap();
        let i = tl.intended(312);
        assert_eq!(i.dissolves, vec![(144, 156), (204, 216)]);
        assert_eq!(i.cuts, vec![240, 264]);
    }
}
