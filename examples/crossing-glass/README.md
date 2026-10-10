# Crossing Glass: native planar depth source demonstration

An original six-second, 30 fps, 960×540 scene: translucent red and blue cards
cross over a checkerboard; the red card has a cutout window. A camera orbits,
then rolls, under a crisp 2D title. All geometry is native and synthetic. The
bundled Noto Sans test font retains its existing license. No client material,
pre-rendered depth, generated image, light or blur illusion is used.

Candidate adapter tests, before/after native and lossless renders, actual image
inspection and independent review were completed on 2026-10-10. See the
[executed evaluation](../../docs/evaluations/2026-10-10-agent-visual-workflows.md)
for exact source/binary hashes, sample scopes and the corrected lossy viewing
copy. Playback, listening and installed-route acceptance remain separate.

| File | Intended control |
|---|---|
| `legacy-before.json` | Existing painter sort cannot switch front surface across a card intersection. |
| `crossing-glass.json` | Opt-in per-pixel depth, fractional alpha, alpha window, orbit and roll. |
| `reordered.json` | Swaps red/blue authoring order. Physically separated interior pixels must stay unchanged; exact depth ties deliberately use the later ordinal. |
| `no-window.json` | Removes only the red subtractive mask; the hole must disappear and underlying visible material change. |
| `enable-depth.ops` | Typed reversible edits from the before project to the native scene, including its camera controls. No journal exists until these are applied through the normal tools. |

The before and candidate share camera positions and target throughout. The
before does not support roll: compare intersection behavior during f0–89, when
the candidate roll is zero. After f90, the candidate demonstrates the new roll
control as a separate capability, rather than a matched legacy-camera control.
All clips run from time 0 through time 6 (half-open), so the last frame is
f179 at 179/30. The title and subtitle are 2D overlays after the depth scene.

The exact interior alpha oracle is tested separately with controlled working
textures: red `(0.5,0,0,0.5)` and blue `(0,0,0.75,0.75)`. Red in front gives
`(0.5,0,0.375,0.875)`; blue in front gives `(0.125,0,0.75,0.875)`. This is linear
premultiplied over, with no background. The artistic demo has different colors,
a checkerboard and antialiased source shapes, so those constants are not its
encoded-display pixel predictions.

For reproduction, use normal CLI/MCP schema/params, typed
edit/plan/diff/undo, preview_frames and native FFV1 render. Retain the original
and edited timeline bytes, journal, binaries/revision, frame identities and
output hashes. The minimum image sequence is f0, f30, f60, dense f88–92,
f120, f150 and f179, with full-resolution intersection/window/title crops or
whole frames. Inspect actual returned images, then decode the encoded result at
the same frames and both endpoints. Record model observations and independent
review separately; image paths or successful JSON do not prove inspection.
Check cold/warm/forced caches and worker-count determinism on one backend.

Filtering currently uses bilinear source samples and single-sample raster edges;
inspect oblique edges separately from stable interior tests. No lights, shadows,
native aperture depth of field, mesh/PBR, one-node orientation camera or full
After Effects parity is claimed. See the [depth contract](../../docs/design/2026-10-09-native-depth-d1.md)
for supported combinations and the memory model.
