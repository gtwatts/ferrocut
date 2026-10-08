# Nest two shots and screen them over a background

`timeline.json` has a *Tears of Steel* background shot on `V1` (clip `bg`, 0 to 6 s) and two shots cut back to
back on `V2` above it (clips `a` and `b`, 0 to 3 s and 3 to 6 s), which currently hide the background.

1. Nest `a` and `b` into a new composition file **`comps/overlay.json`** (a nested sequence / precomp). On the
   main timeline, `V2` must then hold a single clip with id **`overlay`** that plays that composition from 0 to 6 s.
   Inside the composition the two shots keep their timing and source ranges.
2. Composite the `overlay` clip over the background with the **Screen** blend mode (full opacity).

Leave `bg` as it is. Render the result to `out.mkv`.
