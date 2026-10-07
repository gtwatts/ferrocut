# cutline-color

OpenColorIO v2 color transforms as Cutline render nodes on the GPU.

```
OCIO processor ──► GpuShaderDesc (GLSL_VK_4_6 + LUTs) ──► rewrite for WebGPU ──► naga (GLSL→IR→WGSL) ──► wgpu compute pass
       └──────────► CPU processor (reference / fallback)
```

* `cpp/ocio_shim.{h,cpp}`: a thin C ABI over OCIO (no C++ exceptions cross it), compiled by `build.rs` with `cc`. Bindings are hand-written (`src/ffi.rs`); the ABI is ~20 functions, so we skip bindgen/libclang.
* `src/glsl.rs`: OCIO emits Vulkan GLSL with combined `samplerND` LUTs. WebGPU has no combined image samplers, so each one becomes a `textureND` + `sampler` pair (sampler binding = texture binding + 64), and each `texture(lut, uv)` becomes `textureLod(samplerND(tex, smp), uv, 0.0)` (compute shaders have no implicit LOD). The function is wrapped in an 8×8 compute shader that **unpremultiplies → OCIOMain → re-premultiplies** (alpha passes through; alpha = 0 stays black), then naga parses, validates and writes WGSL.
* `src/gpu.rs`: uploads OCIO's 1D/2D/3D LUTs (`R32Float`/`Rgba32Float` when the device has `FLOAT32_FILTERABLE`, else f16) and runs the pass into a `Rgba16Float` storage texture (`cutline_core::Frame::new_gpu`).
* `src/node.rs`: `OcioTransformNode` implements `cutline_core::RenderNode`. The content hash covers the node version, OCIO version, the processor's cache ID (derived from the transform's content), the input/output tags and the LUT-bake setting. The input frame's color space tag must match the node's source space, or the render returns a `NodeError`.

## Build

System requirements: a C++17 compiler, CMake ≥ 3.14, git, and network access **once** to fetch OCIO's sources. No system `-dev` packages and no sudo are needed.

```sh
crates/cutline-color/scripts/build-ocio.sh      # ~3-5 min; builds into crates/cutline-color/third_party/install (git-ignored)
cargo test -p cutline-color -- --nocapture     # needs a wgpu adapter (any Vulkan GPU; lavapipe works)
```

`OCIO_ROOT=<prefix>` points `build.rs` at another static OCIO install (same layout: `lib/libOpenColorIO.a` + `lib/ocio-ext/*.a`). If OCIO isn't built, `build.rs` prints a warning and compiles an empty stub crate (`cfg(cutline_no_ocio)`), so `cargo build --workspace` still works on a fresh clone. `CUTLINE_ADAPTER=<name substring>` picks the GPU used by the tests.

## Pinned versions / licenses

| Component | Version | License | How |
|---|---|---|---|
| OpenColorIO | v2.5.2 | BSD-3-Clause | static, built by `scripts/build-ocio.sh` |
| OCIO ext deps (yaml-cpp, pystring, expat, Imath, minizip-ng, zlib) | as pinned by OCIO v2.5.2 | MIT / BSD-3 / MIT / BSD-3 / Zlib / Zlib | built by OCIO's CMake (`OCIO_INSTALL_EXT_PACKAGES=ALL`) |
| naga, wgpu | 30 | MIT OR Apache-2.0 | crates.io |

Nothing third-party is committed; `third_party/` is git-ignored.

## Tests (`tests/gpu_vs_cpu.rs`)

* ACEScg → `sRGB - Display` / `ACES 2.0 - SDR 100 nits (Rec.709)` (built-in `cg-config-v4.0.0_aces-v2.0_ocio-v2.5`; uses two LUT textures) GPU vs OCIO CPU on a 67×45 premultiplied f16 pattern with exposures up to 4.0 and alphas of 1, .75, .5, .25 and 0.
* ACEScg → `sRGB - Texture` (analytic only, no LUTs).
* A user 17³ tetrahedral 3D LUT (3D texture path).
* The `RenderNode` contract: hash stability, two renders bit-identical on one device, and a rejected color-space mismatch.

Tolerance: max |gpu−cpu| / max(1, |cpu|) < 2e-3 (the output frame is f16, whose step is 4.9e-4 at 1.0).
