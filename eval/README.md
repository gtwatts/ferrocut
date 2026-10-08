# Ferrocut in-house eval

Five small editing tasks on clips from two Blender open movies. Each task has a brief an agent reads, a starting
`timeline.json`, machine-checkable expectations, and a reference solution. The grader scores the agent's final
timeline and render. It is free to run: the media is CC-BY, everything runs locally, and the agent run uses an
existing Codex (ChatGPT plan) login, never a paid API key.

```sh
eval/fetch-media.sh                      # once: download films (~1 GB), verify, cut 12 clips (eval/media/, gitignored)
eval/run.sh all                          # reference solutions (--agent none): validates tasks, grader and engine
eval/run.sh tos-dissolve-fade --agent codex   # one task with Codex CLI
```

Needs `cargo build --release -p ferrocut-engine -p ferrocut-mcp -p ferrocut-perceive` and the LGPL FFmpeg
(`scripts/build-ffmpeg-lgpl.sh`). Results go to `eval/results/<run-id>/` (gitignored): `<task>.json` with
every check, the agent log (`*.agent.log`; Codex: `*.codex.jsonl` and the last message), and `summary.json`.
Workdirs live under `/tmp/ferrocut-eval/<run-id>/<task>/` (`EVAL_WORK` to move them).

## Tasks

| Task | Film | What the agent must do | How it's checked |
|---|---|---|---|
| `sintel-assemble` | Sintel | build a 12 s sequence from three clips with given in-points and lengths, from an empty timeline | clip order, starts, in-points, lengths; cuts at 4 s and 8 s; loudness |
| `tos-remove-shot` | Tears of Steel | delete the third shot of four and close the gap | remaining clips unchanged and contiguous, 9 s; cuts at 3 s and 6 s |
| `sintel-jcut` | Sintel | turn a straight cut into a J-cut: incoming sound 1 s early, picture cut unchanged | audio regions `[0, 4)` / `[4, 10)`, picture cut at 5 s |
| `sintel-loudness` | Sintel | redeliver for broadcast: -23 LUFS ±1, true peak ≤ -2 dBTP, edit unchanged | loudness settings, and the render *measured* by the checker |
| `tos-dissolve-fade` | Tears of Steel | 1 s dissolve centred on the cut (needs handles), fade in from and out to black | clip edges, `transition_in`, opacity curve, frame luma, no visible hard cut |

Every task also checks:

- `out.mkv` exists;
- it is bit-identical to a fresh render of the final `timeline.json` (no stale or hand-made renders);
- frame count and output settings;
- **`ferrocut check --require`**, SeePlus's perceptual checker through the engine hook. It is given the brief's
  cut list (`--brief-cuts`), so a timeline that renders fine but has the wrong cuts still fails. It also checks
  black, frozen and flash frames and loudness/true peak. A missing checker is an error, not a skip.

Score = passed checks / total, and a task passes only if every check passes. The tasks are deliberately small:
they measure whether an agent can drive the engine correctly (rational times, handles, linked audio, transitions,
loudness) rather than taste.

### Clips

`fetch-media.sh` cuts 6 s single-shot segments: 144 frames at 24 fps, MPEG-4 Part 2 q2 (an LGPL encoder) in
MKV.

- Sintel clips (1280x544) keep their sound as stereo PCM.
- Tears of Steel clips (1280x534) are **picture only**, because the ToS soundtrack is licensed CC-BY-ND
  (see below), so the eval never edits it.

Shots were picked by scene detection and verified with the checker: back to back, every intended cut is detected
and nothing else. One exception: the s5→s6 cut is too similar to detect, so no task puts those two together.

## Attribution and licences

The media is **not** in the repository. `fetch-media.sh` downloads it from an official Blender mirror
(`ftp.nluug.nl`; download.blender.org is behind bot protection). The SHA-256 of each download is pinned in the
script.

- **Sintel**: © copyright Blender Foundation | durian.blender.org. Licensed under the
  [Creative Commons Attribution 3.0](https://creativecommons.org/licenses/by/3.0/) licence. Used: picture and
  sound (`Sintel.2010.720p.mkv`).
- **Tears of Steel**: (CC) Blender Foundation | mango.blender.org. The film is licensed under
  [Creative Commons Attribution 3.0](https://creativecommons.org/licenses/by/3.0/). Used: picture only
  (`tears_of_steel_720p.mov`). The original soundtrack, (C) Joram Letwory, www.tearsofsteel.org, is CC-BY-ND 3.0,
  so the eval strips it.

The renders an eval run produces are derivative works of these films. If you share them, credit as above.

## Agents

- `--agent none` applies `tasks/<task>/reference.json` through `ferrocut-mcp`, the same way an agent would:
  MCP tool calls, plus JSON patches for properties no edit op sets (loudness targets, opacity, transitions).
  Then it grades the result. All five must score 100 %.
- `--agent codex` runs `codex exec` once per task, non-interactively:
  - Codex runs in the task workdir with `approval_policy = "never"` and `sandbox_mode = "workspace-write"`.
  - Its only configured MCP server is `ferrocut-mcp --root <workdir>`.
  - The prompt says: read `BRIEF.md`, edit, render, and verify with `quality_check`.
- **Isolation:**
  - `CODEX_HOME` is a scratch directory (`~/.cache/ferrocut-eval/codex-home`, or `CODEX_SCRATCH`) with its own
    `config.toml`. `~/.codex/config.toml` is never read or written.
  - The only link to `~/.codex` is a **symlink to `auth.json`**, the existing ChatGPT login.
  - The script never logs in. It checks `codex login status` first and stops if that fails, or if the login is
    an API key.
  - `OPENAI_API_KEY` and `CODEX_API_KEY` are removed from the environment, so Codex can't fall back to paid API
    billing.
  - `CODEX_ROUTER=off` bypasses a local model router wrapper, if one is installed.
- Codex still sees the account's remote curated plugins (cached into the scratch home). Unauthenticated ones,
  such as Netlify, fail to start and are harmless.
- The workspace-write sandbox restricts writes, not reads: Codex can read files outside the workdir.

## Adding a task

1. Add `tasks/<name>/` with these files:
   - `brief.md`, the agent's instructions;
   - `start.json`, with sources as `media/<clip>.mkv`;
   - `task.json`: `clips`, plus `expect` (see `lib/evallib.py` `grade()` for the check kinds: `tracks`,
     `duration`, `audio_regions`, `fields`, `opacity`, `render.frames`, `render.luma`, `check.cuts` /
     `config` / `args`);
   - `reference.json`.
2. Run `eval/run.sh <name>`: the reference must score 100 %.
3. Check that a no-op scores well below 100 %.
