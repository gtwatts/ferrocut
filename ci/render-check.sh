#!/usr/bin/env bash
# CPU-render regression check (CI and local): renders examples/demo.json and
# examples/demo-av.json on the software Vulkan adapter (Mesa lavapipe) and checks
#
#   1. determinism: each timeline rendered twice (-j $JOBS, then -j 2 --force
#      into a separate cache) gives a bit-identical master (final blake3);
#   2. frame counts;
#   3. audio: exact blake3 of the mixed PCM (CPU-only f32 math, no GPU);
#   4. video: SSIM against committed reference frames (ci/reference/*.ref.mkv:
#      every 24th frame, area-scaled to 960x540, FFV1) above fixed thresholds;
#   5. quality: `ferrocut check --require` (SeePlus's ferrocut-perceive) passes
#      on demo-av (cuts at 5 s / 10 s, -14 LUFS, true peak <= -1 dBTP). Runs when
#      ferrocut-perceive sits next to ferrocut (PERCEIVE_CHECK=1 forces it,
#      =0 skips). demo.json has no audio, which the checker fails by default.
#
# Why SSIM, not an exact video hash: lavapipe's output depends on the Mesa/LLVM
# version (llvmpipe shader codegen and float rounding), and ubuntu-latest's
# Mesa differs from the one the references were made with (see
# expected.json "made_with"). Even NVIDIA vs lavapipe differ by only 1-2 LSB
# (at this scale: SSIM mean 0.99989, worst frame 0.99918), so the thresholds catch real regressions
# (missing layers, wrong transforms, colour shifts, wrong frames) without
# flapping on rounding. The exact hashes from the reference machine are
# reported too: an exact match is shown, a mismatch alone does not fail.
#
#   ci/render-check.sh            # check
#   ci/render-check.sh --update   # re-render and rewrite ci/reference/ (commit + explain)
#
# Needs: target/release/ferrocut (or $FERROCUT), the LGPL FFmpeg in
# third_party/ffmpeg-lgpl (scripts/build-ffmpeg-lgpl.sh) and media/
# (scripts/gen-test-media.sh), python3.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd -P)"
FERROCUT="${FERROCUT:-$REPO/target/release/ferrocut}"
FF="$REPO/third_party/ffmpeg-lgpl/bin/ffmpeg"
OUT="${OUT:-$REPO/target/ci-render}"
REF="$REPO/ci/reference"
JOBS="${JOBS:-4}"
STEP=24
SCALE="scale=960:540:flags=area,format=bgr0"
UPDATE=0
[ "${1:-}" = "--update" ] && UPDATE=1
export LD_LIBRARY_PATH="$REPO/third_party/ffmpeg-lgpl/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export FERROCUT_ADAPTER=cpu
mkdir -p "$OUT"
cd "$REPO"

for name in demo demo-av; do
  echo "== $name: render A (-j $JOBS)"
  "$FERROCUT" render "examples/$name.json" -o "$OUT/$name-a.mkv" --cpu -j "$JOBS" \
    --cache-dir "$OUT/cache-a" --force > "$OUT/$name-a.log"
  tail -n 3 "$OUT/$name-a.log"
  if [ "$UPDATE" = 1 ]; then
    "$FF" -v error -y -i "$OUT/$name-a.mkv" -an \
      -vf "select='not(mod(n\,$STEP))',$SCALE" -fps_mode passthrough \
      -c:v ffv1 -level 3 -g 1 "$REF/$name.ref.mkv"
    continue
  fi
  echo "== $name: render B (-j 2, separate cache)"
  "$FERROCUT" render "examples/$name.json" -o "$OUT/$name-b.mkv" --cpu -j 2 \
    --cache-dir "$OUT/cache-b" --force > "$OUT/$name-b.log"
  "$FF" -v error -i "$OUT/$name-a.mkv" -i "$REF/$name.ref.mkv" -filter_complex \
    "[0:v]select='not(mod(n\,$STEP))',$SCALE,format=gbrp,setpts=N[a];[1:v]format=gbrp,setpts=N[b];[a][b]ssim=stats_file=$OUT/$name.ssim.txt" \
    -fps_mode passthrough -f null -
done

PERCEIVE_CHECK="${PERCEIVE_CHECK:-auto}"
if [ "$PERCEIVE_CHECK" = auto ]; then
  PERCEIVE_CHECK=0
  [ -x "$(dirname "$FERROCUT")/ferrocut-perceive" ] && PERCEIVE_CHECK=1
fi
rm -f "$OUT/demo-av.check.json"
if [ "$UPDATE" = 0 ] && [ "$PERCEIVE_CHECK" = 1 ]; then
  echo "== demo-av: quality check (ferrocut check --require)"
  "$FERROCUT" check "$OUT/demo-av-a.mkv" --timeline examples/demo-av.json --require \
    --timeout 600 > "$OUT/demo-av.check.json" || true
fi

python3 - "$REPO" "$OUT" "$UPDATE" "$PERCEIVE_CHECK" <<'PY'
import hashlib, json, os, re, sys
repo, out, update = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
perceive_check = sys.argv[4] == "1"
ref = os.path.join(repo, "ci/reference")
exp_path = os.path.join(ref, "expected.json")
names = ["demo", "demo-av"]
rep = {n: json.load(open(os.path.join(out, f"{n}-a.report.json"))) for n in names}

def media_sha():
    d = os.path.join(repo, "media")
    return {f: hashlib.sha256(open(os.path.join(d, f), "rb").read()).hexdigest()
            for f in sorted(os.listdir(d)) if not f.startswith(".")}

if update:
    exp = {
        "comment": "Reference for ci/render-check.sh. Regenerate with `ci/render-check.sh --update` and explain the change in the commit.",
        "made_with": {"adapter": rep["demo"]["adapter"], "ffmpeg": rep["demo"]["ffmpeg"], "engine": rep["demo"]["engine"]},
        "frames": {n: rep[n]["total_frames"] for n in names},
        "audio_blake3": {n: rep[n]["audio"]["blake3"] for n in names if rep[n].get("audio")},
        "ssim": {"step": 24, "size": "960x540", "mean_min": 0.999, "frame_min": 0.995},
        "reference_machine_hashes": {n: {"final_blake3": rep[n]["final_blake3"], "video_blake3": rep[n]["video_blake3"]} for n in names},
        "media_sha256": media_sha(),
    }
    json.dump(exp, open(exp_path, "w"), indent=2)
    open(exp_path, "a").write("\n")
    print(f"wrote {exp_path} and ci/reference/*.ref.mkv")
    sys.exit(0)

exp = json.load(open(exp_path))
fails, lines = [], []
def check(ok, msg):
    lines.append(("PASS " if ok else "FAIL ") + msg)
    if not ok:
        fails.append(msg)
def info(msg):
    lines.append("info " + msg)

info(f"adapter: {rep['demo']['adapter']}")
info(f"references made with: {exp['made_with']['adapter']}")
ms = media_sha()
same_media = ms == exp["media_sha256"]
info("media/ matches the reference machine byte for byte" if same_media else
     f"media/ differs from the reference machine: {[k for k in ms if ms.get(k) != exp['media_sha256'].get(k)]}")
for n in names:
    a = rep[n]
    b = json.load(open(os.path.join(out, f"{n}-b.report.json")))
    check(a["final_blake3"] == b["final_blake3"],
          f"{n}: deterministic (-j {a['jobs']} vs -j {b['jobs']}, fresh caches): {a['final_blake3'][:16]} / {b['final_blake3'][:16]}")
    check(a["total_frames"] == exp["frames"][n], f"{n}: {a['total_frames']} frames (expected {exp['frames'][n]})")
    if n in exp["audio_blake3"]:
        got = (a.get("audio") or {}).get("blake3")
        check(got == exp["audio_blake3"][n], f"{n}: audio blake3 {str(got)[:16]} (expected {exp['audio_blake3'][n][:16]})")
    vals = [float(m.group(1)) for m in re.finditer(r"All:([0-9.]+)", open(os.path.join(out, f"{n}.ssim.txt")).read())]
    want = exp["frames"][n] // exp["ssim"]["step"] + (1 if exp["frames"][n] % exp["ssim"]["step"] else 0)
    check(len(vals) == want, f"{n}: {len(vals)} SSIM samples (expected {want})")
    if vals:
        mean, lo = sum(vals) / len(vals), min(vals)
        worst = vals.index(lo) * exp["ssim"]["step"]
        check(mean >= exp["ssim"]["mean_min"], f"{n}: SSIM mean {mean:.6f} >= {exp['ssim']['mean_min']}")
        check(lo >= exp["ssim"]["frame_min"], f"{n}: SSIM min {lo:.6f} (frame {worst}) >= {exp['ssim']['frame_min']}")
    rh = exp["reference_machine_hashes"][n]
    info(f"{n}: video {'matches the reference machine bit-exactly' if a['video_blake3'] == rh['video_blake3'] else 'differs from the reference machine (expected when Mesa/LLVM differ)'}"
         f" ({a['video_blake3'][:16]} vs {rh['video_blake3'][:16]})")
    info(f"{n}: render {a['render_fps']:.1f} fps on {a['jobs']} jobs")
if perceive_check:
    p = os.path.join(out, "demo-av.check.json")
    try:
        c = json.load(open(p))
    except Exception as e:  # noqa: BLE001
        c = {"status": "error", "message": f"no check output: {e}"}
    probs = [f"{x['reason']} {x['range']}" for x in c.get("problems", [])]
    warns = [f"{x['reason']} {x['range']}" for x in c.get("warnings", [])]
    check(c.get("status") == "pass",
          f"demo-av: quality check {c.get('status')}"
          + (f" ({c.get('message')})" if c.get("message") else "")
          + (f" problems {probs}" if probs else ""))
    if warns:
        info(f"demo-av: quality warnings {warns}")
else:
    info("quality check skipped (no ferrocut-perceive next to ferrocut)")
text = "\n".join(lines)
print(text)
summary = os.environ.get("GITHUB_STEP_SUMMARY")
if summary:
    with open(summary, "a") as f:
        f.write("### CPU render check (lavapipe)\n\n```\n" + text + "\n```\n")
if fails:
    print(f"\n{len(fails)} check(s) failed", file=sys.stderr)
    sys.exit(1)
print("\nall render checks passed")
PY
