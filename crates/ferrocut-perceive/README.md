# ferrocut-perceive

The critique loop's eyes and ears: a **perception report** an agent reads
after each render, the way an editor checks scopes, a contact sheet and a
loudness meter before calling a cut done.

- **Scopes** (GPU compute, exact CPU reference): luma waveform, RGB parade,
  vectorscope and histograms per sampled frame, summarized as numbers
  (black/white levels, clipped/crushed %, saturation, mean hue, skin-tone
  line deviation), with optional scope PNGs.
- **Contact sheets**: PNG grids of sampled frames with timecodes, per chunk
  and for the whole timeline.
- **Shots**: hard cuts, dissolves, flash frames, black frames and frozen
  frames, cross-checked against the cuts and dissolves the timeline intends
  (missed / unexpected cuts).
- **Audio**: EBU R128 / ITU-R BS.1770-4 integrated, short-term and
  momentary loudness, loudness range, true peak, silence and clipping spans,
  overall and per chunk. Implemented from the standards (no C dependency).
  Measured from the PCM decoded out of the master, so join errors show up.
  Analysis runs per engine chunk with stored filter state, so a partial
  re-render reproduces a full measure bit for bit.
- **Grading** (`check`): pass/fail plus `problems[]` with stable reason
  codes, exit 0/1/2, for eval harnesses and the engine's `ferrocut check` hook.
- **Issues**: a sorted list of things to fix (`flash_frame`, `missed_cut`,
  `clipped_highlights`, `true_peak_over`, …) so an agent can act without
  interpreting every number.

The report is versioned JSON (`schema_version: "ferrocut.perceive/1"`),
keyed to the engine's output frames and chunks, **byte-identical** for
identical inputs (GPU or CPU, cached or not), and **diffable**: after an edit,
`ferrocut-perceive diff` shows exactly which chunks' picture or sound changed.

Per-chunk analysis is cached under the engine's chunk key, so after a
one-clip edit only the re-rendered chunks are decoded and analyzed again.
Audio is the exception: it is re-analysed from the first changed chunk
forward, until the filter state matches the cache again.

## CLI

```sh
# after `ferrocut render timeline.json out.mkv` (writes out.report.json)
ferrocut-perceive analyze --timeline timeline.json --render-report out.report.json \
    --out perceive/ [--audio master.flac] [--scope-images] [--sample-every 12]
# -> perceive/perceive.json, perceive/sheets/{timeline,chunk-NNNN}.png,
#    perceive/scopes/frame-NNNNNN.png (with --scope-images)

ferrocut-perceive check out.mkv --timeline timeline.json [--brief-cuts cuts.json] [--json]
                                              # grade: exit 0 pass, 1 fail, 2 error (below)
ferrocut-perceive diff before/perceive.json after/perceive.json [--json]   # exit 1 if different
ferrocut-perceive loudness master.flac                                   # EBU R128 of any file
ferrocut-perceive schema [report|check|audio-chunk]                      # JSON Schemas
```

`--cache-dir` defaults to `<render report dir>/.ferrocut-cache`, like the
engine CLI. `--cpu` skips the GPU (same numbers, slower).

## Grading: `check`

For eval harnesses and the engine's hook (`ferrocut check`, `ferrocut render
--check`, MCP `quality_check`, all in `ferrocut_engine::perceive`):

```sh
ferrocut-perceive check <render> --timeline <timeline> [--brief-cuts <file>] [--json] \
    [--config thresholds.json] [--loudness-target -14] [--loudness-tolerance 1] \
    [--true-peak-max -1] [--cut-tolerance-frames 1] [--max-black-frames 0] \
    [--max-edge-black-s 2] [--max-frozen-s 2] [--max-flash-frames 0] \
    [--allow-no-audio] [--no-cut-check] [--render-report F] [--cache-dir D] [--out D] [--cpu]
```

- `<render>`: the master (`out.mkv`, report read from `out.report.json`) or
  the render report itself. The perception analysis runs (or is reused from the
  cache, found through the report's `chunk_dir`), and audio is decoded from
  the master.
- **Exit codes:** 0 = pass, 1 = fail, 2 = error (bad arguments, unreadable
  inputs, analysis failure).
- **Output:** with `--json`, one JSON object on stdout; without it, a human summary.
- **Thresholds:** defaults, overridden by `--config` (JSON, any subset of the
  `thresholds` keys below; unknown keys are an error), overridden by flags.
- **`--brief-cuts`:** the brief's expected cut times replace the timeline's hard
  cuts. The file is a JSON array, or `{"cuts": [...]}`, of rational strings
  (`"121/24"`), numbers (seconds), `HH:MM:SS:FF` timecodes or `{"time": ...}`
  entries; or plain text, one per line, with `#` comments.

**JSON (`ferrocut.perceive.check/1`, `schema/perceive-check.schema.json`):**

```json
{
  "schema_version": "ferrocut.perceive.check/1",
  "pass": false,
  "problems": [
    { "reason": "black_frames", "range": ["2", "9/4"], "measured": 6.0, "threshold": 0.0,
      "severity": "error", "unit": "frames", "timecode": ["00:00:02:00", "00:00:02:06"],
      "frames": [48, 54], "message": "6 black frame(s) mid-program" }
  ],
  "warnings": [ ... same shape, severity "warning" ... ],
  "thresholds": { "loudness_target_lufs": -14.0, "loudness_tolerance_lu": 1.0, ... },
  "measured": { "frames": 96, "fps": "24", "duration": "4", "integrated_lufs": -23.0,
                "true_peak_dbtp": -23.0, "expected_cuts": [24, 25, 48, 54],
                "detected_cuts": [24, 25, 48, 54], "cuts_from": "timeline" },
  "perceive_schema_version": "ferrocut.perceive/1"
}
```

The contract, matching the engine hook (60b5652):

- Top level: `schema_version`, `pass` (bool) and `problems`.
- Each problem: `reason`, `range: [start, end]` and `measured` / `threshold`
  (numbers, or `null` when there is nothing to measure).
  - `range` is an end-exclusive pair of ferrocut-types `RationalTime`s in their
    serde form (`"num/den"` or `"num"`, seconds).
- Everything else is additive.
- `problems` holds failures only, so `pass == problems.is_empty()` and the exit
  code agrees with `pass`.
- Observations under the thresholds go to `warnings`.
- On exit 2 the object is `{schema_version, pass: false, problems: [], error}`.

| `reason` | fails when | `measured` / `threshold` (unit) | `range` |
|---|---|---|---|
| `missed_cut` | an expected cut (brief or timeline) has no detected cut within ±`cut_tolerance_frames` | shot-change distance at that frame (or null) / `cut_min` (distance) | the expected frame |
| `extra_cut` | a detected hard cut matches no expected cut (cuts bounding a flash or inside a dissolve excepted) | its distance / `cut_min` | the cut frame |
| `black_frames` | mid-program black run > `max_black_frames`, or black at the start/end > `max_edge_black_s` | frames / frames, or s / s | the run |
| `frozen_frames` | identical-frame run > `max_frozen_s` | s / s | the run |
| `flash` | flash (≤ 2-frame shot between matching shots) longer than `max_flash_frames` | frames / frames | the flash |
| `loudness_off_target` | `|integrated − target| > tolerance`, or no signal above the gates | LUFS / target LUFS (+ `tolerance` LU) | whole program |
| `true_peak_over` | true peak > `true_peak_max_dbtp` | dBTP / dBTP | first to last chunk over |
| `missing_audio` (additive) | no audio and `require_audio` | null / null | whole program |
| `audio_join_mismatch` (additive) | a chunk's decoded master PCM differs from the engine's `audio_blake3` | null / null | the chunk |

Defaults: -14 LUFS ±1 LU, true peak ≤ -1 dBTP, cut tolerance ±1 frame, no
mid-program black, ≤ 2 s black at the edges (fades), frozen ≤ 2 s, no flash
frames, audio required. Library: `check::grade(&report, &timeline, brief, &thresholds)`.

## Library

```rust
let (report, stats) = ferrocut_perceive::analyze(Request {
    timeline: &Timeline::load(tl_path)?,          // ferrocut_perceive::input mirror types
    render: &RenderReport::from_json(&engine_report_json)?,
    cache_dir: &cache_dir,                        // the engine's cache dir
    out_dir: &out_dir,                            // images go here; report paths are relative to it
    audio: AudioInput::from_render(&rr, report_dir), // the master; or File(path) / Buffer(AudioBuffer)
    options: Options::default(),
    gpu: Some(&gpu),                              // or None
})?;
std::fs::write(out_dir.join("perceive.json"), report.to_json())?;
```

This crate does **not** depend on `ferrocut-engine` (only as a dev-dependency
for end-to-end tests); it reads the engine's render report, chunk masters
(`<chunk_dir>/<key>.mkv`, FFV1 BGRZ, per GPU adapter) and timeline JSON through mirror
types in `input.rs` that ignore unknown fields. That keeps the dependency
arrow free for the engine to call perceive after each render.

### Audio adapter

- **`AudioInput::Master(path)`** (what `from_render` returns): the engine's
  master MKV. Its PCM f32 track is decoded, so what is measured is what was
  muxed. Each chunk's samples are hashed and compared with the render report's
  per-chunk `audio_blake3`; a difference is an `audio_join_mismatch` error.
- **`AudioInput::File(path)`:** anything FFmpeg (the project's LGPL build) can
  read, with no hash check.
- **`AudioInput::Buffer`:** interleaved f32 PCM in memory, with BS.1770 channel
  weights (L/R/C 1.0, LFE 0, surrounds 1.41 for 5.1).
- **Sample 0** is timeline time 0 in every case.
- **Chunk ranges:** analysis runs per engine chunk on the engine's own sample
  ranges, `[sample_at(start_frame), sample_at(start_frame + frames))` with
  `sample_at(f) = round(f / fps × rate)` (exact rational, halves away from
  zero), clamped to the decoded length. See "Audio chunk cache" below.
- **Engine cross-check:** when the render report carries the engine's own
  measurement, `audio.engine` holds it and the deltas. A disagreement above
  0.1 LU / 0.2 dB is an `audio_measure_disagrees` warning.

## Report (schema summary)

Full contract: [`schema/perceive-report.schema.json`](schema/perceive-report.schema.json)
(JSON Schema 2020-12; every object is closed). Frames are output frame
indices (frame *i* is shown at *i*/fps); times are exact rationals in
seconds as strings (`"3/2"`); timecodes are non-drop `HH:MM:SS:FF`.

| field | contents |
|---|---|
| `schema_version`, `generator` | `ferrocut.perceive/1`, `ferrocut-perceive <version>` |
| `timeline` | name, size, fps, total frames, chunk size, duration |
| `settings` | sampling stride, scope images on/off, thumbnail width, every shot threshold |
| `summary` | frames, sampled frames, levels (all frames), color (sampled), shot/cut/dissolve/missed/unexpected counts, flash/black/frozen frame counts, integrated LUFS, true peak, issue counts |
| `issues[]` | `{severity, kind, frame, end_frame, timecode, chunk, message}`, errors first, then by frame |
| `shots` | `cuts[] {frame, distance, intended}`, `dissolves[] {start, end, fit, intended}`, `missed_cuts[] {frame, distance}`, `unexpected_cuts[]`, `missed_dissolves[]`, `unexpected_dissolves[]`, `flash_frames[]`, `black[]`, `frozen[]`, `shots[]` (spans are `[start, end)`) |
| `audio` | source, rate, channels, duration, loudness (below), deltas to -23 LUFS (EBU R128) and -14 LUFS (streaming), short-term loudness per second, `engine` (the engine's measurement + deltas, or null), `join_mismatch_chunks` |
| `contact_sheet` | `sheets/timeline.png` |
| `chunks[]` | index, frame range, times, timecode, engine `key`, `analysis_key`, levels, color, motion, black/frozen frame counts, `samples[]` (frame, time, timecode, levels, color, scope image), `events[]` (shot events starting in the chunk), `audio` (loudness for the chunk's time range), contact sheet |

`levels`: `luma_black`/`luma_white` (1st/99th percentile, 0..1 codes),
`luma_mean`, `luma_median`, `luma_clipped_pct` (Y' ≥ 253),
`luma_crushed_pct` (Y' ≤ 2), and per-channel `rgb_black/white/mean/clipped_pct/crushed_pct`.
`color`: `chroma_mean`, `chroma_p95`, `saturated_pct` (chroma > 0.35),
`hue_mean_deg`, `skin_pct`, `skin_line_dev_deg` (signed deviation from the
123° skin-tone line). Loudness: `integrated_lufs`, `loudness_range_lu`,
`momentary_max_lufs`, `short_term_max_lufs`, `true_peak_dbtp`,
`sample_peak_dbfs`, `silence[]`, `clipping[]` (`null` = nothing above the gates).

Versioning: removing, renaming or changing the meaning of a field bumps
`schema_version`; adding optional fields doesn't. `Report::from_json`
rejects other versions.

### Diff

`diff(a, b)` (and `ferrocut-perceive diff`) matches chunks by index and
reports `changed_chunks`, per-chunk changes (`rerendered` when the engine's
chunk key changed) and timeline-level changes, each as a JSON Pointer with
old and new values. Records in arrays (issues, events, cuts, spans, samples)
are aligned by identity (kind + frame), so one new issue is one change.

## How it works

**Scopes.** Each chunk master is decoded once. A compute shader bins every
frame (integer atomics; BT.709 Y'CbCr on encoded values, as broadcast scopes
do): 4×256 histograms and a 32×18 thumbnail on every frame, plus waveform,
parade and vectorscope on sampled frames (every `sample_every` frames,
default one per second, plus each chunk's first and last frame). The same
integer math runs on the CPU as the reference and fallback; a test asserts
the GPU counts are identical. GPU memory is a frame buffer plus ~1.3 MB.

**Shots.** Frame distance = mean of the thumbnail L1 (/255) and the luma
histogram earth mover's distance. A **cut** is `d > 0.10` and `d > 4 ×`
the local median (±6 frames). A **dissolve** is a run of frames whose `d`
exceeds `max(0.008, 1.5 ×` the local 25th percentile) (motion raises the
bar), at least 3 frames long, whose end points differ by more than a cut,
and whose middle frames are a least-squares blend of the end points
(residual ≤ 25 % of the end-point distance, in encoded or linear light). A
**flash** is a shot of ≤ 2 frames between two cuts whose outer frames match.
**Black**: ≥ 98 % of pixels at Y' ≤ 20. **Frozen**: ≥ fps/2 consecutive
identical (hashed) non-black frames. Detected and intended boundaries match
within ±1 frame. Limits: dissolves longer than ~2 s over heavy motion can
be missed (reported as `missed_dissolve`, a soft signal); a `missed_cut`
with a distance near 0 is usually a split of continuous footage, not an error.

**Audio.** K-weighting from the analog prototype (exact at any sample
rate), 100 ms sub-blocks, momentary 400 ms / short-term 3 s windows, gating
at -70 LUFS absolute and -10 LU (integrated) / -20 LU (LRA) relative, LRA =
P95 − P10 of short-term (only windows of complete sub-blocks count, as in
libebur128). True peak uses libebur128's interpolator (49-tap Hann-windowed
sinc, 4× below 96 kHz, 2× below 192 kHz, f32 outputs, max with the sample
peak), so it matches the engine's `ebur128` figures.
Silence: sample peak < -60 dBFS for ≥ 0.5 s. Clipping: ≥ 3 consecutive
samples at |x| ≥ 0.999. Verified against EBU Tech 3341 (cases 1 and 3) and
3342 (LRA) signals.

**Determinism.** Integer pixel math, floats rounded before serialization
(no `-0`), no timings or absolute paths in the report (run statistics are
returned separately), fixed ordering everywhere, deterministic PNG encoding.

**Cache.** `<cache_dir>/perceive/v1/<analysis_key>.{json,thumbs.png,scope-NNNNN.png}`
where `analysis_key` = blake3(analysis version, engine chunk key, frame
range, sampling, image settings). Audio: `<cache_dir>/perceive/v1/audio/<key>.json`,
one per engine chunk (next section).
Bump `ANALYSIS_VERSION` when per-chunk results would change for the same frames.

## Audio chunk cache (format for the normalizer)

One entry per engine chunk (`schema/perceive-audio-chunk.schema.json`,
version `ferrocut.perceive.audio-chunk/1`). It is designed so that a partial
re-render gives **bit-exactly** the same program figures as a full measure.

**Grid.**
- Sub-block `k` covers timeline samples `[floor(k·rate/10), floor((k+1)·rate/10))`,
  anchored at timeline sample 0, not at the chunk.
- A chunk `[start, end)` stores one *piece* per sub-block it intersects:
  `{block: k, start: max(block start, start), n, energy[c], sample_peak, true_peak}`.
  The first and last pieces are usually partial.
- `energy[c]` is Σ y² of channel `c`'s K-weighted signal over the piece's
  samples, accumulated in sample order starting from 0.0 in f64. It is
  unweighted and not divided by `n`.

**Filters** (per channel, f64, direct form I:
`y = b0·x + b1·x1 + b2·x2 − a1·y1 − a2·y2`, then `x2=x1, x1=x, y2=y1, y1=y`).
- Stage 1: high shelf from `k = tan(π·1681.974450955533/rate)`, `Vh = 10^(3.999843853973347/20)`,
  `Vb = Vh^0.4996667741545416`, `Q = 0.7071752369554196`, `a0 = 1 + k/Q + k²`, with
  - `b = [(Vh + Vb·k/Q + k²)/a0, 2(k² − Vh)/a0, (Vh − Vb·k/Q + k²)/a0]`
  - `a = [1, 2(k² − 1)/a0, (1 − k/Q + k²)/a0]`
- Stage 2 (input = stage 1's output): RLB high-pass from
  `k = tan(π·38.13547087602444/rate)`, `Q = 0.5003270373238773`, with
  `b = [1, −2, 1]` and `a` as above.
- `x` is the f32 sample widened to f64.

**True peak** (libebur128's interpolator).
- Factor 4 below 96 kHz, 2 below 192 kHz, else none (true peak = sample peak).
- 49 taps `h[j] = sinc((j − 24)/factor) · 0.5(1 − cos(2πj/48))`, keeping only
  `|h[j]| > 1e-6`. Tap `j` belongs to phase `j % factor` at input lag `j / factor`.
- Each phase output is Σ `history[newest − lag] · h[j]` in increasing `j`,
  accumulated in f64 and rounded to f32.
- The peak of a sample is max(|x|, |outputs|). The history (the last
  `ceil(49/factor)` samples, so 13 at 48 kHz) is updated with the sample
  before computing its outputs.

**Edge state** (`state_out`, the next chunk's input; `EdgeState::initial` at sample 0 is all zeros).
- `{sample, channels: [{shelf: [x1, x2, y1, y2], highpass: [x1, x2, y1, y2], tp_history: [oldest … newest]}]}`.
- Floats are stored as IEEE-754 bit patterns in lowercase hex
  (`format!("{:016x}", f64.to_bits())`, 8 digits for f32), so round trips are exact.
- **State digest:** blake3 over `"ferrocut.perceive.audio-state/1\0"`, then
  `sample` (u64 LE), then per channel the 4 shelf and 4 highpass values (u64 LE
  of `to_bits`), the history length (u32 LE) and the history (u32 LE of `to_bits`).
- **Cache key:** blake3 over `"ferrocut.perceive.audio-chunk/1\0"`, rate (u32
  LE), channels (u16 LE), start and end (u64 LE), `pcm_blake3` (hex text),
  `\0`, and the incoming state digest (hex text).
  - `pcm_blake3` is the blake3 of the chunk's interleaved f32 LE samples:
    the same bytes as the render report's `audio_blake3`.
- **Clip runs:** runs of ≥ 3 samples at |x| ≥ 0.999, plus shorter runs touching
  either chunk edge. Assembly joins runs that continue across an edge, then
  drops runs still shorter than 3.

**Forward propagation.** Because the key includes the incoming state,
editing chunk N gives N a new key and a new `state_out`, which changes N+1's
key, and so on. Re-analysis continues chunk by chunk until a chunk's
`state_out` is bit-identical to the cached one. From there every key is
unchanged and the cached tail is reused; otherwise it runs to the end.
- On continuous audio the two IIR states typically stay a few ulps apart,
  so propagation runs to the end. That is cheap: two biquads and a short FIR
  per sample, with no video decode.
- Digital silence lets the states decay to identical bits, and propagation
  stops there. In the test that took most of a 6 s silence.

**Assembly** (`audio::assemble`, chunks in order, each `state_in` = its
predecessor's `state_out` digest, first chunk at sample 0).
- Pieces with the same `block` merge left to right: per-channel energies
  added (`e += piece.e`), `n` summed, peaks maxed.
- Block energy = Σ_c `weight[c] · e[c]`, summed over channels in order from 0.0.
- From the merged blocks:
  - momentary windows: 4 blocks; short-term: 30 blocks;
  - only windows ending on a complete block count;
  - `lufs = −0.691 + 10·log10(Σ energy / Σ n)`;
  - integrated loudness: −70 LUFS absolute gate, then −10 LU relative gate
    over momentary windows;
  - LRA: −20 LU relative gate, P95 − P10 of the short-term windows.
- A chunked analysis equals a single pass to ~1e-15 relative (the energy sums
  split at edges). Partial vs. full with the same chunk plan is bit-exact.

## Tests

`cargo test -p ferrocut-perceive`: GPU counts == CPU counts (4 frame sizes);
EBU R128 known answers (1 kHz at -23 dBFS reads -23.0 LUFS at 48 and 44.1
kHz, through a WAV and FFmpeg too; 3341 case 3; 3342 LRA; inter-sample true
peak; silence/clipping spans); shot detection on synthetic features; and an
end-to-end test that renders a timeline with known cuts, a flash frame,
black frames, a dissolve, a frozen shot and an overlay through the engine,
then checks detection against the plan, byte-identical reports (cached vs.
fresh, GPU vs. CPU, library vs. CLI), schema validity, and that editing one
clip changes exactly one chunk in the diff (video and audio caches), with
every chunk's decoded audio matching the engine's `audio_blake3` and our
loudness within 0.1 LU of the engine's.

**Audio chunks:**
- Partial re-analysis after a one-chunk edit is bit-identical to a cold full
  analysis: every piece, every edge state, every block and the summary.
- Re-analysis runs from the edited chunk to the first chunk whose state
  re-converges, or to the end.
- Chunked analysis equals a single pass.
- Clip runs join across edges.
- The cache round-trips bit-exactly.

**`check`** (CLI, JSON validated against the check schema, deserialized with
the engine hook's field names and run through the hook's own `interpret`):
- pass on a clean render;
- fail on flash, mid-program black and quiet audio;
- the same render passes with thresholds from flags and from a config file;
- brief cuts give missed and extra cuts;
- true peak over;
- error exits (2).

**demo-av:** renders `examples/demo-av.json` and checks the agreed known answer
(-14.00 LUFS, -2.18 dBTP), so it passes the loudness checks. It skips without
the demo media.

GPU tests skip without an adapter.
