# Native vector source-bounds repair

The synthetic off-canvas L in [the fixture](../../examples/vector-source-bounds.json)
now survives translation and rotation into the frame. The baseline discarded its
paint during source rasterization, before the clip transform could move it.

Validated source: `f82eb428467b5599e4c7dc3255312d621744098c`, implementation
`e17ba0614ff00f46fc685fcd9c8055111183d9b4`, based on
`3ad837219d07d482f077b98889e710d7176c6c28`. The installed baseline was
`73935ba661c0772ca5b464fa6aa9c2a8f89626e3`; its backend, Cargo files and CI
matched the branch base. The final documentation commit does not change the
validated source. Independent static and technical review accepted `f82eb42`.
The reviewer checked every retained frame's polygon footprint, eight PNGs,
output/binary hashes, full keys, cache/undo/root behavior and unchanged CI.
Receipt: `out/production-cycle-20261009/cycle3/review/engine-technical-f82eb42.json`,
SHA-256 `1507995b36e99b4297edf7e1efc5ce00d205c90344b6d7f944e107bb96fdb81f`.

## Behavior and bounds

Native shape and vector-group nodes prepare operated fill paths and actual local
stroke outlines, apply group transforms, and retain a checked signed data window.
Its bounds include the declared display and painted extent. Logical display,
default anchor, fit, source time and clip-local transform time keep their existing
meaning. Gradient samples map from offset storage back to source/local space.
The existing CPU viewport helpers retain their clipped indexing contract.

The [contract](../design/2026-10-09-vector-source-bounds.md) defines the limits.
Signed coordinates, pixel allocation, instance work and selected GPU dimensions
are checked before image allocation/upload. Native shape and group source-key
versions change once (`vector.v3.source-bounds` and `vector.instances.v2.source-bounds`);
old clipped vector chunks cannot be reused. Nonvector source keys are unchanged.

## Actual before/after output

Both lossless FFV1 masters contain 48 frames at 24 fps over two seconds, 128 by 96
pixels, with no authored audio. Original synthetic material only.

| Frames | Baseline | Repaired painted region, exclusive upper bounds |
| --- | --- | --- |
| 0–23 | Background only | Translated L: `[16,16,48,48]`, 544 pixels |
| 24–47 | Background only | Rotated L: `[72,32,104,64]`, 544 pixels |

All 48 decoded RGB frames were compared with the analytic L geometry. The maker
also inspected before/after PNGs at frames 0 and 24. The ordinary in-frame control
moves the source geometry +64 pixels and compensates its clip position/anchor;
all its decoded pixels are identical before/after and match the repaired fixture.

One-worker, two-worker and warm-cache repaired masters are byte-identical. A
journaled CLI edit changes the first shot's geometry and position, rebuilds 24
frames and reuses 24. Undo restores exact canonical timeline bytes and keys,
reuses all 48 original frames, and restores the original repaired master bytes.
Fresh baseline and repaired MCP subprocesses verify full chunk keys, nonvector
key equality, the same edit/undo behavior and outside-root rejection. CLI plan
displays only key prefixes; full-key comparisons use MCP and render reports.

SHA-256 identities:

| Artifact | SHA-256 |
| --- | --- |
| Baseline CLI | `38603e7af5a92882a7ba9f8126adb4ccfdc418bdbf6bc9a1042ec0dd3f94c247` |
| Repaired CLI | `25d1fa9fe9d98f83babf7b127fd25ada83cda9c89a31c5ac1326731eec4929aa` |
| Repaired MCP | `58f4df3dbccc2721aa5f03971bfa772bbd99b51c31c2f8e80a38cbbb7008b829` |
| Before master | `429e3396f90e626f3566e9081eade9c5cd405b188aee4778a8f75224254f80f4` |
| After master, both job counts and warm cache | `ee1d37617cc2b17a47763198aae06a334d230075100d7e3b581d073e98ef454c` |

## Checks and retained evidence

All required CONTRIBUTING checks passed at the validated source:

- Formatting and Clippy with warnings denied; locked offline release build of
  engine, MCP and perceive.
- Workspace excluding delivery: 619 passed, 2 ignored documentation examples.
  Existing OCIO, CEF, ThorVG and OpenFX dependencies were included with no stub
  warnings. Delivery lib/bins: 8 passed. No OpenH264 download or enable action.
- Nine focused source-bounds regressions passed, including half-alpha graph
  pixels, all four sides, miter/cap/curve fringe, gradients, repeater bounds,
  negative-source masks/adjacent matte, GPU limit rejection, edit/undo and nested
  native display/fit/anchor. The focused GPU cases used the CPU adapter without
  skips. The initial run's unsupported point-leaf edit path failed in the test;
  its retained log has eight passes and one failure. The test-only follow-up uses
  the existing whole-geometry operation and passes all nine.
- Release fixture/route harness: 40 commands, 94 passing assertions.
- Unchanged `ci/render-check.sh` and references: 288-frame demo and 312-frame
  demo-av each byte-identical across one/two-worker runs; SSIM mean/min 1.0;
  reference video hashes exact. Demo-av retains 624,000 stereo samples at 48 kHz,
  exact reference audio hash, and a passing required quality report.

Local retained evidence is under
`out/production-cycle-20261009/cycle3/engine-vector-bounds/`:

- `checks.jsonl`, named logs and `source-check-totals.json` retain commands,
  completion statuses, counts and the initial failure.
- `release-evidence/` retains both native masters, controls, decoded RGB, PNGs,
  actual CLI output, full MCP discovery/responses, journals and the output manifest.
- `ci-render/` retains both job-count masters/reports, SSIM records and quality JSON.
- `final-manifest.json` binds 156 evidence files, source and binary hashes,
  unchanged CI script/reference hashes, inspection scope and released heavy lease.
  Manifest SHA-256:
  `a80eab2bcbeb218d109df18d0518a7116d909d29550d1428f7525632dfcaaaf9`.

To reproduce the public fixture after building, run:

```sh
target/release/ferrocut render examples/vector-source-bounds.json \
  -o out/vector-bounds-j1.mkv --cpu -j 1 --cache-dir out/vector-cache-j1 --force
target/release/ferrocut render examples/vector-source-bounds.json \
  -o out/vector-bounds-j2.mkv --cpu -j 2 --cache-dir out/vector-cache-j2 --force
```

## Scope of acceptance

This evidence covers the native vector repair and unchanged demo regressions.
It does not establish private-film creative acceptance or continuous playback/
listening. Effect overscan, collapsed transforms, composition-boundary policy,
reusable/nonadjacent mattes, independent matte visibility and 3D redesign remain
outside this slice. The worktree binaries were exercised directly; shared
integration, installed MCP verification and installation remain with the lead.
