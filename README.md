# Cutline

Headless, agent-native video editing and compositing engine (Rust).

Brief: Obsidian vault `Pi Memory/Projects/cutline-research-brief-2026-10-07.md`.

## Crate ownership
- `cutline-core`: shared frame type, render node trait, rational time (Rusty + SeePlus)
- `cutline-engine`: timeline, scheduler, render graph, FFmpeg I/O, wgpu compositor (Rusty)
- `cutline-color`: OCIO bridge (SeePlus)
- `cutline-ofx`: out-of-process OpenFX host (SeePlus)

## Engine spike (`cutline-engine`)

Headless render path: JSON timeline -> pull-based render graph -> FFmpeg decode ->
wgpu compositor (linear ACEScg, half float, premultiplied) -> readback -> FFV1/MKV
chunks rendered in parallel -> lossless concat.

```sh
./scripts/gen-test-media.sh                 # synthetic 1080p24 clips into media/ (gitignored)
cargo build --release
cargo test --release
./target/release/cutline adapters           # which GPU wgpu picks (discrete NVIDIA preferred; CUTLINE_ADAPTER=<name> overrides)
./target/release/cutline plan examples/demo.json          # chunk keys, no decode/GPU
./target/release/cutline render examples/demo.json -o out/demo.mkv
./target/release/cutline render examples/demo-edit-opacity.json -o out/edit.mkv   # only chunks 8,9 re-render
```

- **Time** is exact rationals everywhere (`cutline_core::RationalTime`); JSON times are `"n"` or `"n/d"`.
- **Cache keys**: each frame's key is a Merkle hash of the node's parameters at `t`, `t`, and
  the keys of the inputs it pulls at `t`. A chunk's key hashes its frame keys plus the encoder
  fingerprint. Chunks live in `<out dir>/.cutline-cache/chunks/<key>.mkv`; `--force` re-renders all.
- **Chunks** are `gop * gops_per_chunk` frames, GOP-aligned, each an independent closed-GOP encode.
- **Determinism**: bit-exact on the same machine/driver (any `--jobs`). Across GPUs expect a
  perceptual match (NVIDIA vs Intel Arc on watts: SSIM 0.99989, PSNR 72.7 dB), not identical bytes.
- **FFmpeg**: `ffmpeg-next` 9 against system libav* found by pkg-config; `build.rs` embeds an rpath
  to that libdir. Only LGPL-native codecs are used (FFV1 master); no x264/x265.
- Color transforms in the compositor are placeholders until `cutline-color` (OCIO) lands.
