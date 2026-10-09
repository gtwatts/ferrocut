# Native vector source bounds and clip transforms

Status: proposed contract for independent review, 2026-10-09. Base source:
`3ad837219d07d482f077b98889e710d7176c6c28`. No execution evidence yet.

## Defect and intended behavior

`VectorSpec::rasterize_checked` and `VectorGroup::rasterize_checked` allocate a
display-sized CPU image. tiny-skia clips coverage to that image. The graph then
applies the clip transform, which cannot recover discarded geometry. A shape
entirely at negative x stays absent when its clip is translated into the frame.
An explicit anchor in that source region does not recover it either.

`examples/vector-source-bounds.json` is original synthetic material: a small
off-canvas L shape, first translated and then rotated around an explicit anchor.
Its expected transformed geometry lies entirely inside a 128 by 96 display.
The existing code predicts a missing L in both shots. This is a static prediction
until baseline execution is granted; no private production asset is in the fixture.

## Coordinate and allocation contract

- A `shape` or `vector_group` generator's **display** remains its owning timeline's
  width and height. Source coordinates use that unchanged origin, +x right and
  +y down. An off-canvas shape does not resize the logical canvas or recenter itself.
- Clip anchor remains in source/display coordinates; the default is the declared
  display center, not the painted bounds center. Clip position remains in output
  coordinates. The existing affine is unchanged: position + A(source - anchor).
  Geometry and group controls use source time; clip transform uses clip-local time.
- The source's **data window** must retain the bounded painted geometry before
  the clip transform. It may extend beyond the display, including negative origins.
  Use the display union a conservatively rounded painted rectangle, so ordinary
  in-frame sources retain their current full-window storage and raster path.
- Calculate painted bounds after shape operators, group/repeater transforms and
  actual stroke outlining. A centerline or geometric path bound alone is not a
  stroke bound: caps, joins, miter limits and dashes matter. Include a one-pixel
  antialias guard outside nonintegral/edge coverage as needed; this is not an
  arbitrary blur/effect margin. Bound coordinates and allocation sizes before use.
- Raster storage uses an integer offset from source coordinates. Paint/gradient
  samples must map back to the same source/local coordinates. Offset buffers do
  not change fill winding, stroke construction, compositing order or premultiplied
  linear ACEScg/half-float semantics.
- Retain existing pixel, path/instance and per-frame work budgets. Reject
  unrepresentable or excessive windows with a clear error rather than silently
  clipping the geometry or attempting an unbounded allocation. Check the selected
  GPU texture dimension limit before uploading an extended source image.
- Existing full-window CPU `rasterize` helpers keep their viewport semantics for
  callers. Add an explicit source-bounds path for graph nodes and focused tests;
  do not silently change the indexing contract of existing helper callers.

## Transform, effects and nesting boundary

Use the existing `Frame::data_window` path through clip opacity, source masks and
the 2D clip transform. No compositor or interpolation rewrite is intended. Final
output still crops to the output display. Existing transform nodes crop their
result to their owning output, so a later transform cannot recover geometry that
an earlier transform has already discarded; collapsed-transform behavior is not
introduced here.

A nested composition keeps its declared native size for fit and anchor. The
nested regression first moves the shape into the inner frame, then scales/places
that composition in a differently sized outer frame. It must not derive a new
fit or default pivot from the vector's expanded storage. This slice does not add
new overscan/collapse options or change composition boundary policy.

Stroke extent is covered by this repair. Blur, shadow, third-party effect fringe
and nodes that explicitly reframe to a display keep their current ROI/contracts;
this is not a guarantee that every effect preserves arbitrary overscan. Existing
mask coverage supports a negative data-window origin and needs a focused control.
Nonadjacent/reusable mattes, independent matte visibility, variable feather, 3D
or motion-blur redesign, and typography are out of scope.

## Cache and agent workflow contract

Version the affected native vector source rendering semantics so previously cached
clipped pictures cannot be reused. A one-time invalidation of affected native
vector keys is acceptable and must be documented; unrelated source keys/pixels
stay unchanged. Geometry/group/transform edits continue through existing typed
parameters, journal, plan/diff and undo. No schema or serialized transform change
is proposed. MCP root containment must remain unchanged.

## Planned evidence, all unexecuted

1. Source tests for negative/right/bottom bounds, default versus explicit anchor,
   stroke cap/join fringe, gradients with nonzero origins, group/repeater bounds,
   empty/transparent geometry, resource rejection and in-frame pixel equality.
2. Full graph tests for the translated and rotated synthetic L, including
   premultiplied half-alpha and an adjacent matte; a differently sized nested comp
   verifies native size, fit and anchor behavior. Exact rational sample times and
   held-pose keys remain stable.
3. Existing CLI and MCP edit/plan/undo routes: retain initial bytes, change geometry
   and transform, prove affected keys change, undo to the original document/keys.
4. During an explicit lease, retain old-fail/new-pass native masters and inspected
   frames from the same public fixture, plus binary/source/output hashes and
   different-job determinism. Do not substitute test source for actual renders.
5. Required CONTRIBUTING checks and unchanged `ci/render-check.sh`/references before
   verified push and feature PR. Independent code/output review precedes integration.

Likely owned files: `src/vector.rs`, `src/vector_instances.rs`, their focused tests,
this contract and the public fixture. Any need to change compilation or node wiring
will be explained before extending this scope. No main/lib/index or MCP schema edit
is currently needed; workflow owns its disjoint index recovery slice.
