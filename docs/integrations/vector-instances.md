# Native vector groups and repeaters

Ferrocut's `vector_group` generator keeps a tree of editable native shapes,
group transforms, and repeat controls. Agents use familiar anchor, position,
scale, rotation, skew, opacity, copies, and offset properties. Numeric controls
are ordinary source-time `Animatable` values, including exact rational
keyframes and the engine's sandboxed expressions.

## Reuse boundary

The retained EffectCraft revision is
`6943872cf65b3da1275f1e0f808b60b2e51d84dc`.

- [`effectcraft-geom::Mat3`](../../vendor/storytold/effectcraft/crates/geom/src/lib.rs)
  executes native affine composition, rotations, skew, inversion, and point
  mapping. Ferrocut calls this actual vendored implementation.
- [`effectcraft-path::transform`](../../vendor/storytold/effectcraft/crates/path/src/lib.rs)
  transforms actual Bezier control points before coverage. Existing shape
  operators continue to call the retained EffectCraft path library.
- The upstream group/repeater renderer is private code in
  [`render/src/shapes.rs`](../../vendor/storytold/effectcraft/crates/render/src/shapes.rs),
  coupled to `EvalCtx`, layer property groups, its raster buffers, and its paint
  stack. Ferrocut does **not** claim that renderer is connected. Copy enumeration,
  partial-copy opacity, hierarchy traversal, limits, and cache integration are
  Ferrocut adapter code informed by the retained public property model and
  upstream transform formula.

This is an editable native generator, with no browser or external prerender.
It does not provide the upstream interleaved path/paint/operator slot stack,
group-wide merge/trim, isolated group blending, per-copy time offsets, or exact
proprietary After Effects behavior.

## Document and parameter contract

```json
{
  "type": "vector_group",
  "group": {
    "transform": {"position": [120, 80], "opacity": 100},
    "repeat": {
      "copies": {"keyframes": [{"t": 0, "v": 1}, {"t": 2, "v": "5/2"}]},
      "position": [80, 0],
      "scale": [95, 95],
      "rotation": 5,
      "start_opacity": 100,
      "end_opacity": 60,
      "composite": "above"
    },
    "items": [{
      "type": "shape",
      "shape": {
        "geometry": {"type": "star", "center": [0, 0], "points": 5,
          "inner_radius": 16, "outer_radius": 36},
        "operators": [{"type": "round_corners", "radius": 3}]
      }
    }]
  }
}
```

`VectorItem` is tagged `shape {shape: VectorSpec}` or
`group {group: VectorGroup}`. Rust boxes the payloads; JSON contains ordinary
objects. `items` is required and may be empty. `transform` defaults to identity,
and omitted `repeat` makes one unmodified copy. All objects reject unknown
fields. Existing `shape` generators and omitted operator stacks retain their
legacy rendering behavior.

The group visitor exposes every nested numeric leaf with stable paths, such as:

```text
generator.group.transform.position.x
generator.group.repeat.copies
generator.group.items.0.group.repeat.rotation
generator.group.items.0.group.items.1.shape.geometry.outer_radius
generator.group.items.0.shape.operators.0.end
```

`all`/`all_mut` and their `animatables` aliases return matching paths. Existing
array indices are required; editing does not create sparse hierarchy nodes.
The root editor/schema integration supplies timeline `set_param`,
`set_keyframes`, expressions, and complete subtree replacement.

## Geometry and compositing

Coordinates are output pixels, with +x right, +y down and positive clockwise
rotation. Scale and opacity are percentages. The group transform is:

```text
T(position) · R(rotation) · Skew(-skew, skew_axis)
  · S(scale/100) · T(-anchor)
```

Repeat copy index `i` uses exponent `k = i + offset`:

```text
T(position · k) · T(anchor) · R(rotation · k)
  · S((scale/100)^k) · T(-anchor)
```

The group's transform applies after its copy transform and its descendants'
transforms. The same exact source time samples all controls; copies do not
advance the animation clock. Offset changes the transform exponent, while the
opacity ramp still spans copy indices `0..ceil(copies)-1`. A fractional final
copy multiplies its ramp opacity by the fractional part. Zero copies produce
transparent output.

Items paint bottom to top. `composite: "below"` (default) keeps the original
above later copies; `"above"` paints later copies above the original. Group and
repeat opacity multiply each descendant paint. They do not first flatten an
isolated group: two overlapping opaque paints at group opacity 50% produce
75% overlap alpha. This rule is explicit and tested.

Each leaf's existing operators run before instance transforms. Fills transform
the complete curve. Strokes are dashed and outlined in local coordinates before
transforming, so anisotropic scaling stretches the stroke geometry, including
caps and joins. Gradients inverse-map the output pixel center into local paint
space; a nonuniformly scaled radial gradient becomes elliptical. Color remains
floating-point premultiplied linear ACEScg, with antialiased 8-bit geometric
coverage and half-float output. Compositing uses source-over in that working
space. This adds no RGBA8 color quantization.

## Bounds, errors, and cancellation

The adapter enforces these independent limits:

| Resource/control | Limit |
| --- | --- |
| Hierarchy depth, counting the root group | 8 groups |
| Total groups and shape leaves | 128 nodes |
| Expanded leaf draws | 512 |
| Repeat copies / offset | 0..128 / -128..128 |
| Source geometry estimate across leaves and expanded draws | 262,144 elements |
| Actual transformed fill/stroke outline elements across a frame | 262,144 |
| Single transformed path | Existing 32,768-element operator limit |
| Full-frame pixel work, output pixels × expanded draws | 268,435,456 |
| Output dimensions | Nonzero, at most 67,108,864 pixels |
| Anchor/position coordinates | ±1,000,000 pixels |
| Group scale | -10,000..10,000 percent |
| Repeat scale | 0.01..1,000 percent |
| Rotation/skew axis | ±360,000 degrees |
| Group skew | ±85 degrees |
| Opacity controls | 0..100 percent |
| Composed matrix components / transformed coordinates | Finite, ±1,000,000 |

Positive repeat scale avoids undefined negative bases at fractional exponents.
Negative group scale mirrors geometry; zero group scale collapses it to
transparent output. Geometric growth can exceed the matrix bound even when
individual knobs are valid; that sample returns an error. Key values are
validated and evaluated samples are checked again, covering expression results
and easing overshoot. Existing operator-specific limits also apply.

Tree and expansion validation happen before output allocation. One shared float
buffer is allocated for the final frame, with a temporary coverage mask for
each paint. The pixel-work bound admits 60 simple copies at 1920×1080 and is
more restrictive at higher output sizes. Actual transformed-element budgeting
also covers path-operator and stroke expansion.

Render cancellation/deadlines are checked before traversal, between children,
copies and paints, during color compositing, and while packing the output.
Cancellation remains `ErrorKind::Cancelled` at the render-node boundary. A
single bounded tiny-skia coverage or outline call does not poll internally.

## Cache and acceptance evidence

`VectorGroupNode` structural hashes include the complete editable document and
version. Sample hashes include the sampled draw order, f64 matrix, opacity,
dimensions, and each unique leaf's existing `VectorNode::content_hash_at`.
Held values can reuse the same node hash as equivalent constants. Distant key
edits do not change an unaffected sample. Intrinsic Wiggle Paths time is kept
through the leaf hash. Source-time mapping remains the clip renderer's job.

The focused executable coverage is
[`tests/vector_instances.rs`](../../crates/ferrocut-engine/tests/vector_instances.rs):
legacy identity pixels, analytic areas and alpha ramps, nested matrix products,
anchor/negative offsets, copy/item order, linear premultiplied composition,
inverse radial-gradient samples, anisotropic stroke coverage, rational source
times, mutable visitor paths, held/intrinsic-animation hashes, bounded hostile
inputs/work, and native GPU upload/cancellation. The native timeline integration
test also checks every frame key across split/in-trim, nested parameter/keyframe
edits, expression defaults, direct-vs-compiled pixels, repeated-frame cache hits,
2x speed, time remapping, and freeze. Adapter names are recorded by
the tests; default and CPU preferences may select the same software adapter.
These are engineering acceptance checks, not a creative review or a claim of
complete After Effects parity.

Validated on 2026-10-08: 17 group/repeater tests passed, alongside all 15 existing
vector and 15 existing vector-operator tests. Default and CPU preferences both
selected llvmpipe (LLVM 20.1.2, Vulkan CPU) in this environment; this is not a
cross-hardware validation. Logs: `/tmp/ferrocut-vector-instances.log` and
`/tmp/ferrocut-vector-instances-controls.log`.
