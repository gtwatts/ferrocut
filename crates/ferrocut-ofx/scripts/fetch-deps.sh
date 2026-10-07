#!/usr/bin/env bash
# Fetch the pinned C/C++ sources ferrocut-ofx builds from (into third_party/,
# which is git-ignored). build.rs never touches the network.
set -euo pipefail
OPENFX_TAG="${OPENFX_TAG:-OFX_Release_1.5.1}"
EXPAT_TAG="${EXPAT_TAG:-R_2_8_5}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TP="$HERE/third_party"
mkdir -p "$TP"

fetch() { # url tag dir
  local url="$1" tag="$2" dir="$3"
  if [ ! -d "$dir/.git" ]; then
    git -c advice.detachedHead=false clone --depth 1 --branch "$tag" "$url" "$dir"
  else
    git -C "$dir" fetch --depth 1 origin "refs/tags/$tag:refs/tags/$tag" 2>/dev/null || true
    git -c advice.detachedHead=false -C "$dir" checkout -q "$tag"
  fi
  echo "$(basename "$dir"): $(git -C "$dir" describe --tags --exact-match)"
}
fetch https://github.com/AcademySoftwareFoundation/openfx.git "$OPENFX_TAG" "$TP/src-openfx"
fetch https://github.com/libexpat/libexpat.git "$EXPAT_TAG" "$TP/src-expat"
