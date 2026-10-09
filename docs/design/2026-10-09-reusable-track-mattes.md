# Reusable track mattes and source visibility

Source implementation for AE-105/106, based on `21f9bb6`. Validation, rendered
output, installed-tool verification and independent acceptance are separate
milestones. This document does not claim those executions occurred.

## User operation

A named video track can supply the same finished picture to several recipients,
regardless of its position in the track list:

```json
[
  {"op":"set_param","track":"Panel A","param":"matte",
   "value":{"mode":"alpha","source":{"track":"Stencil"}}},
  {"op":"set_param","track":"Panel B","param":"matte",
   "value":{"mode":"luma","source":{"track":"Stencil"}}},
  {"op":"set_param","track":"Stencil","param":"visible","value":false}
]
```

These are ordinary CLI `edit` / MCP `edit_apply` operations. Their normal dry-run,
journal, atomic failure, plan/diff and undo paths apply. `timeline_schema` exposes
the source union and the fixed Boolean `visible` parameter. No renderer-specific
operation or media path is smuggled into the track reference.

`visible` defaults to true and is omitted when true on serialization. It is not
keyframed. It controls picture-stack participation, including adjustment tracks;
linked audio remains governed by its existing audio bus. Hidden tracks are still
validated, compiled and subject to normal asset/root checks. Hiding a missing or
out-of-root source does not make it valid.

## Identity and compatibility

The new source is `{"track":"Stencil"}`, a reference to an existing unique,
nonempty video track name within the same composition. Track names already serve
as edit and audio-ducking references. Inserting or reordering tracks does not
retarget named mattes. Names are case-sensitive. Audio-only tracks, empty names,
missing names, self references and cycles are errors. Compilation independently
rejects ambiguous named sources instead of selecting the first match.

Existing validation already rejects duplicate **nonempty** track names across
video/audio and permits repeated empty names. That rule is unchanged. No rename
or delete operation is introduced: unsupported edit operations are rejected;
changing/removing a source name in a document requires updating/removing its
references. A failed typed edit changes neither document nor journal. Emptying
a source track is different from deleting it: its picture becomes transparent.

An omitted source, or `"source":"track_above"`, retains the adjacent-track
contract: the track directly above is consumed and is not composited. That source
still cannot have its own matte. Consumption remains even if its recipient is
hidden or another track references the source by name. A source's `visible:true`
does not override legacy consumption. Use an explicit named source on every
recipient when independent source visibility is wanted.

Default-visible legacy projects retain the same node operations, pull times,
matte arithmetic and key definitions; only graph allocation order may differ.
Legacy render and chunk-key equality are controls to execute, not assumptions
that replace evidence. No key-version bump or cache-format migration is needed
because this change adds graph edges/stack selection using existing node kinds.

## Picture and time contract

The source is its standalone track picture after clip sequencing, source retiming,
clip transforms/opacity, track effects and its own matte. Its blend mode against
the composition backdrop is not part of that picture. The compiler resolves
dependencies before composing visible tracks bottom to top. Each track picture
is built once and shared; normal acyclic chains are allowed.

The matte node pulls recipient and source at the **same exact composition time**.
Each track then applies its own clip-local and source-time mapping. Moving or
retiming a recipient does not implicitly move/retime the matte track. A source
gap is transparent black; inverted modes reveal the recipient there. Clip bounds
stay half-open. Track effects retain their existing timeline clock, and source
animation retains the source clock after that source clip's retiming.

The existing linear-light, premultiplied ACEScg kernels remain unchanged:

- alpha coverage is clamped source alpha;
- luma coverage is clamped AP1 luminance of premultiplied RGB (brightness times
  alpha for ordinary nonnegative premultiplied pixels);
- inverted modes use one minus that coverage;
- every premultiplied recipient channel, including alpha, is multiplied by it.

No unpremultiplication, alpha-only replacement or floating time conversion is
added. Tests include authored nonzero RGB at zero alpha, graded alpha, negative
data-window origins and ordinary gap behavior. Sources with a 3D layer use its
existing projected picture, not a new depth or lighting interpretation.

Adjustment tracks may receive a matte from a normal track, including a named
source. They cannot supply one: their output depends on the accumulated backdrop,
so allowing it would introduce order-dependent composition semantics. The existing
restriction on adjustment tracks in compositions containing 3D layers remains.

## Nested compositions and edits

References resolve inside one composition. An inner `Stencil` is independent of
an outer `Stencil`; a parent name cannot satisfy a missing inner reference. An
outer matte can use a normal track containing a nested composition; then it uses
that nested clip's finished, fitted/retimed picture. Root containment still follows
the existing recursive asset checks.

The existing `nest` operation moves selected clips and does not transfer track
matte relationships. It now rejects selected tracks that are hidden, have a matte
or supply any matte. This prevents silent picture changes. Author an explicit
nested project instead. `unnest` rejects hidden or matted inner tracks; flattening
multiple tracks also rejects a parent that is hidden or participates in a matte.
Single-track expansion can retain the parent's own visibility/matte relationship.
Unrelated nesting remains available. Broader lossless track-graph migration is
outside this slice.

Foreign timeline export still reports matte/effect loss. Hidden video tracks add
an explicit visibility-loss report; this slice does not claim interchange support
for reusable mattes. Import constructors retain the prior visible default.

## Verification and visible demonstration

Focused tests and the original example live at:

- `crates/ferrocut-engine/tests/reusable_mattes.rs`;
- `crates/ferrocut-mcp/tests/reusable_mattes.rs`;
- `examples/reusable-mattes/`.

Required controls cover names/ambiguity/cycles, legacy adjacency, one source
feeding two recipients, visibility independent of availability/audio, exact
retiming and gap boundaries, nested scope, adjustment recipients, root containment,
schema discovery, edit/undo and affected frame/chunk keys. GPU picture controls
must skip explicitly without an adapter; a skip does not establish pixel behavior.

The demonstration uses original generated grayscale/alpha artwork, two animated
colored panels and a third visible stencil region. The alpha and luma phases
must differ; hiding the stencil must remove only its visible presentation while
both cutouts continue. A separate legacy adjacent control must remain equal.
See the example README for frame checkpoints and reversible `.ops` files.

Later execution needs one shared slot: focused engine/MCP tests, required scoped
fmt/Clippy/full tests, exact release CLI/MCP discovery and edit/undo/plan, bounded
before/after masters and unchanged `ci/render-check.sh`. Start with `-j4` build
jobs and at most two render workers, preserving all exits, binary/source/output
hashes and prior failed attempts. No shared install is owned by this branch.

The calling image-capable agent must actually display the native previews and
decoded final output, inspect dense ordered boundary frames and full-resolution
details, and record timecoded observations. Native frame keys, timeline snapshots,
PNG hashes and encoded artifact identities must remain distinguishable. File paths,
successful encoding and a server-produced image block alone are not visual review.
Ordered still inspection is not continuous playback or listening. Independent
review owns acceptance after the maker's artifact and exact evidence handoff.
