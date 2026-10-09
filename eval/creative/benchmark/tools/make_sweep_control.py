"""Synthetic controls for caption_gap_sweep.py (no Ferrocut render): a 3 s 30 fps 320x180 gray video whose
caption band (y 120-160) holds a text-like pattern on chosen frames, plus a matching timeline JSON.
Expected verdicts (band 20 120 300 160): baseline fail; repaired pass (with --probe-from the baseline sweep);
repaired-dropped fail; repaired-all-blank fail (inconclusive); repaired-truncated fail.
usage: make_sweep_control.py OUTDIR"""
import json, subprocess, sys
from pathlib import Path
import numpy as np
out = Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=True)
rng = np.random.default_rng(7)
pattern = (rng.random((40, 280)) > 0.7).astype(np.uint8) * 200 + 30
def video(name, blank_frames):
    frames = np.full((90, 180, 320), 40, np.uint8)
    for f in range(90):
        if f not in blank_frames:
            frames[f, 120:160, 20:300] = pattern
    p = subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "gray", "-s", "320x180", "-r", "30",
                        "-i", "-", "-c:v", "ffv1", str(out / name)], input=frames.tobytes(), check=True)
def timeline(name, cues):
    clips = [{"id": f"Captions.{i+1}", "generator": {"type": "solid", "color": ["1", "1", "1"]}, "start": s, "duration": d}
             for i, (s, d) in enumerate(cues)]
    json.dump({"name": name, "output": {"width": 320, "height": 180, "fps": "30", "duration": "3"},
               "tracks": [{"name": "Captions", "clips": clips}]}, open(out / f"{name}.json", "w"), indent=1)
# baseline: gaps 101/100..1043/1000 (frame 31) and 2407/1000..61/25 (frame 73), rendered blank there
timeline("baseline", [("0", "101/100"), ("1043/1000", "341/250"), ("61/25", "14/25")])
video("baseline.mkv", {31, 73})
# repaired: gaps closed (cues abut), rendered with captions on every frame -> must pass
timeline("repaired", [("0", "1043/1000"), ("1043/1000", "1397/1000"), ("61/25", "14/25")])
video("repaired.mkv", set())
# repaired timeline but a renderer that still drops frame 31 -> covered_blank must catch it
video("repaired-dropped.mkv", {31})
# repaired timeline over an all-blank render -> must fail (inconclusive reference), as must a truncated master
video("repaired-all-blank.mkv", set(range(90)))
subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(out / "repaired.mkv"), "-frames:v", "60", "-c:v", "ffv1",
                str(out / "repaired-truncated.mkv")], check=True)
print("controls in", out)
