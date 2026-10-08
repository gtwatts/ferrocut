#!/usr/bin/env bash
# Build whisper.cpp (MIT) user-space into third_party/whisper.cpp for
# `ferrocut index`. CUDA if a CUDA toolkit is found (nvcc on PATH or
# $CUDA_HOME), else CPU. No sudo; nothing installed outside the repo.
#
#   scripts/build-whisper.sh            # auto (CUDA if available)
#   WHISPER_CUDA=0 scripts/build-whisper.sh   # force CPU
#   CUDA_ARCH=120 scripts/build-whisper.sh    # GPU arch (default: native)
#
# Result: third_party/whisper.cpp/build/bin/whisper-cli (FERROCUT_WHISPER_CLI
# overrides the path at run time). Models: scripts/fetch-whisper-model.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
TAG="${WHISPER_TAG:-v1.9.5}"
SRC=third_party/whisper.cpp
if [ ! -d "$SRC/.git" ]; then
  git clone --depth 1 --branch "$TAG" https://github.com/ggml-org/whisper.cpp "$SRC"
else
  have="$(git -C "$SRC" describe --tags --exact-match 2>/dev/null || true)"
  if [ "$have" != "$TAG" ]; then
    git -C "$SRC" fetch --depth 1 origin tag "$TAG"
    git -C "$SRC" checkout -q "$TAG"
  fi
fi

cuda=OFF
if [ "${WHISPER_CUDA:-auto}" != 0 ]; then
  nvcc_bin="$(command -v nvcc || true)"
  [ -z "$nvcc_bin" ] && [ -n "${CUDA_HOME:-}" ] && nvcc_bin="$CUDA_HOME/bin/nvcc"
  if [ -n "$nvcc_bin" ] && [ -x "$nvcc_bin" ]; then
    cuda=ON
    root="$(cd "$(dirname "$(readlink -f "$nvcc_bin")")/.." && pwd)"
    export CUDAToolkit_ROOT="${CUDA_HOME:-$root}"
    CUDACXX="$(readlink -f "$nvcc_bin")"
    export CUDACXX
  elif [ "${WHISPER_CUDA:-auto}" = 1 ]; then
    echo "WHISPER_CUDA=1 but no nvcc found" >&2; exit 1
  fi
fi
echo "whisper.cpp $TAG, CUDA=$cuda"
args=(-S "$SRC" -B "$SRC/build" -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF
      -DWHISPER_BUILD_TESTS=OFF -DWHISPER_BUILD_SERVER=OFF -DGGML_CUDA="$cuda")
if [ "$cuda" = ON ]; then
  args+=(-DCMAKE_CUDA_ARCHITECTURES="${CUDA_ARCH:-native}")
fi
cmake "${args[@]}"
cmake --build "$SRC/build" --config Release -j "${JOBS:-$(nproc)}" --target whisper-cli
"$SRC/build/bin/whisper-cli" --help >/dev/null 2>&1 || true
echo "built $SRC/build/bin/whisper-cli (CUDA=$cuda)"
