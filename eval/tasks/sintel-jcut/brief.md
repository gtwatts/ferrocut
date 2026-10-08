# Turn a straight cut into a J-cut

`timeline.json` has two *Sintel* shots on track `V1`, each with its own sound, and a straight cut at 5 s.

Make that cut a **J-cut**. The second shot's sound should start **1 second before** its picture, replacing the
first shot's sound from 4 s. The picture cut stays at exactly 5 s, and the sequence stays 10 s long. Render the
result to `out.mkv`.
