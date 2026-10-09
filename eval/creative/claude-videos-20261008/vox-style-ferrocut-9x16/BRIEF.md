# Brief: "FerroCut, explained" — a 60-second Vox-style social explainer (9:16)

Slug: `vox-style-ferrocut-9x16`. Read `../CONTRACT.md` first; it governs everything here.

## Deliver

1080 × 1920, 30 fps, exactly 60.000 s (1,800 frames), H.264/AAC MP4 plus the
lossless master. Narrated, with large burned-in captions (the piece must work
muted). Phone-safe: no text in the top 220 px or bottom 340 px, 80 px side
margins. Loudness −14 LUFS integrated, true peak ≤ −1 dBTP.

## Idea

An explainer *about this project*, FerroCut, in the editorial style of the
best explainer channels (think Vox): a sharp question up top, a narrator who
reasons out loud in plain language, evidence on screen (diagrams that build,
documents and code that get highlighted and underlined as the narrator reads
them), big flat colors, confident typography, and a point at the end. Do not
use Vox branding, their logo, their music or their name on screen; the style
is the reference, not the brand.

The facts come from the repository: `README.md`, `docs/parity/README.md`,
`docs/parity/AGENT_GUIDE.md`, `docs/evaluations/`, `CONTRIBUTING.md`, and the
live CLI (`ferrocut --help`, `ferrocut capabilities`, `ferrocut effects`).
Every claim in the script must be true of the current checkout. Say what it
is (a headless, agent-native video editing and compositing engine in Rust,
Apache-2.0, early and open), not what it might become; "not Adobe parity" is
part of the honesty of the piece. Name the GitHub repository
(`github.com/gtwatts/ferrocut`) on the end card.

Suggested arc (improve it if you can):

1. Hook (0–7 s): "AI agents can write a video. They can't *edit* one." Show
   the glue agents use today (a browser rendering titles, a script stitching
   clips, a command line mixing audio) as three disconnected boxes.
2. The problem (7–20 s): nothing shares a clock; a change means re-rendering
   everything; the agent never sees its own frames. Underline the words as
   they are spoken.
3. The idea (20–40 s): FerroCut is one timeline for all of it: tracks are
   layers, clips are exact rational time, titles and shapes are editable
   artwork, audio ducks under the voice, effects stack, every edit is a
   journaled operation with undo and branches, and only the chunks that
   changed re-render. Build the diagram live; show one real edit op as code
   (a `split` or `set_keyframes` JSON), typed on screen and highlighted.
4. The loop (40–52 s): edit, look (stills and contact sheets straight from
   the engine), revise; the agent's eyes. Show a real contact sheet: render
   one from `examples/demo-av.json` or your own project with `ferrocut stills`
   and use the PNG as evidence on screen (a PNG is a usable clip source if
   `ferrocut probe` accepts it; otherwise reproduce its look natively).
5. Status and button (52–60 s): early, open source, built for agents; the
   repository on the end card; sign off.

## Design

Flat editorial palette: an off-white or paper background with ink text and
one or two saturated accents (a yellow highlight for underlines and
emphasis, one cool accent), or the inverse at night. Fira Sans or Open Sans
at a bold type scale (display ≥ 96 px, body ≥ 48 px, captions ≥ 44 px),
generous margins. Motion: 200–400 ms eased moves, highlights that sweep in
under words as they are spoken, diagrams whose parts arrive in reading order,
cutaways that slide in as cards. Captions: phrase-timed from the real
narration, two lines max, in a fixed caption zone with a plate.

## Audio

Narration: ElevenLabs preferred (verified voice IDs on 2026-10-08: Matilda
`XrExE9yKIg1WjnnlVkGX`, River `SAz9YHcvj6GT2YYXdXww`, Alice
`Xb7hH8MSUJpSbSDYk0k2`, George `JBFqnCBsd6RMkjVDRZzb`; cap 1,200 characters,
record the voice ID, model and character count in `SOURCES.md`). Script
130–150 words at an explainer pace. An original music bed with a light pulse,
ducked ≥ 8 dB under the voice, two or three sound-design accents for
highlights and card arrivals. Fade out.

## Must show in the edit

Text animators (word reveals and an underline/highlight sweep built from
shapes keyed to the narration), a vector-group diagram that builds over time,
one real edit op shown as code, a real contact sheet or stills as evidence, an
adjustment-layer effect, audio ducking, captions imported with
`ferrocut captions`, and the repository end card.
