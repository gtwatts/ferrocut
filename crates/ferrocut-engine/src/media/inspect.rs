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
//!
//! Containment: the file is opened once and read only through that handle
//! (custom I/O). The demuxer may not open anything else: the protocol
//! whitelist names no protocol, and only self-contained container demuxers
//! ([`DEMUXERS`]) are allowed, so playlists (HLS), concat lists or external
//! references fail instead of reading other files.
//!
//! Identity: the handle's bytes are hashed before decoding and again after;
//! a difference is an error. That is an observed recheck of the same open
//! file, not an immutable snapshot: a change that is undone between the two
//! hashes is not detectable.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context as _, anyhow, bail, ensure};
use ferrocut_core::CancelToken;
use ferrocut_core::{Rational, RationalTime};
use ffmpeg_next::util::color;
use ffmpeg_next::util::format::Pixel;
use ffmpeg_next::{Dictionary, Error as FfError, Packet, codec, format, frame, software::scaling};
use serde::Serialize;

use super::{init, to_core};

/// Most frames one request may return.
pub const MAX_INSPECT_FRAMES: usize = 64;
/// Most frames decoded for one request (about an hour at 24 fps).
pub const MAX_DECODE_FRAMES: u64 = 100_000;
/// Largest frame accepted (width x height), checked on the stream's declared
/// size and on every decoded frame before it is kept or converted.
pub const MAX_FRAME_PIXELS: u64 = 1 << 26;
/// Tallest frame accepted (rows), checked with the pixel limit on every
/// declared and decoded frame. It bounds the decoder's alignment headroom
/// (see `decoder_pixel_cap`).
pub const MAX_FRAME_HEIGHT: u64 = 16_384;
/// Widest row alignment the decoder allocation cap covers. FFmpeg checks
/// `max_pixels` against `FFALIGN(width, STRIDE_ALIGN) * height`, and
/// STRIDE_ALIGN is 64 in the LGPL build used here (at most 64 on x86-64).
const STRIDE_ALIGN_BOUND: u64 = 64;
/// Most bytes held at once for returned frames (RGBA) plus the frames kept
/// for indices counted from the end (decoded planes).
pub const MAX_RETAINED_BYTES: u64 = 1 << 30;
/// Self-contained container demuxers this route opens (FFmpeg names): ones
/// that declare their streams in the header. Formats that discover streams
/// while reading (MPEG-TS/PS, FLV, Ogg) are not offered, because FFmpeg
/// probes such late streams with default decoder options (no pixel cap);
/// any demuxer that still flags late discovery is refused at open.
pub const DEMUXERS: &str =
    "matroska,webm,mov,mp4,m4a,3gp,3g2,mj2,avi,ivf,nut,mxf,h264,hevc,yuv4mpegpipe";

/// How a frame's code values became RGBA, and what was not converted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Conversion {
    /// FFmpeg pixel format name of the decoded frame.
    pub source_format: String,
    /// The frame's own tags, as FFmpeg reports them.
    pub source_matrix: String,
    pub source_range: String,
    pub source_primaries: String,
    pub source_transfer: String,
    /// What was applied: `rgb` (no matrix), `gray`, or the YUV matrix, with
    /// why (`tagged`, or the fallback for an unspecified matrix).
    pub applied_matrix: String,
    /// `full` or `limited`, with why (`tagged`, `yuvj format`, or `assumed`).
    pub applied_range: String,
    pub output: &'static str,
    pub not_converted: &'static str,
}

/// One decoded frame, straight-alpha RGBA8 at the frame's own size.
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
    /// The source format carries alpha (RGBA keeps it, straight).
    pub alpha: bool,
    pub conversion: Conversion,
    pub rgba: Vec<u8>,
}

/// What [`inspect_file`] read.
#[derive(Clone, Debug, Serialize)]
pub struct EncodedStream {
    pub demuxer: String,
    pub stream_index: usize,
    pub codec: String,
    /// Stream time base (rational string).
    pub time_base: String,
    pub start_pts: Option<i64>,
    /// The container's nominal rate, informational only: frame times come
    /// from timestamps.
    pub nominal_rate: Option<String>,
    pub frames_decoded: u64,
    /// Total frames, when decoding reached the end of the stream. A cleanly
    /// truncated file (whole packets missing at the end) decodes as a shorter
    /// stream; only an expected count can reveal that.
    pub frame_count: Option<u64>,
}

/// The file's bytes as observed (see the module docs).
#[derive(Clone, Debug, Serialize)]
pub struct FileIdentity {
    pub blake3: String,
    pub bytes: u64,
    /// Always `observed_recheck`: hashed before and after decoding, equal.
    pub kind: &'static str,
}

/// Decoder-side allocation cap for a frame limit of `max_px` pixels.
/// FFmpeg checks `max_pixels` against the decoder's row-aligned size,
/// `FFALIGN(w, 64) * h` (an 8x16 frame counts as 64x16 = 1024). For any
/// frame within the exact limits (`w * h <= max_px`, `h <= MAX_FRAME_HEIGHT`)
/// `FFALIGN(w, 64) * h <= (w + 63) * h <= max_px + 63 * MAX_FRAME_HEIGHT`, so
/// this cap admits every valid frame however narrow, while bounding what a
/// decoder may allocate before the exact checks to that plus about 1 Mi
/// pixels. The exact `max_px` and height limits are still enforced on every
/// declared and decoded frame.
fn decoder_pixel_cap(max_px: u64) -> i64 {
    max_px
        .saturating_add(STRIDE_ALIGN_BOUND * MAX_FRAME_HEIGHT)
        .min(i64::MAX as u64) as i64
}

/// Resource limits of one inspection ([`Limits::default`] is the published
/// [`MAX_FRAME_PIXELS`] / [`MAX_RETAINED_BYTES`]).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_frame_pixels: u64,
    pub max_retained_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_frame_pixels: MAX_FRAME_PIXELS,
            max_retained_bytes: MAX_RETAINED_BYTES,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Inspection {
    pub identity: FileIdentity,
    pub stream: EncodedStream,
    pub frames: Vec<EncodedFrame>,
    /// Bytes of RGBA the returned frames hold (one copy per requested
    /// position, duplicates included), counted against the budget.
    pub held_bytes: u64,
}

/// [`inspect_file`] without the identity or a test hook.
pub fn decode_frames(
    path: &Path,
    requested: &[i64],
    cancel: &CancelToken,
) -> anyhow::Result<(EncodedStream, Vec<EncodedFrame>)> {
    let i = inspect_file(path, requested, cancel, None)?;
    Ok((i.stream, i.frames))
}

/// blake3 and length of everything `file` holds, from its start; cancellable.
fn hash_handle(file: &mut std::fs::File, cancel: &CancelToken) -> anyhow::Result<(String, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        ensure!(!cancel.is_cancelled(), "cancelled");
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    file.seek(SeekFrom::Start(0))?;
    Ok((h.finalize().to_hex().to_string(), total))
}

/// Decode the frames at `requested` ordinals (negative: from the end, -1 is
/// the last frame) from the best video stream of `path`, in request order,
/// with the file's observed identity. Errors on an index past the end
/// (naming the frame count), a frame or retained total over the limits, a
/// read or decode error (a read error is never taken as the end of the
/// file), an unsupported conversion, a changed file or cancellation.
/// `after_first_frame` runs once after the first decoded frame (a test seam
/// for changing the file mid-decode).
pub fn inspect_file(
    path: &Path,
    requested: &[i64],
    cancel: &CancelToken,
    after_first_frame: Option<&mut dyn FnMut()>,
) -> anyhow::Result<Inspection> {
    inspect_with_limits(
        path,
        requested,
        cancel,
        after_first_frame,
        Limits::default(),
    )
}

/// [`inspect_file`] with explicit [`Limits`].
pub fn inspect_with_limits(
    path: &Path,
    requested: &[i64],
    cancel: &CancelToken,
    mut after_first_frame: Option<&mut dyn FnMut()>,
    limits: Limits,
) -> anyhow::Result<Inspection> {
    let (max_px, max_bytes) = (limits.max_frame_pixels, limits.max_retained_bytes);
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

    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "{} is not a file",
        path.display()
    );
    let (before, bytes) = hash_handle(&mut file, cancel)?;
    let mut check = file.try_clone()?;

    init();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let mut ictx = open_contained(file, name, cancel, max_px).map_err(|e| {
        if cancel.is_cancelled() {
            anyhow!("cancelled")
        } else {
            e.context(format!(
                "opening {} (only self-contained video container files are inspected; no secondary files are read)",
                path.display()
            ))
        }
    })?;
    let demuxer = ictx.format().name().to_string();
    let stream = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
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
    // The decoder refuses larger frames itself, before allocating them.
    // SAFETY: plain field write on an owned, not yet opened codec context.
    unsafe {
        (*cctx.as_mut_ptr()).max_pixels = decoder_pixel_cap(max_px);
    }
    let mut decoder = cctx.decoder().video()?;
    let codec_name = decoder
        .codec()
        .map(|c| c.name().to_string())
        .unwrap_or_default();
    let (dw, dh) = (decoder.width() as u64, decoder.height() as u64);
    ensure!(
        dw * dh <= max_px && dh <= MAX_FRAME_HEIGHT,
        "{} declares {dw}x{dh} video (max {max_px} pixels, {MAX_FRAME_HEIGHT} rows)",
        path.display()
    );

    // Positive ordinals wanted -> request positions; the last `tail` frames
    // are kept (as decoded planes) until the end is known.
    let mut wanted: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (pos, &i) in requested.iter().enumerate() {
        if i >= 0 {
            wanted.entry(i as u64).or_default().push(pos);
        }
    }
    let mut out: Vec<Option<EncodedFrame>> = vec![None; requested.len()];
    let mut ring: VecDeque<(u64, frame::Video, u64)> = VecDeque::new();
    let mut retained: u64 = 0;
    let mut scaler: Option<Scaler> = None;
    let mut index: u64 = 0;
    let mut eof_sent = false;
    let mut reached_end = false;
    let need_end = tail > 0;
    let over_budget = |what: &str| {
        format!(
            "{what} would hold more than {max_bytes} bytes of frames; request fewer or smaller frames"
        )
    };

    'decode: loop {
        ensure!(!cancel.is_cancelled(), "cancelled");
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
                let (w, h) = (f.width() as u64, f.height() as u64);
                ensure!(
                    w > 0 && h > 0 && w * h <= max_px && h <= MAX_FRAME_HEIGHT,
                    "frame {i} is {w}x{h} (max {max_px} pixels, {MAX_FRAME_HEIGHT} rows)"
                );
                if let Some(hook) = after_first_frame.take() {
                    hook();
                }
                if let Some(positions) = wanted.remove(&i) {
                    // One RGBA copy per requested position (duplicates too).
                    let add = w * h * 4 * positions.len() as u64;
                    ensure!(
                        retained + add <= max_bytes,
                        "{}",
                        over_budget("this request")
                    );
                    retained += add;
                    let e = to_encoded(&f, i, time_base, start_pts, &mut scaler)?;
                    for p in positions {
                        out[p] = Some(e.clone());
                    }
                }
                if need_end {
                    let add: u64 = (0..f.planes()).map(|p| f.data(p).len() as u64).sum();
                    while ring.len() as u64 >= tail {
                        let (_, _, b) = ring.pop_front().expect("nonempty");
                        retained -= b;
                    }
                    ensure!(
                        retained + add <= max_bytes,
                        "{}",
                        over_budget("the end window")
                    );
                    retained += add;
                    ring.push_back((i, f, add));
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
        // Read packets directly: only a true end of file ends the stream; a
        // read error after valid frames is an error, not a shorter file.
        let mut pkt = Packet::empty();
        match pkt.read(&mut ictx) {
            Ok(()) => {
                if pkt.stream() == stream_index {
                    decoder.send_packet(&pkt).with_context(|| {
                        format!(
                            "{}: corrupt or unsupported packet after frame {index}",
                            path.display()
                        )
                    })?;
                }
            }
            Err(FfError::Eof) => {
                decoder.send_eof()?;
                eof_sent = true;
            }
            Err(FfError::Exit) => bail!("cancelled"),
            Err(FfError::Other { errno }) if errno == ffmpeg_next::util::error::EAGAIN => {}
            Err(e) => bail!(
                "reading {} after frame {index}: {e} (a read error is not taken as the end of the file)",
                path.display()
            ),
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
            let (_, f, _) = ring
                .iter()
                .find(|(j, _, _)| *j == want)
                .expect("kept the last frames");
            let add = f.width() as u64 * f.height() as u64 * 4;
            ensure!(
                retained + add <= max_bytes,
                "{}",
                over_budget("this request")
            );
            retained += add;
            out[pos] = Some(to_encoded(f, want, time_base, start_pts, &mut scaler)?);
        }
    }
    drop(ring);
    drop(decoder);
    drop(ictx); // releases the decode handle before the recheck

    let (after, after_bytes) = hash_handle(&mut check, cancel)?;
    ensure!(
        before == after && bytes == after_bytes,
        "{} changed while it was decoded (blake3 {before} -> {after}); inspect it again",
        path.display()
    );
    let frames: Vec<EncodedFrame> = out
        .into_iter()
        .map(|f| f.expect("every request filled"))
        .collect();
    let held_bytes = frames.iter().map(|f| f.rgba.len() as u64).sum();
    Ok(Inspection {
        identity: FileIdentity {
            blake3: before,
            bytes,
            kind: "observed_recheck",
        },
        stream: EncodedStream {
            demuxer,
            stream_index,
            codec: codec_name,
            time_base: time_base.to_string(),
            start_pts,
            nominal_rate,
            frames_decoded: index,
            frame_count,
        },
        frames,
        held_bytes,
    })
}

type ScalerKey = (
    Pixel,
    u32,
    u32,
    color::Space,
    color::Range,
    color::Primaries,
    color::TransferCharacteristic,
);

/// The file, read only through this handle; every read and seek fails once
/// `cancel` fires (the custom I/O path does not poll the interrupt itself).
struct CancelRead {
    file: std::fs::File,
    cancel: CancelToken,
}

impl Read for CancelRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        self.file.read(buf)
    }
}

impl Seek for CancelRead {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        if self.cancel.is_cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        self.file.seek(pos)
    }
}

/// Open `file` as a demuxer input without letting it read anything else,
/// and without decoding oversized frames while probing:
/// - custom I/O over the handle; the protocol whitelist names no protocol
///   and only [`DEMUXERS`] may open, so no secondary file is opened;
/// - after the container header, every video stream's declared size is
///   checked against `max_px` BEFORE stream probing;
/// - demuxers that may add streams while reading are refused, so stream
///   probing (`avformat_find_stream_info`, which may decode frames) runs
///   with `max_pixels` (the decoder allocation cap) and one thread for every
///   stream it probes;
/// - the interrupt callback and the reader stop on `cancel`.
fn open_contained(
    file: std::fs::File,
    name: &str,
    cancel: &CancelToken,
    max_px: u64,
) -> anyhow::Result<format::context::Input> {
    use ffmpeg_next::ffi;
    use std::ffi::CString;
    let mut io = format::context::StreamIo::from_read_seek(CancelRead {
        file,
        cancel: cancel.clone(),
    })
    .map_err(|e| anyhow!("custom I/O: {e}"))?;
    let token = cancel.clone();
    let interrupt = ffmpeg_next::util::interrupt::new(Box::new(move || token.is_cancelled()));
    let mut opts = Dictionary::new();
    opts.set("protocol_whitelist", "ferrocut-none");
    opts.set("format_whitelist", DEMUXERS);
    let fname = CString::new(name).unwrap_or_default();
    let px = CString::new(decoder_pixel_cap(max_px).to_string()).expect("digits");
    // SAFETY: standard libavformat open sequence on a context we allocate and
    // own: on open failure FFmpeg frees it (and not our custom pb); after open
    // we close it on every early return, or hand it to Input, which then owns
    // it together with the StreamIo and the interrupt guard.
    unsafe {
        let mut ps = ffi::avformat_alloc_context();
        ensure!(!ps.is_null(), "out of memory");
        (*ps).interrupt_callback = interrupt.interrupt;
        (*ps).pb = io.as_mut_ptr();
        (*ps).flags |= ffi::AVFMT_FLAG_CUSTOM_IO;
        let mut raw = opts.disown();
        let r = ffi::avformat_open_input(&mut ps, fname.as_ptr(), std::ptr::null(), &mut raw);
        Dictionary::own(raw);
        if r < 0 {
            bail!("{}", FfError::from(r));
        }
        // Late stream discovery would probe new streams with default options.
        if (*ps).ctx_flags & ffi::AVFMTCTX_NOHEADER != 0 {
            ffi::avformat_close_input(&mut ps);
            bail!("this container discovers streams while reading; it is not inspected");
        }
        let n = (*ps).nb_streams as usize;
        for i in 0..n {
            let par = (*(*(*ps).streams.add(i))).codecpar;
            if (*par).codec_type == ffi::AVMediaType::AVMEDIA_TYPE_VIDEO {
                let (w, h) = ((*par).width.max(0) as u64, (*par).height.max(0) as u64);
                if w * h > max_px || h > MAX_FRAME_HEIGHT {
                    ffi::avformat_close_input(&mut ps);
                    bail!(
                        "a stream declares {w}x{h} video (max {max_px} pixels, {MAX_FRAME_HEIGHT} rows)"
                    );
                }
            }
        }
        let mut dicts: Vec<*mut ffi::AVDictionary> = vec![std::ptr::null_mut(); n];
        for d in &mut dicts {
            ffi::av_dict_set(d, c"max_pixels".as_ptr(), px.as_ptr(), 0);
            ffi::av_dict_set(d, c"threads".as_ptr(), c"1".as_ptr(), 0);
        }
        let r = ffi::avformat_find_stream_info(
            ps,
            if n == 0 {
                std::ptr::null_mut()
            } else {
                dicts.as_mut_ptr()
            },
        );
        for d in &mut dicts {
            ffi::av_dict_free(d);
        }
        if r < 0 {
            ffi::avformat_close_input(&mut ps);
            bail!("{}", FfError::from(r));
        }
        if (*ps).nb_streams as usize != n {
            ffi::avformat_close_input(&mut ps);
            bail!("streams appeared while probing; the file is not inspected");
        }
        Ok(format::context::Input::wrap_with_custom_io_and_interrupt(
            ps,
            io,
            interrupt.guard,
        ))
    }
}

struct Scaler {
    /// Everything the conversion and its reported metadata depend on.
    key: ScalerKey,
    ctx: scaling::Context,
    conversion: Conversion,
    alpha: bool,
}

/// Coefficient table of a supported YUV matrix. `Err` names a matrix that is
/// tagged but not converted correctly here (refused rather than mislabelled).
fn sws_matrix(space: color::Space) -> anyhow::Result<(i32, String)> {
    use ffmpeg_next::ffi::{
        SWS_CS_BT2020, SWS_CS_FCC, SWS_CS_ITU601, SWS_CS_ITU709, SWS_CS_SMPTE240M,
    };
    Ok(match space {
        color::Space::BT709 => (SWS_CS_ITU709, "bt709 (tagged)".into()),
        color::Space::BT470BG | color::Space::SMPTE170M => (SWS_CS_ITU601, "bt601 (tagged)".into()),
        color::Space::BT2020NCL => (SWS_CS_BT2020, "bt2020 non-constant (tagged)".into()),
        color::Space::FCC => (SWS_CS_FCC, "fcc (tagged)".into()),
        color::Space::SMPTE240M => (SWS_CS_SMPTE240M, "smpte240m (tagged)".into()),
        color::Space::Unspecified => (SWS_CS_ITU601, "bt601 (fallback: matrix unspecified)".into()),
        other => bail!(
            "YUV matrix {other:?} is not supported by frame inspection (constant-luminance, ICtCp, YCgCo and chroma-derived matrices are refused)"
        ),
    })
}

/// The conversion for a frame format, fixed per (format, size, tags).
/// libswscale coefficients and full-range flag to apply (None for RGB), the
/// reported conversion, and whether the format carries alpha.
type ConversionPlan = (Option<(i32, bool)>, Conversion, bool);

fn conversion_for(f: &frame::Video) -> anyhow::Result<ConversionPlan> {
    use ffmpeg_next::ffi::{
        AV_PIX_FMT_FLAG_ALPHA, AV_PIX_FMT_FLAG_BAYER, AV_PIX_FMT_FLAG_HWACCEL, AV_PIX_FMT_FLAG_PAL,
        AV_PIX_FMT_FLAG_RGB,
    };
    let fmt = f.format();
    let desc = fmt
        .descriptor()
        .ok_or_else(|| anyhow!("pixel format {fmt:?} has no descriptor"))?;
    // SAFETY: a descriptor pointer from av_pix_fmt_desc_get is static data.
    let flags = unsafe { (*desc.as_ptr()).flags };
    let has = |bit: i32| flags & bit as u64 != 0;
    ensure!(
        !has(AV_PIX_FMT_FLAG_HWACCEL) && !has(AV_PIX_FMT_FLAG_BAYER),
        "pixel format {} is not supported by frame inspection",
        desc.name()
    );
    let alpha = has(AV_PIX_FMT_FLAG_ALPHA);
    let name = desc.name().to_string();
    let tag = |v: &dyn std::fmt::Debug| format!("{v:?}").to_ascii_lowercase();
    let range = f.color_range();
    let mut conv = Conversion {
        source_format: name.clone(),
        source_matrix: tag(&f.color_space()),
        source_range: tag(&range),
        source_primaries: tag(&f.color_primaries()),
        source_transfer: tag(&f.color_transfer_characteristic()),
        applied_matrix: String::new(),
        applied_range: String::new(),
        output: "rgba8, straight alpha, full-range code values",
        not_converted: "transfer, primaries/gamut and tone mapping are not converted: output values are the decoded code values through the matrix only",
    };
    if has(AV_PIX_FMT_FLAG_RGB) || has(AV_PIX_FMT_FLAG_PAL) {
        conv.applied_matrix = "rgb (no matrix)".into();
        conv.applied_range = "full (rgb)".into();
        return Ok((None, conv, alpha));
    }
    // Range, independent of the matrix: deprecated yuvj formats are full
    // range by definition; otherwise the tag, else the limited default.
    let full = if name.starts_with("yuvj") {
        conv.applied_range = "full (yuvj format)".into();
        true
    } else {
        match range {
            color::Range::JPEG => {
                conv.applied_range = "full (tagged)".into();
                true
            }
            color::Range::MPEG => {
                conv.applied_range = "limited (tagged)".into();
                false
            }
            _ => {
                conv.applied_range = "limited (assumed: range unspecified)".into();
                false
            }
        }
    };
    let gray = desc.nb_components() <= 2;
    let (table, matrix) = if gray {
        (
            ffmpeg_next::ffi::SWS_CS_DEFAULT,
            "gray (luma only)".to_string(),
        )
    } else {
        sws_matrix(f.color_space())?
    };
    conv.applied_matrix = matrix;
    Ok((Some((table, full)), conv, alpha))
}

fn to_encoded(
    f: &frame::Video,
    index: u64,
    time_base: Rational,
    start_pts: Option<i64>,
    scaler: &mut Option<Scaler>,
) -> anyhow::Result<EncodedFrame> {
    let (w, h) = (f.width(), f.height());
    let key = (
        f.format(),
        w,
        h,
        f.color_space(),
        f.color_range(),
        f.color_primaries(),
        f.color_transfer_characteristic(),
    );
    if scaler.as_ref().map(|s| s.key) != Some(key) {
        let (details, conversion, alpha) = conversion_for(f)?;
        let mut ctx = scaling::Context::get(
            key.0,
            w,
            h,
            Pixel::RGBA,
            w,
            h,
            scaling::Flags::POINT | scaling::Flags::BITEXACT | scaling::Flags::ACCURATE_RND,
        )?;
        if let Some((cs, full)) = details {
            // Always applied for YUV/gray, so the reported matrix and range
            // are the ones libswscale uses (not its defaults).
            // SAFETY: the context is valid and exclusively owned here; the
            // coefficient tables are static libswscale data.
            let rc = unsafe {
                let src = ffmpeg_next::ffi::sws_getCoefficients(cs);
                let dst = ffmpeg_next::ffi::sws_getCoefficients(ffmpeg_next::ffi::SWS_CS_DEFAULT);
                ffmpeg_next::ffi::sws_setColorspaceDetails(
                    ctx.as_mut_ptr(),
                    src,
                    full as i32,
                    dst,
                    1,
                    0,
                    1 << 16,
                    1 << 16,
                )
            };
            ensure!(
                rc >= 0,
                "libswscale refused {} / {} for {} ({rc})",
                conversion.applied_matrix,
                conversion.applied_range,
                conversion.source_format
            );
        }
        *scaler = Some(Scaler {
            key,
            ctx,
            conversion,
            alpha,
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
    if !s.alpha {
        // Formats without alpha are opaque, whatever the scaler left there.
        for px in rgba.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
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
        alpha: s.alpha,
        conversion: s.conversion.clone(),
        rgba,
    })
}
