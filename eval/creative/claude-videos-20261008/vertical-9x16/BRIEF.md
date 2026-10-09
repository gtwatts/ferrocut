# Brief: "Sintel" — a 30-second vertical social cut (9:16)

Slug: `vertical-9x16`. Read `../CONTRACT.md` first; it governs everything here.

## Deliver

1080 × 1920 at the source frame rate, exactly 30.000 s, H.264/AAC MP4 plus the
lossless master. Phone-safe: keep text out of the top 220 px and bottom 340 px
and 80 px from the sides. Loudness −14 LUFS integrated, true peak ≤ −1 dBTP.

## Idea

A character spotlight on Sintel, cut from `eval/media/clips/s1..s6.mkv`
(Blender Foundation, CC-BY 3.0), made for a phone feed: it must earn the first
two seconds, read without sound, and reward sound. Probe the clips, pull stills,
find one or two spoken lines with `ferrocut index <clip> --search`, then write
the treatment.

Reframing the 16:9 source to 9:16 is the craft problem: use keyframed scale and
position (punch-ins that follow the action), one masked split-screen or
stacked-frames moment, and a tracked label that follows a character
(`ferrocut tracking analyze` on the source, `ferrocut tracking keyframes` to
get ordinary keyframes, applied with `ferrocut edit`).

## Design

Bold, high-contrast typography (Fira Sans Heavy / Noto Sans Bold) with a real
hierarchy; kinetic captions of the spoken line(s) (word-timed from the real
audio), a title moment on a freeze frame after a speed ramp, a consistent
accent color, a plate or stroke wherever text sits on picture. Motion:
eased, snappy (150–300 ms), nothing linear.

## Audio

Source dialogue/ambience from the clips, an original procedural music bed with
a hook in the first bars, ducked under speech. Hard-stop or quick fade at the
end.

## Must show in the edit

Keyframed reframing, a mask, a tracked attachment, a speed ramp and freeze
frame, captions as native text clips, a split-screen or stacked composition,
audio ducking, the CC-BY attribution (small, within safe margins, on the last
seconds).
