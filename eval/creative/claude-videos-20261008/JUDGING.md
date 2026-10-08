# Judging: how the four videos are scored

Each video is judged by two independent agents with different lenses, then a
synthesizer ranks the set and turns the producers' friction logs into an
engineering backlog. Judges never talk to producers and never edit anything.

## Evidence a judge must gather

A judge cannot play video or listen, so it samples densely and measures:

- The producer's `TREATMENT.md`, `REVISIONS.md`, `REPORT.md`, `SOURCES.md`,
  `FRICTION.md`, `delivery/check.json`, `delivery/master.report.json`, and the
  brief it was given.
- Its own stills from the final `project.json`, rendered on the CPU adapter:
  `ferrocut stills project.json -o <judge dir>/spread --spread 48 --cols 6 --cell-width 320`
  for pacing and continuity, plus `--at <t> --each` around every cut and every
  text moment (read `ferrocut log` and the timeline's clip starts to find them).
  Read full-resolution frames to judge typography.
- Measurements on `delivery/<slug>.mp4`: `ffprobe -show_streams -show_format`
  (duration, frames, size, rate), `ffmpeg -i ... -af ebur128=peak=true -f null -`
  (integrated loudness, true peak, loudness range), and
  `ferrocut index delivery/<slug>.mp4 --json` to transcribe the narration and
  compare it with the script and captions.

Write the stills into the judge's own directory under
`out/claude-videos-20261008/judging/<slug>/<lens>/`, never into the producer's.

## Criteria (score each 1–10)

| Criterion | 9–10 | 5–6 | 1–2 |
|---|---|---|---|
| Concept and story | a clear idea, every shot earns its place, a real ending | coherent but generic; filler | no idea; a slideshow |
| Typography and layout | real hierarchy and scale, consistent margins, text always legible | readable but inconsistent sizes/margins; text fights the background | clipped, tiny, unreadable |
| Motion craft | eased, timed to meaning, restrained, nothing dead on screen | some linear or floaty moves; static holds | jerky, wrong, or no motion |
| Editing and pacing | cuts on beats/phrases, J/L cuts, transitions with intent | acceptable rhythm, some awkward cuts | flash frames, gaps, random cuts |
| Audio | intelligible voice over ducked music, levels on target, sound design | on target but flat; ducking weak | clipping, off-target, missing |
| Technical compliance | exact spec, checker pass, journaled, portable | minor misses explained | wrong duration/rate, broken files |
| Professional feel | a brand or broadcaster would run it | competent, visibly machine-made | would be rejected on sight |

Calibration: 9–10 is agency or broadcast quality; 7–8 is strong social
content; 5–6 competent with amateur tells; 3–4 rough; 1–2 broken. Be hard:
the point is to find what Ferrocut and its agents cannot do yet.

## Lenses

- **Art director**: concept, typography/layout, motion craft, professional
  feel. Reads every text frame at full resolution.
- **Post supervisor**: editing/pacing, audio, technical compliance, and
  whether the producer's own report is honest (compare its claims with your
  measurements).

Both report: scores with one-line justifications, a defect list (timecode,
still path, what is wrong, how serious), three strengths, and the single
change that would most improve the piece.

## Synthesis

The synthesizer averages the lenses, ranks the four videos, writes the human
summary (what to watch for in each, in order), and distills every
`FRICTION.md` into a ranked engineering backlog: missing features, bad error
messages, documentation gaps, performance, with the producer's exact evidence
and an estimate of how many producers hit each item.
