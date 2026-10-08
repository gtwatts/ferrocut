//! FFmpeg input (Rusty's LGPL build, same `ffmpeg-next` features as the
//! engine, so nothing in the shared build changes): sequential BGRZ frames
//! from the engine's FFV1 chunk masters, and PCM from an audio file.

use std::path::Path;

use anyhow::{Context as _, anyhow, bail};
use ffmpeg_next::{
    Error as FfError, codec, format, frame, media, software::scaling, util::format::Pixel,
};

use crate::audio::AudioBuffer;

pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        ffmpeg_next::init().expect("ffmpeg init");
        ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Error);
    });
}

fn open_decoder(
    ictx: &format::context::Input,
    kind: media::Type,
) -> anyhow::Result<(usize, codec::decoder::Decoder)> {
    let stream = ictx
        .streams()
        .best(kind)
        .ok_or_else(|| anyhow!("no {kind:?} stream"))?;
    let mut cctx = codec::context::Context::from_parameters(stream.parameters())?;
    cctx.set_threading(codec::threading::Config {
        kind: codec::threading::Type::None,
        count: 1,
    });
    Ok((stream.index(), cctx.decoder()))
}

/// Drive `decoder` over every packet of `stream`, calling `on_frame` for each
/// decoded frame (in decode order, which is presentation order for intra codecs).
fn pump<F: std::ops::DerefMut<Target = frame::Frame>>(
    ictx: &mut format::context::Input,
    stream: usize,
    dec: &mut codec::decoder::Opened,
    frame: &mut F,
    mut on_frame: impl FnMut(&F) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut drain = |dec: &mut codec::decoder::Opened, frame: &mut F| -> anyhow::Result<bool> {
        loop {
            match dec.receive_frame(frame) {
                Ok(()) => on_frame(frame)?,
                Err(FfError::Eof) => return Ok(true),
                Err(FfError::Other { errno }) if errno == ffmpeg_next::util::error::EAGAIN => {
                    return Ok(false);
                }
                Err(e) => return Err(e.into()),
            }
        }
    };
    for (s, p) in ictx.packets() {
        if s.index() == stream {
            dec.send_packet(&p)?;
            drain(dec, frame)?;
        }
    }
    dec.send_eof()?;
    drain(dec, frame)?;
    Ok(())
}

/// Decode every video frame of `path` as tightly packed BGRZ (B, G, R, x).
/// Calls `f(index, pixels, width, height)` in order.
pub fn for_each_frame(
    path: &Path,
    mut f: impl FnMut(usize, &[u8], u32, u32) -> anyhow::Result<()>,
) -> anyhow::Result<usize> {
    init();
    let mut ictx = format::input(path).with_context(|| format!("opening {}", path.display()))?;
    let (si, dec) = open_decoder(&ictx, media::Type::Video)?;
    let mut dec = dec.video()?;
    let (w, h) = (dec.width(), dec.height());
    let mut scaler: Option<scaling::Context> = None;
    let mut packed = vec![0u8; w as usize * h as usize * 4];
    let mut n = 0usize;
    let mut converted = frame::Video::empty();
    let mut fr = frame::Video::empty();
    pump(&mut ictx, si, &mut dec, &mut fr, |src| {
        let src = if src.format() == Pixel::BGRZ {
            src
        } else {
            let sc = match &mut scaler {
                Some(s) => s,
                None => scaler.insert(scaling::Context::get(
                    src.format(),
                    w,
                    h,
                    Pixel::BGRZ,
                    w,
                    h,
                    scaling::Flags::POINT | scaling::Flags::BITEXACT,
                )?),
            };
            sc.run(src, &mut converted)?;
            &converted
        };
        let stride = src.stride(0);
        let row = w as usize * 4;
        let data = src.data(0);
        for y in 0..h as usize {
            packed[y * row..(y + 1) * row].copy_from_slice(&data[y * stride..y * stride + row]);
        }
        f(n, &packed, w, h)?;
        n += 1;
        Ok(())
    })?;
    Ok(n)
}

/// Decode the best audio stream of `path` to interleaved f32 (no resampling:
/// analysis runs at the file's own rate).
pub fn decode_audio(path: &Path) -> anyhow::Result<AudioBuffer> {
    use ffmpeg_next::format::Sample;
    use ffmpeg_next::format::sample::Type as Layout;
    init();
    let mut ictx = format::input(path).with_context(|| format!("opening {}", path.display()))?;
    let (si, dec) = open_decoder(&ictx, media::Type::Audio)?;
    let mut dec = dec.audio()?;
    let mut out: Vec<f32> = Vec::new();
    let mut rate = 0u32;
    let mut channels = 0usize;
    let mut fr = frame::Audio::empty();
    pump(&mut ictx, si, &mut dec, &mut fr, |a| {
        let ch = a.channels() as usize;
        let n = a.samples();
        if rate == 0 {
            rate = a.rate();
            channels = ch;
        } else if a.rate() != rate || ch != channels {
            bail!("audio format changes mid-stream");
        }
        let planar = matches!(
            a.format(),
            Sample::U8(Layout::Planar)
                | Sample::I16(Layout::Planar)
                | Sample::I32(Layout::Planar)
                | Sample::I64(Layout::Planar)
                | Sample::F32(Layout::Planar)
                | Sample::F64(Layout::Planar)
        );
        let bytes = match a.format() {
            Sample::U8(_) => 1,
            Sample::I16(_) => 2,
            Sample::I32(_) | Sample::F32(_) => 4,
            Sample::I64(_) | Sample::F64(_) => 8,
            Sample::None => bail!("unknown sample format"),
        };
        let conv = |b: &[u8]| -> f32 {
            match a.format() {
                Sample::U8(_) => (b[0] as f32 - 128.0) / 128.0,
                Sample::I16(_) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                Sample::I32(_) => {
                    (i32::from_le_bytes(b[..4].try_into().unwrap()) as f64 / 2147483648.0) as f32
                }
                Sample::I64(_) => {
                    (i64::from_le_bytes(b[..8].try_into().unwrap()) as f64 / 9.223372036854776e18)
                        as f32
                }
                Sample::F32(_) => f32::from_le_bytes(b[..4].try_into().unwrap()),
                Sample::F64(_) => f64::from_le_bytes(b[..8].try_into().unwrap()) as f32,
                Sample::None => 0.0,
            }
        };
        for i in 0..n {
            for c in 0..ch {
                let v = if planar {
                    conv(&a.data(c)[i * bytes..])
                } else {
                    conv(&a.data(0)[(i * ch + c) * bytes..])
                };
                out.push(v);
            }
        }
        Ok(())
    })?;
    if rate == 0 {
        bail!("{}: no audio decoded", path.display());
    }
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(AudioBuffer::new(rate, channels as u16, out, label))
}
