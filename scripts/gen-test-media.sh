#!/usr/bin/env bash
# Generate short synthetic source clips for Cutline engine tests into media/ (gitignored).
# Uses only LGPL-native encoders (mpeg4); long-GOP with B-frames on purpose so the
# decoder's keyframe-seek + decode-forward path gets exercised.
set -euo pipefail
# Prefer the project's LGPL FFmpeg CLI (scripts/build-ffmpeg-lgpl.sh) when present.
LGPL_FFMPEG="$(cd "$(dirname "$0")/.." && pwd)/third_party/ffmpeg-lgpl/bin/ffmpeg"
if [ -x "$LGPL_FFMPEG" ]; then FFMPEG="${FFMPEG:-$LGPL_FFMPEG}"; else FFMPEG="${FFMPEG:-ffmpeg}"; fi
OUT="${1:-$(cd "$(dirname "$0")/.." && pwd)/media}"
SIZE="${SIZE:-1920x1080}"
RATE="${RATE:-24}"
mkdir -p "$OUT"
enc=(-an -c:v mpeg4 -q:v 3 -g 12 -bf 2 -pix_fmt yuv420p -y -loglevel error)
gen() { local name=$1 src=$2 dur=$3
  echo "  $OUT/$name.mov  ($src, ${dur}s)"
  "$FFMPEG" -f lavfi -i "$src" -t "$dur" "${enc[@]}" "$OUT/$name.mov"
}
echo "generating test media into $OUT with $("$FFMPEG" -version | head -1)"
gen bars      "smptehdbars=size=$SIZE:rate=$RATE"                 6
gen testsrc2  "testsrc2=size=$SIZE:rate=$RATE"                    6
gen gradients "gradients=size=$SIZE:rate=$RATE:speed=0.03:seed=7" 7
gen overlay   "testsrc=size=$SIZE:rate=$RATE"                     4
