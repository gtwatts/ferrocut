#!/usr/bin/env bash
# Download a whisper.cpp ggml model from the official repo
# (huggingface.co/ggerganov/whisper.cpp) into third_party/whisper-models/,
# verified against the repo's published LFS sha256.
#
#   scripts/fetch-whisper-model.sh [small|medium|base]   (default: small)
#
# `ferrocut index` looks for $FERROCUT_WHISPER_MODEL, else
# third_party/whisper-models/ggml-small.bin.
set -euo pipefail
cd "$(dirname "$0")/.."
name="${1:-small}"
case "$name" in
  base)   sha=60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe ;;
  small)  sha=1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b ;;
  medium) sha=6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208 ;;
  *) echo "unknown model $name (base|small|medium)" >&2; exit 2 ;;
esac
dir=third_party/whisper-models
out="$dir/ggml-$name.bin"
mkdir -p "$dir"
if [ -f "$out" ] && echo "$sha  $out" | sha256sum -c --status; then
  echo "have $out"; exit 0
fi
url="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-$name.bin"
curl -fL --retry 3 -o "$out.part" "$url"
echo "$sha  $out.part" | sha256sum -c --status || { echo "sha256 mismatch for $url" >&2; rm -f "$out.part"; exit 1; }
mv "$out.part" "$out"
echo "fetched $out"
