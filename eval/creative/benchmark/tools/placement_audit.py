"""BASELINE-ONLY static placement audit (models ferrocut <= 5de8434, which has no `fit`).

For each media clip it computes the displayed aspect under the installed stretch placement: the decoder
swscales the source to the output WxH, and `transform.scale` multiplies that per axis. It only does
arithmetic on the timeline and probed sizes. It renders nothing, and it is not visual proof.

It refuses timelines that use `fit` / `output.fit` (exit 2). Under the fit design the placement arithmetic is
different, so a fit-aware result must come from rendered frames (measure the picture bounds), not from this
model.

usage: placement_audit.py out.json project.json..."""
import json, os, subprocess, sys
from fractions import Fraction as F
out, projs = sys.argv[1], sys.argv[2:]
cache = {}
def probe(p):
    if p not in cache:
        j = json.loads(subprocess.run(["ferrocut", "probe", p], capture_output=True, text=True, check=True).stdout)
        cache[p] = j
    return cache[p]
def dims(j):
    for k in ("width", "height"):
        if k not in j:
            v = next((s for s in j.get("streams", []) if s.get("width")), {})
            return v.get("width"), v.get("height")
    return j["width"], j["height"]
def first(v):
    """Static value, or the first keyframe value of an animated one (marked animated)."""
    if isinstance(v, dict) and "keyframes" in v: return v["keyframes"][0]["v"], True
    return v, False
def scal(v):
    if v is None: return (F(1), F(1)), False
    v, anim = first(v)
    if isinstance(v, list):
        a, an = first(v[0]); b, bn = first(v[1]); return (F(str(a)), F(str(b))), anim or an or bn
    return (F(str(v)), F(str(v))), anim
rows = []
for pj in projs:
    _tl = json.load(open(pj))
    if "fit" in _tl.get("output", {}) or any("fit" in c for t in _tl["tracks"] for c in t.get("clips", [])):
        print(f"{pj}: uses `fit`; this audit models only the pre-fit stretch placement (baseline-only). "
              "Measure rendered picture bounds instead.", file=sys.stderr)
        sys.exit(2)
for pj in projs:
    tl = json.load(open(pj)); W, H = tl["output"]["width"], tl["output"]["height"]; base = os.path.dirname(pj)
    for t in tl["tracks"]:
        for c in t.get("clips", []):
            s = c.get("source")
            if not isinstance(s, str) or not s.endswith((".mkv", ".mp4", ".mov", ".png", ".jpg", ".webm")): continue
            path = os.path.normpath(os.path.join(base, s))
            try: sw, sh = dims(probe(path))
            except Exception as e: rows.append({"project": pj, "clip": c["id"], "source": s, "error": str(e)}); continue
            (sx, sy), animated = scal((c.get("transform") or {}).get("scale"))
            dw, dh = W * sx, H * sy
            change = (dw / dh) / F(sw, sh)
            rows.append({"model": "stretch-to-frame (ferrocut <= 5de8434)", "project": pj, "track": t.get("name"), "clip": c["id"], "source": s, "src": [sw, sh], "out": [W, H],
                         "scale": [str(sx), str(sy)], "displayed_px": [round(float(dw), 1), round(float(dh), 1)],
                         "scale_animated": animated, "aspect_change": round(float(change), 4), "distorted": abs(float(change) - 1) > 0.005})
json.dump(rows, open(out, "w"), indent=1)
from collections import Counter
for pj in projs:
    r = [x for x in rows if x["project"] == pj and "aspect_change" in x]
    d = [x for x in r if x["distorted"]]
    print(f"{pj}: {len(r)} media clips, {len(d)} distorted; changes {sorted(Counter(x['aspect_change'] for x in d).items())}")
    for x in d[:6]: print(f"   {x['clip']} {x['source'].split('/')[-1]} src {x['src']} scale {x['scale']} -> {x['displayed_px']} aspect x{x['aspect_change']}")
