# Native media placement: first implementation slice

This implements the core of the accepted [media-fit design](../design/2026-10-08-media-placement-fit.md), starting from `e5a2e242af71d9f2b67ed0024393e3552d4deda8`. The source and rendered-output revision is `e44a43f30585db55c1641c4b7fb47f2c11f5fd1a`. The installed before binary was revision `5de843447b5965ecbd310809497695f726887afc`; intervening shared commits were documentation. Independent Codex review accepted this technical slice after source and output inspection. This is not full-film creative acceptance or completion of the full design.

## Defect and change

`SourceNode` previously decoded every source into the sequence dimensions before applying the layer transform. A 1280x534 image in a 1920x1080 sequence therefore received different horizontal and vertical scales. The production explainer's card and monitor footage used uniform user scale, preserving that distortion.

Sources now decode at their native size. Clip `fit` overrides `output.fit`, with `contain` the default. `cover`, `none`, and `stretch` are also supported. Fit factors use exact rational arithmetic; the existing premultiplied transform kernel combines fit with the user transform. Anchors and clip masks/effects use source pixels, positions use output pixels. Generators and adjustment layers retain output coordinates.

The source center and output center are distinct. Both affine and projective/motion-blur paths allocate an output-sized display, including transparent and false-identity cases. Equal-size source/output layers retain the old graph and key bytes. Proxies retain the original source dimensions; differently sized nested compositions receive an outer placement. Native layer dimensions inform worker sizing and GPU device limits.

Agent discovery includes strict timeline/add-clip schema fields, fit parameters, updated coordinate documentation, and base placement factors plus stretch warnings in plan/render results. Anchor component edits use probed dimensions after MCP path preflight. Interchange exports baked fit factors and imports explicit native placement; unsupported FCP7 scale-to-frame equivalence remains a reported loss.

## Scope boundaries

The following accepted-design work remains: `punch_in`; transformed/animated-source tracking; per-edit, timeline-get, and media-status placement diagnostics; preview badges; per-frame bounds and visibility diagnostics; native-anchor expression size lookup; additional registered CI/evaluation timelines; decode pre-reduction. Native anchor expressions must currently provide an explicit pre-expression value, and references to an unset media anchor return a clear error. FCP7 scale-to-frame export fidelity still needs a real external-application check. Pixel-aspect handling remains the existing square-source boundary.

Old mismatched timelines may contain per-axis compensation. They need migration: remove that compensation for uniform fitting, or use `fit: stretch` to restore old framing. Convert old output-pixel anchors/masks to source pixels. Stretch restores framing, not identical resampling pixels. Older installed binaries reject the new fields; source implementation does not update an already running MCP server.

## Final technical evidence

Evidence directory in the isolated worktree: `out/fit-evidence/`, under the shared repository's `out/production-cycle-20261009/worktrees/engine/`. It is local and ignored. `manifest.json` binds source and binary revisions, exact check commands, render results and inspection scope; `artifacts.sha256` binds the retained masters, stills and reports. Media copies retain source attribution in `SOURCES.md` and `sources.sha256`.

- Owned-crate formatting and Clippy with `--all-targets -- -D warnings` passed. The initial call-site compilation failures are retained separately.
- `cargo test --workspace --locked --offline --exclude ferrocut-deliver -- --test-threads=1`: **535 passed, 2 ignored**. Fit factors, anchor defaults, cache invalidation, native masks, mismatched affine/3D/blur displays, nested rendered pixels, the 641x361 original / 322x181 proxy case, and fit-aware tracking passed.
- `cargo test --locked --offline -p ferrocut-deliver --lib --bins -- --test-threads=1`: **8 passed**. No codec download was enabled. Optional OCIO/CEF/OFX/ThorVG integrations built their documented stubs; they were not newly validated.
- Release builds of engine, MCP and perceive passed, using the repository's **LGPL FFmpeg 9.0.2**. SHA-256: CLI `9d6e2f68b63c91084c1e0a2766ff9515acd5620283fa00c0415592ca468e28e7`; MCP `eac115732e1aa2da0753e8046cefc59946c214f8acfd90ba2c45744b32673ce3`.
- Actual baseline and release MCP subprocesses executed capability discovery and plans. The complete chunk-key arrays for the same-size transformed fixture were identical. The production fixture reported exact `3/2` fit factors on both axes. This is a concrete fixture check, not a universal key-equivalence proof.

### Rendered production defect

The production fixture `explainer-placement.json` retains card2-footage's `1/5` scale and mon-t5's `1/3` scale, using unchanged CC BY 3.0 Tears of Steel source clips. It simplifies surrounding animation to isolate the defect; it is not a replacement film. Both native FFV1 masters are **48 frames, 24 fps, 1920x1080, 2 seconds, intentionally without audio**.

| Retained master | SHA-256 |
| --- | --- |
| `explainer-before.mkv` (installed 5de8434) | `c33eae0f120969c789fd85695710eba0710f4dc399851afe89b671894ff23221` |
| `explainer-after.mkv` (source e44a43f) | `7f597a20bd1ee12d5cf72b5b255a622c923885f1444f04f9c616405d1da10533` |

The after master is byte-identical with one and two workers in separate fresh caches. A warm-cache repeat reused all four chunks / 48 frames with zero GPU submissions and the same hash.

The maker inspected comparison sheets at 0.5 and 1.5 seconds and a native-size monitor still. Widths and centers stay fixed; vertical stretching disappears. Analytical picture sizes change from 384x216 to **384x160.2** for the card and 640x360 to **640x267** for the monitor. Native-size decoded stills measured nonblack support of **386x218 → 386x162** and **642x362 → 642x268**, respectively, including filter fringes. These are fixture-specific pixel bounds, not a general photographic subject detector. Exact thresholds and rectangles are in `measured-bounds.json`.

### Unchanged demos and independent review

`JOBS=1 PERCEIVE_CHECK=1 ci/render-check.sh` passed; its second pass uses two workers and fresh caches. No reference file changed. Demo retained 288 frames; demo-av retained 312. Both video-stream hashes exactly match the committed references, and all 25 sampled SSIM values are 1.0. Demo-av's audio Blake3 remains `751eda4b8d2d1d61f6afa79ed84d3e6a3161df1eb448ba87ad5b4bc11f208a8a`, and its quality check passed.

The independent reviewer inspected source e44a43f, verified the release CLI hash, decoded fixture frames **0, 23, 24 and 47**, confirmed the expected picture bounds and identical worker-count masters, and independently measured demo-av's **624,000 stereo samples per channel at 48 kHz / 13 seconds**. Both CI renders decode to PCM SHA-256 `7727a68785f7be6994a9604d4da24e208331b36888669ed9995282d6e9158022`. The reviewer read the maker's source-test logs; those were not independent test reruns. Review evidence is retained at `out/production-cycle-20261009/review/checkpoint-20261009/engine-output-evidence.json` in the shared repository.

No continuous playback or listening is claimed. Full-film production, integrated installed-route checks, and Gordon's viewing/listening judgment remain separate. Shared binary/MCP installation belongs to the integration lead. Viewing MP4s were not created by this slice: the existing delivery route requires OpenH264 2.6, its local cache was absent, and the checked system library was 2.4.1. No codec download, enable choice or global setting was changed; native masters and PNG comparisons are retained.
