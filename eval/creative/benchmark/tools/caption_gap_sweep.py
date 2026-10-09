"""Caption coverage check on a RENDERED master (needs numpy and ffmpeg). Exit 0 = pass, 1 = fail/inconclusive.

Scope of the claim: it measures luma contrast (std-dev) in one caption band. A pass says the band shows
structure on every frame the caption track covers and on the former blink frames. It does not read the text,
judge legibility or check the plate separately. Inspect stills of the former blink frames as well.

It decodes every frame of the master, crops the caption band, and measures luma std-dev (text and plate
edges). Three results:

1. covered_blank: frames that the timeline's caption track covers (t = f/fps inside a clip) but whose band
   reads blank (std < blank_ratio x the median std over covered frames). This should be empty. It checks the
   rendered picture against the timeline's own claim of coverage, so it still works after a repaired timeline
   has no gaps left.
2. timeline_gaps: frames that fall between consecutive caption clips in this timeline, classified as
   short (<= --short-frames, a blink) or pause (longer, authored silence).
3. probes: fixed frame indices carried over from a BASELINE sweep (--probe-from before.json: the frames of
   its short gaps). After a repair they must read visible. This holds even though the repaired timeline no
   longer has gaps at those frames.

A band is blank when its std is below max(blank_ratio x reference, --min-visible-std). The verdict also
fails (inconclusive) when there are no covered frames, when the reference itself is below the floor (an
all-blank or uniform band), or when the master's frame count differs from the timeline's or any expected
frame is missing.

Verdict: fail if covered_blank is non-empty, if any short gap remains in the timeline, or if any probe frame
reads blank. Authored pauses are listed but never fail.

Negative control: run it on the baseline master + baseline project with --probe-from the same baseline sweep.
It must fail (the probes read blank).

usage: caption_gap_sweep.py PROJECT MASTER TRACK X0 Y0 X1 Y1 OUT.json [--probe-from BEFORE.json]
       [--short-frames 3] [--blank-ratio 0.5]
"""
import argparse
import json
import math
import subprocess
import sys
from fractions import Fraction as F

import numpy as np

ap = argparse.ArgumentParser()
ap.add_argument("project")
ap.add_argument("master")
ap.add_argument("track")
ap.add_argument("x0", type=int)
ap.add_argument("y0", type=int)
ap.add_argument("x1", type=int)
ap.add_argument("y1", type=int)
ap.add_argument("out")
ap.add_argument("--probe-from")
ap.add_argument("--short-frames", type=int, default=3)
ap.add_argument("--blank-ratio", type=float, default=0.5)
ap.add_argument("--min-visible-std", type=float, default=8.0,
                help="absolute floor: a band with std below this is never 'visible'; the reference must exceed it")
a = ap.parse_args()

tl = json.load(open(a.project))
fps = F(str(tl["output"]["fps"]))
track = next((t for t in tl["tracks"] if t.get("name") == a.track), None)
if track is None:
    sys.exit(f"no track {a.track!r} in {a.project}")
clips = sorted(track["clips"], key=lambda c: F(str(c["start"])))
spans = [(F(str(c["start"])), F(str(c["start"])) + F(str(c["duration"])), c["id"]) for c in clips]


def frames_in(s, e):
    """Output frames whose sample time f/fps lies in [s, e)."""
    return range(math.ceil(s * fps), math.ceil(e * fps))


covered = {}
for s, e, cid in spans:
    for f in frames_in(s, e):
        covered[f] = cid

gaps = []
for (s0, e0, a_id), (s1, e1, b_id) in zip(spans, spans[1:]):
    if s1 > e0:
        fs = list(frames_in(e0, s1))
        gaps.append({"after": a_id, "before": b_id, "end": str(e0), "next_start": str(s1),
                     "gap_seconds": str(s1 - e0), "frames": fs,
                     "kind": "none" if not fs else ("short" if len(fs) <= a.short_frames else "pause")})

w, h = a.x1 - a.x0, a.y1 - a.y0
raw = subprocess.run(["ffmpeg", "-v", "error", "-i", a.master, "-vf", f"crop={w}:{h}:{a.x0}:{a.y0},format=gray",
                      "-f", "rawvideo", "-"], capture_output=True, check=True).stdout
std = np.frombuffer(raw, np.uint8).reshape(-1, h, w).astype(np.float32).std(axis=(1, 2))
n = len(std)
def program_end(t):
    """output.duration, else the end of the last clip on any video or audio track (the engine's default)."""
    if t["output"].get("duration") is not None:
        return F(str(t["output"]["duration"]))
    ends = [F(str(c["start"])) + F(str(c["duration"]))
            for tr in t.get("tracks", []) + t.get("audio_tracks", []) for c in tr.get("clips", [])]
    return max(ends)


expected_n = math.ceil(program_end(tl) * fps)
cov = sorted(covered)
missing = [f for f in cov if f >= n]
cov = [f for f in cov if f < n]
ref = float(np.median(std[cov])) if cov else 0.0
# relative threshold, but never below an absolute floor: an all-blank render must not pass
thr = max(a.blank_ratio * ref, a.min_visible_std)


def blank(f):
    return bool(std[f] < thr)


covered_blank = [{"frame": f, "clip": covered[f], "std": round(float(std[f]), 2)} for f in cov if blank(f)]
for g in gaps:
    g["std"] = [round(float(std[f]), 2) for f in g["frames"] if f < n]
    g["blank"] = [f for f in g["frames"] if f < n and blank(f)]

probes = []
if a.probe_from:
    before = json.load(open(a.probe_from))
    for g in before.get("gaps", []):
        fs = g.get("frames") or g.get("frames_in_gap") or []
        if fs and len(fs) <= a.short_frames:
            for f in fs:
                if f >= n:
                    missing.append(f)
                else:
                    probes.append({"frame": f, "baseline_gap": f'{g.get("after")}->{g.get("before")}',
                                   "std": round(float(std[f]), 2), "blank": blank(f),
                                   "covered_now": f in covered})

short_left = [g for g in gaps if g["kind"] == "short"]
fail_reasons = []
if n != expected_n:
    fail_reasons.append(f"decoded {n} frames, timeline expects {expected_n} (truncated or wrong master)")
if missing:
    fail_reasons.append(f"{len(missing)} expected frames beyond the decoded master (first {missing[0]})")
if not cov:
    fail_reasons.append("inconclusive: no covered caption frames to measure")
elif ref < a.min_visible_std:
    fail_reasons.append(f"inconclusive: reference std {ref:.2f} below the visible floor {a.min_visible_std} "
                        "(band blank or uniform on covered frames; wrong band or captions missing)")
if covered_blank:
    fail_reasons.append(f"{len(covered_blank)} covered frames read blank")
if short_left:
    fail_reasons.append(f"{len(short_left)} short gaps (<= {a.short_frames} frames) remain in the timeline")
if any(p["blank"] for p in probes):
    fail_reasons.append(f"{sum(p['blank'] for p in probes)} baseline probe frames read blank")
res = {"project": a.project, "master": a.master, "track": a.track, "band": [a.x0, a.y0, a.x1, a.y1],
       "frames_decoded": n, "frames_expected": expected_n, "missing_frames": missing, "covered_frames": len(cov), "reference_std": round(ref, 2),
       "blank_threshold": round(thr, 2), "covered_blank": covered_blank, "gaps": gaps, "probes": probes,
       "pauses": [g for g in gaps if g["kind"] == "pause"],
       "verdict": "fail" if fail_reasons else "pass", "fail_reasons": fail_reasons}
json.dump(res, open(a.out, "w"), indent=1)
print(f"{a.project}: verdict {res['verdict']} {fail_reasons}")
print(f"  covered {len(cov)} f, ref std {ref:.1f}, covered_blank {len(covered_blank)}; "
      f"gaps short {len(short_left)} pause {len(res['pauses'])}; probes {len(probes)} "
      f"(blank {sum(p['blank'] for p in probes)})")
sys.exit(1 if fail_reasons else 0)
