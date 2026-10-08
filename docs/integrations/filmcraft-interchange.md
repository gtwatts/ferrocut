# FilmCraft interchange inside Ferrocut

Implemented 2026-10-08. The adapter calls the actual vendored `filmcraft-interchange` readers/writers, using FilmCraft's project/media/time/geometry types internally. It converts back to Ferrocut `Timeline`; Ferrocut's compiler, typed edits, parameter discovery and reversible history remain the native workflow.

Upstream snapshot: [FilmCraft `5231852443363f001c3f6b396dd9b1e6461ae2be`](https://github.com/storytold/filmcraft/tree/5231852443363f001c3f6b396dd9b1e6461ae2be), version 0.4.0. See [vendoring/provenance](../../vendor/storytold/README.md), [MIT license](../../vendor/storytold/filmcraft/LICENSE-MIT), [Apache-2.0 license](../../vendor/storytold/filmcraft/LICENSE-APACHE) and [notice](../../vendor/storytold/filmcraft/NOTICE). This adapter and its synthetic fixtures are original Ferrocut code; no Adobe implementation, project file or test asset was copied.

## Connected formats and fidelity

Only OTIO JSON and FCP7 XML (`xmeml`) are connected. Upstream's EDL, FCPXML, AAF and OMF modules remain present but calls through this adapter reject them as unvalidated. Reader/writer success is not proof of interoperability with Premiere, Resolve, Final Cut or another OTIO implementation; those external applications have not been run for this integration.

| Capability | OTIO | FCP7 XML |
| --- | --- | --- |
| Canvas, rational broadcast frame rate, video track order | Tested | Tested |
| Local media references, gaps, start/source-in/duration at 1x | Tested, including audio-sample subframes | Tested on whole frames; subframes report rounding |
| Track names | Retained | Upstream replaces names with `Video N`/`Audio N`; reported loss |
| Opacity, position, anchor, rotation; linear/hold keys | Tested with nonzero source origin | Tested on frame-aligned keys |
| Uniform scale | Tested | Tested |
| Per-axis scale | Tested | Upstream writes uniform scale; reported loss |
| Timeline/placed-clip markers: source origin, range, name, comment, supported colors | Tested | Tested |
| Native overlapping dissolve ↔ adjacent clips plus transition | Tested | Tested |
| Linked source sound | Retained once on independent native audio tracks | Same |
| Overlapping sound, clip gain/pan automation, constant bus gain/pan, master gain | Tested; overlaps become additive lanes | Track/master mix loss reported; codec precision/rounding checked |
| Resolved native JSON nests with matching canvas | Tested; generated native sibling documents on import | Same |

The geometry bridge compensates for Ferrocut's source decode resizing to the output canvas and FilmCraft's source-pixel transforms. It maps anchor and per-axis scale using actual probed source dimensions. Square pixels are the validated boundary. Native keys are clip-local; upstream effect keys are source-time, so the adapter explicitly adds/removes source-in, including dissolve overlap restoration.

Dissolves require adjacent 1x media edits and available incoming handles. Native export uses `EndAtCut` to avoid introducing half-frame offsets. Import expands the outgoing and incoming ranges into native overlap. One-sided fades, unusual layouts, overlapping transition intervals, unavailable handles, and other transition types fail or produce explicit loss entries rather than silently becoming cuts.

Linked sound is imported into independent native audio tracks and the video's embedded sound is muted, preventing duplicate playback. Constant native bus controls are copied to each generated overlap lane. Linking as a future edit relationship changes representation and is reported as information. J/L offsets, fades, crossfades, preserve-pitch, ducking, audio effects and animated buses/master are outside the connected boundary and report loss.

Generators (including native typography/captions/shapes), adjustment layers, clip/track effects, masks/mattes, non-normal blends, 3D/cameras, motion blur and retiming are not transferred as native foreign effects. Generator/retime clips are omitted leaving gaps; remaining unsupported processing is omitted with loss entries. Prerendering is an explicit future workflow, not an automatic hidden fallback. Foreign caption tracks, multicam/merged metadata, proxy/interpretation settings, shared source markers, specialized markers, groups, mixer routing and nondefault HDR/wide-gamut settings also report losses. Foreign preview/GOP/cache preferences use native defaults and are reported as information.

An explicit native output duration that differs from the last clip end cannot be represented in the upstream sequence model and reports loss. Foreign project bins, labels and clip display names are organizational information outside the native Timeline schema and are reported as information; string identifiers are regenerated and reported. Nonempty custom item metadata reports loss. White native markers have no exact upstream label; unsupported foreign marker label colors also report approximation. Numbered OTIO image sequences report loss: upstream retains a target/base path without native sequence expansion. Parameter values pass through upstream `f64`; nine-decimal native rational fallback reports precision loss, and codec readback compares values within an absolute `1e-9` tolerance. Exact rational time conversion has no such tolerance.

## Pure API

Implementation: [interchange.rs](../../crates/ferrocut-engine/src/interchange.rs).

```rust
export_timeline(&Timeline, Format, &ExportOptions) -> Result<ExportResult>
export_timeline_with_resolver(&Timeline, Format, &ExportOptions,
    impl FnMut(&Path) -> Result<Option<Timeline>>) -> Result<ExportResult>
import_document(&[u8], Format, &ImportOptions) -> Result<ImportResult>
detect(&[u8], Option<&str>) -> Option<Format>
to_tick(RationalTime) -> Result<filmcraft_time::Tick>
from_tick(filmcraft_time::Tick) -> Result<RationalTime>
imported_source_requirements(&Timeline, &BTreeSet<PathBuf>)
    -> Result<BTreeMap<PathBuf, RationalTime>>
```

`ExportOptions` supplies an optional name/base directory and `BTreeMap<PathBuf, SourceMetadata>`. `SourceMetadata` contains duration plus complete video (`width`, `height`, `fps`) and/or audio (`sample_rate`, `channels`) stream metadata. The caller probes media; the adapter does no filesystem access. Missing probe information is an explicit loss, with documented fallback assumptions. A known source shorter than the consumed range, incomplete stream metadata, or an invalid time is an error.

Imported FilmCraft media extents can be expanded by the upstream reader to fit placed clip ranges. The adapter reports this validation limit: normalized document metadata cannot prove actual file duration or available streams. The caller must probe/relink real imported sources before claiming media/handle validity. Missing files are distinct from malformed interchange documents.

`imported_source_requirements` returns the maximum exact consumed source end for each ordinary media path, merging video and independent linked/audio tracks. It skips generated nested sibling names; callers inspect each nested timeline separately and merge maxima. It accepts normalized 1x imports and rejects arbitrary native generators, retimes and J/L offsets. The file/CLI/MCP layer uses actual probes against these requirements; unreadable/unknown/short sources become losses before writing.

`ImportOptions` supplies the input document base directory and zero-based top-level sequence index. Other top-level sequences are reported as loss. `ExportResult` returns bytes/report; `ImportResult` returns the native timeline, report and `NestedTimeline { filename, timeline }` siblings.

The nested export resolver is called only for `.json` source paths and must return validated, path-resolved native compositions. An unresolved composition reports a gap/loss. Imported native composition filenames are generated single-component names `interchange-nest-{id}.json`; references to them are relative sibling names. Write those siblings next to the output native timeline, including references between nested siblings. Imported ordinary media resolves against the input document directory, independently of the output directory. Nest cycles and depth/count limits error; a foreign nested canvas differing from its parent reports loss and omits that clip.

## Report and failure contract

`LossReport.entries` contains `{ severity, feature, location, message }` with serde severities `info` and `loss`. Entries are deduplicated.

- `loss`: omitted or approximated creative/editor data, unknown equivalence, or a codec discrepancy. All upstream findings are promoted to loss because some upstream `Info` messages describe dropped easing handles or rounded keys.
- `info`: a documented representation/bookkeeping change with no known picture/sound loss, such as regenerated IDs, default render/cache settings, independent linked sound or added empty codec tracks.
- `has_losses()` enables the caller's gate; `ensure_lossless()` returns an explicit `allow_loss` error. The pure API returns candidate data plus the report so the caller can review it before writing.
- Unsupported formats, malformed/oversized documents, invalid parameters/ranges, impossible exact conversions, unsafe arithmetic, missing sequence selection, cycles and excessive nesting return errors. A panic guard converts unexpected upstream rejection to an error before any file write.

FilmCraft uses 254,016,000,000 ticks/second. The bridge checks i128 multiplication, exact divisibility and upstream safe signed-tick bounds. It never rounds an unrepresentable native rational into ticks. Export also reads its emitted document through the actual upstream importer and compares sequence/track controls, edit times, markers, effect values/key times and nested sequences. This catches codec losses absent from the upstream report; it does not claim cross-application render equivalence.

Input/output interchange documents are bounded to 16 MiB; compositions to 32 nested levels/256 sequences and 100,000 placed clips. FCP7 reference preflight is bounded to 1,000,000 XML nodes, accepts the standard xmeml DOCTYPE, and has no external-entity resolver. External/image-sequence references are preflighted before upstream path joining, checking both OTIO target URL alternatives: only local filenames/paths and local `file://` URLs are accepted. Every syntactically valid non-file URI scheme (including custom schemes), remote file authorities, UNC/network paths, empty references and decoded control characters are rejected. UTF-8 local paths and Windows drive syntax are covered by tests. This is format validation; the caller must still apply its project sandbox and symlink policy.

## CLI/MCP integration

[interchange_io.rs](../../crates/ferrocut-engine/src/interchange_io.rs) owns bounded file reads, actual source probes, path guards, loss gating and create-new writes. The CLI supports preview before writing:

```sh
ferrocut interchange export project.json -o edit.otio --dry-run
ferrocut interchange export project.json -o edit.otio
ferrocut interchange export project.json -o edit.xml --format fcp7 --dry-run
ferrocut interchange export project.json -o edit.xml --format fcp7 --allow-loss
ferrocut interchange import edit.otio -o imported.json --dry-run
ferrocut interchange import edit.otio -o imported.json
```

Dry-run returns the complete report without writing, including known losses. Writes require explicit `--allow-loss` when the report contains a loss and refuse to overwrite existing output/sibling files. Nested files publish before the parent; ordinary errors roll back files created by the call. This is not a crash-atomic multi-file transaction. An imported document then uses ordinary Ferrocut typed edits/preview/verification/history; import does not replace an open project's edit journal.

## Validation evidence

[interchange.rs tests](../../crates/ferrocut-engine/tests/interchange.rs) use original synthetic fixtures and actual pinned upstream codecs. The focused suite covers OTIO/FCP7 edit round trips (including 24000/1001, 30000/1001 and 60000/1001), paths/gaps/order, source-relative linear/hold opacity/motion keys, different source dimensions, JSON per-axis-scale roundtrip, anisotropic FCP7 loss, native dissolve layout, linked sound once, additive overlap lanes/audio automation/mix controls, timeline/source markers, sample-subframe OTIO versus FCP7 rounding, safe nested outputs/cycles, probed export source-duration bounds, malformed/oversized input, unsupported formats, bad sequence selection, tick precision/overflow, local-versus-network reference preflight, and both image-sequence URL alternatives/custom protocols/encoded controls.

Verified on 2026-10-08: `cargo test -p ferrocut-engine --test interchange --offline` passed all 16 tests; `cargo clippy -p ferrocut-engine --lib --test interchange --offline -- -D warnings` passed. This suite needs no external Adobe application and does not establish Adobe feature parity.
