# ferrocut-deliver

H.264/AAC **MP4 delivery** from the engine's lossless FFV1 master:

```
ferrocut render timeline.json -o out.mkv          # Rusty's engine: FFV1 master
ferrocut-deliver encode out.mkv                   # -> out.mp4 + out.deliver.json
```

* **Video:** H.264 Main profile (CABAC, no B-frames), Cisco's OpenH264
  2.6.0 **downloaded at runtime** (never bundled or built by us), loaded with
  `dlopen` behind a small C shim.
* **Chunk-parallel, deterministic:** the master is cut into chunks (default
  2 s, or the render's own chunk layout); each chunk is decoded by its worker
  (seek to the keyframe at or before its start) and encoded by a fresh,
  single-threaded encoder at **constant QP** (rate control off, no frame
  skipping, no scene-change/background detection, no adaptive quant,
  constant SPS/PPS ids). Every chunk therefore starts with an IDR carrying
  byte-identical SPS/PPS and shares no state with its neighbours, so chunks
  join **without re-encoding** and any job count gives the same bytes, run
  after run (same machine; OpenH264's SIMD dispatch is CPU-specific).
* **Audio:** AAC-LC from FFmpeg's **native `aac` encoder** (part of the LGPL
  libavcodec in Rusty's build; no libfdk, no GPL), 192 kb/s by default,
  bit-exact flags. Opus was not needed.
* **Container:** MP4 with `movflags=+faststart` (moov before mdat) and
  `AVFMT_FLAG_BITEXACT`; the H.264 samples are the chunks' access units with
  SPS/PPS moved into avcC.
* **Color:** master space (default "Camera Rec.709", what the compositor
  writes) → Rec.709/BT.709 via `ferrocut-colorspace` (exact pass-through
  when the master already is that space; anything else, e.g. an ACEScg
  master, goes decode → matrix → BT.709 OETF and clips to [0,1]), then
  BT.709 Y'CbCr (Kr 0.2126, Kb 0.0722), **limited range** (Y' 16-235,
  C 16-240), 4:2:0 with **left** chroma siting ([1 2 1]/4 horizontally on
  each row of the pair, averaged vertically). The SPS VUI and the MP4 say
  so: primaries 1 / transfer 1 / matrix 1 / `video_full_range_flag` 0, SAR
  1:1; no `chroma_loc_info` = type 0 (left) by the spec.

## OpenH264: licensing model (read this before shipping)

Cisco pays the MPEG LA H.264 royalties for **its own binary only**, under
the conditions in [`OPENH264_BINARY_LICENSE.txt`](OPENH264_BINARY_LICENSE.txt)
(verbatim copy of <http://www.openh264.org/BINARY_LICENSE.txt>):

| Cisco condition | How ferrocut-deliver meets it |
|---|---|
| 1. Binary downloaded separately to the user's device, not combined with our software first | Downloaded from `ciscobinary.openh264.org` at runtime into the user cache; never in our repo, packages or builds. |
| 2. User can enable / disable / re-enable it | Nothing is downloaded until the user opts in (`ferrocut-deliver openh264 enable` or `encode --download-openh264`; `Provider::enable`). `openh264 disable [--remove]` blocks every load, even with a cached copy, until re-enabled. |
| 3. Show "OpenH264 Video Codec provided by Cisco Systems, Inc." where users control it | Printed by `openh264 status/enable/disable`, on every download and after every encode; `openh264::NOTICE`; in the delivery report (`openh264.notice`). **UI work for the app: show it next to the H.264 export toggle.** |
| 4. Reproduce the licence text where licensing info is presented | `ferrocut-deliver openh264 license` prints it; `openh264::BINARY_LICENSE`; this README. **The app's About/licences screen must include it.** |

Cisco's licence also notes that commercial content providers/broadcasters may
need their own MPEG LA licence; that is the end user's concern, not the
software's, but the app should surface the text.

Our code stays Apache-2.0: the shim is compiled against Cisco's **BSD-2 API
headers** (vendored in `third_party/openh264/` with their LICENSE), and no
OpenH264 code is linked or redistributed.

### Why our own dlopen shim and not FFmpeg's libopenh264 wrapper

FFmpeg's `libopenh264` encoder links OpenH264 at build time (no dlopen).
Using it would mean (a) rebuilding Rusty's LGPL FFmpeg with
`--enable-libopenh264`, (b) every FFmpeg load then requiring the Cisco
library to be present (it cannot be optional or downloaded later, breaking
conditions 1-2 for everyone who never exports H.264), and (c) less control
over determinism-relevant knobs (threading, RC, IDR/SPS behaviour). The C
shim is ~200 lines; struct layouts come from Cisco's own headers via the C
compiler (no hand-transcribed FFI structs).

### Integrity

Cisco publishes only an **MD5** of each decompressed library
(`<lib>.signed.md5.txt`), no SHA-256. `openh264::PINS` therefore pins, per
platform (linux x86_64/aarch64, macOS arm64/x86_64, Windows x64/arm64):
`.bz2` size + SHA-256, library size + SHA-256, and Cisco's MD5 (all six
verified against Cisco's MD5 files on 2026-10-07). A download must match all
of them **and** Cisco's live `.signed.md5.txt` must still equal the pinned
MD5; the cached library is re-hashed on every load. Cisco serves plain HTTP
(Firefox's GMP download uses the same host); the pins make that safe.
A bad hash is a **Permanent** error and nothing is cached; network failures
are **Retryable**.

### Locations and overrides

| What | Default / variable |
|---|---|
| Cache | `$XDG_CACHE_HOME/ferrocut/openh264/2.6.0/` (`~/.cache/...`), `~/Library/Caches/ferrocut/openh264` (macOS), `%LOCALAPPDATA%\ferrocut\cache\openh264` (Windows); `FERROCUT_OPENH264_CACHE` |
| Offline override | `FERROCUT_OPENH264_LIB=/path/to/libopenh264-2.6.0-linux64.8.so` (must be Cisco's pinned file; `FERROCUT_OPENH264_UNVERIFIED=1` accepts any 2.6.x build, which Cisco's patent licence then does not cover) |
| Mirror | `FERROCUT_OPENH264_URL` (same file names as Cisco's host) |
| Proxy | `http_proxy` / `no_proxy` (loopback is never proxied) |

## API

```rust
use ferrocut_deliver::{deliver, default_output, report_path, ChunkPlan, DeliverOptions};
use ferrocut_deliver::openh264::Provider;

let mut provider = Provider::from_env()?;     // honours the env vars above
provider.allow_download = user_said_yes;       // consent = UserChoice::Enabled
let mut opts = DeliverOptions::new(provider);  // QP 20, jobs min(cores,12), 2 s chunks, AAC 192k
opts.chunks = ChunkPlan::Starts(render_chunk_starts);  // optional: align IDRs to render chunks
let report = deliver(&master_mkv, &default_output(&master_mkv), &opts)?;  // -> out.mp4
std::fs::write(report_path(&report.output), serde_json::to_string_pretty(&report)?)?;
```

Errors are `ferrocut_types::error::NodeError` with the engine's kinds:
missing/disabled/bad-hash codec, bad input, bad settings → `Permanent`;
network/disk hiccups and OpenH264 allocation failures → `Retryable`.
`Provider::{status, enable, disable}` back a settings UI.

## CLI

```
ferrocut-deliver encode <MASTER.mkv> [-o OUT.mp4] [--qp 20] [-j N]
        [--chunk-frames N | --align-render-chunks] [--master-space NAME]
        [--audio-bitrate 192000 | --no-audio] [--download-openh264]
        [--keep-chunks] [--json]
ferrocut-deliver openh264 status [--json] | enable | disable [--remove] | license
```

Output naming follows `ferrocut render`: `out.mkv` → `out.mp4`, report
`out.deliver.json` (like `out.report.json` / `out.check.json`). Exit codes:
0 ok, 1 permanent failure, 75 (EX_TEMPFAIL) retryable failure, 2 usage.

## Measured (watts, RTX 5090 laptop / 24 threads, release build)

`demo-av.mkv` (1920×1080, 24 fps, 312 frames, 13 s, PCM stereo): QP 20,
7 chunks, **3.5 s at -j 12** (≈ 89 fps), 9.5 s at -j 1; 9.2 MB
(5.5 Mb/s video + 187 kb/s AAC). Two -j 12 runs and the -j 1 run are
byte-identical (sha256 `b2879196…52ac`). Quality vs the master, against
swscale's left-sited BT.709-limited conversion of it: PSNR Y 53.2 / Cb 48.1 /
Cr 46.7 dB, Y'CbCr SSIM 0.9970;
in RGB the demo's saturated graphics are 4:2:0-bound (R/B ≈ 33-34 dB even at
QP 1, within 0.7 dB of swscale's own left-sited 4:2:0 round trip with no
codec).

## Integration proposal for the engine (Rusty)

No engine changes are made by this crate. Suggested wiring:

1. **Dependency:** `ferrocut-engine` → `ferrocut-deliver` (deliver depends
   only on types + colorspace + the same `ffmpeg-next` features, so no
   cycle and no FFmpeg feature unification). Alternatively shell out to the
   `ferrocut-deliver` binary like the perceive hook (`FERROCUT_DELIVER`,
   next to `ferrocut`, then PATH) and read `out.deliver.json`.
2. **CLI:** `ferrocut render <TIMELINE> -o out.mkv --deliver mp4
   [--deliver-qp 20] [--download-openh264]` → after the master is written,
   `deliver(&output, &output.with_extension("mp4"), &opts)` and write
   `output.with_extension("deliver.json")`; add a `deliver` section (path,
   sha256, bytes, ms) to `RenderReport`.
3. **Chunks:** pass `ChunkPlan::Starts(report.chunks.iter().map(|c| c.start_frame))`
   so IDRs sit on render chunk boundaries (the FFV1 master's keyframes are
   exactly there, so worker seeks never decode extra frames). This also
   opens incremental delivery later: a delivery chunk's bytes depend only on
   its frames + settings, so it can be cached by the render chunk `key`.
4. **Jobs:** reuse `-j`; encoders are CPU-only, so after the GPU render
   phase they can use all cores. `RenderOptions::cancel` → check between
   chunks (proposal: add a `cancel: Option<CancelToken>` to
   `DeliverOptions`; small follow-up on my side when you want it).
5. **Errors:** map `NodeError` kinds as for nodes: `Permanent` (codec not
   enabled/installed, bad hash) fails the delivery with its message (it
   tells the user exactly what to run); `Retryable` may be retried once.
6. **UI/MCP:** expose `openh264 status/enable/disable` and show
   `openh264::NOTICE` beside the H.264 export toggle; include
   `openh264::BINARY_LICENSE` in the licences screen (conditions 3-4).
7. **Packaging:** never ship the `.so/.dylib/.dll`; ship the crate's
   `OPENH264_BINARY_LICENSE.txt` and `third_party/openh264/LICENSE`.

## Tests

`cargo test -p ferrocut-deliver` (unit + `tests/deliver.rs`): quality vs the
master (luma PSNR ≥ 40 dB, Y'CbCr SSIM ≥ 0.97 against an independent BT.709
reference; RGB PSNR within 2 dB of the ideal 4:2:0 round trip); IDR exactly
at every chunk start, byte-identical SPS/PPS in every chunk and in avcC,
BT.709/limited VUI read back from a raw chunk bitstream; parallel (-j 4) ==
serial (-j 1) == second run, and the MP4's samples are exactly the chunk
bitstreams' access units (joined without re-encoding); missing / corrupt /
unpinned / bad-hash binaries → Permanent (bad downloads are never cached);
unreachable host → Retryable; disable / re-enable; the offline override with
no cache and no network; a real download from Cisco into a fresh cache; CLI
exit codes and licence output. Tests needing the codec or the network print
`SKIP` and pass when offline; the codec comes from `FERROCUT_OPENH264_LIB`
or a one-time download into `target/tmp`.
