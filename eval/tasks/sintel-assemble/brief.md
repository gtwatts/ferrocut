# Assemble three Sintel shots

`media/` holds three clips from *Sintel*: `s1.mkv`, `s2.mkv` and `s3.mkv`. Each is 6 s of picture and sound.
`timeline.json` is an empty project.

Build a 12-second sequence on track `V1`:

1. `s1` from the start of the clip, 4 s long;
2. then `s2`, starting 1 s into the clip, 4 s long;
3. then `s3` from the start of the clip, 4 s long.

The shots play back to back from 0 with no gaps, and each keeps its own sound. Render the result to `out.mkv`.
