//! Source audio decode + resample (swresample) to the project rate, f32 planar.
//!
//! The whole stream is decoded once per render, single-threaded, and
//! resampled by one `SwrContext` with fixed default options, so the result
//! is deterministic. Mono stays mono (panned later with a constant-power
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

/// Decode the best audio stream of `path` resampled to `rate` Hz, or `None`
/// if the file has no audio stream.
pub fn decode_audio(path: &Path, rate: u32) -> anyhow::Result<Option<DecodedAudio>> {
    init();
    let mut ictx = format::input(path).with_context(|| format!("opening {}", path.display()))?;
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
    let mut dec = cctx.decoder().audio().context("opening audio decoder")?;
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
    let out_ch = out_layout.channels() as usize;
    let mut st = State {
        swr: None,
        out_layout,
        rate,
        planes: vec![Vec::new(); out_ch],
        first: None,
        out: frame::Audio::empty(),
    };
    let mut decoded = frame::Audio::empty();
    for (s, pkt) in ictx.packets() {
        if s.index() != idx {
            continue;
        }
        dec.send_packet(&pkt)
            .with_context(|| format!("{}: decoding audio", path.display()))?;
        while dec.receive_frame(&mut decoded).is_ok() {
            st.push(&mut decoded)?;
        }
    }
    dec.send_eof()?;
    while dec.receive_frame(&mut decoded).is_ok() {
        st.push(&mut decoded)?;
    }
    st.flush()?;
    let mut planes = st.planes;
    // Place the first decoded sample relative to the origin.
    if let Some(first) = st.first {
        let lead =
            (RationalTime::from_pts(first, tb).0 - origin.0) * Rational::from_int(rate as i64);
        let lead = lead.round();
        for p in &mut planes {
            if lead > 0 {
                p.splice(0..0, std::iter::repeat_n(0.0, lead as usize));
            } else if lead < 0 {
                p.drain(..((-lead) as usize).min(p.len()));
            }
        }
    }
    Ok(Some(DecodedAudio {
        audio: SourceAudio { planes },
        source_rate,
        source_channels,
        codec: codec_name,
    }))
}

struct State {
    swr: Option<resampling::Context>,
    out_layout: ChannelLayout,
    rate: u32,
    planes: Vec<Vec<f32>>,
    first: Option<i64>,
    out: frame::Audio,
}

const F32P: Sample = Sample::F32(SampleType::Planar);

impl State {
    fn push(&mut self, f: &mut frame::Audio) -> anyhow::Result<()> {
        if self.first.is_none() {
            self.first = Some(f.pts().unwrap_or(0));
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
