# D1 native planar depth and camera contract

Source checkpoint, 2026-10-09. Execution and perceptual acceptance remain pending.
This implements an opt-in `depth_layers_v1` renderer for consecutive planar 3D
tracks. Existing `legacy` projects retain the old homography/painter path and
camera hashing when new controls are absent. The capability ledger is unchanged.

Source-space clip time mapping, masks, opacity and clip effects feed a scene
node before projection. Native fit/default anchors and signed data-window corners
map through the existing orientation·Rx·Ry·Rz·scale convention. Nested scenes
remain textures; source-in/speed/remapping are evaluated before the outer plane.
Normal premultiplied composition applies to resolved scene outputs in track order.

The GPU rasterizes two triangles per card and uses a bounded depth-peeling pass
for every active card (maximum 16 authored surfaces per run). Depth32 values are
ordered nearest first; exact equal values use the higher track/start ordinal
first. Every next peel admits a farther depth or a lower ordinal at the same
depth. Zero-alpha texels and an exhausted sentinel are discarded. There is no
alpha threshold, approximate transparency or depth epsilon. Shared-edge raster
ownership and stable repeated triangle interpolation prevent a single plane
from being visited twice. Sampling is bilinear; geometry edges are single-sample.
Colors accumulate front-to-back in linear premultiplied RGBA16F.

The camera is two-node position/point-of-interest, with an explicit world-up
reference (default `[0,-1,0]`) and roll around its forward axis (default 0°).
Positive roll rotates the camera basis toward screen-down, so scene pixels
rotate oppositely. Near/far default to 1 and 100000 camera-space pixels and must
satisfy `0 < near < far`. Projection uses homogeneous WebGPU clipping, including
partial cards at the near plane. Invalid axes fail at authoring time where
observable and at every rendered shutter sample, with no fallback axis.
Sources in one scene require a common positive PAR; camera width and layer x
coordinates use that same PAR. Exactly singular projected geometry is omitted
with a once-per-clip/worker warning; no arbitrary near-zero test removes a small
valid card. Nonfinite f64/f32 geometry errors explicitly.

Visible unconsumed 2D tracks split scenes even when their clips have a gap.
Hidden tracks and consumed adjacent-matte sources do not split scenes. Every
clip in a 3D track must be 3D; a run may contain at most 16 authored clips, not
just 16 active clips. Within a run D1 rejects non-normal blending, per-track
post-projection effects, adjustment/dissolve clips, overlapping clips on one
track and 3D matte participation. IDs and a nesting alternative accompany those
errors. Explicitly author a texture containing the effect/matte, or put effects
after a nested scene. `unnest` refuses a depth parent or child to avoid silently
merging scene groups or losing a camera; this is a stated D1 editing restriction.

Every card in a blurred run must have the same layer motion-blur switch. Exact
rational shutter samples jointly resolve all card/camera poses and visibility,
then complete sample images are averaged in fixed order. Texture content, masks,
clip opacity/effects are held nominal. At a cut where a card is visible only at
shutter samples, the nearest active sample supplies content (earlier on ties).
This preserves visible cut contributions without pretending to blur animated
content. Disabled blur uses one sample.

Scene keys include a versioned mode/filter/depth contract, output dimensions,
camera controls, evaluated active poses/ordinals/visibility and pulled texture
keys. Camera animation is conservatively hashed in full with sampled time;
an unrelated future camera-key edit can invalidate more than its influence.
An inactive clip's transform edit does not affect a scene frame. The existing
graph hashes exact time and input pulls, so source-in/retime/alpha changes flow
through naturally. Legacy keys do not acquire the scene version.

Scratch is two Depth32, two R32Uint, one RGBA16F peel and two RGBA16F accumulation
textures: 40 bytes/output pixel, or 82,944,000 bytes at 1920×1080 per scene worker.
A returned working texture adds 8 B/pixel; shutter averaging reserves two further
working frames. Same-worker render-target reuse is ordered by the existing
command encoder/pool, so scratch need not multiply by shutter count. Uploads
retain the separate pool recording-epoch fence. CLI and MCP default job sizing
add these costs plus native source textures, summed conservatively across scenes.
This is not measured peak memory: source overscan, clip effects, graph caches,
small per-pass buffers and driver overhead still require runtime accounting and
OOM/backoff. Explicit `--jobs` remains the caller's choice.

All large scratch textures use the existing pooled allocation/error scope;
format usage/filterability, attachment counts/bytes and dimensions are checked
before allocation. Pipelines are worker-local and rebuilt for a changed device
identity. Work records into the existing encoder, with cancellation between
peels/samples; no private queue submit, GPU wait or readback path is added.

Authored regression targets cover rejected combinations/camera axes/budgets,
legacy and active/inactive keys, exact shutter/cut pulls, signed windows/PAR,
interior intersection/transparency/reorder/tie controls, partial near clipping,
per-sample occlusion, nested-source retiming and typed MCP edit/diff/undo/root
containment. These are test sources until actually compiled/executed. A SKIP is
not pixel acceptance. The original [Crossing Glass](../../examples/crossing-glass/README.md)
provides a moving before/candidate/negative-control package for later inspection.

Required next proof: focused actual-adapter tests, unchanged required repository
checks, pinned release CLI and stdio MCP routes, actual before/after native and
encoded frames with model inspection, determinism/cache evidence and independent
acceptance. Device-epoch/OOM recovery, antialiased edge quality and performance
need measured evidence; they are not earned by this source description. Lead
owns later integration/install. D2 lights/shadows, D3 aperture/focus and D4
mesh/material/environment remain separate required successors.
