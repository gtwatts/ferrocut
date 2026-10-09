# Selected-range demo: run request (not yet executed)

Synthetic, original fixture. The only external asset is the repository's own test font,
NotoSans-Regular.ttf (`crates/ferrocut-engine/tests/data/text/`).

Scope: one native 6 s sequence at 24000/1001 fps (144 frames, 640x360). Chunks are 24 frames.
The range is frames [30, 110), so neither end is on a chunk boundary.

Run from the repo root with the branch's built binaries `F=target/release/ferrocut`. Everything is
written to a new `out/selected-range-demo/` directory.

1. `python3 eval/creative/selected-range-demo/make_fixture.py out/selected-range-demo/fx && cp crates/ferrocut-engine/tests/data/text/NotoSans-Regular.ttf out/selected-range-demo/fx/`
2. Captions, through normal edits:
   `$F captions import out/selected-range-demo/fx/timeline.json out/selected-range-demo/fx/captions.srt --style out/selected-range-demo/fx/caption-style.text-style -o out/selected-range-demo/fx/project.json`
3. Full master (the reference), then the range in the same cache:
   `$F render .../project.json -o .../full.mkv -j 4`
   `$F render .../project.json -o .../part.mkv -j 4 --range-frames 30..110`
   - Expect part.report.json `range.source_frames=[30,110]` and `total_frames=80`.
   - Expect chunks [30,48) and [96,110) RENDERED, and [48,72) and [72,96) reused, with the same keys as full's chunks 2 and 3.
4. The same range given as rational time: `--range-time 1001/800..4.6` gives [30,111). Its report must show the time-to-frame rule.
5. Caption sidecar: `$F captions export .../project.json --track Captions -o .../part.srt --range-frames 30..110`.
   - Expect "Straddles the range start" at 0 → 0.44875 s (1.7 − 1.25125).
   - Expect "Straddles the range end" from 2.94875 s to the file end.
6. Identity and time matching through existing outputs (MCP):
   - `artifact_frames {path: part.mkv, frames: [0,1,79,-1]}` and `artifact_frames {path: full.mkv, frames: [30,31,109]}`. Pixel PNG blake3s must match pairwise.
   - `preview_frames {timeline: project.json, frames: [30,31,109], each: true}` gives the native-render side. The same frames' PNG hashes must equal the decoded FFV1 frames.
   - PCM: compare part.mkv's f32le stream with full.mkv samples [fs(30), fs(110)) byte for byte, using the LGPL `third_party/ffmpeg-lgpl/bin/ffmpeg -f f32le`. Both pulses (1.15 s and 4.50 s) cross the range edges.
7. Boundary edit that invalidates only its chunk: an `edit_apply set_param` on clip `count4` text (frames 96..120), then re-render the range. Expect only [96,110) to re-render; [30,48) and the interior are reused. `undo` restores the earlier part.mkv final_blake3.
8. Legacy no-range control: re-render full.mkv. Expect all chunks reused, final_blake3 unchanged, and no `range` in the report.
9. Visual: the calling agent inspects the inline sheets from step 6 and states the observed mover, counter and caption at the range edges. Playback and listening are NOT claimed. Only ordered frames and PCM equality are checked.

Cost estimate, with one GPU job of -j 4 or less:
- Release build of ferrocut + ferrocut-mcp from the branch: about 10–15 min cold, the dominant cost.
- Renders: 144 + 80 + 81 frames at 640x360, under 1 min on GPU or a few minutes with --cpu.
- Decodes and PCM compares: seconds.
- Total wall time under 25 min. No installs, downloads or inference.
