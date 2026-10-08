#!/usr/bin/env bash
# Generate short synthetic source clips for Ferrocut engine tests into media/ (gitignored).
# Uses only LGPL-native encoders (mpeg4, flac, pcm); long-GOP with B-frames on purpose so
# the decoder's keyframe-seek + decode-forward path gets exercised.
#
# Every generator is deterministic (same bytes run to run), so demo render hashes are
# reproducible. Existing files are kept; FORCE=1 regenerates everything.
# (FFmpeg's `gradients` source is NOT deterministic even with seed=, so the gradient
# clip is an exact `geq` expression instead.)
set -euo pipefail
# Prefer the project's LGPL FFmpeg CLI (scripts/build-ffmpeg-lgpl.sh) when present.
LGPL_FFMPEG="$(cd "$(dirname "$0")/.." && pwd)/third_party/ffmpeg-lgpl/bin/ffmpeg"
if [ -x "$LGPL_FFMPEG" ]; then FFMPEG="${FFMPEG:-$LGPL_FFMPEG}"; else FFMPEG="${FFMPEG:-ffmpeg}"; fi
OUT="${1:-$(cd "$(dirname "$0")/.." && pwd)/media}"
SIZE="${SIZE:-1920x1080}"
RATE="${RATE:-24}"
mkdir -p "$OUT"
enc=(-an -c:v mpeg4 -q:v 3 -g 12 -bf 2 -pix_fmt yuv420p -y -loglevel error)
skip() { [ -z "${FORCE:-}" ] && [ -s "$1" ] && echo "  $1  (exists, kept)"; }
gen() { local name=$1 src=$2 dur=$3
  skip "$OUT/$name.mov" && return 0
  echo "  $OUT/$name.mov  ($src, ${dur}s)"
  "$FFMPEG" -f lavfi -i "$src" -t "$dur" "${enc[@]}" "$OUT/$name.mov"
}
echo "generating test media into $OUT with $("$FFMPEG" -version | head -1)"
gen bars      "smptehdbars=size=$SIZE:rate=$RATE"                 6
gen testsrc2  "testsrc2=size=$SIZE:rate=$RATE"                    6
gen gradients "color=c=black:size=$SIZE:rate=$RATE,format=yuv444p,geq=lum='128+60*sin(2*PI*(X/W+T*0.11))+40*cos(2*PI*(Y/H-T*0.07))':cb='128+90*sin(2*PI*((X+Y)/(W+H)+T*0.05))':cr='128+90*cos(2*PI*(X/W-Y/H-T*0.09))'" 7
gen overlay   "testsrc=size=$SIZE:rate=$RATE"                     4

# Audio (LGPL-native encoders only: flac, pcm). Deterministic generators.
aenc=(-y -loglevel error -bitexact -fflags +bitexact)
gen_audio() { local name=$1 rate=$2 layout=$3 expr=$4 dur=$5 codec=$6
  skip "$OUT/$name" && return 0
  echo "  $OUT/$name  (${rate} Hz $layout, ${dur}s)"
  "$FFMPEG" -f lavfi -i "aevalsrc=exprs=$expr:s=$rate:c=$layout:d=$dur" -c:a "$codec" "${aenc[@]}" "$OUT/$name"
}
# Music: a slow chord with a little stereo motion, 44.1 kHz (exercises resampling).
gen_audio music.flac 44100 stereo \
  "0.18*sin(2*PI*220*t)+0.12*sin(2*PI*277.18*t)+0.10*sin(2*PI*329.63*t)+0.06*sin(2*PI*110*t)*(1+sin(2*PI*0.25*t))|0.16*sin(2*PI*220*t)+0.12*sin(2*PI*277.18*t+0.5)+0.11*sin(2*PI*329.63*t)+0.06*sin(2*PI*110*t)*(1-sin(2*PI*0.25*t))" \
  14 flac
# Dialogue: speech-like 0.6 s phrases every 1.5 s, mono 48 kHz.
gen_audio dialogue.wav 48000 mono \
  "0.45*sin(2*PI*180*t+3*sin(2*PI*4*t))*(0.6+0.4*sin(2*PI*7*t))*lt(mod(t\,1.5)\,0.6)" \
  12 pcm_s16le
# Linked A/V camera clips: picture + beeps (mono-ish stereo at 44.1 kHz in MOV/PCM).
gen_av() { local name=$1 vsrc=$2 expr=$3 dur=$4
  skip "$OUT/$name.mov" && return 0
  echo "  $OUT/$name.mov  ($vsrc + audio, ${dur}s)"
  "$FFMPEG" -f lavfi -i "$vsrc" -f lavfi -i "aevalsrc=exprs=$expr:s=44100:c=stereo:d=$dur" -t "$dur" \
    -c:v mpeg4 -q:v 3 -g 12 -bf 2 -pix_fmt yuv420p -c:a pcm_s16le -shortest "${aenc[@]}" "$OUT/$name.mov"
}
gen_av cam_a "testsrc2=size=$SIZE:rate=$RATE" "0.3*sin(2*PI*440*t)*lt(mod(t\,1)\,0.4)|0.3*sin(2*PI*440*t)*lt(mod(t\,1)\,0.4)" 8
gen_av cam_b "smptehdbars=size=$SIZE:rate=$RATE" "0.3*sin(2*PI*660*t)*lt(mod(t\,0.5)\,0.2)|0.25*sin(2*PI*660*t)*lt(mod(t\,0.5)\,0.2)" 8
