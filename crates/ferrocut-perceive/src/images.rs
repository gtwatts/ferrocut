//! PNG output: scope images, contact sheets, a 5x7 bitmap font for timecodes.
//! Integer-only pixel math, so images are byte-identical across runs.

use std::path::Path;

use anyhow::Context as _;

use crate::scopes::{OFF_HIST, OFF_PARADE, OFF_VEC, OFF_WAVE, SKIN_LINE_DEG, WAVE_COLS, WAVE_LEN};

/// An 8-bit RGB image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Rgb {
    pub fn new(w: usize, h: usize, fill: [u8; 3]) -> Self {
        Rgb {
            w,
            h,
            px: fill.repeat(w * h),
        }
    }
    #[inline]
    pub fn put(&mut self, x: usize, y: usize, c: [u8; 3]) {
        if x < self.w && y < self.h {
            let i = (y * self.w + x) * 3;
            self.px[i..i + 3].copy_from_slice(&c);
        }
    }
    pub fn blit(&mut self, src: &Rgb, x0: usize, y0: usize) {
        for y in 0..src.h.min(self.h.saturating_sub(y0)) {
            let n = src.w.min(self.w.saturating_sub(x0)) * 3;
            let d = ((y0 + y) * self.w + x0) * 3;
            self.px[d..d + n].copy_from_slice(&src.px[y * src.w * 3..y * src.w * 3 + n]);
        }
    }
    pub fn write_png(&self, path: &Path) -> anyhow::Result<()> {
        let f =
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), self.w as u32, self.h as u32);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut w = e.write_header()?;
        w.write_image_data(&self.px)?;
        w.finish()?;
        Ok(())
    }
    pub fn read_png(path: &Path) -> anyhow::Result<Self> {
        let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut r = png::Decoder::new(std::io::BufReader::new(f)).read_info()?;
        let mut buf = vec![0u8; r.output_buffer_size().context("png size")?];
        let info = r.next_frame(&mut buf)?;
        anyhow::ensure!(
            info.color_type == png::ColorType::Rgb && info.bit_depth == png::BitDepth::Eight,
            "expected 8-bit RGB png"
        );
        buf.truncate(info.buffer_size());
        Ok(Rgb {
            w: info.width as usize,
            h: info.height as usize,
            px: buf,
        })
    }
}

/// Box-filter downscale of a BGRZ frame to `tw x th` RGB (integer averages).
pub fn downscale_bgrz(px: &[u8], w: usize, h: usize, tw: usize, th: usize) -> Rgb {
    let mut out = Rgb::new(tw, th, [0; 3]);
    for ty in 0..th {
        let (y0, y1) = (ty * h / th, ((ty + 1) * h / th).max(ty * h / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w / tw).max(tx * w / tw + 1));
            let mut s = [0u64; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    s[0] += px[i + 2] as u64;
                    s[1] += px[i + 1] as u64;
                    s[2] += px[i] as u64;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            out.put(
                tx,
                ty,
                [
                    ((s[0] + n / 2) / n) as u8,
                    ((s[1] + n / 2) / n) as u8,
                    ((s[2] + n / 2) / n) as u8,
                ],
            );
        }
    }
    out
}

/// Thumbnail size for a frame: `width` wide, even height, aspect preserved.
pub fn thumb_size(w: u32, h: u32, width: usize) -> (usize, usize) {
    let th = ((width as u64 * h as u64 + w as u64 / 2) / w as u64).max(2) as usize;
    (width, th + th % 2)
}

// ---- font -------------------------------------------------------------------

const GLYPHS: &[(char, [u8; 7])] = &[
    ('0', [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e]),
    ('1', [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e]),
    ('2', [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f]),
    ('3', [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e]),
    ('4', [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02]),
    ('5', [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e]),
    ('6', [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e]),
    ('7', [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08]),
    ('8', [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e]),
    ('9', [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c]),
    (':', [0x00, 0x0c, 0x0c, 0x00, 0x0c, 0x0c, 0x00]),
    ('#', [0x0a, 0x0a, 0x1f, 0x0a, 0x1f, 0x0a, 0x0a]),
    ('-', [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00]),
    ('.', [0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c]),
    (' ', [0; 7]),
];

/// Draw `text` (digits, `:#-.` and space) with its top-left at `(x, y)`.
pub fn text(img: &mut Rgb, x: usize, y: usize, s: &str, scale: usize, c: [u8; 3]) {
    for (k, ch) in s.chars().enumerate() {
        let Some((_, rows)) = GLYPHS.iter().find(|(g, _)| *g == ch) else {
            continue;
        };
        for (ry, row) in rows.iter().enumerate() {
            for rx in 0..5 {
                if row & (0x10 >> rx) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            img.put(x + (k * 6 + rx) * scale + dx, y + ry * scale + dy, c);
                        }
                    }
                }
            }
        }
    }
}

pub fn text_width(s: &str, scale: usize) -> usize {
    s.chars().count() * 6 * scale
}

// ---- contact sheets ---------------------------------------------------------

/// Grid of thumbnails, each labelled underneath (timecode + frame number).
pub fn contact_sheet(cells: &[(&Rgb, String)], cols: usize) -> Rgb {
    let (cw, ch) = cells.first().map_or((160, 90), |(t, _)| (t.w, t.h));
    let (pad, label) = (4, 18);
    let cols = cols.clamp(1, cells.len().max(1));
    let rows = cells.len().div_ceil(cols).max(1);
    let mut img = Rgb::new(
        pad + cols * (cw + pad),
        pad + rows * (ch + label + pad),
        [24, 24, 24],
    );
    for (i, (t, l)) in cells.iter().enumerate() {
        let (x, y) = (
            pad + (i % cols) * (cw + pad),
            pad + (i / cols) * (ch + label + pad),
        );
        img.blit(t, x, y);
        text(&mut img, x + 1, y + ch + 3, l, 2, [230, 230, 230]);
    }
    img
}

// ---- scopes -----------------------------------------------------------------

/// sqrt(c / max) * 255, integer only.
fn intensity(c: u32, max: u32) -> u8 {
    if c == 0 || max == 0 {
        return 0;
    }
    let v = (c as u64 * 65025 / max as u64).isqrt();
    v.clamp(48, 255) as u8 // anything present is visible
}

fn tint(v: u8, c: [u8; 3]) -> [u8; 3] {
    [
        (v as u16 * c[0] as u16 / 255) as u8,
        (v as u16 * c[1] as u16 / 255) as u8,
        (v as u16 * c[2] as u16 / 255) as u8,
    ]
}

fn grid(img: &mut Rgb, x0: usize, y0: usize, w: usize, h: usize) {
    for k in [0usize, 64, 128, 192, 255] {
        let y = y0 + h - 1 - k * (h - 1) / 255;
        for x in (x0..x0 + w).step_by(2) {
            img.put(x, y, [70, 70, 70]);
        }
    }
}

/// Waveform-style panel from 256-row columns at `off` (count[col * 256 + level]).
fn wave_panel(img: &mut Rgb, counts: &[u32], off: usize, x0: usize, c: [u8; 3]) {
    let max = counts[off..off + WAVE_LEN]
        .iter()
        .copied()
        .max()
        .unwrap_or(0);
    grid(img, x0, 0, WAVE_COLS, 256);
    for col in 0..WAVE_COLS {
        for lv in 0..256 {
            let v = intensity(counts[off + col * 256 + lv], max);
            if v > 0 {
                img.put(x0 + col, 255 - lv, tint(v, c));
            }
        }
    }
}

/// 1024x512 scope sheet: luma waveform | vectorscope | histogram on top,
/// RGB parade below (256 columns per channel).
pub fn scope_image(counts: &[u32]) -> Rgb {
    let mut img = Rgb::new(1024, 512, [0, 0, 0]);
    wave_panel(&mut img, counts, OFF_WAVE, 0, [255, 255, 255]);
    // Vectorscope at x 256..512 (2 px gap left/right).
    let vx = 384usize;
    let vmax = counts[OFF_VEC..OFF_VEC + 65536]
        .iter()
        .copied()
        .max()
        .unwrap_or(0);
    for k in 0..256usize {
        // outer circle + skin-tone line as graticule
        let a = k as f64 / 256.0 * std::f64::consts::TAU;
        img.put(
            (vx as f64 + 127.0 * a.cos()) as usize,
            (128.0 - 127.0 * a.sin()) as usize,
            [60, 60, 60],
        );
    }
    let sa = SKIN_LINE_DEG.to_radians();
    for r in 0..128 {
        img.put(
            (vx as f64 + r as f64 * sa.cos()) as usize,
            (128.0 - r as f64 * sa.sin()) as usize,
            [150, 110, 70],
        );
    }
    for cr in 0..256usize {
        for cb in 0..256usize {
            let v = intensity(counts[OFF_VEC + cr * 256 + cb], vmax);
            if v > 0 {
                img.put(vx - 128 + cb, 255 - cr, tint(v, [120, 255, 120]));
            }
        }
    }
    // Histogram at x 512..1024: R, G, B, luma overlaid, 2 px per bin.
    let hmax = counts[OFF_HIST..OFF_HIST + 1024]
        .iter()
        .copied()
        .max()
        .unwrap_or(1)
        .max(1);
    grid(&mut img, 512, 0, 512, 256);
    for (ch, c) in [
        (1usize, [255u8, 60, 60]),
        (2, [60, 255, 60]),
        (3, [80, 120, 255]),
        (0, [230, 230, 230]),
    ] {
        for bin in 0..256usize {
            let n = counts[OFF_HIST + ch * 256 + bin] as u64;
            let top = 255 - (n * 255 / hmax as u64) as usize;
            for y in top..256 {
                for dx in 0..2 {
                    let i = (y * img.w + 512 + bin * 2 + dx) * 3;
                    for (p, &ck) in img.px[i..i + 3].iter_mut().zip(&c) {
                        *p = (*p).max(ck / 2);
                    }
                }
            }
            img.put(512 + bin * 2, top, c);
            img.put(513 + bin * 2, top, c);
        }
    }
    // Parade (256 columns each) in the bottom row.
    let mut parade = Rgb::new(1024, 256, [0, 0, 0]);
    for (k, c) in [[255u8, 60, 60], [60, 255, 60], [80, 120, 255]]
        .into_iter()
        .enumerate()
    {
        wave_panel(
            &mut parade,
            counts,
            OFF_PARADE + k * WAVE_LEN,
            k * 341 + 22,
            c,
        );
    }
    img.blit(&parade, 0, 256);
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_and_text() {
        let mut img = Rgb::new(80, 20, [0, 0, 0]);
        text(&mut img, 1, 1, "01:23#4", 1, [255, 255, 255]);
        assert!(img.px.contains(&255));
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.png");
        img.write_png(&p).unwrap();
        assert_eq!(Rgb::read_png(&p).unwrap(), img);
        let b1 = std::fs::read(&p).unwrap();
        img.write_png(&p).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b1, "png bytes deterministic");
    }
}
