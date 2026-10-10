#!/usr/bin/env python3
"""Write the selected-range demo fixture (original, synthetic, public).

6 s native sequence at 24000/1001 fps, 12-frame GOPs, 24-frame chunks
(boundaries 24, 48, 72, 96, 120). The demo range is frames [30, 110):
neither end is a chunk boundary. Content:
  - a rectangle moving left to right (keyframed transform),
  - a counter: one text clip per second showing 0..5,
  - pulse.wav: 48 kHz stereo float; 1 kHz bursts of 200 ms starting at
    1.15 s and 4.50 s, each crossing one range edge
    (t(30) = 1.25125 s, t(110) = 4.5875416... s),
  - captions.srt: two cues straddling the range start and end.
Usage: make_fixture.py <output dir>   (writes timeline.json, pulse.wav,
captions.srt, caption-style.text-style; refuses an existing dir)
"""
import json
import math
import struct
import sys
from pathlib import Path

FONT = "NotoSans-Regular.ttf"  # copied next to the fixture by RUN.md step 1
RATE = 48_000
SECONDS = 6


def wav(path: Path) -> None:
    n = RATE * SECONDS
    samples = [0.0] * n
    for start in (1.15, 4.50):
        s0 = round(start * RATE)
        for i in range(s0, s0 + RATE // 5):
            samples[i] = 0.5 * math.sin(2 * math.pi * 1000 * (i - s0) / RATE)
    data = b"".join(struct.pack("<ff", v, v) for v in samples)
    hdr = b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt " + struct.pack(
        "<IHHIIHH", 16, 3, 2, RATE, RATE * 8, 8, 32) + b"data" + struct.pack("<I", len(data))
    path.write_bytes(hdr + data)


def text(content: str, x: str, y: str, size: str) -> dict:
    return {"type": "text", "text": {
        "content": content, "font": FONT, "font_size": size,
        "position": [x, y], "box_size": ["400", "80"], "fill": ["0.95", "0.95", "0.9"]}}


def timeline() -> dict:
    counter = [{"id": f"count{k}", "start": k, "duration": 1,
                "generator": text(str(k), "40", "30", "56")} for k in range(SECONDS)]
    return {
        "name": "Selected-range demo (synthetic)",
        "output": {"width": 640, "height": 360, "fps": "24000/1001", "duration": SECONDS,
                   "gop": 12, "gops_per_chunk": 2},
        "tracks": [
            {"name": "Background", "clips": [{"id": "bg", "start": 0, "duration": SECONDS,
                "generator": {"type": "solid", "color": ["0.06", "0.08", "0.11", 1]}}]},
            {"name": "Mover", "clips": [{"id": "mover", "start": 0, "duration": SECONDS,
                "generator": {"type": "vector_group", "group": {"items": [{"type": "shape", "shape": {
                    "geometry": {"type": "rectangle", "x": 0, "y": 150, "width": 60, "height": 60, "radius": 6},
                    "fill": {"type": "solid", "color": ["0.95", "0.45", "0.2", 1]}}}]}},
                "transform": {"position": [
                    {"keyframes": [{"t": "0", "v": "0"}, {"t": str(SECONDS), "v": "580"}]},
                    {"keyframes": [{"t": "0", "v": "0"}, {"t": str(SECONDS), "v": "0"}]}]}}]},
            {"name": "Counter", "clips": counter},
        ],
        "audio_tracks": [{"name": "pulse", "clips": [
            {"id": "pulse", "source": "pulse.wav", "start": 0, "duration": SECONDS}]}],
        "audio": {"sample_rate": RATE},
    }


SRT = """1
00:00:00,800 --> 00:00:01,700
Straddles the range start

2
00:00:04,200 --> 00:00:05,000
Straddles the range end
"""

STYLE = {"content": "", "font": FONT, "font_size": "24", "position": ["40", "290"],
         "box_size": ["560", "48"], "align": "center", "vertical_align": "center",
         "fill": ["0.84", "0.89", "0.87"]}


def main() -> None:
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=False)
    wav(out / "pulse.wav")
    (out / "timeline.json").write_text(json.dumps(timeline(), indent=2) + "\n")
    (out / "captions.srt").write_text(SRT)
    (out / "caption-style.text-style").write_text(json.dumps(STYLE, indent=2) + "\n")


if __name__ == "__main__":
    main()
