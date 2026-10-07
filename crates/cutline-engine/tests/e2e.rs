//! End-to-end: synthesize sources, render on the GPU twice, check determinism,
//! then edit one clip and check only the affected chunks re-render.
//! Skips (passes with a note) when no GPU adapter is available.

use std::path::Path;

use cutline_core::{AdapterPreference, GpuContext, Rational};
use cutline_engine::media::encode::{ChunkEncoder, EncodeSettings};
use cutline_engine::render::{ChunkStatus, RenderOptions};
use cutline_engine::{Timeline, compile, render};

const W: u32 = 160;
const H: u32 = 96;

fn synth(path: &Path, frames: i64, seed: u8) {
    let s = EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(24),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..frames {
        let mut px = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                px[i] = (x as i64 * 2 + f * 3) as u8 ^ seed;
                px[i + 1] = (y as i64 * 2 + f) as u8;
                px[i + 2] = seed.wrapping_mul(37).wrapping_add((x + y) as u8);
                px[i + 3] = 255;
            }
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

fn timeline(dir: &Path, opacity: &str) -> Timeline {
    let json = format!(
        r#"{{
      "output": {{ "width": {W}, "height": {H}, "fps": "24", "gop": 12 }},
      "tracks": [
        {{ "clips": [
          {{ "id": "a", "source": "a.mkv", "start": 0, "duration": "2" }},
          {{ "id": "b", "source": "b.mkv", "start": "3/2", "source_in": "1/4", "duration": "3/2",
             "transition_in": {{ "kind": "dissolve", "duration": "1/2" }} }}
        ]}},
        {{ "clips": [ {{ "id": "t", "source": "c.mkv", "start": "5/2", "duration": "1/4", "opacity": "{opacity}" }} ] }}
      ]
    }}"#
    );
    let p = dir.join(format!("tl-{}.json", opacity.replace('/', "_")));
    std::fs::write(&p, json).unwrap();
    Timeline::load(&p).unwrap()
}

#[test]
fn deterministic_and_incremental() {
    let gpu = match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            return;
        }
    };
    eprintln!("adapter: {}", gpu.describe());
    let dir = tempfile::tempdir().unwrap();
    synth(&dir.path().join("a.mkv"), 60, 1);
    synth(&dir.path().join("b.mkv"), 60, 2);
    synth(&dir.path().join("c.mkv"), 30, 3);

    let tl = timeline(dir.path(), "1/2");
    let c = compile(&tl).unwrap();
    let run = |name: &str, force: bool, tl: &Timeline, c: &cutline_engine::Compiled| {
        let opts = RenderOptions {
            cache_dir: dir.path().join(format!("cache-{name}")),
            force,
            jobs: 4,
        };
        render(tl, c, &gpu, &dir.path().join(format!("{name}.mkv")), &opts).unwrap()
    };
    let r1 = run("one", true, &tl, &c);
    let r2 = run("two", true, &tl, &c);
    assert_eq!(r1.total_frames, 72);
    assert_eq!(r1.chunks.len(), 6);
    let h1: Vec<_> = r1.chunks.iter().map(|c| c.file_blake3.clone()).collect();
    let h2: Vec<_> = r2.chunks.iter().map(|c| c.file_blake3.clone()).collect();
    assert_eq!(h1, h2, "per-chunk hashes differ between runs");
    assert_eq!(
        r1.final_blake3, r2.final_blake3,
        "final file differs between runs"
    );

    // Same cache dir as run "one": unchanged timeline reuses everything.
    let r3 = run("one", false, &tl, &c);
    assert!(r3.chunks.iter().all(|c| c.status == ChunkStatus::Reused));
    assert_eq!(r3.final_blake3, r1.final_blake3);

    // Edit the overlay (frames 60..66 -> chunk 5 only).
    let tl2 = timeline(dir.path(), "3/4");
    let c2 = compile(&tl2).unwrap();
    let r4 = run("one", false, &tl2, &c2);
    let rendered: Vec<usize> = r4
        .chunks
        .iter()
        .filter(|c| c.status == ChunkStatus::Rendered)
        .map(|c| c.plan.index)
        .collect();
    assert_eq!(rendered, vec![5]);
    assert_ne!(r4.final_blake3, r1.final_blake3);
    for i in 0..5 {
        assert_eq!(r4.chunks[i].file_blake3, r1.chunks[i].file_blake3);
    }
}
