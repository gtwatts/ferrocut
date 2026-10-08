//! Proxy media: half-resolution, intra-frame copies of video sources for
//! fast draft renders (`render --proxies`). Final renders always read the
//! original media: proxies are only swapped in on request and never for a
//! delivery, and their frame keys differ from the originals', so draft and
//! final chunks never mix in the cache.
//!
//! Codec: DNxHR LB (`dnxhd`, profile `dnxhr_lb`, 8-bit 4:2:2) when the
//! proxy is at least 256x120 (the encoder's minimum), else FFV1 4:2:2;
//! sources with alpha get FFV1 BGRA. All three are native libavcodec
//! encoders, so the LGPL build has them (no ProRes, no GPL code). Video
//! only (audio always comes from the original), Matroska, bit-exact muxing,
//! timestamps kept (relative to the source's first frame), so frame lookup
//! by time matches the original.
//!
//! Proxies live next to the media in `.ferrocut-proxies/`, named by the
//! source's content hash: an edited or replaced source never picks up a
//! stale proxy.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};
use ffmpeg_next::{
    Dictionary, Error as FfError, Packet, codec, encoder, format, frame, media, software::scaling,
    util::format::Pixel,
};
use serde::Serialize;

use super::init;

/// Bump when the proxy encoding changes (part of proxy frame keys).
pub const PROXY_VERSION: &str = "proxy.v2:half:dnxhr_lb|ffv1:source-timing";
pub const PROXY_DIR: &str = ".ferrocut-proxies";
/// DNxHR's smallest frame.
pub const DNXHR_MIN: (u32, u32) = (256, 120);

#[derive(Clone, Debug, Serialize)]
pub struct ProxyInfo {
    pub source: PathBuf,
    pub proxy: PathBuf,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    /// Frames encoded (only known when the proxy was made just now).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    /// False when an up-to-date proxy already existed.
    pub created: bool,
}

/// blake3 of a file's bytes (the same content hash source nodes use).
pub fn file_hash(path: &Path) -> anyhow::Result<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    h.update_reader(f)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(*h.finalize().as_bytes())
}

/// Where the proxy of `source` (content hash `hash`) lives.
pub fn proxy_path(source: &Path, hash: &[u8; 32]) -> PathBuf {
    let dir = source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(PROXY_DIR);
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media".into());
    // A codec/timestamp fix must also invalidate the on-disk proxy, not just
    // the draft-render frame key derived from it.
    let hex = blake3::Hash::from(proxied_hash(hash)).to_hex();
    dir.join(format!("{stem}.{}.mkv", &hex[..16]))
}

/// The proxy of `source`, if one was generated for its current content.
pub fn find(source: &Path, hash: &[u8; 32]) -> Option<PathBuf> {
    let p = proxy_path(source, hash);
    p.is_file().then_some(p)
}

/// Content hash a source node reads through a proxy (distinct from the
/// original's, so draft frames never collide with final ones).
pub fn proxied_hash(hash: &[u8; 32]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(PROXY_VERSION.as_bytes());
    h.update(hash);
    *h.finalize().as_bytes()
}

/// Half the size, width even (4:2:2), at least 2x1.
pub fn proxy_size(w: u32, h: u32) -> (u32, u32) {
    let pw = w.div_ceil(2).div_ceil(2) * 2;
    (pw.max(2), h.div_ceil(2).max(1))
}

fn has_alpha(p: Pixel) -> bool {
    // SAFETY: av_pix_fmt_desc_get returns a pointer to a static descriptor
    // (or null for an invalid format).
    unsafe {
        let d = ffmpeg_next::ffi::av_pix_fmt_desc_get(p.into());
        // AV_PIX_FMT_FLAG_ALPHA = 1 << 7.
        !d.is_null() && ((*d).flags & (1 << 7)) != 0
    }
}

/// Make the proxy of `source` unless an up-to-date one exists (`force`
/// re-makes it). Written to a temporary file and renamed into place.
pub fn generate(source: &Path, force: bool) -> anyhow::Result<ProxyInfo> {
    let hash = file_hash(source)?;
    let out = proxy_path(source, &hash);
    if !force && out.is_file() {
        let i = super::probe(&out)?;
        let v = i
            .streams
            .iter()
            .find(|s| s.kind == "video")
            .ok_or_else(|| anyhow!("{}: proxy without video", out.display()))?;
        return Ok(ProxyInfo {
            source: source.to_path_buf(),
            proxy: out,
            // Proxies are only ever DNxHR LB or FFV1.
            codec: match v.codec.as_str() {
                "dnxhd" => "dnxhr_lb".to_string(),
                c => c.to_string(),
            },
            width: v.width.unwrap_or(0),
            height: v.height.unwrap_or(0),
            frames: None,
            created: false,
        });
    }
    let dir = out.parent().expect("proxy dir");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = out.with_extension(format!("tmp{}.mkv", std::process::id()));
    let r = encode(source, &tmp);
    match r {
        Ok((codec, width, height, frames)) => {
            std::fs::rename(&tmp, &out)
                .with_context(|| format!("moving the proxy to {}", out.display()))?;
            Ok(ProxyInfo {
                source: source.to_path_buf(),
                proxy: out,
                codec,
                width,
                height,
                frames: Some(frames),
                created: true,
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.context(format!("making the proxy of {}", source.display())))
        }
    }
}

fn encode(source: &Path, out: &Path) -> anyhow::Result<(String, u32, u32, u64)> {
    init();
    let mut ictx =
        format::input(source).with_context(|| format!("opening {}", source.display()))?;
    let stream = ictx
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| anyhow!("no video stream"))?;
    let si = stream.index();
    let in_tb = stream.time_base();
    let start = if stream.start_time() == ffmpeg_next::ffi::AV_NOPTS_VALUE {
        0
    } else {
        stream.start_time()
    };
    let rate = stream.avg_frame_rate();
    let rate = if rate.numerator() > 0 {
        rate
    } else {
        stream.rate()
    };
    let mut dec = codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .video()?;
    let (sw, sh) = (dec.width(), dec.height());
    if sw == 0 || sh == 0 {
        bail!("cannot determine the video size");
    }
    let (pw, ph) = proxy_size(sw, sh);
    let (name, pix, profile) = if has_alpha(dec.format()) {
        ("ffv1", Pixel::BGRA, None)
    } else if pw >= DNXHR_MIN.0 && ph >= DNXHR_MIN.1 {
        ("dnxhd", Pixel::YUV422P, Some("dnxhr_lb"))
    } else {
        ("ffv1", Pixel::YUV422P, None)
    };
    let mut octx = format::output_as(out, "matroska")
        .with_context(|| format!("creating {}", out.display()))?;
    super::encode::set_bitexact(&mut octx);
    let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);
    let codec = encoder::find_by_name(name).ok_or_else(|| anyhow!("{name} encoder missing"))?;
    let mut enc = codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()?;
    enc.set_width(pw);
    enc.set_height(ph);
    enc.set_format(pix);
    enc.set_time_base(in_tb);
    if rate.numerator() > 0 {
        enc.set_frame_rate(Some(rate));
    }
    let mut flags = codec::Flags::BITEXACT;
    if global_header {
        flags |= codec::Flags::GLOBAL_HEADER;
    }
    enc.set_flags(flags);
    let mut opts = Dictionary::new();
    if let Some(p) = profile {
        opts.set("profile", p);
    } else {
        opts.set("level", "3");
        opts.set("slices", "4");
    }
    let mut enc = enc
        .open_with(opts)
        .with_context(|| format!("opening the {name} encoder"))?;
    let mut ost = octx.add_stream(codec)?;
    ost.set_parameters(&enc);
    ost.set_time_base(in_tb);
    if rate.numerator() > 0 {
        // Declared as the Matroska DefaultDuration (see encode.rs).
        ost.set_avg_frame_rate(rate);
        ost.set_rate(rate);
    }
    octx.write_header()?;
    let ost_tb = octx.stream(0).expect("stream").time_base();
    let mut pipe = Pipe {
        scaler: None,
        frames: 0,
        pix,
        size: (pw, ph),
        start,
        tbs: (in_tb, ost_tb),
        durations: BTreeMap::new(),
        pending: None,
        source_end: super::media_duration(source)?.map(|end| end.to_pts(super::to_core(in_tb))),
    };
    for (s, p) in ictx.packets() {
        if s.index() == si {
            // Some decoders omit AVFrame.duration even when the demuxed
            // packet carries it. These intra proxy encoders retain frame PTS.
            if let Some(pts) = p.pts()
                && p.duration() > 0
            {
                pipe.durations.insert(pts - start, p.duration());
            }
            dec.send_packet(&p)?;
            pipe.receive(&mut dec, &mut enc, &mut octx)?;
        }
    }
    dec.send_eof()?;
    pipe.receive(&mut dec, &mut enc, &mut octx)?;
    enc.send_eof()?;
    pipe.drain(&mut enc, &mut octx)?;
    pipe.flush(&mut octx)?;
    octx.write_trailer()?;
    let codec = profile.unwrap_or(name).to_string();
    Ok((codec, pw, ph, pipe.frames))
}

/// Decoder -> scaler -> encoder -> muxer state of one proxy encode.
struct Pipe {
    scaler: Option<(Pixel, u32, u32, scaling::Context)>,
    frames: u64,
    pix: Pixel,
    size: (u32, u32),
    start: i64,
    /// Encoder (= input stream) and output stream time bases.
    tbs: (ffmpeg_next::Rational, ffmpeg_next::Rational),
    /// Decoded frame durations, keyed by encoder PTS in the input time base.
    durations: BTreeMap<i64, i64>,
    /// Hold one packet so a missing duration can follow the next real PTS.
    pending: Option<Packet>,
    /// Known source end, relative to video start, in the input time base.
    source_end: Option<i64>,
}

impl Pipe {
    fn drain(
        &mut self,
        enc: &mut encoder::Video,
        octx: &mut format::context::Output,
    ) -> anyhow::Result<()> {
        let mut pkt = Packet::empty();
        while enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(0);
            if let Some(pts) = pkt.pts()
                && let Some(duration) = self.durations.remove(&pts)
                && pkt.duration() <= 0
            {
                pkt.set_duration(duration);
            }
            if let Some(mut previous) = self.pending.take() {
                if previous.duration() <= 0
                    && let (Some(next), Some(start)) = (pkt.pts(), previous.pts())
                    && let Some(duration) = next.checked_sub(start).filter(|duration| *duration > 0)
                {
                    previous.set_duration(duration);
                }
                previous.rescale_ts(self.tbs.0, self.tbs.1);
                previous.write_interleaved(octx)?;
            }
            self.pending = Some(pkt);
            pkt = Packet::empty();
        }
        Ok(())
    }

    fn flush(&mut self, octx: &mut format::context::Output) -> anyhow::Result<()> {
        if let Some(mut packet) = self.pending.take() {
            // Matroska often supplies only a container end, without per-frame
            // durations, or (with a declared default duration) a per-frame
            // duration truncated to the millisecond time base. The proxy must
            // end exactly where the source ends, so the final frame extends to
            // the known source end; nothing is inferred from an average frame
            // rate (VFR input). A demuxed duration is only ever lengthened.
            if let (Some(end), Some(start)) = (self.source_end, packet.pts())
                && let Some(duration) = end.checked_sub(start).filter(|duration| *duration > 0)
                && packet.duration() < duration
            {
                packet.set_duration(duration);
            }
            packet.rescale_ts(self.tbs.0, self.tbs.1);
            packet.write_interleaved(octx)?;
        }
        Ok(())
    }

    fn push(
        &mut self,
        f: &frame::Video,
        enc: &mut encoder::Video,
        octx: &mut format::context::Output,
    ) -> anyhow::Result<()> {
        let pts = f
            .timestamp()
            .or(f.pts())
            .ok_or_else(|| anyhow!("decoded frame without timestamp"))?;
        let key = (f.format(), f.width(), f.height());
        if self.scaler.as_ref().map(|s| (s.0, s.1, s.2)) != Some(key) {
            let ctx = scaling::Context::get(
                key.0,
                key.1,
                key.2,
                self.pix,
                self.size.0,
                self.size.1,
                scaling::Flags::AREA | scaling::Flags::BITEXACT | scaling::Flags::ACCURATE_RND,
            )?;
            self.scaler = Some((key.0, key.1, key.2, ctx));
        }
        let mut o = frame::Video::empty();
        self.scaler.as_mut().expect("scaler").3.run(f, &mut o)?;
        o.set_pts(Some(pts - self.start));
        if f.packet().duration > 0 {
            self.durations.insert(pts - self.start, f.packet().duration);
        }
        enc.send_frame(&o)?;
        self.frames += 1;
        self.drain(enc, octx)
    }

    fn receive(
        &mut self,
        dec: &mut codec::decoder::Video,
        enc: &mut encoder::Video,
        octx: &mut format::context::Output,
    ) -> anyhow::Result<()> {
        loop {
            let mut f = frame::Video::empty();
            match dec.receive_frame(&mut f) {
                Ok(()) => self.push(&f, enc, octx)?,
                Err(FfError::Eof) => return Ok(()),
                Err(FfError::Other { errno }) if errno == ffmpeg_next::util::error::EAGAIN => {
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Video media files a timeline reads (nested comps followed), in first-use
/// order. Sources must be resolved (as [`crate::Timeline::load`] does).
pub fn video_sources(tl: &crate::Timeline) -> anyhow::Result<Vec<PathBuf>> {
    fn walk(
        tl: &crate::Timeline,
        out: &mut Vec<PathBuf>,
        seen_comps: &mut Vec<PathBuf>,
    ) -> anyhow::Result<()> {
        for c in tl.tracks.iter().flat_map(|t| &t.clips) {
            if c.is_generator() {
                continue;
            }
            if crate::comp::is_comp(&c.source) {
                let key = c.source.canonicalize().unwrap_or_else(|_| c.source.clone());
                if !seen_comps.contains(&key) {
                    seen_comps.push(key);
                    let inner = crate::Timeline::load(&c.source)
                        .with_context(|| format!("clip {}", c.id))?;
                    walk(&inner, out, seen_comps)?;
                }
            } else if !out.contains(&c.source) {
                out.push(c.source.clone());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(tl, &mut out, &mut Vec::new())?;
    Ok(out)
}

/// One media file of a timeline: online or offline, and its proxy.
#[derive(Clone, Debug, Serialize)]
pub struct SourceStatus {
    pub path: PathBuf,
    /// `video` (a video-track clip) or `audio` (audio-track clips only).
    pub kind: &'static str,
    pub online: bool,
    /// The proxy for the file's current content (video, online files).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<PathBuf>,
    pub clips: Vec<String>,
}

/// Every media file (and nested comp) the timeline's clips use, with
/// online / proxy status (`proxies`: also hash online video files to look
/// their proxies up). Sources must be resolved.
pub fn media_status(tl: &crate::Timeline, proxies: bool) -> Vec<SourceStatus> {
    let mut out: Vec<SourceStatus> = Vec::new();
    let mut add =
        |path: &Path, kind: &'static str, id: &str| match out.iter_mut().find(|s| s.path == path) {
            Some(s) => {
                s.clips.push(id.to_string());
                if kind == "video" {
                    s.kind = "video";
                }
            }
            None => out.push(SourceStatus {
                path: path.to_path_buf(),
                kind,
                online: path.is_file(),
                proxy: None,
                clips: vec![id.to_string()],
            }),
        };
    for c in tl.tracks.iter().flat_map(|t| &t.clips) {
        if !c.is_generator() {
            add(&c.source, "video", &c.id);
        }
    }
    for c in tl.audio_tracks.iter().flat_map(|t| &t.clips) {
        add(&c.source, "audio", &c.id);
    }
    if proxies {
        for s in &mut out {
            if s.online && s.kind == "video" && !crate::comp::is_comp(&s.path) {
                s.proxy = file_hash(&s.path).ok().and_then(|h| find(&s.path, &h));
            }
        }
    }
    out
}
