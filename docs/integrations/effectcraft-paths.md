# Native EffectCraft paths

Ferrocut uses the actual `effectcraft-path` Rust crate from EffectCraft commit
`6943872cf65b3da1275f1e0f808b60b2e51d84dc`, vendored at
`vendor/storytold/effectcraft/crates/path`. Its polygon/star and ordered path
operators run inside the existing native shape generator. The resulting project
keeps editable geometry, operator parameters, keyframes, and expressions.

`crates/ferrocut-engine/src/vector.rs` adapts EffectCraft's `BezPath`/`PathEl`
types to Ferrocut's existing tiny-skia geometric coverage. It calls upstream
`polystar` and `ops` functions directly. It retains Ferrocut's floating point
gradient evaluation, display Rec.709 color inputs, premultiplied linear ACEScg
output, and half-float frames. Existing rectangle, ellipse, custom path, fill,
stroke, dash, and gradient rendering takes its original path when `operators`
is omitted or empty.

## Editable example

```json
{
  "type": "shape",
  "shape": {
    "geometry": {
      "type": "star",
      "center": [640, 360],
      "points": 5,
      "inner_radius": 80,
      "outer_radius": 180,
      "rotation": {"expression": "time * 30"},
      "inner_roundness": 15,
      "outer_roundness": 30
    },
    "operators": [
      {"type": "round_corners", "radius": 8},
      {
        "type": "trim",
        "end": {"keyframes": [{"t": 0, "v": 0}, {"t": 2, "v": 100}]}
      },
      {"type": "wiggle", "size": 3, "detail": 4, "speed": 1, "seed": 72}
    ],
    "fill": null,
    "stroke": {
      "paint": {"type": "solid", "color": [1, "0.5", "0.1"]},
      "width": 6,
      "cap": "round",
      "join": "round"
    }
  }
}
```

Numeric fields accept the same `Animatable` constant, keyframe, or expression
syntax as other native generators. Time is **source time in seconds**, after
`source_in`, speed, and time remapping. Wiggle also receives that clock directly;
constant wiggle controls can therefore animate without any keyframes.

## Public Rust and serialized contract

`VectorSpec` adds `operators: Vec<VectorOperator>` with a default empty value
that is omitted during serialization. All geometry/operator structures reject
unknown fields. `VectorSpec::{all,all_mut,animatables,animatables_mut}` expose
identical numeric paths for immutable/mutable traversal; they include the
existing geometry and paint properties too.

| Geometry | Fields | Defaults and units |
| --- | --- | --- |
| `polygon` | `center`, `points`, `radius`, `rotation`, `roundness` | Center `[x,y]` and radius in pixels; rotation 0 degrees; roundness 0 percent |
| `star` | `center`, `points`, `inner_radius`, `outer_radius`, `rotation`, `inner_roundness`, `outer_roundness` | Radii in pixels; rotation and roundness default 0 |

Both use upstream `polystar`: rotation 0 places the first vertex above the center;
increasing angles rotate clockwise in output coordinates. Point counts are
rounded to the nearest integer at the sample time. Polygon/star point counts
accept 3–256, radii 0–1,000,000 pixels, rotation ±1,000,000 degrees, roundness
0–100 percent, and center coordinates ±1,000,000 pixels.

| Operator `type` | Numeric fields and defaults | Discrete fields and behavior |
| --- | --- | --- |
| `trim` | `start:0`, `end:100`, `offset:0` | `mode:"simultaneous"` trims each contour by its own arc length; `"individual"` trims contours as one ordered length. Percent endpoints; offset in degrees wraps around the path. Reversed endpoints are swapped. |
| `round_corners` | `radius` | Radius in pixels; upstream clamps corner cuts to the adjacent segment lengths. |
| `offset` | `amount`, `miter_limit:4`, `copies:1`, `copy_offset:0` | `join:"miter"` also accepts `"round"` and `"bevel"`. Positive amount grows a closed filled path; negative amount insets it. Open paths become outlines of width `2*abs(amount)`. Copy `i` offsets by `amount*(i+1+copy_offset)`. |
| `pucker_bloat` | `amount` | Percent, −100 to 100. Positive moves vertices toward their mean and handles outward; negative produces a spikier path. |
| `zigzag` | `size`, `ridges:1` | `smooth:false`; ridges are rounded, then each original segment gets `ridges+1` arc-length subdivisions, alternating normal displacements. |
| `twist` | `angle`, `center:[0,0]` | Angle in degrees; radial falloff and curve subdivision use upstream mathematics. |
| `wiggle` | `size`, `detail:10`, `speed:2`, `correlation:50`, `phase:0`, `seed:0` | `smooth:true`; deterministic noise at source time. Size in pixels; speed in periods/second, phase in degrees, correlation in percent. Rounded detail controls subdivisions per segment; seed is rounded. |
| `reverse` | None | Reverses each contour's path direction. |
| `merge` | None | `mode:"merge"` combines contours without boolean evaluation. Other modes are `"add"`, `"subtract"` (first operand minus later operands), `"intersect"`, and `"exclude"`. |

The public enums are `VectorOperator`, `VectorTrimMode`, and `VectorMergeMode`;
offset reuses `VectorJoin`. Numeric edit paths are relative to the shape, such
as `geometry.outer_radius`, `operators.0.radius`, `operators.1.end`, and
`operators.2.speed`. Through the timeline these become, for example,
`generator.shape.operators.1.end`. Vector components use `.x`/`.y`.
Discrete changes replace the corresponding operator or operator array.

Operators run in array order before fill and stroke. Each initial custom-path
contour is one operand in drawing order. A merge produces one compound operand
for subsequent operators. Boolean modes fill their operands using nonzero
winding; the shape's final `fill_rule` still controls rasterization. An empty
first subtraction operand and an empty intersection operand produce empty
output, preserving those operands when upstream's boolean helper filters empty
paths. This is the only empty-operand semantic repair in the adapter.

## Validation, cache, and cancellation

The adapter bounds work before calling upstream operations and checks every
returned path for finite coordinates and output size:

| Bound | Accepted range |
| --- | --- |
| Operator stack | At most 32 operators |
| Operator input/output | At most 32,768 path elements; coordinates within ±1,000,000 pixels |
| Boolean/offset input | At most 512 path elements and 64 operands |
| Trim | Start/end 0–100 percent; offset ±1,000,000 degrees |
| Round corners | Radius 0–10,000 pixels |
| Offset | Amount ±10,000 pixels; miter limit 1–100; copies 1–16; copy offset ±16 |
| Zigzag | Size ±10,000 pixels; ridges 0–128 |
| Twist | Angle ±36,000 degrees; center coordinates ±1,000,000 pixels |
| Wiggle | Size 0–10,000 pixels; detail 0–128; speed ±1,000; correlation 0–100; phase ±1,000,000; signed 32-bit seed |
| Wiggle noise clock | At most ±10¹² periods when size is nonzero |

Conservative growth estimates reject a stack before a subdivision operation
could exceed the element budget. Stored key values and evaluated samples are
both checked; easing or expression overshoot outside these bounds returns a
parameter-specific error. Existing legacy paths retain their original
100,000-command limit when no operators are used. Empty trim or fully inset
paths render transparent frames.

The structural and sampled content hashes include the ordered operator stack
and pinned implementation version. Sampled hashes include the source clock
when wiggle is active; a paused or zero-sized constant wiggle retains stable
keys. Numeric tracks hash their sampled values, so unchanged held samples can
reuse frames. Standard generator expression baking and source mapping apply.

`VectorNode` checks cancellation/deadlines before CPU work, between operators,
before paint stages, and before GPU upload. A single upstream operator call is
synchronous and cannot be interrupted internally; its inputs are bounded.

## Verification and scope

Run the focused native suite:

```sh
cargo test --offline -p ferrocut-engine --test vector --test vector_operators -- --test-threads=1 --nocapture
```

The new suite checks analytic polygon/star areas; legacy pixel preservation;
animated arc-length trim and offset wrapping; simultaneous/individual contour
trimming; rounded-corner and offset coverage; all five boolean modes; empty
operands; open-path offset and fractional alpha; pucker/bloat, zigzag, twist,
and operator ordering; seeded wiggle, phase, pause, and cache keys; strict input
and complexity rejection; mutable property traversal; timeline expressions,
edits, split equivalence, speed/remap/freeze, cache reuse; GPU alpha upload;
cancellation and deadlines. The existing vector suite also covers native
track mattes, gradients, strokes, and source-clock geometry.

On 2026-10-08 the focused run passed all 30 tests (15 existing vector tests and
15 operator tests), recorded in `/tmp/ferrocut-storytold-vector.log`. Both the
default adapter preference and explicit CPU preference resolved to llvmpipe
(LLVM 20.1.2), Vulkan, CPU for the upload checks. The timeline render checks
also used that software adapter; these results establish no distinct-hardware
comparison or finished-video creative review.

This boundary currently operates on one geometry's ordered contours. Nested
shape groups, transform repeaters, and the full Adobe shape-layer hierarchy
need separate project-model work. Mathematical behavior comes from the pinned
EffectCraft implementation; pixel matching to proprietary Adobe algorithms is
not claimed.
