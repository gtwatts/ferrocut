# Brief: "Tears of Steel" — a 40-second trailer cut (16:9)

Slug: `trailer-16x9`. Read `../CONTRACT.md` first; it governs everything here.

## Deliver

1920 × 1080 (upscaled from the 720p source; say so in the report) or
1280 × 720 if you judge the upscale too soft, at the source frame rate, exactly
40.000 s, H.264/AAC MP4 plus the lossless master. No narration. Loudness
−16 LUFS integrated, true peak ≤ −1 dBTP.

## Idea

A cinematic trailer for *Tears of Steel* (Blender Foundation, CC-BY 3.0) cut
from `eval/media/clips/t1..t6.mkv` and `d1.mkv` only. Probe and watch the
material first (stills of every clip, `ferrocut index` with `--search` on
`d1.mkv` to find usable dialogue lines). Then write a treatment: the emotional
shape, three movements, the title moment, the button.

Trailer grammar to use deliberately:

- A cold open on a quiet shot, source sound up, then a title card.
- Movement one: slow, wide, dialogue line as a J-cut (sound before picture).
- Movement two: escalation to the music; the cut rate rises; one speed ramp
  (fast to slow, pitch preserved sound) on a key action moment; a dip-to-black
  or a hard cut on the downbeat.
- Movement three: the title typography (tracking-in letters, a plate, a
  subtle glow), a last shot, the attribution card.

## Design

A grade applied consistently (lift/gamma/gain or curves, a vignette, light
grain) on an adjustment layer; letterbox to 2.39:1 if it helps the look.
Typography: Fira Sans Heavy or SemiBold, tracked wide, with a deliberate type
scale and 96 px margins. Transitions: dissolves and dips with intent; at most
one stylized wipe.

## Audio

An original procedural score with a clear tempo that the cuts follow: a low
pulse, a rising pad, hits on the downbeats of movement two, a tail under the
title. Source dialogue and ambience from the clips (J/L cuts, fades), ducked
under the hits where needed. Place markers on the beats and cut to them.

## Must show in the edit

Shot selection from probed/indexed material, J-cut from source audio, a speed
ramp, dissolve and dip-to-black transitions, a graded adjustment layer, native
title typography with animators, timeline markers on musical beats, and the
CC-BY attribution end card.
