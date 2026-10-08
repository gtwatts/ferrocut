//! Reading the master (Rusty's LGPL FFmpeg, the engine's `ffmpeg-next`
//! features): stream info, sequential BGRZ frames, and audio frames.

use std::path::Path;

use ferrocut_types::error::NodeError;
use ffmpeg_next::{
    Error as FfError, codec, format, frame, media, software::scaling, util::format::Pixel,
};

pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        ffmpeg_next::init().expect("ffmpeg init");
        ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Error);
    });
}

pub(crate) fn ff(what: impl std::fmt::Display) -> impl FnOnce(FfError) -> NodeError {
    move |e| NodeError::permanent(format!("{what}: {e}"))
}

/// Video/audio facts about a master file.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct MasterInfo {
    pub width: u32,
    pub height: u32,
    /// Frame rate as (num, den).
    pub fps: (i32, i32),
    /// Container's frame count, if it states one.
    pub frames_hint: Option<i64>,
    pub audio: Option<AudioInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AudioInfo {
    pub rate: u32,
    pub channels: u16,
    pub codec: String,
}

pub fn open(path: &Path) -> Result<format::context::Input, NodeError> {
    init();
    if !path.is_file() {
        return Err(NodeError::permanent(format!(
            "master {} does not exist",
            path.display()
        )));
    }
    format::input(path).map_err(ff(format!("opening {}", path.display())))
}

pub fn probe(path: &Path) -> Result<MasterInfo, NodeError> {
    let ictx = open(path)?;
    let vs = ictx
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| NodeError::permanent(format!("{}: no video stream", path.display())))?;
    let par = vs.parameters();
    let vdec = codec::context::Context::from_parameters(par)
        .and_then(|c| c.decoder().video())
        .map_err(ff("video decoder"))?;
    let mut rate = vs.avg_frame_rate();
    if rate.numerator() <= 0 || rate.denominator() <= 0 {
        rate = vs.rate();
    }
    if rate.numerator() <= 0 || rate.denominator() <= 0 {
        return Err(NodeError::permanent(format!(
            "{}: unknown frame rate",
            path.display()
        )));
    }
    let frames_hint = (vs.frames() > 0).then_some(vs.frames());
    let audio = ictx.streams().best(media::Type::Audio).and_then(|s| {
        let dec = codec::context::Context::from_parameters(s.parameters())
            .ok()?
            .decoder()
            .audio()
            .ok()?;
        Some(AudioInfo {
            rate: dec.rate(),
            channels: dec.channels(),
            codec: format!("{:?}", dec.id()),
        })
    });
    Ok(MasterInfo {
        width: vdec.width(),
        height: vdec.height(),
        fps: (rate.numerator(), rate.denominator()),
        frames_hint,
        audio,
    })
}

pub(crate) fn open_decoder(
    ictx: &format::context::Input,
    kind: media::Type,
    threads: usize,
) -> Result<(usize, codec::decoder::Decoder), NodeError> {
    let stream = ictx
        .streams()
        .best(kind)
        .ok_or_else(|| NodeError::permanent(format!("no {kind:?} stream")))?;
    let mut cctx =
        codec::context::Context::from_parameters(stream.parameters()).map_err(ff("decoder"))?;
    cctx.set_threading(codec::threading::Config {
        kind: if threads > 1 {
            codec::threading::Type::Slice
        } else {
            codec::threading::Type::None
        },
        count: threads.max(1),
    });
    Ok((stream.index(), cctx.decoder()))
}

/// Drive `dec` over every packet of `stream`, calling `on_frame` per frame.
pub(crate) fn pump<F: std::ops::DerefMut<Target = frame::Frame>>(
    ictx: &mut format::context::Input,
    stream: usize,
    dec: &mut codec::decoder::Opened,
    frame: &mut F,
    mut on_frame: impl FnMut(&F) -> Result<(), NodeError>,
) -> Result<(), NodeError> {
    let mut drain = |dec: &mut codec::decoder::Opened, frame: &mut F| -> Result<(), NodeError> {
        loop {
            match dec.receive_frame(frame) {
                Ok(()) => on_frame(frame)?,
                Err(FfError::Eof) => return Ok(()),
                Err(FfError::Other { errno }) if errno == ffmpeg_next::util::error::EAGAIN => {
                    return Ok(());
                }
                Err(e) => return Err(ff("decoding")(e)),
            }
        }
    };
    for (s, p) in ictx.packets() {
        if s.index() == stream {
            dec.send_packet(&p).map_err(ff("decoding"))?;
            drain(dec, frame)?;
        }
    }
    dec.send_eof().map_err(ff("decoding"))?;
    drain(dec, frame)
}

/// Decode every video frame as tightly packed BGRZ, in order; `f(index, pixels)`.
pub fn for_each_frame(
    path: &Path,
    decode_threads: usize,
    mut f: impl FnMut(usize, Vec<u8>) -> Result<(), NodeError>,
) -> Result<usize, NodeError> {
    let mut ictx = open(path)?;
    let (si, dec) = open_decoder(&ictx, media::Type::Video, decode_threads)?;
    let mut dec = dec.video().map_err(ff("video decoder"))?;
    let (w, h) = (dec.width() as usize, dec.height() as usize);
    let mut scaler: Option<scaling::Context> = None;
    let mut n = 0usize;
    let mut converted = frame::Video::empty();
    let mut fr = frame::Video::empty();
    pump(&mut ictx, si, &mut dec, &mut fr, |src| {
        let src = if src.format() == Pixel::BGRZ {
            src
        } else {
            let sc = match &mut scaler {
                Some(s) => s,
                None => scaler.insert(
                    scaling::Context::get(
                        src.format(),
                        w as u32,
                        h as u32,
                        Pixel::BGRZ,
                        w as u32,
                        h as u32,
                        scaling::Flags::POINT | scaling::Flags::BITEXACT,
                    )
                    .map_err(ff("pixel conversion"))?,
                ),
            };
            sc.run(src, &mut converted)
                .map_err(ff("pixel conversion"))?;
            &converted
        };
        let stride = src.stride(0);
        let data = src.data(0);
        let mut packed = vec![0u8; w * h * 4];
        for y in 0..h {
            packed[y * w * 4..(y + 1) * w * 4]
                .copy_from_slice(&data[y * stride..y * stride + w * 4]);
        }
        f(n, packed)?;
        n += 1;
        Ok(())
    })?;
    Ok(n)
}

/// Decode frames `[start, end)` (frame indices on the master's timeline,
/// from timestamps) as tightly packed BGRZ; `end: None` = to the end.
/// Seeks backward to the keyframe at or before `start` (FFV1 non-keyframes
/// depend on earlier frames) and decodes forward. Returns the frame count.
pub fn decode_range(
    path: &Path,
    fps: (i32, i32),
    start: i64,
    end: Option<i64>,
    mut f: impl FnMut(i64, Vec<u8>) -> Result<(), NodeError>,
) -> Result<i64, NodeError> {
    let mut ictx = open(path)?;
    let (si, dec) = open_decoder(&ictx, media::Type::Video, 1)?;
    let tb = ictx.stream(si).expect("video stream").time_base();
    let mut dec = dec.video().map_err(ff("video decoder"))?;
    let (w, h) = (dec.width() as usize, dec.height() as usize);
    let (tn, td) = (tb.numerator() as i128, tb.denominator() as i128);
    let (fnum, fden) = (fps.0 as i128, fps.1 as i128);
    let to_index = |pts: i64| -> i64 {
        let (num, den) = (pts as i128 * tn * fnum, td * fden);
        (2 * num + den).div_euclid(2 * den) as i64 // nearest frame
    };
    if start > 0 {
        // Floor of the start frame's time: never past its (rounded) pts.
        let ts = (start as i128 * fden * td).div_euclid(fnum * tn) as i64;
        // SAFETY: seeking our own open input context on a valid stream index.
        let rc = unsafe {
            ffmpeg_next::ffi::av_seek_frame(
                ictx.as_mut_ptr(),
                si as i32,
                ts,
                ffmpeg_next::ffi::AVSEEK_FLAG_BACKWARD,
            )
        };
        if rc < 0 {
            return Err(NodeError::permanent(format!(
                "{}: cannot seek to frame {start} ({})",
                path.display(),
                FfError::from(rc)
            )));
        }
    }
    let mut scaler: Option<scaling::Context> = None;
    let mut converted = frame::Video::empty();
    let mut fr = frame::Video::empty();
    let mut n = 0i64;
    let mut done = false;
    let mut handle = |fr: &frame::Video, done: &mut bool| -> Result<(), NodeError> {
        let pts = fr.timestamp().or(fr.pts()).ok_or_else(|| {
            NodeError::permanent(format!("{}: frame without timestamp", path.display()))
        })?;
        let idx = to_index(pts);
        if idx < start {
            return Ok(());
        }
        if end.is_some_and(|e| idx >= e) {
            *done = true;
            return Ok(());
        }
        if idx != start + n {
            return Err(NodeError::permanent(format!(
                "{}: expected frame {} after seeking, got {idx} (irregular timestamps?)",
                path.display(),
                start + n
            )));
        }
        let src = if fr.format() == Pixel::BGRZ {
            fr
        } else {
            let sc = match &mut scaler {
                Some(s) => s,
                None => scaler.insert(
                    scaling::Context::get(
                        fr.format(),
                        w as u32,
                        h as u32,
                        Pixel::BGRZ,
                        w as u32,
                        h as u32,
                        scaling::Flags::POINT | scaling::Flags::BITEXACT,
                    )
                    .map_err(ff("pixel conversion"))?,
                ),
            };
            sc.run(fr, &mut converted).map_err(ff("pixel conversion"))?;
            &converted
        };
        let stride = src.stride(0);
        let data = src.data(0);
        let mut packed = vec![0u8; w * h * 4];
        for y in 0..h {
            packed[y * w * 4..(y + 1) * w * 4]
                .copy_from_slice(&data[y * stride..y * stride + w * 4]);
        }
        f(idx, packed)?;
        n += 1;
        Ok(())
    };
    let eagain = |e: &FfError| matches!(e, FfError::Other { errno } if *errno == ffmpeg_next::util::error::EAGAIN);
    for (s, p) in ictx.packets() {
        if s.index() != si {
            continue;
        }
        dec.send_packet(&p).map_err(ff("decoding"))?;
        loop {
            match dec.receive_frame(&mut fr) {
                Ok(()) => handle(&fr, &mut done)?,
                Err(e) if eagain(&e) || e == FfError::Eof => break,
                Err(e) => return Err(ff("decoding")(e)),
            }
            if done {
                return Ok(n);
            }
        }
    }
    dec.send_eof().map_err(ff("decoding"))?;
    loop {
        match dec.receive_frame(&mut fr) {
            Ok(()) => handle(&fr, &mut done)?,
            Err(e) if eagain(&e) || e == FfError::Eof => break,
            Err(e) => return Err(ff("decoding")(e)),
        }
        if done {
            break;
        }
    }
    Ok(n)
}
