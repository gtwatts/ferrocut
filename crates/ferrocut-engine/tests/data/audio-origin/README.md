# Public audio-origin fixture

These four short source files and five interleaved stereo f32le references are
original synthetic test material, licensed Apache-2.0. No client media is used.
`fixture.json` records exact SHA-256/length bindings, recipe hashes and observed
packet/frame metadata. It was copied without codec execution by the reviewed
`scripts/import-audio-origin-fixture.py` from the independently accepted
2026-10-09 public baseline experiment. Its `measurements_only` status describes
the baseline measurements, not a repaired-engine verdict.

The original signal is two seconds of 44.1 kHz stereo signed-16 PCM. The recipe
uses LCG seed `0x5EED2026`, multiplier 1664525 and increment 1013904223 modulo
2^32. Distinct stereo noise bursts of 2205 samples start at 6615, 17640, 35280,
48510, 66150 and 81585; the first 150 ms is authored silence. In every sample,
`state = (1664525 * state + 1013904223) mod 2^32`. Within a burst, left is
`((state >> 16) mod 12001) - 6000` and right is
`((state >> 3) mod 10001) - 5000`; outside bursts both are zero.

The already installed LAME 4.0 encoded `--noreplaygain --cbr -b 128 -m s`, with
`-T` for tagged and `-t` for untagged. It reports GNU Library GPL v2-or-later;
its decoder was never used. FLAC encoding and every reference/probe decode used
the existing dynamically linked LGPL-only FFmpeg 9.0.2. Each MP3 is compared
with its own decode, never the original WAV as a lossy-sample oracle. The
reference command selected the first audio stream, `pcm_f32le`, stereo, and the
named output rate, with one thread. No normalization or gain adjustment occurred.

Observed tagged priming is 1105 samples and trailing discard is 551 at 44.1 kHz.
The untagged source has neither skip nor discard metadata and intentionally
retains its own encoder padding. These are fixture facts, not global codec
constants. The paired C probe differs only in decoder packet-timebase setup;
decoded plane bytes/counts are identical. Packet and reference provenance is
retained in the manifest. The reviewed original run manifest is
`77e8bb4aabee897f07084a870655369e61a443dc7a4ebc12124bb2e19b8f44ed`.

`cargo test --locked -p ferrocut-engine --test audio_origin` runs active tests
without an environment override, optional external command, network or GPU.
Missing or truncated fixture files fail. Tests also build a tiny FFV1/PCM linked
source through the required engine libraries to exercise a real 100 ms A/V
offset; an unavailable required codec fails instead of returning a successful
skip. Source-only preparation is not a test result; see
`docs/evaluations/2026-10-09-audio-origin-regression.md` for validation status.
