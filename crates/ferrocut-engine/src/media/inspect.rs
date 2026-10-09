//! Exact frames of an encoded file (a delivery, an excerpt), for looking at
//! what was actually written rather than re-rendering the timeline.
//!
//! Frames are addressed by ordinal: the position among the frames the decoder
//! outputs, in presentation order, from the start of the stream (frames the
//! container marks for discard, such as edit-list pre-roll, are not counted).
//! Each frame keeps its own timestamp and the stream time base, so a
//! variable-rate file is never mapped through its nominal fps. Decoding is
//! sequential from the start: cost grows with the largest index requested
//! (or the whole stream for indices counted from the end), bounded by
//! [`MAX_DECODE_FRAMES`].

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use anyhow::{Context as _, anyhow, bail, ensure};
use ferrocut_core::CancelToken;
use ferrocut_core::{Rational, RationalTime};
use ffmpeg_next::util::color;
use ffmpeg_next::util::format::Pixel;
use ffmpeg_next::{Error as FfError, codec, format, frame, media, software::scaling};
use serde::Serialize;

use super::{init, to_core};

/// Most frames one request may return.
pub const MAX_INSPECT_FRAMES: usize = 64;
/// Most frames decoded for one request (about an hour at 24 fps).
pub const MAX_DECODE_FRAMES: u64 = 100_000;
/// Largest frame accepted (width x height).
pub const MAX_FRAME_PIXELS: u64 = 1 << 26;

/// One decoded frame, straight RGBA8 at the frame's own size.
#[derive(Clone, Debug)]
pub struct EncodedFrame {
    /// Ordinal in presentation order (0-based).
    pub index: u64,
    /// Best-effort presentation timestamp in `time_base` units, when present.
    pub pts: Option<i64>,
    /// `(pts - stream start) * time_base`: exact, never derived from fps.
    pub time: Option<RationalTime>,
    pub key_frame: bool,
    /// The decoder flagged this frame as corrupt (it is still returned).
    pub corrupt: bool,
    pub width: u32,
    pub height: u32,
    /// Source pixel format and the YUV matrix/range used to convert it.
    pub source_format: String,
    pub conversion: String,
    pub rgba: Vec<u8>,
}

/// What [`decode_frames`] read.
#[derive(Clone, Debug, Serialize)]
pub struct EncodedStream {
    pub stream_index: usize,
    pub codec: String,
    /// Stream time base (rational string).
    pub time_base: String,
    pub start_pts: Option<i64>,
    /// The container's nominal rate, informational only: frame times come
    /// from timestamps.
    pub nominal_rate: Option<String>,
    pub frames_decoded: u64,
    /// Total frames, when decoding reached the end of the stream.
    pub frame_count: Option<u64>,
}

/// Decode the frames at `requested` ordinals (negative: from the end, -1 is
/// the last frame) from the best video stream of `path`, in request order.
/// Errors on an index past the end (naming the frame count), a frame larger
/// than [`MAX_FRAME_PIXELS`], a decode error or cancellation.
pub fn decode_frames(
    path: &Path,
    requested: &[i64],
    cancel: &CancelToken,
) -> anyhow::Result<(EncodedStream, Vec<EncodedFrame>)> {
    ensure!(!requested.is_empty(), "no frames requested");
    ensure!(
        requested.len() <= MAX_INSPECT_FRAMES,
        "{} frames requested (max {MAX_INSPECT_FRAMES})",
        requested.len()
    );
    let max_pos = requested.iter().copied().filter(|&i| i >= 0).max();
    let tail = requested
        .iter()
        .filter(|&&i| i < 0)
        .map(|&i| i.unsigned_abs())
        .max()
        .unwrap_or(0);
    ensure!(
        tail <= MAX_INSPECT_FRAMES as u64,
        "frames counted from the end go back at most {MAX_INSPECT_FRAMES}"
    );
    if let Some(m) = max_pos {
        ensure!(
            (m as u64) < MAX_DECODE_FRAMES,
            "frame {m} is past the decode limit ({MAX_DECODE_FRAMES} frames)"
        );
    }

    init();
    let mut ictx = format::input(path).with_context(|| format!("opening {}", path.display()))?;
    let stream = ictx
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| anyhow!("{} has no video stream", path.display()))?;
    let stream_index = stream.index();
    let time_base = to_core(stream.time_base());
    let start_pts =
        (stream.start_time() != ffmpeg_next::ffi::AV_NOPTS_VALUE).then(|| stream.start_time());
    let nominal_rate = super::stream_rate(&stream).map(|r| r.to_string());
    let mut cctx = codec::context::Context::from_parameters(stream.parameters())?;
    cctx.set_threading(codec::threading::Config {
        kind: codec::threading::Type::None,
        count: 1,
    });
    let mut decoder = cctx.decoder().video()?;
    let codec_name = decoder
        .codec()
        .map(|c| c.name().to_string())
        .unwrap_or_default();

    // Positive ordinals wanted -> request positions; the last `tail` frames
    // are kept (undecoded to RGBA) until the end is known.
    let mut wanted: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (pos, &i) in requested.iter().enumerate() {
        if i >= 0 {
            wanted.entry(i as u64).or_default().push(pos);
        }
    }
    let mut out: Vec<Option<EncodedFrame>> = vec![None; requested.len()];
    let mut ring: VecDeque<(u64, frame::Video)> = VecDeque::new();
    let mut scaler: Option<Scaler> = None;
    let mut index: u64 = 0;
    let mut eof_sent = false;
    let mut reached_end = false;
    let need_end = tail > 0;
    let convert = |f: &frame::Video, i: u64, scaler: &mut Option<Scaler>| {
        to_encoded(f, i, time_base, start_pts, scaler)
    };

    'decode: loop {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        let mut f = frame::Video::empty();
        match decoder.receive_frame(&mut f) {
            Ok(()) => {
                let i = index;
                index += 1;
                ensure!(
                    index <= MAX_DECODE_FRAMES,
                    "{} has more than {MAX_DECODE_FRAMES} frames; request frames from the start",
                    path.display()
                );
                if let Some(positions) = wanted.remove(&i) {
                    let e = convert(&f, i, &mut scaler)?;
                    for p in positions {
                        out[p] = Some(e.clone());
                    }
                }
                if need_end {
                    ring.push_back((i, f));
                    if ring.len() as u64 > tail {
                        ring.pop_front();
                    }
                } else if wanted.is_empty() {
                    break 'decode;
                }
                continue;
            }
            Err(FfError::Eof) => {
                reached_end = true;
                break;
            }
            Err(FfError::Other { errno }) if errno == ffmpeg_next::util::error::EAGAIN => {}
            Err(e) => {
                return Err(anyhow!(e))
                    .with_context(|| format!("decoding {} after frame {index}", path.display()));
            }
        }
        if eof_sent {
            reached_end = true;
            break;
        }
        match ictx.packets().next() {
            Some((s, p)) => {
                if s.index() == stream_index {
                    decoder.send_packet(&p).with_context(|| {
                        format!(
                            "{}: corrupt or unsupported packet after frame {index}",
                            path.display()
                        )
                    })?;
                }
            }
            None => {
                decoder.send_eof()?;
                eof_sent = true;
            }
        }
    }

    let frame_count = reached_end.then_some(index);
    if let Some((&missing, _)) = wanted.iter().next() {
        bail!(
            "frame {missing} is past the end of {} ({index} frames)",
            path.display()
        );
    }
    for (pos, &i) in requested.iter().enumerate() {
        if i < 0 {
            let n = frame_count.expect("decoded to the end");
            let want = n.checked_sub(i.unsigned_abs()).ok_or_else(|| {
                anyhow!(
                    "frame {i} is before the start of {} ({n} frames)",
                    path.display()
                )
            })?;
            let (_, f) = ring
                .iter()
                .find(|(j, _)| *j == want)
                .expect("kept the last frames");
            out[pos] = Some(convert(f, want, &mut scaler)?);
        }
    }
    let frames = out
        .into_iter()
        .map(|f| f.expect("every request filled"))
        .collect();
    Ok((
        EncodedStream {
            stream_index,
            codec: codec_name,
            time_base: time_base.to_string(),
            start_pts,
            nominal_rate,
            frames_decoded: index,
            frame_count,
        },
        frames,
    ))
}

struct Scaler {
    key: (Pixel, u32, u32, color::Space, color::Range),
    ctx: scaling::Context,
    conversion: String,
}

/// libswscale coefficient table id for a tagged YUV matrix; `None` keeps the
/// library default (BT.601), which is reported as untagged.
fn sws_matrix(space: color::Space) -> Option<(i32, &'static str)> {
    use ffmpeg_next::ffi::{
        SWS_CS_BT2020, SWS_CS_FCC, SWS_CS_ITU601, SWS_CS_ITU709, SWS_CS_SMPTE240M,
    };
    Some(match space {
        color::Space::BT709 => (SWS_CS_ITU709 as i32, "bt709"),
        color::Space::BT470BG | color::Space::SMPTE170M => (SWS_CS_ITU601 as i32, "bt601"),
        color::Space::BT2020NCL | color::Space::BT2020CL => (SWS_CS_BT2020 as i32, "bt2020"),
        color::Space::FCC => (SWS_CS_FCC as i32, "fcc"),
        color::Space::SMPTE240M => (SWS_CS_SMPTE240M as i32, "smpte240m"),
        _ => return None,
    })
}

fn to_encoded(
    f: &frame::Video,
    index: u64,
    time_base: Rational,
    start_pts: Option<i64>,
    scaler: &mut Option<Scaler>,
) -> anyhow::Result<EncodedFrame> {
    let (w, h) = (f.width(), f.height());
    ensure!(
        w > 0 && h > 0 && (w as u64) * (h as u64) <= MAX_FRAME_PIXELS,
        "frame {index} is {w}x{h} (max {MAX_FRAME_PIXELS} pixels)"
    );
    let key = (f.format(), w, h, f.color_space(), f.color_range());
    if scaler.as_ref().map(|s| s.key) != Some(key) {
        let mut ctx = scaling::Context::get(
            key.0,
            w,
            h,
            Pixel::RGBA,
            w,
            h,
            scaling::Flags::POINT | scaling::Flags::BITEXACT | scaling::Flags::ACCURATE_RND,
        )?;
        let full = key.4 == color::Range::JPEG;
        let range = if full { "full" } else { "limited" };
        let is_yuv = format!("{:?}", key.0)
            .to_ascii_uppercase()
            .starts_with("YUV")
            || format!("{:?}", key.0)
                .to_ascii_uppercase()
                .starts_with("NV");
        let conversion = match (is_yuv, sws_matrix(key.3)) {
            (false, _) => "rgb source".to_string(),
            (true, Some((cs, name))) => {
                // SAFETY: the context is valid and exclusively owned here; the
                // coefficient tables are static libswscale data.
                let rc = unsafe {
                    let table = ffmpeg_next::ffi::sws_getCoefficients(cs);
                    ffmpeg_next::ffi::sws_setColorspaceDetails(
                        ctx.as_mut_ptr(),
                        table,
                        full as i32,
                        table,
                        1,
                        0,
                        1 << 16,
                        1 << 16,
                    )
                };
                ensure!(rc >= 0, "setting the {name} matrix failed ({rc})");
                format!("{name} {range}")
            }
            (true, None) => format!("untagged matrix (libswscale default bt601) {range}"),
        };
        *scaler = Some(Scaler {
            key,
            ctx,
            conversion,
        });
    }
    let s = scaler.as_mut().expect("scaler");
    let mut rgb = frame::Video::empty();
    s.ctx.run(f, &mut rgb)?;
    let row = w as usize * 4;
    let stride = rgb.stride(0);
    let data = rgb.data(0);
    let mut rgba = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        rgba.extend_from_slice(&data[y * stride..y * stride + row]);
    }
    let pts = f.timestamp().or(f.pts());
    let time =
        pts.map(|p| RationalTime(Rational::from_int(p - start_pts.unwrap_or(0)) * time_base));
    Ok(EncodedFrame {
        index,
        pts,
        time,
        key_frame: f.is_key(),
        corrupt: f.is_corrupt(),
        width: w,
        height: h,
        source_format: format!("{:?}", key.0).to_ascii_lowercase(),
        conversion: s.conversion.clone(),
        rgba,
    })
}
