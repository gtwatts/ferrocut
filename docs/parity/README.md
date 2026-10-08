# Professional capability ledger

Ferrocut's product target is one professional editing, audio, motion-graphics and
compositing system designed for AI agents: the combined capability of Premiere
Pro and After Effects, with familiar production concepts and discoverable tools.
Higher-quality finished videos and reliable revisions are the acceptance target.

This directory turns that target into research, code evidence and executable
acceptance checks. It is a capability roadmap, not a claim of Adobe parity.

## Research and evidence

- [Premiere inventory](PREMIERE_PRO_INVENTORY.md): current official Adobe sources,
  longstanding production features and agent-facing acceptance criteria.
- [After Effects inventory](AFTER_EFFECTS_INVENTORY.md): the corresponding
  compositing, animation, typography, VFX and finishing capabilities.
- [Ferrocut audit](FERROCUT_AUDIT.md): implementation and integration evidence in
  the actual repository, including limitations of optional extension crates.
- [Capability ledger](capabilities.json): stable IDs, current status, limitations
  and acceptance evidence. `python3 scripts/parity-inventory.py check` validates
  the ledger; `summary` prints counts without presenting them as a parity score.
- [Agent onboarding guide](AGENT_GUIDE.md): native text, vectors, finishing,
  exact time, asset handling and reversible edits. Also served by MCP at
  `docs://agent/onboarding.md`.
- [FilmCraft and EffectCraft review](STORYTOLD_REVIEW.md): pinned source,
  reusable component candidates, model differences and unverified runtime claims.
- [Executed FilmCraft/EffectCraft integration](../integrations/STORYTOLD.md):
  retained engines, connected agent controls, adapter tests and rendered evidence.
- [Executed native milestone](EXECUTION.md): actual tests, inspected render,
  title revision/cache/undo evidence and unresolved baseline failures.

The inventories use stable `PP-###` and `AE-###` identifiers. Product version,
source date, beta status and dependencies belong alongside each researched
capability. Cloud services and licensed third-party ecosystems remain explicit
dependencies; similar output does not establish compatibility with Adobe files,
plug-ins or services.

## Completion rules

Each capability needs its own evidence; a passing crate build is insufficient.

1. **Researched:** an official source establishes the feature and its scope.
2. **Implemented:** the relevant code exists, with known limits recorded.
3. **Integrated:** a real project can use it through the timeline and applicable
   CLI/MCP operations. Its schema, documentation and asset dependencies agree.
4. **Verified:** meaningful tests exercise the intended behavior, including
   validation, animation, edit history and cache invalidation where applicable.
   Visual/audio features also require an actual rendered artifact and inspection.
5. **Reviewed:** demanding production examples demonstrate the intended quality
   and revision workflow. Technical correctness and creative review are separate.

Only a capability meeting its recorded acceptance criterion can be checked off.
Partial implementations retain their missing sub-capabilities. The checklist is
maintained as Adobe and Ferrocut change; its row count is not a percent-complete
estimate and does not measure engineering effort.

## Agent usability

Use familiar concepts (clips, tracks, layers, compositions, keyframes, effects),
consistent names, explicit units and time bases, concise tool responses, strict
schemas, actionable errors, dry runs and reversible edits. Provide a short
operating guide and deeper references on demand, plus editable examples that
show typography, motion, picture and sound working together. Test onboarding in
a fresh agent session using only the shipped context.

## Starting checkpoint

On 2026-10-08, implementation began from local `main` at `701b88f`, with existing
uncommitted Rhai expression work. The 16 pre-existing changed/untracked source
files and their patch were preserved in
`/tmp/ferrocut-before-parity-build-20261008/` before integration work.

The earlier focused check passed 84 tests and scoped formatting. Historical
`eval/results/fc9-all/summary.json` records nine passing **reference-solution**
tasks, not nine autonomous-agent successes. The first broader workspace baseline
hit a native `ferrocut-color` test crash; its investigation and subsequent checks
must remain visible in the execution evidence.

No deployment, paid provider use, codec download or external publication is
implied by capability research or local implementation.
