# Speed ramp into slow motion

`timeline.json` has one *Sintel* shot, clip `a` on track `V1`, playing its first 5 seconds of source at normal speed.

Turn it into a **speed ramp**:

- 0 s to 2 s: normal speed (100%).
- 2 s to 3 s: the speed falls **linearly** from 100% to 50%.
- From 3 s on: 50% slow motion, until the clip has shown the source up to exactly **5 s** (the same last source
  frame as now). The clip, and the sequence, get longer accordingly; nothing else changes.

The clip's sound must follow the picture's timing but keep its original **pitch** (no chipmunk/slow-tape effect).
Render the result to `out.mkv`.
