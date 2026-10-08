# Production contract: Claude agents making videos with Ferrocut (2026-10-08)

You are one producer agent. You make one finished video with Ferrocut, by
yourself, from the brief in your task directory. The point of the exercise is
twofold: a video a demanding human will judge, and an honest record of every
place Ferrocut got in your way. Both are deliverables.

## Environment

- Checkout: `/home/gordontwatts/Documents/projects/ferrocut` (release build is
  current). Binaries: `target/release/ferrocut` (CLI: edit, stills, render,
  check, plan, diff, log, undo, branch, probe, index, effects, captions,
  tracking, proxy, scopes, interchange, capabilities) and
  `target/release/ferrocut-mcp` (`--list-tools`, `--list-docs`,
  `--doc timeline-guide` for the authoring guide, `--doc <uri>` for the
  timeline JSON Schema, edit-op schema, parameter registry, integration docs).
  Use the CLI; the session's `ferrocut` MCP server runs an older installed
  build and must not be used for this task.
- Read first: `docs/parity/AGENT_GUIDE.md`, then `ferrocut-mcp --doc timeline-guide`
  (27 KB; read it whole). The schema documents are large (`docs://timeline/schema.json`
  and `docs://timeline/edit-ops.schema.json` about 840 KB each, `params.json`
  540 KB): never read them whole; pipe `--doc` through `grep -n` or a short
  Python snippet to pull the one op, clip field or parameter you need. For
  effect controls use `ferrocut effects --query <word> --details` (limit ≤ 100
  per page). Discover exact payloads this way instead of guessing. `examples/*.json` and
  `examples/*.ops` are working references for native text, shapes, masks,
  vector groups, effects and tracking.
- Work only inside your task directory `out/claude-videos-20261008/<slug>/`
  (gitignored). Create it. Every asset the timeline references lives under it,
  except footage, which you reference in place under `eval/media/clips/` by a
  relative path from the timeline file (e.g. `../../../eval/media/clips/t1.mkv`).
  Never copy the eval media; never write outside your directory.
- The GPU is mostly occupied by another standing service (about 2 GB of VRAM
  is free) and is shared with three other producers. Make stills and draft
  renders on the software adapter (`--cpu`; same compositor, perceptually
  identical output) and keep drafts short (`output.duration` on a trimmed copy,
  or `--proxies` after `ferrocut proxy`). Render the final master on the GPU
  with `-j 2`; the engine backs off on out-of-memory, and if it still fails,
  render the final with `--cpu` and say so in the report.
- Python: `/home/gordontwatts/miniconda3/envs/qwen3-tts/bin/python` has numpy,
  scipy, soundfile (and Qwen3-TTS). The system `python3` has none of them.
- FFmpeg for audio preparation and delivery transcodes only:
  `~/.local/bin/ffmpeg` (libx264, aac, ebur128, loudnorm). Ferrocut composes all
  picture; an FFmpeg filtergraph, HTML, Remotion or any other compositor is out
  of scope for the picture.

## Assets you may use

- Footage: `eval/media/clips/{t1..t6,d1}.mkv` (Tears of Steel, 2012) and
  `{s1..s6}.mkv` (Sintel, 2010), both Blender Foundation, CC-BY 3.0. Probe each
  clip before cutting (`ferrocut probe`). Attribution is mandatory: an end card
  ("Footage: Tears of Steel / Sintel, (CC) Blender Foundation") and an entry in
  `SOURCES.md`. `ferrocut index <clip> --search "<words>"` finds spoken lines
  (whisper.cpp, small model, built in `third_party/`).
- Fonts: copy the files you use into `assets/fonts/` and record the license.
  Freely licensed on this machine: Fira Sans (OFL 1.1,
  `/usr/share/fonts/opentype/fira/`: Book, Regular, Medium, SemiBold, Bold,
  Heavy and italics), Noto Sans (OFL, `/usr/share/fonts/truetype/noto/`,
  Regular/Bold), Open Sans (`/usr/share/fonts/truetype/open-sans/`), DejaVu
  (`/usr/share/fonts/truetype/dejavu/`), Roboto (Apache 2.0, `~/.fonts/`).
  Copy `/usr/share/doc/fonts-*/copyright` when it exists. Ferrocut needs an
  explicit font file per weight; there is no family lookup.
- Narration, two routes. (a) ElevenLabs through the native command runner:
  `N=/home/gordontwatts/.claude/local-marketplaces/gordon-from-codex/plugins/gordon-skills/scripts/native-domain.mjs`;
  `node $N schema elevenlabs_list_voices`, then `node $N run <command> --input params.json --project <your dir>`
  for `elevenlabs_list_voices`, `elevenlabs_estimate_timing`,
  `elevenlabs_generate_voiceover` / `elevenlabs_generate_scene_pack`. Paid:
  the whole production is capped at **1,200 characters per video**; generate
  each line once, keep every original file and the receipt the runner returns,
  and log the character count in `REPORT.md`. Never print credentials.
  (b) Local Qwen3-TTS on the CPU (free, slower): adapt
  `out/codex-60s-20261008-astra/delivery/authoring/generate_voice.py` (copy it;
  do not edit the original). Either way, measure real durations with ffprobe,
  and get word timings for captions from the real audio with
  `ferrocut index narration.wav --json`.
- Music and effects: original procedural synthesis in Python (write 48 kHz
  WAV). Make it musical: a tempo, a key, chords, layered detuned oscillators
  through filters and an envelope, a simple drum pattern if the piece wants
  one, a convolution tail for space. Ferrocut mixes it (bus gain, keyframed
  automation, sidechain `duck` under narration, loudness target). No downloads,
  no third-party music.
- Still images: Ferrocut's native generators (text, shape, vector_group,
  solid, gradients) and effects make the graphics. A PNG you synthesize may be
  used as a clip source only if `ferrocut probe` reports it usable; prefer
  native, editable artwork.

## Process

1. Treatment first: concept, the viewer's takeaway, a timed script/shot list
   with timecodes, and a design system (palette as exact Rec.709 values, type
   scale, safe margins, motion language with durations and easing). Write
   `TREATMENT.md` before touching the timeline.
2. Produce narration (if any) and measure it; the picture follows the audio.
3. Bootstrap `project.json` by hand (output settings, empty tracks), then build
   everything with `ferrocut edit project.json ops.json --plan --json` in small
   batches (`--dry-run` first when unsure). Keep every ops file in
   `authoring/` numbered in order; the journal (`ferrocut log`) must show the
   whole build. Do not hand-edit `project.json` after the bootstrap.
4. After every batch: `ferrocut stills project.json -o stills --spread 12`
   (plus `--at <time>` for specific moments, `--each` for full-resolution
   frames) and LOOK at the PNGs with your image-reading tool. Fix what you see.
   Three revision rounds are the minimum; write the reason for each revision
   in `REVISIONS.md` with the before/after still paths.
5. Render drafts of the parts you cannot judge from stills (motion, transitions,
   audio) and check them: `ferrocut render project.json -o drafts/d1.mkv --cpu -j 6 --check`,
   `ffmpeg -i drafts/d1.mkv -af ebur128=peak=true -f null -` for loudness.
6. Final: `ferrocut render project.json -o delivery/master.mkv -j 2 --check`,
   then `ferrocut check delivery/master.mkv --timeline project.json --require`
   (keep the JSON), then the delivery MP4:
   `ffmpeg -i delivery/master.mkv -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -c:a aac -b:a 192k -movflags +faststart delivery/<slug>.mp4`.
   Verify with ffprobe: exact duration, frame count, dimensions, frame rate;
   with ebur128: integrated loudness and true peak. Make a final
   `ferrocut stills ... --spread 16 --each` set and stills ±1 frame around every
   cut; look at all of them.

## Quality bar (what the judges score)

- Concept and story: a clear idea, a reason for every shot, an ending.
- Typography and layout: hierarchy, a real type scale, consistent margins
  (≥ 5 % of the frame; phone-safe zones for vertical), text never on a busy
  background without a plate, no clipped or orphaned text.
- Motion craft: eased moves (ease_in_out, bezier, speed), 200–600 ms for UI-like
  moves, overshoot only on purpose, nothing static for > 2 s without intent,
  transitions that mean something.
- Editing and pacing: cuts on beats or phrases, J/L cuts where sound leads,
  no flash frames, no black gaps unless designed.
- Audio: narration intelligible over music (duck ≥ 8 dB), −14 LUFS integrated
  for social delivery (−16 for 16:9 web is also fine; state which), true peak
  ≤ −1 dBTP, fades at the ends, sound design that supports the picture.
- Technical: exact duration and frame rate from the brief, checker `pass` with
  explained warnings, journaled build, portable project (relative paths).
- Craft signals of "professional": consistent palette, restraint, breathing
  room, details at cut points, an end card.

## Deliverables (all inside your task directory)

- `project.json` + `.ferrocut/` journal and snapshots; `authoring/*.ops.json` in
  order; `assets/` (fonts, audio, licenses).
- `delivery/master.mkv`, `delivery/<slug>.mp4`, `delivery/master.report.json`,
  `delivery/check.json`, `delivery/sheet.png` (final contact sheet),
  `delivery/stills/` (final per-frame stills), `delivery/captions.srt` if captions.
- `TREATMENT.md`, `REVISIONS.md`, `SOURCES.md` (every asset, origin, license,
  narration provider receipts and character counts), `REPORT.md` (verification
  numbers, what is good, what is weak, what you would do next, time spent),
  and `FRICTION.md`.

`FRICTION.md` is the engineering input. One entry per problem: what you were
trying to do, the exact command or op, the exact error or wrong result, how
long it cost you, your workaround, and the fix you would want (new op, better
error message, missing doc, missing feature, performance). Add a wishlist of
features you reached for and did not find. Be specific and blunt; praise is
not needed there.

## Boundaries

No git operations, no installs, no global configuration changes, no edits to
repository source or docs, no other producer's directory, no publication, no
messages to anyone. If something blocks you, record it in `FRICTION.md`, work
around it if you honestly can, and still finish a complete delivery. Finish
within about two hours of work; a complete, candidly reported video beats an
unfinished perfect one.
