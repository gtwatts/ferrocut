//! MP4 (ISO BMFF) writer: the joined H.264 chunks as-is (no re-encode) plus
//! AAC-LC from FFmpeg's native `aac` encoder (part of the LGPL libavcodec),
//! `movflags=+faststart`, bit-exact flags for reproducible files.

use std::path::Path;

use ferrocut_types::error::NodeError;
use ffmpeg_next::{
    ChannelLayout, Dictionary, Packet, Rational, codec, encoder, format,
    format::{Sample, sample::Type as SampleLayout},
    frame, media,
};

use crate::media::{ff, open, open_decoder, pump};

/// The H.264 track.
#[derive(Clone, Debug)]
pub struct VideoTrack {
    pub width: u32,
    pub height: u32,
    pub fps: (i32, i32),
    /// avcC record (SPS/PPS shared by every chunk).
    pub avcc: Vec<u8>,
    pub profile: i32,
    pub level: i32,
}

/// One video sample: AVCC payload, keyframe flag. Frame `i` has pts = dts = i.
pub struct VideoSample {
    pub data: Vec<u8>,
    pub key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AudioSummary {
    pub codec: String,
    pub encoder: String,
    pub bit_rate: usize,
    pub sample_rate: u32,
    pub channels: u16,
    pub packets: usize,
}

struct EncodedAudio {
    enc: encoder::Audio,
    rate: u32,
    packets: Vec<Packet>,
    summary: AudioSummary,
}

/// (bytes per sample, planar, decode one sample to f32).
type SampleReader = (usize, bool, fn(&[u8]) -> f32);

fn sample_reader(fmt: Sample) -> Result<SampleReader, NodeError> {
    let planar = matches!(
        fmt,
        Sample::I16(SampleLayout::Planar)
            | Sample::I32(SampleLayout::Planar)
            | Sample::F32(SampleLayout::Planar)
            | Sample::F64(SampleLayout::Planar)
    );
    let r: (usize, fn(&[u8]) -> f32) = match fmt {
        Sample::I16(_) => (2, |b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0),
        Sample::I32(_) => (4, |b| {
            (i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64 / 2147483648.0) as f32
        }),
        Sample::F32(_) => (4, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        Sample::F64(_) => (8, |b| f64::from_le_bytes(b[..8].try_into().unwrap()) as f32),
        other => {
            return Err(NodeError::permanent(format!(
                "unsupported master sample format {other:?}"
            )));
        }
    };
    Ok((r.0, planar, r.1))
}

/// Decode the master's audio and encode it to AAC-LC (whole track; AAC at
/// 192 kb/s is ~1.4 MB a minute, so packets are kept in memory).
fn encode_audio(master: &Path, bit_rate: usize) -> Result<Option<EncodedAudio>, NodeError> {
    let mut ictx = open(master)?;
    if ictx.streams().best(media::Type::Audio).is_none() {
        return Ok(None);
    }
    let (si, dec) = open_decoder(&ictx, media::Type::Audio, 1)?;
    let mut dec = dec.audio().map_err(ff("audio decoder"))?;
    let (rate, channels) = (dec.rate(), dec.channels());
    if channels == 0 || channels > 8 {
        return Err(NodeError::permanent(format!(
            "unsupported audio channel count {channels}"
        )));
    }
    let codec = encoder::find_by_name("aac")
        .ok_or_else(|| NodeError::permanent("FFmpeg has no native `aac` encoder"))?;
    let mut ectx = codec::context::Context::new_with_codec(codec)
        .encoder()
        .audio()
        .map_err(ff("aac"))?;
    ectx.set_rate(rate as i32);
    ectx.set_channel_layout(ChannelLayout::default(channels as i32));
    ectx.set_format(Sample::F32(SampleLayout::Planar));
    ectx.set_bit_rate(bit_rate);
    ectx.set_time_base(Rational::new(1, rate as i32));
    ectx.set_flags(codec::Flags::BITEXACT | codec::Flags::GLOBAL_HEADER);
    let mut opts = Dictionary::new();
    opts.set("aac_coder", "twoloop");
    let mut enc = ectx.open_with(opts).map_err(ff("opening aac encoder"))?;
    let frame_size = enc.frame_size().max(1) as usize;
    let ch = channels as usize;

    let mut fifo: Vec<Vec<f32>> = vec![Vec::new(); ch];
    let mut next_pts: i64 = 0;
    let mut packets = Vec::new();
    let send = |enc: &mut encoder::Audio,
                fifo: &mut Vec<Vec<f32>>,
                n: usize,
                packets: &mut Vec<Packet>,
                next_pts: &mut i64|
     -> Result<(), NodeError> {
        let mut f = frame::Audio::new(
            Sample::F32(SampleLayout::Planar),
            n,
            ChannelLayout::default(channels as i32),
        );
        f.set_rate(rate);
        for (c, q) in fifo.iter_mut().enumerate() {
            f.plane_mut::<f32>(c)[..n].copy_from_slice(&q[..n]);
            q.drain(..n);
        }
        f.set_pts(Some(*next_pts));
        *next_pts += n as i64;
        enc.send_frame(&f).map_err(ff("aac encode"))?;
        let mut p = Packet::empty();
        while enc.receive_packet(&mut p).is_ok() {
            packets.push(std::mem::replace(&mut p, Packet::empty()));
        }
        Ok(())
    };
    let mut af = frame::Audio::empty();
    pump(&mut ictx, si, &mut dec, &mut af, |a| {
        let (bytes, planar, read) = sample_reader(a.format())?;
        if a.channels() as usize != ch || a.rate() != rate {
            return Err(NodeError::permanent(
                "master audio format changes mid-stream",
            ));
        }
        for i in 0..a.samples() {
            for (c, q) in fifo.iter_mut().enumerate() {
                let v = if planar {
                    read(&a.data(c)[i * bytes..])
                } else {
                    read(&a.data(0)[(i * ch + c) * bytes..])
                };
                q.push(v);
            }
        }
        while fifo[0].len() >= frame_size {
            send(&mut enc, &mut fifo, frame_size, &mut packets, &mut next_pts)?;
        }
        Ok(())
    })?;
    if !fifo[0].is_empty() {
        let n = fifo[0].len();
        send(&mut enc, &mut fifo, n, &mut packets, &mut next_pts)?;
    }
    enc.send_eof().map_err(ff("aac flush"))?;
    let mut p = Packet::empty();
    while enc.receive_packet(&mut p).is_ok() {
        packets.push(std::mem::replace(&mut p, Packet::empty()));
    }
    let summary = AudioSummary {
        codec: "aac (AAC-LC)".into(),
        encoder: "FFmpeg native aac (LGPL libavcodec)".into(),
        bit_rate,
        sample_rate: rate,
        channels,
        packets: packets.len(),
    };
    Ok(Some(EncodedAudio {
        enc,
        rate,
        packets,
        summary,
    }))
}

fn set_bitexact(octx: &mut format::context::Output) {
    // SAFETY: setting a flag on our own, not yet written output context.
    // (The cast is needed with bindgen versions that emit the flag as u32.)
    #[allow(clippy::unnecessary_cast)]
    unsafe {
        (*octx.as_mut_ptr()).flags |= ffmpeg_next::ffi::AVFMT_FLAG_BITEXACT as i32;
    }
}

/// Write `out` (MP4, faststart): `video` samples in order, audio from `master`.
pub fn write_mp4(
    out: &Path,
    track: &VideoTrack,
    mut video: impl Iterator<Item = Result<VideoSample, NodeError>>,
    master_audio: Option<(&Path, usize)>,
) -> Result<Option<AudioSummary>, NodeError> {
    use ffmpeg_next::ffi;
    let audio = match master_audio {
        Some((m, br)) => encode_audio(m, br)?,
        None => None,
    };
    let mut octx =
        format::output_as(out, "mp4").map_err(ff(format!("creating {}", out.display())))?;
    set_bitexact(&mut octx);
    let vtb = Rational::new(track.fps.1, track.fps.0);
    {
        let mut ost = octx
            .add_stream(encoder::find(codec::Id::H264))
            .map_err(ff("mp4 video stream"))?;
        ost.set_time_base(vtb);
        ost.set_avg_frame_rate(Rational::new(track.fps.0, track.fps.1));
        ost.set_rate(Rational::new(track.fps.0, track.fps.1));
        // SAFETY: filling our own, freshly added stream's codec parameters
        // before write_header; extradata is av_malloc'd with the required
        // padding and owned by the parameters from here on.
        unsafe {
            let st = ost.as_mut_ptr();
            (*st).sample_aspect_ratio = ffi::AVRational { num: 1, den: 1 };
            let par = (*st).codecpar;
            (*par).codec_type = ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
            (*par).codec_id = ffi::AVCodecID::AV_CODEC_ID_H264;
            (*par).codec_tag = 0;
            (*par).width = track.width as i32;
            (*par).height = track.height as i32;
            (*par).format = ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
            (*par).profile = track.profile;
            (*par).level = track.level;
            (*par).sample_aspect_ratio = ffi::AVRational { num: 1, den: 1 };
            (*par).field_order = ffi::AVFieldOrder::AV_FIELD_PROGRESSIVE;
            (*par).color_range = ffi::AVColorRange::AVCOL_RANGE_MPEG;
            (*par).color_primaries = ffi::AVColorPrimaries::AVCOL_PRI_BT709;
            (*par).color_trc = ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
            (*par).color_space = ffi::AVColorSpace::AVCOL_SPC_BT709;
            (*par).chroma_location = ffi::AVChromaLocation::AVCHROMA_LOC_LEFT;
            (*par).video_delay = 0;
            let pad = ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
            let buf = ffi::av_mallocz(track.avcc.len() + pad) as *mut u8;
            if buf.is_null() {
                return Err(NodeError::retryable("out of memory (extradata)"));
            }
            std::ptr::copy_nonoverlapping(track.avcc.as_ptr(), buf, track.avcc.len());
            (*par).extradata = buf;
            (*par).extradata_size = track.avcc.len() as i32;
        }
    }
    let audio_idx = match &audio {
        Some(a) => {
            let mut ost = octx
                .add_stream(encoder::find(codec::Id::AAC))
                .map_err(ff("mp4 audio stream"))?;
            ost.set_parameters(&a.enc);
            ost.set_time_base(Rational::new(1, a.rate as i32));
            // SAFETY: clearing the codec tag on our own stream parameters before write_header.
            unsafe {
                (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            }
            Some(ost.index())
        }
        None => None,
    };
    let mut opts = Dictionary::new();
    opts.set("movflags", "+faststart");
    octx.write_header_with(opts)
        .map_err(ff("writing mp4 header"))?;
    let ost_vtb = octx.stream(0).expect("video stream").time_base();
    let ost_atb = audio_idx.map(|i| octx.stream(i).expect("audio stream").time_base());

    // Interleave by presentation time: video frame i at i*den/num s, audio
    // packets at their pts/rate s (compared exactly with integer math).
    let mut apkts = audio.as_ref().map(|a| a.packets.iter().peekable());
    let rate = audio.as_ref().map_or(1, |a| a.rate as i64);
    let (fnum, fden) = (track.fps.0 as i64, track.fps.1 as i64);
    let mut i: i64 = 0;
    let write_audio_until =
        |octx: &mut format::context::Output,
         apkts: &mut Option<std::iter::Peekable<std::slice::Iter<'_, Packet>>>,
         limit: Option<i64>|
         -> Result<(), NodeError> {
            let (Some(it), Some(ai), Some(atb)) = (apkts.as_mut(), audio_idx, ost_atb) else {
                return Ok(());
            };
            while let Some(p) = it.peek() {
                let pts = p.pts().unwrap_or(0);
                // audio pts/rate < frame/fps  <=>  pts*fnum < frame*fden*rate
                if let Some(frame) = limit
                    && (pts as i128) * (fnum as i128)
                        >= (frame as i128) * (fden as i128) * (rate as i128)
                {
                    break;
                }
                let mut q = (*p).clone();
                q.set_stream(ai);
                q.rescale_ts(Rational::new(1, rate as i32), atb);
                q.set_position(-1);
                q.write_interleaved(octx).map_err(ff("writing audio"))?;
                it.next();
            }
            Ok(())
        };
    for s in video.by_ref() {
        let s = s?;
        write_audio_until(&mut octx, &mut apkts, Some(i))?;
        let mut p = Packet::copy(&s.data);
        p.set_stream(0);
        p.set_pts(Some(i));
        p.set_dts(Some(i));
        p.set_duration(1);
        if s.key {
            p.set_flags(ffmpeg_next::packet::Flags::KEY);
        }
        p.rescale_ts(vtb, ost_vtb);
        p.set_position(-1);
        p.write_interleaved(&mut octx)
            .map_err(ff("writing video"))?;
        i += 1;
    }
    write_audio_until(&mut octx, &mut apkts, None)?;
    octx.write_trailer().map_err(ff("finishing mp4"))?;
    Ok(audio.map(|a| a.summary))
}
