# Ferrocut production benchmark: four video classes

Baseline established 2026-10-09. Paths below are relative to this directory;
`out/...` and `docs/...` paths refer to the repository root. Rendered evidence in
`out/` is local and gitignored. Later cycles compare against these dated baselines
without overwriting them. The 2026-10-09 cycle delivered repaired explainer and
caption-heavy social films and the first commercial on installed revision
`bda8a21`; see the [production report](../../../docs/evaluations/2026-10-09-production-cycle.md)
for full hashes, editable packages, technical review scope and remaining limits.

## Matrix

| Class | Benchmark | Brief | Spec | What it stresses | Baseline (2026-10-08, ferrocut 5de8434) |
|---|---|---|---|---|---|
| Educational explainer | `explainer-16x9` | `../claude-videos-20261008/explainer-16x9/BRIEF.md` | 1920×1080, 30 fps, 45.000 s, −16 LUFS | narration-led pacing, captions, diagrams, footage inside graphics | master `03a27acf…`, mp4 `40debd42…`; 1350 f; −16.0 LUFS, TP −1.4; check pass |
| Vertical social | `vertical-9x16` (primary), `vox-style-ferrocut-9x16` (caption-heavy variant) | the same directories | 1080×1920, 24 fps × 30.000 s; 30 fps × 60.000 s; −14 LUFS | 2.35:1 footage in 9:16, hook, captions with plates, safe zones | vertical master `30268ed6…`, mp4 `0b3bb796…`, 720 f, −14.1 LUFS, TP −1.7; vox master `d4b7207c…`, mp4 `35da130f…`, 1800 f, −14.1 LUFS, TP −1.1; both check pass |
| Commercial spot | `commercial-16x9` (new, original, fictional product) | `commercial-16x9/BRIEF.md` (contract: `../claude-videos-20261008/CONTRACT.md`) | 1920×1080, 24 fps, 15.000 s, −16 LUFS | placement by cover, glow compositing over footage, vector product art, callouts locked to parts, supers; optional cut-down | first production 2026-10-09 on bda8a21: master `d4340211…`, mp4 `50815ad2…`, 360 f; native −16.0 LUFS, TP −1.06; explicit-target check pass, automatic −14 target failure retained |
| Motion / compositing | `kinetic-type-1x1` (primary), `trailer-16x9` (editorial and grade) | the same directories | 1080×1080, 30 fps, 24.000 s, −14 LUFS; 1920×1080, 24 fps, 40.000 s, −16 LUFS | text animators, nested comps, 3D layers, speed ramp, dissolves, adjustment grade | kinetic master `5724ea95…`, mp4 `c26ff5d6…`, 720 f, −14.1 LUFS, TP −1.1; trailer master `1212764e…`, mp4 `4490c191…`, 960 f, −16.0 LUFS, TP −1.4; both check pass |

For the five dated baseline films, loudness and true peak are ffmpeg
`ebur128=peak=true` on their delivered MP4s, measured 2026-10-09; full hashes are in
`out/production-cycle-20261009/production/baseline/BASELINE.json`. The new commercial
row labels native-master audio measurements; its master/viewing hashes are in
`out/production-cycle-20261009/production/deliveries/HASHES.sha256`. Earlier judge scores (stills and measurements only, nobody listened)
are in `docs/evaluations/2026-10-08-claude-video-tests.md`. They are context, not acceptance.

## Bounded regression cases (run before any complete film)

| Case | Files | Before (installed 5de8434) | After a fix, expect |
|---|---|---|---|
| caption-gap-blink | `regressions/caption-blink/` (from `out/production-cycle-20261009/production/repro/caption-blink/min/`) (3 cues, 30 fps, gaps containing frames 31 and 73) | measured on the masters: vox has 21 cue-gap dropout groups (caption and plate absent), plus 1 separate plate/contrast dropout at f956 (the caption is active, but plate15 ends at 956/30 s, and the caption fill matches the page), explainer 17 unintended one-frame text blanks (plus 4 authored pauses) | no blank frame at the 1-3 frame gaps (default close-gap threshold 1/10 s; a custom threshold stays explicit). Cue edges are ceiling-snapped to the frame grid and the closures listed. Authored pauses of 4 or more frames (explainer frames 493-503, 610-613, 1039-1044, 1189-1214) stay as they are |
| media-fit | `regressions/media-fit/fit-16x9.json`, `fit-9x16.json` (from `out/.../repro/media-fit/min/`) (t1.mkv 1280×534) | stretched to the frame (aspect ×0.7417 / ×0.2347, reproduced in CPU-rendered stills) | contain: 1920×801 / 1080×450.6 picture, aspect ×1.000 measured from picture bounds |
| production-fit | explainer monitor shots (mon-t1, mon-t5, mon-s4, mon-t6, card2-footage) | 5 of 5 distorted ×0.74–0.76 (static audit; rendered measurement pending) | aspect ×1.000. In trailer and vertical, 28 per-axis compensations become removable |

## Evidence protocol per benchmark run

Keep these separate. None of them substitutes for another.
1. **Source tests**: the engine checks named in CONTRIBUTING.md, run by the engineers.
2. **Render**: the exact command, the ferrocut revision, the master and mp4 sha256, frame count, duration and rate.
3. **Technical QC**: `ferrocut check --require` JSON, ebur128 loudness and true peak, the caption-gap sweep
   (`tools/caption_gap_sweep.py`: rendered coverage of every covered frame, plus the legacy blink frames as probes
   via `--probe-from`; validated by `tools/make_sweep_control.py`). Placement: `tools/placement_audit.py` is
   baseline-only (it models the pre-fit stretch and refuses timelines using `fit`). After the fix, measure the
   placement from rendered frames (picture bounds) and the engine's own placement report.
   The caption sweep measures band contrast, not text presence or legibility: a
   textured picture or plate can conceal missing text. Choose an interior text
   band that excludes plate edges, include a plate-only negative control, and
   inspect actual caption boundaries alongside the result. The Vox wide-band
   false positive and corrected interior result are retained in this cycle.
4. **Frame inspection**: stills at ±1 frame of every cut, caption change and transition, plus a spread sheet.
   Name the frames inspected.
5. **Perceptual acceptance**: continuous playback and listening by Gordon. The agent interface samples stills
   and measures signals. It cannot watch continuously or listen, so this step stays open until Gordon reports it.

Each run records its workaround steps. For example, per-axis scales written by hand, an SRT post-processed to
close gaps, plate clips aligned by hand, or a frame sweep run to find blinks. The before/after count of those
steps is the customer-facing measure of a fix.
