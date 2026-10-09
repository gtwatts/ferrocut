# Fresh Codex creative test: Why AI agents need a real video editor

Create the finished video yourself using the installed Ferrocut agent tools. Gordon
wants to see what his local Codex can actually make with Ferrocut. This is a real
creative task, not a reference-solution replay or a request for a plan alone.

Deliver one **exactly 60-second, 1080 × 1920, 9:16 social media explainer** about
**“Why AI agents need a real video editor.”** Target a polished editorial explainer
inspired by Vox: a strong opening question, clear visual reasoning, energetic but
controlled pacing, readable typography, purposeful diagrams, precise animation,
and a satisfying ending. Use your own design; do not use Vox branding or imply an
affiliation. You own the treatment, script, storyboard, artwork, edit and revisions.

Explain the practical value of timelines, compositing, editable motion graphics,
audio control and an inspect-render-revise loop. Avoid unsupported claims about
model training or promises that Ferrocut matches all Adobe features. Show what
you mean through evolving visuals, rather than filling every shot with prose.
Keep text in phone-safe margins, accommodate social UI, and avoid tiny labels.
Aim for an engaging narrated film with intentional sound design and captions.

## Environment and boundaries

- The workstation has `ferrocut`, `ferrocut-mcp`, `ferrocut-perceive` and
  `ferrocut-deliver` installed in `~/.local/bin`. Use the `ferrocut` MCP server for
  discovery, timeline validation, edits, rendering and returned reports.
- The checkout is `/home/gordontwatts/Documents/projects/ferrocut`. Start with
  `docs/parity/AGENT_GUIDE.md` and the live schemas/tools. A feature inventory is
  not proof a feature is executable. Discover exact payloads instead of guessing.
- Work only inside this task's current directory. All assets referenced by the
  timeline must be within the task directory, because MCP paths are root-confined.
  Copy the repository's licensed Noto text fixtures and their license if useful.
- Compose, animate and render with Ferrocut's native timeline and generators.
  Do not substitute Remotion, browser/HTML/CSS rendering, or an FFmpeg filtergraph
  composition for Ferrocut. You may write authoring/asset scripts and use FFmpeg
  for audio preparation, independent QC, contact sheets and delivery transcoding.
- Use existing local tools and cached models where possible. Inspect availability
  before claiming a voice or model works. Local Qwen3-TTS 1.7B CustomVoice and
  VoiceDesign caches have been seen on this workstation; this is only a discovery
  lead, not execution proof. Do not clone a person's voice, download large models,
  install system software, use paid generation services, or change global settings.
  If narration cannot be produced, report the specific limitation and finish the
  strongest reviewable film you can rather than hiding it.
- Research/download freely licensed source assets if useful; retain source/license
  records. Do not reuse another client's project audio or private creative assets.
- No publication, Git operations, unrelated file changes or messages to others.
  The supervising session will deliver your output to Gordon's Pictures directory.

## Required work and evidence

1. Write your treatment, narration/script and timed shot plan. Make assets and
   an editable `project.json`; preserve authoring scripts and source/license notes.
2. Use live Ferrocut discovery/schema calls, validate the project, and render
   previews. Inspect actual rendered frames and audio before the final render.
3. Make at least one meaningful improvement through `edit_apply`, retaining its
   journal and the reason for the change. Retain failed attempts and diagnostics.
4. Render a lossless master, read the real render report and run `quality_check`.
   A skipped checker is not a pass. Make a social-ready MP4 using an already
   available encoder if possible; do not download a codec. Keep the master too.
5. Verify exact duration, aspect, frame rate, audio presence/levels and visible
   typography. Review at least every shot boundary and a contact sheet of the
   finished film. Distinguish numeric checks from your creative judgment.
6. Put final deliverables in `delivery/`, including the video(s), a portable
   editable project with its assets, script/storyboard, `README.md` explaining
   exactly how to reproduce it, and a candid `TEST_REPORT.md` listing what worked,
   failures, assistance and limitations. Keep caches and credentials out of delivery.

Time allowance: one 60-minute initial run. If a tool or environment blocks you,
retain concrete diagnostics and complete everything independent of that blocker.
Do not spend the whole run building a production pipeline instead of the video.
