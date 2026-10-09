# Brief: "Say it with motion" — a 24-second kinetic typography piece (1:1)

Slug: `kinetic-type-1x1`. Read `../CONTRACT.md` first; it governs everything here.

## Deliver

1080 × 1080, 30 fps, exactly 24.000 s (720 frames), H.264/AAC MP4 plus the
lossless master. Music-driven, no narration. Loudness −14 LUFS integrated, true
peak ≤ −1 dBTP.

## Idea

Pure motion design: an original six-to-eight-line text (your own words; a short
manifesto about making, looking, revising; no brands, no claims about any
product) set to an original piece of music with a strong tempo, where every
line arrives, transforms and leaves in a way that means something. The piece
should feel like one composition, not a slideshow: a camera that moves through
stacked layers, shapes that respond to the words, a palette that evolves.

## Design

Choose a type system (one display face, maybe one secondary), a four-color
palette that shifts across the piece, and a motion language: per-character or
per-word text animators, position/scale/rotation keyframes with real easing
(bezier or AE speed), expressions for secondary motion (`wiggle`, `loop_out`,
a value driven by another layer), vector groups with repeaters for pattern,
shape operators (trim paths, round corners, zigzag, twist) for line drawing, a
blend mode somewhere it matters, an adjustment layer with glow or blur, a 3D
camera dolly across layers with `three_d`. Place timeline markers on beats and
make cuts and keyframes land on them.

## Audio

An original procedural track with an audible grid (kick/pulse at a fixed BPM,
a bass line, a pad, a few accents). Keyframe the master or bus gain if the
piece needs dynamics. Clean fade-out.

## Must show in the edit

Text animators by characters and by words, vector group with a repeater, at
least two shape operators, two expressions, a blend mode, an adjustment-layer
effect, a camera move over 3D layers, markers on beats, nested composition for
a repeated element.
