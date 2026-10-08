# 3D camera move over generated layers

`timeline.json` (1280x534, 24 fps) has one *Tears of Steel* shot, clip `shot` on track `V1`, 0 s to 4 s. Turn it
into a small 2.5D scene with a camera move. All layers span 0 s to 4 s. Colors are `[r, g, b]` in 0..1.

1. **Background:** a new track `BG` *below* `V1` holding a clip `bg`: a **linear gradient** from `[640, 0]`
   (color `[0.05, 0.08, 0.2]`) to `[640, 534]` (black `[0, 0, 0]`). It stays a flat 2D layer.
2. **The shot:** make `shot` a **3D layer**, scaled to **60 %**, rotated **20°** around its Y axis.
3. **A card:** a new track `Card` *above* `V1` holding a clip `card`: a **solid** orange `[1, 0.55, 0.1]`
   layer, 3D, scaled to **15 %**, positioned at `[1000, 140]` with Z position **-300** (closer to the camera
   than the shot).
4. **Camera:** point of interest fixed at `[640, 267, 0]`; the camera position moves **linearly** from
   `[640, 267, -1778]` at 0 s to `[400, 200, -1100]` at 4 s. Leave the zoom at its default.

Render the result to `out.mkv`.
