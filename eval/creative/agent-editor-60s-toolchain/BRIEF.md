# Codex, image generation, ElevenLabs and Ferrocut: a real creative test

Gordon watched the first local-only Ferrocut review cut. He now explicitly wants
Codex to use its other creative tools: native Codex image generation for assets,
ElevenLabs for narration, and Ferrocut for the edit. Create that second version
yourself. Do not stop at a plan. The topic remains **“Why AI agents need a real
video editor.”** Make an original, polished, Vox-inspired editorial explainer:
**exactly 60 seconds, 1080 × 1920, 30 fps**, with narration, captions, purposeful
motion, an engaging opening, clear visual reasoning and a satisfying conclusion.
No Vox branding or implied affiliation. You own the treatment, script, asset
prompts, storyboard, timeline and revisions. A diagram-only repeat of the first
test does not satisfy this brief.

## Use the requested tools

- Use the built-in native Codex image generation tool for a small coherent set
  of original raster assets that advance the explanation. The installed CLI's
  `image_generation` feature is enabled. Read the installed `imagegen` skill.
  Gordon referred to GPT Image “2.5”; preserve the actual tool/model metadata and
  do not claim a model version that the tool does not expose. Use the native
  ChatGPT-connected tool, not an Images API key or an unrequested CLI fallback.
  Inspect the generated images. Copy selected outputs into this task's assets,
  retain exact prompts and any generation receipts, and use them visibly in the
  finished Ferrocut timeline. Keep typography and explanatory overlays editable.
- Use **ElevenLabs**, which Gordon explicitly requested for this test. The
  existing account passed a read-only authentication check and has sufficient
  existing character allowance. Read the installed
  `gordon-skills:elevenlabs-audio-production` skill and native command contract.
  Discover live command schemas using its `scripts/native-domain.mjs` runner.
  List available voices, choose an authorized stock/premade English narrator,
  estimate timing, and synthesize a stitched scene pack using the existing
  configured credentials. Keep total submitted narration under 3,000 characters
  across attempts. Do not clone a person's voice, buy credits, change the plan,
  or print/package credentials. Keep original generated takes, exact text,
  actual voice/model/settings, provider request IDs and durations. Reconcile
  unknown paid outcomes before retrying. Do not replace ElevenLabs with Qwen.
- If either required provider tool is unavailable or fails, preserve its exact
  diagnostic and write `evidence/REQUIRED_TOOL_BLOCKER.md` promptly. Include
  `evidence/image-requests.json` with your intended prompts if native image
  generation is unavailable. Continue independent work; do not secretly replace
  a requested provider or pretend generation succeeded.

## Ferrocut owns the video

- Work only in this task directory. The installed `ferrocut` MCP server is rooted
  here. Begin with capabilities, applicable live schemas and
  `/home/gordontwatts/Documents/projects/ferrocut/docs/parity/AGENT_GUIDE.md`.
  Installed executables are in `~/.local/bin`. The engine is source revision
  `80c5d888faef04f059ab5551112b8ed40fbb8b7d`; do not rebuild it for this test.
- Import the generated raster assets and narration into a native Ferrocut
  timeline. Compose, animate, mix and render in Ferrocut. No Remotion, browser
  composition, HTML/CSS renderer, or FFmpeg picture-composition filtergraph.
  Authoring scripts are allowed. FFmpeg may prepare audio, extract review frames,
  measure media and encode the final delivery. Use existing encoders/tools.
- Discover exact image/media clip and animation payloads rather than guessing.
  Retain a portable `delivery/project.json`, relative assets, fonts/licenses,
  native edit history, build and reproduction scripts. Preserve request files
  rather than embedding huge schemas in shell arguments. Query only the needed
  schema definitions; full timeline schemas previously exceeded tool IPC limits.
- Render short previews early. Use `view_image` on actual rendered frames and
  contact sheets, inspect every scene and both sides of cuts, and make at least
  one meaningful revision using `edit_apply`. Preserve the journal and reasons.
  This model can inspect still frames; do not claim continuous video playback or
  listening if the runtime cannot accept audio. Use transcription and signal
  analysis for audio checks and clearly state the remaining listening limit.
- Derive captions from the actual generated narration and verify readable
  phone-safe placement. Do not use estimated timings as word alignment. Keep
  enough breathing room for clear speech; the runtime must be exactly 60 seconds.

## Deliver and verify

Write the brief, treatment, timed shot plan and script before final assembly.
Render a lossless native master and a social-ready H.264/AAC MP4. Read actual
render reports and run Ferrocut `quality_check`; preserve failed checks before
fixing them. Do not loosen a failing threshold without a documented reason.
Independently check duration, 1,800 frames, dimensions, frame rate, decode, audio
presence/levels, narration words and captions. Review a final contact sheet and
shot boundaries after revisions. Technical checks do not certify creative taste.

Put the finished film, portable project, assets, captions, poster/contact sheet,
source/license records, prompts/provider receipts, README with reproduction steps,
and candid TEST_REPORT in `delivery/`. Separate observed checks from assumptions,
provider authentication from generation, and numeric audio checks from listening.
Keep caches, models, account details and raw private session/memory logs out of
delivery. No Git operations, publication, messages to others, software installs,
unrelated file edits or global settings changes. The supervising session will
copy the completed package into Gordon's Pictures directory and back up source.

Time allowance: one 60-minute run. Report concrete blockers promptly. Finish all
work that does not depend on them and retain an honest evidence trail.
