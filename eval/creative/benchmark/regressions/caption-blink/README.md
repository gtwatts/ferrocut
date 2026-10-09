# Regression: caption blink at sub-frame SRT gaps

Run everything from this directory (`eval/creative/benchmark/regressions/caption-blink/`). Prerequisites:
`ferrocut` on PATH, plus Python 3 with numpy and the repository's built LGPL
FFmpeg for the sweep in step 4. Select that decoder for the helper scripts:

```sh
FERROCUT_REPO=$(git rev-parse --show-toplevel)
export PATH="$FERROCUT_REPO/third_party/ffmpeg-lgpl/bin:$PATH"
export LD_LIBRARY_PATH="$FERROCUT_REPO/third_party/ffmpeg-lgpl/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
```

Files:
- `captions-30.json`: the pristine bootstrap (1920×1080, 30 fps, 3 s gradient on track `BG`). It has no
  captions until step 1, so don't render or sweep it as is.
- `cues.srt`: three cues with 33 ms gaps placed so that each gap contains an output frame sample:
  Captions.1 ends at 1.010 s and Captions.2 starts at 1.043 s, so frame 31 (1.0333 s) falls between them.
  Captions.2 ends at 2.407 s and Captions.3 starts at 2.440 s, so frame 73 (2.4333 s) falls between them.
  Round SRT times (e.g. 0.967 → 1.000 s) contain no frame sample and hide the bug.
- `style.text-style`: caption TextSpec. It uses `FiraSans-SemiBold.otf` (SIL OFL 1.1, see `FiraSans-OFL.txt`).

## 1. Import (always into a scratch copy, so the bootstrap stays pristine)
```
cp captions-30.json work.json
ferrocut captions import --style ./style.text-style ./work.json ./cues.srt
```
Write `./style.text-style` with the `./` prefix: ferrocut 5de8434 fails on a bare `style.text-style` with
"No such file or directory". On a build with the caption repair, add the documented flags from
`ferrocut captions import --help` (policy: ceiling frame snap, close gaps ≤ 1/10 s inclusive, minimum duration
opt-in). This regression expects the defaults.

## 2. Inspect the cue timing (no render)
```
python3 -c "import json;from fractions import Fraction as F;t=json.load(open('work.json'));c=[x for tr in t['tracks'] if tr['name']=='Captions' for x in tr['clips']];[print(x['id'],F(str(x['start'])),F(str(x['start']))+F(str(x['duration']))) for x in c]"
```
- Legacy (5de8434): ends and starts are copied from the SRT, so 101/100 → 1043/1000 and 2407/1000 → 61/25.
- After the fix: consecutive cues abut on the frame grid, and the import output lists the closures.

## 3. Render the probe frames (CPU, seconds)
```
ferrocut stills work.json -o stills --frame 30 --frame 31 --frame 32 --frame 72 --frame 73 --frame 74 --each --cpu --json
```
- Legacy: frames 31 and 73 show no caption, while 30, 32, 72 and 74 do.
- After the fix: all six show a caption.

## 4. Coverage sweep on a rendered master
```
ferrocut render work.json -o work.mkv --cpu -j 2
python3 ../../tools/caption_gap_sweep.py work.json work.mkv Captions 160 880 1760 976 sweep.json
```
To compare a repaired master against saved baseline boundaries, append
`--probe-from before-sweep.json`, using the actual path of that baseline report.
The sweep checks every frame the caption track covers against the rendered band, lists short gaps (≤ 3 frames)
left in the timeline, and re-checks the legacy blink frames from `--probe-from`.
- Legacy: verdict fail (2 short gaps).
- After the fix, with `--probe-from` the legacy sweep: verdict pass.
Synthetic controls for the tool itself: `../../tools/make_sweep_control.py <dir>` (see the tool docstring).
