//! Stills: render chosen output frames to PNG, plus a labeled contact sheet.
//!
//! An agent's eyes. Frames go through the same graph, compositor output
//! transform and readback as a master render, so a still shows exactly the
//! pixels the master would hold at that frame (8-bit Rec.709), without
//! encoding a chunk or a video. Times are snapped to the output frame grid.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

use anyhow::{Context as _, ensure};
use ferrocut_core::{CancelToken, GpuContext, NodeError, RationalTime, RenderCtx, WorkerState};
use serde::Serialize;

use crate::compile::Compiled;
use crate::compositor::{Compositor, ReadbackRing, compositor_slot};
use crate::graph::FrameCache;
use crate::timeline::Timeline;

/// Most frames one call renders (a contact sheet beyond this is unreadable).
pub const MAX_STILLS: usize = 64;

/// Retries per frame for transient (retryable) node errors, as in a render.
const STILL_RETRIES: u32 = 2;

/// Largest contact sheet, in pixels (64 Mpx: 8192 x 8192).
pub const MAX_SHEET_PIXELS: u64 = 1 << 26;

/// One rendered output frame, straight RGBA8 (alpha 255).
pub struct Still {
    pub frame: i64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Output frames for `count` evenly spaced samples of the whole timeline:
/// the midpoints of `count` equal spans (so neither the black first frame of
/// a fade-in nor the last frame dominates).
pub fn spread(tl: &Timeline, count: usize) -> Vec<i64> {
    let n = tl.frame_count().max(1);
    let count = count.clamp(1, MAX_STILLS) as i64;
    let mut v: Vec<i64> = (0..count)
        .map(|i| ((2 * i + 1) * n / (2 * count)).min(n - 1))
        .collect();
    v.dedup();
    v
}

/// Parse an exact timeline time: an integer, `"n/d"` or an exact decimal.
pub fn parse_time(s: &str) -> anyhow::Result<RationalTime> {
    let r: ferrocut_core::Rational = s
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("bad time {s:?}: {e}"))?;
    Ok(RationalTime(r))
}

/// The frames to render: every `at` time's frame, every explicit frame
/// index, and `spread` evenly spaced frames; sorted, deduplicated. With
/// nothing chosen, 12 spread frames.
pub fn select_frames(
    tl: &Timeline,
    at: &[RationalTime],
    frames: &[i64],
    spread_count: Option<usize>,
) -> anyhow::Result<Vec<i64>> {
    let n = tl.frame_count();
    ensure!(n > 0, "the timeline has no frames");
    let mut v = Vec::new();
    for &t in at {
        v.push(frame_at(tl, t)?);
    }
    for &f in frames {
        ensure!(
            (0..n).contains(&f),
            "frame {f} is outside the timeline (0..{n})"
        );
        v.push(f);
    }
    let spread_count = match spread_count {
        Some(c) => {
            ensure!(
                (1..=MAX_STILLS).contains(&c),
                "spread must be 1..={MAX_STILLS}"
            );
            Some(c)
        }
        None if v.is_empty() => Some(12),
        None => None,
    };
    if let Some(c) = spread_count {
        v.extend(spread(tl, c));
    }
    v.sort_unstable();
    v.dedup();
    ensure!(
        v.len() <= MAX_STILLS,
        "{} frames requested; at most {MAX_STILLS} per call",
        v.len()
    );
    Ok(v)
}

/// The output frame showing timeline time `t` (frame whose span contains it).
pub fn frame_at(tl: &Timeline, t: RationalTime) -> anyhow::Result<i64> {
    let n = tl.frame_count();
    let f = t.frame_floor(tl.output.fps);
    ensure!(
        (0..n).contains(&f),
        "time {t} is outside the timeline (0 to {})",
        RationalTime::from_frames(n, tl.output.fps)
    );
    Ok(f)
}

/// Render `frames` (output frame indices) on `gpu`: the same graph,
/// compositor output pass and readback as a master render, with the same
/// per-frame retries, GPU error scopes and cancellation.
pub fn render_stills(
    tl: &Timeline,
    c: &Compiled,
    gpu: &GpuContext,
    frames: &[i64],
    cancel: &CancelToken,
) -> anyhow::Result<Vec<Still>> {
    ensure!(!frames.is_empty(), "no frames to render");
    ensure!(
        frames.len() <= MAX_STILLS,
        "at most {MAX_STILLS} frames per call"
    );
    let (w, h) = (tl.output.width, tl.output.height);
    let (comp, mut state, mut ring) =
        ferrocut_core::with_alloc_scope(gpu, || -> anyhow::Result<_> {
            let comp = std::sync::Arc::new(Compositor::new(gpu));
            let mut state = WorkerState::default();
            state.slot(compositor_slot(), || Ok(comp.clone()))?;
            Ok((comp, state, ReadbackRing::new(gpu, w, h, 2)))
        })
        .map_err(anyhow::Error::new)
        .context("setting up the output pass")??;
    let mut cache = FrameCache::new(16);
    let retries = AtomicU64::new(0);
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(frames.len());
    let mut sink = |rows: &[u8], stride: usize| -> anyhow::Result<()> {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            for px in rows[y * stride..y * stride + w as usize * 4]
                .as_chunks::<4>()
                .0
            {
                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
        }
        out.push(rgba);
        Ok(())
    };
    for &i in frames {
        crate::render::check_cancel(cancel, None).map_err(anyhow::Error::new)?;
        let t = RationalTime::from_frames(i, tl.output.fps);
        let frame = crate::render::with_retries(STILL_RETRIES, cancel, None, &retries, |_| {
            let r = gpu.scoped(|| {
                let mut ctx = RenderCtx::new(gpu, &mut state, cancel, None);
                c.graph.evaluate(c.output, t, &mut ctx, &mut cache)
            });
            if r.as_ref().is_err_and(NodeError::is_gpu_out_of_memory) {
                cache.clear();
            }
            r
        })
        .map_err(anyhow::Error::new)
        .with_context(|| format!("frame {i}"))?;
        let scope = gpu.error_scope();
        let pushed = {
            let mut ctx = RenderCtx::new(gpu, &mut state, cancel, None);
            ring.push(&comp, &mut ctx, &frame, &mut sink)
        };
        if let Some(e) = scope.finish() {
            return Err(anyhow::Error::new(e).context(format!("frame {i}: output")));
        }
        pushed.with_context(|| format!("frame {i}: output"))?;
        cache.clear();
    }
    let scope = gpu.error_scope();
    let drained = ring.drain(gpu, &mut sink);
    if let Some(e) = scope.finish() {
        return Err(anyhow::Error::new(e).context("output"));
    }
    drained?;
    // Never report pixels that may come from a dying device.
    gpu.check_lost().map_err(anyhow::Error::new)?;
    Ok(frames
        .iter()
        .zip(out)
        .map(|(&frame, rgba)| Still {
            frame,
            width: w,
            height: h,
            rgba,
        })
        .collect())
}

/// Encode straight RGBA8 as PNG bytes.
/// blake3 of bytes as lowercase hex (content identity of an inline image).
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub fn png_bytes(width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<Vec<u8>> {
    let size = tiny_skia::IntSize::from_wh(width, height).context("empty image")?;
    // Opaque pixels: straight == premultiplied, so no conversion is needed.
    let pm = tiny_skia::Pixmap::from_vec(rgba.to_vec(), size).context("pixel buffer size")?;
    pm.encode_png().context("encoding PNG")
}

/// Encode as PNG, halving the image (area average) while the file exceeds
/// `max_bytes` and its longer side is above 256 px. Returns the final size
/// and bytes; the result may still exceed the budget at the floor.
pub fn png_within(
    width: u32,
    height: u32,
    rgba: &[u8],
    max_bytes: usize,
) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    let (mut w, mut h, mut img) = (width, height, rgba.to_vec());
    let mut png = png_bytes(w, h, &img)?;
    while png.len() > max_bytes && w.max(h) > 256 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        img = downscale(&img, w, h, nw, nh);
        (w, h) = (nw, nh);
        png = png_bytes(w, h, &img)?;
    }
    Ok((w, h, png))
}

/// Write straight RGBA8 as a PNG.
pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let bytes = png_bytes(width, height, rgba)?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// Shrink an opaque RGBA8 image so its longer side is at most `max_side`
/// (unchanged when it already fits).
pub fn fit_within(rgba: &[u8], w: u32, h: u32, max_side: u32) -> (u32, u32, Vec<u8>) {
    let max_side = max_side.max(16);
    if w <= max_side && h <= max_side {
        return (w, h, rgba.to_vec());
    }
    let (nw, nh) = if w >= h {
        (
            max_side,
            ((h as u64 * max_side as u64) / w as u64).max(1) as u32,
        )
    } else {
        (
            ((w as u64 * max_side as u64) / h as u64).max(1) as u32,
            max_side,
        )
    };
    (nw, nh, downscale(rgba, w, h, nw, nh))
}

/// Area-average downscale of opaque RGBA8 to `nw` x `nh`.
pub fn downscale(src: &[u8], w: u32, h: u32, nw: u32, nh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        let (y0, y1) = (y * h / nh, ((y + 1) * h / nh).max(y * h / nh + 1));
        for x in 0..nw {
            let (x0, x1) = (x * w / nw, ((x + 1) * w / nw).max(x * w / nw + 1));
            let mut acc = [0u32; 3];
            for sy in y0..y1.min(h) {
                for sx in x0..x1.min(w) {
                    let p = ((sy * w + sx) * 4) as usize;
                    for k in 0..3 {
                        acc[k] += src[p + k] as u32;
                    }
                }
            }
            let n = (y1.min(h) - y0) * (x1.min(w) - x0);
            let o = ((y * nw + x) * 4) as usize;
            for k in 0..3 {
                out[o + k] = ((acc[k] + n / 2) / n) as u8;
            }
            out[o + 3] = 255;
        }
    }
    out
}

/// 3x5 bitmap glyphs for labels: digits, `:`, `.`, `f`, `s`, `#`, space.
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        'f' => [0b011, 0b100, 0b110, 0b100, 0b100],
        's' => [0b011, 0b100, 0b010, 0b001, 0b110],
        '#' => [0b101, 0b111, 0b101, 0b111, 0b101],
        _ => [0; 5],
    }
}

/// Draw `text` white on a dark box at (x, y), `scale` pixels per glyph dot.
fn label(img: &mut [u8], w: u32, h: u32, x: u32, y: u32, text: &str, scale: u32) {
    let (bw, bh) = ((text.chars().count() as u32 * 4 + 1) * scale, 7 * scale);
    for yy in y..(y + bh).min(h) {
        for xx in x..(x + bw).min(w) {
            let p = ((yy * w + xx) * 4) as usize;
            for k in 0..3 {
                img[p + k] = (img[p + k] as u32 * 3 / 10) as u8;
            }
        }
    }
    for (ci, c) in text.chars().enumerate() {
        let g = glyph(c);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..3 {
                if bits >> (2 - col) & 1 == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let xx = x + (1 + ci as u32 * 4 + col) * scale + dx;
                        let yy = y + (1 + row as u32) * scale + dy;
                        if xx < w && yy < h {
                            let p = ((yy * w + xx) * 4) as usize;
                            img[p..p + 3].copy_from_slice(&[255, 255, 255]);
                        }
                    }
                }
            }
        }
    }
}

/// `m:ss.ss` label of an output frame, plus its frame number (hundredths
/// rounded once, so 59.996 s reads 1:00.00, never 0:60.00).
pub fn timecode(tl: &Timeline, frame: i64) -> String {
    let t = RationalTime::from_frames(frame, tl.output.fps)
        .seconds()
        .to_f64();
    let cs = (t * 100.0).round() as i64;
    format!(
        "{}:{:02}.{:02} f{}",
        cs / 6000,
        (cs % 6000) / 100,
        cs % 100,
        frame
    )
}

/// Lay out stills in a grid of `cols` columns, each scaled to `cell_w` wide
/// and labeled with its time and frame number. Bounded by [`MAX_SHEET_PIXELS`].
pub fn contact_sheet(
    tl: &Timeline,
    stills: &[Still],
    cols: u32,
    cell_w: u32,
) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    ensure!(!stills.is_empty(), "no stills");
    let (sw, sh) = (stills[0].width, stills[0].height);
    let cell_w = cell_w.min(sw).max(16);
    let cell_h = ((sh as u64 * cell_w as u64) / sw as u64).max(1) as u32;
    let cols = cols.clamp(1, stills.len() as u32);
    let rows = (stills.len() as u32).div_ceil(cols);
    let gap = 4;
    let (w, h) = (
        cols as u64 * cell_w as u64 + (cols as u64 + 1) * gap as u64,
        rows as u64 * cell_h as u64 + (rows as u64 + 1) * gap as u64,
    );
    ensure!(
        w * h <= MAX_SHEET_PIXELS,
        "a {w}x{h} contact sheet is too large (max {MAX_SHEET_PIXELS} pixels): fewer columns, a smaller cell_width or fewer frames"
    );
    let (w, h) = (w as u32, h as u32);
    let mut img = vec![0u8; (w as usize) * (h as usize) * 4];
    for px in img.as_chunks_mut::<4>().0 {
        *px = [24, 24, 28, 255];
    }
    let scale = (cell_w / 160).clamp(1, 4);
    for (i, s) in stills.iter().enumerate() {
        let (cx, cy) = (i as u32 % cols, i as u32 / cols);
        let (ox, oy) = (gap + cx * (cell_w + gap), gap + cy * (cell_h + gap));
        let small = downscale(&s.rgba, s.width, s.height, cell_w, cell_h);
        for y in 0..cell_h {
            let src = &small[(y * cell_w * 4) as usize..((y + 1) * cell_w * 4) as usize];
            let d = (((oy + y) * w + ox) * 4) as usize;
            img[d..d + src.len()].copy_from_slice(src);
        }
        label(&mut img, w, h, ox, oy, &timecode(tl, s.frame), scale);
    }
    Ok((w, h, img))
}

/// What [`write_stills`] wrote.
#[derive(Serialize)]
pub struct StillsReport {
    pub frames: Vec<StillEntry>,
    pub sheet: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize)]
pub struct StillEntry {
    pub frame: i64,
    /// Exact timeline time of the frame (rational string).
    pub time: String,
    pub timecode: String,
    pub path: Option<PathBuf>,
}

/// A file-name prefix: ASCII letters, digits, `.`, `_`, `-`, not starting
/// with `.`, at most 64 characters. Anything else (a path separator, `..`, an
/// absolute path) could place files outside the stills directory.
pub fn check_prefix(prefix: &str) -> anyhow::Result<()> {
    ensure!(
        !prefix.is_empty()
            && prefix.len() <= 64
            && !prefix.starts_with('.')
            && prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-'),
        "prefix {prefix:?}: use letters, digits, '.', '_' or '-' (not starting with '.'), at most 64 characters"
    );
    Ok(())
}

/// Write `stills` as `<dir>/<prefix>-f<frame>.png` (when `each`) and a
/// contact sheet `<dir>/<prefix>-sheet.png` (when `sheet`; `cols` columns of
/// `cell_w`-pixel cells).
pub fn write_stills(
    tl: &Timeline,
    stills: &[Still],
    dir: &Path,
    prefix: &str,
    each: bool,
    sheet: Option<(u32, u32)>,
) -> anyhow::Result<StillsReport> {
    check_prefix(prefix)?;
    let mut frames = Vec::new();
    for s in stills {
        let path = if each {
            let p = dir.join(format!("{prefix}-f{:05}.png", s.frame));
            write_png(&p, s.width, s.height, &s.rgba)?;
            Some(p)
        } else {
            None
        };
        frames.push(StillEntry {
            frame: s.frame,
            time: RationalTime::from_frames(s.frame, tl.output.fps)
                .seconds()
                .to_string(),
            timecode: timecode(tl, s.frame),
            path,
        });
    }
    let sheet = match sheet {
        Some((cols, cell_w)) => {
            let (w, h, img) = contact_sheet(tl, stills, cols, cell_w)?;
            let p = dir.join(format!("{prefix}-sheet.png"));
            write_png(&p, w, h, &img)?;
            Some(p)
        }
        None => None,
    };
    Ok(StillsReport {
        frames,
        sheet,
        width: tl.output.width,
        height: tl.output.height,
    })
}
