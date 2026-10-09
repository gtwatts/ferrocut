# Native media placement: first implementation slice

This implements the core of the accepted [media-fit design](../design/2026-10-08-media-placement-fit.md), starting from `e5a2e242af71d9f2b67ed0024393e3552d4deda8`. The installed before binary was revision `5de843447b5965ecbd310809497695f726887afc`; intervening shared commits were documentation. This checkpoint is under independent review; it is not creative acceptance or completion of the full design.

## Defect and change

`SourceNode` previously decoded every source into the sequence dimensions before applying the layer transform. A 1280x534 image in a 1920x1080 sequence therefore received different horizontal and vertical scales. The production explainer's card and monitor footage used uniform user scale, preserving that distortion.

Sources now decode at their native size. Clip `fit` overrides `output.fit`, with `contain` the default. `cover`, `none`, and `stretch` are also supported. Fit factors use exact rational arithmetic; the existing premultiplied transform kernel combines fit with the user transform. Anchors and clip masks/effects use source pixels, positions use output pixels. Generators and adjustment layers retain output coordinates.

The source center and output center are distinct. Both affine and projective/motion-blur paths allocate an output-sized display, including transparent and false-identity cases. Equal-size source/output layers retain the old graph and key bytes. Proxies retain the original source dimensions; differently sized nested compositions receive an outer placement. Native layer dimensions inform worker sizing and GPU device limits.

Agent discovery includes strict timeline/add-clip schema fields, fit parameters, updated coordinate documentation, and base placement factors plus stretch warnings in plan/render results. Anchor component edits use probed dimensions after MCP path preflight. Interchange exports baked fit factors and imports explicit native placement; unsupported FCP7 scale-to-frame equivalence remains a reported loss.

## Scope boundaries

The following accepted-design work remains: `punch_in`; transformed/animated-source tracking; per-edit, timeline-get, and media-status placement diagnostics; preview badges; per-frame bounds and visibility diagnostics; native-anchor expression size lookup; additional registered CI/evaluation timelines; decode pre-reduction. Native anchor expressions must currently provide an explicit pre-expression value, and references to an unset media anchor return a clear error. FCP7 scale-to-frame export fidelity still needs a real external-application check. Pixel-aspect handling remains the existing square-source boundary.

Old mismatched timelines may contain per-axis compensation. They need migration: remove that compensation for uniform fitting, or use `fit: stretch` to restore old framing. Convert old output-pixel anchors/masks to source pixels. Stretch restores framing, not identical resampling pixels. Older installed binaries reject the new fields; source implementation does not update an already running MCP server.

## Checkpoint evidence

Evidence directory in the isolated worktree: `out/fit-evidence/`. It is local and ignored; media copies retain their source attribution in `SOURCES.md` and SHA-256 manifest.

- Clippy passed after the initial call-site corrections; the original failure log is retained.
- The first workspace test run is still completing at checkpoint time. Placement, edit/default, native-mask, fit pixel, transform, cache, proxy and nested tests executed so far passed on the CPU Vulkan adapter. Added odd-proxy, nested-pixel and tracking-fit cases need the final focused run.
- Final formatting/Clippy/workspace/delivery checks and the release build are pending.
- Actual before/after videos and unchanged-demo render checks are pending. No playback or listening claim is made.

The production fixture `explainer-placement.json` retains card2-footage's `1/5` scale and mon-t5's `1/3` scale, using unchanged CC BY 3.0 Tears of Steel source clips. It simplifies surrounding animation to isolate the defect; it is not a replacement film. Full professional acceptance remains with production review and Gordon's viewing/listening judgment.
