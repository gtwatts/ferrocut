# Audio packet-timebase repair and public regressions

Status: **source checkpoint only**. The synthetic baseline diagnosis is
independently accepted, its nine public files have been imported and rehashed,
and the Rust source is formatted. The new regression target and repair have
**not been compiled or executed**. No repaired render, cache verdict, required
repository-check result, push, installation or listening acceptance is claimed.

The branch preserves preparation `47e4a50d917adb627439c68528c43d2e137d5cba`,
based on `df57e8debad37c48c07dc724971fb5d09c5b1537`. Regression checkpoint
`5e90aa2` leaves the production decoder/cache version unchanged and activates
the same tests and fixture used by the following repair. It provides a frozen
expected-failure source revision for a later leased comparison; those failures
are predictions until the tests actually run.

## Accepted failure and minimal change

The completed public experiment used old engine revision
`73935ba661c0772ca5b464fa6aa9c2a8f89626e3`, binary SHA-256
`38603e7af5a92882a7ba9f8126adb4ccfdc418bdbf6bc9a1042ec0dd3f94c247`.
The sole C-probe treatment, setting the decoder packet timebase to the selected
stream's timebase, changed the first retained tagged MP3 PTS by exactly the
observed 1105/44100 seconds while preserving all decoded sample bytes/counts.
Baseline source PCM and seven program masters matched the timestamp-based
prediction exactly: a second drop of 1203 samples at 48 kHz or 1105 at 44.1 kHz.
WAV, FLAC and genuinely untagged MP3 matched their own references.

For the public fixture this advances content by 25.0625 ms at 48 kHz; it is not
an audio delay. This does not identify the cause or sign of every historical
private-film or mixed-film correlation residual. The independent diagnosis
receipt is locally retained at
`out/production-cycle-20261009/cycle3/review/audio-origin-baseline-diagnosis-accepted.json`,
SHA-256 `32cb0bb29cd570113a447ee10315614d7af70415a043d43727ca14a7b9b9000e`.
The original 175-file manifest is
`77e8bb4aabee897f07084a870655369e61a443dc7a4ebc12124bb2e19b8f44ed`.

The production change calls `set_packet_time_base(ist.time_base())` before
opening `AudioStream`'s decoder. libavcodec continues to remove automatic
priming and trailing discard once. No manual skip, seek, codec-specific offset,
timestamp replacement or new resampling/placement rule is introduced.
`AUDIO_DECODE_VERSION` changes from v1 to
`audio-decode.v2:pkt-timebase:swr-default:f32p:mono-or-stereo` so stale decoded
sources and dependent mixes cannot supply the old shortened content. All decoded
audio identities change once, including lossless controls; video keys do not.

Source zero remains the first video stream timestamp for linked A/V or the
audio stream start for audio-only inputs, with the existing zero fallback for a
missing start or first PTS. Packet timestamps use stream units; skip/discard
counts use source samples. Resampling and alignment precede authored placement.
Authored silence and real A/V offsets are not codec priming.

## Active normal-CI coverage

[`audio_origin.rs`](../../crates/ferrocut-engine/tests/audio_origin.rs) uses the
committed [public fixture](../../crates/ferrocut-engine/tests/data/audio-origin/README.md).
All six integration tests run normally, without an environment variable or
`--ignored`. Inputs and their own same-library references are present in the
repository; no external codec command, download, GPU or optional-tool skip is
used. Missing fixture files, decode/mux failures and unavailable required FFV1
support fail the target. The private `State` unit test is included in normal
engine library tests.

| Control | Required assertion |
|---|---|
| WAV, FLAC and observed untagged MP3 at 48 kHz | Exact decoded length and sample values. Untagged encoder padding is retained. WAV must expose a missing stream start, exercising its real zero-origin fallback. |
| Tagged MP3 at 44.1 and 48 kHz | Exact decoded source length and its own reference values, preserving initial silence and trailing discard. |
| WAV/tagged at 48 kHz; tagged at 44.1 kHz; source-in 1/4, start 1/8, duration 1 | Exact authored program interval and selected reference samples, with silence outside the clip. |
| Tiny real linked A/V with video start 2 s and PCM audio start 21/10 s | Inspect actual muxed stream origins, then retain exactly 4800 leading silent samples before the known PCM at 48 kHz. Check both decode and the linked timeline mix. |
| Decoded frame with missing first PTS | Preserve the zero fallback with zero, positive and negative source origins through the real `State::push/emit/flush` path. No demuxer guess substitutes for this control. |
| Actual old source keys, reconstructed observed bad PCM and old premix/final cache | Correct source key must differ; old caches remain untouched; new source and full mix match reference with zero old premix/final reuse. |
| Warm cache, forced re-decode and a fresh cache, both project rates | Exact PCM bit equality. Read the warm result before forced rewriting; require actual warm premix/final reuse and no forced/fresh final reuse. |

Placement retains the existing independent exact rounding of the program bounds
and `source_in-start` offset. At 48 kHz the test maps source `[12000,60000)`
to program `[6000,54000)`. At 44.1 kHz, `round(R/8)=5513` for both the start
and source offset, so source `[11026,55126)` maps to program `[5513,49613)`.
Independently rounding `source_in*R` would change the existing contract by one
sample; this repair deliberately preserves it.

Decoded/reference comparisons have exact length and f32 numeric equality,
with +0 and -0 treated as the same silence. Cached repeat comparisons require
identical bits. No lag fitting, fixed correction, gain fitting or adjustable
tolerance can hide a timing or value mismatch. Cache invalidation is exercised
with source keys from the actual accepted old run, not a duplicated production
key algorithm. Its legacy sample loss is derived from recorded timestamps,
not assumed to be a universal MP3 constant.

## Imported provenance

The reviewed `scripts/import-audio-origin-fixture.py` was run once, unchanged,
as a copy-only operation. It verifies frozen recipe/run hashes, all 39 command
exits and seven cases, observed tagged/no-tag metadata, and equal probe-arm
bytes/counts. It copied four original synthetic sources and five reference PCM
files (4,273,439 bytes) into `crates/ferrocut-engine/tests/data/audio-origin/`.
Its manifest SHA-256 is
`e450c2c961d65a710bf2c4ccfa2d8bef0d5e6c3a1df97c5691f08b002b40205b`.
All nine copied file hashes/lengths match. It copied no private custody, raw
command paths, native film or client assets and invoked no codec/subprocess.

The public fixture README and manifest preserve the original Apache-2.0 seeded
noise recipe, actual timing metadata and SHA-256 bindings. Existing LAME 4.0
(GNU Library GPL v2-or-later) only encoded the two MP3 controls. Every reference
and probe used the pinned dynamically linked LGPL-only FFmpeg 9.0.2. MP3 is
compared against its own decode, never original WAV values as a lossy oracle.
No dependency or FFmpeg build/license change is part of the repair.

## Planned validation after a separate host-slot grant

No commands in this section have run for the repair. Run one sequential batch,
max four build jobs, using the established project LGPL prefix and a bounded
shared Cargo target only while leased. Preserve stdout, stderr and actual exits.
Do not promote a compilation error to an expected regression failure.

1. Freeze/check both revisions and fixture hashes. In a detached isolated test
   checkout at regression checkpoint `5e90aa2`, run:

   ```sh
   cargo test --locked -j 4 -p ferrocut-engine --test audio_origin -- --test-threads=1 --nocapture
   cargo test --locked -j 4 -p ferrocut-engine --lib media::audio::tests::missing_first_pts_keeps_zero_fallback_and_source_origin -- --test-threads=1 --nocapture
   ```

   Expected old result: lossless/untagged, real linked offset and missing-PTS
   controls pass; tagged decode, nonzero placement and stale-cache rejection
   fail for the documented second drop/key reuse. Unexpected failures remain
   evidence requiring investigation.
2. Run the **same frozen fixture/tests** on the exact repair checkpoint. All
   must pass. Budget after compilation: 30 seconds per test invocation; two
   tiny FFV1 frames per integration invocation, no GPU or external encoder.
   The first compile can dominate runtime: request a bounded 15-minute focused
   slot, build `-j 4`, and report/stop at the agreed boundary rather than overlap.
3. Run required formatting/Clippy/workspace/delivery-unit checks from
   CONTRIBUTING.md. The lead may combine these and the release build with the
   independent checker slice to avoid repeated cold builds. Do not push before
   the required verified milestone.
4. With a separately scheduled exact release CLI/MCP, retain repaired versions
   of the seven original tiny timelines and compare their PCM with the accepted
   references and old masters. On tagged zero/trim fixtures, run cold, warm and
   fresh `-j 1`/`-j 2` renders with multiple chunks; require matching file/PCM
   hashes across worker counts and explicit warm-cache reuse. Bind executable,
   output, report and fixture hashes. Through fresh MCP, retain `plan`, `render`,
   repeat render, root-rejection and clean EOF exit-zero evidence. Use at most
   two render workers and one sequential batch; no shared installation here.
5. Run unchanged `ci/render-check.sh` and retain exact audio hashes, counts,
   determinism and SSIM reports. Keep existing J/L-cut tests. Any required
   combined/install verification remains lead-owned. Playback/listening and
   private-film acceptance are separate from sample measurements.

The original recipe/probe/baseline files and older worktrees remain preserved.
No heavy test, codec, render or install action was authorized by this source
checkpoint itself.
