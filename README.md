# Ferrocut

Headless, agent-native video editing and compositing engine (Rust).

Brief: Obsidian vault `Pi Memory/Projects/cutline-research-brief-2026-10-07.md` (written before the rename to Ferrocut).

License: Apache-2.0 (see `LICENSE`).

## Crate ownership
- `ferrocut-types`: GPU-free shared types: rational time, content hashes, file manifests, color-space tags, pixel windows, CPU frames, `NodeError`, `CancelToken` (Rusty + SeePlus)
- `ferrocut-core`: shared GPU context + texture pool, GPU `Frame`, `RenderNode`/`RenderCtx`; re-exports `ferrocut-types` (Rusty + SeePlus)
- `ferrocut-engine`: timeline, edit ops, scheduler, render graph, FFmpeg I/O, wgpu compositor, layer transform (Rusty)
- `ferrocut-audio`: pure-Rust, deterministic audio mixer: buses, fades/crossfades, ducking, EBU R128 normalization, true-peak limiter (Rusty)
- `ferrocut-colorspace`: pure-Rust Rec.709/sRGB/ACEScg matrices, transfer functions and matching WGSL (SeePlus)
- `ferrocut-color`: OCIO bridge (SeePlus)
- `ferrocut-ofx`: out-of-process OpenFX host (SeePlus)
- `ferrocut-ipc`: shared plumbing for out-of-process hosts (SeePlus)
- `ferrocut-lottie`, `ferrocut-html`: Lottie and HTML layers (SeePlus)

## Engine spike (`ferrocut-engine`)

Headless render path: JSON timeline -> pull-based render graph -> FFmpeg decode ->
wgpu compositor (linear ACEScg, half float, premultiplied) -> readback -> FFV1/MKV
chunks rendered in parallel -> lossless concat, with the audio mix (`ferrocut-audio`)
muxed alongside as PCM.

```sh
./scripts/build-ffmpeg-lgpl.sh              # once: LGPL-only shared FFmpeg into third_party/ffmpeg-lgpl (~1-2 min)
./scripts/gen-test-media.sh                 # synthetic 1080p24 clips into media/ (gitignored)
cargo build --release
./target/release/ferrocut ffmpeg             # must say "license: LGPL version 2.1 or later"
cargo test --release
./target/release/ferrocut adapters           # which GPU wgpu picks (discrete NVIDIA preferred; FERROCUT_ADAPTER=<name> overrides)
./target/release/ferrocut plan examples/demo.json          # chunk keys, no decode/GPU
./target/release/ferrocut render examples/demo.json -o out/demo.mkv
./target/release/ferrocut render examples/demo-edit-opacity.json -o out/edit.mkv   # only chunks 8,9 re-render
./target/release/ferrocut render examples/demo-av.json -o out/av.mkv   # dialogue + ducked music, J/L cuts, animated overlay, -14 LUFS
echo '[{"op":"slip","clip":"cam_b","delta":"1/2"}]' > /tmp/ops.json
./target/release/ferrocut edit examples/demo-av.json /tmp/ops.json -o out/av-slip.json --plan   # prints the chunks that will re-render
```

- **Environment variables**: `FERROCUT_ADAPTER=<name substring>` picks the GPU;
  `FERROCUT_LGPL_FFMPEG_PREFIX` (set by `.cargo/config.toml`) is the expected LGPL FFmpeg prefix;
  `FERROCUT_REQUIRE_LGPL_FFMPEG=1` fails the build on a non-LGPL fallback;
  `FERROCUT_FFMPEG_LIBDIR` overrides the embedded rpath (empty disables it).
- **Time** is exact rationals everywhere (`ferrocut_core::RationalTime`); JSON times are `"n"` or `"n/d"`.
  Every time -> pts/frame conversion rounds to nearest with exact halves away from zero, matching
  FFmpeg's `av_rescale_rnd(.., AV_ROUND_NEAR_INF)` (tested against libavutil and a real mux + seek).
- **Cache keys**: each frame's key is a Merkle hash of the node's parameters at `t`, `t`, and
  the keys of the inputs it pulls at `t`. A chunk's key hashes its frame keys plus the encoder
  fingerprint. Chunks live in `<out dir>/.ferrocut-cache/chunks/<key>.mkv`; `--force` re-renders all.
- **Chunks** are `gop * gops_per_chunk` frames, GOP-aligned, each an independent closed-GOP encode.
- **Determinism**: bit-exact on the same machine/driver (any `--jobs`). Across GPUs expect a
  perceptual match (NVIDIA vs Intel Arc on watts: SSIM 0.99989, PSNR 72.7 dB), not identical bytes.
- **FFmpeg (LGPL only)**: Ferrocut is Apache-2.0, so it links a shared, LGPL v2.1+ FFmpeg
  built in user space by `scripts/build-ffmpeg-lgpl.sh` (FFmpeg 9.0.2, signature-checked tarball;
  `--disable-autodetect`, no `--enable-gpl`/`--enable-nonfree`/`--enable-version3`; zlib +
  nv-codec-headers for NVENC/NVDEC/CUVID; NASM is built locally if missing). Sources, build tree and
  install all live in gitignored `third_party/`; set `PREFIX=...` to install elsewhere.
  - `.cargo/config.toml` points `PKG_CONFIG_PATH` at `third_party/ffmpeg-lgpl/lib/pkgconfig`, so
    `ffmpeg-sys-next` finds it. An explicit `PKG_CONFIG_PATH` in your environment overrides this
    (e.g. a `~/.local/opt` prefix).
  - **Relocatable:** the install has no absolute paths. The libs and `ffmpeg`/`ffprobe` carry
    RUNPATH `$ORIGIN/../lib` and the `.pc` files use `prefix=${pcfiledir}/../..` (the script
    checks both). `crates/ferrocut-engine/build.rs` canonicalizes the libdir and, when it is inside
    the workspace, embeds `$ORIGIN`-relative rpaths (`$ORIGIN/../../third_party/ffmpeg-lgpl/lib`
    for `target/<profile>/ferrocut`, `$ORIGIN/../../../…` for test binaries), so moving or renaming
    the checkout needs no rebuild. A prefix outside the workspace is embedded as its canonical
    absolute path.
  - **Fallback (not for distribution):** if the LGPL prefix is missing, pkg-config falls back to the
    system FFmpeg (on watts: linuxbrew 9.0.1, a GPL build with x264/x265). The build prints a
    `FALLBACK FFmpeg ... do not distribute` warning, `ferrocut render` prints a non-LGPL warning, and
    `FERROCUT_REQUIRE_LGPL_FFMPEG=1` turns the fallback into a build error (use it in CI/release).
  - The engine itself only uses LGPL-native codecs (FFV1 master). H.264/HEVC/AV1 delivery would use
    NVENC (`h264_nvenc`/`hevc_nvenc`/`av1_nvenc`), which this build includes. No software AV1
    decoder yet (dav1d not built); AV1 decodes via NVDEC (`av1_cuvid` / `-hwaccel cuda`).
- **Color**: the compositor's input (Rec.709 BT.709 OETF → linear ACEScg) and output (inverse)
  transforms come from `ferrocut-colorspace` (`wgsl()` prepended to the shaders; OCIO 2.5 matrices).
  `ferrocut_colorspace::VERSION` is part of every chunk key. Full OCIO transforms are `ferrocut-color` nodes.
  The kernels' conversion functions are resolved by space name (`named::transfer`, `named::wgsl_matrix_fn`)
  from the frame tags: source/output `Camera Rec.709`, working `ACEScg`.
- **Test media** (`scripts/gen-test-media.sh`) is byte-reproducible: existing files are kept unless
  `FORCE=1`, and every generator is deterministic (FFmpeg's `gradients` source is not, even with a seed,
  so it was replaced by a `geq` expression). Regenerating media with a nondeterministic source changes
  render hashes without any engine change.

### Audio

- **Model.** Video clips carry their linked audio (`"audio": {...}` on a clip); audio-only tracks live
  in `audio_tracks`. A clip's audio region is `[start + in_offset, end + out_offset)`, so a **J-cut** is a
  negative `in_offset` (audio leads, needs source handles) and an **L-cut** a positive `out_offset`.
  Per clip: `gain_db`, `pan` (keyframable), `mute`, `fade_in`/`fade_out`, `crossfade_in` (`linear` or
  `equal_power`; the outgoing clip on the same track must end where the crossfade ends). Each track
  has a bus (`gain_db`, `pan`, `mute`, keyframable) and an optional sidechain **duck**
  (`key` tracks, `threshold_db`, `ratio`, `attack_ms`, `release_ms`, `range_db`). The master bus has
  `audio.master_gain_db` and an optional `audio.loudness` target (`target_lufs`, `true_peak_dbtp`).
- **Decode.** FFmpeg decode + swresample to the project rate (default 48 kHz) as f32 planar, mono kept
  mono, >2 channels downmixed to stereo. Source time 0 is the video stream's start (A/V files) so linked
  audio lines up with the decoder's frames. Mono pans with constant power (-3 dB center), stereo with balance.
- **Sample-exact placement.** Every position is `RationalTime -> sample` with the same rounding as the video
  side (nearest, halves away from zero). Chunk `[f0, f1)` gets samples `[S(f0/fps), S(f1/fps))`, and the master
  carries one PCM packet per video frame `[S(i), S(i+1))`, so 48000/23.976 = 2002.002 samples/frame
  partitions exactly and lossless concat stays exact (PCM `pcm_f32le` stereo in the MKV: no codec priming).
- **Determinism.** `ferrocut_audio::analyze` is one sequential whole-program pass (ducking envelopes, R128
  measurement, normalization gain, limiter gain curve); `render_range(a, b)` is stateless and sums in a
  fixed order, so chunked == unchunked sample-for-sample and runs are bit-exact. No fast-math, no
  thread-dependent reduction. Audio is re-mixed on every render (cheap); video chunk keys don't depend on it.
- **Loudness.** Two passes: measure integrated loudness (EBU R128, `ebur128` crate, MIT, pure Rust), apply a
  constant gain to the target (e.g. -23 LUFS broadcast, -14 streaming), then a 5 ms lookahead true-peak
  limiter (4x oversampled) holds the ceiling; iterated until within 0.02 LU. The render report has the
  analysis (before/after loudness, gain, limiter reduction, duck stats) and per-chunk audio hashes.

### Edit operations (`ferrocut edit`)

Typed, agent-facing timeline edits applied as a JSON list (`[...]` or `{"ops": [...]}`), atomically and in
order: `split {clip, at, new_id}`, `trim {clip, edge: in|out, delta}`, `ripple_delete {clip, all_tracks}`,
`ripple_insert {track, at, clip, all_tracks}`, `roll {clip, delta}` (edit point after `clip`),
`slip {clip, delta}`, `slide {clip, delta}`, `move {clip, to, track}`, `jl_cut {clip, in_offset, out_offset}`.
Works on video and audio tracks. Each op is validated with a clear error (e.g. a slip beyond the source
media names the maximum `source_in`; lengths are probed with FFmpeg unless `--no-probe`) and reports
the timeline span it affects; `--plan` lists the video chunks that will re-render. Property tests cover
the invariants (ripple keeps downstream order without overlaps, roll preserves total duration, slip
preserves position/duration, slide preserves track duration, failed scripts change nothing). Frame keys
depend only on what is visible at `t` (the clip's placement is captured by the pulled source frame), so
on `demo-av.json` a 1/2 s slip re-renders 5 of 13 chunks and a roll 1 of 13.

### Keyframes and the layer transform

Any keyframable value is a constant (`"1/2"`, `"0.8"`; exact decimals are accepted) or
`{"keyframes": [{"t": "0", "v": "0", "interp": ...}, ...]}`. The interpolation on a key governs the
segment to the next key: `hold`, `linear` (default), CSS `ease`/`ease_in`/`ease_out`/`ease_in_out`,
After Effects `easy_ease`, `{"bezier": [x1, y1, x2, y2]}`, or AE
`{"speed": {"out_speed", "out_influence", "in_speed", "in_influence"}}`. Values are exact at keys, easing
is monotone in time, evaluation is deterministic. Clip parameters use clip-local time (0 = clip start;
edit ops keep keys in place on the timeline); bus parameters use timeline time. Keyframable: clip
`opacity`, audio `gain_db`/`pan`, bus `gain_db`/`pan`, and the clip `transform`:

```json
"transform": { "anchor": ["960", "540"], "position": ["480", "270"], "scale": "0.5", "rotation": "-12" }
```

Position/anchor are pixels (default: frame center), scale a factor (uniform or `[x, y]`), rotation
degrees clockwise. The transform node inverse-maps each output pixel in f64-planned math and filters in
linear premultiplied space with a separable Catmull-Rom kernel (interpolating, so integer moves are exact
copies), widened by the minification factor (up to 8x; beyond that it aliases). It is pixel-aspect aware
and outputs only the transformed bounding box as its data window. Held poses reuse cached frames.

### Core contract (agreed Rusty + SeePlus, Oct 7 2026)

- **One GPU device per render.** Nodes declare `gpu_requirements()` (required/optional wgpu features,
  minimum limits); the engine creates one `GpuContext` with `required ∪ (optional ∩ adapter)` and
  nodes check `gpu.has_features(..)` to fall back (e.g. OCIO LUTs drop to f16 without
  `FLOAT32_FILTERABLE`). No node creates its own device.
- **Frames** carry a display window (`width`/`height`), a **data window** (`PixelRect`, may be
  smaller or larger than the display; storage covers only it; outside is transparent black) and an
  exact **pixel aspect ratio**. Compositor ops output the union of their inputs' windows and require
  matching PARs; the output transform crops to the display window. Nodes that don't opt in via
  `supports_data_window()` get inputs reframed to the full display window.
- **GPU batching.** Working textures come from a pool keyed by (size, format, usage), sharded by
  allocating thread. Nodes that return `batches_gpu_work() = true` record into the worker's encoder;
  the scheduler submits once per frame and reads output back through a 3-deep staging ring per chunk
  (no blocking readback per frame). Other nodes get a flushed encoder and may submit on their own.
- **Errors.** `NodeError { kind: Retryable | Permanent | Cancelled, message }`. The scheduler retries a
  frame on `Retryable` (default 2 retries, `--retries`), never on the others. `NodeError::new` is
  `Permanent`. `RenderCtx` carries a `CancelToken` and optional deadline (`--timeout <secs>`), checked
  between frames and available to nodes via `ctx.check()`; the first failing chunk cancels its siblings.
- **GPU faults.** `NodeError::from_gpu(&wgpu::Error)` (`ferrocut_core::GpuErrorExt`) classifies
  out-of-memory and device-lost as `Retryable` with a `gpu_fault` (`NodeError::gpu_out_of_memory` /
  `NodeError::device_lost` build them directly); other validation/internal errors are `Permanent`.
  Each frame runs in a `GpuContext::error_scope`, so wgpu OOM becomes an error instead of a panic:
  the pool is trimmed and the frame retried. Device loss (reported by a node, or seen via wgpu's
  device-lost callback) is not retried per frame: the scheduler recreates the shared device, queue
  and texture pool on the same adapter (`SharedGpu::recover`) and re-renders the chunk on a fresh
  worker, at most `max_chunk_restarts` (2) times per chunk. Nodes that cache device objects outside
  worker slots must key them by `GpuContext::id()`, which changes on recreation.
- **Sequential nodes.** `RenderNode::access_pattern()` returns `Random` (default) or `Sequential`
  (can only step forward cheaply: browser pages, simulations). If any node feeding the output is
  sequential, the scheduler gives each worker one contiguous run of chunks, renders it in increasing
  time order on one persistent `WorkerState`, and the graph never asks a sequential node for an
  earlier time than its last on that worker without calling `reset_sequential(worker)` first (default:
  drop the worker slot keyed by `content_hash()`, so the node pre-rolls fresh).
- **File dependencies.** Nodes whose output depends on local files put
  `ferrocut_types::FileManifest::digest()` into their `NodeHash`: `(relative path, blake3 of bytes)`
  per file, sorted, independent of discovery order and of where the project lives. Network fetches
  can't be hashed: such nodes must block them during renders.
