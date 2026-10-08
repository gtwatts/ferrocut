//! Lossless concatenation of chunk files: packets are copied, never re-encoded.
//!
//! Audio rides along as uncompressed PCM (`pcm_f32le`, interleaved stereo):
//! each video frame `i` is followed by the audio packet holding samples
//! `[S(i), S(i+1))` of the master, so the stream is sample-exact by
//! construction (no codec priming/padding, no frame-size constraints like
//! FLAC's fixed block numbering) and chunk boundaries never split a packet.

use std::path::Path;

use anyhow::{Context as _, anyhow, ensure};
use ferrocut_core::{FrameRate, RationalTime};
use ffmpeg_next::{codec, encoder, format, media};

use super::encode::{pts_to_frame, set_bitexact};
use super::{init, to_core};

/// Where the muxer gets the audio master: interleaved stereo f32 samples
/// `[s0, s1)`, requested in increasing order.
pub trait AudioFeed {
    fn read(&mut self, s0: i64, s1: i64) -> anyhow::Result<Vec<f32>>;
}

/// An in-memory master starting at sample 0.
pub struct StereoFeed<'a>(pub &'a ferrocut_audio::Stereo);

impl AudioFeed for StereoFeed<'_> {
    fn read(&mut self, s0: i64, s1: i64) -> anyhow::Result<Vec<f32>> {
        ensure!(
            0 <= s0 && s0 <= s1 && s1 as usize <= self.0.len(),
            "audio {s0}..{s1} outside the buffer"
        );
        let mut v = Vec::with_capacity((s1 - s0) as usize * 2);
        for i in s0 as usize..s1 as usize {
            v.push(self.0.l[i]);
            v.push(self.0.r[i]);
        }
        Ok(v)
    }
}

impl AudioFeed for crate::mixdown::FinalReader<'_> {
    fn read(&mut self, s0: i64, s1: i64) -> anyhow::Result<Vec<f32>> {
        crate::mixdown::FinalReader::read(self, s0, s1)
    }
}

/// The audio master to mux next to the video.
pub struct ConcatAudio<'a> {
    pub rate: u32,
    pub feed: &'a mut dyn AudioFeed,
    /// Sample index of output frame `i`'s start (exact, see `audio::frame_sample`).
    pub frame_sample: &'a (dyn Fn(i64) -> i64 + Sync),
    /// Total samples (clamps the last frame's packet).
    pub total: i64,
}

#[derive(Clone, Debug, Default)]
pub struct ConcatStats {
    pub video_packets: u64,
    pub audio_packets: u64,
    /// blake3 over the video packets' payloads in order (the video stream content).
    pub video_blake3: String,
    /// blake3 over the PCM payloads in order (interleaved f32le samples).
    pub audio_blake3: Option<String>,
    /// The same per input chunk (that chunk's frames' samples).
    pub chunk_audio_blake3: Vec<String>,
}

/// Add a `pcm_f32le` stereo stream at `rate` Hz.
fn add_pcm_stream(octx: &mut format::context::Output, rate: u32) -> anyhow::Result<usize> {
    use ffmpeg_next::ffi;
    let mut ost = octx.add_stream(encoder::find(codec::Id::PCM_F32LE))?;
    ost.set_time_base(ffmpeg_next::Rational::new(1, rate as i32));
    // SAFETY: filling our own, freshly added stream's codec parameters before write_header.
    unsafe {
        let par = ost.parameters().as_mut_ptr();
        (*par).codec_type = ffi::AVMediaType::AVMEDIA_TYPE_AUDIO;
        (*par).codec_id = ffi::AVCodecID::AV_CODEC_ID_PCM_F32LE;
        (*par).codec_tag = 0;
        (*par).format = ffi::AVSampleFormat::AV_SAMPLE_FMT_FLT as i32;
        (*par).sample_rate = rate as i32;
        ffi::av_channel_layout_uninit(&mut (*par).ch_layout);
        ffi::av_channel_layout_default(&mut (*par).ch_layout, 2);
        (*par).bits_per_coded_sample = 32;
        (*par).bits_per_raw_sample = 32;
        (*par).block_align = 8;
    }
    Ok(ost.index())
}

/// Concatenate chunks (each starting at frame `start_frames[i]` on the output
/// timeline) into `out`. Verifies all chunks share identical codec extradata.
pub fn concat(
    chunks: &[&Path],
    start_frames: &[i64],
    fps: FrameRate,
    out: &Path,
    mut audio: Option<&mut ConcatAudio<'_>>,
) -> anyhow::Result<ConcatStats> {
    init();
    ensure!(
        !chunks.is_empty() && chunks.len() == start_frames.len(),
        "bad concat input"
    );
    let mut octx = format::output_as(out, "matroska")
        .with_context(|| format!("creating {}", out.display()))?;
    set_bitexact(&mut octx);
    let first = format::input(chunks[0])?;
    let ist = first
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| anyhow!("chunk has no video"))?;
    let ref_extra = extradata(&ist.parameters());
    {
        let mut ost = octx.add_stream(encoder::find(codec::Id::None))?;
        ost.set_parameters(ist.parameters());
        ost.set_time_base(ist.time_base());
        // Declared as the Matroska DefaultDuration (see encode.rs).
        ost.set_avg_frame_rate(super::to_ff(fps));
        ost.set_rate(super::to_ff(fps));
        // SAFETY: clearing the codec tag on our own stream parameters before write_header.
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
        }
    }
    drop(first);
    let audio_idx = audio
        .as_ref()
        .map(|a| add_pcm_stream(&mut octx, a.rate))
        .transpose()?;
    octx.write_header()?;
    let ost_tb = octx.stream(0).expect("stream").time_base();
    let ast_tb = audio_idx.map(|i| octx.stream(i).expect("audio stream").time_base());
    let mut stats = ConcatStats::default();
    let mut vh = blake3::Hasher::new();
    let mut ah = blake3::Hasher::new();
    for (path, &start) in chunks.iter().zip(start_frames) {
        let mut ch = blake3::Hasher::new();
        let mut ictx =
            format::input(path).with_context(|| format!("opening chunk {}", path.display()))?;
        let (idx, ist_tb) = {
            let ist = ictx
                .streams()
                .best(media::Type::Video)
                .ok_or_else(|| anyhow!("chunk has no video"))?;
            ensure!(
                extradata(&ist.parameters()) == ref_extra,
                "{}: codec config differs",
                path.display()
            );
            (ist.index(), ist.time_base())
        };
        for (s, mut pkt) in ictx.packets() {
            if s.index() != idx {
                continue;
            }
            let local = pts_to_frame(
                pkt.pts().ok_or_else(|| anyhow!("packet without pts"))?,
                ist_tb,
                fps,
            );
            let g = start + local;
            let pts = RationalTime::from_frames(g, fps).to_pts(to_core(ost_tb));
            let next = RationalTime::from_frames(g + 1, fps).to_pts(to_core(ost_tb));
            pkt.set_pts(Some(pts));
            pkt.set_dts(Some(pts)); // intra-only: dts == pts
            pkt.set_duration(next - pts);
            pkt.set_position(-1);
            pkt.set_stream(0);
            vh.update(pkt.data().unwrap_or_default());
            pkt.write_interleaved(&mut octx)?;
            stats.video_packets += 1;
            if let (Some(a), Some(ai), Some(atb)) = (audio.as_deref_mut(), audio_idx, ast_tb) {
                // This frame's samples.
                let s0 = (a.frame_sample)(g).min(a.total);
                let s1 = (a.frame_sample)(g + 1).min(a.total);
                if s1 > s0 {
                    let pcm = a.feed.read(s0, s1)?;
                    ensure!(
                        pcm.len() == (s1 - s0) as usize * 2,
                        "audio feed returned {} values for {s0}..{s1}",
                        pcm.len()
                    );
                    let mut data = Vec::with_capacity(pcm.len() * 4);
                    for v in &pcm {
                        data.extend_from_slice(&v.to_le_bytes());
                    }
                    ah.update(&data);
                    ch.update(&data);
                    let mut apkt = ffmpeg_next::Packet::copy(&data);
                    let rtb = ferrocut_core::Rational::new(1, a.rate as i64);
                    let at = |s: i64| RationalTime::from_pts(s, rtb).to_pts(to_core(atb));
                    apkt.set_pts(Some(at(s0)));
                    apkt.set_dts(Some(at(s0)));
                    apkt.set_duration(at(s1) - at(s0));
                    apkt.set_position(-1);
                    apkt.set_stream(ai);
                    apkt.set_flags(ffmpeg_next::packet::Flags::KEY);
                    apkt.write_interleaved(&mut octx)?;
                    stats.audio_packets += 1;
                }
            }
        }
        stats
            .chunk_audio_blake3
            .push(ch.finalize().to_hex().to_string());
    }
    octx.write_trailer()?;
    stats.video_blake3 = vh.finalize().to_hex().to_string();
    stats.audio_blake3 = audio.map(|_| ah.finalize().to_hex().to_string());
    if audio_idx.is_none() {
        stats.chunk_audio_blake3.clear();
    }
    Ok(stats)
}

fn extradata(p: &codec::Parameters) -> Vec<u8> {
    // SAFETY: reading extradata/extradata_size from valid codec parameters.
    unsafe {
        let raw = p.as_ptr();
        if (*raw).extradata.is_null() || (*raw).extradata_size <= 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts((*raw).extradata, (*raw).extradata_size as usize).to_vec()
        }
    }
}
