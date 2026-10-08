# Installed Codex video tests, 2026-10-08

Two fresh local Codex CLI sessions used the installed Ferrocut tools to author
original narrated 60-second vertical explainers about why agents need a real
video editor. The supervisor installed the tools, supplied briefs, and checked
outputs. It supplied no script, reference timeline, artwork, or creative fixes.

Both sessions used the existing ChatGPT login and the user's selected
GPT-6-Astra override for these tests. The configured GPT-6.1-Sol was rejected
before creative work in an initial attempt. Global model settings were preserved.
The installed engine revision was `80c5d888faef04f059ab5551112b8ed40fbb8b7d`.

## Local-only baseline: The edit makes the story

The [baseline brief](../../eval/creative/agent-editor-60s/BRIEF.md) produced a
completed native project, film, lossless master, portable ZIP, source records,
authoring scripts, and review evidence. The session exited zero after 3,006.86
seconds. The package was copied into the user's Pictures directory, with the
video and all twelve referenced audio/font dependencies hash checked.

The film uses native text/vector artwork, local Qwen3-TTS stock Ryan narration,
and original procedural music and effects. It did not call image generation or
ElevenLabs. The final project has seventeen video tracks, 111 native generator
clips, three audio tracks, and six journaled edit batches containing 588 operations.

Independent verification of the exact final MP4 found 60.000000 seconds, 1,800
frames, 1080 × 1920, 30 fps, H.264 YUV420P, stereo 48 kHz AAC, error-free full
decode, −14.0 LUFS, and −1.1 dBFS true peak. SHA-256:
`b109b536ba06aa5a32a0ae0ed6f6a84ee33298de4cbfc881c805f5f243982e40`.

The native quality checker passed with zero problems and seven warnings for
intentional 0.567–1.233-second reading holds. All seven supplied cuts were detected
using the original thresholds. Earlier failures for long static holds and two
missed cuts were retained and corrected through edits. Final-mix local ASR
recovered all 124 scripted words. The agent inspected all 28 caption cues and
both sides of the cuts; thirteen code-mode invocations contained actual
`view_image` calls, some inspecting several images.

The copied project planned successfully for 1,800 frames, a relocated one-second
sample rendered, and the portable ZIP passed CRC checks. A full other-machine
render and end-to-end reproduction-script rerun were not performed.

## Provider-backed test: Edit the meaning

The [second brief](../../eval/creative/agent-editor-60s-toolchain/BRIEF.md) adds
native Codex image generation for assets and ElevenLabs for narration, as
requested after review of the baseline. Ferrocut still owns picture composition,
animation, timeline edits, mixing, and the lossless native render. FFmpeg handles
audio preparation, review extraction, final encoding, and container repair.

Three native `image_gen.imagegen` calls produced the selected collage, falling
cup, and fictional portrait. The supervisor independently compared the selected
files against the native generated-image outputs; all three hashes matched, all
three assets are referenced in the actual project, and their use is visible in
rendered samples. The image tool exposed no exact model version; this test does
not establish a “GPT Image 2.5” version claim.

ElevenLabs generated eight original narration takes using the listed premade
Matilda voice and `eleven_multilingual_v2`. The original takes, exact text,
settings, and eight matching provider history receipts establish execution.
The requests submitted 672 primary narration characters. The wrapper omitted
request IDs; the agent recovered matching records with a read-only history query
using the existing native credential configuration, without regenerating speech.
Music and effects are original procedural synthesis.

The agent inspected previews, corrected stretched images through native
`edit_apply`, improved label contrast, and revised captions into 32 timed phrases.
The final mixed-audio transcription matched the complete script.

The native Matroska render contained 1,800 frames over 60 seconds but omitted a
default frame duration. Readers inferred 29.97 fps, and an initial viewing export
became 60.06 seconds. The agent repaired container metadata to declare 30 fps;
video and audio stream hashes matched the untouched native output. Both outputs,
the failed repair attempt, repair script, and verification records were retained.
The engine itself has not yet been fixed for this issue.

The corrected viewing copy passed the supervisor's independent checks:
60.000000 seconds, 1,800 frames, 1080 × 1920, 30 fps, H.264 YUV420P, stereo 48 kHz
AAC, complete error-free decode, −14.1 LUFS, and −1.2 dBFS true peak. SHA-256:
`27b14144d22835b7d517993df337b16ead10001bd7c7b8d23660692f50394b45`.
It was copied and hash checked in the user's Pictures directory while the agent
finished its editable package. The native checker passed with zero problems and
56 non-failing caption/overlay-boundary warnings, with unchanged defaults.

## Limits and follow-up

These runs demonstrate sampled visual inspection and revision. The model
interface can inspect rendered frames and contact sheets; it cannot watch
continuous playback or listen to audio. Speech recognition and signal checks do
not certify narration naturalness, musical taste, or the whole viewing experience.
No physical-phone review, platform upload, or general Adobe feature parity is
established. Creative quality remains a human review judgment.

Large live schemas exceeded response limits and required compressed discovery
through the installed stdio MCP server. The provider-backed run also encountered
an unavailable FFmpeg `drawtext` filter during review-sheet extraction. Container
frame-rate declaration, schema size, review-tool availability, and turnaround
time remain concrete engineering issues exposed by the tests.

Raw Codex conversations, private Obsidian memory, credentials, provider account
status, and model caches are excluded from public source and delivery packages.

CI for source revision `8406f2e` passed rustfmt, Clippy with warnings denied,
workspace tests, release builds, and the CPU render checks:
[run 37825166466](https://github.com/gtwatts/ferrocut/actions/runs/37825166466).

## Follow-up, later on 2026-10-08

The engineering issues these tests exposed were fixed the same day on
`feat/agent-native-core-adoption`: masters, chunks and proxies now declare
their frame rate (and always carry the exact rate as a `FERROCUT_FRAME_RATE`
stream tag, since 59.94p cannot be expressed as an integer-nanosecond
Matroska default duration); the published tool schemas are compacted from
~15 MB to ~125 KB; `preview_frames` / `ferrocut stills` render frames and
labeled contact sheets without a video encode; and `ferrocut-mcp --doc`
serves the guide and schemas to agents without an MCP client. The next
test, with Claude producer agents and the CLI, is described in
`eval/creative/claude-videos-20261008/`.
