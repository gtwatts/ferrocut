#!/usr/bin/env bash
# Builds a pinned, static, self-contained OpenColorIO into third_party/install.
# All of OCIO's dependencies (yaml-cpp, pystring, expat, Imath, minizip-ng, zlib)
# are fetched and built by OCIO's own CMake (OCIO_INSTALL_EXT_PACKAGES=ALL), so no
# system -dev packages are needed beyond a C++17 compiler, CMake >= 3.14 and git.
set -euo pipefail

OCIO_TAG="${OCIO_TAG:-v2.5.2}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"  # physical: never bake a symlinked path
TP="$HERE/third_party"
SRC="$TP/src-ocio"
BUILD="$TP/build-ocio"
PREFIX="$TP/install"
JOBS="${JOBS:-$(nproc)}"

export CC="${CC:-/usr/bin/gcc}"
export CXX="${CXX:-/usr/bin/g++}"

mkdir -p "$TP"
if [ ! -d "$SRC/.git" ]; then
  git -c advice.detachedHead=false clone --depth 1 --branch "$OCIO_TAG" \
    https://github.com/AcademySoftwareFoundation/OpenColorIO.git "$SRC"
fi
actual="$(git -C "$SRC" describe --tags --exact-match 2>/dev/null || echo unknown)"
if [ "$actual" != "$OCIO_TAG" ]; then
  echo "error: $SRC is at '$actual', expected $OCIO_TAG (delete it to re-clone)" >&2
  exit 1
fi

# A build tree or install configured from another checkout path (the repo was
# moved or renamed) bakes that path into CMakeCache.txt, the .pc file and the
# install manifest: start both over. The source clone is path-independent.
if [ -f "$BUILD/CMakeCache.txt" ] && ! grep -qxF "CMAKE_HOME_DIRECTORY:INTERNAL=$SRC" "$BUILD/CMakeCache.txt"; then
  echo "build tree was configured for another path; rebuilding from scratch"
  rm -rf "$BUILD" "$PREFIX"
fi
if [ -f "$PREFIX/lib/pkgconfig/OpenColorIO.pc" ] && ! grep -qxF "prefix=$PREFIX" "$PREFIX/lib/pkgconfig/OpenColorIO.pc"; then
  rm -rf "$PREFIX"
fi

# Keep the dependency search away from Homebrew/system copies so the result is
# reproducible: every ext package is built from source by OCIO.
cmake -S "$SRC" -B "$BUILD" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$PREFIX" \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
  -DCMAKE_IGNORE_PREFIX_PATH=/home/linuxbrew/.linuxbrew \
  -DBUILD_SHARED_LIBS=OFF \
  -DOCIO_INSTALL_EXT_PACKAGES=ALL \
  -DOCIO_BUILD_APPS=OFF \
  -DOCIO_BUILD_TESTS=OFF \
  -DOCIO_BUILD_GPU_TESTS=OFF \
  -DOCIO_BUILD_DOCS=OFF \
  -DOCIO_BUILD_PYTHON=OFF \
  -DOCIO_BUILD_JAVA=OFF \
  -DOCIO_BUILD_OPENFX=OFF
cmake --build "$BUILD" --parallel "$JOBS"
cmake --install "$BUILD"
# A static OCIO needs its ext deps at link time; OCIO only installs them in the
# build tree, so copy them next to libOpenColorIO.a for build.rs.
mkdir -p "$PREFIX/lib/ocio-ext"
cp "$BUILD"/ext/dist/lib/*.a "$PREFIX/lib/ocio-ext/"
echo "OCIO $OCIO_TAG installed to $PREFIX"
