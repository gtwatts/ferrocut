//! Chunk encoder: FFV1 (lossless, intra) in Matroska, bit-exact muxing.
//!
//! FFV1 is LGPL and native to libavcodec, so no GPL encoder (x264/x265) is ever
//! referenced. Delivery codecs come later; this is the deterministic master.

use std::path::Path;

use anyhow::{Context as _, anyhow, ensure};
use ferrocut_core::{FrameRate, Rational, RationalTime};
use ffmpeg_next::{Dictionary, Packet, codec, encoder, format, frame, util::format::Pixel};

use super::{init, to_core, to_ff};

#[derive(Clone, Debug)]
pub struct EncodeSettings {
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
    pub gop: u32,
}

impl EncodeSettings {
    pub const CODEC: &'static str = "ffv1";
    /// Every parameter that affects encoded bytes; part of the chunk cache key.
    pub fn fingerprint(&self) -> String {
        format!(
            "ffv1 level=3 slices=4 slicecrc=1 threads=1 pix=bgr0 g={} {}x{} fps={} packet-duration=1-frame mux=matroska+bitexact",
            self.gop, self.width, self.height, self.fps
        )
    }
}

pub(crate) fn set_bitexact(octx: &mut format::context::Output) {
    // The constant's integer type differs across FFmpeg versions, hence the cast.
    #[allow(clippy::unnecessary_cast)]
    // SAFETY: plain flag write on a valid, not-yet-started output context.
    unsafe {
        (*octx.as_mut_ptr()).flags |= ffmpeg_next::ffi::AVFMT_FLAG_BITEXACT as i32;
    }
}

pub struct ChunkEncoder {
    octx: format::context::Output,
    enc: encoder::Video,
    enc_tb: ffmpeg_next::Rational,
    ost_tb: ffmpeg_next::Rational,
    frame: frame::Video,
    settings: EncodeSettings,
    next_index: i64,
}

impl ChunkEncoder {
    pub fn create(path: &Path, settings: &EncodeSettings) -> anyhow::Result<Self> {
        init();
        let mut octx = format::output_as(path, "matroska")
            .with_context(|| format!("creating {}", path.display()))?;
        set_bitexact(&mut octx);
        let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);
        let codec = encoder::find_by_name(EncodeSettings::CODEC)
            .ok_or_else(|| anyhow!("ffv1 encoder missing"))?;
        let mut enc = codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()?;
        let enc_tb = to_ff(Rational::ONE / settings.fps);
        enc.set_width(settings.width);
        enc.set_height(settings.height);
        enc.set_format(Pixel::BGRZ);
        enc.set_time_base(enc_tb);
        enc.set_frame_rate(Some(to_ff(settings.fps)));
        enc.set_gop(settings.gop);
        let mut flags = codec::Flags::BITEXACT;
        if global_header {
            flags |= codec::Flags::GLOBAL_HEADER;
        }
        enc.set_flags(flags);
        let mut opts = Dictionary::new();
        opts.set("level", "3");
        opts.set("slices", "4");
        opts.set("slicecrc", "1");
        opts.set("threads", "1");
        let enc = enc.open_with(opts).context("opening ffv1 encoder")?;
        let mut ost = octx.add_stream(codec)?;
        ost.set_parameters(&enc);
        ost.set_time_base(enc_tb);
        octx.write_header()?;
        let ost_tb = octx.stream(0).expect("stream").time_base();
        let frame = frame::Video::new(Pixel::BGRZ, settings.width, settings.height);
        Ok(ChunkEncoder {
            octx,
            enc,
            enc_tb,
            ost_tb,
            frame,
            settings: settings.clone(),
            next_index: 0,
        })
    }

    /// Append one frame of tightly packed BGRA8/BGR0.
    pub fn push_bgra(&mut self, bgra: &[u8]) -> anyhow::Result<()> {
        self.push_bgra_strided(bgra, self.settings.width as usize * 4)
    }

    /// Like [`Self::push_bgra`] but rows are `row_stride` bytes apart (e.g. a
    /// GPU readback buffer with 256-byte aligned rows), saving a compaction copy.
    pub fn push_bgra_strided(&mut self, bgra: &[u8], row_stride: usize) -> anyhow::Result<()> {
        let (w, h) = (self.settings.width as usize, self.settings.height as usize);
        ensure!(row_stride >= w * 4, "row stride too small");
        ensure!(
            bgra.len() >= row_stride * (h - 1) + w * 4,
            "frame size mismatch"
        );
        let stride = self.frame.stride(0);
        let data = self.frame.data_mut(0);
        for y in 0..h {
            data[y * stride..y * stride + w * 4]
                .copy_from_slice(&bgra[y * row_stride..y * row_stride + w * 4]);
        }
        self.frame.set_pts(Some(self.next_index));
        self.next_index += 1;
        self.enc.send_frame(&self.frame)?;
        self.drain()
    }

    fn drain(&mut self) -> anyhow::Result<()> {
        let mut pkt = Packet::empty();
        while self.enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(0);
            // FFV1 can omit packet duration. Our encoder time base is exactly
            // one frame, so supplying it lets Matroska include the final frame
            // in the stream duration instead of ending at its starting PTS.
            if pkt.duration() <= 0 {
                pkt.set_duration(1);
            }
            pkt.rescale_ts(self.enc_tb, self.ost_tb);
            pkt.write_interleaved(&mut self.octx)?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> anyhow::Result<i64> {
        self.enc.send_eof()?;
        self.drain()?;
        self.octx.write_trailer()?;
        Ok(self.next_index)
    }
}

/// Frame index of a packet timestamp at `fps` (nearest), for re-timing on concat.
pub(crate) fn pts_to_frame(pts: i64, tb: ffmpeg_next::Rational, fps: FrameRate) -> i64 {
    RationalTime::from_pts(pts, to_core(tb)).frame_round(fps)
}
