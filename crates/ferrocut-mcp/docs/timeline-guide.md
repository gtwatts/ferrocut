# Ferrocut timeline authoring guide

A timeline is a JSON file. Change it with `edit_apply` ops, not by editing the JSON by hand: ops are
checked one by one (handles, media length, overlaps, ranges), applied atomically, journaled (`log`,
`undo`, `branch`) and reported with the span of output they change. `dry_run: true` previews.
Full schema: `timeline_schema` (part `timeline`), or the resource `docs://timeline/schema.json`.
Start with `docs://agent/onboarding.md` for a complete native graphics workflow and revision example.

## Values

- **Times and numbers are exact rationals**: integers or strings `"5"`, `"5/2"`, `"0.5"`,
  `"1001/30000"`. JSON floats (`2.5`) are rejected.
- **Animatable** numbers are a constant or `{"keyframes": [{"t": "0", "v": "0"}, {"t": "1", "v": "1",
  "interp": "ease_in_out"}]}`. `interp` (segment to the next key): `linear` (default), `hold`,
  `ease`, `ease_in`, `ease_out`, `ease_in_out`, `easy_ease`, `{"bezier": [x1, y1, x2, y2]}`,
  `{"speed": {...}}`. Before the first key the first value holds; after the last, the last.
  Any animatable number may instead be an **expression** (see "Expressions").
- **Key times**: clip parameters (opacity, transform, clip audio) are **clip-local** (0 = the clip's
  `start`); bus and master parameters use **timeline time**. `set_keyframes` with
  `timeline_time: true` converts timeline times to the parameter's base for you.

## Structure

- `output`: `width`, `height`, `fps`, optional `gop` (24), `gops_per_chunk` (1), `duration`
  (default: end of the last clip).
- `tracks`: video tracks, **0 = bottom layer**. Each has a unique `name`, an audio bus `audio`, an
  optional track `matte`, `visible` (default true), and `clips`.
- A **video clip**: `id` (unique), `source` (path relative to the timeline file), `start`
  (timeline), `source_in` (source time of its first frame), `duration`, `opacity`, `transform`,
  `transition_in`, `blend_mode`, `audio` (linked audio: gain, pan, mute, fades, J/L offsets). A clip shows source
  `[source_in, source_in + duration)` at timeline `[start, start + duration)`. Instead of `source`, a
  clip can have a `generator` (see Generator layers).
- `audio_tracks`: audio-only tracks (music, dialogue) with a `bus` and audio clips
  (`id`, `source`, `start`, `source_in`, `duration`, `audio`).
- `audio`: `sample_rate` (48000), `master_gain_db`, `loudness` (`target_lufs`: -23 broadcast,
  -14 streaming; `true_peak_dbtp`, default -1).
- `camera` and `motion_blur`: optional (see 3D layers and camera, Motion blur).

## Rules the engine enforces

- Clips on a track don't overlap, except a clip with a dissolve `transition_in` of length `d` must
  overlap the previous clip by at least `d` (the overlap comes from **handles**: source media before
  the in point / after the out point). `add_transition` makes that overlap for you.
- `source_in >= 0` and `source_in + duration <=` the media length (`media_probe` tells you it).
- J-cuts (`audio.in_offset < 0`) need source before `source_in`; L-cuts need source after.
- Retimed clips (`speed` != 1 or `time_remap`) must stay inside the media over their whole
  mapped range (picture and audio regions).
- Opacity in [0, 1], pan in [-1, 1], duck ratio >= 1, loudness target in [-70, 0).

## Time remapping

- `speed`: constant (`"2"`, `"1/2"`, `"-1"` reverse, `"0"` freeze) or keyframes in clip-local
  time (a speed ramp; source time is the integral of speed from `source_in`). Linear and hold
  segments integrate exactly; eased ones numerically (deterministic).
- `time_remap`: AE-style keys from clip-local time to source seconds (overrides `speed`).
- `sampling`: `nearest` (default; snaps to the source frame grid) or `frame_blend` (mixes the
  two neighbouring source frames for smooth slow motion).
- Linked audio follows the same map: resampled (pitch follows speed) or, with
  `audio.preserve_pitch: true`, time-stretched with pitch kept (WSOLA). Reverse plays backwards.
- Recipes: slow motion `set_speed` `"1/2"` with `ripple: true`; a ramp 1x -> 3x -> 1x is
  `set_keyframes` on `speed` (`[{t:0,v:1,interp:ease_in_out},{t:1,v:3,interp:ease_in_out},{t:2,v:1}]`);
  a 2 s freeze at 5 s is `freeze_frame` `{at: "5", duration: "2"}`.

## Nested compositions

- A clip whose `source` is a timeline file (`*.json`) shows that timeline (an AE precomp /
  Premiere nested sequence). `source_in` and the duration pick the part of the inner timeline;
  speed, time remap, opacity, transform and blend mode work as for media. The inner canvas
  is fitted into the parent with the clip fit; its fps may differ (nearest sampling snaps to the inner frame grid).
- Its linked audio is the inner mix (clip and bus gains, ducking, master gain; loudness
  normalization only on the outermost timeline).
- Frame keys compose, so after editing the inner file only the outer chunks that show the
  changed frames re-render. Comps nest to any depth; a cycle is an error.
- `nest` moves clips into a new comp file and puts one clip in their place; `unnest` puts a
  plain comp clip's contents back (trimmed to the part it shows).

## Generator layers

- A video clip with `generator` instead of `source` synthesizes its picture (no audio):
  - `{"type": "solid", "color": [r, g, b]}` (or `[r, g, b, a]`)
  - `{"type": "linear_gradient", "start": [x, y], "end": [x, y], "start_color", "end_color"}`
    (defaults: left-center to right-center, black to white)
  - `{"type": "radial_gradient", "center": [x, y], "radius", "start_color", "end_color"}`
    (defaults: frame center, half the diagonal, black to white)
  - gradients take `interpolation`: `display` (default; like After Effects' Gradient Ramp) or
    `linear` (even light).
- Colors are in [0, 1], display-referred Rec.709 like decoded video (a solid of 0.5 matches a 50 %
  video level), straight alpha. Positions are output pixels. Every number can be keyframed;
  generator key times are the clip's **source time**, mapped through its speed or time remap
  (at speed 1, clip-local + `source_in`; a new generator clip has `source_in` 0), so `split`
  and `trim` keep the animation in place. Components:
  `generator.color.r` ... `.a`, `generator.start.x`, ...
- Add one with `add_clip` `{"track": "V1", "generator": {...}, "duration": "5"}`; opacity,
  transform, blend modes, mattes, dissolves and speed work as for media.
- Native text: `{"type":"text","text":{"content":"A title","font":"fonts/NotoSans-Regular.ttf",
  "font_size":"64","position":["80","120"],"fill":[1,1,1,1]}}`.
  Fonts and `fallback_fonts` are explicit assets relative to the timeline. Their bytes enter
  the cache keys and must be inside the configured MCP project root. Text supports shaping,
  wrapping, paragraph alignment, tracking, line height, an outside stroke and range-selector
  animators for position, scale, rotation, fill and opacity. It currently uses one text style
  per clip; rich style runs and text on a path are not implemented.
- Native vectors: `{"type":"shape","shape":{"geometry":{"type":"rectangle","x":"80","y":"80",
  "width":"320","height":"180","radius":"16"},"fill":{"type":"solid","color":[1,1,1,1]}}}`.
  Geometry can be `rectangle`, `ellipse` or a `path` with move/line/quad/cubic/close commands.
  Fills and strokes support solid, linear and radial gradients, plus caps, joins and dashes.
  Use `timeline_schema` for their exact nested fields. Shape groups/operators and masks
  attached to arbitrary clips are not implemented; native shapes can supply track mattes.
- Edit text through `generator.text.content`, `generator.text.font_size`,
  `generator.text.position`, `generator.text.animators` and other discovered params.
  Edit shapes through `generator.shape.geometry`, `generator.shape.fill`,
  `generator.shape.stroke` and discovered numeric fields. Replace complete animator,
  path-command or gradient-stop arrays when needed. `set_param` on `generator` replaces
  the full generator; changing its type requires a complete valid payload.

## 3D layers and camera

- `three_d: true` on a video clip (After Effects' 3D switch) makes it a flat card in 3D space:
  x right, y down, z away from the viewer, in output pixels. Extra transform fields (need the
  switch): `position_z`, `anchor_z`, `rotation_x` (positive tilts the bottom edge away),
  `rotation_y` (positive brings the right edge towards you), `orientation` `[x, y, z]`;
  `rotation` is the Z rotation. Order: orientation, then X, Y, Z rotation, then scale.
- `camera` on the timeline (keys in **timeline time**): `position` `[x, y, z]`,
  `point_of_interest` `[x, y, z]` (it looks there, y up on screen), and `zoom` (pixels at which a
  layer at that distance is 100 %) or `fov_deg` (horizontal). Default: After Effects' 50 mm
  camera, `zoom = width * 50 / 36` at `[w/2, h/2, -zoom]` looking at `[w/2, h/2, 0]`, so an
  untransformed 3D layer looks exactly like the 2D one. Setting one component (`camera.position.x`)
  fills the others from that default; set `position.z` to `-zoom` yourself if you change `zoom`.
- Default `renderer: "legacy"` stacking: consecutive 3D layers (tracks whose clip is 3D at that time; empty tracks are
  skipped) are drawn farthest first by the camera-space depth of their position (ties keep
  track order); a 2D layer in between splits the run and stays in track order. No
  intersections, lights, shadows or depth of field. Parts of a card less than 1 px in front of
  the camera are not drawn.
- Recipe: a dolly past generator cards: `set_param` `three_d` true and `transform.position_z`
  on each card, then `set_keyframes` `camera.position.x` (and keep `camera.point_of_interest`
  fixed for an orbit-like move).

## Native planar depth (opt-in)

Set timeline `renderer` to `depth_layers_v1` to resolve intersections per pixel,
including fractional source alpha and cutout windows. The legacy default and its
keys remain unchanged when the new mode/controls are absent. This is a bounded
planar compositor, without native lights, shadows, aperture focus or meshes.

- The existing camera position/target/zoom controls apply. New mode-only controls:
  `camera.reference_up` (animatable vector, default `[0,-1,0]`), `camera.roll`
  (animatable degrees about the forward axis, default 0), `camera.near` (fixed,
  default 1) and `camera.far` (fixed, default 100000). Require `0 < near < far`.
  Coincident position/target, zero up or collinear up/view direction are errors;
  there is no automatic replacement axis. Camera validity is rechecked at every
  shutter sample, including between valid keyframes.
- Visible, unconsumed 2D tracks delimit scenes even during clip gaps. Hidden or
  consumed matte tracks do not. Each scene accepts at most **16 authored clips**
  across its consecutive 3D tracks, with no overlapping clips on a single track.
  Inactive authored clips still count toward this bound. Higher track/start
  ordinal wins **exact represented Depth32 ties**; there is no depth epsilon.
- Sources retain native fit/anchor and signed storage windows. Every texture in a
  scene must have one common positive pixel aspect; normalize mixed-PAR sources
  before combining them. A nested composition is one textured plane, with its
  source-in/speed/remap intact. No implicit collapse-through; `unnest` refuses
  either a parent or inner depth composition to avoid changing scene boundaries.
- Only normal blending is supported within a scene. Mixing 2D/3D on one track,
  track effects, adjustment clips, dissolves or matte participation by 3D tracks
  is rejected with affected IDs. Apply clip effects before projection, or author
  the effect/matte inside a nested texture or after a nested scene.
- With timeline motion blur, every card in a scene must use the same layer blur
  switch. Geometry, camera and visibility resolve together at each exact shutter
  sample, then complete sample colors are averaged. Content/masks/clip effects
  are held at the nominal frame; a card visible only during shutter samples uses
  its nearest active sample (earlier on ties). This is transform/camera blur,
  not animated-content blur.
- Filtering is bilinear in source space, with single-sample raster edges. Very
  small/oblique cards can alias; this does not claim legacy Catmull-Rom filtering
  equivalence. Empty projected cards contribute nothing; exactly singular cards
  warn once per clip/worker. Clipping is against the homogeneous near/far planes.
- Scratch uses about 40 B/output pixel per worker, plus one 8 B/pixel returned
  texture, up to two extra working frames for shutter averaging, and source
  textures. CLI and MCP automatic job sizing include this additional model;
  explicit job counts and runtime memory backoff keep their existing behavior.
  The model is not measured peak memory; overscan/effects can need more.

The [Crossing Glass](../../../examples/crossing-glass/README.md) source package
contains a legacy before case, native candidate and negative controls. Its
source checkpoint is not evidence of rendered, inspected or installed behavior.

## Motion blur

- `motion_blur` on the timeline turns it on (After Effects' composition switch):
  `{"shutter_angle": 180, "shutter_phase": -90, "samples": 16}` (defaults; angle [0, 720],
  phase [-360, 360], samples 2..64); clips opt in with `motion_blur: true` (the layer switch).
- Each frame averages the layer resampled at `samples` sub-frame times
  `t + (phase + angle * (i + 1/2) / samples) / 360 / fps` (exact, deterministic). Transform and
  camera motion blur; motion inside the clip's content does not. Frames where the layer does
  not move render exactly like unblurred ones.

## Blend modes and track mattes

- `blend_mode` on a video clip: how its track composites onto everything below while the clip is
  active (`normal` = over, `add`, `multiply`, `screen`, `overlay`, `soft_light`, `hard_light`,
  `darken`, `lighten`, `difference`, `exclusion`, `color_dodge`, `color_burn`, `hue`,
  `saturation`, `color`, `luminosity`). Computed in linear light on premultiplied pixels with the
  W3C / After Effects formulas; modes defined on [0, 1] clamp their inputs (HDR values > 1 only
  survive `normal`, `add`, `multiply`, `darken`, `lighten`, `difference`).
- `matte` on a video track takes `mode`: `alpha`, `alpha_inverted`, `luma` or
  `luma_inverted`. Alpha uses source coverage; luma uses ACEScg (AP1) luminance of
  premultiplied RGB, i.e. brightness times alpha. Inverted modes use one minus
  that coverage. Outside the source's clips, its picture is transparent black.
- Reuse one nonadjacent source on multiple tracks with
  `{"mode":"alpha","source":{"track":"Stencil"}}`. The name must be an
  existing unique nonempty **video** track in this composition; track insertion
  and reordering do not retarget it. Source picture includes its own clips,
  transforms, effects and matte. Acyclic chains are supported; self/cycles,
  missing names and adjustment-layer sources are errors. Source and recipient
  sample the same exact composition time, each with its own clip retiming.
- Set `visible:false` on a video track to hide only its final stack picture.
  It remains available as a named matte and its linked audio still plays
  (`bus.mute` controls audio). Visibility is a fixed Boolean, not animation.
  A source reference never crosses a nested-composition boundary. Hidden-source
  assets still undergo the normal root/validity checks.
- Legacy `{"mode":"alpha"}` (omitted source) or `"source":"track_above"`
  consumes the directly adjacent track above it. That source cannot have a matte
  itself and remains consumed regardless of either track's visibility. Old
  adjacent projects retain this behavior. Use named sources throughout to control
  source visibility independently.
- Recipe: text through a picture: put the picture on V1, the title on V2 and
  `set_param` `matte` `{"mode": "alpha"}` on V1 (`track: "V1"`).
- Reusable recipe: apply `set_param` on Panel A and Panel B with `param:"matte"`,
  `value:{"mode":"alpha","source":{"track":"Stencil"}}`; then on Stencil
  set `param:"visible",value:false`. Use `plan:true`, dry-run and undo normally.
  Track rename/delete operations are not provided; dangling references fail
  validation. `nest` / `unnest` reject hidden or matte relationships they would
  discard; author an explicit nested project in those cases. The original
  `examples/reusable-mattes/` demo and the reusable-matte design document record
  the source contract and execution status.

## Video effects

- `effects` on a clip (on the clip's picture after retiming and before its transform; the clip
  `opacity` applies after them; keyframes clip-local) and on a video track (on the track's
  picture before its matte; timeline time): an ordered stack of `{"type": ..., "id"?: name,
  "enabled"?: true, params}`; the first entry runs first. Numeric params take a rational
  (decimals as strings, `"0.8"`) or `{"keyframes": [...]}`; omitted ones take the default.
  `enabled: false` bypasses an effect without losing its settings.
  - `gaussian_blur`: `sigma` px (5; radius ~3 sigma), `dimensions` both | horizontal | vertical,
    `repeat_edges` (false: blurs into transparency and grows the picture; true for full-frame video)
  - `directional_blur`: `angle` deg (0 = horizontal), `length` px (10)
  - `unsharp_mask`: `amount` (0.5), `sigma` px (1), `threshold` (0); `sharpen`: `amount` (0.5)
  - `glow`: `threshold` (0.8, linear luminance), `sigma` px (10), `intensity` (1), `color` tint
  - `drop_shadow`: `color` ([0, 0, 0, 1]), `opacity` (0.5), `angle` deg (135 = down-right,
    After Effects convention), `distance` px (5), `softness` px (0), `shadow_only`
  - `transform`: `anchor`, `position` ([x, y] px, default frame center), `scale` (1 or [x, y]),
    `rotation` deg, `opacity` (1)
  - `crop`: `left`, `top`, `right`, `bottom` px from the frame edges, `feather` px (inward)
  - `letterbox`: `aspect` (2.39), `color` ([0, 0, 0, 1]), `opacity` (1); pillarbox when the
    aspect is narrower than the frame
  - `exposure`: `stops` (0); `contrast`: `amount` (1), `pivot` (0.18);
    `saturation`: `amount` (1); these operate in linear ACEScg
  - `lift_gamma_gain`: RGB vectors `lift` ([0,0,0]), `gamma` ([1,1,1]), `gain` ([1,1,1]);
    operates in encoded Rec.709 before conversion back to linear ACEScg
  - `chroma_key`: `key_color` ([0,1,0]), `tolerance` (0.1), `softness` (0.1), `spill` (0);
    a basic chroma-distance keyer, with no matte-cleanup or edge-refinement controls
  - `luma_key`: `threshold` (0.1), `softness` (0.1), `invert` (false)
  - other types registered by plug-in crates (color grading, OFX) appear in timeline_schema
    `params` -> `video_effects` with their params and work the same way.
- Ops: `add_video_effect` (`effect`, optional `index`), `set_video_effect_param` (`effect` =
  index or id, `param` such as `sigma`, `color.g`, `enabled`; `value` incl. keyframes, `null` =
  default), `remove_video_effect`, `move_video_effect` (`to`); each takes `clip` or `track`.
- Effects work in linear light on premultiplied pixels; blurs, glows and shadows may extend
  past the layer's edges (until a later `crop`), so put `crop` last to trim them.

## Expressions

Any animatable number (opacity, one component of `transform.position`, a video effect's `sigma`,
`audio.gain_db`, `camera.zoom`, ...) can be an After Effects-style expression:
`{"expression": "wiggle(2, 30)", "value": "960"}`. Set it with `set_param` / `set_video_effect_param`
(value = the expression object). `value` (optional; default the parameter's default) is the
pre-expression value, a constant or keyframes, available as `value` in the script.

- Syntax: [rhai](https://rhai.rs). Integer division truncates: write `1.0 / 2`. The last expression
  is the result and must be a number. Sandboxed: no imports, files, clock, `eval`; operation and
  recursion limits.
- Variables: `time` (seconds in the parameter's time base: clip-local for clip parameters, source
  time for generator parameters, timeline time for tracks/timeline), `value`, `fps`, `frame`,
  `comp_time` (timeline seconds), `duration`, `in_point`.
- Functions: `wiggle(freq, amp [, octaves, amp_mult, t])` (returns `value` + smooth deterministic
  noise, seeded per parameter: the same expression on x and y wiggles independently),
  `seed_random(n [, timeless])`, `random()`, `random(max)`, `random(min, max)`, `noise(x)`,
  `linear(t, t_min, t_max, v1, v2)` / `linear(t, v1, v2)` and `ease`, `ease_in`, `ease_out` alike,
  `clamp(x, lo, hi)`, `lerp(a, b, s)`, `value_at_time(t)`, `loop_out(type [, keys])` /
  `loop_in(...)` over `value`'s keyframes (`"cycle"` default, `"pingpong"`, `"offset"`,
  `"continue"`).
- References: `param("transform.rotation")` (same clip/track), `layer("clip_id").param("opacity")`,
  `track("V2").param("audio.gain_db")`, `comp().param("camera.zoom")`; vector components as `.x`/`.y`
  (or `.0`), effects by id or index (`"effects.blur.sigma"`). Evaluated at the same timeline time;
  cycles are errors naming the chain.
- Evaluation is deterministic and done when the timeline is validated (every op, load, render): each
  expression becomes one value per frame (linear in between) that is part of the cache keys, so
  only frames whose values change re-render. Errors name the owner, parameter, time, line and
  column, e.g. `clip "a": opacity: the expression gives 1.25 at clip time 1/2 s (frame 12), above
  the maximum 1`.
- Source-clock expressions follow constant speed, reverse and monotonic keyframed remaps.
  A repeated source time must produce the same value. Expression-driven speed/remap cannot
  yet be combined with source expressions or source-parameter reads, even from another clip;
  bake the timing curve to ordinary keyframes first. Nonmonotonic remaps with source expressions
  fail with guidance rather than producing an ambiguous baked animation.
- Example: `{"op": "set_param", "clip": "title", "param": "transform.position.y", "value":
  {"expression": "value + 20 * sin(time * 6.283)", "value": "540"}}`; a looping bounce:
  `set_param` `{"expression": "loop_out(\"pingpong\")", "value": {"keyframes": [...]}}`.

## Adjustment layers

- A clip with `"adjustment": true` (no source or generator) and `effects` applies its stack to
  everything composited below it while it is active (After Effects / Premiere adjustment
  layer); its `opacity` mixes the result with the untouched picture, and a track `matte` on its
  track (an adjacent or named alpha / luma source) limits where it applies. An adjustment track
  holds only adjustment clips; they take no transform, 3D, blend mode, speed or transition (use
  a `transform` effect, or a matte, instead).
- Build one: `add_track` (video, above the tracks it should affect), `add_clip` `{"track":
  "ADJ", "adjustment": true, "start": "2", "duration": "3"}` (id defaults to `adjustment`),
  then `add_video_effect` on that clip.

## Audio effects

- `audio.effects` on a clip (after its gain, fades and pan; keyframes clip-local) and
  `bus.effects` on a track (on the summed clips before the bus gain/balance, i.e. pre-fader;
  keyframes in timeline time): an ordered list of `{"type": ..., params}`. Every numeric
  parameter takes a rational or `{"keyframes": [...]}`; omitted ones take the default.
  - `eq`: `bands: [{kind: peak | low_shelf | high_shelf (peak), freq_hz, gain_db (0), q (0.7071)}]`
  - `high_pass`, `low_pass`: `freq_hz`, `q` (0.7071); 12 dB/oct
  - `compressor`: `threshold_db` (-20), `ratio` (4, >= 1), `attack_ms` (10), `release_ms` (100),
    `knee_db` (6), `makeup_db` (0); stereo-linked peak detector, soft knee
  - `limiter`: `ceiling_db` (-1, <= 0), `release_ms` (50); sample-peak brickwall, zero latency
    (the loudness stage still adds the true-peak limiter on the master)
  - `gate`: `threshold_db` (-50), `range_db` (40), `attack_ms` (1), `hold_ms` (50), `release_ms` (100)
- Ops: `add_effect` (`effect`, optional `index`), `set_effect_param` (`index`, `param` such as
  `threshold_db` or `bands.1.gain_db`, `value` incl. keyframes, `null` = default),
  `remove_effect` (`index`); each takes `clip` or `track`.
- Rendering streams the audio in 5 s chunks and caches each one: after an edit only the chunks
  it touches are re-mixed (the loudness pass re-measures from per-chunk records).

## Markers

- `markers` on the timeline (time in timeline seconds) and on video/audio clips (time in the
  clip's **source** seconds, so a marker stays on its frame through trims, slips and moves;
  after a split each part keeps the markers in its own range): `[{id, time, duration?, name?,
  color?, comment?}]`, colors `green` (default), `red`, `purple`, `orange`, `yellow`, `white`,
  `blue`, `cyan`; `duration` > 0 makes a range marker. Timeline markers do not move with
  ripple edits.
- Ops `add_marker` / `update_marker` / `remove_marker` (with `clip` for clip markers;
  `timeline_time: true` converts a timeline time to the clip's source time). `markers_list`
  returns all of them in timeline time. Markers never change the render (no chunk re-renders),
  so use them freely to note beats, shots, problems and decisions.

## Media management

- `media_status`: every file the timeline uses, online or offline, the clips using it, and
  its proxy. `relink` (edit op) points clips at moved media: `clip` + `to`, `from` + `to`
  (file or directory prefix) or `search` (find offline files by name under a directory).
- Proxies: `proxy_generate` makes half-resolution DNxHR LB proxies (FFV1 for tiny or alpha
  media) in `<media dir>/.ferrocut-proxies/`, keyed by the file's content. `render` with
  `proxies: true` is a draft: it reads proxies where they exist (summary `draft: true`) and
  caches its chunks apart. Renders without `proxies`, and every `deliver`, use the original
  full-resolution media: the swap back is automatic.

## Ops (edit_apply)

| Op | What it does |
|---|---|
| `add_track` | new empty video track (`index` 0 = bottom; default top) or audio track |
| `add_clip` | clip from a media file (probed; defaults: `start` = end of track, `source_in` 0, `duration` = rest of the media, `id` = file stem) or a `generator` layer (`duration` required, `id` = its type) |
| `add_transition` | dissolve into `clip` from the previous clip; `align` `center` (default) / `start` / `end` relative to the cut; uses handles, moves nothing else; adds a matching audio crossfade |
| `set_param` | any parameter by name on a clip (`clip`), a track (`track`: its bus, `visible`, `matte` or effects) or the timeline (neither); `null` removes an optional object |
| `set_keyframes` | keyframes on an animatable parameter (`mode` replace / merge) |
| `split`, `trim`, `roll`, `slip`, `slide`, `move` | NLE trims and moves (see each op's schema) |
| `ripple_delete`, `ripple_insert` | remove / insert and close / open the gap (`all_tracks` = sync lock) |
| `jl_cut` | linked-audio offsets: `in_offset < 0` J-cut, `out_offset > 0` L-cut |
| `set_speed` | constant speed keeping the source range (duration = range / \|speed\|); `-1` reverses; `ripple`, `preserve_pitch` |
| `freeze_frame` | hold the frame at `at`: to the clip end (split), or insert a `duration` hold and push later clips |
| `nest` | move video `clips` (any tracks) into a new comp file `path` and replace them with one clip `id` on the lowest of their tracks |
| `unnest` | replace a plain comp clip by the comp's clips; extra inner tracks go on new tracks right above |
| `add_effect`, `set_effect_param`, `remove_effect` | audio effect chain of a clip (`clip`) or track bus (`track`); see Audio effects |
| `add_video_effect`, `set_video_effect_param`, `remove_video_effect`, `move_video_effect` | video effect stack of a clip (`clip`, incl. adjustment layers) or video track (`track`); see Video effects |
| `add_marker`, `update_marker`, `remove_marker` | timeline or clip (`clip`) markers; see Markers |
| `relink` | point clips at moved / offline media; see Media management |

Parameter names (`timeline_schema` part `params` lists unit, range, default and time base):

- video clip: `opacity`, `transform.position` (`.x`/`.y`), `transform.anchor`, `transform.scale`
  (uniform or `.x`/`.y`), `transform.rotation`, `three_d`, `transform.position_z`,
  `transform.anchor_z`, `transform.rotation_x`, `transform.rotation_y`, `transform.orientation`
  (`.x`/`.y`/`.z`), `motion_blur`, `transition_in`, `sampling`, `blend_mode`, `speed`,
  `time_remap`, `audio.gain_db`, `audio.pan`, `audio.mute`, `audio.fade_in`, `audio.fade_out`,
  `audio.crossfade_in`, `audio.preserve_pitch`, `audio.effects`, `effects` (video effects),
  `adjustment`; generator clips: `generator`,
  `generator.color` (`.r`/`.g`/`.b`/`.a`), `generator.start_color`, `generator.end_color`,
  `generator.start`, `generator.end`, `generator.center` (`.x`/`.y`), `generator.radius`,
  `generator.interpolation`
- audio clip: the `audio.*` ones, `speed`, `time_remap`
- track: `visible`, `matte` (video tracks), `bus.gain_db`, `bus.pan`, `bus.mute`, `bus.effects`, `bus.duck` (`{key: [...]}`), `bus.duck.threshold_db`,
  `bus.duck.ratio`, `bus.duck.attack_ms`, `bus.duck.release_ms`, `bus.duck.range_db`
- timeline: `audio.master_gain_db`, `audio.loudness`, `audio.loudness.target_lufs`,
  `audio.loudness.true_peak_dbtp`, `output.duration`, `renderer`, `camera`, `camera.position`,
  `camera.point_of_interest` and `camera.reference_up` (`.x`/`.y`/`.z`), `camera.zoom`, `camera.fov_deg`,
  `camera.roll`, `camera.near`, `camera.far`, `motion_blur`,
  `motion_blur.shutter_angle`, `motion_blur.shutter_phase`, `motion_blur.samples`

## Recipes

```json
[{"op": "add_clip", "track": "V1", "source": "media/a.mkv", "source_in": "1", "duration": "4"},
 {"op": "add_clip", "track": "V1", "source": "media/b.mkv", "source_in": "1/2", "duration": "4"},
 {"op": "add_transition", "clip": "b", "duration": "1"},
 {"op": "set_keyframes", "clip": "a", "param": "opacity", "keyframes": [{"t": "0", "v": "0"}, {"t": "1", "v": "1"}]},
 {"op": "set_param", "param": "audio.loudness", "value": {"target_lufs": "-23", "true_peak_dbtp": "-2"}}]
```

- Fade from / to black: keyframe the clip's `opacity` 0 -> 1 over its first second, 1 -> 0 over
  its last (clip-local times, or `timeline_time: true`).
- Remove a shot and close the gap: `ripple_delete`.
- Sound leads picture by 1 s: `jl_cut` with `in_offset: "-1"` on the incoming clip.
- Music under dialogue: `add_track` audio, `add_clip`, then `set_param` `bus.duck`
  `{"key": ["V1"]}` on the music track.
- Cleaner dialogue: `add_effect` `{"type": "high_pass", "freq_hz": "80"}` then
  `{"type": "compressor", "threshold_db": "-24", "ratio": "3"}` on the dialogue clip or track.
- Keep one spoken line: `transcript_search` the words; each hit has `cut_in`/`cut_out` (source
  times with a handle, on the frame grid). For a clip at `start` S with `source_in` I, the
  timeline time of source time t is `S + t - I`: `split` there and `ripple_delete` the parts
  you don't want. `shots_list` gives shot boundaries the same way.

## Nodes

Video clips, generator layers (solid, gradients, native text and vectors), opacity, 2D and 3D transforms with a camera,
motion blur, dissolves and the audio graph above are what timelines contain today. HTML, Lottie and color nodes (SeePlus's `ferrocut-html`, `ferrocut-lottie`,
`ferrocut-color`) will appear here with their published parameter schemas once they are wired
into the timeline format; until then they are not valid timeline content.

## Rendering and checking

`preview_frames` renders chosen output frames to PNG stills and a labeled contact sheet
straight from the graph (no video encode): the same 8-bit Rec.709 pixels a master would
hold. Pick frames by timeline time (`at`), index (`frames`) or `spread` (N evenly spaced,
default 12). The sheet (or a single frame) comes back inline as an image; `each: true` also
writes full-resolution PNGs, the right way to check small text. Look before and after every
edit batch; it is far cheaper than a draft render and shows exactly what will be encoded.

`render` writes a lossless FFV1/PCM MKV and reuses unchanged chunks. `quality_check` (or
`render` with `check: true`) runs the perceptual checker: cuts, black/frozen/flash frames,
loudness, true peak, audio presence. `expect_audio` defaults to `auto`: a timeline with no audio
isn't expected to have any. Checker flags go in `args`; a wrong flag returns the checker's
`--help` in the error. The checker's report schema: `docs://perceive/check.schema.json`.


## Placing media without distortion

Media and nested compositions decode at their native dimensions. A clip's `fit`
overrides `output.fit`; when both are absent the default is `contain`.

| Fit | Placement before the user transform |
| --- | --- |
| `contain` | Uniform scale to show the entire picture, centered, with transparent bars |
| `cover` | Uniform scale to fill the output, centered, cropping overflow |
| `none` | Native pixels at 1:1, centered |
| `stretch` | Independent horizontal/vertical scale to fill; warns on aspect distortion |

`add_clip` accepts `fit`. Set it with `set_param` on a clip, or set `output.fit`
on the timeline. `null` removes either override. Fit is not animatable and
cannot be set on generators or adjustment layers.

`transform.anchor` is in native source pixels (use `media_probe` dimensions),
`transform.position` is in output pixels, and `transform.scale` multiplies the
fit. Default anchor is source center; default position is output center. For a
1280x534 source in a 1920x1080 sequence, contain uses exactly 3/2 on both axes,
showing a 1920x801 picture at y=139.5. Cover uses 180/89 on both axes. A vertical
cutdown can set `output.fit` to `cover` and then reframe individual clips with
position/anchor/scale. Clip masks and pixel-unit clip effects run in source
pixels before fit; track and adjustment effects run in output pixels.

`plan` and render reports include base `placements` (native/output dimensions,
effective fit and exact fit factors) and stretch `warnings`. These entries are
before user transforms, not per-frame bounding boxes. Proxy decoding conforms
to the original native dimensions so placement is identical in draft and final.

Migration: earlier builds stretched media to output size before transforming.
Mismatched sources now default to contain. `fit: stretch` restores framing,
though resampling pixels differ. Convert old stretched-layer anchor/mask x
coordinates by source_width/output_width and y coordinates by
source_height/output_height. Remove compensating per-axis scales as appropriate.
Equal-size source/output pictures preserve the old path and cache keys.

This first placement slice does not add `punch_in`, contact-sheet badges,
per-edit placement reports, or transformed-source tracking. Tracking continues
to require an untransformed source and now maps displacement through its fit.
For an unset media anchor, component edits need probing; with probing disabled,
set both anchor components. Expressions referencing an unset media anchor must
set it explicitly; anchor expressions need an explicit pre-expression `value`.
No media probing occurs inside expression validation or MCP path pre-checks.
