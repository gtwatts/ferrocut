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
- `tracks`: video tracks, **0 = bottom layer**. Each has a unique `name`, an audio bus `audio`, and
  `clips`.
- A **video clip**: `id` (unique), `source` (path relative to the timeline file), `start`
  (timeline), `source_in` (source time of its first frame), `duration`, `opacity`, `transform`,
  `transition_in`, `audio` (linked audio: gain, pan, mute, fades, J/L offsets). A clip shows source
  `[source_in, source_in + duration)` at timeline `[start, start + duration)`.
- `audio_tracks`: audio-only tracks (music, dialogue) with a `bus` and audio clips
  (`id`, `source`, `start`, `source_in`, `duration`, `audio`).
- `audio`: `sample_rate` (48000), `master_gain_db`, `loudness` (`target_lufs`: -23 broadcast,
  -14 streaming; `true_peak_dbtp`, default -1).

## Rules the engine enforces

- Clips on a track don't overlap, except a clip with a dissolve `transition_in` of length `d` must
  overlap the previous clip by at least `d` (the overlap comes from **handles**: source media before
  the in point / after the out point). `add_transition` makes that overlap for you.
- `source_in >= 0` and `source_in + duration <=` the media length (`media_probe` tells you it).
- J-cuts (`audio.in_offset < 0`) need source before `source_in`; L-cuts need source after.
- Retimed clips (`speed` != 1 or `time_remap`) must stay inside the media over their whole
  mapped range (picture and audio regions).

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
- Opacity in [0, 1], pan in [-1, 1], duck ratio >= 1, loudness target in [-70, 0).

## Ops (edit_apply)

| Op | What it does |
|---|---|
| `add_track` | new empty video track (`index` 0 = bottom; default top) or audio track |
| `add_clip` | clip from a media file; probed; defaults: `start` = end of track, `source_in` 0, `duration` = rest of the media, `id` = file stem |
| `add_transition` | dissolve into `clip` from the previous clip; `align` `center` (default) / `start` / `end` relative to the cut; uses handles, moves nothing else; adds a matching audio crossfade |
| `set_param` | any parameter by name on a clip (`clip`), a track's bus (`track`) or the timeline (neither); `null` removes an optional object |
| `set_keyframes` | keyframes on an animatable parameter (`mode` replace / merge) |
| `split`, `trim`, `roll`, `slip`, `slide`, `move` | NLE trims and moves (see each op's schema) |
| `ripple_delete`, `ripple_insert` | remove / insert and close / open the gap (`all_tracks` = sync lock) |
| `jl_cut` | linked-audio offsets: `in_offset < 0` J-cut, `out_offset > 0` L-cut |
| `set_speed` | constant speed keeping the source range (duration = range / \|speed\|); `-1` reverses; `ripple`, `preserve_pitch` |
| `freeze_frame` | hold the frame at `at`: to the clip end (split), or insert a `duration` hold and push later clips |

Parameter names (`timeline_schema` part `params` lists unit, range, default and time base):

- video clip: `opacity`, `transform.position` (`.x`/`.y`), `transform.anchor`, `transform.scale`
  (uniform or `.x`/`.y`), `transform.rotation`, `transition_in`, `sampling`, `speed`, `time_remap`,
  `audio.gain_db`, `audio.pan`, `audio.mute`, `audio.fade_in`, `audio.fade_out`,
  `audio.crossfade_in`, `audio.preserve_pitch`
- audio clip: the `audio.*` ones, `speed`, `time_remap`
- track: `bus.gain_db`, `bus.pan`, `bus.mute`, `bus.duck` (`{key: [...]}`), `bus.duck.threshold_db`,
  `bus.duck.ratio`, `bus.duck.attack_ms`, `bus.duck.release_ms`, `bus.duck.range_db`
- timeline: `audio.master_gain_db`, `audio.loudness`, `audio.loudness.target_lufs`,
  `audio.loudness.true_peak_dbtp`, `output.duration`

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
- Keep one spoken line: `transcript_search` the words; each hit has `cut_in`/`cut_out` (source
  times with a handle, on the frame grid). For a clip at `start` S with `source_in` I, the
  timeline time of source time t is `S + t - I`: `split` there and `ripple_delete` the parts
  you don't want. `shots_list` gives shot boundaries the same way.

## Nodes

Video clips, opacity, transforms, dissolves and the audio graph above are what timelines contain
today. HTML, Lottie and color nodes (SeePlus's `ferrocut-html`, `ferrocut-lottie`,
`ferrocut-color`) will appear here with their published parameter schemas once they are wired
into the timeline format; until then they are not valid timeline content.

## Rendering and checking

`render` writes a lossless FFV1/PCM MKV and reuses unchanged chunks. `quality_check` (or
`render` with `check: true`) runs the perceptual checker: cuts, black/frozen/flash frames,
loudness, true peak, audio presence. `expect_audio` defaults to `auto`: a timeline with no audio
isn't expected to have any. Checker flags go in `args`; a wrong flag returns the checker's
`--help` in the error. The checker's report schema: `docs://perceive/check.schema.json`.
