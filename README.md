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
./scripts/build-ffmpeg-lgpl.sh              # once: LGPL-only shared FFmpeg into third_party/ffmpeg-lgpl (~1-2 min)
./scripts/gen-test-media.sh                 # synthetic 1080p24 clips into media/ (gitignored)
cargo build --release
./target/release/cutline ffmpeg             # must say "license: LGPL version 2.1 or later"
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
- **FFmpeg (LGPL only)**: Cutline is headed for Apache-2.0, so it links a shared, LGPL v2.1+ FFmpeg
  built in user space by `scripts/build-ffmpeg-lgpl.sh` (FFmpeg 9.0.2, signature-checked tarball;
  `--disable-autodetect`, no `--enable-gpl`/`--enable-nonfree`/`--enable-version3`; zlib +
  nv-codec-headers for NVENC/NVDEC/CUVID; NASM is built locally if missing). Sources, build tree and
  install all live in gitignored `third_party/`; set `PREFIX=...` to install elsewhere.
  - `.cargo/config.toml` points `PKG_CONFIG_PATH` at `third_party/ffmpeg-lgpl/lib/pkgconfig`, so
    `ffmpeg-sys-next` finds it; `crates/cutline-engine/build.rs` embeds an rpath to that libdir.
    An explicit `PKG_CONFIG_PATH` in your environment overrides this (e.g. a `~/.local/opt` prefix).
  - **Fallback (not for distribution):** if the LGPL prefix is missing, pkg-config falls back to the
    system FFmpeg (on watts: linuxbrew 9.0.1, a GPL build with x264/x265). The build prints a
    `FALLBACK FFmpeg ... do not distribute` warning, `cutline render` prints a non-LGPL warning, and
    `CUTLINE_REQUIRE_LGPL_FFMPEG=1` turns the fallback into a build error (use it in CI/release).
  - The engine itself only uses LGPL-native codecs (FFV1 master). H.264/HEVC/AV1 delivery would use
    NVENC (`h264_nvenc`/`hevc_nvenc`/`av1_nvenc`), which this build includes. No software AV1
    decoder yet (dav1d not built); AV1 decodes via NVDEC (`av1_cuvid` / `-hwaccel cuda`).
- Color transforms in the compositor are placeholders until `cutline-color` (OCIO) lands.
