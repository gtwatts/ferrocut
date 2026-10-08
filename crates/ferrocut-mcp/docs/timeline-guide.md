# Ferrocut timeline authoring guide

A timeline is a JSON file. Change it with `edit_apply` ops, not by editing the JSON by hand: ops are
checked one by one (handles, media length, overlaps, ranges), applied atomically, journaled (`log`,
`undo`, `branch`) and reported with the span of output they change. `dry_run: true` previews.
Full schema: `timeline_schema` (part `timeline`), or the resource `docs://timeline/schema.json`.

## Values

- **Times and numbers are exact rationals**: integers or strings `"5"`, `"5/2"`, `"0.5"`,
  `"1001/30000"`. JSON floats (`2.5`) are rejected.
- **Animatable** numbers are a constant or `{"keyframes": [{"t": "0", "v": "0"}, {"t": "1", "v": "1",
  "interp": "ease_in_out"}]}`. `interp` (segment to the next key): `linear` (default), `hold`,
  `ease`, `ease_in`, `ease_out`, `ease_in_out`, `easy_ease`, `{"bezier": [x1, y1, x2, y2]}`,
  `{"speed": {...}}`. Before the first key the first value holds; after the last, the last.
- **Key times**: clip parameters (opacity, transform, clip audio) are **clip-local** (0 = the clip's
  `start`); bus and master parameters use **timeline time**. `set_keyframes` with
  `timeline_time: true` converts timeline times to the parameter's base for you.

## Structure

- `output`: `width`, `height`, `fps`, optional `gop` (24), `gops_per_chunk` (1), `duration`
  (default: end of the last clip).
- `tracks`: video tracks, **0 = bottom layer**. Each has a unique `name`, an audio bus `audio`, an
  optional track `matte`, and `clips`.
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
  speed, time remap, opacity, transform and blend mode work as for media. The inner frame size
  must match; its fps may differ (nearest sampling snaps to the inner frame grid).
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
  generator key times are the clip's **source time** (clip-local + `source_in`; a new generator
  clip has `source_in` 0), so `split` and `trim` keep the animation in place. Components:
  `generator.color.r` ... `.a`, `generator.start.x`, ...
- Add one with `add_clip` `{"track": "V1", "generator": {...}, "duration": "5"}`; opacity,
  transform, blend modes, mattes, dissolves and speed work as for media.
- Vector shapes (rectangles, ellipses, paths with fill/stroke) and shape masks are not
  generators: they come from the Lottie/ThorVG path (`ferrocut-lottie`).

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
- Stacking: consecutive 3D layers (tracks whose clip is 3D at that time; empty tracks are
  skipped) are drawn farthest first by the camera-space depth of their position (ties keep
  track order); a 2D layer in between splits the run and stays in track order. No
  intersections, lights, shadows or depth of field. Parts of a card less than 1 px in front of
  the camera are not drawn.
- Recipe: a dolly past generator cards: `set_param` `three_d` true and `transform.position_z`
  on each card, then `set_keyframes` `camera.position.x` (and keep `camera.point_of_interest`
  fixed for an orbit-like move).

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
- `matte` on a video track: `{"mode": "alpha" | "alpha_inverted" | "luma" | "luma_inverted"}`.
  The track directly above becomes the matte (it is not composited); `luma` uses the
  ACEScg (AP1) luminance of the premultiplied matte, i.e. luminance times alpha. The top track can't have a matte, and a matte
  source can't have its own matte.
- Recipe: text through a picture: put the picture on V1, the title on V2 and
  `set_param` `matte` `{"mode": "alpha"}` on V1 (`track: "V1"`).

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
| `set_param` | any parameter by name on a clip (`clip`), a track (`track`: its bus, or `matte`) or the timeline (neither); `null` removes an optional object |
| `set_keyframes` | keyframes on an animatable parameter (`mode` replace / merge) |
| `split`, `trim`, `roll`, `slip`, `slide`, `move` | NLE trims and moves (see each op's schema) |
| `ripple_delete`, `ripple_insert` | remove / insert and close / open the gap (`all_tracks` = sync lock) |
| `jl_cut` | linked-audio offsets: `in_offset < 0` J-cut, `out_offset > 0` L-cut |
| `set_speed` | constant speed keeping the source range (duration = range / \|speed\|); `-1` reverses; `ripple`, `preserve_pitch` |
| `freeze_frame` | hold the frame at `at`: to the clip end (split), or insert a `duration` hold and push later clips |
| `nest` | move video `clips` (any tracks) into a new comp file `path` and replace them with one clip `id` on the lowest of their tracks |
| `unnest` | replace a plain comp clip by the comp's clips; extra inner tracks go on new tracks right above |
| `add_effect`, `set_effect_param`, `remove_effect` | audio effect chain of a clip (`clip`) or track bus (`track`); see Audio effects |
| `add_marker`, `update_marker`, `remove_marker` | timeline or clip (`clip`) markers; see Markers |
| `relink` | point clips at moved / offline media; see Media management |

Parameter names (`timeline_schema` part `params` lists unit, range, default and time base):

- video clip: `opacity`, `transform.position` (`.x`/`.y`), `transform.anchor`, `transform.scale`
  (uniform or `.x`/`.y`), `transform.rotation`, `three_d`, `transform.position_z`,
  `transform.anchor_z`, `transform.rotation_x`, `transform.rotation_y`, `transform.orientation`
  (`.x`/`.y`/`.z`), `motion_blur`, `transition_in`, `sampling`, `blend_mode`, `speed`,
  `time_remap`, `audio.gain_db`, `audio.pan`, `audio.mute`, `audio.fade_in`, `audio.fade_out`,
  `audio.crossfade_in`, `audio.preserve_pitch`, `audio.effects`; generator clips: `generator`,
  `generator.color` (`.r`/`.g`/`.b`/`.a`), `generator.start_color`, `generator.end_color`,
  `generator.start`, `generator.end`, `generator.center` (`.x`/`.y`), `generator.radius`,
  `generator.interpolation`
- audio clip: the `audio.*` ones, `speed`, `time_remap`
- track: `matte` (video tracks), `bus.gain_db`, `bus.pan`, `bus.mute`, `bus.effects`, `bus.duck` (`{key: [...]}`), `bus.duck.threshold_db`,
  `bus.duck.ratio`, `bus.duck.attack_ms`, `bus.duck.release_ms`, `bus.duck.range_db`
- timeline: `audio.master_gain_db`, `audio.loudness`, `audio.loudness.target_lufs`,
  `audio.loudness.true_peak_dbtp`, `output.duration`, `camera`, `camera.position`,
  `camera.point_of_interest` (`.x`/`.y`/`.z`), `camera.zoom`, `camera.fov_deg`, `motion_blur`,
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

Video clips, generator layers (solid, gradients), opacity, 2D and 3D transforms with a camera,
motion blur, dissolves and the audio graph above are what timelines contain today. HTML, Lottie and color nodes (SeePlus's `ferrocut-html`, `ferrocut-lottie`,
`ferrocut-color`) will appear here with their published parameter schemas once they are wired
into the timeline format; until then they are not valid timeline content.

## Rendering and checking

`render` writes a lossless FFV1/PCM MKV and reuses unchanged chunks. `quality_check` (or
`render` with `check: true`) runs the perceptual checker: cuts, black/frozen/flash frames,
loudness, true peak, audio presence. `expect_audio` defaults to `auto`: a timeline with no audio
isn't expected to have any. Checker flags go in `args`; a wrong flag returns the checker's
`--help` in the error. The checker's report schema: `docs://perceive/check.schema.json`.
