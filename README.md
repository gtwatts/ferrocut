# Ferrocut

Headless, agent-native video editing and compositing engine (Rust).

Brief: Obsidian vault `Pi Memory/Projects/cutline-research-brief-2026-10-07.md` (written before the rename to Ferrocut).

License: Apache-2.0 (see `LICENSE`).

## Crate ownership
- `ferrocut-types`: GPU-free shared types: rational time, content hashes, file manifests, color-space tags, pixel windows, CPU frames, `NodeError`, `CancelToken` (Rusty + SeePlus)
- `ferrocut-core`: shared GPU context + texture pool, GPU `Frame`, `RenderNode`/`RenderCtx`; re-exports `ferrocut-types` (Rusty + SeePlus)
- `ferrocut-engine`: timeline, edit ops, scheduler, render graph, FFmpeg I/O, wgpu compositor, layer transform (Rusty)
- `ferrocut-audio`: pure-Rust, deterministic audio mixer: buses, fades/crossfades, ducking, EBU R128 normalization, true-peak limiter (Rusty)
- `ferrocut-mcp`: MCP server (stdio) exposing timeline inspection, journaled edits, diff, plan, render and reports to agents (Rusty)
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
./target/release/ferrocut render examples/demo.json -o out/demo-cpu.mkv --cpu   # software Vulkan (lavapipe), no GPU needed
./target/release/ferrocut plan examples/demo.json          # chunk keys, no decode/GPU
./target/release/ferrocut render examples/demo.json -o out/demo.mkv
./target/release/ferrocut render examples/demo-edit-opacity.json -o out/edit.mkv   # only chunks 8,9 re-render
./target/release/ferrocut render examples/demo-av.json -o out/av.mkv   # dialogue + ducked music, J/L cuts, animated overlay, -14 LUFS
echo '[{"op":"slip","clip":"cam_b","delta":"1/2"}]' > /tmp/ops.json
./target/release/ferrocut edit examples/demo-av.json /tmp/ops.json -o out/av-slip.json --plan   # prints the chunks that will re-render
./target/release/ferrocut diff examples/demo-av.json out/av-slip.json --summary   # structured diff + chunks to re-render (JSON without --summary)
./target/release/ferrocut log out/av-slip.json && ./target/release/ferrocut undo out/av-slip.json
```

- **Environment variables**: `FERROCUT_ADAPTER=<name substring>` picks the GPU (`cpu`: the software adapter);
  `FERROCUT_LGPL_FFMPEG_PREFIX` (set by `.cargo/config.toml`) is the expected LGPL FFmpeg prefix;
  `FERROCUT_REQUIRE_LGPL_FFMPEG=1` fails the build on a non-LGPL fallback;
  `FERROCUT_FFMPEG_LIBDIR` overrides the embedded rpath (empty disables it).
- **Time** is exact rationals everywhere (`ferrocut_core::RationalTime`); JSON times are `"n"` or `"n/d"`.
  Every time -> pts/frame conversion rounds to nearest with exact halves away from zero, matching
  FFmpeg's `av_rescale_rnd(.., AV_ROUND_NEAR_INF)` (tested against libavutil and a real mux + seek).
- **Cache keys**: each frame's key is a Merkle hash of the node's parameters at `t`, `t`, and
  the keys of the inputs it pulls at `t`. A chunk's key hashes its frame keys plus the encoder
  fingerprint. Chunks live in `<out dir>/.ferrocut-cache/chunks/<adapter>/<key>.mkv`; `--force` re-renders
  all. `<adapter>` hashes the adapter's name, ids, backend and driver version, because output is bit-exact
  per adapter and driver but only perceptually equal across them, so a master never mixes chunks from two
  adapters. Keys themselves stay adapter-free, so `plan` and `diff` need no GPU.
- **Chunks** are `gop * gops_per_chunk` frames, GOP-aligned, each an independent closed-GOP encode.
- **Determinism**: bit-exact on the same machine/driver (any `--jobs`). Across GPUs expect a
  perceptual match (NVIDIA vs Intel Arc on watts: SSIM 0.99989, PSNR 72.7 dB), not identical bytes.
- **CPU-only rendering** (`ferrocut render --cpu`, `FERROCUT_ADAPTER=cpu`, the MCP `render` tool's `cpu: true`) runs the same
  wgpu compositor on Mesa's software Vulkan driver, lavapipe (llvmpipe), with no separate code path.
  `ferrocut adapters --cpu` shows the pick. Lavapipe ships in `mesa-vulkan-drivers` (on watts: Mesa 26.1.6,
  `/usr/share/vulkan/icd.d/lvp_icd.json`), so nothing is built. Measured on watts (24 threads, 1080p24, load 4-10):

  | | CPU (lavapipe) | NVIDIA RTX 5090 Laptop |
  |---|---|---|
  | `demo.json` fps | 37.5 (`-j 4`), 40.8 (`-j 8`), 39.0 (`-j 24`) | 68.8 (`-j 4`) |
  | `demo-av.json` fps | 28.3 (`-j 4`), 29.6 (`-j 8`), 31.1 (`-j 24`) | 72.8 (`-j 4`) |
  | `demo.json` blake3 | `f9ae266e…` | `2b5f6c89…` |
  | `demo-av.json` blake3 | `25938728…` | `42cec359…` |

  - Lavapipe is bit-exact run to run and across `-j`.
  - Against the NVIDIA output: mean SSIM 0.99976 / 0.99991 (demo / demo-av; minimum 0.99104 on the last dissolve
    frame / 0.99907) and PSNR 70.3 / 73.1 dB (minimum 60.0 / 63.5). The largest difference is 1 / 2 levels in
    8 bits, on 0.61% / 0.32% of samples.
  - Audio is computed on the CPU and identical on both adapters.
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

### Journal, dry runs, undo, diff and branches

- **Journal.** `ferrocut edit` edits in place (or writes `-o`) and appends one JSON line per applied script to
  `<timeline>.journal.jsonl` beside the written file: the ops with their parameters, each op's change summary and
  span, the branch, and the timeline's canonical hash before and after. The journal is append-only, and it carries a
  sequence number and **no timestamps**, so the same edits give a byte-identical journal. The canonical hash is
  blake3 over the timeline's serde JSON with object keys sorted, so formatting and key order don't change it.
  Every state the journal mentions is stored in `<dir>/.ferrocut/snapshots/<hash>.json`.
  `--dry-run` applies and reports without writing anything; `--no-journal` writes without journaling;
  `--json` prints the outcome as JSON.
- **`ferrocut log <tl>`** lists the entries, the current branch, and whether the file still matches the journal
  (`--json` for agents).
- **`ferrocut undo <tl>`** restores the `before` snapshot of the newest live edit or merge on the current branch,
  and repeated undos walk back. It refuses if the file changed since that edit; `--force` overrides, and the
  discarded state stays in snapshots. Undo is itself journaled. There is no redo yet.
- **`ferrocut diff <a.json> <b.json>`** prints structured JSON:
  - `settings` and `tracks` changes;
  - `clips` matched by id: `added`, `removed`, or `changed` with tags (`moved`, `trimmed_in`, `trimmed_out`,
    `slipped`, `retimed`, `track_changed`, `source_changed`, `opacity_changed`, `transform_changed`,
    `transition_changed`, `audio_changed`, `keyframes_changed`), field-level `from`/`to` per changed path, and
    keyframe diffs matched by key time (`added`/`removed`/`changed`);
  - the `affected` timeline spans (conservative: the union of changed clips' video and audio extents);
  - `render`: the output chunks that would re-render (`dirty_chunks`, merged frame/second `ranges`, reuse counts).
    This compares chunk keys exactly as the content-addressed cache does, so the media must exist; otherwise
    `render_error` says why. Audio is re-mixed on every render. `--summary` prints a human-readable version.
- **Branches** are named snapshots tracked in the journal:
  - `ferrocut branch <tl> <name>` names the current state;
  - `ferrocut checkout <tl> <name>` swaps the file to that branch's tip (refusing to drop unjournaled changes
    without `--force`);
  - `ferrocut merge <tl> <name>` replays the branch's live ops since it forked onto the current branch, atomically
    and rebase-style. An op that no longer applies (e.g. a clip deleted on this branch) fails the merge and nothing
    is written. A merge undoes as one step.

### MCP server (`ferrocut-mcp`)

`ferrocut-mcp` is a stdio [MCP](https://modelcontextprotocol.io) server built on the official Rust SDK
(`rmcp` 3.5, Apache-2.0; every new transitive dependency is MIT and/or Apache-2.0). It is a thin layer over
`ferrocut_engine::{project, diff}`, so tools and CLI behave identically.

| Tool | What it does |
|---|---|
| `timeline_get` | Canonical hash, timeline JSON, duration, frame/chunk counts, flat clip list, journal status |
| `edit_apply` | Apply edit ops atomically (`dry_run`, `plan` for chunks that would re-render, `output`, `return_timeline`); journaled |
| `diff` | The structured diff above, with render impact |
| `plan` | Chunk plan (index, frame range, content key) without decoding or GPU |
| `render` | Incremental render (`jobs` (default 4, lowered to fit free VRAM), `force`, `cache_dir`, `cpu`, `timeout_s`, `deliver: "mp4"` or `{format, output, qp, audio, jobs}`); returns `report_path`, hashes, chunk-reuse stats, `oom_backoffs` and, with `deliver`, a `deliver` section |
| `report_read` | Summary (or `full`) of a render report |
| `quality_check` | Perceptual quality check of a render via `ferrocut-perceive` (below): `status`, `problems`, `warnings`; `render` also takes `check: true` |
| `log`, `undo`, `branch` | Journal log, undo, and branch `create`/`checkout`/`merge` |
| `openh264` | Cisco's OpenH264 binary for delivery: `status`, `enable` (downloads it; only on the user's explicit request), `disable` (`remove` deletes it), `license`; every result carries Cisco's notice |

- **Schemas** are hand-written JSON Schema (`crates/ferrocut-mcp/src/schema.rs`), not derived:
  - each edit op is a `oneOf` branch with `op` as a `const`, its required fields, `additionalProperties: false`
    and an example;
  - rationals are integers or strings matching `^-?[0-9]+(/[1-9][0-9]*|\.[0-9]+)?$`;
  - keyframes, interpolations (presets, `bezier`, `speed`), transforms, fades and the `ripple_insert` clip object
    are spelled out;
  - a test checks that every op kind has a branch, that every example parses with the engine, and that every
    property the schema allows is one the engine accepts.
- **Results** are structured JSON (`structuredContent`, also sent as text). Tool failures, such as an op that
  doesn't apply, a missing file or a render error, come back as `isError` results with
  `{"error": "op 0 (trim nope): ..."}`; an unknown tool is a JSON-RPC invalid-params error.
- **Project root (sandbox).** Every path a tool reads or writes must resolve inside one directory: timelines,
  outputs, reports, cache dirs, and every clip's media source. The root is `--root DIR`, else
  `$FERROCUT_MCP_ROOT`, else the server's working directory, canonicalized at startup. Relative paths are relative
  to the root.
  - An existing path is canonicalized: symlinks are resolved and `..` is folded. A new output's nearest existing
    ancestor is canonicalized and the missing tail appended.
  - Rejected: anything resolving outside the root (`../x`, absolute paths elsewhere, a symlinked file or directory
    that points out), dangling symlinks (writing through one would create its target), and `..` after a missing
    directory.
  - Media sources are checked before anything probes, hashes or decodes them. That includes clips that
    `edit_apply` `ripple_insert`s or a `branch` merge would bring in; this is pre-checked by a dry run, and on
    failure nothing is written or journaled.
  - Limits: this guards against agent mistakes and injected paths. It is not an OS sandbox: a symlink swapped in
    between check and use, or a hard link, is not caught.
- **Progress.** A `render` call whose request carries `_meta.progressToken` gets `notifications/progress`:
  - `progress` is the number of frames done, starting at the frames reused from the cache, then one step each
    for audio, concat and done; `total` is the frame count + 3 (+ 4 with `deliver`, which adds a `deliver`
    step before done);
  - `message` reads like `rendering: 7/48 chunks (2 reused), 84/576 frames`;
  - updates come per finished chunk, strictly increasing, and all are sent before the result.
- **Cancellation.** `notifications/cancelled` for an in-flight call fires the engine's `CancelToken`:
  - the render stops between frames;
  - the chunk being encoded is discarded (chunks are written to a temp file and renamed only when complete);
  - finished chunks stay in the cache, so the next render reuses them.

  Renders run one at a time per server.
- **Libraries:** the binary embeds the same relocatable FFmpeg rpath as `ferrocut` (via `links` metadata from
  the engine's build script), so it needs no `LD_LIBRARY_PATH`.
- **Tests:** `tests/stdio.rs` spawns the server over stdio and runs:
  1. list tools;
  2. `timeline_get`;
  3. a dry-run edit with plan (chunks 4-7), then a real edit;
  4. `log`, `diff`, `plan`, and error cases;
  5. render a 64x32 timeline (12 chunks rendered), render again (12 of 12 reused, same hash);
  6. `report_read`;
  7. `undo`, then render again (only chunks 4-7 re-render).

  `tests/sandbox.rs` checks root resolution and escapes: relative, absolute, symlinked file and directory,
  dangling symlinks, and `..` through missing directories. It also checks that every tool refuses outside
  timelines, outputs, caches, reports and media sources, with nothing written. `tests/progress.rs` covers:
  - `--root`, `$FERROCUT_MCP_ROOT` and the cwd default;
  - a 48-chunk render cancelled after its first progress notifications;
  - the follow-up render: only the chunks finished before the cancel are reused, its progress runs from the
    reused frames to the total, and no temp or partial files are left;
  - a forced render into a fresh cache matching the follow-up render bit for bit.

```sh
cargo build --release -p ferrocut-mcp
./target/release/ferrocut-mcp --list-tools     # prints every tool with its JSON Schema
```

**Register with Codex.** Add this to `~/.codex/config.toml`, or to a trusted project's `.codex/config.toml`.
Codex's default 60 s tool timeout is too short for real renders, hence `tool_timeout_sec`:

```toml
[mcp_servers.ferrocut]
command = "/home/gordontwatts/Documents/projects/ferrocut/target/release/ferrocut-mcp"
# Tools may only touch files under --root (default: cwd); relative paths resolve against it.
args = ["--root", "/home/gordontwatts/Documents/projects/ferrocut"]
cwd = "/home/gordontwatts/Documents/projects/ferrocut"
startup_timeout_sec = 20
# Renders block until done; allow long ones (Codex's default is 60 s).
tool_timeout_sec = 1800
# Prompt before tools not marked read-only (edit_apply, render, undo, branch, openh264).
default_tools_approval_mode = "writes"
```

The CLI equivalent (without the timeouts) is
`codex mcp add ferrocut -- /home/gordontwatts/Documents/projects/ferrocut/target/release/ferrocut-mcp`.
Check it with `codex mcp get ferrocut`, or `/mcp` in the TUI. Any other MCP client works the same way: run the
binary over stdio.

### Quality check hook (`ferrocut check`, eval grader)

`ferrocut_engine::perceive` runs SeePlus's perceptual checker, `ferrocut-perceive` (crate `ferrocut-perceive`;
`cargo build --release` puts it next to `ferrocut`). If the binary is missing, the hook **skips gracefully** with
`status: "skipped"`, never a failure. Graders pass `--require` so that a missing checker counts as an error.

- **Invocation:** `ferrocut-perceive check <render> --timeline <timeline> --json [extra args]`.
  - Exit codes: 0 pass, 1 fail, 2 error.
  - Output is a `ferrocut.perceive.check/1` report, with its schema in
    `crates/ferrocut-perceive/schema/perceive-check.schema.json`. The engine reads exactly these fields:
    - `schema_version`, which must be `ferrocut.perceive.check/1`;
    - `pass`, where `pass == problems.is_empty()`;
    - `problems` (failures) and `warnings` (non-failing findings, kept and surfaced). Each entry is
      `{reason, range: [start, end), measured, threshold}`. `range` holds RationalTime strings such as
      `["1", "25/24"]`; `measured` and `threshold` are numbers or null.
  - Reason codes: `missed_cut`, `extra_cut`, `black_frames`, `frozen_frames`, `flash`, `loudness_off_target`,
    `true_peak_over`, `missing_audio`, `audio_join_mismatch`. Codes added later are kept as-is.
  - Other fields (`severity`, `unit`, `tolerance`, `timecode`, `frames`, `message`, ...) are additive. They're
    passed through verbatim in each problem. The raw report is always kept.
  - Checker defaults: -14 LUFS ±1 LU, true peak ≤ -1 dBTP, and audio required (a render with no audio fails
    `missing_audio`). Other thresholds go through as flags or a `--config` JSON, passed verbatim.
- **Binary lookup:** explicit path, then `FERROCUT_PERCEIVE`, then next to `ferrocut`, then `PATH`.
- **Errors:** these all give `status: "error"`:
  - a different `schema_version`, a missing or mistyped field, or a pass flag that contradicts the problems or
    the exit code;
  - exit 2 (the checker's `error` message is reported), or any other unexpected exit code;
  - malformed JSON;
  - a timeout, which kills the checker's whole process group.
- **Entry points:**
  - `ferrocut check out.mkv --timeline tl.json [--perceive BIN] [--require] [--timeout S] [-- checker flags]`
    prints the outcome JSON and exits 0 on pass (or skipped), 1 on fail, 2 on error. `--require` makes a missing
    checker exit 2.
  - `ferrocut render ... --check [--check-arg X]...` also writes `<output>.check.json` and exits 1 or 2 on
    fail or error.
  - The MCP tools: `quality_check`, and `render` with `check: true`.
- **Tests:** `crates/ferrocut-engine/tests/perceive.rs` and the MCP stdio test use a fake checker that prints the
  real checker's output shapes. SeePlus's crate tests the real checker through `interpret()`.

### Delivery (`render --deliver mp4`)

`ferrocut_engine::deliver` wraps SeePlus's `ferrocut-deliver`: H.264 (Cisco's OpenH264, loaded at run time,
constant QP, BT.709 limited range) + AAC in a faststart MP4, encoded from the finished FFV1 master.

- **IDRs on render chunk boundaries:** the delivery chunk plan is the render report's chunk starts, so every
  delivery chunk starts at a master keyframe and could later be cached per render chunk.
- **Deterministic:** the MP4 is bit-identical for any number of encoder jobs. Encoders are CPU-only; `-j` is
  reused if given, else min(cores, 12).
- **The codec is never fetched implicitly.** It is used only if the user already enabled it (or
  `FERROCUT_OPENH264_LIB` points at a copy). Otherwise delivery fails `Permanent` with instructions. Fetching
  Cisco's binary takes an explicit act: `ferrocut-deliver openh264 enable`, `ferrocut render ...
  --download-openh264`, or the MCP `openh264` tool's `enable`. The library is never vendored or shipped; only
  its license files are (`crates/ferrocut-deliver/OPENH264_BINARY_LICENSE.txt`).
- **CLI:** `ferrocut render tl.json -o out.mkv --deliver mp4 [--deliver-qp 20] [--deliver-output F]
  [--deliver-no-audio] [--download-openh264]` writes `out.mp4`, `out.deliver.json` and a `deliver` section in
  `out.report.json` (`output`, `output_sha256`, `output_bytes`, `frames`, `idr_frames`, `qp`, `jobs`, `encoder`,
  timings). With `--check`, delivery runs only if the check passes. Exit 1 on a permanent delivery error, 75 on
  a retryable one (retried once first). The master is always kept.
- **MCP:** `render` with `deliver`; the result's `deliver` carries Cisco's notice. Cancelling stops before the
  encode starts; a delivery cancelled mid-encode removes its MP4 (the encoder has no cancel hook yet).
- **Tests:** `crates/ferrocut-engine/tests/deliver.rs` and `crates/ferrocut-mcp/tests/deliver.rs`. Without the
  codec they check the clean refusal (nothing downloaded or recorded), cancellation and the sandbox; the encode
  tests (bit-identical across jobs, IDRs at the render chunk starts via ffprobe, progress order) run only with a
  codec the user provided and otherwise print `SKIP`. Nothing in the tests or CI can download OpenH264.

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
  Each frame runs in a `GpuContext::error_scope`, so wgpu OOM becomes an error instead of a panic.
- **Out of GPU memory.** A failed wgpu allocation doesn't fail where it happens: it leaves an
  invalid object, and a later use fails validation ("BindGroup with '' label is invalid"). Wrap
  allocations outside `render` (setup, lazily built caches) in
  `ferrocut_core::with_alloc_scope(&gpu, || ...) -> Result<T, NodeError>`. It pushes OutOfMemory +
  Validation error scopes and maps a failed allocation (including "... is invalid" fallout once
  the device has run out of memory) to `Retryable` + `GpuFault::OutOfMemory`. Any OOM poisons the
  texture pool, so textures leased before it are freed instead of reused. The scheduler doesn't
  retry an OOM frame. It drops the worker, restarts the chunk and lowers the number of chunks in
  flight (`RenderReport.oom_backoffs`, `min_jobs_in_flight`; the output is identical). If even one
  chunk in flight runs out of memory it fails with a clear error (free VRAM, lower the resolution or
  use `--cpu`). The default `--jobs` is `min(cores, 12)`, lowered to what fits in free VRAM where
  NVML reports it (NVIDIA; about 80 bytes per output pixel per job plus 512 MiB headroom). MCP
  `render` defaults to at most 4. `FERROCUT_VRAM_BUDGET_MB` (or `GpuContext::set_memory_budget`)
  simulates a small GPU for tests. Device loss (reported by a node, or seen via wgpu's
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
