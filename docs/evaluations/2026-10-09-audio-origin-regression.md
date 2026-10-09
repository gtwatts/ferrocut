# Audio-origin regression preparation

Status: **source-only, not compiled, formatted, imported or executed**. There are
no new fixture bytes or baseline/candidate test results. The production decoder
and audio cache version are unchanged. Four explicitly ignored tests make the
pending execution visible; an ordinary workspace test run cannot certify them.

This checkpoint is based on `df57e8debad37c48c07dc724971fb5d09c5b1537`.
It prepares the public regression for the independently accepted synthetic
experiment, whose local preparation manifest is
`59a3165fa019d308c69e259e08b0d5ee849bfb83e4aabc7cf2b9b9647226d106`.
That acceptance covers the proposed experiment, not its execution or cause.

## Source observation and unresolved mechanism

`media/audio.rs::AudioStream::open` copies codec parameters without setting the
decoder packet timebase. `State::push` then compares the first frame PTS with the
source origin and rounds their difference at the project sample rate; `emit`
inserts or drops samples. The linked FFmpeg source removes automatic skip
samples, but advances their frame timestamp only when the decoder packet
timebase is set. The hypothesis is a second drop after automatic priming removal.
No production fix or fixed MP3 delay follows from this source observation alone.

The frozen C experiment compares unset and stream packet timebases using the
same decoder/library, automatic skip handling and packet sequence. Its raw sample
bytes/counts, actual skip/discard metadata and first/last PTS must establish the
mechanism. The engine's cached source must also match the timestamp prediction at
both output rates. Contradictions remain evidence; importing a fixture is not a
causal verdict.

Source zero remains the first video stream timestamp for linked A/V, or the audio
stream start for audio-only inputs, with the existing zero fallback for a missing
start. Packet/frame timestamps use the stream timebase; skip/discard counts use
source samples. Resampling and source alignment occur before authored clip
placement. Authored silence, linked offsets and trailing discard must survive.

## Prepared regression

[`audio_origin.rs`](../../crates/ferrocut-engine/tests/audio_origin.rs) calls the
real `decode_audio` and cached `audio::prepare` paths without a GPU. Its oracle is
each input's own same-build LGPL FFmpeg decode, including lossy MP3; original WAV
sample values are never used as an MP3 oracle.

| Control | Assertion |
|---|---|
| WAV, FLAC and observed untagged MP3 at 48 kHz | Exact source length and sample values; exposes a blanket codec or resampling offset. Untagged encoder padding is preserved as observed. |
| Tagged MP3 at 44.1 and 48 kHz | Exact source length and sample values against its own reference, retaining authored initial silence and trailing discard. |
| WAV and tagged MP3 at 48 kHz, source-in 1/4 s, start 1/8 s, duration 1 s | Source samples [12000,60000) occupy program [6000,54000); all other samples in the two-second program are zero. |

The placement boundaries are integral at 48 kHz. Separate source checks at
44.1 and 48 kHz distinguish decoding from resampling without changing the
existing rational placement rounding rule. Comparisons use no lag fitting, fixed correction or adjustable
tolerance; +0 and -0 are equivalent silence. Numeric differences must be
investigated, not hidden by changing the oracle.

[`import-audio-origin-fixture.py`](../../scripts/import-audio-origin-fixture.py)
imports a completed run from the frozen experiment. It verifies recipe and run
file SHA-256 bindings, command completion, all seven cases, actual tagged/no-tag
metadata, and equal probe-arm decoded bytes/counts. It copies only four synthetic
sources and five reference PCM files, with sanitized timing metadata and a new
hash manifest. It does not copy private custody or raw command logs, invoke any
codec, or declare the suspected mechanism verified. Existing output directories
are refused. Tests check file lengths; the importer and retained manifest provide
the SHA-256 custody, which must be rechecked before freezing fixtures for CI.

## Provenance and licensing

The accepted recipe authors two seconds of 44.1 kHz stereo signed-16 PCM with
LCG seed `0x5EED2026`, multiplier 1664525 and increment 1013904223 modulo 2^32.
Bursts of 2205 samples start at 6615, 17640, 35280, 48510, 66150 and 81585.
The first 150 ms is authored silence. The original recipe and synthetic content
are Apache-2.0; no client media is an input.

Exact recipe, probe and tool-pin digests are in the importer. The scheduled
experiment uses the already installed standalone LGPL LAME encoder: `-T` for the
tagged MP3 and `-t` for the no-tag control, with `--noreplaygain --cbr -b 128 -m s`.
Its decoder is never used. FLAC encoding and every reference/probe decode use the
pinned dynamically linked LGPL-only FFmpeg build. No dependency, download,
install, codec setting or existing experiment pin changes in this checkpoint.

## Later execution sequence

1. The lead schedules the frozen experiment under its existing execution custody.
   Retain exact binary/library hashes, logs and its measurements-only manifest.
   Independent review decides whether the packet-timebase hypothesis is supported;
   an incomplete or contradictory run does not authorize a repair.
2. Under a granted slot, import those public synthetic outputs into a fresh
   fixture directory. From this worktree, with `RECIPE`, `RUN` and `FIXTURE` set
   to the actual accepted preparation, completed run and fresh output paths:

   ```sh
   python3 scripts/import-audio-origin-fixture.py \
     --recipe-dir "$RECIPE" --experiment-dir "$RUN" --output "$FIXTURE"
   ```

3. Verify formatting/Clippy and run the prepared tests against this unchanged
   decoder, retaining every result, including unexpected control failures:

   ```sh
   FERROCUT_AUDIO_ORIGIN_FIXTURE_DIR="$FIXTURE" \
     cargo test --locked -j 4 -p ferrocut-engine --test audio_origin \
     -- --ignored --test-threads=1 --nocapture
   ```

   Missing inputs fail explicitly when these tests are requested. There is no
   silent successful skip. An actual baseline failure has not yet been observed.
4. Only after mechanism review and separate implementation authority, make the
   minimal decoder change and bump `AUDIO_DECODE_VERSION`. Run the same frozen
   fixture and tests on that exact candidate; preserve old-fail/new-pass evidence.
   A constant audio offset is not an authorized substitute.
5. Before integration, freeze the small public fixture and hashes, make the
   regression active in CI, add the accepted linked-A/V offset control, verify
   cache invalidation and cold/warm determinism, and run required repository and
   unchanged demo checks. Those are repair acceptance work, not results here.

No native master, playback/listening, mixed-film correlation, installed-route or
creative acceptance is claimed by this preparation.
