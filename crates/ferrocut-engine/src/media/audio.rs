//! Source audio decode + resample (swresample) to the project rate, f32 planar.
//!
//! The stream is decoded front to back, single-threaded, and resampled by
//! one `SwrContext` with fixed default options, so the result is
//! deterministic. [`AudioStream`] hands the samples to a sink block by block
//! (bounded memory: the mixdown writes them straight to its disk cache);
//! [`decode_audio`] collects them in memory (tests, reference path). Mono stays mono (panned later with a constant-power
//! law); everything else is downmixed by swresample to stereo.
//!
//! Alignment: source time 0 is the video stream's first timestamp when the
//! file has video (so linked A/V stays in sync with the video decoder, which
//! uses the same origin), else the audio stream's start time. Audio that
//! starts later is padded with silence; earlier audio is dropped.

use std::path::Path;

use anyhow::{Context as _, anyhow, ensure};
use ferrocut_audio::SourceAudio;
use ferrocut_core::{Rational, RationalTime};
use ffmpeg_next::software::resampling;
use ffmpeg_next::util::format::sample::{Sample, Type as SampleType};
use ffmpeg_next::{ChannelLayout, codec, format, frame, media};

use super::{init, to_core};

/// Part of the audio cache keys: bump when decoding/resampling changes.
pub const AUDIO_DECODE_VERSION: &str = "audio-decode.v1:swr-default:f32p:mono-or-stereo";

#[derive(Clone, Debug)]
pub struct DecodedAudio {
    pub audio: SourceAudio,
    pub source_rate: u32,
    pub source_channels: u16,
    pub codec: String,
}

fn nopts(v: i64) -> Option<i64> {
    (v != ffmpeg_next::ffi::AV_NOPTS_VALUE).then_some(v)
}

/// Decode the best audio stream of `path` resampled to `rate` Hz into
/// memory, or `None` if the file has no audio stream.
pub fn decode_audio(path: &Path, rate: u32) -> anyhow::Result<Option<DecodedAudio>> {
    let Some(s) = AudioStream::open(path, rate)? else {
        return Ok(None);
    };
    let mut planes = vec![Vec::new(); s.channels()];
    let (source_rate, source_channels, codec) = (s.source_rate, s.source_channels, s.codec.clone());
    s.run(&mut |block| {
        for (p, b) in planes.iter_mut().zip(block) {
            p.extend_from_slice(b);
        }
        Ok(())
    })?;
    Ok(Some(DecodedAudio {
        audio: SourceAudio { planes },
        source_rate,
        source_channels,
        codec,
    }))
}

/// An opened audio stream, decoded by [`AudioStream::run`].
pub struct AudioStream {
    ictx: format::context::Input,
    idx: usize,
    tb: Rational,
    origin: RationalTime,
    dec: codec::decoder::Audio,
    rate: u32,
    out_layout: ChannelLayout,
    path: std::path::PathBuf,
    pub source_rate: u32,
    pub source_channels: u16,
    pub codec: String,
}

impl AudioStream {
    /// `None` if `path` has no audio stream.
    pub fn open(path: &Path, rate: u32) -> anyhow::Result<Option<AudioStream>> {
        init();
        let ictx = format::input(path).with_context(|| format!("opening {}", path.display()))?;
        let Some(ist) = ictx.streams().best(media::Type::Audio) else {
            return Ok(None);
        };
        let idx = ist.index();
        let tb = to_core(ist.time_base());
        // Origin of source time: the video stream's start (see module docs).
        let origin = match ictx.streams().best(media::Type::Video) {
            Some(v) => {
                RationalTime::from_pts(nopts(v.start_time()).unwrap_or(0), to_core(v.time_base()))
            }
            None => RationalTime::from_pts(nopts(ist.start_time()).unwrap_or(0), tb),
        };
        let mut cctx = codec::context::Context::from_parameters(ist.parameters())?;
        cctx.set_threading(codec::threading::Config {
            kind: codec::threading::Type::None,
            count: 1,
        });
        let dec = cctx.decoder().audio().context("opening audio decoder")?;
        let codec_name = dec
            .codec()
            .map(|c| c.name().to_string())
            .unwrap_or_default();
        let source_rate = dec.rate();
        let source_channels = dec.channels();
        ensure!(
            source_rate > 0 && source_channels > 0,
            "{}: bad audio stream",
            path.display()
        );
        let out_layout = if source_channels == 1 {
            ChannelLayout::MONO
        } else {
            ChannelLayout::STEREO
        };
        Ok(Some(AudioStream {
            ictx,
            idx,
            tb,
            origin,
            dec,
            rate,
            out_layout,
            path: path.to_path_buf(),
            source_rate,
            source_channels,
            codec: codec_name,
        }))
    }

    /// Output channels (1 or 2).
    pub fn channels(&self) -> usize {
        self.out_layout.channels() as usize
    }

    /// Decode to the end, handing `sink` consecutive planar blocks aligned
    /// to the source-time origin (leading silence inserted, or samples
    /// before the origin dropped): the concatenation equals what the
    /// whole-stream decode produced.
    pub fn run(
        mut self,
        sink: &mut dyn FnMut(&[Vec<f32>]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let out_ch = self.channels();
        let mut st = State {
            swr: None,
            out_layout: self.out_layout,
            rate: self.rate,
            planes: vec![Vec::new(); out_ch],
            first: None,
            out: frame::Audio::empty(),
            align: Align {
                tb: self.tb,
                origin: self.origin,
                rate: self.rate,
                lead: None,
            },
        };
        let mut decoded = frame::Audio::empty();
        let path = self.path.clone();
        for (s, pkt) in self.ictx.packets() {
            if s.index() != self.idx {
                continue;
            }
            self.dec
                .send_packet(&pkt)
                .with_context(|| format!("{}: decoding audio", path.display()))?;
            while self.dec.receive_frame(&mut decoded).is_ok() {
                st.push(&mut decoded)?;
                st.emit(sink)?;
            }
        }
        self.dec.send_eof()?;
        while self.dec.receive_frame(&mut decoded).is_ok() {
            st.push(&mut decoded)?;
            st.emit(sink)?;
        }
        st.flush()?;
        st.emit(sink)
    }
}

/// Places the first decoded sample relative to the origin.
struct Align {
    tb: Rational,
    origin: RationalTime,
    rate: u32,
    /// Samples still to insert (> 0) or drop (< 0); `None` before the first frame.
    lead: Option<i64>,
}

struct State {
    swr: Option<resampling::Context>,
    out_layout: ChannelLayout,
    rate: u32,
    /// Resampled samples not yet handed to the sink.
    planes: Vec<Vec<f32>>,
    first: Option<i64>,
    out: frame::Audio,
    align: Align,
}

const F32P: Sample = Sample::F32(SampleType::Planar);

impl State {
    fn push(&mut self, f: &mut frame::Audio) -> anyhow::Result<()> {
        if self.first.is_none() {
            let first = f.pts().unwrap_or(0);
            self.first = Some(first);
            let a = &mut self.align;
            let lead = (RationalTime::from_pts(first, a.tb).0 - a.origin.0)
                * Rational::from_int(a.rate as i64);
            a.lead = Some(lead.round());
        }
        let layout = {
            let l = f.channel_layout();
            if l.is_empty() || l.channels() != f.channels() as i32 {
                ChannelLayout::default(f.channels() as i32)
            } else {
                l
            }
        };
        // swr_convert_frame insists the frame's layout equals the configured one.
        if f.channel_layout() != layout {
            f.set_channel_layout(layout);
        }
        if self.swr.is_none() {
            self.swr = Some(
                resampling::Context::get(
                    f.format(),
                    layout,
                    f.rate(),
                    F32P,
                    self.out_layout,
                    self.rate,
                )
                .context("creating swresample context")?,
            );
        }
        let swr = self.swr.as_mut().expect("swr");
        ensure!(
            swr.input().format == f.format() && swr.input().rate == f.rate(),
            "audio format changes mid-stream are not supported"
        );
        let cap = f.samples() * self.rate as usize / f.rate().max(1) as usize + 256;
        self.ensure_out(cap);
        let swr = self.swr.as_mut().expect("swr");
        swr.run(f, &mut self.out).context("resampling")?;
        Self::take(&mut self.planes, &self.out);
        Ok(())
    }

    /// Hand buffered samples to the sink (after the origin alignment).
    fn emit(
        &mut self,
        sink: &mut dyn FnMut(&[Vec<f32>]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        match self.align.lead {
            None => return Ok(()),
            Some(lead) if lead > 0 => {
                // Leading silence, in bounded blocks.
                let mut left = lead as usize;
                while left > 0 {
                    let n = left.min(1 << 16);
                    sink(&vec![vec![0.0f32; n]; self.planes.len()])?;
                    left -= n;
                }
                self.align.lead = Some(0);
            }
            Some(lead) if lead < 0 => {
                let have = self.planes.first().map_or(0, Vec::len);
                let d = ((-lead) as usize).min(have);
                for p in &mut self.planes {
                    p.drain(..d);
                }
                self.align.lead = Some(lead + d as i64);
            }
            _ => {}
        }
        if self.planes.first().is_some_and(|p| !p.is_empty()) {
            sink(&self.planes)?;
            for p in &mut self.planes {
                p.clear();
            }
        }
        Ok(())
    }

    fn ensure_out(&mut self, cap: usize) {
        let cap = cap.max(4096);
        // SAFETY: `extended_data` is only null for an unallocated frame.
        let allocated = unsafe { !(*self.out.as_ptr()).extended_data.is_null() };
        if !allocated || self.capacity() < cap {
            self.out = frame::Audio::new(F32P, cap, self.out_layout);
        }
        // swr_convert_frame treats an allocated frame's nb_samples as its capacity.
        let c = self.capacity();
        self.out.set_samples(c);
    }

    fn capacity(&self) -> usize {
        // SAFETY: linesize[0] of an allocated planar f32 frame is its plane capacity in bytes.
        unsafe { (*self.out.as_ptr()).linesize[0].max(0) as usize / 4 }
    }

    fn take(planes: &mut [Vec<f32>], out: &frame::Audio) {
        for (c, p) in planes.iter_mut().enumerate() {
            p.extend_from_slice(&out.plane::<f32>(c)[..out.samples()]);
        }
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        if self.swr.is_none() {
            return Ok(());
        }
        loop {
            self.ensure_out(4096);
            let swr = self.swr.as_mut().expect("swr");
            swr.flush(&mut self.out)
                .map_err(|e| anyhow!("flushing swresample: {e}"))?;
            if self.out.samples() == 0 {
                return Ok(());
            }
            Self::take(&mut self.planes, &self.out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic decoded frames exercise the existing no-PTS fallback without
    // asking a demuxer to invent timestamps. Real missing stream-start metadata
    // is covered by the committed WAV integration control.
    #[test]
    fn missing_first_pts_keeps_zero_fallback_and_source_origin() {
        init();
        let samples = [0.25_f32, -0.5, 0.75, -0.25];
        let align_frame = |pts, origin| {
            let mut frame = frame::Audio::new(F32P, samples.len(), ChannelLayout::MONO);
            frame.set_rate(48_000);
            frame.set_pts(pts);
            frame.plane_mut::<f32>(0).copy_from_slice(&samples);
            let mut state = State {
                swr: None,
                out_layout: ChannelLayout::MONO,
                rate: 48_000,
                planes: vec![Vec::new()],
                first: None,
                out: frame::Audio::empty(),
                align: Align {
                    tb: Rational::new(1, 48_000),
                    origin: RationalTime::new(origin, 48_000),
                    rate: 48_000,
                    lead: None,
                },
            };
            let mut actual = Vec::new();
            let mut sink = |block: &[Vec<f32>]| {
                actual.extend_from_slice(&block[0]);
                Ok(())
            };
            state.push(&mut frame).unwrap();
            state.emit(&mut sink).unwrap();
            state.flush().unwrap();
            state.emit(&mut sink).unwrap();
            actual.into_iter().map(f32::to_bits).collect::<Vec<_>>()
        };
        let bits = |values: &[f32]| values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        assert_eq!(align_frame(None, 0), bits(&samples));
        assert_eq!(align_frame(None, 2), bits(&samples[2..]));
        assert_eq!(
            align_frame(None, -2),
            bits(&[0.0, 0.0, 0.25, -0.5, 0.75, -0.25])
        );
        assert_eq!(align_frame(None, 2), align_frame(Some(0), 2));
        assert_eq!(align_frame(Some(2), 2), bits(&samples));
    }
}
