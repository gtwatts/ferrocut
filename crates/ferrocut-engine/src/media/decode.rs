//! Frame-accurate decoder with keyframe seek + decode-forward pre-roll.

use std::path::Path;

use anyhow::{Context as _, anyhow, bail};
use ferrocut_core::{Rational, RationalTime};
use ffmpeg_next::{
    Error as FfError, codec, format, frame, media, software::scaling, util::format::Pixel,
};

use super::{init, to_core};

struct Decoded {
    time: RationalTime,
    pts: i64,
    frame: frame::Video,
}

/// Owns one demuxer + decoder + scaler for one source file, used by exactly one
/// render worker. Holds the most recently decoded frame so sequential access is
/// cheap and only seeks on backwards or long forward jumps.
pub struct Decoder {
    ictx: format::context::Input,
    stream_index: usize,
    decoder: codec::decoder::Video,
    time_base: Rational,
    start_pts: i64,
    /// Half a source frame: tolerance for container timestamp rounding.
    half_frame: Rational,
    out_w: u32,
    out_h: u32,
    scaler: Option<(Pixel, u32, u32, scaling::Context)>,
    cur: Option<Decoded>,
    ahead: Option<Decoded>,
    eof_sent: bool,
    rgba: Vec<u8>,
    rgba_pts: Option<i64>,
    pub frames_decoded: u64,
    pub seeks: u64,
}

// SAFETY: every FFmpeg object here (format ctx, codec ctx, SwsContext, frames) is
// exclusively owned by this struct and only ever touched by the worker that owns
// it. FFmpeg contexts may move between threads as long as they are not used
// concurrently; ffmpeg-next just doesn't mark `scaling::Context` Send.
unsafe impl Send for Decoder {}

const FORWARD_SEEK_THRESHOLD: Rational = Rational::from_int(2);

impl Decoder {
    pub fn open(path: &Path, out_w: u32, out_h: u32) -> anyhow::Result<Self> {
        init();
        let ictx = format::input(path).with_context(|| format!("opening {}", path.display()))?;
        let stream = ictx
            .streams()
            .best(media::Type::Video)
            .ok_or_else(|| anyhow!("no video stream"))?;
        let stream_index = stream.index();
        let time_base = to_core(stream.time_base());
        let start_pts = if stream.start_time() == ffmpeg_next::ffi::AV_NOPTS_VALUE {
            0
        } else {
            stream.start_time()
        };
        let rate = super::stream_rate(&stream).unwrap_or(Rational::ZERO);
        if rate <= Rational::ZERO {
            bail!("cannot determine frame rate");
        }
        let half_frame = Rational::ONE / rate / Rational::from_int(2);
        let mut cctx = codec::context::Context::from_parameters(stream.parameters())?;
        // One decode thread per worker: parallelism comes from chunks.
        cctx.set_threading(codec::threading::Config {
            kind: codec::threading::Type::None,
            count: 1,
        });
        let decoder = cctx.decoder().video()?;
        Ok(Decoder {
            ictx,
            stream_index,
            decoder,
            time_base,
            start_pts,
            half_frame,
            out_w,
            out_h,
            scaler: None,
            cur: None,
            ahead: None,
            eof_sent: false,
            rgba: Vec::new(),
            rgba_pts: None,
            frames_decoded: 0,
            seeks: 0,
        })
    }

    fn seek(&mut self, t: RationalTime) -> anyhow::Result<()> {
        // avformat_seek_file with stream -1 works in AV_TIME_BASE (microseconds).
        let start_us =
            (Rational::from_int(self.start_pts) * self.time_base * Rational::from_int(1_000_000))
                .floor();
        let ts = (t.seconds() * Rational::from_int(1_000_000)).floor() + start_us;
        if self.ictx.seek(ts, ..ts).is_err() {
            self.ictx.seek(i64::MIN, ..).context("seek failed")?;
        }
        self.decoder.flush();
        self.cur = None;
        self.ahead = None;
        self.eof_sent = false;
        self.seeks += 1;
        Ok(())
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<Decoded>> {
        loop {
            let mut f = frame::Video::empty();
            match self.decoder.receive_frame(&mut f) {
                Ok(()) => {
                    let pts = f
                        .timestamp()
                        .or(f.pts())
                        .ok_or_else(|| anyhow!("decoded frame without timestamp"))?;
                    self.frames_decoded += 1;
                    let time =
                        RationalTime(Rational::from_int(pts - self.start_pts) * self.time_base);
                    return Ok(Some(Decoded {
                        time,
                        pts,
                        frame: f,
                    }));
                }
                Err(FfError::Eof) => return Ok(None),
                Err(FfError::Other { errno }) if errno == ffmpeg_next::util::error::EAGAIN => {}
                Err(e) => return Err(e.into()),
            }
            if self.eof_sent {
                return Ok(None);
            }
            match self.ictx.packets().next() {
                Some((s, p)) => {
                    if s.index() == self.stream_index {
                        self.decoder.send_packet(&p)?;
                    }
                }
                None => {
                    self.decoder.send_eof()?;
                    self.eof_sent = true;
                }
            }
        }
    }

    /// RGBA8 (straight alpha, out_w x out_h, tightly packed) of the source frame
    /// displayed at source time `t`: the last frame whose timestamp is <= t
    /// (within half a frame). Past the end, the last frame is held.
    pub fn frame_at(&mut self, t: RationalTime) -> anyhow::Result<&[u8]> {
        let target = RationalTime(t.seconds() + self.half_frame);
        let needs_seek = match (&self.cur, &self.ahead) {
            (Some(c), _) => c.time > target || (t - c.time).seconds() > FORWARD_SEEK_THRESHOLD,
            (None, Some(a)) => a.time > target,
            (None, None) => true,
        };
        if needs_seek {
            self.seek(t)?;
        }
        loop {
            if let Some(a) = &self.ahead {
                if a.time > target {
                    break;
                }
                self.cur = self.ahead.take();
                continue;
            }
            match self.next_frame()? {
                Some(d) => self.ahead = Some(d),
                None => break,
            }
        }
        let chosen = match (&self.cur, &self.ahead) {
            (Some(c), _) => c,
            (None, Some(a)) => a, // t precedes the first frame
            (None, None) => bail!("no decodable frames at {t}"),
        };
        if self.rgba_pts != Some(chosen.pts) {
            let src = &chosen.frame;
            let key = (src.format(), src.width(), src.height());
            if self.scaler.as_ref().map(|s| (s.0, s.1, s.2)) != Some(key) {
                let ctx = scaling::Context::get(
                    key.0,
                    key.1,
                    key.2,
                    Pixel::RGBA,
                    self.out_w,
                    self.out_h,
                    scaling::Flags::BILINEAR
                        | scaling::Flags::BITEXACT
                        | scaling::Flags::ACCURATE_RND,
                )?;
                self.scaler = Some((key.0, key.1, key.2, ctx));
            }
            let mut out = frame::Video::empty();
            self.scaler.as_mut().expect("scaler").3.run(src, &mut out)?;
            let row = self.out_w as usize * 4;
            let stride = out.stride(0);
            let data = out.data(0);
            self.rgba.clear();
            for y in 0..self.out_h as usize {
                self.rgba
                    .extend_from_slice(&data[y * stride..y * stride + row]);
            }
            self.rgba_pts = Some(chosen.pts);
        }
        Ok(&self.rgba)
    }
}
