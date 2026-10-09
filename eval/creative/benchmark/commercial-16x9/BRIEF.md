# Brief: "Vessa Field Lantern": a 15-second product spot (16:9)

Slug: `commercial-16x9`. Read `../../claude-videos-20261008/CONTRACT.md` first (relative to `eval/creative/benchmark/commercial-16x9/`). It governs everything here. Vessa is a **fictional**
product and company invented for this benchmark. It must not resemble a real brand, logo or domain. Use
`vessa.example` (an RFC 2606 reserved domain) for the call to action.

## Deliver

1920 × 1080, 24 fps, exactly 15.000 s (360 frames), H.264/AAC MP4 plus the lossless master. Loudness
−16 LUFS integrated (16:9 web), true peak ≤ −1 dBTP. Optional B deliverable: a 1080 × 1080 cut-down of the
same timeline, built from the same project, not rebuilt by hand. Record every step it took.

## Idea

Darkness, a click, light. A cold cave plate holds for a beat, the lantern switches on, and its light becomes
the graphic language of the spot: warm rays, a glow that the type sits in, and callout lines that follow
the product. Three claims, one per beat, then the product hero and the end card.

| Time | Beat |
|---|---|
| 0.00–2.50 | Cold open: the empty crystal cave (`eval/media/clips/s6.mkv`, source 0–1.5 s, slowed to 40 %; no character in frame, though a small creature crosses low). Graded cold and dark, slow push-in. Room tone only. |
| 2.50–3.75 | The switch: a click on the frame, the lantern (native vector art) fades up in the center, and a warm bloom spreads over the plate. |
| 3.75–11.25 | Three claims, 2.5 s each (4 beats at 96 bpm), on the beat: "14 hours of light", "Storm-proof to IP67", "One-hand dimmer". Each claim has a callout line anchored to the product part it names (the battery, the seal, the dial) and moves with the product as it turns. |
| 11.25–13.125 | Hero: the product on a warm gradient, light rays turning slowly behind it, the name "VESSA" tracking in. |
| 13.125–15.00 | End card: wordmark, "Field Lantern", `vessa.example`, and a small attribution line "Background footage: Sintel, (CC) Blender Foundation". Music button on 13.125 s, tail to 15.0. |

## Design

- Palette (Rec.709): night #0B1220, ember #FF9A3C, warm white #FFF1DC, steel #8A97A8.
- Type: Fira Sans Heavy for claims, SemiBold for supers, with a real type scale (claims 96 px, supers 40 px,
  legal 22 px). Margins at 96 px, title-safe at 90 %.
- Motion: 300–500 ms ease_out entrances, callout lines drawn on (trim paths), nothing static for more
  than 2 s, and one deliberate overshoot on the product reveal.
- The product is native vector art (`vector_group` / `shape`), so it stays editable. No bitmap product shot.

## Audio

An original procedural score, 96 bpm, warm pads and a soft pulse, entering at the switch (2.5 s), hits on the
claim downbeats and a button at 13.125 s. The switch click and a faint electrical hum are synthesized SFX.
Optional single voice-over tagline in the last 3 s, with local Qwen3-TTS only (no paid voices). If used, duck
the music by at least 8 dB under it.

## Must show in the edit

Footage placed without distortion (2.35:1 source filling 16:9 by cover, then a push-in). A speed change on
footage. A grade on an adjustment layer. Additive or screen glow compositing over footage with a mask. Native
vector product art with animated parts. Callout lines that follow the product (expressions or keyframes tied to
it). Kinetic claims with animators. Markers on the music beats. An end card with a legible attribution line.
Duration and frame rate must be exact.

## What this benchmark is for

It covers the commercial class in the four-class matrix (`../BENCHMARK.md`). Its measured weak points are the
targets: placement (fit/cover), compositing (glow over footage), callouts locked to animated vector parts,
supers that must not blink, and a multi-aspect cut-down. Record each workaround step in `FRICTION.md` so the
count can be compared after engine fixes.
