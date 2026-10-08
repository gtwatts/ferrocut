# Native masks, repeaters and measured motion

The second core-adoption milestone connects source-time mask stacks, editable
vector groups/repeaters, and EffectCraft point tracking/translation stabilization
to Ferrocut's normal edit and render workflow. It was built and checked on
2026-10-08. This report records the tested milestone before GitHub publication.

| Agent capability | What executes | Boundary |
| --- | --- | --- |
| Clip mask stacks | Actual FilmCraft path flatten/combine, adapted bounded signed-distance coverage, existing GPU alpha matte | Seven modes, animated Bézier/vector geometry, inversion, opacity, isotropic feather and expansion; no variable-width feather or effect-specific masks |
| Shape groups and repeaters | Actual EffectCraft matrices/path transforms, native vector coverage and paint | Nested editable shapes, skew, per-copy transforms/opacity, fractional copies and ordering; group opacity applies per descendant without isolation |
| Point tracking | Actual EffectCraft NCC/LK CPU tracker on original decoded pixels | Explicit seed, measured confidence, stopped/lost points, exact sample times; no planar/perspective or 3D camera solve |
| Translation stabilization | Actual EffectCraft Gaussian/no-motion translation corrections | Smooth or lock mode, ordinary editable position keys; transparent exposed borders, no crop/fill or mesh warp |

Masks and group numeric properties use **source time**, following speed/remap and
preserving animation across split/trim. The compiler applies masks before clip
effects and the layer transform. Group scale/opacity are percentages; mask
opacity is a fraction. Existing shape rasterization keeps its earlier pixel
behavior. Parameter discovery exposes bounded indexed paths, strict schemas,
defaults, units and animation clocks. Invalid batches leave the timeline intact.

`tracking_analyze` writes a new analysis file and never overwrites one.
`tracking_keyframes` reads it and returns normal `set_keyframes` operations.
Apply them with `edit_apply` for validation, history, undo and cache invalidation.
The planner checks project-root paths and full source-file hashes before edits.
It supports exact nonzero constant source speeds, including reverse; nonlinear
retime inversion and geometrically altered source pictures are explicit errors.
Source-pixel displacement scales per axis to the output canvas. Tracking JSON is
editable and consistency-checked; hashes are not signatures or authenticity proofs.

The MCP now exposes 26 tools, including these two new motion tools. Its resources
include `docs://integrations/native-masks.md`,
`docs://integrations/vector-instances.md`, and `docs://integrations/tracking.md`.
The upstream dependency closure remains 19 packages; three existing transitive
packages are now direct dependencies. This milestone does not import the private
upstream renderer or change its older, unconnected FilmCraft dependency pin.

## Reproduce the complete agent workflow

```sh
cargo build --locked --offline -p ferrocut-engine --bin ferrocut
python3 scripts/motion-showcase.py --output-dir /tmp/ferrocut-motion-demo
```

The output directory must be empty. The helper calls the public CLI to render an
original textured native vector source, analyze its motion, generate/apply keys,
render the attached overlay and a separately stabilized edit, change native title,
repeater and mask controls, render again, repeat from cache, undo, and render the
restored project. It checks the recorded frame counts and exact output bytes.

Inputs are [motion-source.json](../../examples/motion-source.json),
[motion-showcase.json](../../examples/motion-showcase.json),
[tracking settings](../../examples/motion-tracking.settings),
[tracking options](../../examples/motion-tracking.options), and
[motion-revise.ops](../../examples/motion-revise.ops). Typography uses an explicit
repository font. The script resolves font/media paths in its temporary working
timeline. Browser/CSS rendering is not involved.

## Executed workflow and visual review

The 2-second 640×360, 16 fps demonstration contains 32 frames. Its original
320×180 source moves a textured target exactly two pixels right and one down
per sampled frame. All 32 tracking samples are present; the 31 measured samples
have NCC confidence 100 and zero maximum axis error against this **synthetic**
ground truth. This does not establish tracking accuracy on real footage.

| Render | Rendered / reused frames | GPU submissions | Total time on llvmpipe |
| --- | --- | --- | --- |
| Attached masks/repeater/title | 32 / 0 | 32 | 4.644 s |
| Native style revision | 32 / 0 | 32 | 4.948 s |
| Repeat revised project | 0 / 32 | 0 | 0.019 s |
| Undo to attached original | 0 / 32 | 0 | 0.019 s |
| Translation-stabilized edit | 32 / 0 | 32 | 4.724 s |

Revision and repeat files are byte-identical. Original and undo files are
byte-identical. The changed title and 12→18 repeated markers visibly differ;
the feather setting changes while the ring/crosshair remains attached to the
moving source target. The stabilized source target stays in place across the
inspected start/end frames, with exposed borders visible. This is a technical
integration review, not a creative production review or an Adobe comparison.

Retained reports, editable snapshots, analysis, image inspections, source/input
hashes and logs are indexed by
[the artifact manifest](artifacts/native-motion/manifest.json).
The six video masters are retained in
[the media artifact folder](artifacts/native-motion/media/). The manifest keeps
their original `/tmp/ferrocut-motion-repro-20261008` execution paths as provenance;
`repository_path` identifies each backed-up copy. The helper reproduces them.
Frame extraction
uses the installed FFmpeg skill only for external QA; the application renders
native geometry, text, masks and transforms itself.

## Validation and remaining work

The final four-package regression run passed **417 tests, with zero failures and
one ignored doctest**, across 60 target reports. All-target Clippy passed with
warnings denied, scoped formatting passed for 61 changed/untracked project Rust
files, and the 456-ID capability ledger and its 24 validator cases passed.
Exact commands and logs are retained with the artifact manifest. Checks cover
independent mask-distance/Bézier/combination math,
negative overscan, UHD feathering, nested transforms, anisotropic strokes and
gradients, animated radial repeaters, source clocks, split/trim/retime, cache
invalidation, cancellation, real FFV1 tracking, occlusion, confidence/failure,
stale-source rejection, path escapes, strict schemas, normal edits and undo.
An additional overlapping 35-test run exercised native mask rendering and vector
upload on NVIDIA RTX 5090 Laptop Vulkan and llvmpipe CPU Vulkan. Vector-group
timeline checks used llvmpipe; the two later independent geometry tests are in
the final regression. This does not assert bitwise cross-adapter output.

The whole workspace is not newly certified: the previously recorded parallel
color-crate GPU failure and Chromium sandbox limitation remain outside this
four-package regression scope. The capability ledger keeps broad tracking,
stabilization, effect-specific masks and variable feather incomplete. Planar and
two-point rotation/scale tracking, tracked mask geometry, real-footage drift
validation, wider retained audio/export adapters, and production review remain.

Detailed contracts: [masks](native-masks.md),
[groups/repeaters](vector-instances.md), [tracking](tracking.md),
and [first core adoption](STORYTOLD.md).
