#!/usr/bin/env bash
# Fetch the eval films (CC-BY 3.0, Blender Foundation; see eval/README.md) from
# an official Blender mirror, verify them, and cut the short clips the tasks use
# into eval/media/clips/ with the repo's LGPL FFmpeg. Idempotent; media is
# gitignored (never commit it).
#
#   eval/fetch-media.sh            # download if missing, verify, cut clips
#   MIRROR=https://... eval/fetch-media.sh
#
# download.blender.org sits behind bot protection; ftp.nluug.nl is one of the
# official mirrors listed on blender.org and serves the same files.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd -P)"
MIRROR="${MIRROR:-https://ftp.nluug.nl/pub/graphics/blender/demo/movies}"
SRC="$REPO/eval/media/src"
CLIPS="$REPO/eval/media/clips"
FF="$REPO/third_party/ffmpeg-lgpl/bin/ffmpeg"
export LD_LIBRARY_PATH="$REPO/third_party/ffmpeg-lgpl/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
[ -x "$FF" ] || { echo "LGPL FFmpeg missing: run scripts/build-ffmpeg-lgpl.sh" >&2; exit 2; }
mkdir -p "$SRC" "$CLIPS"

fetch() { # url file sha256
  local url=$1 file=$2 sum=$3
  if [ ! -f "$SRC/$file" ] || ! echo "$sum  $SRC/$file" | sha256sum -c --quiet - 2>/dev/null; then
    echo "== downloading $url"
    curl -fL --retry 3 -C - -o "$SRC/$file.part" "$url"
    mv "$SRC/$file.part" "$SRC/$file"
  fi
  echo "$sum  $SRC/$file" | sha256sum -c -
}
fetch "$MIRROR/ToS/tears_of_steel_720p.mov" tears_of_steel_720p.mov \
  efa9062d9cdb7a338e40ad530dfdf234806743f29ae6a1a136b97ece4e588e8f
fetch "$MIRROR/Sintel.2010.720p.mkv.zip" Sintel.2010.720p.mkv.zip \
  4f013b146ec6480813b1a515bfb96a2ae3e350644e4338373b8365a06a024b84
if [ ! -f "$SRC/Sintel.2010.720p.mkv" ]; then
  (cd "$SRC" && unzip -o -q Sintel.2010.720p.mkv.zip)
fi
# md5 published next to the zip on the mirror
echo "08d1108e0160b847f894acfdbce82305  $SRC/Sintel.2010.720p.mkv" | md5sum -c -

# name  film  start(s)  [seconds, default 6]
# s*/t*: 6 s single-shot segments, picked from long shots 0.5 s past the shot's
# first frame. d*: longer dialogue scenes (with cuts) for transcript tasks.
CLIP_LIST="
s1 sintel 89.04
s2 sintel 138.62
s3 sintel 162.58
s4 sintel 383.42
s5 sintel 663.04
s6 sintel 547.58
t1 tos 102.92
t2 tos 258.42
t3 tos 159.04
t4 tos 239.17
t5 tos 445.38
t6 tos 605.04
d1 sintel 116.5 24
"
cut() { # name film start [seconds]
  local name=$1 film=$2 ss=$3 secs=${4:-6} out="$CLIPS/$1.mkv"
  local frames=$((secs * 24))
  [ -f "$out" ] && [ "${FORCE:-0}" != 1 ] && return
  echo "== $name ($film @ ${ss}s, ${secs}s)"
  if [ "$film" = sintel ]; then
    # Sintel: picture + score/dialogue (AC-3 5.1 -> stereo PCM)
    "$FF" -nostdin -v error -y -ss "$ss" -i "$SRC/Sintel.2010.720p.mkv" -map 0:v:0 -map 0:a:0 \
      -frames:v "$frames" -t "$secs" -c:v mpeg4 -q:v 2 -g 24 -bf 0 -pix_fmt yuv420p -threads 1 \
      -c:a pcm_s16le -ar 48000 -ac 2 -fflags +bitexact -flags:v +bitexact -flags:a +bitexact \
      "$out.tmp.mkv"
  else
    # Tears of Steel: picture only (its soundtrack is CC-BY-ND, so no audio edits)
    "$FF" -nostdin -v error -y -ss "$ss" -i "$SRC/tears_of_steel_720p.mov" -map 0:v:0 -an \
      -frames:v "$frames" -c:v mpeg4 -q:v 2 -g 24 -bf 0 -pix_fmt yuv420p -threads 1 \
      -fflags +bitexact -flags:v +bitexact "$out.tmp.mkv"
  fi
  mv "$out.tmp.mkv" "$out"
}
while read -r name film ss secs; do
  [ -n "${name:-}" ] && cut "$name" "$film" "$ss" "$secs"
done <<< "$CLIP_LIST"
echo "clips in $CLIPS:"
ls -l "$CLIPS"
