//! Plain-text SRT/WebVTT interchange and editable native caption layers.
//!
//! Timing is exact milliseconds on interchange, rational seconds in projects.
//! Rich subtitle styling, regions and embedded stream captions are deliberately
//! rejected with diagnostics rather than silently discarded. Overlapping cues
//! round-trip, but a single video track requires non-overlapping imported cues.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context as _, bail, ensure};
use ferrocut_core::{Rational, RationalTime};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::edit::{EditOp, TrackKind};
use crate::generator::GeneratorSpec;
use crate::timeline::Timeline;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum CaptionFormat {
    Srt,
    Vtt,
}

impl CaptionFormat {
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        match path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("srt") => Ok(Self::Srt),
            Some("vtt") => Ok(Self::Vtt),
            _ => bail!("caption format must be .srt or .vtt"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionCue {
    pub id: String,
    pub start: RationalTime,
    pub end: RationalTime,
    pub text: String,
}

pub fn validate(cues: &[CaptionCue]) -> anyhow::Result<()> {
    ensure!(cues.len() <= 100_000, "caption cue limit is 100000");
    let mut ids = HashSet::new();
    let mut previous = RationalTime::ZERO;
    for (i, c) in cues.iter().enumerate() {
        ensure!(
            !c.id.is_empty() && !c.id.contains(['\n', '\r']) && !c.id.contains("-->"),
            "cue {}: invalid id",
            i + 1
        );
        ensure!(ids.insert(&c.id), "cue {}: duplicate id {:?}", i + 1, c.id);
        ensure!(
            c.start >= RationalTime::ZERO && c.end > c.start,
            "cue {}: end must follow nonnegative start",
            c.id
        );
        ensure!(
            i == 0 || c.start >= previous,
            "cue {}: starts before previous cue",
            c.id
        );
        ensure!(
            !c.text.trim().is_empty() && !c.text.contains('\0') && !c.text.contains('\r'),
            "cue {}: empty/invalid text",
            c.id
        );
        ensure!(
            !c.text.lines().any(|s| s.trim().is_empty()),
            "cue {}: blank text line would split a subtitle block",
            c.id
        );
        previous = c.start;
    }
    Ok(())
}

fn timestamp(s: &str, format: CaptionFormat) -> anyhow::Result<RationalTime> {
    let separator = match format {
        CaptionFormat::Srt => ',',
        CaptionFormat::Vtt => '.',
    };
    let (clock, millis) = s
        .split_once(separator)
        .context("timestamp requires three millisecond digits")?;
    ensure!(
        millis.len() == 3 && millis.bytes().all(|b| b.is_ascii_digit()),
        "timestamp requires three millisecond digits"
    );
    let fields: Vec<_> = clock.split(':').collect();
    ensure!(
        fields.len() == 3 || (format == CaptionFormat::Vtt && fields.len() == 2),
        "expected HH:MM:SS.mmm (VTT also MM:SS.mmm)"
    );
    ensure!(
        fields
            .iter()
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())),
        "invalid timestamp digits"
    );
    let values: Vec<i128> = fields
        .iter()
        .map(|s| s.parse::<i128>())
        .collect::<Result<_, _>>()?;
    let (hours, minutes, seconds) = if values.len() == 3 {
        (values[0], values[1], values[2])
    } else {
        (0, values[0], values[1])
    };
    ensure!(
        minutes < 60 && seconds < 60,
        "minutes and seconds must be below 60"
    );
    ensure!(
        fields[fields.len() - 1].len() == 2 && fields[fields.len() - 2].len() == 2,
        "minutes and seconds require two digits"
    );
    let ms = hours
        .checked_mul(3_600_000)
        .and_then(|n| {
            n.checked_add(minutes * 60_000 + seconds * 1000 + millis.parse::<i128>().ok()?)
        })
        .context("timestamp overflow")?;
    Ok(RationalTime(Rational::try_new(ms, 1000)?))
}

fn decode_text(s: &str, format: CaptionFormat) -> anyhow::Result<String> {
    ensure!(
        !s.contains('<') && !s.contains('>') && !s.contains("-->"),
        "rich caption markup is unsupported; import plain text or convert styling explicitly"
    );
    if format == CaptionFormat::Srt {
        return Ok(s.to_owned());
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').context("invalid WebVTT text entity")?;
        out.push_str(match &rest[..=end] {
            "&amp;" => "&",
            "&lt;" => "<",
            "&gt;" => ">",
            "&nbsp;" => "\u{a0}",
            "&lrm;" => "\u{200e}",
            "&rlm;" => "\u{200f}",
            _ => bail!("unsupported WebVTT text entity {:?}", &rest[..=end]),
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

pub fn parse(input: &str, format: CaptionFormat) -> anyhow::Result<Vec<CaptionCue>> {
    ensure!(
        input.len() <= 16 * 1024 * 1024,
        "caption file exceeds 16 MiB"
    );
    let input = input
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut blocks = Vec::<Vec<&str>>::new();
    for line in input.lines() {
        if line.trim().is_empty() {
            if blocks.last().is_some_and(|b| !b.is_empty()) {
                blocks.push(Vec::new());
            }
        } else {
            if blocks.is_empty() {
                blocks.push(Vec::new());
            }
            blocks.last_mut().unwrap().push(line);
        }
    }
    blocks.retain(|b| !b.is_empty());
    let skip = if format == CaptionFormat::Vtt {
        let header = blocks.first().context("missing WEBVTT header")?;
        ensure!(
            header.len() == 1
                && (header[0] == "WEBVTT"
                    || header[0].starts_with("WEBVTT ")
                    || header[0].starts_with("WEBVTT\t"))
                && !header[0].contains("-->"),
            "expected WEBVTT header followed by a blank line; metadata is unsupported"
        );
        1
    } else {
        0
    };
    let mut cues = Vec::new();
    for (index, block) in blocks.into_iter().skip(skip).enumerate() {
        if format == CaptionFormat::Vtt
            && (block[0] == "NOTE"
                || block[0].starts_with("NOTE ")
                || block[0].starts_with("NOTE\t"))
        {
            continue;
        }
        ensure!(
            block[0] != "STYLE" && block[0] != "REGION",
            "WebVTT STYLE/REGION blocks are unsupported; convert their styling explicitly"
        );
        let timing = usize::from(!block[0].contains("-->"));
        ensure!(
            format == CaptionFormat::Vtt
                || (timing == 1 && block[0].bytes().all(|b| b.is_ascii_digit())),
            "SRT cue {} needs a numeric sequence id",
            index + 1
        );
        ensure!(
            block.len() > timing + 1,
            "cue {}: missing timing/text",
            index + 1
        );
        let (start, end) = block[timing]
            .split_once("-->")
            .context("missing cue arrow")?;
        ensure!(
            !end.trim().contains(char::is_whitespace),
            "cue {}: WebVTT positioning/settings and SRT coordinates are unsupported",
            index + 1
        );
        let start =
            timestamp(start.trim(), format).with_context(|| format!("cue {} start", index + 1))?;
        let end =
            timestamp(end.trim(), format).with_context(|| format!("cue {} end", index + 1))?;
        let id = if timing == 1 {
            block[0].to_owned()
        } else {
            format!("cue-{}", index + 1)
        };
        let text = decode_text(&block[timing + 1..].join("\n"), format)
            .with_context(|| format!("cue {id}"))?;
        cues.push(CaptionCue {
            id,
            start,
            end,
            text,
        });
    }
    validate(&cues)?;
    Ok(cues)
}

fn formatted_time(t: RationalTime, format: CaptionFormat) -> anyhow::Result<String> {
    let ms = t.0.checked_mul(Rational::from_int(1000))?.round();
    ensure!(ms >= 0, "caption timestamp must be nonnegative");
    let sep = if format == CaptionFormat::Srt {
        ','
    } else {
        '.'
    };
    Ok(format!(
        "{:02}:{:02}:{:02}{sep}{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    ))
}

/// Export uses nearest milliseconds; quantization cannot create zero-length cues.
pub fn write(cues: &[CaptionCue], format: CaptionFormat) -> anyhow::Result<String> {
    validate(cues)?;
    let mut out = if format == CaptionFormat::Vtt {
        "WEBVTT\n\n".to_owned()
    } else {
        String::new()
    };
    for (i, c) in cues.iter().enumerate() {
        if format == CaptionFormat::Vtt {
            ensure!(
                c.id != "NOTE"
                    && !c.id.starts_with("NOTE ")
                    && !c.id.starts_with("NOTE\t")
                    && c.id != "STYLE"
                    && c.id != "REGION",
                "cue {:?}: reserved WebVTT block identifier; rename the cue before export",
                c.id
            );
        }
        let start = formatted_time(c.start, format)?;
        let end = formatted_time(c.end, format)?;
        ensure!(
            start != end,
            "cue {}: duration collapses at millisecond precision",
            c.id
        );
        let text = if format == CaptionFormat::Vtt {
            c.text
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        } else {
            ensure!(
                !c.text.contains(['<', '>']) && !c.text.contains("-->"),
                "cue {}: plain SRT cannot preserve markup-like text",
                c.id
            );
            c.text.clone()
        };
        let id = if format == CaptionFormat::Srt {
            (i + 1).to_string()
        } else {
            c.id.clone()
        };
        out.push_str(&format!("{id}\n{start} --> {end}\n{text}\n\n"));
    }
    Ok(out)
}

/// How `captions import` fits cue times to the picture. Interchange times
/// are milliseconds, so two cues that abut in speech often leave a gap of a
/// few ms that happens to contain a frame time: that frame shows no caption
/// (a one-frame blink). The defaults snap and close such gaps; deliberate
/// pauses longer than `close_gaps` stay. [`CaptionTiming::exact`] keeps the
/// interchange times unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptionTiming {
    /// Move each boundary to the first frame time at or after it. A clip
    /// shows on frame n when start <= n/fps < end, so this changes no
    /// displayed frame; it makes boundaries and gaps whole frames.
    pub snap: bool,
    /// Extend a cue to the next cue's start when the gap between them is at
    /// most this long (0 disables). At most 2 s.
    pub close_gaps: RationalTime,
    /// Extend shorter cues toward this duration, into the following gap only:
    /// never over the next cue and never adding an output frame. With `snap`
    /// the last cue may end at the end of the program's last frame, past an
    /// off-grid program end (3.01 s at 30 fps, 91 frames: 91/30). Off by default.
    pub min_duration: Option<RationalTime>,
}

impl Default for CaptionTiming {
    fn default() -> Self {
        CaptionTiming {
            snap: true,
            close_gaps: RationalTime::new(1, 10),
            min_duration: None,
        }
    }
}

impl CaptionTiming {
    /// The interchange times as they are: no snap, gap closing or extension.
    pub fn exact() -> Self {
        CaptionTiming {
            snap: false,
            close_gaps: RationalTime::ZERO,
            min_duration: None,
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.close_gaps >= RationalTime::ZERO && self.close_gaps <= RationalTime::new(2, 1),
            "close_gaps must be 0..=2 seconds"
        );
        ensure!(
            self.min_duration
                .is_none_or(|d| d > RationalTime::ZERO && d <= RationalTime::new(10, 1)),
            "min_duration must be above 0 and at most 10 seconds"
        );
        Ok(())
    }
}

/// One cue whose imported timing differs from its interchange timing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CueRetime {
    pub cue: String,
    pub from: [RationalTime; 2],
    pub to: [RationalTime; 2],
    /// Displayed frames after the change (first, last), inclusive.
    pub frames: [i64; 2],
    /// Any of `snapped`, `one_frame`, `closed_gap`, `extended`.
    pub reasons: Vec<&'static str>,
}

/// A gap left between consecutive cues that shows frames without a caption.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CueGap {
    pub after: String,
    pub before: String,
    pub duration: RationalTime,
    /// Uncaptioned frames (first, last), inclusive.
    pub frames: [i64; 2],
}

/// What [`retime`] did, for the agent to check rather than assume.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TimingReport {
    pub timing: CaptionTiming,
    pub changed: Vec<CueRetime>,
    pub gaps_kept: Vec<CueGap>,
    pub warnings: Vec<String>,
}

/// Displayed frame range (first, last) of a half-open time range; first >
/// last when it shows no frame.
fn displayed(start: RationalTime, end: RationalTime, fps: ferrocut_core::FrameRate) -> [i64; 2] {
    [start.frame_ceil(fps), end.frame_ceil(fps) - 1]
}

/// Fit cue times to the frame grid at `fps` as `timing` asks. `program_end`
/// is the timeline's current end: with the cues imported exactly, the
/// program ends at the later of it and the last cue's end, and nothing here
/// adds a frame past that (a snapped end may pass it within its last frame).
/// The input cues are not modified; the result never overlaps and never
/// drops a cue. A cue that shows no frame at all gets one when `snap` is on
/// (warned) and is an error when the next cue or the program end leaves no
/// room for it.
pub fn retime(
    cues: &[CaptionCue],
    fps: ferrocut_core::FrameRate,
    program_end: RationalTime,
    timing: &CaptionTiming,
) -> anyhow::Result<(Vec<CaptionCue>, TimingReport)> {
    validate(cues)?;
    timing.validate()?;
    for pair in cues.windows(2) {
        ensure!(
            pair[0].end <= pair[1].start,
            "overlapping cues {} and {} need separate tracks",
            pair[0].id,
            pair[1].id
        );
    }
    let frame = RationalTime::from_frames(1, fps);
    // Program end and frame count with the cues as given.
    let end_exact = cues.last().map_or(program_end, |c| program_end.max(c.end));
    let frames_end = end_exact.frame_ceil(fps);
    let snap = |t: RationalTime| {
        if timing.snap {
            RationalTime::from_frames(t.frame_ceil(fps), fps)
        } else {
            t
        }
    };
    let mut out: Vec<CaptionCue> = cues.to_vec();
    let mut reasons: Vec<Vec<&'static str>> = vec![Vec::new(); cues.len()];
    let mut warnings = Vec::new();
    for (c, r) in out.iter_mut().zip(&mut reasons) {
        let (s, e) = (snap(c.start), snap(c.end));
        if (s, e) != (c.start, c.end) {
            r.push("snapped");
        }
        c.start = s;
        c.end = e;
    }
    for i in 0..out.len() {
        let [first, last] = displayed(out[i].start, out[i].end, fps);
        if first <= last {
            continue;
        }
        let next = out.get(i + 1).map(|n| n.start);
        if !timing.snap {
            warnings.push(format!(
                "cue {} [{}, {}) shows no frame at {} fps",
                out[i].id, cues[i].start, cues[i].end, fps
            ));
            continue;
        }
        let end = out[i].start + frame;
        ensure!(
            next.is_none_or(|n| end <= n),
            "cue {} [{}, {}) shows no frame at {} fps and the next cue starts on the same frame; merge or retime it, or import with exact timing",
            out[i].id,
            cues[i].start,
            cues[i].end,
            fps
        );
        ensure!(
            next.is_some() || first < frames_end,
            "cue {} [{}, {}) shows no frame at {} fps and the program ends before frame {first} ({frames_end} frames); retime it, lengthen the program, or import with exact timing",
            out[i].id,
            cues[i].start,
            cues[i].end,
            fps
        );
        out[i].end = end;
        reasons[i].push("one_frame");
        warnings.push(format!(
            "cue {} [{}, {}) showed no frame at {} fps; it now shows frame {}",
            out[i].id, cues[i].start, cues[i].end, fps, first
        ));
    }
    if timing.close_gaps > RationalTime::ZERO {
        for i in 0..out.len().saturating_sub(1) {
            let gap = out[i + 1].start - out[i].end;
            if gap > RationalTime::ZERO && gap <= timing.close_gaps {
                out[i].end = out[i + 1].start;
                reasons[i].push("closed_gap");
            }
        }
    }
    if let Some(min) = timing.min_duration {
        for i in 0..out.len() {
            if out[i].end - out[i].start >= min {
                continue;
            }
            // The last cue may run to the end of the program's last frame.
            let limit = out
                .get(i + 1)
                .map_or(snap(end_exact).max(out[i].end), |n| n.start);
            let end = snap(out[i].start + min).min(limit);
            if end > out[i].end {
                out[i].end = end;
                reasons[i].push("extended");
            }
            if end - out[i].start < min {
                warnings.push(format!(
                    "cue {} lasts {} s, under min_duration {} s: the {} leaves no room",
                    out[i].id,
                    out[i].end - out[i].start,
                    min,
                    if i + 1 < out.len() {
                        "next cue"
                    } else {
                        "program end"
                    }
                ));
            }
        }
    }
    let changed = out
        .iter()
        .zip(cues)
        .zip(reasons)
        .filter(|((o, c), _)| (o.start, o.end) != (c.start, c.end))
        .map(|((o, c), reasons)| CueRetime {
            cue: o.id.clone(),
            from: [c.start, c.end],
            to: [o.start, o.end],
            frames: displayed(o.start, o.end, fps),
            reasons,
        })
        .collect();
    let mut gaps_kept = Vec::new();
    for pair in out.windows(2) {
        let [first, last] = displayed(pair[0].end, pair[1].start, fps);
        if first > last {
            continue;
        }
        if last - first < 2 {
            warnings.push(format!(
                "{} uncaptioned frame(s) {first}..={last} between cues {} and {} read as a blink; raise close_gaps to close it",
                last - first + 1,
                pair[0].id,
                pair[1].id
            ));
        }
        gaps_kept.push(CueGap {
            after: pair[0].id.clone(),
            before: pair[1].id.clone(),
            duration: pair[1].start - pair[0].end,
            frames: [first, last],
        });
    }
    validate(&out)?;
    Ok((
        out,
        TimingReport {
            timing: timing.clone(),
            changed,
            gaps_kept,
            warnings,
        },
    ))
}

/// Produce normal journalable edits; a supplied TextSpec defines the style.
/// Each cue remains an editable native text clip, addressable by its stable id.
pub fn import_ops(
    tl: &Timeline,
    cues: &[CaptionCue],
    track: &str,
    style: &crate::text::TextSpec,
) -> anyhow::Result<Vec<EditOp>> {
    validate(cues)?;
    style.validate().map_err(anyhow::Error::msg)?;
    ensure!(!track.is_empty(), "caption track name must not be empty");
    ensure!(
        !tl.audio_tracks.iter().any(|t| t.name == track),
        "caption track is an audio track"
    );
    for pair in cues.windows(2) {
        ensure!(
            pair[0].end <= pair[1].start,
            "overlapping cues {} and {} need separate tracks",
            pair[0].id,
            pair[1].id
        );
    }
    let mut ops = Vec::new();
    if !tl.tracks.iter().any(|t| t.name == track) {
        ops.push(EditOp::AddTrack {
            kind: TrackKind::Video,
            name: track.into(),
            index: None,
        });
    }
    for c in cues {
        let mut text = style.clone();
        text.content = c.text.clone();
        ops.push(EditOp::AddClip {
            track: track.into(),
            source: Default::default(),
            generator: Some(json!({"type":"text","text":text})),
            id: Some(format!("{track}.{}", c.id)),
            start: Some(c.start),
            source_in: None,
            duration: Some(c.end - c.start),
            adjustment: false,
            fit: None,
        });
    }
    Ok(ops)
}

/// [`import_ops`] after [`retime`] at the timeline's output rate, bounded
/// by its current program end; the report says what changed.
pub fn import_ops_timed(
    tl: &Timeline,
    cues: &[CaptionCue],
    track: &str,
    style: &crate::text::TextSpec,
    timing: &CaptionTiming,
) -> anyhow::Result<(Vec<EditOp>, TimingReport)> {
    let (cues, report) = retime(cues, tl.output.fps, tl.duration(), timing)?;
    Ok((import_ops(tl, &cues, track, style)?, report))
}

/// Export the explicitly chosen track; refuse mixed picture/text tracks.
pub fn from_track(tl: &Timeline, track: &str) -> anyhow::Result<Vec<CaptionCue>> {
    let t = tl
        .tracks
        .iter()
        .find(|t| t.name == track)
        .with_context(|| format!("no video track {track:?}"))?;
    let mut cues = Vec::new();
    for c in &t.clips {
        let Some(GeneratorSpec::Text { text }) = &c.generator else {
            bail!(
                "clip {} is not text; select a dedicated caption track",
                c.id
            );
        };
        cues.push(CaptionCue {
            id: c.id.clone(),
            start: c.start,
            end: c.end(),
            text: text.content.clone(),
        });
    }
    cues.sort_by_key(|c| c.start);
    validate(&cues)?;
    Ok(cues)
}
