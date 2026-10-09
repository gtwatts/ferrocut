# Native animated layer masks

Ferrocut now has CPU mask coverage for native rectangles, rounded rectangles,
ellipses, polygons, stars, and compound paths with line, quadratic and cubic
Bézier segments. Clip masks are applied before clip effects and the clip's
transform. The primitive requires no browser, GPU, media host, or external
process. The timeline adapter uploads its coverage for the existing GPU alpha
matte compositor.

The implementation calls the pinned EffectCraft path constructors and
`from_kurbo` converter, FilmCraft's `MaskPath::flatten`, and FilmCraft's
`MaskMode::start/combine`. Its signed-distance row kernel is adapted from
FilmCraft, with bounded horizontal edge spans, allocation checks and
cancellation checks. This is a real reuse of the native source algorithms;
neither application's old timeline, tick clock, UI nor git dependencies are
adopted.

Sources and licenses:

- [FilmCraft path model and Bézier flattening](https://github.com/storytold/filmcraft/blob/5231852443363f001c3f6b396dd9b1e6461ae2be/crates/project/src/mask.rs).
- [FilmCraft CPU signed-distance mask kernel](https://github.com/storytold/filmcraft/blob/5231852443363f001c3f6b396dd9b1e6461ae2be/crates/render/src/mask.rs).
- [EffectCraft path primitives and conversion](https://github.com/storytold/effectcraft/blob/6943872cf65b3da1275f1e0f808b60b2e51d84dc/crates/path/src/lib.rs).
- Both upstream projects are MIT OR Apache-2.0; the vendored license texts and
  pinned provenance remain in `vendor/storytold`.

The clip's `masks` property is an ordered array. For example:

```json
{
  "masks": [
    {
      "geometry": {
        "type": "ellipse",
        "center": [
          {"keyframes": [{"t": 0, "v": 280}, {"t": 2, "v": 360}]},
          180
        ],
        "radius": [180, 120]
      },
      "mode": "add",
      "feather": 12,
      "expansion": 4
    },
    {
      "geometry": {
        "type": "rectangle",
        "x": 300,
        "y": 120,
        "width": 80,
        "height": 120,
        "radius": 16
      },
      "mode": "subtract",
      "opacity": "1/2",
      "feather": 4
    }
  ]
}
```

Geometry uses native source pixels for media/comp clips and output pixels for
generators, before fit and the layer transform, with +x
right and +y down. It moves with the layer transform. All numeric leaves use
exact rational **source-time** keyframes, including geometry controls, opacity,
feather and expansion. Source in-points, speed, reverse playback and remaps
therefore select the mask's animation time. Splits and in-trims retain the same
mask keys and advance the clip's source mapping.

Numbers are integers or exact rational/decimal strings (`"1/2"`, `"0.25"`).
Expressions use the existing expression object and must be baked by Ferrocut
before the standalone coverage primitive can sample them. The engine's indexed
parameter paths are `masks.<index>.geometry.<property>`,
`masks.<index>.opacity`, `.feather`, `.expansion`, `.mode`, `.inverted`,
`.enabled` and `.fill_rule`. Existing geometry fields can be edited individually;
replace `masks.<index>.geometry` or the `masks` array to change topology. Sparse
indices are rejected.

Each mask supports `none`, `add`, `subtract`, `intersect`, `lighten`, `darken`
and `difference`. With accumulated coverage `a` and this mask's coverage `m`:

| Mode | Result |
| --- | --- |
| Add | `a + m - a*m` |
| Subtract | `a*(1-m)` |
| Intersect | `a*m` |
| Lighten | `max(a,m)` |
| Darken | `min(a,m)` |
| Difference | `a + m - 2*a*m` |

The first subtract/intersect/darken mask starts from full coverage; additive
modes start from zero. `none` and disabled masks are ignored. Inversion applies
before opacity. Opacity is a fraction in `[0,1]`. An empty or entirely inactive
stack returns `active=false` and all-one coverage; CPU application returns the
original frame and preserves its metadata.

The coverage model uses signed distance to the flattened contour. Expansion
adds a signed pixel distance. Feather is one isotropic smooth falloff centred
on that expanded edge; it is not a Gaussian blur. Zero feather retains a
one-pixel linear antialiasing ramp. Compound contours use `nonzero` by default,
or `even_odd`; open contours fill as if closed. Flattening requests 0.05-pixel
geometric tolerance and rejects curves that would exceed FilmCraft's
256-chord-per-segment accuracy bound. Coverage storage and distance evaluation
are float32; the timeline's alpha-matte upload uses half floats. There is no
8-bit mask bridge.

The standalone `MaskStack::coverage` and `coverage_checked` APIs return a
`MaskCoverage` plane for a `PixelRect`, including negative overscan origins.
`coverage_checked` preserves cancellation error kinds. `MaskStack::apply_cpu`
and `MaskCoverage::apply_cpu` multiply every premultiplied RGBA channel without
clipping negative or HDR RGB, and retain display/data windows, color space,
alpha mode and pixel aspect. The render adapter uses this same coverage.

Resource bounds are enforced before image allocation or raster work:

- 64 masks, 256 commands per path, 16,384 numeric properties, and 65,536 total
  keys per stack.
- 32,768 flattened edges, with a 256-chord accuracy limit per curved segment.
- Canvas and region axes at most 8192; each is at most 8,388,608 pixels, which
  includes UHD 3840×2160. Empty regions are supported. Region coordinates and
  endpoints must be within ±65,536.
- Coordinates/dimensions within ±1,000,000 pixels (dimensions nonnegative),
  feather `[0,512]`, expansion `[-512,512]`, opacity `[0,1]`, roundness `[0,100]`,
  rotation ±36,000 degrees, polygon points `[3,256]`, star points `[3,128]`.
- At most 256,000,000 estimated row/edge operations. The preflight counts
  horizontal edge spans, row scans and sorting bounds. Complex stacks or wide
  feathers may require a smaller region or simpler geometry. Coverage occupies
  at most 32 MiB; CPU RGBA application adds at most 64 MiB, excluding the input.

Structural validation accepts expression placeholders; sampled values are
checked after baking. Invalid easing overshoot, unrepresentable exact rational
interval arithmetic, malformed geometry, excessive flattening or excessive
work return errors. Cache bytes include the renderer/upstream version and
evaluated float32 geometry/style. Static masks reuse the same evaluated bytes
at different source times. The caller includes dimensions, region and its input
frame key. The timeline adapter gives invalid samples distinct error keys so a
warm valid frame cannot hide an invalid mask.

Validation runs with:

```sh
cargo test -p ferrocut-engine --offline --test masks -- --nocapture
```

The tests compare pixels against independent analytic rectangle/circle/rounded
rectangle distances, a dense cubic Bézier reference, and explicit fractional
combine math. They cover negative overscan, compound holes, wide feather and
expansion, UHD feathering, exact source clocks, cancellation, malformed inputs,
bounded work, cache identities, CPU HDR/premultiplication and metadata. Timeline
tests verify indexed edits/expression baking, reverse/split/trim keys and GPU
RGBA, mask-before-blur ordering, transformed masks, and a warm-cache error case.
GPU test output records the available adapters and any skips.

This milestone supplies layer masks. Adjustment clips reject masks. Per-effect
mask selection, anisotropic or variable feather points, tracked paths,
rotoscoping, mask-specific motion blur, topology keyframes and host mask-path
services for the previously unsupported EffectCraft effects remain separate
work. Existing vector paths and layer transforms remain available alongside
these masks.
