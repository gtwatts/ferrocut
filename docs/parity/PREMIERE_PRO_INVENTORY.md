# Adobe Premiere feature inventory for Ferrocut

Research snapshot: **2026-10-08**. Product scope: Premiere desktop, historically named Premiere Pro. This document is a requirements inventory, **not a claim of Ferrocut implementation or Adobe parity**. The proposed acceptance checks are engineering recommendations inferred from the documented capabilities; they are not Adobe certifications.

## Version and evidence baseline

- Adobe's canonical [release notes][S01], retrieved on 2026-10-08, were last updated **2026-10-05**. The newest listed stable release is **26.5.2, September 2026**; the newest feature release is **26.5**. Confidence is high for this documentation baseline. Adobe was not installed or executed in this research, so platform-specific availability, account entitlements and exact binary behavior remain untested.
- The [desktop guide navigation][S00] and [what's-new page][S02] were reviewed across setup, organization, editing, graphics, effects, audio, color, delivery, collaboration, integrations and troubleshooting. Deep articles are linked below. Longstanding capabilities are included alongside recent additions.
- `GA-doc` means the current desktop guide documents the capability without an explicit beta restriction. `GA-release` means stable release notes also explicitly identify it. Neither label implies a local execution test. `Beta` remains outside the stable baseline.
- Adobe's Object Mask article has a stale beta title, but stable 26.0 release notes explicitly ship base object masking. It is classified as `GA-release`; future refinements still need per-version checks. The new Color mode, Generate Music and Premiere AI Assistant are explicitly beta.
- The newer Generative Extend overview says generated extensions remain **8-bit SDR**, and sources above 30 fps receive 30 fps extensions. An older FAQ contains partly inconsistent wording. Prefer the September overview for current input/output distinctions; do not mistake broader accepted sources for native HDR generation.
- Frame.io help distinguishes Legacy and V4 panels; V4 accounts cannot use the Legacy panel. Those are service integrations, not native-core editing functions.

## How to use this checklist

Each row is one independently addressable capability. Preserve IDs when refining requirements; add child requirements or new IDs rather than renumbering. `native` means a bundled desktop capability; it does not mean Adobe is free. `ui-equivalent` marks a desktop interaction whose agent equivalent should expose the same information or edit semantics through structured tools. `ecosystem` requires another app, codec/plug-in, hardware or external system. `cloud` requires a hosted service; `/paid` additionally flags credits, licensing or premium entitlement documented by Adobe. Some rows combine classifications because both components matter.

The table deliberately has **no Ferrocut status column**. Assign implementation status only after tracing schema/commands, rendering or media execution, automated validation and a representative agent workflow. An API accepting a parameter is insufficient evidence that the corresponding feature works.

Acceptance checks assume stable asset/clip/track IDs, explicit time units, an undoable project edit, readable errors, and persisted results. Tests should include a successful use, a relevant failure or boundary case, and a rendered or exported result where applicable. Visual and listening review remains necessary for subjective quality.

## Project, ingest, media and organization

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-001 | Project | Create, reopen and duplicate projects | native | GA-doc | Project state and media references are distinct from source files. | [Project templates][S03] | Save and reopen a multi-sequence project; duplicate it without changing the original. |
| PP-002 | Project | Reusable project templates | native | GA-doc | Templates carry bins, track configuration, labels and effects. | [Project templates][S03] | Instantiate a template twice with independent project IDs and matching defaults. |
| PP-003 | Organization | Hierarchical bins and media organization | native/ui-equivalent | GA-doc | Reusable organizational structure is part of the project. | [Project templates][S03] | Create nested bins, move assets and enumerate them without losing clip references. |
| PP-004 | Organization | Metadata/search bins and labels | native/ui-equivalent | GA-doc | Saved organization can be reused in project templates. | [Project templates][S03] | Query tagged assets and persist an automatically updating saved search. |
| PP-005 | Ingest | Native audiovisual format import | native/ecosystem | GA-doc | Container recognition does not guarantee its codec can be decoded. | [Supported formats][S04] | Probe and decode supported codec/container fixtures; report unsupported streams precisely. |
| PP-006 | Ingest | Professional RAW and camera media | native/ecosystem | GA-release | Support varies by camera, SDK, platform and release; 26.5 adds camera workflows. | [Release notes][S01] | Publish a tested camera/codec matrix and preserve camera metadata on sample ingest. |
| PP-007 | Ingest | Large still images | native | GA-doc | Adobe documents size limits; scaling needs adequate source resolution. | [Still images][S05] | Import a large image, preserve dimensions/alpha and reject declared limit violations. |
| PP-008 | Ingest | Numbered image sequences | native | GA-doc | A series of stills can become temporal media. | [Still images][S05] | Import a numbered sequence at an explicit rate; identify missing frames. |
| PP-009 | Ingest | Layered Photoshop import | native/ecosystem | GA-doc | Selected layers can be clips, a sequence or a merged image. | [Layered assets][S06] | Import a layered fixture with named layers and correct transparency/placement. |
| PP-010 | Ingest | Illustrator artwork import | native/ecosystem | GA-doc | Import support does not imply every source-app feature survives. | [Layered assets][S06] | Import a supported vector-art fixture and document rasterization and unsupported attributes. |
| PP-011 | Ingest | Verified media copy | native | GA-doc | Ingest copies source media with verification. | [Ingest/proxy][S07] | Copy a media set, verify hashes, and fail visibly on a corrupt copy. |
| PP-012 | Ingest | Transcode during ingest | native/ecosystem | GA-doc | Working copies can use easier-to-edit codecs. | [Ingest/proxy][S07] | Batch transcode with explicit codec settings while preserving source identity/timecode. |
| PP-013 | Proxy | Generate lightweight proxies | native/ecosystem | GA-doc | Editing media can differ from final full-resolution media. | [Ingest/proxy][S07] | Generate proxies, link them to sources and show job progress and failures. |
| PP-014 | Proxy | Attach, detach and reconnect proxies | native | GA-doc | The guide distinguishes proxy and full-resolution relationships. | [Attach proxies][S143] | Swap proxy associations without altering edits or source identity. |
| PP-015 | Proxy | Full-resolution conform after proxy editing | native | GA-doc | Final media substitution must retain timing and geometry. | [Ingest/proxy][S07] | Edit on proxies, restore originals and render the same cuts at full resolution. |
| PP-016 | Relink | Offline media inspection | native/ui-equivalent | GA-doc | Missing links retain filenames, clip names and previous paths. | [Offline files][S08] | Open a moved project and list all missing sources with their previous identities. |
| PP-017 | Relink | Metadata-assisted batch relink | native | GA-doc | File name, media start, tape and descriptive metadata can disambiguate. | [Offline files][S08] | Relink a relocated tree, reject ambiguous matches and preserve edit offsets. |
| PP-018 | Archive | Consolidate used media with handles | native/ecosystem | GA-doc | Project Manager can trim/transcode selected sequences and exclude unused media. | [Consolidate/archive][S09] | Package only used ranges plus specified handles and reopen without original paths. |
| PP-019 | Archive | Portable mezzanine project package | native/ecosystem | GA-doc | Consolidation codec and sequence selection are configurable. | [Consolidate/archive][S09] | Archive a project with a manifest and verify renders from the relocated package. |
| PP-020 | Metadata | Read and edit technical/descriptive metadata | native | GA-doc | Metadata includes acquisition facts and production/rights notes. | [Metadata][S10] | Read and update named fields while distinguishing read-only technical values. |
| PP-021 | Metadata | XMP and cross-application metadata | native/ecosystem | GA-doc | Metadata can travel with assets across production applications. | [Metadata][S10] | Round-trip supported XMP fields and preserve unknown fields. |
| PP-022 | Timecode | Source and sequence timecode | native | GA-doc | Frame identification and synchronization need a shared timebase. | [Timecode][S11] | Convert frame/timecode values at rational rates without cumulative drift. |
| PP-023 | Markers | Clip and sequence annotations | native/ui-equivalent | GA-doc | Markers can carry comments and timed ranges. | [Markers][S12] | Create, edit, find and export point/range annotations by stable IDs. |
| PP-024 | Markers | Chapter and segmentation metadata | native/ecosystem | GA-doc | Some marker types are output/workflow specific; legacy Flash is not a target. | [Markers][S12] | Preserve supported chapter/segment ranges and explicitly report unsupported delivery mappings. |
| PP-025 | Search | Semantic visual shot search | native/model | GA-doc | Returns matching temporal ranges; documented FAQ excludes face identity and OCR. | [Media intelligence][S13] | Retrieve ranked ranges for descriptive queries and expose confidence/provenance. |
| PP-026 | Search | Transcript and metadata search | native | GA-doc | Dialogue search depends on transcription; metadata search differs from visual semantics. | [Media intelligence][S13] | Query dialogue and metadata separately with stable asset IDs and usable ranges. |
| PP-027 | Inspection | Sequence Index and QC filtering | native/ui-equivalent | GA-release | Searchable clip table surfaces effects, transitions, offline media and flash frames; CSV export. | [Sequence Index][S14] | Export a queryable sequence index and locate deliberately injected offline/flash-frame problems. |

## Timeline editing, trimming, timing and multicamera

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-028 | Timeline | Explicit sequence configuration | native | GA-doc | Timebase, frame geometry and preview settings are separate from delivery. | [Sequence settings][S15] | Create sequences at rational rates and differing dimensions; serialize settings exactly. |
| PP-029 | Timeline | Multiple video/audio tracks | native | GA-doc | Tracks are separately controllable structural elements. | [Add tracks][S144] | Add, rename, reorder and remove tracks without corrupting clip references. |
| PP-030 | Timeline | Insert editing | native | GA-doc | Insertion creates space for source content. | [Add/remove clips][S16] | Insert a source range at a frame and shift the defined affected tracks. |
| PP-031 | Timeline | Overwrite editing | native | GA-doc | Overwrite replaces the addressed timeline region. | [Add/remove clips][S16] | Overwrite a range, retain surrounding media and report replaced clip portions. |
| PP-032 | Timeline | Source ranges and audio/video selection | native/ui-equivalent | GA-doc | Source Monitor prepares In/Out and source-track selection. | [Source/Program monitors][S17] | Insert only requested source streams and expose selected source ranges. |
| PP-033 | Timeline | Source patching and track targeting | native/ui-equivalent | GA-doc | Import routing and edit targeting are distinct operations. | [Source patching][S145] | Address destination tracks explicitly and reject incompatible stream routing. |
| PP-034 | Timeline | Move, nudge and rearrange clips | native | GA-doc | Position changes can be exact rather than mouse-dependent. | [Move clips][S18] | Move a group by an exact frame delta with declared collision handling. |
| PP-035 | Timeline | Lift/delete and ripple removal | native | GA-doc | Removal can leave a gap or close it. | [Add/remove clips][S16] | Delete identical ranges in lift and ripple modes and verify downstream timing. |
| PP-036 | Trim | Ordinary edge trims | native | GA-doc | Adjust source boundaries without implicit ripple. | [Trim a clip][S19] | Trim either edge within source bounds and preserve unaddressed clips. |
| PP-037 | Trim | Ripple trim | native | GA-doc | Downstream content shifts; marker behavior has its own setting. | [Ripple edits][S20] | Ripple a boundary and verify selected tracks, markers and resulting duration. |
| PP-038 | Trim | Rolling trim | native | GA-doc | Moves a shared cut while keeping combined duration. | [Rolling edit][S21] | Move a shared cut within both sources' handles; preserve outer bounds. |
| PP-039 | Trim | Slip edit | native | GA-doc | Source content shifts within fixed timeline boundaries. | [Trim guide navigation][S00] | Change source In/Out equally and keep the timeline range unchanged. |
| PP-040 | Trim | Slide edit | native | GA-doc | A clip moves while adjacent boundaries absorb the change. | [Trim guide navigation][S00] | Slide a middle clip within handles and preserve total sequence duration. |
| PP-041 | Trim | J cuts | native | GA-doc | Incoming audio precedes its picture cut. | [J/L cuts][S22] | Create a specified audio lead and verify linked media alignment after later trims. |
| PP-042 | Trim | L cuts | native | GA-doc | Outgoing audio continues past its picture cut. | [J/L cuts][S22] | Create a specified audio tail without shifting picture timing. |
| PP-043 | Trim | Dynamic/asymmetrical multi-track trimming | native/ui-equivalent | GA-doc | Trim mode supports operations across different selected edit sides. | [Asymmetrical trimming][S146] | Submit a multi-boundary trim and preview the resulting edit atomically. |
| PP-044 | Safety | Sync-lock policy | native | GA-doc | Determines which non-primary tracks participate in ripple operations. | [Sync Lock][S23] | Ripple one track with mixed sync-lock states and verify each affected interval. |
| PP-045 | Safety | Track locking | native | GA-doc | Locked content remains visible/audible in export but cannot be edited. | [Track Lock][S24] | Reject mutations on a locked track while rendering it normally. |
| PP-046 | Timeline | Grouping and linked selection | native | GA-doc | Group membership differs from audiovisual synchronization. | [Group clips][S147] | Move grouped items coherently; independently unlink picture/sound without losing sync metadata. |
| PP-047 | Timeline | Subclips and adjustable source bounds | native | GA-doc | Subclips reference a bounded portion of source media. | [Subclips][S148] | Create named source ranges and extend only when source-bound policy allows. |
| PP-048 | Timeline | Nested reusable sequences | native | GA-doc | Source changes update instances; instance effects remain distinct. | [Nested sequences][S25] | Reuse a sequence twice, alter its source and preserve per-instance processing. |
| PP-049 | Media interpretation | Frame-rate, PAR, field and alpha interpretation | native | GA-doc | Interpretation changes media meaning without rewriting its source. | [Interpret Footage][S26] | Override each supported interpretation and render a known reference correctly. |
| PP-050 | Retime | Constant speed, reverse and rate stretch | native | GA-doc | Speed/duration, rate-stretch and remapping are distinct controls. | [Speed/duration][S27] | Fit a clip to a target duration and test reverse, audio policy and source bounds. |
| PP-051 | Retime | Variable speed ramps and frame holds | native | GA-doc | Time remapping changes source-time progression. | [Speed/duration][S27] | Render a continuous ramp plus hold with predictable endpoints and audio behavior. |
| PP-052 | Retime | Frame sampling and frame blending | native | GA-doc | Sampling repeats/drops; blending mixes neighboring frames. | [Time interpolation][S28] | Render the same retime in both modes against distinct reference expectations. |
| PP-053 | Retime | Optical-flow interpolation | native/model | GA-doc | Motion estimation synthesizes intermediate frames and needs artifact review. | [Time interpolation][S28] | Slow a moving subject; inspect occlusions and expose analysis/progress errors. |
| PP-054 | Multicam | Synchronize multiple cameras | native/model | GA-doc | Supports timecode, audio, In/Out and markers; gaps need explicit policy. | [Multicam sources][S29] | Align multiple cameras plus external sound using selectable sync methods. |
| PP-055 | Multicam | Switch and revise camera-angle edits | native/ui-equivalent | GA-doc | Multicam views show synchronized sources for angle selection. | [Multicam target][S30] | Create angle decisions by time range, revise one choice and preserve sound sync. |

## Transcription and dialogue-driven editorial

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-056 | Transcript | Source/sequence speech transcription | native/model | GA-doc | Spoken dialogue and language packs are prerequisites. | [Speech to Text][S31] | Produce timestamped words/segments and persist model/language provenance. |
| PP-057 | Transcript | Speaker labels | native/model | GA-doc | Transcription can separate speaker labels. | [Speech to Text][S31] | Label and rename speakers while preserving all word timestamps. |
| PP-058 | Transcript | Select audio source and transcription range | native | GA-doc | Supports chosen tracks/dialogue and In/Out transcription. | [Speech to Text][S31] | Transcribe only a selected channel/range and map timestamps to source and sequence. |
| PP-059 | Transcript | Transcript-synchronized sequence editing | native | GA-doc | Text edits can alter timeline content with synchronized metadata. | [Text-Based Editing][S32] | Delete/reorder word ranges and verify linked picture/sound and refreshed transcript. |
| PP-060 | Transcript | Dialogue search and navigation | native/ui-equivalent | GA-doc | Text matches identify source/timeline moments. | [Text-Based Editing][S32] | Search a phrase, return all occurrences and preview the chosen range. |
| PP-061 | Transcript | Bulk pause removal | native/model | GA-doc | Pauses can be found and removed through the transcript. | [Detect/delete pauses][S149] | Remove pauses over a threshold with explicit padding and a reversible diff. |
| PP-062 | Transcript | Speaker-selective removal | native | GA-doc | Guide documents removing all instances of a speaker. | [Remove speaker instances][S150] | Remove one speaker's selected contributions and retain remaining timing coherently. |
| PP-063 | Transcript | Paper Edit assemblies | native/ui-equivalent | GA-release | Select discontinuous sentences/parts and preview before sequence creation. | [Paper Edit][S33] | Assemble multiple transcript selections into a new editable sequence with source links. |
| PP-064 | Transcript | Editable transcript text and export | native | GA-doc | Spelling corrections must remain distinct from media edits. | [Text-Based Editing][S32] | Correct a word without cutting media; export a time-aligned transcript. |
| PP-065 | Analysis | Scene edit detection | native/model | GA-doc | Can create cuts, source subclips or markers from detected boundaries. | [Scene Edit Detection][S34] | Analyze a known cut montage and emit selectable markers/cuts with reviewable results. |
| PP-066 | Dialogue | Bulk mute or bleep | native/model | GA-doc | Uses transcript word lists with import/export and selectable censorship treatment. | [Bulk mute/bleep][S35] | Censor all selected word occurrences with bounded fades while preserving the original source. |

## Typography, shapes, graphics templates and captions

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-067 | Typography | Editable native title layers | native | GA-doc | Titles remain text with font, appearance and alignment controls. | [Create titles][S36] | Create, edit and render native title text without baked image substitution. |
| PP-068 | Typography | Point and paragraph text | native | GA-doc | Text can begin at a point or wrap in a box. | [Color fonts][S37] | Render point text and constrained multiline text with stable line breaks. |
| PP-069 | Typography | Font/size/style/alignment controls | native | GA-doc | Text styling is editable after creation. | [Create titles][S36] | Change font, size and alignment and return measured text bounds. |
| PP-070 | Typography | Fill, multiple strokes and shadows | native | GA-doc | Stroke alignment and layered appearances are supported. | [Text styles][S38] | Render styled glyph edges and multiple shadows against pixel references. |
| PP-071 | Typography | Reusable text styles | native | GA-doc | Styles apply across graphic layers. | [Text styles][S38] | Save and reapply a named style, preserving explicitly overridden properties. |
| PP-072 | Typography | Linked title and caption-track styles | native | GA-doc | Styles can be saved in project or locally for reuse. | [Linked/track styles][S39] | Update a shared style and propagate to linked titles/captions with undo. |
| PP-073 | Typography | Project-wide font replacement | native/ecosystem | GA-doc | Replacement requires installed or activated fonts. | [Replace fonts][S40] | List dependencies, substitute a font across the project and report missing glyphs. |
| PP-074 | Typography | Color-font rendering | native/ecosystem | GA-doc | Font format/platform support matters. | [Color fonts][S37] | Render supported color glyph fixtures and publish supported font formats. |
| PP-075 | Typography | Emoji text | native/ecosystem | GA-doc | Adobe's documented picker workflow is macOS-specific. | [Emojis][S41] | Resolve emoji sequences deterministically and report font/platform dependencies. |
| PP-076 | Shapes | Straight/Bezier shape paths | native | GA-doc | Vertices and tangent handles define editable curves. | [Pen Tool][S42] | Create an editable closed Bezier path and render fill/stroke at multiple scales. |
| PP-077 | Graphics | Layer grouping, ordering and alignment | native/ui-equivalent | GA-doc | Graphics consist of separately editable layers. | [Group graphic layers][S151] | Group and distribute graphics using exact bounds and retain layer IDs. |
| PP-078 | Graphics | Responsive spatial constraints | native | GA-doc | Layers pin to frame edges or other layers. | [Responsive graphics][S43] | Resize text and frame aspect ratio while maintaining a responsive lower-third background. |
| PP-079 | Graphics | Protected intro/outro animation regions | native | GA-doc | Duration changes preserve protected ends and stretch the middle. | [Protected animation][S44] | Lengthen a title while keeping entrance/exit timing and keyframes intact. |
| PP-080 | Graphics | Source graphics shared across instances | native | GA-doc | Text, style and contents propagate across instances. | [Source graphics][S45] | Update a reusable source graphic and all its instances without duplicating media. |
| PP-081 | Templates | Install, browse and customize motion templates | native/ecosystem | GA-doc | MOGRT may originate in Premiere or After Effects; exposed controls limit customization. | [Motion templates][S46] | Load a native equivalent template, discover parameter schemas and render an edited instance. |
| PP-082 | Templates | Export reusable motion templates | native/ecosystem | GA-doc | Template packaging is distinct from flattening a render. | [Motion templates][S46] | Export and reopen an editable template with assets and exposed parameters. |
| PP-083 | Templates | Data-driven graphics | native/ecosystem | GA-doc | Author-defined text, color and numeric column types are fixed. | [Data-driven templates][S47] | Feed a table into a chart template and validate schema/type mismatches. |
| PP-084 | Templates | Replace media inside a graphic template | native/ecosystem | GA-doc | Replacement slots preserve template structure. | [Media replacement][S48] | Replace a logo/footage slot and keep animation/layout relationships. |
| PP-085 | Captions | Transcript-to-caption segmentation | native/model | GA-doc | Character length, duration, gaps and line count are configurable. | [Create captions][S49] | Generate editable caption cues meeting requested timing/line-length policies. |
| PP-086 | Captions | Single-word caption layout | native/model | GA-release | One-word cues align with spoken audio. | [Single-word captions][S50] | Generate word-synchronized cues and reflow them after an editorial change. |
| PP-087 | Captions | Caption styles and layout | native | GA-doc | Captions use reusable track styles and format-specific controls. | [Linked/track styles][S39] | Restyle a caption track consistently and preserve cue timing/content. |
| PP-088 | Captions | Caption sidecar import | native/ecosystem | GA-doc | Adobe lists SRT, SCC, MCC, STL and XML subtitle families. | [Caption formats][S51] | Import tested subtitle formats and report unsupported styling or stream data. |
| PP-089 | Captions | Caption translation | native/model | GA-doc | Translations require review; language coverage is separately documented. | [Translate captions][S52] | Create a separate translated track, preserve alignment and allow text corrections. |
| PP-090 | Captions | Burn-in, sidecar and embedded delivery | native/ecosystem | GA-doc | Embedded support depends on container/format; outputs are distinct. | [Caption export][S53] | Deliver styled burn-in and sidecar captions; verify embedded captions for declared formats. |

## Audio editing, mixing and restoration

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-091 | Audio | Mono, stereo, adaptive and 5.1 tracks | native | GA-doc | Track channel layouts are explicit. | [Audio concepts][S54] | Route and render mono/stereo/surround fixtures with correct channel counts. |
| PP-092 | Audio | Source channel remapping | native | GA-doc | Source channels can be enabled/disabled and assigned to clip channels. | [Channel mapping][S55] | Select/remap channels from a multichannel recording with no unintended downmix. |
| PP-093 | Audio | Waveform inspection | native/ui-equivalent | GA-doc | Waveforms expose amplitude over time. | [Audio concepts][S54] | Return multichannel waveform envelopes at requested resolution and time range. |
| PP-094 | Audio | Clip gain and volume envelopes | native | GA-doc | Static gain and animated levels serve different roles. | [Audio concepts][S54] | Apply gain plus an envelope and measure expected sample-level amplitude. |
| PP-095 | Audio | Track mixer levels, pan, mute and solo | native/ui-equivalent | GA-doc | Track processing differs from clip processing. | [Audio Track Mixer][S56] | Mix several tracks with independent controls and verify the summed output. |
| PP-096 | Audio | Submix buses and sends | native | GA-doc | Shared routing is documented separately from per-clip effects. | [Audio submix][S152] | Route multiple tracks into a bus, apply bus processing and reject cycles. |
| PP-097 | Audio | Track/clip effects with ordered processing | native | GA-doc | Effects can operate on entire tracks or individual clips. | [Audio Track Mixer][S56] | Process a clip and its bus in a declared order and expose the routing graph. |
| PP-098 | Audio | Recorded automation modes | native/ui-equivalent | GA-doc | Track automation supports Read, Write, Touch, Latch and Off semantics. | [Track automation][S57] | Record control changes and replay envelopes with documented mode behavior. |
| PP-099 | Audio | Clip mixer Touch/Latch automation | native/ui-equivalent | GA-doc | Clip automation applies to clips under the playhead. | [Clip automation][S58] | Capture a clip-level mix pass without altering track-level automation. |
| PP-100 | Audio | Audio crossfade curves | native | GA-doc | Constant Gain, Constant Power and Exponential Fade have different curves. | [Audio crossfades][S59] | Render each fade law and test endpoints and midpoint power behavior. |
| PP-101 | Audio | Dialogue/music/SFX/ambience classification | native/model | GA-doc | Essential Sound exposes controls based on clip type. | [Essential Sound][S60] | Assign and inspect audio roles with reversible manual overrides. |
| PP-102 | Audio | Loudness matching | native | GA-doc | Essential Sound unifies recordings to a common loudness. | [Essential Sound][S60] | Normalize dialogue groups to a specified target without unexpected clipping. |
| PP-103 | Audio | Automatic ducking | native/model | GA-doc | Ducking follows the selected controlling audio role. | [Automatic ducking][S153] | Generate editable music gain envelopes from dialogue with explicit attack/release. |
| PP-104 | Audio | Speech enhancement | native/model | GA-doc | Background processing improves dialogue; model/hardware behavior needs profiling. | [Enhance Speech][S61] | Enhance noisy speech, preserve the original and compare intelligibility and artifacts. |
| PP-105 | Audio | Dialogue repair: noise, hum, reverb and clarity | native | GA-doc | Repair, compression and EQ are available in dialogue workflows. | [Essential Sound][S60] | Apply parameterized repair to reference recordings and evaluate artifacts by listening. |
| PP-106 | Audio | Equalization and filters | native | GA-doc | Frequency shaping is a distinct audio-effects family. | [Audio effects][S62] | Verify filter frequency response and automate supported parameters. |
| PP-107 | Audio | Compression, limiting and dynamics | native | GA-doc | Amplitude processing affects level and dynamic range. | [Audio effects][S62] | Measure threshold/ratio/ceiling behavior on a calibrated signal. |
| PP-108 | Audio | Delay, modulation, reverb and stereo imaging | native | GA-doc | Creative sound treatments complement editorial mixing. | [Audio effects][S62] | Render representative effects and verify tails, channel routing and bypass. |
| PP-109 | Audio | Loudness and true delivery metering | native | GA-doc | ITU-based metering supports mix, track and bus analysis with destination presets. | [Loudness Meter][S63] | Report integrated loudness, loudness range and peaks on a known program fixture. |
| PP-110 | Audio | Music remix to target duration | native/model | GA-doc | Structural edits preserve beginning/end; target duration is approximate. | [Remix][S64] | Fit music via editable segment choices/crossfades and report actual resulting duration. |
| PP-111 | Audio | Voice-over recording with In/Out and preroll | native/ecosystem | GA-doc | Recording depends on audio input hardware and controlled range. | [Voice-over][S65] | Record into an addressed track/range with preroll and preserve original takes. |
| PP-112 | Audio | Global monitoring mute | native/ui-equivalent | GA-release | Monitoring mute does not change clip/track settings. | [Release notes][S01] | Mute preview output, then export the same audible mix without changing project levels. |

## Effects, transforms, compositing, masks and motion

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-113 | Effects | Discover and apply parameterized effects | native/ui-equivalent | GA-doc | Effects are searchable and adjustable per clip. | [Apply effects][S66] | Discover supported effect schemas, apply one and inspect resolved parameters. |
| PP-114 | Effects | Reorder, copy, bypass and remove effect chains | native | GA-doc | Effect order and enabled state are part of the edit. | [Copy/paste effects][S154] | Copy an ordered chain and verify output changes when order/bypass changes. |
| PP-115 | Effects | Reusable presets containing keyframes | native | GA-doc | Presets persist values and animation. | [Effect presets][S67] | Save a chain/animation preset, apply it to a new clip and verify timing policy. |
| PP-116 | Effects | Adjustment layers | native | GA-doc | A layer processes the composite beneath its timeline interval. | [Adjustment layers][S68] | Apply a grade across lower tracks and verify unaffected upper/outside content. |
| PP-117 | Effects | Source-level processing shared by instances | native | GA-doc | Source effects propagate to sequence instances. | [Source effects][S69] | Change a source effect and update all instances while preserving local effects. |
| PP-118 | Transform | Position, scale, rotation and anchor | native | GA-doc | Fixed Motion controls operate on timeline clips. | [Motion][S70] | Animate a clip around an explicit anchor and verify transforms numerically. |
| PP-119 | Transform | Vector motion without premature rasterization | native | GA-doc | Vector graphic transforms retain scalable detail. | [Vector Motion][S71] | Scale/animate vector titles without source-resolution pixelation or unwanted clipping. |
| PP-120 | Animation | Keyframed visual and audio properties | native | GA-doc | Properties change over time using keyframes. | [Keyframes][S72] | Add/update/delete keys and reproduce sampled values after save/reopen. |
| PP-121 | Animation | Temporal and spatial interpolation | native | GA-doc | Interpolation controls values/motion between keys. | [Interpolation][S73] | Expose linear/hold/Bezier equivalents with inspectable velocity and path results. |
| PP-122 | Compositing | Opacity and blend-mode compositing | native | GA-doc | Layer blend operations have distinct color/alpha semantics. | [Blend modes][S74] | Verify representative darken/lighten/contrast/component modes on known colors and alpha. |
| PP-123 | Keying | Chroma key with matte refinement | native | GA-doc | Ultra Key removes a sampled background and offers matte cleanup/spill control. | [Ultra Key][S75] | Key a green-screen fixture with editable cleanup and inspect alpha/spill artifacts. |
| PP-124 | Masks | Rectangle, ellipse and custom vector masks | native | GA-release | Shape tools and editable paths can drive effect isolation. | [Mask properties][S76] | Create and animate shape/path masks and apply them to selected effects. |
| PP-125 | Masks | Feather, expansion, opacity and inversion | native | GA-doc | Mask refinement depends partly on the associated effect. | [Mask properties][S76] | Render each refinement independently against alpha references. |
| PP-126 | Masks | Combine and reuse masks | native | GA-doc | Object/vector masks share refinement and combination controls. | [Combine masks][S77] | Compose add/subtract/intersect equivalents and reuse masks across compatible effects. |
| PP-127 | Masks | Bidirectional and per-frame tracking | native/model | GA-doc | Tracking follows a defined subject and can require correction. | [Track masks][S78] | Track both directions, edit a failed frame and preserve corrected results. |
| PP-128 | Masks | AI object/person isolation | native/model | GA-release | Base feature shipped in stable 26.0 despite stale article title. | [Object Mask][S79], [release notes][S01] | Select a subject, generate temporal alpha and allow explicit include/exclude corrections. |
| PP-129 | Masks | Object-mask edge quality/smoothness | native/model | GA-release | Sharp/Smooth and later smoothing refinements affect edge quality. | [Object Mask][S79] | Compare hard and soft subject boundaries, including motion and fine detail. |
| PP-130 | Masks | HSL and luminance masks | native | GA-doc | These masks can feed effects, unlike grading-only HSL Secondary. | [Range masks][S80] | Generate reusable mattes from selected color/luminance ranges and refine thresholds. |
| PP-131 | Analysis | Automatic reframing across aspect ratios | native/model | GA-doc | Can process clips or sequences and specify target resolution. | [Auto Reframe][S81] | Produce an editable vertical/square crop path and review subject retention. |
| PP-132 | Analysis | Stabilization and border handling | native/model | GA-doc | Motion analysis is separate from later transforms; crop/border decisions matter. | [Warp Stabilizer][S82] | Stabilize shaky footage, report crop and allow smooth/no-motion and border controls. |
| PP-133 | Transitions | Cut-centered or offset transition alignment | native | GA-doc | Duration and placement need not be symmetric around the edit. | [Transition alignment][S83] | Place one-sided and offset transitions with exact start/end frames. |
| PP-134 | Transitions | Handle validation and insufficient-media fallback | native | GA-doc | Adobe warns and repeats end frames when source handles are insufficient. | [Clip handles][S84] | Diagnose unavailable handles and use an explicitly selected fallback policy. |
| PP-135 | Transitions | Dissolves, wipes and directional moves | native | GA-doc | Multiple transition families expose timing and directional controls. | [Modern transitions][S85] | Render a dissolve, linear wipe and push with independently specified parameters. |
| PP-136 | Transitions | Shape dissolve and geometric transitions | native | GA-release | Shape and motion controls broaden basic wipe behavior. | [Modern transitions][S85] | Generate an editable geometric reveal and verify edge/alpha behavior. |
| PP-137 | Transitions | 3D spin/roll and motion-driven transitions | native | GA-doc | Modern transitions include spatial transforms and motion treatments. | [Modern transitions][S85] | Render a configurable spin/roll with easing and coherent outgoing/incoming timing. |
| PP-138 | Transitions | Morph Cut | native/model | GA-doc | Face tracking/optical flow works best for restrained talking-head shots. | [Morph Cut][S86] | Bridge a short interview jump cut and expose failed analysis/artifact review. |
| PP-139 | Effects library | Blur/sharpen and matte-driven blur | native | GA-doc | Modern library includes bokeh and compound blur. | [Modern effects][S87] | Render representative spatial and matte-driven blurs with bounded sampling. |
| PP-140 | Effects library | Glow, light rays, lens flare and RGB split | native | GA-doc | Bundled modern effects have editable parameters. | [Modern effects][S87] | Build and revise representative light treatments without baking intermediate images. |
| PP-141 | Effects library | Corner pin, lens distortion and magnification | native | GA-doc | Geometry and local magnification have separate controls. | [Modern effects][S87] | Place footage on four corners and create an editable magnifying callout. |
| PP-142 | Effects library | Turbulence, mirror and twirl distortion | native | GA-doc | Procedural distortions complement fixed transforms. | [Modern effects][S87] | Animate procedural distortion with repeatable parameters and inspect boundaries. |
| PP-143 | Animation presets | Text/graphic motion behaviors | native | GA-doc | Bundled modern animation effects expose reusable motion designs. | [Modern animations][S88] | Apply and retime a motion behavior while retaining editable graphics. |
| PP-144 | Compatibility | Effect migration and obsolete-effect diagnostics | native | GA-doc | 26.0 renamed, moved, deprecated and removed effects. | [Effects changes][S89] | Migrate a versioned project with an explicit unsupported/replaced-effects report. |

## Color correction, color management, scopes and HDR

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-145 | Color | White balance, exposure and contrast | native | GA-doc | Primary correction exposes independently adjustable controls. | [Basic correction][S90] | Correct a calibrated image using numeric values and compare expected pixel output. |
| PP-146 | Color | Highlights, shadows, whites, blacks and saturation | native | GA-doc | Tonal controls and chroma adjustments remain editable. | [Basic correction][S90] | Preserve reference neutrals while selectively changing defined tonal ranges. |
| PP-147 | Color | Input/custom LUT application | native/ecosystem | GA-doc | LUT correction is distinct from complete color-management configuration. | [Basic correction][S90] | Apply a validated LUT with explicit ordering and reject malformed tables. |
| PP-148 | Color | Master and per-channel RGB curves | native | GA-doc | Curves adjust luma/tonal distribution and individual channels. | [RGB curves][S91] | Evaluate control-point curves predictably and verify interpolation/clamping. |
| PP-149 | Color | Hue-versus-saturation and hue-versus-hue curves | native | GA-doc | Selected hue ranges control chroma or hue shifts. | [Hue/saturation curves][S92] | Adjust a bounded hue range with cyclic continuity and preserved unrelated colors. |
| PP-150 | Color | Hue-versus-luma, luma-versus-saturation and saturation curves | native | GA-doc | Selection domain and modified property differ among curves. | [Hue/saturation curves][S92] | Evaluate each curve family on calibrated color ramps. |
| PP-151 | Color | Three-way color wheels | native | GA-doc | Shadows, midtones and highlights can be controlled independently. | [Color wheels][S93] | Grade tonal regions separately and expose numerical wheel equivalents. |
| PP-152 | Color | Shot matching and reference comparison | native/model/ui-equivalent | GA-doc | Reference and current frames support side-by-side or split comparison. | [Shot matching][S94] | Match a target to a reference, show adjustable results and export comparison frames. |
| PP-153 | Color | HSL Secondary key and isolated correction | native | GA-doc | Qualification and refinement isolate a portion of the image for grading. | [HSL Secondary][S95] | Qualify/refine a color range and grade only its selected matte. |
| PP-154 | Color | Automatic correction with editable intensity | native/model | GA-doc | Auto Color changes basic controls and permits manual refinement. | [Auto Color][S96] | Suggest a reversible primary correction and expose its resolved values/intensity. |
| PP-155 | Color | LUT export/reuse | native/ecosystem | GA-doc | Adobe exports supported transforms as .cube. | [LUT export][S97] | Export a supported grade transform and verify samples in an independent LUT reader. |
| PP-156 | Scopes | Waveform, RGB/YUV parade, vectorscope and histogram | native/ui-equivalent | GA-doc | Scope types reveal different signal properties. | [Lumetri Scopes][S98] | Produce calibrated numerical/visual scopes with explicit range and color-space assumptions. |
| PP-157 | Color management | Per-source color interpretation and overrides | native | GA-doc | Camera log/RAW and common standard spaces require correct interpretation. | [Color options][S99] | Auto-detect and override source color space without silently changing source pixels. |
| PP-158 | Color management | Wide-gamut sequence working space | native | GA-doc | Source, working and output transforms are separate. | [Color management][S100] | Mix multiple source spaces in an explicit working space and verify reference transforms. |
| PP-159 | Color management | SDR, HLG and PQ output spaces | native | GA-doc | Standard and HDR spaces are listed separately. | [Color options][S99] | Render the same sequence to declared SDR/HLG/PQ targets with correct tags. |
| PP-160 | Color management | Tone/gamut mapping | native | GA-doc | Automatic mapping adapts mixed HDR/log sources for SDR. | [Tone mapping][S101] | Map calibrated highlights into SDR without uncontrolled clipping or color shifts. |
| PP-161 | Color management | Configurable sequence color presets | native/ui-equivalent | GA-doc | Presets simplify source/working/output color policy. | [Color management][S100] | Save a color policy preset and reproduce it on a new sequence. |
| PP-162 | Color management | Color-consistent compositing/application round trips | native/ecosystem | GA-doc | Adobe documents HDR monitoring and still-format limitations across apps. | [Premiere/AE color][S102] | Round-trip nested graphics/media with explicit transforms and quantify differences. |
| PP-163 | HDR | HDR metadata and graphics-white delivery | native/ecosystem | GA-doc | HDR10 metadata and target luminance are format-specific settings. | [Export settings][S103] | Encode supported HDR output, inspect tags/metadata and validate graphics-white levels. |
| PP-164 | Monitoring | Managed display/HDR monitoring | native/ecosystem/ui-equivalent | GA-doc | Physical monitoring depends on OS, GPU and display support. | [Color management][S100] | Export a documented preview transform and distinguish it from final encoded pixels. |

## Rendering, export, interchange and specialist delivery

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-165 | Render | Render sequence ranges for preview | native | GA-doc | Previews are acceleration artifacts, not a new source of project truth. | [Render section][S155] | Render a selected range, reuse valid cache and invalidate changed dependencies only. |
| PP-166 | Render | Render/replace with restoration | native | GA-doc | Heavy clips/compositions can be flattened and later restored. | [Render and Replace][S104] | Cache a heavy subtree, use it for playback and restore original editable structure. |
| PP-167 | Render | Smart rendering without recompression | native/ecosystem | GA-doc | Requires matching codec, geometry, frame rate and bit rate. | [Smart rendering][S105] | Copy eligible segments without re-encoding; fall back safely for changed segments. |
| PP-168 | Performance | Hardware decode/encode | native/ecosystem | GA-doc | Codec/profile/GPU/platform combinations determine availability. | [Hardware acceleration][S106] | Expose a capability probe and compare supported hardware output to software references. |
| PP-169 | Export | Full sequence and selected-range export | native | GA-doc | Export settings and destination are explicit. | [Export video][S107] | Export an exact frame range with matching audio duration and job status. |
| PP-170 | Export | Delivery codec/container profiles | native/ecosystem | GA-doc | Supported outputs depend on container, encoder and platform. | [Export formats][S108] | Publish a tested output matrix including mezzanine and distribution formats. |
| PP-171 | Export | Match Source video/audio settings | native | GA-release | 26.5 adds channel/sample-rate matching, downmixing and incompatibility warnings. | [Release notes][S01] | Match source settings and report every necessary conversion before encoding. |
| PP-172 | Export | Frame size/rate/PAR/field controls | native | GA-doc | Export geometry and sampling can differ from sequence values. | [Export settings][S103] | Encode declared geometry/rate/field settings and verify with an independent probe. |
| PP-173 | Export | CBR/VBR/profile/level settings | native/ecosystem | GA-doc | Format and encoder constrain valid combinations. | [Export settings][S103] | Validate settings before a job and verify bitrate/profile/level on encoded fixtures. |
| PP-174 | Export | Maximum-depth/scaling-quality controls | native/ecosystem | GA-doc | Processing depth and scaling quality are not equivalent to output bit depth. | [Export settings][S103] | Test high-quality resizing/depth modes and report intermediate/output precision. |
| PP-175 | Export | Still frames and image sequences | native | GA-doc | Image output formats have differing alpha/depth capabilities. | [Export formats][S108] | Export an exact frame or range to declared image formats with correct alpha. |
| PP-176 | Export | Saved presets and queued/batch jobs | native/ecosystem | GA-doc | Adobe Media Encoder is a distinct application path. | [Export video][S107] | Queue multiple preset-based variants with cancellation, independent progress and retry. |
| PP-177 | Interchange | EDL export | native/ecosystem | GA-doc | Best suited to simple tracks/edits; nested/effect fidelity is limited. | [EDL export][S109] | Export a documented EDL subset and emit a loss report for unsupported structures. |
| PP-178 | Interchange | Final Cut Pro XML export | native/ecosystem | GA-doc | Adobe exports the older FCP XML interchange; not automatic modern FCPXML parity. | [FCP XML export][S110] | Round-trip a supported XML fixture and report effect/audio/nesting losses. |
| PP-179 | Interchange | AAF export | native/ecosystem | GA-doc | Adobe tests Avid workflows; Windows embedded files above 2 GB are restricted. | [AAF export][S111] | Export a declared AAF subset with verified media references and handles. |
| PP-180 | Interchange | OMF audio handoff | native/ecosystem | GA-doc | Export targets Pro Tools; Premiere does not import OMF. | [OMF export][S112] | Deliver clip-based audio/handles and document which envelopes/transitions survive. |
| PP-181 | Delivery | Caption/transcript/text exports | native/ecosystem | GA-doc | Subtitle output and plain transcript output serve different uses. | [Caption export][S53] | Export all selected text tracks with timestamps and declared style loss. |
| PP-182 | Delivery | Social publishing destinations | cloud/ecosystem | GA-doc | External accounts and platform APIs are required. | [Export video][S107] | Produce validated platform variants; publish only through authorized destination credentials. |
| PP-183 | Delivery | Content Credentials | native/cloud/ecosystem | GA-release | Identity/provenance and AI-use preferences are attached at export. | [Content Credentials][S113] | Preserve generation/edit provenance and validate an optional signed export manifest. |
| PP-184 | Streaming | Secure Reliable Transport monitoring | native/ecosystem | GA-doc | SRT streams follow configured video/audio output and can use encryption. | [SRT][S114] | Send a documented live review stream to a receiver with configurable reliability/security. |
| PP-185 | Immersive | 180/360-degree VR editing and metadata | native/ecosystem/ui-equivalent | GA-doc | Projection, stereo interpretation and viewer support matter. | [VR editing][S115] | Interpret a known spherical source, render views and preserve VR delivery metadata. |
| PP-186 | Immersive | Ambisonic audio and immersive effects | native/ecosystem | GA-doc | Spatial audio requires a declared channel/projection convention. | [Ambisonics][S156] | Rotate video and sound coherently and verify the declared ambisonic channel layout. |

## Collaboration, review and application integration

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-187 | Collaboration | Cloud Team Projects | cloud | GA-doc | Hosted project state supports geographically distributed editors. | [Team Projects][S116] | Two authorized agents open shared state and exchange reviewable edit revisions. |
| PP-188 | Collaboration | Sequence locking and collaborator presence | cloud/ui-equivalent | GA-doc | Concurrent edit ownership and presence are visible. | [Team Projects][S116] | Reject conflicting sequence mutations and expose current ownership/presence. |
| PP-189 | Collaboration | Published versions and offline synchronization | cloud | GA-doc | Cloud sync and saved/published versions are distinct states. | [Team Projects][S116] | Edit offline, reconnect and reconcile versions without silently discarding changes. |
| PP-190 | Collaboration | Multi-project Productions | native/ecosystem | GA-doc | Shared local storage holds linked projects and shared source assets. | [Productions][S117] | Open a multi-project production and reference shared assets without duplication. |
| PP-191 | Collaboration | Shared-storage project locks | native/ecosystem | GA-doc | Project ownership matters under concurrent editors. | [Project lock][S157] | Enforce cross-process project ownership and recover stale locks with an audit trail. |
| PP-192 | Review | Frame.io review uploads and versions | cloud/ecosystem | GA-doc | Legacy and V4 integrations differ; account compatibility must be checked. | [Frame.io][S118] | Upload an authorized review version with source revision and exported timebase. |
| PP-193 | Review | Time-linked comments/annotations | cloud/ecosystem/ui-equivalent | GA-doc | Reviewer feedback is associated with frames/ranges. | [Frame.io][S118] | Import a comment set and preserve frame/range anchors across review versions. |
| PP-194 | Review | Review comments as timeline markers | cloud/ecosystem | GA-doc | Linked playheads/markers connect review feedback to editorial. | [Review markers][S119] | Convert review notes into stable annotations with status and provenance. |
| PP-195 | Assets | Shared Creative Cloud Libraries | cloud/ecosystem | GA-doc | Reusable assets sync across projects, devices and Adobe applications. | [CC Libraries][S120] | Resolve versioned shared assets and make missing/offline dependencies explicit. |
| PP-196 | Assets | Stock graphics, footage and audio licensing | cloud/ecosystem/paid | GA-doc | Browsing is distinct from asset licensing and reuse rights. | [Motion templates][S46], [Adobe Stock audio][S158] | Track asset license provenance and substitute missing/licensing-restricted assets explicitly. |
| PP-197 | Integration | Live After Effects composition linkage | ecosystem | GA-doc | Dynamic Link needs matching major application versions. | [Dynamic Link][S121] | Embed a live composition equivalent with shared timing/assets and dependency-aware updates. |
| PP-198 | Integration | Replace editorial clips with compositions | ecosystem | GA-doc | Selected clips can become dynamically linked compositions. | [Dynamic Link][S121] | Promote selected clips into an editable composition without breaking timeline timing. |
| PP-199 | Integration | Audio handoff to Audition | ecosystem | GA-doc | External audio-app workflows remain separate from native mixer parity. | [Audition workflow][S159] | Export a documented session/stem handoff and reconcile rendered revisions. |
| PP-200 | Integration | Photoshop image edit round trip | ecosystem | GA-doc | Layered import and source-file update are separate concerns. | [Layered assets][S06] | Refresh a revised still/layer source while preserving timeline placement and source IDs. |
| PP-201 | Integration | Firefly Boards to editable sequence | cloud/ecosystem | GA-release | Selected images/videos can import in selection order for further editing. | [Firefly Boards][S122] | Import a storyboard asset manifest into an editable ordered sequence with provenance. |

## Automation, observability, accessibility and operational reliability

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-202 | Automation | Modern JavaScript/UXP project API | native/ecosystem | GA-release | UXP is stable from 25.6; method/version coverage is explicit. | [UXP release][S123], [API][S124] | Discover a versioned programmatic editing surface and run documented workflows headlessly. |
| PP-203 | Automation | Typed API discovery and compatibility checks | native/ecosystem | GA-doc | Premiere API members have minimum-version information. | [Premiere API][S124] | Expose schemas/examples plus capability/version queries before operations run. |
| PP-204 | Extensibility | Native C++/hybrid extensions | ecosystem | GA-release | UXP hybrid support arrives in 26.2; runtime/platform support is separate. | [Hybrid plug-ins][S125] | Load a supported extension with explicit ABI/version checks and isolate failure reports. |
| PP-205 | Extensibility | Codec/effect/hardware plug-in ecosystem | ecosystem | GA-doc | Adobe offers low-level SDKs and panel automation; plug-ins are not bundled capabilities. | [Developer overview][S126] | Discover installed extensions and distinguish installed, loaded and successfully executed states. |
| PP-206 | Ergonomics | Command discovery and keyboard mappings | native/ui-equivalent | GA-doc | Desktop shortcuts are customizable; agents need equivalent discoverable commands. | [Keyboard shortcuts][S127] | List/search named commands with examples and make all key edit operations addressable. |
| PP-207 | Ergonomics | Source and program previews | native/ui-equivalent | GA-doc | Source preparation and sequence-output inspection are distinct views. | [Monitors][S17] | Preview source or composed sequence ranges with explicit transforms and synchronized audio. |
| PP-208 | Ergonomics | Task-specific workspaces | native/ui-equivalent | GA-doc | Panels/layouts can be saved per workflow. | [Workspaces][S128] | Provide focused query/context bundles for editing, sound, graphics, grading and delivery. |
| PP-209 | Accessibility | Keyboard and assistive-technology access | native/ui-equivalent/ecosystem | GA-doc | Adobe documents limited screen-reader support and platform exceptions. | [Accessibility][S129] | Make agent tools self-describing and human review controls keyboard/assistive accessible. |
| PP-210 | Reliability | Undo/history and recoverable edit states | native | GA-doc | History records actions/states and supports return to earlier states. | [History][S130] | Undo/redo a compound edit and verify exact persisted project-state restoration. |
| PP-211 | Reliability | Autosave and retained versions | native | GA-doc | Save interval and retained versions are configurable. | [Auto Save][S131] | Interrupt a write/job and reopen a valid checkpoint without corrupting the last saved state. |
| PP-212 | Reliability | Crash recovery and explicit restore | native | GA-doc | Recovery/Auto-Save versions can be reopened independently. | [Crash recovery][S132] | Recover an interrupted session with a visible revision choice and retained source files. |
| PP-213 | Observability | Structured errors and event details | native/ui-equivalent | GA-doc | Events expose warnings/errors, including plug-in issues. | [Events][S133] | Return actionable structured diagnostics with affected IDs and deterministic failure behavior. |
| PP-214 | Operations | Age/size-based media-cache management | native | GA-doc | Cache retention is configurable and separate from source media. | [Cache management][S134] | Evict rebuildable artifacts by policy and verify project/source integrity afterward. |

## Generative AI and premium cloud processing

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-215 | Generative | Extend video beyond source handles | cloud/paid/model | GA-release | Broader accepted source formats do not imply native output-format parity. | [Generative Extend][S135] | Request bounded synthetic handles, retain original media and expose generated-media provenance. |
| PP-216 | Generative | Ambient-audio extension | cloud/paid/model | GA-doc | Spoken dialogue/music are excluded; mono/stereo are supported. | [Generative Extend][S135] | Generate ambient extensions and reject unsupported dialogue/music/channel conditions explicitly. |
| PP-217 | Generative | Extension format conversion disclosure | cloud/paid/model | GA-release | Current overview specifies 8-bit SDR and 30 fps output for faster sources. | [Generative Extend][S135] | Report generated format, conversions and seams before accepting extension media. |
| PP-218 | Generative | Text/reference-guided video generation | cloud/paid/model | GA-release | In-timeline results are editable clips; models depend on entitlement/region. | [Generative Media][S136] | Generate into a selected range using a prompt/reference and import as normal editable media. |
| PP-219 | Generative | Text/voice-guided sound-effect generation | cloud/paid/model | GA-release | Optional vocal guidance shapes timing/intensity. | [Generative Media][S136] | Generate a sound effect with timing guidance and retain an editable audio clip. |
| PP-220 | Generative | Model selection and regeneration | cloud/paid/model | GA-doc | Firefly/partner models vary by plan and region. | [Generative FAQ][S137] | Probe providers, select a supported model and regenerate as a new linked variant. |
| PP-221 | Generative | Generation history with prompt/reference context | cloud/paid/model | GA-doc | Adobe retains generation metadata and organizes media in project history. | [Generative FAQ][S137] | Persist prompts, references, model/settings and result IDs without overwriting prior variants. |
| PP-222 | Generative | Credits, asynchronous jobs and cloud capability state | cloud/paid/model | GA-doc | Generation consumes credits and requires internet; installed UI is insufficient. | [Generative FAQ][S137] | Return entitlement/cost estimates, progress, cancellation and explicit unavailable-provider failures. |
| PP-223 | Dialogue separation | Separate overlapping speakers | cloud/paid/model | GA-doc | Premium processing charges per audio second and includes handles; source is preserved. | [Separate Crosstalk][S138] | Produce separately editable speaker stems, preserve the source and evaluate leakage/artifacts. |
| PP-224 | Cloud processing | Explicit audio upload/processing state | cloud/paid/model | GA-doc | Voice features upload selected audio and require sign-in/internet. | [Cloud voice][S139] | Expose which media is processed remotely and preserve result/source/provenance associations. |

## Beta watchlist: excluded from stable parity totals

| ID | Family | Capability | Class | Adobe channel | Constraints / behavior | Official source | Proposed agent acceptance |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PP-225 | Beta color | New Color mode and grade-management workflow | beta/native/ui-equivalent | Beta | Separate workflow includes clip grid, operations, groups and style modules. | [Color mode beta][S140] | Track as an exploratory design requirement until shipping behavior is verified. |
| PP-226 | Beta generative | Generate original instrumental music | beta/cloud/paid/model | Beta | Timeline generation supports prompt, tempo, loop and editable audio results. | [Generate Music beta][S141] | Evaluate provider-generated music with explicit duration/provenance without counting stable parity. |
| PP-227 | Beta assistant | Natural-language project preparation/assembly | beta/cloud/model/ui-equivalent | Beta | Adobe documents organization, footage preparation and initial edits. | [AI Assistant beta][S142] | Benchmark a fresh agent on documented tools and a realistic edit/revision brief. |

## Dependency priorities suggested by this inventory

These are implementation-order recommendations, not assertions about current Ferrocut code.

1. **Shared project/time/media identity:** rational time, source ranges, track and link semantics, undo, source/proxy identity, relinking and portable assets underpin later features. Editorial tools should report exactly what changed.
2. **Native graphics and text:** font discovery, shaping/metrics, paragraph layout, paths, styles, responsive relationships, editable template instances and caption cues are prerequisites for professional title/caption work. A rendered image alone does not satisfy editability.
3. **Compositing/effect/mask graph:** effect ordering, alpha/color semantics, reusable masks, tracking data and source-versus-instance processing support keying, local grading, stabilization and sophisticated graphics.
4. **Production audio and color:** channel layout/routing, buses, envelopes, loudness/peaks, source/working/output color transforms, scopes and HDR delivery need reference fixtures and independent measurements.
5. **Round trips and review:** conformable exports, interchange loss reports, portable projects, review annotations and revision identity make the tool usable in real productions.
6. **Agent operating context and benchmarks:** versioned schemas, clear errors, examples, previews and targeted context must accompany features. Use fresh-agent creative/revision tasks and editor review alongside deterministic tests.
7. **Optional service providers:** generation, cloud separation, stock, review hosting and shared libraries should use explicit provider boundaries. Core editing remains independently testable; service entitlement/authentication/execution must be separately evidenced.

## Coverage limits and follow-up research

- **227 capability rows:** 224 non-beta requirements and 3 explicitly separated beta requirements. This is broad workflow coverage, not an exhaustive transcription of every Adobe menu, effect parameter, keyboard command, codec variant or SDK member. Many rows intentionally represent a family that needs finer child specifications during implementation.
- The effects/transition library is especially large and changes across versions. The modern-effect, animation and migration pages remain authoritative discovery indexes; this inventory does not claim individual verification of every effect in the advertised library. Legacy/obsolete names are not automatically targets.
- Slip and slide are supported by the current guide navigation; their linked deep pages failed retrieval in this session. Their proposed edit semantics require dedicated fixture validation before implementation is marked complete.
- Camera formats, audio layouts, maximum sizes, hardware acceleration, caption standards, color transforms and interchange need independent compatibility matrices with test media. Account/cloud functions need authenticated execution evidence. Adobe regional pages can lag the canonical English page.
- Desktop spatial interactions are translated into proposed agent operations rather than prescribing a duplicate UI. Examples include Sequence Index queries, named trim operations, graph inspection, preview ranges and event diagnostics.
- No Adobe application, paid service, entitlement, SDK download, plug-in or Ferrocut feature was executed by this research lane. All current-product claims are documentation-based. Third-party tools are mentioned only where an Adobe primary source describes an integration.
- Source dates below are the publisher's visible last-update dates, not proof of when a feature first shipped. All links were retrieved on **2026-10-08**; `not displayed` means the cited page did not expose a reliable date in the retrieved text.

## Official source register

The identifiers are Markdown reference links used directly by inventory rows. Sources are Adobe Help/Developer pages; there are no secondary-market feature claims here.

| Source | Article / area | Visible update or publication date |
| --- | --- | --- |
| [S00] | Desktop help navigation | Not displayed |
| [S01] | Desktop release notes | 2026-10-05 |
| [S02] | What's new | 2026-09-10 |
| [S03] | Project templates | 2026-04-02 |
| [S04] | Supported import formats | 2026-09-30 |
| [S05] | Still images | 2026-04-01 |
| [S06] | Photoshop/Illustrator import | 2026-01-07 |
| [S07] | Ingest/proxy workflow | 2026-01-07 |
| [S08] | Locate/link offline files | 2026-04-08 |
| [S09] | Consolidate/archive | 2026-01-07 |
| [S10] | Metadata | 2026-08-18 |
| [S11] | Timecode | 2026-01-07 |
| [S12] | Markers | 2026-01-07 |
| [S13] | Media intelligence FAQ | 2026-08-19 |
| [S14] | Sequence Index | 2026-08-18 |
| [S15] | Sequence settings | 2026-01-07 |
| [S16] | Add/remove clips | 2026-04-08 |
| [S17] | Source/Program monitors | 2026-01-07 |
| [S18] | Move clips | 2026-04-01 |
| [S19] | Trim a clip | 2025-08-22 |
| [S20] | Ripple edits | 2026-03-23 |
| [S21] | Rolling edit | 2025-08-22 |
| [S22] | J/L cuts | 2025-08-22 |
| [S23] | Sync Lock | 2026-01-07 |
| [S24] | Track Lock | 2026-01-07 |
| [S25] | Nested sequences | 2026-08-18 |
| [S26] | Interpret Footage | 2026-04-02 |
| [S27] | Speed/duration | 2026-01-07 |
| [S28] | Time interpolation | 2026-01-07 |
| [S29] | Multicam sources | 2026-01-07 |
| [S30] | Multicam target | 2025-08-22 |
| [S31] | Speech to Text | 2026-06-02 |
| [S32] | Text-Based Editing | 2026-01-07 |
| [S33] | Paper Edit | 2026-09-09 |
| [S34] | Scene Edit Detection | 2026-08-18 |
| [S35] | Bulk mute/bleep | 2026-08-18 |
| [S36] | Titles | 2026-04-08 |
| [S37] | Color fonts | 2026-04-08 |
| [S38] | Text styles | 2026-04-08 |
| [S39] | Linked/track styles | 2026-04-08 |
| [S40] | Font replacement | 2026-04-08 |
| [S41] | Emojis | 2026-04-08 |
| [S42] | Pen Tool | 2026-01-07 |
| [S43] | Responsive graphics | 2026-01-07 |
| [S44] | Protected intro/outro | 2026-01-07 |
| [S45] | Source graphics | 2026-01-07 |
| [S46] | Motion templates | 2026-01-07 |
| [S47] | Data-driven templates | 2026-01-07 |
| [S48] | Media replacement | 2026-08-18 |
| [S49] | Create captions | 2026-01-07 |
| [S50] | Single-word captions | 2026-08-18 |
| [S51] | Caption formats | 2026-01-07 |
| [S52] | Caption translation | 2026-06-02 |
| [S53] | Caption export | 2026-01-07 |
| [S54] | Audio concepts | 2026-01-07 |
| [S55] | Channel mapping | 2026-01-07 |
| [S56] | Audio Track Mixer | 2026-01-07 |
| [S57] | Track automation | 2026-08-18 |
| [S58] | Clip automation | 2026-08-18 |
| [S59] | Audio crossfades | 2026-01-07 |
| [S60] | Essential Sound | 2026-01-07 |
| [S61] | Enhance Speech | 2026-01-07 |
| [S62] | Audio effects library | 2026-01-07 |
| [S63] | Loudness Meter | 2026-01-07 |
| [S64] | Remix | 2026-08-18 |
| [S65] | Voice-over | 2026-01-07 |
| [S66] | Apply effects | 2026-01-07 |
| [S67] | Effect presets | 2026-01-07 |
| [S68] | Adjustment layers | 2026-08-18 |
| [S69] | Source effects | 2026-08-18 |
| [S70] | Motion | 2026-01-07 |
| [S71] | Vector Motion | 2026-01-07 |
| [S72] | Keyframes | 2026-01-07 |
| [S73] | Interpolation | 2026-01-07 |
| [S74] | Blend modes | 2026-03-09 |
| [S75] | Ultra Key | 2026-01-07 |
| [S76] | Mask properties | 2026-03-09 |
| [S77] | Refine/combine masks | 2026-08-18 |
| [S78] | Mask tracking | 2026-03-09 |
| [S79] | Object Mask | 2026-09-09; title still says beta |
| [S80] | HSL/luminance masks | 2026-08-26 |
| [S81] | Auto Reframe | 2026-04-15 |
| [S82] | Warp Stabilizer | 2026-01-07 |
| [S83] | Transition alignment | 2026-01-07 |
| [S84] | Clip handles | 2026-01-07 |
| [S85] | Modern transitions | 2026-08-18 |
| [S86] | Morph Cut | 2026-01-07 |
| [S87] | Modern effects | 2026-09-09 |
| [S88] | Modern animations | 2026-08-18 |
| [S89] | 26.0 effects changes | 2026-02-25 |
| [S90] | Basic color correction | 2026-01-07 |
| [S91] | RGB curves | 2025-08-22 |
| [S92] | Hue/saturation curves | 2026-04-01 |
| [S93] | Color wheels | 2026-08-18 |
| [S94] | Shot matching | 2026-08-18 |
| [S95] | HSL Secondary | 2026-08-18 |
| [S96] | Auto Color | 2026-08-18 |
| [S97] | LUT export | 2026-01-07 |
| [S98] | Lumetri Scopes | 2026-08-18 |
| [S99] | Color options | 2026-01-07 |
| [S100] | Color management | 2026-01-07 |
| [S101] | Tone mapping | 2026-03-05 |
| [S102] | Premiere/After Effects color | 2026-03-05 |
| [S103] | Export settings | 2026-08-18 |
| [S104] | Render and Replace | 2026-01-07 |
| [S105] | Smart rendering | 2026-08-18 |
| [S106] | Hardware acceleration | 2026-01-07 |
| [S107] | Export video | 2026-01-07 |
| [S108] | Export formats | 2026-08-18 |
| [S109] | EDL export | 2026-01-07 |
| [S110] | FCP XML export | 2026-01-07 |
| [S111] | AAF export | 2026-08-18 |
| [S112] | OMF export | 2026-08-18 |
| [S113] | Content Credentials | 2026-08-11 |
| [S114] | SRT | 2026-01-07 |
| [S115] | VR editing | 2026-03-05 |
| [S116] | Team Projects | 2026-01-07 |
| [S117] | Productions | 2026-08-18 |
| [S118] | Frame.io overview | 2025-11-19 |
| [S119] | Frame.io comments/markers | 2025-11-19 |
| [S120] | CC Libraries | 2026-08-18 |
| [S121] | Dynamic Link | 2026-08-18 |
| [S122] | Firefly Boards import | 2026-08-18 |
| [S123] | UXP stable announcement | 2025-12-16 |
| [S124] | Premiere UXP API | 2026-07-01 |
| [S125] | Hybrid plug-in announcement | April 2026; exact day not displayed |
| [S126] | Premiere developer overview | Not displayed |
| [S127] | Keyboard shortcuts | 2026-01-07 |
| [S128] | Workspaces | 2026-01-07 |
| [S129] | Screen reader/magnifier support | 2026-01-07 |
| [S130] | History | 2026-01-07 |
| [S131] | Auto Save | 2026-01-07 |
| [S132] | Crash recovery | 2026-01-07 |
| [S133] | Events/diagnostics | 2026-01-07 |
| [S134] | Cache management | 2026-01-07 |
| [S135] | Generative Extend overview | 2026-09-09 |
| [S136] | Generative Media overview | 2026-09-09 |
| [S137] | Generative Media FAQ | 2026-09-09 |
| [S138] | Separate Crosstalk | 2026-07-28 |
| [S139] | Cloud voice features | 2026-09-24 |
| [S140] | Color mode beta | 2026-08-24 |
| [S141] | Generate Music beta | 2026-08-25 |
| [S142] | Premiere AI Assistant beta | 2026-06-18 |

| [S143] | Attach existing proxies | 2026-01-07 |
| [S144] | Add video/audio/submix tracks | 2026-01-07 |
| [S145] | Source patching | 2026-04-08 |
| [S146] | Asymmetrical trimming | 2026-08-18 |
| [S147] | Group clips | 2026-08-18 |
| [S148] | Create subclips | 2026-04-08 |
| [S149] | Detect/delete transcript pauses | 2026-01-07 |
| [S150] | Remove transcript speaker instances | 2025-08-22 |
| [S151] | Group text/graphic layers | 2026-01-07 |
| [S152] | Create audio submix | 2026-01-07 |
| [S153] | Automatic audio ducking | 2026-01-07 |
| [S154] | Copy/paste clip effects | 2026-01-07 |
| [S155] | Render sequence sections | 2026-01-07 |
| [S156] | Ambisonic audio assembly | 2026-01-07 |
| [S157] | Production project locks | 2026-08-18 |
| [S158] | Adobe Stock audio | 2026-01-07 |
| [S159] | Audition integration | 2026-08-18 |

[S00]: https://helpx.adobe.com/premiere/desktop.html
[S01]: https://helpx.adobe.com/premiere/desktop/whats-new/release-notes.html
[S02]: https://helpx.adobe.com/premiere/desktop/whats-new/whats-new.html
[S03]: https://helpx.adobe.com/premiere/desktop/organize-media/create-projects/create-your-own-project-templates.html
[S04]: https://helpx.adobe.com/premiere/desktop/organize-media/import-files/supported-file-formats.html
[S05]: https://helpx.adobe.com/premiere/desktop/organize-media/import-files/import-still-images.html
[S06]: https://helpx.adobe.com/premiere/desktop/organize-media/import-files/import-photoshop-and-illustrator-files.html
[S07]: https://helpx.adobe.com/premiere/desktop/organize-media/ingest-proxy-workflow/ingest-and-proxy-workflow.html
[S08]: https://helpx.adobe.com/premiere/desktop/organize-media/file-organization/locate-and-link-offline-files.html
[S09]: https://helpx.adobe.com/premiere/desktop/organize-media/create-projects/consolidate-and-archive-projects.html
[S10]: https://helpx.adobe.com/premiere/desktop/organize-media/edit-metadata/metadata-in-premiere.html
[S11]: https://helpx.adobe.com/premiere/desktop/organize-media/apply-labeling/about-timecode.html
[S12]: https://helpx.adobe.com/premiere/desktop/organize-media/apply-labeling/overview-of-markers.html
[S13]: https://helpx.adobe.com/premiere/desktop/organize-media/file-organization/media-intelligence-and-search-panel.html
[S14]: https://helpx.adobe.com/premiere/desktop/organize-media/file-organization/navigate-timelines-with-sequence-index.html
[S15]: https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-sequence/sequence-settings-reference.html
[S16]: https://helpx.adobe.com/premiere/desktop/edit-projects/intro-to-editing/add-or-remove-clips.html
[S17]: https://helpx.adobe.com/premiere/desktop/get-started/source-and-program-monitor-adjustments/about-source-monitor-and-program-monitor.html
[S18]: https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-sequence/different-ways-to-move-clips.html
[S19]: https://helpx.adobe.com/premiere/desktop/edit-projects/trim-clips/trim-a-clip.html
[S20]: https://helpx.adobe.com/premiere/desktop/edit-projects/trim-clips/perform-ripple-edits.html
[S21]: https://helpx.adobe.com/premiere/desktop/edit-projects/trim-clips/perform-rolling-edits.html
[S22]: https://helpx.adobe.com/premiere/desktop/edit-projects/trim-clips/perform-j-cuts-and-l-cuts.html
[S23]: https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-sequence/sync-lock-to-prevent-changes.html
[S24]: https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-sequence/track-lock-to-prevent-changes.html
[S25]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-nested-sequences/about-nested-sequences.html
[S26]: https://helpx.adobe.com/premiere/desktop/edit-projects/modify-clip-properties/modifying-clip-properties-with-interpret-footage.html
[S27]: https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-speed/different-ways-to-change-clip-speed-and-duration.html
[S28]: https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-speed/apply-time-interpolation-methods-to-adjust-clip-speed.html
[S29]: https://helpx.adobe.com/premiere/desktop/edit-projects/set-up-multi-camera-sequences-for-editing/create-a-multi-camera-source-sequence.html
[S30]: https://helpx.adobe.com/premiere/desktop/edit-projects/set-up-multi-camera-sequences-for-editing/create-and-edit-a-multi-camera-target-sequence.html
[S31]: https://helpx.adobe.com/premiere/desktop/add-text-images/insert-captions/auto-transcribe-video-using-speech-to-text.html
[S32]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-video-using-text-based-editing/overview-of-text-based-editing.html
[S33]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-video-using-text-based-editing/create-a-sequence-with-paper-edit.html
[S34]: https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-sequence/detect-edit-points-using-scene-edit-detection.html
[S35]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/advanced-audio-techniques/clean-audio-with-bulk-mute-or-bleep.html
[S36]: https://helpx.adobe.com/premiere/desktop/add-text-images/stylize-text/create-titles.html
[S37]: https://helpx.adobe.com/premiere/desktop/add-text-images/stylize-text/use-color-fonts.html
[S38]: https://helpx.adobe.com/premiere/desktop/add-text-images/stylize-text/create-text-styles.html
[S39]: https://helpx.adobe.com/premiere/desktop/add-text-images/stylize-text/create-linked-and-track-styles.html
[S40]: https://helpx.adobe.com/premiere/desktop/add-text-images/stylize-text/replace-fonts.html
[S41]: https://helpx.adobe.com/premiere/desktop/add-text-images/stylize-text/use-emojis.html
[S42]: https://helpx.adobe.com/premiere/desktop/add-text-images/draw-objects/draw-with-pen-tool.html
[S43]: https://helpx.adobe.com/premiere/desktop/add-text-images/insert-images-and-graphics/create-responsive-graphics.html
[S44]: https://helpx.adobe.com/premiere/desktop/add-text-images/insert-images-and-graphics/preserve-intro-outro-animations-while-creating-responsive-design-graphics.html
[S45]: https://helpx.adobe.com/premiere/desktop/add-text-images/insert-images-and-graphics/create-source-graphics.html
[S46]: https://helpx.adobe.com/premiere/desktop/add-text-images/use-motion-graphics-templates/about-motion-graphics-templates.html
[S47]: https://helpx.adobe.com/premiere/desktop/add-text-images/use-motion-graphics-templates/use-data-driven-motion-graphics-templates.html
[S48]: https://helpx.adobe.com/premiere/desktop/add-text-images/use-motion-graphics-templates/media-replacement-in-motion-graphics-templates.html
[S49]: https://helpx.adobe.com/premiere/desktop/add-text-images/insert-captions/create-captions.html
[S50]: https://helpx.adobe.com/premiere/desktop/add-text-images/insert-captions/create-single-word-captions.html
[S51]: https://helpx.adobe.com/premiere/desktop/add-text-images/insert-captions/supported-file-formats-for-captions.html
[S52]: https://helpx.adobe.com/premiere/desktop/add-text-images/insert-captions/translate-captions.html
[S53]: https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/export-caption-tracks.html
[S54]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/basic-audio-editing/audio-editing-concepts.html
[S55]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/advanced-audio-techniques/audio-channel-mapping.html
[S56]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/advanced-audio-techniques/about-audio-track-mixer.html
[S57]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/apply-audio-effects/audio-track-mixer-automation-modes.html
[S58]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/apply-audio-effects/set-automation-modes-for-audio-clip-mixer.html
[S59]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/apply-audio-transitions/audio-crossfade-transitions.html
[S60]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/adjust-volume-and-levels/audio-editing-with-essential-sound-panel.html
[S61]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/adjust-volume-and-levels/enhance-speech.html
[S62]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/apply-audio-effects/audio-effects-library.html
[S63]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/apply-audio-effects/about-loudness-meter.html
[S64]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/apply-audio-effects/remix-audio-in-premiere.html
[S65]: https://helpx.adobe.com/premiere/desktop/organize-media/import-files/record-a-voice-over-on-an-audio-track-from-the-timeline.html
[S66]: https://helpx.adobe.com/premiere/desktop/add-video-effects/apply-video-effects/apply-effects.html
[S67]: https://helpx.adobe.com/premiere/desktop/add-video-effects/apply-video-effects/effect-presets.html
[S68]: https://helpx.adobe.com/premiere/desktop/add-video-effects/apply-video-effects/create-adjustment-layers.html
[S69]: https://helpx.adobe.com/premiere/desktop/add-video-effects/apply-video-effects/apply-effects-to-source-clips.html
[S70]: https://helpx.adobe.com/premiere/desktop/add-video-effects/commonly-used-effects/apply-motion-effect.html
[S71]: https://helpx.adobe.com/premiere/desktop/add-video-effects/commonly-used-effects/edit-vector-graphics-using-vector-motion-effect.html
[S72]: https://helpx.adobe.com/premiere/desktop/add-video-effects/control-effects-and-transitions-using-keyframes/about-keyframes.html
[S73]: https://helpx.adobe.com/premiere/desktop/add-video-effects/control-effects-and-transitions-using-keyframes/control-effect-changes-using-keyframe-interpolation.html
[S74]: https://helpx.adobe.com/premiere/desktop/add-video-effects/work-with-composites/blend-mode-options.html
[S75]: https://helpx.adobe.com/premiere/desktop/add-video-effects/effects-and-transitions-library/apply-and-customize-chromakey-using-the-ultra-key-effect.html
[S76]: https://helpx.adobe.com/premiere/desktop/add-video-effects/work-with-masks/adjust-mask-properties.html
[S77]: https://helpx.adobe.com/premiere/desktop/add-video-effects/work-with-masks/refining-and-combining-masks.html
[S78]: https://helpx.adobe.com/premiere/desktop/add-video-effects/work-with-masks/track-masks.html
[S79]: https://helpx.adobe.com/premiere/desktop/add-video-effects/work-with-masks/object-masking.html
[S80]: https://helpx.adobe.com/premiere/desktop/add-video-effects/work-with-masks/create-masks-using-hsl-and-luminance.html
[S81]: https://helpx.adobe.com/premiere/desktop/add-video-effects/commonly-used-effects/auto-reframe-overview.html
[S82]: https://helpx.adobe.com/premiere/desktop/add-video-effects/commonly-used-effects/warp-stabilizer-settings.html
[S83]: https://helpx.adobe.com/premiere/desktop/add-video-effects/apply-video-transitions/align-transitions.html
[S84]: https://helpx.adobe.com/premiere/desktop/add-video-effects/apply-video-transitions/clip-handles-settings.html
[S85]: https://helpx.adobe.com/premiere/desktop/add-video-effects/types-of-effects/transitions.html
[S86]: https://helpx.adobe.com/premiere/desktop/add-video-effects/apply-video-transitions/morph-cut-overview.html
[S87]: https://helpx.adobe.com/premiere/desktop/add-video-effects/types-of-effects/effects.html
[S88]: https://helpx.adobe.com/premiere/desktop/add-video-effects/types-of-effects/animations.html
[S89]: https://helpx.adobe.com/premiere/desktop/add-video-effects/effects-and-transitions-library/list-of-effects-and-transitions.html
[S90]: https://helpx.adobe.com/premiere/desktop/correct-color/color-correction-fundamentals/basic-color-correction-options.html
[S91]: https://helpx.adobe.com/premiere/desktop/correct-color/add-color-effects/correct-color-using-rgb-curves.html
[S92]: https://helpx.adobe.com/premiere/desktop/correct-color/add-color-effects/correct-color-using-hue-and-saturation-curves.html
[S93]: https://helpx.adobe.com/premiere/desktop/correct-color/add-color-effects/correct-color-using-color-wheel.html
[S94]: https://helpx.adobe.com/premiere/desktop/correct-color/add-color-effects/match-color-between-shots.html
[S95]: https://helpx.adobe.com/premiere/desktop/correct-color/add-color-effects/correct-color-using-hsl-secondary-controls.html
[S96]: https://helpx.adobe.com/premiere/desktop/correct-color/add-color-effects/use-auto-color.html
[S97]: https://helpx.adobe.com/premiere/desktop/correct-color/add-color-effects/save-or-export-look-up-tables.html
[S98]: https://helpx.adobe.com/premiere/desktop/correct-color/add-color-effects/available-lumetri-scopes.html
[S99]: https://helpx.adobe.com/premiere/desktop/correct-color/set-up-color-management/color-management-options.html
[S100]: https://helpx.adobe.com/premiere/desktop/correct-color/set-up-color-management/about-color-management.html
[S101]: https://helpx.adobe.com/premiere/desktop/correct-color/set-up-color-management/tone-mapping-in-premiere.html
[S102]: https://helpx.adobe.com/premiere/desktop/correct-color/set-up-color-management/premiere-after-effects-color-management-compatibility.html
[S103]: https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/overview-of-export-settings.html
[S104]: https://helpx.adobe.com/premiere/desktop/render-and-export/render-sequences-for-playback/render-and-replace-media-in-a-sequence.html
[S105]: https://helpx.adobe.com/premiere/desktop/render-and-export/render-sequences-for-playback/smart-rendering-supported-formats.html
[S106]: https://helpx.adobe.com/premiere/desktop/get-started/technical-requirements/hardware-accelerated-decoding-and-encoding.html
[S107]: https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/export-video.html
[S108]: https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/supported-export-file-formats.html
[S109]: https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/export-a-project-as-an-edl-file.html
[S110]: https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/export-a-project-as-a-final-cut-pro-xml-file.html
[S111]: https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/export-aaf-files.html
[S112]: https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/export-omf-files-for-pro-tools.html
[S113]: https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/export-videos-with-content-credentials.html
[S114]: https://helpx.adobe.com/premiere/desktop/render-and-export/stream-video/overview-of-secure-reliable-transport.html
[S115]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-vr-content/vr-editing-in-premiere.html
[S116]: https://helpx.adobe.com/premiere/desktop/collaborate-with-others/collaborate-using-team-projects/about-team-projects.html
[S117]: https://helpx.adobe.com/premiere/desktop/collaborate-with-others/collaborate-using-productions/about-productions.html
[S118]: https://helpx.adobe.com/premiere/desktop/collaborate-with-others/share-for-review-using-frame-io/about-frameio.html
[S119]: https://helpx.adobe.com/premiere/desktop/collaborate-with-others/share-for-review-using-frame-io/import-comments-as-markers.html
[S120]: https://helpx.adobe.com/premiere/desktop/collaborate-with-others/collaborate-using-creative-cloud-libraries/about-creative-cloud-libraries.html
[S121]: https://helpx.adobe.com/premiere/desktop/use-premiere-with-other-apps/working-with-other-adobe-applications/share-assets-between-after-effects-and-premiere-using-dynamic-link.html
[S122]: https://helpx.adobe.com/premiere/desktop/organize-media/import-files/import-media-from-firefly-boards.html
[S123]: https://blog.developer.adobe.com/en/publish/2025/12/uxp-arrives-in-premiere-a-new-era-for-plugin-development
[S124]: https://developer.adobe.com/premiere-pro/uxp/ppro-reference/
[S125]: https://blog.developer.adobe.com/en/publish/2026/04/uxp-hybrid-plugins-now-available-for-premiere
[S126]: https://developer.adobe.com/premiere-pro/
[S127]: https://helpx.adobe.com/premiere/desktop/get-started/keyboard-shortcuts/about-keyboard-shortcuts.html
[S128]: https://helpx.adobe.com/premiere/desktop/get-started/tour-the-workspace/what-are-workspaces.html
[S129]: https://helpx.adobe.com/premiere/desktop/get-started/set-up-accessibility-features/screen-reader-screen-magnifier-and-operating-system-accessibility-support.html
[S130]: https://helpx.adobe.com/premiere/desktop/edit-projects/correct-mistakes/view-or-make-changes-in-the-history-panel.html
[S131]: https://helpx.adobe.com/premiere/desktop/get-started/preferences-and-settings/auto-save-preferences.html
[S132]: https://helpx.adobe.com/premiere/desktop/troubleshooting/crash-issues/recover-projects-after-a-crash.html
[S133]: https://helpx.adobe.com/premiere/desktop/edit-projects/correct-mistakes/view-warnings-errors-and-information-events.html
[S134]: https://helpx.adobe.com/premiere/desktop/troubleshooting/media-issues/automatically-manage-your-media-cache-files.html
[S135]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-with-generative-ai/generative-extend-overview.html
[S136]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-with-generative-ai/generative-media-tool-overview.html
[S137]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-with-generative-ai/generative-media-tool-faq.html
[S138]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/basic-audio-editing/separate-overlapping-speakers.html
[S139]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-with-generative-ai/about-cloud-based-ai-voice-features.html
[S140]: https://helpx.adobe.com/premiere/desktop/correct-color/color-mode-fundamentals/color-mode-basics.html
[S141]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-with-generative-ai/generate-music-overview.html
[S142]: https://helpx.adobe.com/premiere/desktop/premiere-ai-assistant/overview.html
[S143]: https://helpx.adobe.com/premiere/desktop/organize-media/ingest-proxy-workflow/attach-proxies-to-full-resolution-media.html
[S144]: https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-sequence/add-tracks.html
[S145]: https://helpx.adobe.com/premiere/desktop/edit-projects/intro-to-editing/add-media-to-the-timeline-using-source-patching.html
[S146]: https://helpx.adobe.com/premiere/desktop/edit-projects/trim-clips/asymmetrical-trimming.html
[S147]: https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-sequence/group-clips.html
[S148]: https://helpx.adobe.com/premiere/desktop/edit-projects/intro-to-editing/create-a-subclip-from-the-project-panel.html
[S149]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-video-using-text-based-editing/detect-and-delete-pauses-in-transcripts.html
[S150]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-video-using-text-based-editing/remove-all-instances-of-one-speaker-in-transcript.html
[S151]: https://helpx.adobe.com/premiere/desktop/add-text-images/insert-images-and-graphics/group-text-and-graphic-layers.html
[S152]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/advanced-audio-techniques/create-a-submix.html
[S153]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/adjust-volume-and-levels/automatically-duck-audio.html
[S154]: https://helpx.adobe.com/premiere/desktop/add-video-effects/apply-video-effects/copy-and-paste-clip-effects.html
[S155]: https://helpx.adobe.com/premiere/desktop/render-and-export/render-sequences-for-playback/render-a-section-of-a-sequence.html
[S156]: https://helpx.adobe.com/premiere/desktop/edit-projects/edit-vr-content/assembling-ambisonics-audio.html
[S157]: https://helpx.adobe.com/premiere/desktop/collaborate-with-others/collaborate-using-productions/change-project-lock-status-in-production.html
[S158]: https://helpx.adobe.com/premiere/desktop/add-audio-effects/use-adobe-stock-audio/use-adobe-stock-audio-in-your-project.html
[S159]: https://helpx.adobe.com/premiere/desktop/use-premiere-with-other-apps/working-with-other-adobe-applications/how-premiere-works-with-audition-for-audio-editing.html
