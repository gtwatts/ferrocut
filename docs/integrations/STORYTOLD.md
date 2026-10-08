# FilmCraft and EffectCraft in Ferrocut

Gordon requested reuse of both projects' Rust internals on 2026-10-08. Their core
source is retained in Ferrocut and selected capabilities execute through native
Ferrocut adapters. Existing timelines, edit batches, expression evaluation and
render caches remain the authoring model.

## Source and executable features

| Component | Retained source | Connected in this build |
| --- | --- | --- |
| FilmCraft `5231852`, version 0.4.0 | 42 core packages | OTIO/FCP7 timeline interchange; numeric waveform, parade, histogram and YUV/HLS scopes |
| EffectCraft `6943872`, version 0.6.0 | 31 core packages | 170 CPU effects; polygon/star; nine ordered path operations |

Nineteen upstream packages form the build dependency closure. The remaining
source packages include codecs, containers, editor commands, audio DSP, export,
text, tracking, renderers and format readers. These are available for subsequent
adapters; **retained source is not an enabled or verified feature**. Run
`ferrocut capabilities` for the package inventory and connection boundaries.

All 306 EffectCraft effect entries are classified: 170 connected, 136 unsupported.
Unavailable entries name the missing host or data requirement. They are absent
from the executable registry. Examples include effects needing other layers or
frames, auxiliary EXR channels, audio DSP, simulation state, external files or
models. Counts do not establish equivalence to After Effects.

The source archive hashes, exact revisions, license files, notices, original
workspace manifests, file hashes and exclusions are retained under
[vendor/storytold](../../vendor/storytold/README.md). One documented FilmCraft
patch fixes UTF-8 file-URL slicing. Repository assets, desktop UIs and ArtCraft
branding were excluded. EffectCraft's older FilmCraft git dependency pin remains
unchanged in its unconnected media/render crates.

## Agent workflow

MCP discovery is deliberately paged. Start with `capabilities`, then
`effects_catalog({"query":"glow","details":true})`. Use exact effect IDs and
parameter names from that result; numeric controls support constants, keyframes
and expressions through the existing edit operations. Effect points are canvas
fractions, effect colors are linear ACEScg, and popups use the documented labels.

```json
{"op":"add_video_effect","clip":"title","effect":{
  "type":"ec.stylize.glow","id":"halo","radius":4,"intensity":"0.55"
}}
```

```json
{"op":"set_param","clip":"star",
 "param":"generator.shape.operators.1.angle","value":{
   "keyframes":[{"t":0,"v":0},{"t":2,"v":85}]
 }}
```

Effects sample their controls in clip-local time, or timeline time on tracks.
Intrinsic procedural phase has a separate `clock_offset`, maintained by split
and trim. Shape controls and intrinsic path wiggle use source time. This matters
when revising retimed clips. Frame keys include procedural time and output rate;
constant controls cannot freeze animated noise or strobe through cache reuse.

Import/export tools accept `format: "otio" | "fcp7"`, `dry_run` and `allow_loss`.
Inspect the loss report first. A lossy write requires `allow_loss:true`; an
existing output is never overwritten. Imported compositions become generated
siblings of the new native document. Ordinary write errors roll back created
files; a multi-file import is not a crash-atomic transaction. MCP checks media,
fonts, nested documents and outputs against its project root before file access.
Non-file URI sources require relinking to local media.

Imports probe guarded media files and compare their actual duration with every
consumed source range, including generated nested documents. Foreign declared
duration cannot grant additional handles. Missing, corrupt, unknown-duration or
short media adds a loss and requires `allow_loss:true` for a write.

```sh
ferrocut effects --query ec.stylize.glow --details
ferrocut interchange export edit.json -o edit.otio --dry-run
ferrocut interchange import edit.otio -o imported.json --dry-run
ferrocut scopes render.mkv --at 1
```

`scopes_read({"path":"render.mkv","at":1})` uses the reused FilmCraft scope
math on decoded display-encoded RGB8. It returns bounded numeric feedback with
sample size, alpha information and explicit units. It does not infer a transfer
function, composite transparent RGB or certify HDR mastering. Full decoding is
bounded to 16,777,216 pixels; scope sampling is nearest-sample, at most 480×270.

See the detailed [effect adapter](effectcraft-effects.md),
[shape operator](effectcraft-paths.md) and [interchange](filmcraft-interchange.md)
contracts for precise parameters, losses and unsupported modes.

## Evidence and limits

The implementation checks source provenance, runs focused and existing regression
suites, and renders [a native integration example](../../examples/storytold-showcase.json).
Final command logs, frame images and measurements are recorded with the integration
artifacts. Automated tests compare adapter pixels to pinned upstream execution and
use independent analytic expectations for selected controls; this is adapter
verification, not proprietary Adobe pixel parity.

The [inspected revision frame](artifacts/storytold-revision-compare.png) comes from
two 640×360, 24-fps, two-second FFV1 masters. Ordinary edits change native title
content and star twist keys. The repeated revision reuses all 48 frames with zero
GPU submissions. Undo also reuses all 48 frames and restores the original full
file hash. The [artifact manifest](artifacts/manifest.json) records the reports,
operation files, source hashes and inspection scope. Masters remain in `/tmp`;
the editable example, operations, reports and PNGs are retained in the repository.

A separate media-only edit exports through FilmCraft OTIO and imports back with
no reported picture/sound losses. All 36 decoded RGBA frames match exactly; GOP
defaults change, so compressed packet hashes differ. Editing the imported second
clip's opacity and rendering again succeeds. This demonstrates this fixture,
not external Adobe application acceptance. See [round-trip measurements](artifacts/roundtrip-evidence.json).

The final 37 focused effect/expression tests passed with actual NVIDIA RTX 5090
Laptop GPU and llvmpipe Vulkan rendering. The 16 pure interchange, 30 vector and
operator, 7 editor-control, 4 scope and 10 MCP integration checks cover their
documented boundaries. The latter include protocol/root escapes, loss gates,
source probing, generated-nest collisions and new-file refusal. Tests overlap
with the broader regression run; their totals must not be added together.

The final serial engine/MCP/core/types regression command passed **357 tests,
zero failures, one ignored doctest** ([full log](artifacts/regressions.log)).
All-target Clippy for those four packages passed with warnings denied. Scoped
formatting, source-provenance verification, ledger validation and the ledger's
24 self-checks passed. The native-title acceptance receipt was refreshed against
the current source inputs. This does not certify the entire workspace: the
previous color-crate parallel-test crash and HTML sandbox limits remain recorded
in [the earlier execution report](../parity/EXECUTION.md).

Integration exposed and fixed native master/proxy final-frame duration metadata,
anisotropic scale expression coercion, UTF-8 file-URL slicing and procedural
clock/cache boundary errors. Proxy encoding preserves next-frame timing and the
known source end; it does not invent VFR frame duration from average frame rate.
The proxy encoding version participates in on-disk filenames and render keys so
older proxies cannot mask the timing fix.

The effect bridge executes upstream CPU kernels. GPU-backed inputs require a
readback/upload. Canvas, padded buffers, work estimates, text controls and geometry
expansion are bounded; oversized cases return an error. Cancellation is checked
between copies and upstream calls; a monolithic upstream kernel cannot be
interrupted mid-call. Real-time performance has not been established.

Shape groups and repeaters, cross-frame/layer effect hosting, model-backed
tracking/roto, native codec replacement and remaining retained engines still need
adapters and demanding production tests. FCP7 has narrower edit representation;
anisotropic scale, subframe timing, track names and unsupported processing are
reported as losses. No external Premiere/Resolve import acceptance has been
measured. Ferrocut is not yet Premiere Pro/After Effects level.
