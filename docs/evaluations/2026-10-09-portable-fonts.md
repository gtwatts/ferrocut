# Portable caption imports and stable text cache identity

Combined source `73935ba661c0772ca5b464fa6aa9c2a8f89626e3` is pushed on
`feat/agent-native-core-adoption` (PR 1) and installed locally. This report describes
the bounded technical milestone; whole-film creative acceptance remains separate.

The first production cycle exposed two related costs when moving an editable film.
The CLI caption importer stored style-file font paths as absolute paths, so moved
projects lost their fonts. Relinking an otherwise identical font also changed text
chunk keys and produced font-change noise in the default timeline diff.

The reviewed fixes address both paths. CLI import resolves primary and fallback
fonts relative to the style file, checks their real canonical location, and stores
contained assets relative to the timeline. External assets remain explicit and are
reported through `nonportable_fonts`. A sibling `--output` uses the existing edit
operation's absolute rebasing; its report now lists those fonts and warns that the
new output is not portable. No automatic asset copy or package creation is implied.

Native text cache identity now uses ordered frozen font bytes, face index and render
parameters without path strings. Moving identical assets keeps text chunk keys.
Different font bytes or fallback order still invalidate affected text. Default
render-aware file diff compares full font-content digests; `--no-render` and the
in-memory structural diff retain path differences and do not read font contents.
Missing/invalid fonts retain error detail. MCP project-root checks run before new
font-content reads.

The text-key change invalidates previous text caches once. Non-text key domains and
the sampled rendered text pixels remain unchanged. Document hashes do not snapshot
external files or reconstruct font bytes overwritten in the past.

## Reviewed source and evidence

- Engine source `cfc92ec`, evidence-only follow-up `66f4891`, PR 5: seven real-font
  regressions, source checks and actual CLI/MCP controls. Twelve before/after/moved
  PNGs are byte-identical; a changed-font control alters 3,920 pixels. Unchanged CI
  masters match first-cycle outputs. Isolated optional-native test exclusions and an
  interrupted CI attempt followed by a clean unchanged retry are preserved in the
  [font-content identity report](2026-10-09-font-content-identity.md).
- Workflow source `239129d`, PR 4: 22 caption tests, Clippy and formatting. A real
  moved-project import renders former blink frames 31/73 identically to the accepted
  first-cycle repair. A preserved old binary falsely reports an empty nonportable
  list for sibling output; the new binary reports both primary/fallback paths in
  dry-run and write. Independent review confirms unchanged output timeline bytes.
- Combined source `73935ba` contains the exact 10 changed files from those reviewed
  makers; independent source mapping found no drift. The fully provisioned combined
  checkout passed required formatting and all-target Clippy, **610 workspace tests
  (2 ignored doctests)** and **8 delivery tests**. All four release binaries built.
  The unchanged CPU render CI produced the same four master-file hashes as the
  first-cycle integration, with exact reference video/audio hashes, 25 sampled
  SSIM values of 1.0 and matching one/two-worker outputs. All **9 reference MCP
  workflows** passed (`--agent none`, no provider or creative score).

## Installed route

The local installer completed at `20261009T111917Z`, retaining a recoverable backup.
The preinstall and postinstall Codex configuration hashes are identical; all four
backup binaries match the prior installation. A fresh subprocess launched through
that existing MCP entry executed 16 calls, and the installed CLI executed four
additional probes. These prove primary/fallback relative import and moved-project
planning, sibling-output dry-run/write reporting, content-based keys and default
versus structural diff, changed-font/fallback-order invalidation, native fit,
caption coverage at former blank frames, undo and outside-root refusal. They do
not establish that an already-running harness server reloaded.

| Installed binary | SHA-256 |
| --- | --- |
| `ferrocut` | `38603e7af5a92882a7ba9f8126adb4ccfdc418bdbf6bc9a1042ec0dd3f94c247` |
| `ferrocut-mcp` | `119c110454021ca7148924de0a51d3e3f3df24c701c75cd69e23d3addb9380ba` |
| `ferrocut-perceive` | `3f502aa8aabb6fd58ac9f41cf31c93aeb9216f3a8c77484f94539eb48875ca28` |
| `ferrocut-deliver` | `897bd20cb1964b34c0593237e620bb50e589ad20b1b483bd1fe7b3cfd02ceeaa` |

Local evidence is retained under `out/production-cycle-20261009/cycle2/`:
`integration/checks.json`, `integration/installed-verification.json`,
`integration/installation-preservation.json` and the independent `review/` receipts.
The combined technical acceptance is pinned to the exact source above; installation
has its own executed evidence and review scope.

No whole-film rerender was required for this font-path/cache repair. The first-cycle
masters, viewing copies and accepted packages remain frozen. This evidence covers
source behavior, actual tool paths and bounded samples; it does not establish full
motion/listening or professional creative acceptance. Caption-run plates and
timeline-aware checker targets remain separate open work.
