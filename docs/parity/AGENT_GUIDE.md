# Ferrocut agent onboarding

Verified against the working tree on 2026-10-08. This guide describes implemented interfaces; the [Premiere inventory](PREMIERE_PRO_INVENTORY.md) and [After Effects inventory](AFTER_EFFECTS_INVENTORY.md) describe requirements, not a claim of Adobe parity.

Ferrocut combines editing and composition in one inspectable project. Treat a timeline as a sequence, a video track as a compositing layer, a timeline used as a clip source as a nested composition, and a generator as editable native artwork. Work through typed operations, inspect their reported changes, render, and revise. Familiar concepts should carry over from Premiere, After Effects, and ordinary programming without requiring a browser or HTML layout engine.

## Discover the installed interface

Start with the tools and schemas exposed by the running build. An agent should retrieve the relevant sections when needed instead of assuming that a feature in this backlog already exists.

| Need | Current interface |
| --- | --- |
| Distinguish retained source, compiled engines and connected features | `capabilities {}` |
| Find an effect and its exact controls | `effects_catalog {"query":"glow","details":true}` |
| Inspect a project and its journal state | `timeline_get {"timeline":"project.json"}` |
| Discover timeline structure | `timeline_schema {"part":"timeline"}` |
| Discover one typed operation | `timeline_schema {"part":"edit_ops","op":"split"}` (the unfiltered part is ~0.8 MB) |
| Discover parameter names, units, defaults, ranges and time bases | `timeline_schema {"part":"params","query":"opacity"}` (unfiltered: ~0.5 MB) |
| Read the compact authoring reference | `timeline_schema {"part":"guide"}` |
| Probe source duration, dimensions, rates and streams | `media_probe {"path":"media/interview.mov"}` |
| Inspect offline media and proxies | `media_status {"timeline":"project.json"}` |
| Plan render keys without decoding or GPU execution | `plan {"timeline":"project.json"}` |
| See frames before encoding any video | `preview_frames {"timeline":"project.json","spread":12}` or `{"at":["5/2"],"each":true}` |
| Look at what an encoded file actually holds | `artifact_frames {"path":"out/cut.mp4","frames":[0,47,-1]}`: decoded pixels with exact pts, not a re-render |
| Inspect edit history / restore the last journaled edit | `log` / `undo`, each with `timeline` |
| Import/export an editable foreign timeline with explicit loss reports | `timeline_import` / `timeline_export`, `format:"otio"` or `"fcp7"`, start with `dry_run:true` |
| Measure rendered picture with numeric video scopes | `scopes_read {"path":"renders/title-draft.mkv","at":"1/2"}` |
| Measure seeded point motion, confidence and failures | `tracking_analyze {"path":"media/shot.mkv","output":"shot.analysis.json","settings":{...}}` |
| Import SRT/WebVTT cues as frame-snapped, gap-free caption clips | `captions_import {"timeline":"project.json","subtitles":"dialogue.srt","style":{...},"dry_run":true}`, then check `caption_timing` |
| Generate ordinary attachment/stabilization edits for review | `tracking_keyframes {"analysis":"shot.analysis.json","timeline":"project.json","options":{...}}` then `edit_apply` |

The same documentation is available through MCP resources: `docs://timeline/guide.md`, `docs://timeline/schema.json`, `docs://timeline/edit-ops.schema.json`, `docs://timeline/params.json`, and `docs://perceive/check.schema.json`. The MCP server's `--list-tools` option prints tool definitions, `--list-docs` the resources and `--doc <uri|name>` one resource (for example `ferrocut-mcp --doc timeline-guide`), without starting a client session.

`docs://integrations/storytold.md` explains the reused engines and their limits;
`docs://capabilities.json` provides their package and connection inventory.
Search the effect catalog narrowly and request details before authoring. Only
connected entries can execute; every unsupported entry explains its missing host
or data requirement. Effect numeric controls accept the usual exact constants,
keyframes and expressions. Shape operators are ordered in
`generator.shape.operators`; edit an existing numeric slot with a path such as
`generator.shape.operators.1.angle`. Creating/removing/reordering operators uses
the whole array, not sparse numeric indices.

Native source-time masks live in `clips.masks`; native nested vector groups and
repeaters use `generator:{"type":"vector_group","group":{...}}`. Edit existing
controls with `masks.0.feather`, `generator.group.repeat.copies` or
`generator.group.items.1.group.repeat.rotation`. Group scale/opacity and repeat
opacity use **percent**; clip scale is a factor and mask opacity is a fraction.
Replace whole item/mask arrays to add, remove or reorder. Numeric indices never
create sparse items. Read `docs://integrations/native-masks.md`,
`docs://integrations/vector-instances.md`, and `docs://integrations/tracking.md`
for strict settings, examples, animation clocks and supported boundaries.

Tracking uses original source pixels and exact source sample times. The planner
checks source path/content identity and emits target clip-local position keys.
Apply these with `edit_apply` to preserve undo and cache behavior. A seed is
supplied by the caller; only later samples have measured confidence. Lost points
stay failed. Translation stabilization exposes borders without automatic crop
or fill. Nonlinear source retiming and planar/3D tracking are not connected.
Run `python3 scripts/motion-showcase.py --output-dir /tmp/ferrocut-motion-demo`
from a built checkout for a complete native render, analysis, attachment,
stabilization, style revision, cached repeat and exact-byte undo workflow.

From a built checkout:

```sh
./target/debug/ferrocut-mcp --list-tools
./target/debug/ferrocut-mcp --root /absolute/path/to/project
```

The second command is a stdio MCP server. Configure the agent's MCP client to launch it with that command and argument; stdout carries protocol messages. Tool paths are relative to `--root`. Font and media paths *inside a timeline* are relative to that timeline's directory. All assets, including fallback fonts, must be inside the configured root; escaping symlinks and paths are rejected.

`index_media` and `transcript_search` can provide source-time word ranges when a whisper.cpp model/runtime is configured. Check their returned status. `scripts/install-local.py` links an existing checkout's whisper executable and supported models into the installed runtime when present; these are absolute links, not bundled copies or downloads. Explicit `FERROCUT_WHISPER_CLI` and `FERROCUT_WHISPER_MODEL` overrides still take precedence (a path that does not exist is an error naming it; an MCP server reads them when it starts).

When whisper or its model is missing, an `unavailable` transcript gives the current reason, and indexing again after fixing the paths transcribes without deleting `.ferrocut-index/`. A failed or unparsable whisper run is not cached, and one cached by an older build is retried by normal indexing. `transcript_search` and `shots_list` index on demand the same way; only the engine's cached-only read (`IndexOptions::cached_only`) returns the cached result without running transcription (a missing-dependency reason is still refreshed). An empty transcript (no speech) is a result and is cached. `shots_list` reports `unavailable` when its optional detector is absent. A listed tool does not establish that an external model, codec or service is installed and working.

## Preserve exact time and choose the right time base

Times and numeric parameters use exact rationals: an integer, or a string such as `"1/2"`, `"0.75"`, or `"1001/30000"`. A JSON floating-point value such as `0.75` is rejected. At 24 fps, frame 12 is `"1/2"`; at `"30000/1001"` fps, frame 30 is `"1001/1000"`. Ranges use an inclusive start and exclusive end.

| Time base | Used by | Meaning |
| --- | --- | --- |
| Timeline | Clip placement; track/master parameters; camera | Seconds from the composition start |
| Clip-local | Clip transform, opacity, effects, audio controls, speed and time-remap key times | Timeline time minus the clip's `start` |
| Source | Numeric generator properties, including text, shape groups and repeaters; clip masks and markers | Time in the source content after retiming |

Read each parameter's registry entry. For a normal clip, source time is `source_in + clip_local_time`. With constant speed `s`, it is `source_in + s * clip_local_time`. Speed ramps integrate speed; `time_remap` directly supplies absolute source seconds and overrides speed.

`set_keyframes` accepts `timeline_time:true` to convert key locations to the property's time base. Source properties use the clip's actual speed/remapping, including its `source_in`. Conversion maps key locations; interpolation still belongs to the resulting source curve. For precise easing under a nonlinear map, design the source curve explicitly. Multiple timeline keys mapping to the same source time are rejected rather than silently overwritten.

Generator animation stays with its source through a split or an in-trim. A title split at timeline time `"3/2"` should show the same frame on either side of the cut. Moving a clip changes placement; slipping changes the source range; trimming changes its exposed range. Use the corresponding operation instead of trying to reproduce these distinctions with hand-edited timestamps.

Procedural effect phase uses a separate `clock_offset` maintained by split/trim;
effect parameter keys keep their usual clip-local clock. Do not overwrite that
offset to change a numeric effect control. Intrinsic path wiggle follows source
time. See the [integration contract](../integrations/STORYTOLD.md) for examples.

## Build a native title through normal edit operations

There is currently no MCP `create_project` tool. Bootstrap a new project JSON with the filesystem, then use `edit_apply` for revisions. For this example, place explicitly licensed fonts at `assets/fonts/NotoSans-Regular.ttf` and `assets/fonts/NotoSansArabic-Regular.ttf` inside the project. The repository's text test fixtures contain these fonts and their license; preserve the license when copying them.

Save this initial file as `project.json`:

```json
{
  "output": {"width":1280,"height":720,"fps":24,"duration":3,"gop":12},
  "tracks": [
    {"name":"Background","clips":[]},
    {"name":"Titles","clips":[]}
  ]
}
```

Track zero composites at the bottom. Clip IDs and track names must be unique. A generator clip needs an explicit duration and has no generated audio. Clips on one track cannot overlap except where the transition rules permit it; put simultaneous artwork on separate tracks.

Send the following as the arguments to `edit_apply`. This first call is a structural preview and writes nothing:

```json
{
  "timeline":"project.json",
  "dry_run":true,
  "plan":true,
  "return_timeline":true,
  "ops":[
    {"op":"add_clip","track":"Background","id":"background","start":0,"duration":3,
     "generator":{"type":"solid","color":["0.04","0.06","0.1",1]}},
    {"op":"add_clip","track":"Titles","id":"title","start":"1/2","duration":2,
     "generator":{"type":"text","text":{
       "content":"Make better videos",
       "font":"assets/fonts/NotoSans-Regular.ttf",
       "fallback_fonts":["assets/fonts/NotoSansArabic-Regular.ttf"],
       "font_size":72,"line_height":90,"tracking":0,
       "position":[64,200],"box_size":[1152,320],
       "align":"left","vertical_align":"top","wrap":"word_or_glyph",
       "fill":[1,1,1,1],"stroke":{"color":[0,0,0,1],"width":1}
     }}},
    {"op":"set_keyframes","clip":"title","param":"generator.text.position.y",
     "timeline_time":true,"keyframes":[{"t":"1/2","v":200},{"t":1,"v":164,"interp":"hold"}]},
    {"op":"set_param","clip":"title","param":"generator.text.animators","value":[
      {"selector":{"unit":"words","start":{"keyframes":[{"t":0,"v":0},{"t":"3/4","v":3}]},"end":3},
       "opacity":0}
    ]}
  ]
}
```

Inspect the result, affected spans, and render plan. Reuse the same request with `dry_run:false` to apply it atomically and append the edit to the journal. Every operation in the list must validate; if one fails, the project file is unchanged. A preview with `plan:true` opens/hashes assets and compiles the project, but does not render pixels.

The title enters at half a second, rises 36 pixels, and progressively reveals its three words. The word range's numeric keys are source seconds; the position keys were supplied as timeline seconds and converted. Change the wording with `set_param` on `generator.text.content`; use `set_keyframes` on registered numeric properties. `generator.text.animators` and `generator.text.fallback_fonts` are whole-list edits. Do not invent an individual `animators.0.selector.end` edit parameter.

Text is shaped with kerning, ligatures, bidirectional ordering, grapheme-aware ranges and explicit fallback assets. `position` is the paragraph box's top-left; `box_size` controls wrapping and alignment. Text size, tracking, line height and stroke widths are output pixels. `fill` and stroke colors are encoded Rec.709 straight RGBA, converted into the engine's linear ACEScg premultiplied working pixels. Text opacity applies to the combined fill and stroke.

Range selectors use zero-based, half-open `start`/`end` indices. `characters` counts Unicode grapheme clusters, `words` counts Unicode words, and `lines` counts laid-out visual lines. Fractional endpoints weight whole shaping clusters; a ligature or combining sequence is not torn into broken glyph pieces. Range animators can offset position, multiply opacity and apply a fill. They are ordered. Character rotation, scale, text-on-path and rich style runs are not implemented by this interface.

The font file determines the face, weight and style; a family name alone is insufficient. `font_index` selects a face in a collection. There is no automatic system-font discovery. A missing non-whitespace glyph produces a render error identifying its UTF-8 byte range: add an appropriate explicit fallback font or revise the content. Treat that error as a failed preview. Ordinary Latin, Arabic/RTL, combining marks and ligatures have regression coverage; every script and color-font format has not been certified. Bitmap-only color glyphs do not acquire an outline stroke.

Native shape artwork uses `generator:{"type":"shape","shape":...}` with rectangle, ellipse or path geometry, fill/stroke and source-time numeric keys. Discover its exact payload through the schema and parameter registry; do not route simple editable vector artwork through a browser merely to create pixels.

## Gradients, dashed strokes and motion blur

Two gradient schemas coexist. This is not a version mismatch; each is correct in its own place.

- Layer generators `{"type":"linear_gradient"}` and `{"type":"radial_gradient"}` take two colors, `start_color` and `end_color`, plus `start`/`end` (linear) or `center`/`radius` (radial). They have no stops.
- Shape paints, in `generator.shape.fill` or `generator.shape.stroke.paint`, use `"type":"linear_gradient"` or `"type":"radial_gradient"` with `stops`: an array of 2 to 256 `{"offset", "color"}` entries.

Shape strokes accept `dashes`, an even-length array of 2 to 256 on/off lengths in pixels (`dash_offset` is the phase, in pixels). An empty array is solid. Lengths clamp to nonnegative, and an all-zero pattern is invisible, not solid. A pattern whose estimated segment count on the path reaches about one million is rejected when the frame is evaluated (render or preview), not when the edit is validated. `dashes` is not a registered numeric parameter, so replace the whole stroke object with `set_param` on `generator.shape.stroke`; `generator.shape.stroke.dash_offset` is keyable.

```json
{"type":"shape","shape":{
  "geometry":{"type":"rectangle","x":80,"y":80,"width":320,"height":180},
  "fill":null,
  "stroke":{"paint":{"type":"linear_gradient","start":[80,0],"end":[400,0],
    "stops":[{"offset":0,"color":[1,"1/2",0,1]},{"offset":1,"color":[0,"1/2",1,1]}]},
    "width":6,"dashes":[12,6],"dash_offset":0}}}
```

Motion blur is layer transform blur. The timeline's `motion_blur` object (`shutter_angle` default 180, `shutter_phase` default -90, `samples` default 16) and the clip's `motion_blur: true` are both required. Each frame samples the layer's transform, and the camera for 3D layers, across the shutter, then averages the results. The layer's content is still the frame at that time, so motion inside the content (for example an animated path or text effect within the clip) is not blurred. It is not 3D lighting and not depth of field; the 3D layer model has neither.

## Expressions and deterministic animation

An animatable number can contain an expression object. For example, a `set_param` value for `generator.text.position.x` can be:

```json
{"expression":"value + 12 * sin(time * 6.283185)","value":64}
```

Expressions use Rhai syntax. The final expression must be numeric; `1 / 2` uses integer division, so use `1.0 / 2` for a fraction. `time` follows the parameter's time base; `comp_time` is timeline seconds. `value` is the optional underlying constant/keyframe curve. The reference guide lists deterministic noise, `wiggle`, easing, `value_at_time`, looping and parameter-reference functions.

Expressions are sandboxed and baked on the frame grid for validation and content-addressed rendering. They do not execute arbitrary filesystem/network operations. A bad expression reports the owner, parameter and failing time. References such as `layer("title").param("opacity")` are evaluated at the corresponding composition time; dependency cycles are errors. Sub-frame sampling interpolates the baked values rather than evaluating an unrestricted live expression engine.

Source-time expressions follow the actual clip map, including reverse and monotonic ramps/remaps. Current explicit limits are:

- A source expression cannot be combined with expression-driven `speed` or `time_remap`; bake the timing curve first.
- A source expression cannot use a remap that changes direction; provide source keyframes instead.
- If repeated/frozen source time would require different values because `comp_time` or another referenced timeline property changes, compilation rejects the ambiguity. A single source curve cannot encode both values.

These limits concern source expressions. Do not infer that all reverse, freeze or nonmonotonic *keyframed* content is unsupported.

## Preview, verify and revise

Agree on measurable delivery goals from the brief: duration, aspect ratio, frame rate, safe margins, exact wording, edit beats, audio targets and the intended viewer response. Then use this loop:

1. Inspect the current timeline and media. Retrieve relevant schema/parameter entries and probe real assets before cutting beyond their available handles.
2. Apply a small coherent edit batch with `dry_run:true,plan:true`. Read every result and any asset/validation errors.
3. Apply the accepted batch normally, keeping its journal and hashes. Use `diff` for a separate candidate file or `branch` for an alternative direction.
4. Look at the picture with `preview_frames`: stills and a labeled contact sheet rendered straight from the graph (the same pixels a master would hold), returned inline as an image; `each:true` writes full-resolution PNGs for reading small text. Render a short draft for motion and sound. Proxies help media drafts; final renders use original media.
5. Read the render report, run the quality checker when available, and check the brief. Revise through another typed batch. Use `undo` to restore the newest journaled edit when necessary; there is no advertised redo tool.

For the title example, MCP `render` arguments are:

```json
{"timeline":"project.json","output":"renders/title-draft.mkv","cpu":true,"check":true,"timeout_s":120}
```

`cpu:true` selects a software Vulkan adapter, not a browser renderer. It still needs a working software Vulkan implementation. Omit it to use the normal GPU selection. `render` blocks until completion and returns output/report information. Use its returned report path; the default here is `renders/title-draft.report.json`. `report_read` accepts `{"report":"renders/title-draft.report.json","full":true}` for details. `quality_check` accepts `{"render":"renders/title-draft.mkv","timeline":"project.json"}`.

Checker statuses are `pass`, `fail`, `error` and `skipped`. `skipped` means no verdict, commonly because the checker is not installed. Inspect failure ranges and thresholds; an intentional freeze or black beat needs creative review. A checker pass does not establish good pacing, readable typography or narrative quality. Watch the result, listen to the audio and read every displayed line. When automated checks are required, CLI `check --require` turns a missing checker into an error.

Equivalent CLI workflow, with the edit list saved as an array in `ops.json`:

```sh
./target/debug/ferrocut edit project.json ops.json --dry-run --plan --json
./target/debug/ferrocut edit project.json ops.json --plan --json
./target/debug/ferrocut render project.json -o renders/title-draft.mkv --cpu --check
./target/debug/ferrocut check renders/title-draft.mkv --timeline project.json --require
./target/debug/ferrocut log project.json --json
./target/debug/ferrocut undo project.json --json
```

The native master is lossless FFV1 video/PCM audio in MKV. Optional H.264/AAC MP4 delivery needs the configured OpenH264 provider. Inspect `openh264` status; its enable/download action requires an explicit user request under the tool contract. Rendering a native master and editing typography do not require that codec download.

## Captions are editable text clips

The CLI imports plain-text SRT/WebVTT cues through the same atomic edits and journal. Supply a `TextSpec` style JSON with explicit font assets; its `content` is replaced by each cue. Fonts in that style resolve relative to the **style file**, not the caption or timeline file. Primary and fallback fonts are placed by their real file (every `.`, `..` and symlink resolved by the filesystem): a font whose real file is inside the timeline's directory is stored relative to it (`assets/fonts/x.otf`), so the project stays portable when moved; a font elsewhere, including a project symlink pointing outside, is stored as its absolute path, listed in the output's `nonportable_fonts` and noted on stderr. A missing font is an error and nothing is written. A bare style file name (`--style style.json`) is relative to the working directory.

```sh
./target/debug/ferrocut captions import project.json dialogue.vtt --style caption-style.json --track Captions --dry-run
./target/debug/ferrocut captions import project.json dialogue.vtt --style caption-style.json --track Captions
./target/debug/ferrocut captions export project.json --track Captions -o dialogue-edited.srt
```

Cue times are fitted to the output frame grid by default, because interchange times are milliseconds and a cue change often leaves a gap of a few ms that contains one frame time: that frame shows no caption, a one-frame blink. A clip shows on frame n when start <= n/fps < end, so:

- `--snap frames` (default) moves each boundary to the first frame at or after it. No displayed frame changes; boundaries and gaps become whole frames. `--snap none` keeps the times.
- `--close-gaps 1/10` (default) extends a cue to the next cue's start when the gap is at most 0.1 s (inclusive: a 3-frame gap at 30 fps closes, 4 frames stay). Longer pauses are kept as authored. `--close-gaps 0` disables it.
- `--min-duration <s>` (off by default) extends shorter cues into the following gap only: never over the next cue and never adding an output frame (or audio sample). With snapping, the last cue may end at the end of the program's last frame, slightly past an off-grid program end: a 3.01 s program at 30 fps has 91 frames, so the bound is 91/30 s. A cue it cannot reach is a warning.
- `--exact-timing` keeps the subtitle file's times exactly (all three off; the behavior before these options). `--snap none` alone still closes gaps.

A cue that shows no frame at all (for example 1.010-1.020 s at 30 fps) gets one frame and a warning, or is an error when the next cue starts on that frame; cues are never dropped and never overlap. The command prints the edit outcome with a `caption_timing` report: every changed cue (`from`, `to`, displayed `frames`, `reasons`: `snapped`, `one_frame`, `closed_gap`, `extended`), every kept uncaptioned gap with its frames, and warnings (a 1-2 frame gap left open reads as a blink). `captions export` warns about such gaps in the track it exports. The source subtitle file is never changed. MCP `captions_import` does the same with a `timing` object (`{"snap":false,"close_gaps":0}` for exact times) and an inline `style` whose fonts are relative to the timeline.

Only the caption clips are retimed. A plate, highlight or any clip authored separately from the cue times (for example one shape per cue on its own track) keeps its own times and blinks on its own; derive such clips from the imported caption clips (`timeline_get`), not from the subtitle file.

Use `--ops-only` to inspect the generated operations without changing a project. The selected export track must contain text clips only. Rendering burns the text into picture; caption export writes a sidecar. Export rounds exact timings to milliseconds and rejects cues that would collapse to zero length. Existing output files are not overwritten.

Multiline Unicode text, BOM/CRLF and supported entities are handled. Rich caption markup, voice tags, WebVTT regions/positioning and STYLE blocks are rejected rather than silently discarded. Overlapping cues require separate tracks for import. Speech transcription and language translation are separate capabilities; caption file import does not perform either.

## Keep assets and evidence with the project

Keep primary/fallback fonts and source media in the project tree with their licenses and provenance. Record source, version, file hash and license/use permission in a sidecar manifest or project notes; the timeline schema does not define an arbitrary `asset_manifest` field. File content participates in render keys. Changing font bytes invalidates frames that use that font; an already compiled text node retains its font snapshot until the project is compiled again.

Relative fonts survive relocation when the whole directory layout is preserved. Editing to a timeline in another directory resolves asset references to absolute paths so that the output still renders; that output is not automatically a portable archive. Check paths when handing a project to another machine. `media_status` is useful for media, but a full compile/plan and actual render also exercise font resolution and shaping.

Keep verification evidence with each completed feature: exact operation or fixture, expected behavior, test/render outcome, and known limits. Passing unit tests establishes the behavior covered by those tests. Mark a parity item complete only when its acceptance criterion has been demonstrated at the intended scope.

Implementation references: [MCP authoring guide](../../crates/ferrocut-mcp/docs/timeline-guide.md), [tool implementation](../../crates/ferrocut-mcp/src/lib.rs), [parameter registry](../../crates/ferrocut-engine/src/params.rs), [native text implementation](../../crates/ferrocut-engine/src/text.rs), [typography tests](../../crates/ferrocut-engine/tests/text.rs), and [full timeline text regressions](../../crates/ferrocut-engine/tests/native_text_timeline.rs). The latter exercises normal edit/compile/render paths, source-time split/trim/retime, timeline-key conversion, font content keys, relocation, atomic failures and undo.


## Media placement

Media and comp clips use `fit: contain` by default, showing the whole native
picture without stretching. Clip `fit` overrides `output.fit`; `cover` fills and
crops, `none` keeps native pixels, and `stretch` deliberately changes aspect.
Use `set_param` for either field (`null` restores inheritance/default). Probe
source dimensions before setting `transform.anchor` in source pixels. Position
is in output pixels and scale multiplies the fit. Masks and clip effects use
source pixels; generator geometry and track effects use output pixels.

Check `plan.placements`/render reports for exact base fit factors and stretch
warnings. They do not describe transformed bounds. The first placement slice
has no `punch_in` op or preview badges. See the timeline guide's placement section
for old-project migration and the explicit-anchor expression limitation.
