# Contributing to Ferrocut

Ferrocut is an Apache-2.0, agent-native video editor in Rust. This page covers setup, the checks CI runs, and
the house rules.

## Who owns what

See [Crate ownership](README.md#crate-ownership).

- **Rusty:** `ferrocut-types`, `ferrocut-core` (shared with SeePlus), `ferrocut-audio`, `ferrocut-engine`,
  `ferrocut-mcp`, `ferrocut-build` (build-script helpers: FFmpeg lookup + relocatable rpaths, for any crate
  that links FFmpeg).
- **SeePlus:** `ferrocut-color`, `ferrocut-colorspace`, `ferrocut-ofx`, `ferrocut-ipc`, `ferrocut-lottie`,
  `ferrocut-html`, `ferrocut-perceive`, `ferrocut-deliver`, and their `scripts/`.

Changes to someone else's crate go in a separate commit labelled `[<owner> review needed]`.

## Setup (no sudo needed beyond the distro packages)

```sh
# System: Rust 1.98+, a C/C++17 toolchain, cmake, pkg-config, git, curl, python3 (+venv),
# and for CPU rendering Mesa's Vulkan drivers (Ubuntu: mesa-vulkan-drivers libvulkan1).
./scripts/build-ffmpeg-lgpl.sh            # LGPL-only shared FFmpeg -> third_party/ffmpeg-lgpl (~5 min)
./scripts/gen-test-media.sh               # deterministic synthetic media -> media/
# Optional (SeePlus's crates build as stubs without these):
crates/ferrocut-color/scripts/build-ocio.sh     # OpenColorIO (static)
crates/ferrocut-ofx/scripts/fetch-deps.sh       # OpenFX + libexpat sources
crates/ferrocut-lottie/scripts/build-thorvg.sh  # ThorVG (static)
crates/ferrocut-html/scripts/fetch-cef.sh       # CEF minimal (~310 MB download)
cargo build --release
```

`.cargo/config.toml` points pkg-config at `third_party/ffmpeg-lgpl`. The `ferrocut` and `ferrocut-mcp` binaries
carry a relocatable RUNPATH, so they need no `LD_LIBRARY_PATH`. The `ffmpeg` CLI in `third_party` does.

## Checks (what CI runs)

CI is `.github/workflows/ci.yml` on `ubuntu-latest` with no GPU: wgpu runs on Mesa lavapipe. Locally:

```sh
cargo fmt --check -p ferrocut-types -p ferrocut-core -p ferrocut-audio -p ferrocut-engine -p ferrocut-mcp -p ferrocut-build
cargo clippy --locked -p ferrocut-types -p ferrocut-core -p ferrocut-audio -p ferrocut-engine -p ferrocut-mcp -p ferrocut-build \
  --all-targets -- -D warnings
cargo test --workspace --locked --exclude ferrocut-deliver
cargo test --locked -p ferrocut-deliver --lib --bins   # see "OpenH264" below

# The same tests on lavapipe only, as on CI (hides every other GPU from wgpu):
WGPU_BACKEND=vulkan VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json cargo test --workspace --locked
# (the ICD file is lvp_icd.x86_64.json on Ubuntu)

cargo build --release -p ferrocut-engine -p ferrocut-mcp -p ferrocut-perceive
ci/render-check.sh       # CPU render of examples/demo.json + demo-av.json
```

`ci/render-check.sh` checks:

- **Determinism:** each demo is rendered twice with different `-j` into fresh caches, and the two outputs must
  be bit-identical.
- **Frame counts** match the reference.
- **Audio:** exact blake3.
- **Video:** SSIM against the committed reference frames in `ci/reference/`, with a mean of at least 0.999 and
  no frame below 0.995.
- **Quality:** `ferrocut check --require` on demo-av when `ferrocut-perceive` sits next to `ferrocut`.

Video is not checked by exact hash, because lavapipe's output depends on the Mesa/LLVM version. For example,
Ubuntu 24.04's Mesa 25.2.8 reproduces demo.json bit for bit but not demo-av.json (SSIM 0.99998). If a change
alters the render on purpose, run `ci/render-check.sh --update` and say in the commit message why the reference
changed.

Validate workflow edits with [actionlint](https://github.com/rhysd/actionlint). The job can also be run in
Docker with [act](https://github.com/nektos/act):
`act -j test -P ubuntu-latest=catthehacker/ubuntu:act-24.04`.

### OpenH264 (delivery)

CI never downloads Cisco's OpenH264: enabling it is the user's explicit choice. `ferrocut-deliver`'s
integration test downloads it whenever it is online, so CI excludes that one target; the crate's unit tests
still run. `FERROCUT_OPENH264_URL` points at a dead port as a backstop. The engine and MCP delivery tests
encode only with a codec you already provide, via `FERROCUT_OPENH264_LIB=/path/libopenh264.so.2.6.0` (add
`FERROCUT_OPENH264_UNVERIFIED=1` for a non-Cisco build). Otherwise they print `SKIP`.

## Engine rules

- **Determinism:** renders are bit-exact run to run and independent of `-j`. If a change alters the demo hashes
  in the README, the commit must explain why.
- **Frames are premultiplied alpha** end to end.
- **Time is exact rationals**, never floats, in timelines and edit ops.
- **GPU tests skip cleanly** (print `SKIP` and return) when no adapter exists, and must pass on lavapipe.
  Keep GPU jobs modest (`-j 4`): the dev machine shares its GPU with other workloads.

## Licensing

- Ferrocut is Apache-2.0.
- New Rust dependencies must be MIT and/or Apache-2.0 (or compatible permissive).
- FFmpeg is linked dynamically and must stay LGPL-only: no `--enable-gpl`, no `--enable-nonfree`, and never
  x264/x265.
- C/C++ deps fetched by scripts land in gitignored `third_party/` directories and are never committed.

## Commits

- Commit small, self-contained changes with a message explaining the why.
- Stage only your own paths explicitly. Never use `git add -A` or `git commit -a`: the tree may hold other
  people's work in progress.
- If you hit `.git/index.lock`, wait and retry; don't delete it.
- Run the checks above before pushing.

## MCP server

`ferrocut-mcp` refuses any path outside its project root (`--root`, `$FERROCUT_MCP_ROOT`, or the cwd). See the
[README](README.md#mcp-server-ferrocut-mcp) for tools, progress and cancellation.
