# Design: non-distorting media placement (`fit`)

Status: accepted 2026-10-08, implementation in progress. Chosen by a design panel (three independent designs: correctness-first, compatibility-first, ergonomics-first; scored and merged by a judge) after four of five producer agents in the 2026-10-08 video test hit the defect (`out/claude-videos-20261008/*/FRICTION.md`, see `docs/evaluations/`).

Decisions on the open questions: source pixel aspect stays out of scope (sources are square today; the formulas take `sw*par_src` when it lands); clip effects work in source pixels (After Effects layer rule), track and adjustment effects in output pixels; tracking through an animated or rotated source transform is evaluated in f64 and flagged `exact: false`; very large sources decode at native size (a decode-time box pre-reduction is a later optimization that must stay bit-identical); the FCP7 `scaleToFrameSize` mapping is to be checked against a real Premiere export before its loss note is removed.


## 0. Verified defect (read-only check of the tree at 5de8434)

- `compile.rs:77` takes `(w, h)` from `tl.output`; `:138` calls `source_factory(&c.source, w, h)`; `nodes.rs:131-132` opens `Decoder::open(&path, w, h)`; `decode.rs:186-199` swscales every frame to `out_w x out_h` (BILINEAR|BITEXACT|ACCURATE_RND). A 1280x534 shot in 1920x1080 is stretched 1.5 x 2.0225; every 16:9 clip in 9:16 is stretched 0.84375 x 3.529.
- `compile.rs:181-193` builds `TransformNode { width: w, height: h }`; `transform.rs:295-310` defaults anchor and position to that output center, so `transform.anchor` is in stretched-layer pixels although `params.rs:80`, `schema.rs:931` and `transform.rs:54` say source pixels; `scale` 1 means "fitted (stretched) to the frame".
- Masks rasterize on that layer (`mask_node.rs:71-77`; `masks.rs:1-3` says canvas resolution); clip effects take their canvas from the input (`fx/mod.rs:656-662`); tracking bakes per-axis output/analysis ratios into keys (`tracking_io.rs:214-223`); interchange compensates the stretch (`interchange.rs:2157-2162`, `1832`, `1851/1862`); nested comps must match the frame (`compile.rs:120-127`); proxies are swapped by path/hash only (`compile.rs:308-320`) and upscaled by swscale to the output, so their size never changed the GPU work.
- Frame keys: `content_hash_at` defaults to `content_hash` (`ferrocut-core/src/node.rs:187-189`), so the source's decoded size (`nodes.rs:102-111`) is in every frame key today; the per-frame `transform.at` key (`nodes.rs:426`) has no output size; `nodes.rs:447` plans with the input frame's size; `compositor.rs:717-724`, `782-789`, `827-835` allocate outputs with the input's display size.
- Nothing committed depends on the stretch: `media/*.mov` are 1920x1080 (ffprobe) in 1920x1080 examples (`ci/render-check.sh` renders demo and demo-av; `expected.json` compares SSIM, exact hashes are informational, and its `made_with.engine` still says `render.v2`); all nine eval `start.json` outputs equal the clip sizes (1280x544 Sintel, 1280x534 ToS, cut without scaling by `eval/fetch-media.sh`; `tos-3d-camera/expected.json` is 1280x534 with a same-size 3D clip; `tos-nest-blend`'s comp is created by `nest` at the parent size); every integration test synthesizes media at its timeline size. The one in-repo mismatch is `examples/motion-showcase.json` (640x360 over `motion-source.mkv`, rendered from `motion-source.json` at 320x180 by `scripts/motion-showcase.py:56-61`), whose masks are written in canvas pixels. The producers' `out/claude-videos-20261008/*/project.json` double-compensate with per-axis scales; they are throwaway outputs, not references.

## 1. Surface

### 1.1 Timeline JSON

**`fit`** on a video clip whose `source` is media or a nested comp (`timeline.rs` `Clip`, after `transform`; `Option<Fit>`, `skip_serializing_if = Option::is_none`):

| value | meaning | fit factor F = (fx, fy), exact rationals from native (sw, sh) and output (W, H) |
|---|---|---|
| `contain` | uniform scale so the whole picture fits inside the frame, centered; letterbox/pillarbox bars are transparent in the comp (black in the master unless a track below shows through) | fx = fy = min(W/sw, H/sh) |
| `cover` | uniform scale so the picture fills the frame, centered; the overflow is cropped by the frame edges | fx = fy = max(W/sw, H/sh) |
| `none` | native pixels 1:1, centered (After Effects placement) | fx = fy = 1 |
| `stretch` | today's framing: fills the frame, non-uniform when aspects differ; emits a pixel-aspect warning | fx = W/sw, fy = H/sh |

Rust: `#[serde(rename_all = "snake_case")] pub enum Fit { Contain, Cover, #[serde(rename = "none")] Native, Stretch }` in a new `crates/ferrocut-engine/src/placement.rs`.

**`output.fit`** (`OutputSpec`, `Option<Fit>`, serialized only when set): the default for clips without their own `fit`. Effective fit = `clip.fit ?? output.fit ?? contain`. One `set_param output.fit cover` reframes a whole 9:16 cut-down (Premiere's sequence-level Default Media Scaling). Each nested timeline resolves its own default for its own clips.

Validation (`validate_baked`, next to the adjustment checks at `timeline.rs:628-646` and the generator arm at `647-655`): `fit` on a generator or adjustment clip → `clip {id}: fit applies to media and nested-composition clips (generators and adjustment layers render at the frame size)`. `fit` is not animatable.

**Transform fields** keep their names; their meaning is now exact for every source size:
- `transform.position` `[x, y]`: output pixels where the anchor lands. Default `[W/2, H/2]`.
- `transform.anchor` `[x, y]`: native source pixels of the pivot (a comp: pixels of its own output). Default `[sw/2, sh/2]`.
- `transform.scale`: factor relative to the fitted picture (1 = exactly the fit; with `none`, 1 = native pixels). Uniform or `[x, y]`.
- `rotation` and the 3D fields: unchanged; rotation turns the fitted picture about the anchor (under `stretch` the stretched picture rotates rigidly).

Examples (1920x1080 timeline, 1280x534 source):
```json
{ "id": "shot", "source": "media/t1.mkv", "start": "0", "duration": "4" }
  → contain: 1920x801 picture at (0, 139.5)
{ "id": "shot", "source": "media/t1.mkv", "start": "0", "duration": "4", "fit": "cover",
  "transform": { "anchor": ["640", "267"], "scale": "11/10" } }
  → cover 180/89, then 1.1x punch-in about the source center
{ "id": "overlay", "source": "comps/overlay.json", "start": "2", "duration": "3", "fit": "none" }
  → a 960x540 comp placed 1:1, centered
{ "id": "legacy", "source": "../media/old.mov", "start": "0", "duration": "6", "fit": "stretch" }
  → today's framing (warning: pixel aspect changed by 1.3483)
```

### 1.2 Parameter registry (`params.rs`)

- VIDEO_CLIP: `ParamSpec::fixed("fit", Choice, "", "\"contain\"", "how the source picture is placed in the frame before the transform: contain (uniform, whole picture, letterbox/pillarbox) | cover (uniform, fills, crops) | none (native pixels, centered) | stretch (per-axis to the frame; changes the pixel aspect). Media and comp clips only; absent = output.fit (default contain)")` with choices.
- `transform.anchor`: default `"[sw/2, sh/2]"`, doc "pivot in native source pixels [x, y] (media_probe width/height; a comp: its own output); default: source center".
- `transform.position`: doc "where the anchor lands, output pixels [x, y] (default: frame center)".
- `transform.scale`: doc "scale relative to the fitted picture (1 = as placed by fit; with fit none, 1 = native pixels); uniform or [x, y]".
- TIMELINE: `ParamSpec::fixed("output.fit", Choice, "", "\"contain\"", "default fit for clips that don't set one")`.
- `vec_default(spec, frame, native: Option<(u32, u32)>)`: `transform.anchor` defaults to the native center; a component edit (`transform.anchor.x`) of a missing anchor with no native size errors: `transform.anchor.x: the source size is needed to fill .y; set both components, or enable probing / relink the media`.
- `null` restores the default for every parameter kind (`check_value_shape`, `params.rs:950-1000`: accept null everywhere; `set_path` already removes the key at `1084-1086`). Doc sentence "null restores the default" in the module doc and the `set_param` schema text.

### 1.3 Edit ops (`edit.rs`)

- `add_clip` gains optional `fit` (same enum); `ripple_insert`'s clip object accepts `fit` through serde. When the probed source size differs from the output, the change summary states the placement, e.g. `add clip s1 (media/s1.mkv 1280x544 -> contain 27/32: 1080x459 at (0, 730.5))`; nothing is stamped into the file (the effective fit is reported, `output.fit` keeps working).
- `set_param fit` / `set_param output.fit` through the registry; `null` restores.
- New op **`punch_in`** (sugar: writes ordinary transform values, journaled as itself, keeps the clip's fit):
```json
{ "op": "punch_in", "clip": "shot", "rect": ["320", "100", "640", "360"] }
{ "op": "punch_in", "clip": "shot", "pivot": ["640", "267"], "magnification": "3/2" }
{ "op": "punch_in", "clip": "shot",
  "keyframes": [ { "t": "0", "rect": ["0", "0", "1280", "534"] },
                 { "t": "4", "rect": ["320", "100", "640", "360"], "interp": "ease_in_out" } ] }
```
  Fields: `rect` `[x, y, rw, rh]` in source pixels, or `pivot` `[x, y]` (source pixels) + `magnification` (factor relative to the fitted picture, default 1); `mode`: `cover` (default: the rect fills the frame, its own overflow cropped) or `contain` (the whole rect visible); `position` (output pixels, default frame center); `keyframes` (each `{t, rect | pivot+magnification, interp?}`, `t` clip-local, `timeline_time: true` accepted as in `set_keyframes`) produce keys on anchor/position/scale; `snap: true` nudges `position` so the layer's top-left edge lands on whole output pixels (scale never changes). Errors: generator/adjustment/3D clip; unknown native size (probing disabled or offline media: `punch_in: the native size of media/x.mkv is unknown; run media_probe or relink`); `rw <= 0 || rh <= 0`. Warning when the rect leaves the source (`rect extends 40 px past the right edge of 1280x534; that area is transparent`). Summary: `punch_in shot: rect (320,100,640,360) -> anchor [640, 280], position [960, 540], scale 2; layer rect 0,0 1920x1080`.
- `unnest` additionally refuses a comp clip with an explicit `fit` or whose comp size differs from the parent: `unnest would change the picture; set fit/size first`.
- `relink` to a file of another size re-evaluates placements and reports them.
- CLI: `ferrocut probe <file> --json` accepted as a no-op.

### 1.4 Reports and warnings

Per media/comp clip, a **placement** entry:
```json
{ "clip": "s01_t2", "fit": "contain", "explicit": false, "native": [1280, 534], "output": [1920, 1080],
  "fit_scale": ["3/2", "3/2"], "scale_to_canvas": ["3/2", "3/2"],
  "layer_rect": [0, "279/2", 1920, 801], "bars": { "top": "279/2", "bottom": "279/2", "left": 0, "right": 0 },
  "aspect_change": "1", "animated": false }
```
`scale_to_canvas`/`layer_rect` are evaluated at the clip start when the transform is animated (`animated: true`). Where it appears: `plan` (MCP and CLI), `RenderReport.placements` + `warnings`, `timeline_get` with `placements: true` (probes), `media_status` (+ `width`/`height` per video source), `EditOutcome.placements` for clips an op touched (source, fit, transform, relink; also in dry runs) and `EditOutcome.warnings`, `preview_frames`/`stills` (`StillEntry.clips: [{id, fit, scale_to_canvas, aspect_change}]` and a red 2 px border plus `!1.35` label on cells where a visible clip has `aspect_change > 1.005`; add `!` and `x` to the 3x5 glyphs at `preview.rs:289-309`).

Warnings (never errors), produced by compile as `Compiled.notes: Vec<Note { level, clip, message }>` so CLI render/plan, MCP render/plan/preview_frames and edit_apply all carry them:
- `fit: stretch` with |fx/fy − 1| > 0.005: `clip legacy: fit stretch changes the pixel aspect by 1.3483 (1280x534 into 1920x1080: x1.50, y2.02); use contain, cover or none, or set transform.scale explicitly`.
- Tracking planner on a stretched source clip: same text, level warning.
- Per-axis user scale is never flagged (deliberate); the placement's `aspect_change` carries the number.

### 1.5 MCP (`schema.rs`, `lib.rs`)

`fit` on both clip schemas (`schema.rs:175-212`, `1040-1078`) and `output.fit` (`1110-1121`); `transform()` text (`923-942`): "Defaults place the fitted picture centered: anchor at the source center, position at the frame center, scale 1, rotation 0"; `anchor` "native source pixels [x, y] of the pivot (media_probe width/height)", `scale` "relative to the fitted picture (1 = as placed by fit)"; `add_clip` (`621-627`) gains `fit`; `set_param` (`646`) lists `fit`, `output.fit`, "null restores the default"; new `punch_in` op schema with the examples above; `nest`/`unnest` texts (`508-514`); `media_probe` "use width/height for transform.anchor and punch_in"; `media_status` sizes; `timeline_get` `placements` arg; `preview_frames` result doc; `proxy_generate` note "framing identical to the final by construction". `compact.rs` is generic: no change.

## 2. Semantics

### 2.1 Spaces and the placement map

- Layer space = native source pixels (sw x sh from the probe, `probe.rs:139-140`; a comp: its `output`), origin top-left, y down, pixel i spans [i, i+1). Output space = frame pixels (W x H). Square pixels on both sides today (sources are uploaded with PAR 1, `compositor.rs:422`; `OutputSpec` has no PAR); the output-side `P = diag(pixel_aspect, 1)` of `transform.rs:3-5` is kept.
- F = diag(fx, fy) per the table, computed as exact `Rational`s; evaluation and hashing stay f64 as in `transform.rs:107-121`.
- Forward map (After Effects convention, `transform.rs:3-5` extended): a source pixel s lands at `d = position + P⁻¹·R(rotation)·S(scale)·P·F·(s − anchor)`. F and P are diagonal and commute, so this equals `P⁻¹·R·(S·F)·P` applied to (s − anchor): the implementation folds F into the evaluated scale (`scale_eff = [sx·fx, sy·fy]`) and the kernel path is unchanged.
- Defaults: anchor (sw/2, sh/2), position (W/2, H/2), scale 1, rotation 0 → the fitted picture centered. Layer rect without rotation: `x0 = px − sx·fx·ax`, `y0 = py − sy·fy·ay`, size `(sx·fx·sw, sy·fy·sh)`. Visible source rect (inverse): `s = anchor + (S·F)⁻¹·R⁻¹·(d − position)` for d in the frame. `aspect_change = (sy·fy)/(sx·fx)` (reported as the ratio ≥ 1).
- Worked examples (by hand from media_probe + clip fields): 1280x534 into 1920x1080: contain f = 3/2 → 1920x801 at (0, 139.5); cover f = 180/89 ≈ 2.02247 → 2588.76x1080 at (−334.38, 0), visible source x in [165.3, 1114.7]; none → 1280x534 at (320, 273); stretch (3/2, 180/89), aspect change 360/267 = 1.3483. 1920x1080 into 1080x1920: contain 9/16 → 1080x607.5 at (0, 656.25); cover 16/9 → 3413.33x1920 at (−1166.67, 0). 1280x544 into 1080x1920: contain 27/32 → 1080x459 at (0, 730.5); cover 60/17 → 4517.6x1920; stretch (27/32, 60/17), aspect change 4.18 (the vertical producer's distortion). Half-pixel edges (139.5, 656.25) are kept exact as Premiere/AE do; `punch_in … snap` or an explicit position moves them onto whole pixels. Integer placements are exact copies (the kernel is interpolating, `transform.rs:12-13`).
- punch_in math: rect (x, y, rw, rh): `g = max(W/rw, H/rh)` (cover) or `min(…)` (contain); `anchor = (x + rw/2, y + rh/2)`; `position` = given or (W/2, H/2); `scale = g/f` for uniform fits, `[g/fx, g/fy]` for stretch. pivot + magnification m: `anchor = pivot`, `scale = m`. Check: 1280x534 under contain (f = 3/2), rect (320, 100, 640, 360): g = 3 → scale 2, anchor (640, 280); on screen the rect's left edge lands at 960 − 2·(3/2)·320 = 0 and its width is 640·3 = 1920. Keyframed form: one key per channel per entry with the given interp; every value an exact rational.

### 2.2 Identity and node insertion

`TransformAt::is_identity()` keeps its meaning on the folded values. A TransformNode is inserted for a media/comp clip iff `native != (W, H)` or the clip has a transform, `three_d` or motion blur (so same-size clips without a transform keep today's graph and keys). The input frame is passed through (`nodes.rs:442`) only when the placed affine is the identity AND `native == (W, H)`; a `fit: none` clip whose explicit position equals its anchor still runs the kernel (exact copy at an integer offset) because the display size changes.

### 2.3 Data-window plumbing

1. `SourceNode` frame: display (sw, sh), full data window, PAR 1, ACEScg (`input_rec709` unchanged, called with native w/h).
2. `ClipNode` (opacity; frame-blend dissolve of two native frames of the same source), `MaskNode` (coverage over `layer.width/height/data_window`, `mask_node.rs:71-77`: native pixels, no code change), `EffectNode` (canvas = input display, `fx/mod.rs:656-664`; overscan bound from the native display, `fx/mod.rs:493-498`) keep display (sw, sh); data windows may shrink (crop) or grow (blur overscan).
3. `TransformNode`: input display (sw, sh) → output display (W, H). `plan(fwd, in.data_window, W, H)` (`transform.rs:201`) already clamps to the caller's size and the kernel reads the source through `src_origin` (`transform.wgsl:60-67`), so any native size with any data window works. Required changes: `nodes.rs:447/457` pass `self.output` instead of `f.width/f.height`; `KernelSetup` and `MultiSetup` gain `display: (u32, u32)` set by `plan`/`plan_multi`; `Compositor::transform_reduced`/`transform_multi` allocate `out` with `k.display` (keeping `a.pixel_aspect`); new `clear_display(ctx, w, h, window, par, cs)` replaces `clear_window(like = input)` on the singular/off-screen path (`nodes.rs:449/459`).
4. Output data window = `bbox(F·S·R·(data_window ± kernel margin)) ∩ [0,W]x[0,H]` (`transform.rs:241-255`): cover crops automatically; contain/none yield a sub-window whose outside is transparent; over/dissolve union windows (`compositor.rs:443/471`) and the output pass blacks out uncovered pixels (`tests/frames.rs:122-164`), so bars are transparent in the comp and black in the master. `check_compatible` (`compositor.rs:207-220`) holds because every clip chain ends at (W, H); compile adds a debug assertion that every SequenceNode input has the output display size. SequenceNode gaps, MatteNode, BlendNode, StackNode, AdjustNode and `empty_sequence` are unchanged.
5. Track effects and adjustment layers run after the fit on (W, H) frames: output pixels. Rule: clip effects = source pixels (before the fit), track/adjustment effects = output pixels.

### 2.4 Nested compositions

The inner graph's output frame has the inner display size; the outer ClipNode passes it through and the outer TransformNode fits it. `compile.rs:120-127`'s `ensure!` is removed; native = `inner.output` (no probe). `fit: none` places a comp 1:1. `nest` keeps creating comps at the parent size (`edit.rs:2765-2768`); `unnest` refuses fitted or mismatched comps. Interchange's own matching-canvas rules (`interchange.rs:1195-1198`, `1955-1957`) are relaxed (see 3).

### 2.5 Generators

Unchanged: `generator::node(spec, w, h)` renders at the frame size (`compile.rs:109-113`); text/vector coordinates stay output pixels; F = I by construction; no node unless the clip has a transform/3D/blur (as today).

### 2.6 Probe, decode size, proxies

- `SourceNode::new(path)` probes once (already done for fps, `nodes.rs:43-46`) and stores native width/height; no video stream or a zero size is a compile error naming the clip. `Decoder::open(path, out_w, out_h)` keeps its signature; the engine passes the native size, so swscale is a format conversion only; frames whose size changes mid-file are conformed to the header size (scaler keyed per input size, `decode.rs:186-199`; documented).
- Decode at native, always. Decoding at the fitted size would make pixels depend on whether a later user scale exists (swscale bilinear vs the GPU Catmull-Rom/box path), put the output size back into the source key, and break the source-pixel contract for masks and effects. The GPU kernel widens for minification and takes exact 2x box levels past 8x (`transform.rs:17-22`), so 4K→1080p is a 2x radius-4 resample. Cost: native-size uploads and textures; `vram::jobs_for` sizes jobs by `max(output pixels, largest native layer pixels)` (`Compiled.max_layer_pixels`).
- Proxies: `compile_proxies` builds the SourceNode from the original (native size), then swaps only path and `file_hash` (`compile.rs:308-320`); the decoder conforms the half-size proxy (`proxy_size`, `proxy.rs:92-95`) to (sw, sh) with the same swscale flags. Framing, masks, effects and keys are identical by construction between draft and final except pixel content and `proxied_hash` (`proxy.rs:84-89`); today's cost profile is preserved (proxies never reduced GPU work). Phase 2 (not in this change): carry the proxy at its own size with `L = diag(sw/pw, sh/ph)` folded into F and mask geometry scaled by L⁻¹; only worth it once GPU time dominates.

### 2.7 Masks

`MaskNode` already rasterizes on the layer frame; with native layers mask geometry is in source pixels and moves with the fit and transform (compile order unchanged: masks → effects → transform, `compile.rs:162-193`). Docs only (`masks.rs:1-3`, `native-masks.md:61-62`).

### 2.8 Tracking

Analysis unchanged (`tracking.rs:3`, `737-750`: original decoded pixels, resize refused). The planner (`tracking_io.rs:117-281`) drops the untransformed-source `ensure!` (`131-140`) and the per-axis `output/analysis` scale (`214-223`): it bakes the timeline (`expr::bake`), then for each sample at source time s with timeline time g and source-clip-local l computes `canvas(g) = placed_affine(source clip, l).apply(p(s))` where `placed_affine = position + P⁻¹·R·(S·F)·P·(· − anchor)` (F of the clip's effective fit, exact rationals). Attach: keys = `base + offset + (canvas(g) − canvas(g_seed))`; base stays the target's constant position or (W/2, H/2). Stabilize: keys = `base + linear_part(placed_affine)(correction offset)` (offsets are source pixels, `tracking.rs:827`). Still rejected: generator source, 3D, motion blur, source clip/track effects, non-constant speed, time remap. Exactness: exact rationals when the source transform is constant and unrotated; otherwise f64 evaluation rounded to 1/65536 px with `"exact": false` in the result. The result gains `placement` and the `semantics` string (`277`) becomes "source-pixel displacement mapped through the source clip's fit and transform". A stretched source with fx ≠ fy is allowed with a warning. For `examples/motion-showcase.json` (320x180 in 640x360) F = 2 equals today's ratio, so its keyframes are unchanged.

### 2.9 Effects

Clip stacks see canvas = native size, so pixel-unit parameters (blur sigma, drop-shadow distance, native transform/crop/letterbox; EffectCraft normalized `[0.5, 0.5]` = layer center) are in source pixels and are scaled on screen by the fit (AE's layer-space rule). Track and adjustment stacks are unchanged (output pixels). `fx/native.rs:2-3` wording "display pixels" → "layer pixels".

### 2.10 3D layers and camera

`layer_homography` (`layer3d.rs:371-423`) receives the placed TransformAt: `m = R·diag(sx·fx, sy·fy, 1)` and `m·anc` uses the native anchor, so `(s, 0) → position + O·Rx·Ry·Rz·S·F·((s, 0) − anchor)`; z unscaled (as today). Camera, default zoom (`layer3d.rs:91-93`), projection center and depth sort (`428-447`, now fed the placement) are in output pixels and unchanged; `plan_multi` (`491`) takes the output size for the window clamp; `MultiSetup` gains `display`. Motion-blur samples (`nodes.rs:306-331`) evaluate the placed TransformAt per sample; `multi_key` already hashes width/height (= output). A same-size untransformed 3D layer still equals the 2D layer (`tests/layer3d.rs:398`).

### 2.11 Cache keys (rule: a key changes iff pixels can change; no version bumps)

- Source frame key: `H(source: file_hash, native w, h, SOURCE_VERSION)` (`nodes.rs:102-111`, bytes unchanged; width/height now mean native). Same-size sources keep today's keys; mismatched sources get new keys automatically.
- Transform per-frame key (`nodes.rs:423-429`): `transform.identity` only under the pass-through rule of 2.2; otherwise `NodeHash("transform.at", [TRANSFORM_VERSION, placed TransformAt bytes, and only when native != output: b"display", W, H])` (fixes the missing output-size dependency: the same placed parameters into two output sizes must not share a key). Same-size transformed clips (demo-av `logo`, the 3D eval card) hash byte-identically to today. `multi_key` unchanged. Static `content_hash` appends native + fit only when non-trivial (same pattern as `nodes.rs:400-411`).
- Changing `fit`, `output.fit` or any transform value re-keys only the frames that show the affected clips; `SOURCE_VERSION`, `TRANSFORM_VERSION`, `LAYER3D_VERSION`, `PROXY_VERSION` and `ENGINE_VERSION` are not bumped (pixels of every unchanged-key frame are identical; mismatched sources re-key through the source hash). Document this in the `SOURCE_VERSION` comment.

### 2.12 Pixel aspect of sources

Out of scope (sources are uploaded square, probe exposes no SAR). Hook: F is computed on the display-corrected width `sw·par_src` vs `W·par_out`; every formula above stays valid.

## 3. Compatibility and migration

- **Default: `contain` for every timeline, old or new; no format-version field.** Why: the engine is 0.0.1 and the stretch is a defect reported by four of five producers (trailer, vertical, vox, explainer friction logs) and the earlier Codex run (`docs/evaluations/2026-10-08-agent-video-tests.md:66`); `CONTRACT.md:94` tells agents to bootstrap `project.json` by hand, so a version flag would keep distortion alive in every new project and every copied example; a flag that switches the meaning of `anchor`/masks per clip (stretch = canvas pixels) would leave two coordinate conventions in one file, which is the confusion that caused the defect; and nothing committed depends on the stretch (section 0). `none` as default (vertical producer) would crop 4K sources in 1080p timelines; `contain` is the whole-picture, non-distorting default (Premiere's Set/Scale to Frame Size).
- Reference renders, evals, tests: unchanged pixels and keys for same-size clips (no node added, same source bytes, same transform key bytes). `ci/render-check.sh` passes without `--update` (demo/demo-av hashes identical, the migration proof); `eval/run.sh all` references score 100 % untouched; `tos-3d-camera` (same-size 3D clip, F = I) unchanged; integration tests unchanged except those listed in 5.
- `examples/motion-showcase.json`: a uniform 2x, so contain keeps the framing and tracking scale; convert the masks to source pixels (ellipse center [320,180]→[160,90], radius [310,147]→[155,73.5], feather 12→6; subtract center [510,250]→[255,125], radius 38→19, feather 4→2, expansion keys 0→0 and 12→6) and `examples/motion-revise.ops` `masks.0.feather` 6→3; the reticle position [197,173] (generator, output pixels) and `motion-tracking.settings` (320x180) are unchanged; `scripts/motion-showcase.py` only asserts repeat/undo hash equality and source-pixel tracking positions, which hold. Pixels differ slightly (GPU Catmull-Rom magnification instead of swscale bilinear); its renders are evidence, not a gate.
- Mismatched sources in external timelines change from stretched to letterboxed. `fit: stretch` restores the framing (not bit-identical pixels) and explicit anchors/mask geometry authored in stretched-layer pixels convert by `x·sw/W`, `y·sh/H` (radii per axis; feather/expansion ≈ geometric mean). Documented in the guide and a CHANGELOG/README note; phase 2: `ferrocut timeline migrate --legacy-stretch` (stamp `fit: stretch`, convert coordinates, print the diff) for the producers' `out/` projects, which otherwise double-compensate.
- Nested comps of another size, previously a compile error, now compile with the fit; `tests/nest.rs:102-139` is rewritten (cycles still error).
- Interchange (`interchange.rs`): import `scale_to_frame` → `fit: contain` with scale = motion/100 (Info, no loss); other clips → `fit: none` with anchor/position/scale copied 1:1 (the stretch-compensating scale at `2157-2162` and the anchor factors at `2174-2182`/`2204-2219` go away); nested sequences of another canvas import with `fit: none` instead of being omitted (`1955-1957`). Export: anchor in source pixels unscaled, position unscaled, motion scale = `100·scale·F` per axis (`uniform_scale` iff sx = sy and fx = fy); `scale_to_frame` true when fit is contain and scale is the constant 1 (motion 100); cover/stretch bake F into scale (Info for cover, Loss for anisotropic stretch on FCP7, uniform only); the nested-canvas `ensure!` at `1195-1198` is replaced by baking the placement into the nested clip's motion. Round-trip invariant: same placed matrix. `filmcraft-interchange.md:23/25/59` updated.
- Tracking keyframes generated before the change for mismatched sources encoded output/source ratios; they are plain position keys and stay exact only under `fit: stretch`; the planner's new `placement` field makes the difference explicit.
- Proxies: existing `.ferrocut-proxies` files stay valid; draft keys still differ from final keys via `proxied_hash`. Caches: nothing invalidated globally.
- Old binaries reject `fit`/`output.fit` with a clear serde error (`deny_unknown_fields`, `timeline.rs:31/266/309`), as every new field has shipped.

## 4. Implementation plan (ordered; paths under crates/ferrocut-engine/src unless noted)

1. **placement.rs (new)**: `Fit`, `Placement { native, output, fit }`, `fit_scale() -> [Rational; 2]`, `is_trivial()` (native == output && F == I), `placed(&TransformSpec, t) -> TransformAt` (defaults anchor native/2, position output/2, scale × F), `layer_rect`, `visible_source_rect`, `aspect_change`, `hash_bytes()` (empty when trivial), `punch_in(rect | pivot+mag, mode, position, fit) -> (anchor, position, scale)` in exact rationals, `snap`, `PlacementReport` JSON, `warning(&self) -> Option<String>`. Unit tests (no GPU).
2. **timeline.rs**: `Clip.fit: Option<Fit>`, `OutputSpec.fit: Option<Fit>`, `Clip::effective_fit(&OutputSpec) -> Fit`; `validate_baked` rejects fit on generator/adjustment clips (`:628-655`); doc comment on `transform`.
3. **transform.rs**: `TransformSpec::placed(t, &Placement)`; keep `at(t, w, h)` as `placed` with a trivial placement so existing callers/tests pin today's numbers; `KernelSetup.display` set in `plan`; module header (`1-8`) and field docs (`51-59`).
4. **layer3d.rs**: `MultiSetup.display` set in `plan_multi`; `layer_depth` takes a `Placement`; header note that scale carries the fit.
5. **compositor.rs**: `transform_reduced` (`782-789`) and `transform_multi` (`717-724`) allocate with `k.display`; add `clear_display`; keep `check_compatible`.
6. **nodes.rs**: `SourceNode::new(path)` probes native width/height (error when absent/zero); `TransformNode` replaces `width/height` with `placement: Placement` (+ `output`); `at()/sample()` use `placed`; `content_hash` adds `placement.hash_bytes()`; `content_hash_at` per 2.11; `render` passes `self.placement.output` to `plan`/`plan_multi`, uses `clear_display`, pass-through per 2.2; `StackClip.depth` carries the placement; update the DEMO stub (`:879-892`, factory signature `FnMut(&PathBuf)`).
7. **compile.rs**: `source_factory: FnMut(&PathBuf) -> Result<SourceNode>` (update stubs in nodes.rs tests, tests/nest.rs:14, tests/blend.rs, tests/retime.rs); per clip native = `node.width/height` (media) or `inner.output` (comp); remove the `ensure!` (`120-127`); insert the TransformNode per 2.2 (`181`); `Compiled { notes, max_layer_pixels }`; debug assertion on sequence inputs' display size; `compile_proxies` keeps the probed native size and swaps path/hash only.
8. **media/decode.rs, media/proxy.rs**: doc comments (engine decodes at native; proxies conformed to it; framing identical by construction).
9. **vram.rs, render.rs, main.rs, ferrocut-mcp/src/lib.rs**: `jobs_for`/`default_jobs` take `max(output, max_layer_pixels)` (compile before sizing at main.rs:976 and lib.rs:1160); `RenderReport.placements` and `warnings` (from `Compiled.notes`); `plan` prints placements/warnings.
10. **params.rs**: `fit` and `output.fit` specs; anchor/position/scale docs and defaults; `vec_default(spec, frame, native)`; `null` accepted for every kind in `check_value_shape`; registry tests (`1212-1230`).
11. **expr.rs** (`:419`, `:1155`): a referenced but unset `transform.anchor` resolves its default from a native-size lookup supplied by compile (`bake_with(tl, sizes)`), else errors "set transform.anchor explicitly".
12. **edit.rs**: `MediaFacts { width, height }` filled by `comp::source_facts` (comp.rs:84-102; comps report their output); `AddClip.fit` + placement summary; `set_param` threads the clip's native size for anchor defaults (error without probe); `EditOp::PunchIn` per 1.3; `unnest` guard (`2860-2868`); `relink` placement reporting; warnings/placements collected after ops touching source/fit/transform.
13. **project.rs**: `EditOutcome { warnings: Vec<String>, placements: Vec<PlacementReport> }` (skip if empty; filled in dry runs too); `placements(tl, probe)` helper shared by MCP and CLI.
14. **diff.rs**: `("fit", "fit_changed")` in the tag list (`511-521`); `output.fit` in settings changes.
15. **tracking_io.rs / tracking.rs**: planner per 2.8; `semantics` string; `tracking.rs:827` comment.
16. **interchange.rs**: per 3; `interchange_io.rs` already probes sizes (`141-146`).
17. **preview.rs**: `StillEntry.clips`, contact-sheet badge, glyphs.
18. **ferrocut-mcp/src/schema.rs, lib.rs**: per 1.5; `media_status` width/height; `timeline_get` `placements`; warnings passthrough for render/plan/preview_frames/edit_apply.
19. **main.rs**: `probe --json` no-op; `media status`/`plan`/`render` print placements and warnings.
20. **Examples and CI**: convert `examples/motion-showcase.json` masks and `motion-revise.ops`; add `wide` (1280x534 `testsrc2`) to `scripts/gen-test-media.sh`; add `examples/reframe.json` (1920x1080: contain, cover, none, stretch and a keyframed punch_in over `wide.mov` and a 1920x1080 clip, plus a 960x540 nested comp with `fit: none`) and register it in `ci/render-check.sh`; `ci/render-check.sh --update` must only add the new entries (demo/demo-av hashes unchanged in the commit).
21. **Eval**: `eval/tasks/sintel-reframe-9x16` (1080x1920, `output.fit: cover` + a `punch_in` on s2, graded with `render.match` against an `expected.json` and `fields` on `fit`) and `eval/tasks/tos-letterbox-16x9` (1920x1080 contain: luma checks that the bars are black and the picture fills the width); `eval/README.md` rows. The nine existing tasks are untouched.
22. **Docs** (section 6), then run `cargo test -p ferrocut-engine -p ferrocut-mcp --offline`, `ci/render-check.sh` (no `--update` for demo/demo-av), `eval/run.sh all`, `python3 scripts/motion-showcase.py`.

Estimate: ~2,400 lines (engine ~900, MCP ~150, CLI ~50, tests ~800, docs/examples ~350, eval ~150); 35-40 hours for one implementer.

## 5. Test plan

- **placement.rs unit**: fit factors for all four fits on 1280x534→1920x1080 (3/2; 180/89; 1; (3/2, 180/89)), 1280x544→1080x1920 (27/32; 60/17; 1; (27/32, 60/17)), 1920x1080→1080x1920 (9/16; 16/9), 320x180→640x360 (contain = stretch = 2), same size (trivial); layer rects (0,139.5,1920,801), (0,656.25,1080,607.5), (0,730.5,1080,459); `aspect_change` 1 for uniform fits, 1.3483 for the stretched 1280x534 case; `is_trivial` false whenever sizes differ even with scale 1; punch_in math (rect (320,100,640,360) under contain → scale 2, anchor (640,280); pivot lands on position for every fit; rect corners land on the frame edges for cover and contain; stretch gives per-axis [g/fx, g/fy]); snap moves only position; hash_bytes empty iff trivial.
- **transform.rs unit**: `placed` defaults; `at(t, w, h) == placed(trivial)` for every existing spec (pins legacy numbers); `KernelSetup.display` recorded.
- **nodes.rs / compile key tests**: the DEMO stub timelines produce exactly the pre-change graph length and keys (commit the current key list as a golden fixture); a transformed same-size clip's `content_hash_at` is byte-identical (extend `tests/transform.rs:260-327`); a mismatched source gets a TransformNode and new keys; changing `fit` re-keys only frames showing that clip; changing `output.fit` re-keys only clips without their own fit; the same placed TransformAt into two output sizes yields different per-frame keys; proxies: draft keys differ from final, final keys ignore proxies (extend `tests/proxy.rs:105-216` with a 256x128 source in a 512x256 timeline).
- **compile tests**: a 128x32 comp in a 64x32 timeline compiles (rewrite `tests/nest.rs:102-139`: cycles still error; the size case asserts a TransformNode with fit scale 1/2 and keys that compose); generators get no fit node; `fit` on a generator/adjustment clip fails validation with the documented message; the sequence-input display assertion holds for dissolves between a fitted clip and a same-size clip, a fitted clip over a generator, and three different native sizes on one track.
- **GPU pixel checks** (skip without an adapter, `tests/transform.rs` style, also on lavapipe): synthetic 64x32 source in a 128x32 timeline: `none` and `contain` (integer offset) are exact copies at x offset 32 with transparent bars that read black in the master; `cover` is a 2x magnification compared against a CPU Catmull-Rom reference; `stretch` reproduces today's framing (bbox fills the frame, 1.5 x 2.02 for a 1280x534-like 64x27 case); a 9:16 timeline with a 16:9 source under cover crops symmetrically; output data windows equal the computed layer rect clamped to the frame; anchor [100, 50] moves the top-left to position − F·(100, 50); punch_in 2x on the pattern center keeps the center pixel fixed; a 3D contain layer with the default camera equals the 2D contain layer; motion-blur path outputs (W, H); determinism across two renders.
- **Masks and effects**: extend `tests/masks.rs:752` with a synthesized 16x8 media source in a 32x24 timeline: a source-pixel rectangle lands at the fitted output location (contain and cover follow the layer); a clip `gaussian_blur` sigma s on a 2x-fitted layer spreads 2s on screen while the same blur on the track spreads s; a clip `crop` of 10 px on a 2x-fitted source removes 20 output pixels.
- **Proxies**: draft vs final renders of a mismatched timeline have identical alpha-coverage bounding boxes and SSIM above a threshold; the proxy decode conforms 256x128→512x256 (`frame_at` length); an odd-sized source (641x361) keeps the same bbox under proxy; the pre-proxy cache is reused by the final render (existing assertion).
- **Tracking**: a stub analysis (320x180, two samples) planned into a 640x360 contain timeline scales displacement by 2 (unchanged keys), into a 1920x1080 `none` timeline by 1, under stretch per axis with the warning; a source clip with constant scale 2 and position offset maps keys through the placed affine exactly; a keyframed source scale produces keys equal to the hand-mapped affine within 1/65536 px with `exact: false`; 3D/motion blur/effects still rejected with the existing messages; stabilization offsets follow the linear part; `scripts/motion-showcase.py` ops byte-compare against the current `tracking-keyframes.log`.
- **Edit ops**: `set_param fit` round-trips and rejects bad values and generators; `set_param output.fit`; `add_clip` with `fit` and the placement summary for a mismatched source; `punch_in` constant and keyframed forms write exact rationals (snapshot JSON) and report layer rects; errors for generators/unknown size/empty rect; overflow warning; `snap`; `null` resets `transform.anchor`, `transform.scale`, `opacity`; `transform.anchor.x` alone fills `.y` from the native center and errors without a probe; `unnest` refuses fitted/mismatched comps; `relink` to another size reports a placement change; `EditOutcome.warnings` carries the stretch warning; diff tags `fit_changed`; journal/undo round-trips `punch_in`.
- **Interchange**: rewrite `tests/interchange.rs:130-189` to compare placed matrices; round trips for fit none and contain with a 1280x720 source in a 640x360 timeline are exact; FCP7 `scaleToFrameSize` imports as contain and exports back as `scale_to_frame`; per-axis scale still reports the FCP7 uniform loss; a nested sequence of another canvas imports with `fit: none` and exports without the ensure; a golden export of a same-size fixture is unchanged.
- **Schema/MCP**: `timeline_schema` lists `fit`, `output.fit` and `punch_in` (`crates/ferrocut-mcp/tests/timeline_schema.rs` builds timelines through every op); the clip schema rejects `fit: "fill"` with the enum; `timeline_get placements`; `media_status` sizes; render/plan/edit_apply/preview_frames results carry `warnings` for a stretched mismatched timeline and none for matching sizes; stills badge pixel assertion (red border present only for the stretched cell).
- **References**: `ci/render-check.sh` passes with the demo/demo-av entries untouched and the new `reframe` entry added; `eval/run.sh all` scores 100 % on the nine references with no task file changed, and the two new tasks score 100 % with their references and well below 100 % for a no-op; `scripts/motion-showcase.py` completes with its assertions.

## 6. Docs to change

- `transform.rs:1-8` and `51-59`; the new `placement.rs` module doc (spaces, F table, formulas, worked examples).
- `params.rs:67-90` (position/anchor/scale), new `fit`/`output.fit` entries, module doc (`13-17`: "null restores the default"), `masks` doc (`263`: source pixels).
- `nodes.rs:22-35` (SOURCE_VERSION comment: width/height = native; why no bump), `compile.rs:44-46`, `comp.rs:1-13` (comps of any size are fitted), `masks.rs:1-3`, `mask_node.rs:1`, `media/decode.rs:1/153-155`, `media/proxy.rs:1-5`, `fx/mod.rs:10-16`, `fx/native.rs:2-3`, `layer3d.rs:6-16`, `tracking_io.rs:114-116` + `277`, `tracking.rs:827`.
- `crates/ferrocut-mcp/src/schema.rs` texts (`175-212`, `430-436`, `508-514`, `616-631`, `644-655`, `819`, `923-942`, `1040-1078`, `1110-1121`, `1231-1240`, `media_probe`) and `lib.rs` tool descriptions (`272-278`, `timeline_get`, `plan`, `preview_frames`, `media_status`) and the server instructions string (`1577+`: "media is placed with fit, default contain; use punch_in").
- `crates/ferrocut-mcp/docs/timeline-guide.md`: Structure (`24-37`: `fit`, `output.fit`, two sentences on the placement model), Nested compositions (`68-69`: drop "inner frame size must match"), Video effects (`164-181`: clip effects in source pixels, track/adjustment in output pixels; letterbox at output size goes on a track/adjustment), Media management/Proxies (`285-289`: framing identical by construction), Ops table (`305-310`: `punch_in`), params list (`314-316`), new section "Placing media in the frame" with the fit table, formulas, worked examples, the 9:16 recipe (`output.fit: cover` + `punch_in`) and the stretch conversion formulas.
- `docs/parity/AGENT_GUIDE.md`: coordinate spaces beside the time-base table (`77-82`: source px vs output px), tracking paragraph (`52-57`), a "Place media without distortion" paragraph (media_probe → fit → punch_in → placements → stills badge).
- `docs/integrations/native-masks.md:61-62`, `docs/integrations/tracking.md:3/34/59`, `docs/integrations/NATIVE_MOTION.md:28`, `docs/integrations/filmcraft-interchange.md:23/25/59`.
- `docs/parity/FERROCUT_AUDIT.md:34` (nested comps) and `:51` (PAR note), `PREMIERE_PRO_INVENTORY.md` + `capabilities.json` (add Motion "Set/Scale to Frame Size" as native), `AFTER_EFFECTS_INVENTORY.md` AE-022 note (comp vs footage dimensions), `storytold.rs:62` capability text.
- `README.md:406-425` (native layers, fit, anchor space, scale meaning) plus a migration note; CHANGELOG entry; `CONTRIBUTING.md` note that new fields are rejected by older binaries.
- `examples/motion-showcase.json`, `examples/motion-revise.ops`, new `examples/reframe.json`, `scripts/gen-test-media.sh` (`wide.mov`), `ci/render-check.sh` + `ci/reference/expected.json` (new entries only), `eval/README.md` (two new tasks; note that the nine existing tasks use frame-sized clips on purpose), `eval/creative/claude-videos-20261008/CONTRACT.md:94` (bootstrap text mentions `fit`/`punch_in`), `docs/evaluations/2026-10-08-agent-video-tests.md` one-line follow-up.