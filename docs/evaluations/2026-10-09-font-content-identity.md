# Font-content cache identity — 2026-10-09

Moving a timeline and byte-identical font assets previously changed every chunk
that used native text. Default render-aware diff also reported the relocated font
paths as changes. Source `cfc92ec44caf8d610e129f83bba6024fda2607e8` repairs both
behaviors, based on `bda8a2153be04d76bd1262c420a131b563765e6f`.

## Behavior and boundaries

Text keys now omit font locators from the rendering parameters and retain the
ordered, frozen font bytes, face index, raster dimensions and other controls.
Moving identical fonts preserves static/animated node and planned chunk keys.
Replacing bytes at the same absolute pathname, changing primary/fallback roles,
or reordering distinct fallbacks still invalidates text. Existing compiled nodes
retain their original font snapshot after an on-disk replacement or deletion.

This changes the text hash domain once, invalidating old text keys. Non-text
domains are unchanged. The rasterization implementation is unchanged.

Default file diff compares full 256-bit font-content identities in comparison-only
copies. Primary and fallback slots remain ordered. Missing, unreadable or invalid
fonts preserve structural differences and an explicit `render_error`; they are
never reduced to a common empty identity. MCP root preflight still runs before
font-content reads. The tool's discovery description documents the distinction.

`--no-render` and the in-memory diff remain structural and do not read font
contents. They can report font-path changes. Document hashes still describe the
timeline JSON, not an external-asset snapshot: equal document hashes can accompany
different font bytes. Historical overwritten bytes cannot be reconstructed.

## Reproduction and actual outputs

The retained 640×180, 24 fps, two-second fixture uses synthetic text
`Ferrocut مرحبا`, licensed Noto Sans and Noto Sans Arabic test fonts, and a real
Fira Sans Bold control from the existing production package. Font files, licenses,
timelines and commands are retained locally; nothing was downloaded.

At the baseline, relocation changed chunks 1 and 2, which contain text, while
background-only chunks 0 and 3 stayed equal. The repaired release gives equal keys
across relocation, an empty default diff and no dirty chunks. Genuine changed-font
and reordered-fallback controls dirty chunks 1 and 2. Structural diff retains the
path changes. Fresh MCP processes also verified missing-font failures and refusal
of fonts outside the project root before content comparison.

Four PNGs at frames 0, 12, 24 and 36 were retained for each of baseline, repaired,
and repaired-after-relocation: all 12 are byte-identical across those routes and
have the expected dimensions. Text frames contain 1,907 bright text pixels; the
maker inspected the actual before/after frame 24. A thirteenth PNG uses Fira Bold
at the same **relative font slot in a separate project directory** and visibly
changes the Latin glyph weight. The regression test separately overwrites one
**absolute** font pathname. Independent review counted 3,920 changed RGBA pixels
in the Fira control and independently compared all 12 relocation frames.

| Retained PNG | SHA-256 |
| --- | --- |
| Text frames 12/24, all three equivalent routes | `eafdf1e591b0bbedd3a394dfd7abd49b62f7f4af1abbf1b0974cd5d991d87b13` |
| Background frames 0/36, all three routes | `ea29249a85991c80d862ee67b992838156441a94c0d05a2e4333cac8d0b2eb23` |
| Fira Bold frame 24 | `1f91c09564a72b424c055a8ec1ce61e6b8f381c4443a46c9bc182cebca8a5570` |

## Checks and exact release

The CONTRIBUTING format and Clippy checks passed for the owned crates, including
all targets with warnings denied. Locked, offline workspace tests excluding the
delivery integration target passed **555 tests, with 2 ignored**; delivery library
and binary tests passed **8**. The workspace run includes seven new real-font
regressions. Release engine, MCP and perceive builds passed. Checks ran sequentially
with at most four build jobs and one render process at a time.

OCIO, CEF, ThorVG and OpenFX built their documented stubs in this isolated tree;
their 51 optional integration tests were not executed. This explains the count
difference from the preceding fully provisioned cycle: 599 − 51 + 7 = 555.
No OpenH264 integration download was attempted. Shared FFmpeg reports
`9.0.2-ferrocut-lgpl`, LGPL 2.1 or later, without GPL/nonfree configuration.

| Binary | SHA-256 |
| --- | --- |
| Baseline CLI, source `bda8a21` | `e9ab2ea09c7ad040cea8560ed150e874166e566096093aafcbdbbe2d6bd38d95` |
| Repaired CLI, source `cfc92ec` | `14afd2f749cb688db1630bfb1ece9ad8a357be589c047961263cbaaf4cb58374` |
| Repaired MCP, source `cfc92ec` | `b9f574d12ce61a273289ad173916e2ff857b7831d4641c5819dc1c690c222187` |
| Repaired perceive, source `cfc92ec` | `e330640baef1f2f959df79aa1912bd4835398480873bb4b3ba5e861db95ec782` |

Unchanged `ci/render-check.sh` passed on llvmpipe Vulkan, LLVM 20.1.2, Mesa
26.1.6. Fresh `-j 1` and `-j 2` renders are byte-identical: demo has 288 frames,
demo-av has 312. Both videos match the reference hashes, all 25 sampled SSIM
values are 1.0, and demo-av's quality check passes. Its 624,000 stereo samples at
48 kHz match reference BLAKE3
`751eda4b8d2d1d61f6afa79ed84d3e6a3161df1eb448ba87ad5b4bc11f208a8a`.
The demo master SHA-256 is
`db96a9df0b762ba9d886e1d1385583bfbe30d2c2a29c8a42fca420bf99ffd269`;
demo-av is `c6a3938852dc30c5c9ffc73c888193ca888bf787d59f3789914777c06eba9131`.
No reference was changed.

The first CI runner terminated with exit 143 during the second demo-av render;
the child verdict and cause were unconfirmed. Its log and partial outputs remain
under `ci-render.log` and `ci-render-interrupted/`. The clean retry exited 0 in
72.659 seconds without source or reference edits. Early test compile/assertion
and Clippy failures are also retained, with their corrections and passing reruns.

## Evidence custody and limits

The engine worktree is `out/production-cycle-20261009/cycle2/worktrees/engine`.
Its `out/font-evidence/manifest.json`, `checks.jsonl`, `source.patch`,
`before-mcp.json`, `after-mcp.json`, `pixel-evidence.json`, `SOURCES.md`,
`ci-render-retry.log`, retained PNGs and CI masters bind the claims above to the
exact source and binaries. The final handoff adds `final.patch` and
`artifacts.sha256`. Independent reviewer receipts are in the shared root's
`out/production-cycle-20261009/cycle2/review/engine-*-cfc92ec.json`.

The reviewer accepted the source, required source-check evidence, exact binaries,
fresh MCP responses, retained PNG comparison and final unchanged CI in
`engine-technical-acceptance-cfc92ec.json`. Independently hashed CI masters also
match the preceding integration cycle byte for byte. The heavy slot was released
at 02:50:31 UTC.
The lead owns integration, installation and fresh installed-route verification.
This branch verifies its exact release subprocesses; it does not replace the
installed MCP configuration.

This is bounded font identity and semantic-diff evidence. It does not establish
full-film, continuous-playback or listening acceptance. Caption import, generalized
font discovery and the separately reported package-storage issue are outside this
slice. Declared and daemon-observed model: `gpt-6-astra`, runtime Codex; backend
routing and remaining quota were not independently verified.
