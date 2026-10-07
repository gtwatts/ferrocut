#!/usr/bin/env bash
# Build an LGPL-only, shared FFmpeg for Cutline in user space (no sudo).
#
#   ./scripts/build-ffmpeg-lgpl.sh            # -> third_party/ffmpeg-lgpl (gitignored)
#   PREFIX=~/.local/opt/ffmpeg-lgpl ./scripts/build-ffmpeg-lgpl.sh
#
# License posture: no --enable-gpl, no --enable-nonfree, no --enable-version3.
# Result is LGPL v2.1+, linkable from an Apache-2.0 project as shared libraries.
# --disable-autodetect so nothing (GPL or otherwise) sneaks in from the host;
# every external dependency is listed explicitly below:
#   zlib            (zlib license)  - matroska/png zlib support
#   nv-codec-headers (MIT)          - NVENC/NVDEC/CUVID; libcuda/libnvidia-encode are
#                                     dlopen'ed from the NVIDIA driver at runtime
# Build-only tool: NASM (BSD-2) for FFmpeg's x86 SIMD; built locally if missing.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
FFMPEG_VERSION="${FFMPEG_VERSION:-9.0.2}"
FFMPEG_SHA256="${FFMPEG_SHA256:-8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e}"
NASM_VERSION="2.16.03"
NASM_SHA256="1412a1c760bbd05db026b6c0d1657affd6631cd0a63cddb6f73cc6d4aa616148"
# SDK 13.0 needs NVIDIA driver >= 570 at runtime.
NVHEADERS_REF="${NVHEADERS_REF:-n13.0.19.1}"
PREFIX="${PREFIX:-$REPO/third_party/ffmpeg-lgpl}"
BUILD="${BUILD_DIR:-$REPO/third_party/build}"
JOBS="${JOBS:-$(nproc)}"
# Use the system toolchain and pkg-config so nothing resolves into linuxbrew.
export PATH="$BUILD/tools/bin:/usr/local/bin:/usr/bin:/bin"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig"

mkdir -p "$BUILD/dl" "$BUILD/tools"
fetch() { # url sha256
  local f="$BUILD/dl/$(basename "$1")"
  if [ ! -f "$f" ]; then curl -fsSL "$1" -o "$f.part" && mv "$f.part" "$f"; fi
  echo "$2  $f" | sha256sum -c --quiet - || { echo "checksum mismatch: $f" >&2; exit 1; }
  echo "$f"
}

# 1. NASM (build tool only)
if ! command -v nasm >/dev/null; then
  echo "== building nasm $NASM_VERSION"
  t=$(fetch "https://www.nasm.us/pub/nasm/releasebuilds/$NASM_VERSION/nasm-$NASM_VERSION.tar.xz" "$NASM_SHA256")
  rm -rf "$BUILD/nasm-$NASM_VERSION" && tar -xf "$t" -C "$BUILD"
  (cd "$BUILD/nasm-$NASM_VERSION" && ./configure -q --prefix="$BUILD/tools" && make -s -j"$JOBS" nasm \
    && install -D -m755 nasm "$BUILD/tools/bin/nasm")
fi
nasm -v

# 2. nv-codec-headers (MIT) into the prefix
echo "== nv-codec-headers $NVHEADERS_REF"
rm -rf "$BUILD/nv-codec-headers"
git -c advice.detachedHead=false clone -q --depth 1 --branch "$NVHEADERS_REF" \
  https://github.com/FFmpeg/nv-codec-headers.git "$BUILD/nv-codec-headers"
make -s -C "$BUILD/nv-codec-headers" PREFIX="$PREFIX" install

# 3. FFmpeg
echo "== ffmpeg $FFMPEG_VERSION -> $PREFIX"
t=$(fetch "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" "$FFMPEG_SHA256")
SRC="$BUILD/ffmpeg-$FFMPEG_VERSION"
rm -rf "$SRC" && tar -xf "$t" -C "$BUILD"
cd "$SRC"
./configure \
  --prefix="$PREFIX" \
  --pkg-config=pkg-config \
  --enable-shared --disable-static --enable-pic \
  --disable-autodetect \
  --disable-doc --disable-ffplay \
  --enable-zlib \
  --enable-ffnvcodec --enable-nvenc --enable-nvdec --enable-cuvid \
  --extra-ldflags="-Wl,-rpath,$PREFIX/lib" \
  --extra-version=cutline-lgpl
make -j"$JOBS"
make install

# 4. Verify the license posture
echo "== verify"
"$PREFIX/bin/ffmpeg" -hide_banner -L | head -3
if "$PREFIX/bin/ffmpeg" -hide_banner -buildconf | grep -Eq -- '--enable-(gpl|nonfree|libx264|libx265)'; then
  echo "ERROR: GPL/nonfree component in build configuration" >&2; exit 1
fi
"$PREFIX/bin/ffmpeg" -hide_banner -encoders | grep -E 'ffv1|ffvhuff|nvenc' || true
echo "OK: LGPL FFmpeg $FFMPEG_VERSION installed in $PREFIX"
