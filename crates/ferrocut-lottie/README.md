# ferrocut-lottie

Lottie animation layer for Ferrocut. [ThorVG](https://github.com/thorvg/thorvg)
(MIT) rasterizes on the CPU through its own C API (`thorvg_capi.h`); this crate
maps the graph's `RationalTime` to an exact Lottie frame, converts the 8-bit
output to Ferrocut's RGBA16F working format, and exposes a
`ferrocut_core::RenderNode` (`LottieNode`, a generator with no inputs).

## Build

```sh
crates/ferrocut-lottie/scripts/build-thorvg.sh   # ~20 s; no sudo
cargo test -p ferrocut-lottie --release
```

The script downloads the pinned release tarball, checks its SHA-256, applies
`patches/*.patch`, creates a private meson/ninja venv and installs a static
`libthorvg-1.a` into `third_party/install/` (all gitignored). Without it the
crate builds as an empty stub with a cargo warning, so the workspace still
builds. `FERROCUT_THORVG_PREFIX` points at a different install.

| Pinned | Version | License |
|---|---|---|
| ThorVG | 1.1.2 (`sha256 4ac22e76…80603cd`) | MIT |
| bundled in ThorVG: rapidjson | | MIT |
| bundled in ThorVG: JerryScript (Lottie expressions) | | Apache-2.0 |
| build-only: meson 1.9.1, ninja 1.13.0 (pip, private venv) | | Apache-2.0 |

System deps: a C++17 compiler, python3 (venv), curl, xz. ThorVG options:
`engines=cpu loaders=lottie,png,jpg,ttf bindings=capi partial=false simd=false
threads=true file=false extra=lottie_exp`, static.

## Time: only the graph's clock

`render(t)` takes layer-local `RationalTime` seconds. The Lottie frame is
`t × fr` computed exactly in i128 (`fr` is parsed from the JSON as a decimal,
e.g. `29.97` → 2997/100), quantized to **1/1000 frame** (ties toward +∞), and
handed to ThorVG relative to `ip`. Nothing in the crate reads a clock.

Why the 1/1000 grid: ThorVG rounds frames to 1e-4 and silently *skips* an
update when the new frame is within 0.0009 of the previous one, which would make
the output depend on what was rendered before. On a 1/1000 grid, different
requests are ≥ 0.001 apart and equal requests are bit-identical.

Outside `[0, op−ip)`: `EndBehavior::Hold` (default; clamps to `op−1`, same as
ThorVG), `Loop`, or `Transparent`.

## Determinism

The same `(document, params, t)` gives bit-identical output regardless of
render order, chunking, worker thread or process. That is the contract the
engine's chunked renders rely on. Two things in ThorVG needed fixing:

1. **Randomness** (`patches/thorvg-1.1.2-deterministic-random.patch`). Lottie
   expressions `random()`, `wiggle()`, JerryScript `Math.random()` and the
   text-selector "randomize" option all used libc `rand()`. That state is
   process-global and depends on history, so frame N rendered after frame N−1
   differed from frame N rendered cold. The patch swaps in a thread-local
   SplitMix64 that is reseeded on every expression evaluation from
   (stable per-expression seed, frame number). The seed hashes the expression
   code plus its position in document order. JerryScript's `Date` is compiled
   out upstream, so expressions cannot read the wall clock.
2. **Threading.** ThorVG's CPU engine and its expression engine are thread-affine:
   - The memory pool is indexed by ThorVG's internal task-thread id, which is
     0 for every external caller.
   - JerryScript contexts are keyed by `std::thread::id` while the real
     pointer lives in TLS. A recycled thread id hands a new thread a context
     with a null TLS pointer, which segfaulted under parallel tests.

   All ThorVG calls therefore run on **one dedicated thread**. Rasterization
   is serialized process-wide; the pixel conversion runs on the calling worker.

Other choices: CPU engine only, no partial (dirty-region) rendering, no SIMD.

## Color

ThorVG composites in 8-bit, premultiplied, sRGB-encoded space, the same way After
Effects does in 8-bit, so it matches the animator's look. Two output encodings
(`OutputEncoding`):

- **`AcesCg`** (default): unpremultiply, sRGB EOTF (exact piecewise),
  Rec.709 → AP1 (matrix from OCIO 2.5's built-in
  `cg-config-v4.0.0_aces-v2.0_ocio-v2.5`, Bradford), re-premultiply. Tagged
  `ACEScg`, like the rest of the working space, so no extra color node is needed.
  The EOTF is evaluated once into a 64K-entry table in f64. Per pixel the work
  is three lookups plus a fixed f32 3×3 matrix and round-to-nearest-even to f16,
  with no libm calls.
- **`SrgbEncoded`**: ThorVG's values /255, premultiplied, tagged
  `sRGB Encoded Rec.709 (sRGB)` for an explicit OCIO node downstream (ferrocut-color
  unpremultiplies before transforming).

## Tests (`tests/determinism.rs`)

- `same_time_renders_identical_bytes_twice`: same t on the same renderer
  (back-to-back, and after moving away), and on a fresh one.
- `chunk_boundary_matches_sequential`: 72 frames at 24 fps sampling a 30 fps
  Lottie. Three chunks are each rendered on a fresh renderer in reverse order,
  plus a backwards walk across the boundary, all compared to one sequential pass.
- `expressions_are_a_pure_function_of_time`: wiggle/random/Math.random layer,
  after a 40-frame history vs cold.
- `concurrent_workers_match_sequential` and `many_short_lived_worker_threads`.
- `end_behaviors`, `in_point_offset_maps_time_zero_to_ip`,
  `color_conversion_and_tags`, `bad_document_is_a_permanent_node_error`.
- `render_node_through_core_on_gpu`: the real `RenderNode` path. The upload
  round trip is byte-identical, and a cancelled token gives `ErrorKind::Cancelled`.
  Skips without a GPU.

## Layout

`src/adapter.rs` is the only file that touches `ferrocut-core`. `renderer.rs`,
`thorvg.rs` (FFI wrapper + ThorVG thread), `timing.rs`, `color.rs` (math in `ferrocut-colorspace`) and `meta.rs`
are core-independent.
