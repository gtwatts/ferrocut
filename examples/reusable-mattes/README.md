# Reusable named matte demonstration

This original 640×240, 24 fps, six-second composition uses only native generators
and rational keyframes. The nonadjacent `Stencil` supplies both an orange rounded
panel and a blue ellipse. Their motion is independent. An opaque backing covers
the stencil's presentation beneath the recipients; the stencil is also visible
in the right third. There is no audio, external font, client media or download.

The stencil includes opaque black, graded alpha and authored nonzero RGB at zero
alpha. Alpha coverage and premultiplied luma therefore produce different cutouts.
The two recipient regions occupy x < 426; source visibility should change only
the uncovered stencil presentation in x >= 426.

`showcase.json` is a native nested composition with three continuous phases:

| Frames | Time | Project | Expected behavior |
| --- | --- | --- | --- |
| 0–47 | [0, 2) s | `reusable-mattes.json` | Alpha cuts both moving panels; stencil visible. |
| 48–95 | [2, 4) s | `luma.json` | Both panels use luma; opaque black now cuts them out. |
| 96–143 | [4, 6) s | `hidden.json` | Same luma cutouts; stencil presentation hidden. |

Each phase samples its six-second inner project at the matching source time.
It does not restart the animation at a cut.

The reversible phase scripts are JSON documents stored as `.ops` files because
every `.json` in `examples/` is validated as a Timeline:

- `alpha.ops`: set both recipients to alpha and show the source.
- `luma.ops`: set both recipients to luma and show the source.
- `hidden-source.ops`: hide `Stencil` (apply after `luma.ops`).
- `legacy-adjacent.ops`: invert the separate legacy control's alpha matte.

Use the normal CLI `edit` or MCP `edit_apply` with these operations on a working
copy. Exercise dry-run, committed edit, plan/diff and `undo`. `alpha.ops` and
`luma.ops` are complete phase settings; `hidden-source.ops` changes only visibility.
The authored snapshots permit same-time comparisons without mutating a source.

When a render slot is granted, compare frames 0, 36, 72, 108 and 143 across alpha
and luma, then luma and hidden. Require identical recipient-region pixels for
the visibility pair and a nonempty changed region on the right. Inspect the full
six-second result plus ordered boundary frames 45–50 and 93–98, first frame 0
and last frame 143. Record actual inspected images and timecoded findings; these
checkpoints are planned controls, not observations.

`legacy-control.json` is the independent 64×32, 24 fps, one-second adjacent
control retained from the installed baseline plan. Its mask is consumed above
the panel. Compare unchanged old/new chunk keys and native render pixels, then
apply `legacy-adjacent.ops` and undo to recover the original.

No render, playback, encoded output, image inspection, or perceptual acceptance
claim is made here. Native execution, before/after images, undo, and independent
review remain pending the lead's render slot.
