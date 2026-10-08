//! JSON timeline: video tracks of linked A/V clips, audio-only tracks, and
//! audio bus/master settings, all with exact rational times.
//!
//! Times and numeric parameters are exact rationals: `"n"`, `"n/d"` or exact
//! decimal strings (`"0.5"`), or integers; JSON floats are rejected.
//! Track 0 is the bottom layer; higher tracks composite over lower ones.
//!
//! Audio: a video clip whose source has an audio stream plays that audio on
//! its track's audio bus. `audio.in_offset` / `audio.out_offset` move the
//! audio in/out points independently of the video (negative `in_offset` =
//! audio starts early = J-cut; positive `out_offset` = audio runs past the
//! video cut = L-cut); the source mapping stays linked (sync is kept).
//! `audio_tracks` hold audio-only clips (music, dialogue). Overlapping audio
//! on a track sums; `crossfade_in` fades the previous clip out under this one.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail, ensure};
use ferrocut_audio::FadeCurve;
use ferrocut_core::{Animatable, FrameRate, Rational, RationalTime};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    #[serde(default)]
    pub name: String,
    pub output: OutputSpec,
    pub tracks: Vec<Track>,
    /// Audio-only tracks, mixed after the video tracks' audio buses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_tracks: Vec<AudioTrack>,
    #[serde(default, skip_serializing_if = "AudioSettings::is_default")]
    pub audio: AudioSettings,
}

/// Project audio format and master bus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSettings {
    /// Project sample rate; all sources are resampled to it.
    #[serde(default = "default_rate")]
    pub sample_rate: u32,
    /// Master gain in dB (keyframes in timeline time), before normalization.
    #[serde(default, skip_serializing_if = "is_zero_anim")]
    pub master_gain_db: Animatable,
    /// Two-pass loudness normalization of the master + true-peak limiter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness: Option<LoudnessSpec>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings {
            sample_rate: default_rate(),
            master_gain_db: Animatable::default(),
            loudness: None,
        }
    }
}

impl AudioSettings {
    pub fn is_default(&self) -> bool {
        *self == AudioSettings::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoudnessSpec {
    /// Integrated loudness target: -23 (EBU R128) or -14 (streaming), LUFS.
    pub target_lufs: Rational,
    /// True-peak ceiling, dBTP.
    #[serde(default = "default_tp")]
    pub true_peak_dbtp: Rational,
}

fn default_rate() -> u32 {
    48_000
}
fn default_tp() -> Rational {
    Rational::from_int(-1)
}
pub(crate) fn is_zero_anim(a: &Animatable) -> bool {
    matches!(a, Animatable::Constant(v) if v.is_zero())
}
fn is_zero_time(t: &RationalTime) -> bool {
    *t == RationalTime::ZERO
}
fn is_false(b: &bool) -> bool {
    !*b
}

/// Sidechain ducking: this bus is turned down while the `key` buses are loud.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuckSpec {
    /// Names of the key tracks (video or audio tracks).
    pub key: Vec<String>,
    #[serde(default = "default_threshold")]
    pub threshold_db: Rational,
    #[serde(default = "default_ratio")]
    pub ratio: Rational,
    #[serde(default = "default_attack")]
    pub attack_ms: Rational,
    #[serde(default = "default_release")]
    pub release_ms: Rational,
    /// Maximum reduction, dB (positive).
    #[serde(default = "default_range")]
    pub range_db: Rational,
}

fn default_threshold() -> Rational {
    Rational::from_int(-30)
}
fn default_ratio() -> Rational {
    Rational::from_int(4)
}
fn default_attack() -> Rational {
    Rational::from_int(10)
}
fn default_release() -> Rational {
    Rational::from_int(250)
}
fn default_range() -> Rational {
    Rational::from_int(12)
}

/// A track's audio bus.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusSpec {
    /// Bus gain in dB (keyframes in timeline time).
    #[serde(default, skip_serializing_if = "is_zero_anim")]
    pub gain_db: Animatable,
    /// Bus balance in [-1, 1] (keyframes in timeline time).
    #[serde(default, skip_serializing_if = "is_zero_anim")]
    pub pan: Animatable,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mute: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duck: Option<DuckSpec>,
}

impl BusSpec {
    pub fn is_default(&self) -> bool {
        *self == BusSpec::default()
    }
}

/// A fade (or crossfade) length and curve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FadeSpec {
    pub duration: RationalTime,
    #[serde(default)]
    pub curve: FadeCurve,
}

/// Audio of one clip. On a video clip it is the linked audio.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipAudio {
    /// Audio in point relative to the clip's start (negative: J-cut).
    #[serde(default, skip_serializing_if = "is_zero_time")]
    pub in_offset: RationalTime,
    /// Audio out point relative to the clip's end (positive: L-cut).
    #[serde(default, skip_serializing_if = "is_zero_time")]
    pub out_offset: RationalTime,
    /// Clip gain, dB (keyframes in clip-local time: 0 = the clip's `start`).
    #[serde(default, skip_serializing_if = "is_zero_anim")]
    pub gain_db: Animatable,
    /// Clip pan in [-1, 1] (keyframes in clip-local time).
    #[serde(default, skip_serializing_if = "is_zero_anim")]
    pub pan: Animatable,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mute: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in: Option<FadeSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out: Option<FadeSpec>,
    /// Crossfade from the previous clip on the same track: this clip fades in
    /// over its first `duration` of audio while the clip whose audio ends
    /// exactly then fades out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crossfade_in: Option<FadeSpec>,
}

impl ClipAudio {
    pub fn is_default(&self) -> bool {
        *self == ClipAudio::default()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioTrack {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "BusSpec::is_default")]
    pub bus: BusSpec,
    pub clips: Vec<AudioClip>,
}

/// An audio-only clip.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioClip {
    pub id: String,
    /// Media path, relative to the timeline file.
    pub source: PathBuf,
    pub start: RationalTime,
    #[serde(default)]
    pub source_in: RationalTime,
    pub duration: RationalTime,
    #[serde(default, skip_serializing_if = "ClipAudio::is_default")]
    pub audio: ClipAudio,
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
    /// The audio bus carrying this track's clips' linked audio.
    #[serde(default, skip_serializing_if = "BusSpec::is_default")]
    pub audio: BusSpec,
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
    /// Linked audio (used if the source has an audio stream).
    #[serde(default, skip_serializing_if = "ClipAudio::is_default")]
    pub audio: ClipAudio,
}

fn one() -> Rational {
    Rational::ONE
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transition {
    Dissolve { duration: RationalTime },
}

/// Audio region `[start + in_offset, end + out_offset)` of a clip.
pub fn audio_region(
    start: RationalTime,
    duration: RationalTime,
    a: &ClipAudio,
) -> (RationalTime, RationalTime) {
    (start + a.in_offset, start + duration + a.out_offset)
}

impl AudioClip {
    pub fn end(&self) -> RationalTime {
        self.start + self.duration
    }
    pub fn audio_region(&self) -> (RationalTime, RationalTime) {
        audio_region(self.start, self.duration, &self.audio)
    }
}

impl Clip {
    pub fn end(&self) -> RationalTime {
        self.start + self.duration
    }
    pub fn audio_region(&self) -> (RationalTime, RationalTime) {
        audio_region(self.start, self.duration, &self.audio)
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
        for t in &mut tl.audio_tracks {
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
        self.validate_audio()
    }

    fn validate_audio(&self) -> anyhow::Result<()> {
        let a = &self.audio;
        ensure!(
            (8_000..=192_000).contains(&a.sample_rate),
            "audio.sample_rate must be in 8000..=192000"
        );
        a.master_gain_db
            .validate()
            .map_err(|e| anyhow::anyhow!("audio.master_gain_db: {e}"))?;
        if let Some(l) = &a.loudness {
            ensure!(
                l.target_lufs >= Rational::from_int(-70) && l.target_lufs < Rational::ZERO,
                "audio.loudness.target_lufs must be in [-70, 0)"
            );
            ensure!(
                l.true_peak_dbtp >= Rational::from_int(-20) && l.true_peak_dbtp <= Rational::ZERO,
                "audio.loudness.true_peak_dbtp must be in [-20, 0]"
            );
        }
        // Unique clip ids and track names (edit ops and duck keys refer to them).
        let mut ids = std::collections::HashSet::new();
        for id in self.clip_ids() {
            ensure!(ids.insert(id), "duplicate clip id {id:?}");
        }
        let names: Vec<&str> = self.track_names().collect();
        let mut seen = std::collections::HashSet::new();
        for n in &names {
            ensure!(
                n.is_empty() || seen.insert(*n),
                "duplicate track name {n:?}"
            );
        }
        let buses = self
            .tracks
            .iter()
            .map(|t| (&t.name, &t.audio))
            .chain(self.audio_tracks.iter().map(|t| (&t.name, &t.bus)));
        for (name, bus) in buses.clone() {
            check_anim(&bus.gain_db, &format!("track {name:?} audio gain_db"), None)?;
            check_anim(
                &bus.pan,
                &format!("track {name:?} audio pan"),
                Some((-1, 1)),
            )?;
            if let Some(d) = &bus.duck {
                ensure!(
                    !d.key.is_empty(),
                    "track {name:?}: duck needs at least one key track"
                );
                for k in &d.key {
                    let Some((_, kb)) = buses.clone().find(|(n, _)| *n == k) else {
                        bail!("track {name:?}: duck key {k:?} is not a track name");
                    };
                    ensure!(
                        k != name && kb.duck.is_none(),
                        "track {name:?}: duck key {k:?} must be another track that is not itself ducked"
                    );
                }
                ensure!(
                    d.ratio >= Rational::ONE,
                    "track {name:?}: duck ratio must be >= 1"
                );
                ensure!(
                    d.attack_ms > Rational::ZERO && d.release_ms > Rational::ZERO,
                    "track {name:?}: duck attack/release must be > 0"
                );
                ensure!(
                    d.range_db >= Rational::ZERO,
                    "track {name:?}: duck range_db must be >= 0"
                );
            }
        }
        for t in &self.tracks {
            let clips: Vec<_> = t
                .clips
                .iter()
                .map(|c| (&c.id, c.source_in, c.duration, c.audio_region(), &c.audio))
                .collect();
            validate_track_audio(&t.name, &clips)?;
        }
        for t in &self.audio_tracks {
            for c in &t.clips {
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
            }
            let clips: Vec<_> = t
                .clips
                .iter()
                .map(|c| (&c.id, c.source_in, c.duration, c.audio_region(), &c.audio))
                .collect();
            validate_track_audio(&t.name, &clips)?;
        }
        Ok(())
    }

    pub fn clip_ids(&self) -> impl Iterator<Item = &str> {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(|c| c.id.as_str()))
            .chain(
                self.audio_tracks
                    .iter()
                    .flat_map(|t| t.clips.iter().map(|c| c.id.as_str())),
            )
    }

    pub fn track_names(&self) -> impl Iterator<Item = &str> + Clone {
        self.tracks
            .iter()
            .map(|t| t.name.as_str())
            .chain(self.audio_tracks.iter().map(|t| t.name.as_str()))
    }

    pub fn duration(&self) -> RationalTime {
        self.output.duration.unwrap_or_else(|| {
            self.tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(Clip::end))
                .chain(
                    self.audio_tracks
                        .iter()
                        .flat_map(|t| t.clips.iter().map(AudioClip::end)),
                )
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

fn check_anim(a: &Animatable, what: &str, range: Option<(i64, i64)>) -> anyhow::Result<()> {
    a.validate().map_err(|e| anyhow::anyhow!("{what}: {e}"))?;
    if let Some((lo, hi)) = range {
        let (min, max) = a.key_range();
        ensure!(
            min >= Rational::from_int(lo) && max <= Rational::from_int(hi),
            "{what}: values must be in [{lo}, {hi}]"
        );
    }
    Ok(())
}

type AudioClipView<'a> = (
    &'a String,
    RationalTime,
    RationalTime,
    (RationalTime, RationalTime),
    &'a ClipAudio,
);

fn validate_track_audio(track: &str, clips: &[AudioClipView<'_>]) -> anyhow::Result<()> {
    for &(id, source_in, _, (a0, a1), au) in clips {
        check_anim(&au.gain_db, &format!("clip {id}: audio gain_db"), None)?;
        check_anim(&au.pan, &format!("clip {id}: audio pan"), Some((-1, 1)))?;
        ensure!(
            a1 > a0,
            "clip {id}: audio region is empty (in_offset {} / out_offset {})",
            au.in_offset,
            au.out_offset
        );
        ensure!(
            source_in + au.in_offset >= RationalTime::ZERO,
            "clip {id}: audio in_offset {} reaches before the start of the source (source_in {}): J-cuts need source handles",
            au.in_offset,
            source_in
        );
        let len = a1 - a0;
        let mut used = RationalTime::ZERO;
        for (what, f) in [("fade_in", &au.fade_in), ("fade_out", &au.fade_out)] {
            if let Some(f) = f {
                ensure!(
                    f.duration > RationalTime::ZERO,
                    "clip {id}: audio {what} duration must be positive"
                );
                used = used + f.duration;
            }
        }
        ensure!(
            used <= len,
            "clip {id}: audio fades are longer than its audio ({len})"
        );
        if let Some(x) = &au.crossfade_in {
            ensure!(
                x.duration > RationalTime::ZERO && x.duration <= len,
                "clip {id}: audio crossfade_in must be positive and no longer than the clip's audio"
            );
            let want = a0 + x.duration;
            if !clips
                .iter()
                .any(|&(oid, _, _, (_, e), _)| oid != id && e == want)
            {
                bail!(
                    "clip {id}: audio crossfade_in of {} needs another clip on track {track:?} whose audio ends at {want} (overlapping this clip's audio by exactly the crossfade)",
                    x.duration
                );
            }
        }
    }
    Ok(())
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
