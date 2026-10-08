# Native point tracking and translation stabilization

Ferrocut now adapts the retained EffectCraft CPU point tracker and translation correction solver. The output is serializable analysis and ordinary position keyframes. It follows a seeded feature in original decoded-source pixels; one point can attach an overlay or hold that feature steady. This milestone does not establish planar, 3D, camera-solve, or Adobe feature parity.

## Provenance and algorithm boundary

Retained source: [EffectCraft `6943872cf65b3da1275f1e0f808b60b2e51d84dc`](https://github.com/storytold/effectcraft/tree/6943872cf65b3da1275f1e0f808b60b2e51d84dc), retrieved 2026-10-08; exact local vendor provenance is in [manifest.json](../../vendor/storytold/manifest.json). The upstream crates declare MIT OR Apache-2.0; their retained license/notice files remain in the vendor tree. Fixtures in Ferrocut's tracking tests are original synthetic images and locally encoded FFV1 media.

| Retained component | Source evidence | Connected and verified here |
| --- | --- | --- |
| Feature/search-region point tracker | [track/src/lib.rs](https://github.com/storytold/effectcraft/blob/6943872cf65b3da1275f1e0f808b60b2e51d84dc/crates/track/src/lib.rs) | Actual `Tracker::new/step`: normalized cross-correlation pyramid search, optional Lucas–Kanade subpixel refinement, luminance channel, fixed seed template, stop on low confidence. Rotation/scale estimation, template adaptation and extrapolation are disabled. |
| Translation correction | [track/src/stabilize.rs](https://github.com/storytold/effectcraft/blob/6943872cf65b3da1275f1e0f808b60b2e51d84dc/crates/track/src/stabilize.rs) | Actual `corrections`, `Method::Position`: Gaussian smooth-motion correction or no-motion lock to the middle sampled frame; no crop, zoom or mesh warp. |
| Source pixel conversion | [raster/src/lib.rs](https://github.com/storytold/effectcraft/blob/6943872cf65b3da1275f1e0f808b60b2e51d84dc/crates/raster/src/lib.rs) | Actual `Image::from_rgba8`; no browser, GPU, inference model or downloaded asset. |
| Broader motion fitting | [EffectCraft track](https://github.com/storytold/effectcraft/tree/6943872cf65b3da1275f1e0f808b60b2e51d84dc/crates/track/src), [FilmCraft render/track.rs](https://github.com/storytold/filmcraft/blob/5231852443363f001c3f6b396dd9b1e6461ae2be/crates/render/src/track.rs) | Source audit only: retained KLT, forward/backward checks and seeded RANSAC translation/similarity/homography primitives are candidates for later multi-point/mask/planar work. Their presence does not verify an exposed Ferrocut planar tracker or automatic stabilization. |

The adapter is [tracking.rs](../../crates/ferrocut-engine/src/tracking.rs). Root integration is [tracking_io.rs](../../crates/ferrocut-engine/src/tracking_io.rs). `ALGORITHM` pins the retained revision and adapter version so loaded results cannot silently select another implementation.

## Settings and result contract

All configuration structs reject unknown JSON fields. Numeric coordinates, confidence thresholds and sample times use Ferrocut rational strings; JSON floats are not required.

```json
{
  "start":"3/2", "fps":"30000/1001", "width":1920, "height":1080,
  "frame_count":120,
  "points":[{"id":"sign-corner","center":["960.5","540.5"],
    "feature_size":[15,15],"search_size":[47,47]}],
  "min_confidence":"80", "subpixel":true
}
```

`track_frames(settings, cancel, frame_at, progress)` accepts a callback returning tightly packed, straight-alpha RGBA8 frames. The callback receives exact source time `start + frame/fps`; the provider is responsible for source extent and identity. `track_file(path, settings, cancel, progress)` uses Ferrocut's existing CPU `Decoder`, probes video dimensions/duration, and rejects coordinate-changing resize or samples at/after the known end. File callers must apply their sandbox guard first. Only a local regular file is accepted; FFmpeg URI protocols are rejected.

These are requested source sampling times, before clip trims or retiming. Decoder time zero is the video stream's first timestamp. The native decoder chooses the nearest displayed source frame; a different sampling rate can repeat or skip source frames. No optical-flow interpolation occurs. Coordinates refer to original decoded pixels: pixel `(i,j)` occupies `[i,i+1)×[j,j+1)` with centre `(i+0.5,j+0.5)`.

Limits: 1–16 uniquely named points, 1–2400 requested frames, 16–4096 pixels per side, at most 8,388,608 pixels, positive sampling rate at most 240 fps, samples within the first seven source days, feature sides 5–63 pixels and search sides `feature+4` through 191 pixels. Upstream rounds half the feature size to its patch radius; the full rounded patch plus a one-pixel sampling margin must fit inside the source. Seeds outside that margin fail validation. A tracked feature leaving it becomes an explicit failure.

`TrackingAnalysis` stores `version`, `algorithm`, `settings`, optional `source`, `completion`, `processed_frames`, `sampled_frames_hash` and `tracks`. Each track has `id`, `outcome` and sequential samples with `frame`, exact `source_time`, `status`, optional `position`, optional `confidence`, and optional `failure`.

| Status/outcome | Meaning |
| --- | --- |
| `seeded` | Caller-supplied position on frame zero; confidence is absent because this is not a measured match. |
| `tracked` | Actual upstream match with measured position and correlation confidence. Spatial estimates and confidence are rounded to six decimals; timestamps remain exact. Confidence validation allows half one rounding quantum at the stop threshold. |
| `failed` | No position is emitted. Reasons are `no_texture`, `low_confidence`, `out_of_bounds`, or `non_finite`. Low-confidence failure preserves the measured confidence; textureless seeds have none. |
| `inactive` | This point was already lost; no position or confidence is fabricated while other points continue. The reason is `already_lost`. |
| Track `tracked` / `seed_only` / `lost` / `cancelled` | Explicit final point outcome. A single seed does not establish a successful track. |
| Analysis `completed` / `seed_only` / `all_points_lost` / `cancelled` | `completed` means the requested frame range was processed with at least one surviving point; individual lost tracks remain visible. The unprocessed tail is absent and its extent is explicit in `processed_frames`. |

`TrackingAnalysis::validate()` checks strict identity, dimensions/settings, exact sequential sample times and indices, committed counts, ordered point IDs, bounded positions/confidence, and consistent status/outcome/completion history. It rejects forged success with missing samples, confidence on a seed, or a tracked sample after loss. This is consistency validation; editable JSON and a hash are not signed authenticity proofs.

## Provenance, cancellation and edits

For file analysis, `TrackingSource` records canonicalized caller-resolved `path`, original `width/height`, source `fps`, usable `duration`, and `full_file_hash`. `hash_source(path,cancel)` streams Blake3 in 1 MiB chunks with cancellation and regular-file checks. The file is hashed before probing/decoding and after tracking; a changed digest rejects the analysis. The guarded keyframe path checks the current full file hash again before producing edits. `sampled_frames_hash` separately hashes dimensions, committed exact sample times and actual consumed RGBA pixels. It describes consumed pixels, not the entire media file.

The pure callback API checks cancellation before/after frame retrieval and between point calls; it commits a whole frame atomically and returns partial `cancelled` results with verified samples. Progress reports `completed_frames`, `total_frames`, exact `source_time`, and `active_points`. An individual FFmpeg read, image conversion or bounded upstream point step is not interruptible. File analysis returns an explicit cancellation error if cancellation prevents the required full-file provenance checks. No timeline is modified by either analysis function.

`stabilization(analysis, point_id, {mode:"smooth"|"lock", smoothness:"0".."100"})` requires a completed analysis with at least two frames and a selected point whose entire history is seeded then measured. It refuses lost, inactive, incomplete, cancelled or changed-time samples. It calls upstream translation corrections and returns exact `source_time` keys with source-pixel `offset`. Lock mode exposes `reference_time` because upstream locks to the middle sampled frame. Smooth mode reduces the selected point's jitter; a moving foreground object is not automatically background camera motion. Exposed borders stay transparent, without synthetic fill or cropping.

Root's `tracking_keyframes` planner converts source sample times to source clip placement and then target clip-local key times. It scales source-pixel displacement per axis to the native output canvas, emits normal `set_keyframes` operations, and relies on `edit_apply` for reversible history, validation and cache invalidation. Constant nonzero speed including reverse is supported; nonlinear or expression-driven time mappings and geometrically altered source clips/tracks are rejected. Attach mode follows displacement from the seed relative to the target's constant starting position. Stabilize mode targets the source clip itself. Generated position keys replace the target's position channels, so the returned operations are reviewable before applying them.

## Verification and remaining limits

Focused command: `cargo test -p ferrocut-engine --test tracking --offline`. Original fixtures verify integer translation, subpixel displacement, measured confidence, blank seeds, real occlusion, one lost point alongside a surviving point, deterministic serialized output, exact NTSC source-time grids, progress/cancellation, structural budgets, malformed buffers, forged/gapped loaded results, real FFV1 decode, no silent resize, end-hold rejection, full-file provenance/change detection, and both upstream translation correction modes.

Normalized correlation is not a calibrated probability: repeated textures, lighting changes, motion blur, large jumps, depth/parallax and occlusion can produce wrong or low-confidence matches. There is no automatic recovery, template adaptation, extrapolation, multiview camera solve, planar pin/corner pin, lens correction, mesh warp, GPU tracking, tracking of already-composited effects, or rendering-performance claim in this adapter. Broader retained algorithms require separate integration and adversarial media validation.
