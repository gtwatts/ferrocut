//! Test fixture: synthetic FFV1 sources through the engine's encoder.

use std::path::Path;

use ferrocut_core::Rational;
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};

pub const W: u32 = 160;
pub const H: u32 = 96;

/// Smooth gradient in `base` with a slowly moving bright square (or static).
pub fn synth(path: &Path, frames: i64, base: [u8; 3], moving: bool) {
    let s = EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..frames {
        let mut px = vec![0u8; (W * H * 4) as usize];
        let sx = 20 + if moving { f as u32 } else { 0 };
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                let g = (x / 8 + y / 8) as u8;
                let sq = base != [0, 0, 0] && (sx..sx + 16).contains(&x) && (30..46).contains(&y);
                let [r, gg, b] = if sq {
                    [255, 255, 255]
                } else {
                    [
                        base[0].saturating_add(g),
                        base[1].saturating_add(g),
                        base[2].saturating_add(g),
                    ]
                };
                if base == [0, 0, 0] {
                    px[i..i + 3].copy_from_slice(&[0, 0, 0]);
                } else {
                    px[i..i + 3].copy_from_slice(&[b, gg, r]);
                }
                px[i + 3] = 255;
            }
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}
