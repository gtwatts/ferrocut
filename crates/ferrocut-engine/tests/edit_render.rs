//! Edit ops + incremental cache: a small edit re-renders only the chunks that
//! overlap the op's reported change span, and the incremental result equals a
//! from-scratch render of the edited timeline. Skips without a GPU adapter.

use std::path::Path;

use ferrocut_core::{AdapterPreference, GpuContext, Rational, SharedGpu};
use ferrocut_engine::edit::{MediaLengths, apply, parse_ops};
use ferrocut_engine::media::encode::{ChunkEncoder, EncodeSettings};
use ferrocut_engine::render::{ChunkStatus, RenderOptions, RenderReport};
use ferrocut_engine::{Timeline, compile, render};

const W: u32 = 128;
const H: u32 = 72;
const FPS: i64 = 24;

fn synth(path: &Path, frames: i64, seed: u8) {
    let s = EncodeSettings {
        width: W,
        height: H,
        fps: Rational::from_int(FPS),
        gop: 6,
    };
    let mut e = ChunkEncoder::create(path, &s).unwrap();
    for f in 0..frames {
        let mut px = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                px[i] = (x as i64 * 3 + f * 5) as u8 ^ seed;
                px[i + 1] = (y as i64 * 3 + f * 2) as u8;
                px[i + 2] = seed.wrapping_mul(53).wrapping_add((x ^ y) as u8);
                px[i + 3] = 255;
            }
        }
        e.push_bgra(&px).unwrap();
    }
    e.finish().unwrap();
}

/// 6 s at 24 fps, GOP/chunk 12 frames -> 12 chunks. Clips a [0,2), b [2,4)
/// and c [4,6) on one track; every source is 5 s long so slips have handles.
const BASE: &str = r#"{
  "output": { "width": 128, "height": 72, "fps": "24", "gop": 12 },
  "tracks": [ { "name": "V1", "clips": [
    { "id": "a", "source": "a.mkv", "start": 0, "source_in": "1", "duration": "2" },
    { "id": "b", "source": "b.mkv", "start": "2", "source_in": "1", "duration": "2" },
    { "id": "c", "source": "c.mkv", "start": "4", "source_in": "1", "duration": "2" }
  ]}]
}"#;

fn rendered(r: &RenderReport) -> Vec<usize> {
    r.chunks
        .iter()
        .filter(|c| c.status == ChunkStatus::Rendered)
        .map(|c| c.plan.index)
        .collect()
}

/// Chunks whose frames overlap the timeline span `[s, e)`.
fn overlapping(
    r: &RenderReport,
    span: (ferrocut_core::RationalTime, ferrocut_core::RationalTime),
) -> Vec<usize> {
    let fps = Rational::from_int(FPS);
    let (f0, f1) = (span.0.frame_floor(fps), span.1.frame_ceil(fps));
    r.chunks
        .iter()
        .filter(|c| c.plan.start_frame < f1 && c.plan.start_frame + c.plan.frames > f0)
        .map(|c| c.plan.index)
        .collect()
}

#[test]
fn slip_and_roll_rerender_only_affected_chunks() {
    let gpu = match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => SharedGpu::new(g),
        Err(e) => {
            eprintln!("SKIP: no GPU ({e})");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for (name, seed) in [("a", 11u8), ("b", 22), ("c", 33)] {
        synth(&d.join(format!("{name}.mkv")), 5 * FPS, seed);
    }
    let base_path = d.join("base.json");
    std::fs::write(&base_path, BASE).unwrap();
    let base = Timeline::load(&base_path).unwrap();

    let run = |name: &str, cache: &str, force: bool, tl: &Timeline| {
        let c = compile(tl).unwrap();
        let opts = RenderOptions {
            force,
            jobs: 4,
            ..RenderOptions::new(d.join(cache))
        };
        render(tl, &c, &gpu, &d.join(format!("{name}.mkv")), &opts).unwrap()
    };
    let edit = |ops: &str| {
        let ops = parse_ops(ops).unwrap();
        let mut media = MediaLengths::new(d, |p: &Path| {
            ferrocut_engine::media::media_duration(p).ok().flatten()
        });
        let (tl, changes) = apply(&base, &ops, &mut media).unwrap();
        assert_eq!(changes.len(), 1);
        (tl, changes[0].span)
    };

    let r0 = run("base", "cache", true, &base);
    assert_eq!(r0.chunks.len(), 12);
    assert!(r0.chunks.iter().all(|c| c.status == ChunkStatus::Rendered));

    // Slip b by +1/2 s: frames 48..96 change -> chunks 4..8; position and
    // duration are untouched so nothing else re-renders.
    let (slipped, span) = edit(r#"[{ "op": "slip", "clip": "b", "delta": "1/2" }]"#);
    let r1 = run("slip", "cache", false, &slipped);
    assert_eq!(rendered(&r1), vec![4, 5, 6, 7]);
    assert_eq!(rendered(&r1), overlapping(&r1, span));
    for c in r1.chunks.iter().filter(|c| c.status == ChunkStatus::Reused) {
        assert_eq!(c.file_blake3, r0.chunks[c.plan.index].file_blake3);
    }
    assert_ne!(r1.final_blake3, r0.final_blake3);
    let fresh = run("slip-fresh", "cache-slip-fresh", true, &slipped);
    assert_eq!(
        r1.final_blake3, fresh.final_blake3,
        "incremental != fresh (slip)"
    );

    // Roll the a|b edit point from 2 s to 2 1/4 s: only [2, 2 1/4) changes
    // (b's later frames show the same source frames), i.e. chunk 4 alone.
    let (rolled, span) = edit(r#"[{ "op": "roll", "clip": "a", "delta": "1/4" }]"#);
    assert_eq!(rolled.duration(), base.duration());
    let r2 = run("roll", "cache", false, &rolled);
    assert_eq!(rendered(&r2), vec![4]);
    assert_eq!(rendered(&r2), overlapping(&r2, span));
    for c in r2.chunks.iter().filter(|c| c.status == ChunkStatus::Reused) {
        assert_eq!(c.file_blake3, r0.chunks[c.plan.index].file_blake3);
    }
    let fresh = run("roll-fresh", "cache-roll-fresh", true, &rolled);
    assert_eq!(
        r2.final_blake3, fresh.final_blake3,
        "incremental != fresh (roll)"
    );

    // Undoing both edits reuses every base chunk.
    let r3 = run("base-again", "cache", false, &base);
    assert!(rendered(&r3).is_empty());
    assert_eq!(r3.final_blake3, r0.final_blake3);
}
