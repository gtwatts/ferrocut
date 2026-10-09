# Brief: "One timeline" — a 45-second editorial explainer (16:9)

Slug: `explainer-16x9`. Read `../CONTRACT.md` first; it governs everything here.

## Deliver

1920 × 1080, 30 fps, exactly 45.000 s (1,350 frames), H.264/AAC MP4 plus the
lossless master. Narrated, with burned-in captions. Loudness −16 LUFS
integrated, true peak ≤ −1 dBTP.

## Idea

An editorial explainer, in the spirit of the best explainer channels, about
one thing: why an AI agent that makes videos needs a real editor, and what
"real" means. You write the script (120–140 words; narration capped at 1,200
characters). Do not use anyone's branding. Ferrocut may be named once, at the
end; the piece is about the idea, not a product pitch.

Suggested arc (change it if you have a better one):

1. Hook (0–6 s): a question the viewer recognizes. Today an agent makes a video
   by gluing things: a browser renders titles, a script concatenates clips, a
   command line mixes sound. Show the glue, visually.
2. The problem (6–18 s): nothing is on one clock. A title that lands a frame
   late; narration that drifts from its caption; a change that means
   re-rendering everything. Show it with motion, not with a wall of text.
3. The turn (18–34 s): one timeline. Tracks as layers, clips as bars, exact
   time, editable text and shapes, keyframes, audio ducking under a voice,
   effects. Build the diagram live: bars slide in on beats, a keyframe curve
   draws itself, a caption snaps to the waveform. Everything you draw here must
   be native Ferrocut artwork (text, shapes, vector groups) driven by keyframes
   and expressions.
4. The loop (34–41 s): edit, look, revise; only what changed re-renders.
5. End card (41–45 s): the takeaway in one line, then a quiet logotype-free
   sign-off.

## Design

Dark editorial palette (ink/navy background, one warm accent, one cool accent,
near-white text), Fira Sans at a real type scale (display ≥ 72 px, body ≥ 40 px,
caption ≥ 36 px), 96 px safe margins. Motion: 250–450 ms eased moves, staggered
reveals (text animators by word), no linear slides, no bounce unless it says
something. Captions: 2-line max, phrase-timed from the real narration, in a
consistent lower-third zone with a subtle plate.

## Audio

Narration (ElevenLabs preferred; verified available in this workspace on
2026-10-08: Matilda `XrExE9yKIg1WjnnlVkGX` (professional, informative), River
`SAz9YHcvj6GT2YYXdXww` (neutral, relaxed), Alice `Xb7hH8MSUJpSbSDYk0k2` (clear
educator, British), George `JBFqnCBsd6RMkjVDRZzb` (warm storyteller, British);
pick one, record the voice ID and model in `SOURCES.md`), an original music bed that stays out of the voice's way (sidechain
duck in Ferrocut, ≥ 8 dB), two or three sound-design accents on key moments.
Fade in and out.

## Must show in the edit

At least: text animators (word reveal), a keyframed vector group, an expression
(a wiggle or a loop), one effect on an adjustment layer, a keyframed camera or
transform move, audio ducking, captions imported with `ferrocut captions`.
