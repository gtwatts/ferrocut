#!/usr/bin/env bash
# Build the pinned ThorVG (MIT) as a static library with the C API, CPU engine and
# Lottie loader, apply Ferrocut's determinism patch, install into third_party/install.
# Everything lands under crates/ferrocut-lottie/third_party/ (gitignored); no sudo.
# Needs: curl, python3 (for a private meson/ninja venv), a C++17 compiler.
set -euo pipefail

THORVG_VERSION="1.1.2"
THORVG_SHA256="4ac22e7648e4d163c8d06ccadfd463fe17d62f39854c8e6057cc9291d80603cd"
MESON_VERSION="1.9.1"
NINJA_VERSION="1.13.0"

here="$(cd "$(dirname "$0")/.." && pwd -P)"  # physical: never bake a symlinked path
tp="$here/third_party"
src="$tp/thorvg-$THORVG_VERSION"
prefix="$tp/install"
mkdir -p "$tp"

tarball="$tp/thorvg-$THORVG_VERSION.tar.xz"
if [[ ! -f "$tarball" ]] || ! echo "$THORVG_SHA256  $tarball" | sha256sum -c --quiet - 2>/dev/null; then
  curl -fL --retry 3 -o "$tarball.part" \
    "https://github.com/thorvg/thorvg/releases/download/v$THORVG_VERSION/thorvg-$THORVG_VERSION.tar.xz"
  mv "$tarball.part" "$tarball"
fi
echo "$THORVG_SHA256  $tarball" | sha256sum -c -

# Always start from a pristine tree so the patch applies exactly once.
rm -rf "$src" "$tp/build-thorvg"
tar -xf "$tarball" -C "$tp"
for p in "$here"/patches/thorvg-$THORVG_VERSION-*.patch; do
  echo "applying $(basename "$p")"
  patch -d "$src" -p1 --forward --quiet < "$p"
done

venv="$tp/.venv"
# pip writes absolute shebangs: a venv created under another checkout path (the
# repo was moved or renamed) must be recreated, not reused.
if [[ -f "$venv/bin/meson" ]] && [[ "$(head -c 300 "$venv/bin/meson" | head -n1)" != "#!$venv/bin/python"* ]]; then
  echo "venv was created for another path; recreating"
  rm -rf "$venv"
fi
# Same for an install whose .pc points at another prefix.
if [[ -f "$prefix/lib/pkgconfig/thorvg-1.pc" ]] && ! grep -qxF "prefix=$prefix" "$prefix/lib/pkgconfig/thorvg-1.pc"; then
  rm -rf "$prefix"
fi
if [[ ! -x "$venv/bin/meson" ]] || [[ "$("$venv/bin/meson" --version 2>/dev/null)" != "$MESON_VERSION" ]]; then
  rm -rf "$venv"
  python3 -m venv "$venv"
  "$venv/bin/pip" install --quiet "meson==$MESON_VERSION" "ninja==$NINJA_VERSION"
fi
export PATH="$venv/bin:$PATH"

# Determinism-relevant choices:
#   engines=cpu      software rasterizer only (GPU engines are not bit-reproducible)
#   partial=false    no dirty-region rendering: every frame is drawn from scratch
#   simd=false       identical code path on every x86-64/arm64 machine
#   extra=lottie_exp expressions on (JerryScript, Apache-2.0); no openmp
#   file=false       never touches the filesystem; Ferrocut hands it bytes
meson setup "$tp/build-thorvg" "$src" \
  --prefix="$prefix" --libdir=lib --buildtype=release \
  -Ddefault_library=static -Dstatic=true \
  -Dengines=cpu -Dloaders=lottie,png,jpg,ttf -Dsavers= -Dbindings=capi \
  -Dpartial=false -Dsimd=false -Dthreads=true -Dfile=false \
  -Dextra=lottie_exp -Dtools= -Dtests=false -Dlog=false
ninja -C "$tp/build-thorvg" install
echo "$THORVG_VERSION" > "$prefix/.ferrocut-version"
echo "ThorVG $THORVG_VERSION installed to $prefix"
