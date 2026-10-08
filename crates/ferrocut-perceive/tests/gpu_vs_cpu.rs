//! The GPU scope pass must produce exactly the CPU reference's counts.
//! Skips (passes with a note) without a GPU adapter.

use ferrocut_core::{AdapterPreference, GpuContext};
use ferrocut_perceive::gpu::GpuScopes;
use ferrocut_perceive::scopes::{BASIC_LEN, FULL_LEN, counts_cpu};

/// Deterministic pseudo-random BGRZ frame with saturated and neutral areas.
fn frame(w: usize, h: usize, seed: u32) -> Vec<u8> {
    let mut s = seed.wrapping_mul(2654435761).max(1);
    let mut px = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let i = (y * w + x) * 4;
            let v = if x < w / 4 {
                [255, 0, (s & 0xff) as u8]
            } else if y < h / 3 {
                [(x * 255 / w) as u8; 3]
            } else {
                [(s & 0xff) as u8, (s >> 8) as u8, (s >> 16) as u8]
            };
            px[i..i + 3].copy_from_slice(&v);
            px[i + 3] = (s >> 24) as u8; // padding byte must be ignored
        }
    }
    px
}

#[test]
fn gpu_counts_equal_cpu_counts() {
    let gpu = match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            return;
        }
    };
    eprintln!("adapter: {}", gpu.describe());
    for (w, h) in [(160, 96), (97, 55), (1920, 1080), (33, 19)] {
        let scopes = GpuScopes::new(&gpu, w as u32, h as u32).unwrap();
        for (k, full) in [true, false, true, false].into_iter().enumerate() {
            let px = frame(w, h, (w * h + k) as u32);
            let g = scopes.counts(&gpu, &px, full).unwrap();
            let c = counts_cpu(&px, w, h, full);
            assert_eq!(g.len(), if full { FULL_LEN } else { BASIC_LEN });
            if g != c {
                let first = g.iter().zip(&c).position(|(a, b)| a != b).unwrap();
                panic!(
                    "{w}x{h} full={full}: first difference at counter {first}: gpu {} cpu {}",
                    g[first], c[first]
                );
            }
        }
    }
}
