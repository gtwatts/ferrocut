# Production repair cycle — 2026-10-09

The reviewed source and installed-tool milestone repairs nondistorting media
placement and caption boundary blinks. Complete repaired explainer and social
films, a new fictional commercial, viewing copies and standalone editable
packages are delivered. All three films and editable packages have independent
bounded technical acceptance. This cycle does not
establish Adobe feature parity or professional creative acceptance.

## Reconciled baseline

The source baseline is `e5a2e242af71d9f2b67ed0024393e3552d4deda8` on
`feat/agent-native-core-adoption`, with [PR #1](https://github.com/gtwatts/ferrocut/pull/1)
open. The installed binaries report revision
`5de843447b5965ecbd310809497695f726887afc`; the intervening source commits contain
documentation only. All four installed binary hashes matched their installation
manifest, and the installed MCP route successfully probed the 1280 × 534, 24 fps
`eval/media/clips/t1.mkv` fixture.

The earlier container frame-rate defect is already fixed in that baseline. The
[previous agent evaluation](2026-10-08-agent-video-tests.md) now distinguishes its
original failure from the same-day fix. Existing ergonomics, agent inspection and
audio/checker work remains in its original branches and worktrees; its presence
does not establish integration or runtime acceptance.

## Observed defects

The two selected baseline projects, native masters and viewing copies were hashed
again and matched `out/production-cycle-20261009/production/baseline/BASELINE.json`.

| Film | Baseline master SHA-256 | Evidence checked this cycle |
|---|---|---|
| Educational explainer, 1920 × 1080, 30 fps, 45 s | `03a27acff02c2350f6e8c6a5ba229fff6e7e62c89e3a85ead492de6086187fd2` | Caption text disappears at frame 72 (2.400 s) while its plate remains; adjacent frames 71 and 73 contain text. There are 17 unintended one-frame blanks and four authored pauses of 4, 6, 11 and 26 frames. |
| Caption-heavy vertical social, 1080 × 1920, 30 fps, 60 s | `d4b7207c6cf570d612e563ed92dc522413b77bab176fa85f90a65a62324779c2` | Text and plate disappear at frame 223 (7.433 s); inspected neighboring frames contain captions. The sweep identifies 20 one-frame gaps and one three-frame gap at frames 759–761. |

The caption sweeps and boundary stills are under
`out/production-cycle-20261009/production/repro/caption-blink/`. These counts
supersede the earlier sampled-still estimates. The initial short-gap sweep
described three authored pauses; exact baseline cue arithmetic also preserves
the 26-frame pause at 1189–1214. Longer authored pauses must remain intentional
choices; closing every gap is not acceptance.

The revised coverage sweep also exposed a separate social-video dropout at frame
956 (`478/15` s, about 31.867 s). Independent samples of frames 955–957 confirm
that caption 16 is active at 956 but its plate is absent and the text is invisible
against the page. This is a plate-alignment/contrast defect, separate from the
21 cue-gap groups. The repaired production aligns the separately authored
plates explicitly; importing captions does not retime existing plate clips.

The media-placement audit found five distorted monitor/card clips in the
explainer and 31 manually compensated clips across trailer (12), vertical (16)
and caption-heavy social (3). Those counts come from a static project audit,
not a new rendered measurement. The accepted [media-fit design](../design/2026-10-08-media-placement-fit.md)
defines native source coordinates and separate output coordinates. A 1280 × 534
source contained in 1920 × 1080 should occupy 1920 × 801 pixels, centered at
`y = 139.5`, instead of stretching to the full output rectangle.

## Reviewed implementation and integration

The engine slice at `e44a43f30585db55c1641c4b7fb47f2c11f5fd1a` has independent
technical acceptance. It implements native placement, source/output dimensions,
cache preservation, proxies, nesting and bounded tracking/interchange support.
The [placement report](2026-10-09-media-placement.md) records its 535 workspace
tests, eight delivery tests, two ignored tests, deterministic before/after
masters and unchanged demo checks. The simplified card and monitor retain their
widths while correcting their picture heights from 216 to 160.2 and 360 to 267
pixels, respectively. The decoded measurements include normal filter fringes.

The corrected caption slice at `7d41f5e88c37fbcbff4947933c5504d0e99c9e9e` has
independent static and targeted-still acceptance. Caption import now ceiling-snaps
cue boundaries, closes gaps up to and including 1/10 second by default, and offers
opt-in minimum duration or exact-timing mode through CLI/MCP. It never adds an
output frame. Review caught and corrected a collapsed final cue that previously
could extend beyond the program. The real CLI reproduction changes blank frames
31 and 73 into the expected captions, matching their visible neighboring frames.
Separately authored plates still require explicit alignment.

The combined candidate is `bda8a2153be04d76bd1262c420a131b563765e6f` on the existing
feature branch. Formatting and all-target Clippy passed, as did **599 workspace
tests with two ignored doctests**, **eight delivery unit/bin tests**, and release
builds of all four local binaries. The serial CPU suite uses lavapipe. Conditional
OpenH264 delivery is not established by those test counts; no codec download was
enabled.

Unchanged render CI passed at this combined revision: 288/312 frames, exact
reference video and audio hashes, sampled SSIM mean/minimum 1.0, passing quality
checks, and byte-identical fresh-cache outputs with one and two workers. No
reference was updated. All nine reference MCP workflows passed through real
subprocesses and rendered outputs, using `--agent none`; no model worker or
creative score is involved. Independent Codex review accepted this combined
technical milestone, which is pushed on [PR #1](https://github.com/gtwatts/ferrocut/pull/1).

The project installer installed that revision with a recoverable backup. The
existing Codex configuration remained byte-identical. A fresh subprocess launched
through its actual configured MCP command/root/cwd verified the four installed
binary hashes, native fit reporting, caption dry-run/import/undo, relative font
storage through the inline MCP style route, and rejection of an outside-root
subtitle file. Production separately reproduced absolute font storage through
the CLI style-file route; that remains a defect. CLI discovery exposes the
new timing flags. The CLI SHA-256 is
`e9ab2ea09c7ad040cea8560ed150e874166e566096093aafcbdbbe2d6bd38d95`; MCP is
`d7fc44c79ec5ca837afcfc2de5ab42b5f4c3bff941e63232b50cec5581e8eed3`.

This proves a fresh configured process, not reload of already-running agent
sessions. GitHub CI also passed formatting, Clippy, tests and CPU render checks
at this revision. Local evidence
is under `out/production-cycle-20261009/integration/`, with independent source
acceptance at `review/integrated-20261009/source-acceptance.json` under the same
cycle directory.

The [four-class benchmark](../../eval/creative/benchmark/BENCHMARK.md) includes
reusable caption and placement regressions. Its caption contrast checker rejects
blank, dropped-frame and truncated controls; it is not a text-presence detector.
The packaging helper rejects escaping fallback fonts and preserves distinct
same-named source media. Independent controls bind both helpers to exact hashes.

Original and relinked working projects rendered identical RGBA pixels at all nine
sampled frames: explainer 0/72/300/774/1349 and social 0/223/956/1799. This is
sampled-frame equivalence, not proof for the entire film. Font paths affect text
cache keys, so cache identity cannot establish relocation equivalence here.

## Delivered films and comparisons

All paths in this section are relative to
`out/production-cycle-20261009/production/`. Native masters were rendered through
the installed CLI on the NVIDIA Vulkan adapter with two workers, one film at a
time. H.264/AAC viewing copies use the repository's LGPL FFmpeg and existing
NVIDIA encoder after a successful two-second encode/decode probe. No codec was
downloaded; this is not the native OpenH264 delivery route.

| Film | Viewing copy | Standalone editable project | Frames / duration |
|---|---|---|---|
| Repaired explainer | `rerender/explainer-16x9/delivery/explainer-16x9-repaired.mp4` | `deliveries/explainer-16x9-editable/project.json` | 1350 at 30 fps / 45 s |
| Repaired caption-heavy social | `rerender/vox-style-ferrocut-9x16/delivery/vox-style-ferrocut-9x16-repaired.mp4` | `deliveries/vox-style-ferrocut-9x16-editable/project.json` | 1800 at 30 fps / 60 s |
| New fictional Vessa commercial | `commercial/delivery/commercial-16x9.mp4` | `deliveries/commercial-16x9-editable/project.json` | 360 at 24 fps / 15 s |

Each viewing copy has a neighboring `master.mkv`. Full output SHA-256 values:

| Film | Native master | Viewing copy |
|---|---|---|
| Explainer | `72239199f53ae486eaa545a27522b4f0f70de8e1a0bdee424bf99197622e3bb1` | `699fdc71a8b7f3a3b29c39d1ea67828469412c06274c595c6c7152cc592604db` |
| Social | `93bda5cb64dfc752c263dfb38dcbc3184649f56b16698a2f95494362c3e00c8c` | `476ad477b7fbded1a1d7f2466e362f7fab67092209550c8c48387f5696d3556a` |
| Commercial | `d4340211a3dc0f9665bbb430ecb81b17b26e131d021f76554b0729e81005e475` | `50815ad2b085ef33c1a0b77297c1e8050b7f37795b236216ec8180b2dfe35ae4` |

### Explainer

The repaired explainer is 1350 frames at 30 fps, exactly 45 seconds. Its native
master SHA-256 is `72239199f53ae486eaa545a27522b4f0f70de8e1a0bdee424bf99197622e3bb1`;
the viewing copy hash is recorded above.

Independent review decoded 77 after and 14 before samples, read all 17 former
blink caption crops, and inspected the retained pause boundaries. The baseline
and repaired masters decode to identical PCM SHA-256
`2f38326b402642c8395ef2a1978638991b01ff102da90f3ec7955546503e23d9`.
Measured master/viewing audio is −16.0 LUFS integrated, LRA 1.2 and −1.5 dBTP.
These are measurements, not listening acceptance. Independent review accepted
the technical repair and sampled output evidence in
`review/integrated-20261009/explainer-output/acceptance.json` under the cycle root.

The check using the brief's explicit −16 LUFS target passes. The render's automatic
check defaults to −14 LUFS and fails that policy; both reports are retained.
The audio was not changed to satisfy the wrong target. Initial thresholded
placement measurements confused dark picture content with its extent and swapped
two clip labels; they are retained as raw measurements, not geometry proof.

### Caption-heavy social

The repaired social film closes all 21 short cue-gap groups: 20 single frames
and frames 759–761 (25.300–25.367 s), for 23 formerly blank frames. Frame 223
(7.433 s) is now captioned, and the separate plate/contrast dropout at frame 956
(31.867 s) is repaired. Four intentional pauses remain. Twenty-seven per-cue
plates became five run plates with fades outside caption coverage. Source review
finds exactly one fully opaque plate under all 1477 cue-active frames; the first
and last frames of every run are included in the inspected samples.

The coverage sweep's original wide band included a plate edge: a no-text frame
115 could exceed its threshold. The corrected interior band, `(110,1410)` to
`(970,1540)`, rejects that plate-only control, fails the baseline and passes the
repair with all 23 legacy cue-gap probes visible and no covered blank frames.
Both measurements are retained. Even this corrected contrast test is not OCR
or a general legibility check; decoded caption samples remain necessary.

Independent review decoded 102 repaired and 14 baseline samples, inspected all
23 former cue-gap frames, frame 956 and all ten run endpoints, and accepted the
bounded technical repair. Its receipt is
`review/integrated-20261009/vox-output/acceptance.json` under the cycle root.
Independent native PCM comparison is identical to the baseline. Measured native
audio is −14.0 LUFS, LRA 3.5 and −1.5 dBTP; the AAC viewing copy peaks at −1.1
dBTP. Default-target QC passes. The missed-cut warning count falls from 50 to 26
after removing the per-cue plate boundaries; this is not a checker implementation
fix or proof that every remaining warning is false.

### Commercial

The new 15-second fictional Vessa lantern spot supplies the previously missing
commercial benchmark. It uses local licensed Sintel footage, an original editable
vector lantern/nested composition, original procedural audio and an attributed
fictional end card. Sampled frames show product entry, dimmer, locked callout,
glow/rays and end-card placement. The optional square cut-down was not produced.

Three journaled authoring revisions corrected unintended white fills, repeater
offsets and rays failing to follow the lantern. The white fill also exposed a
parameter-registry mismatch; the repeater offset is a discovery/example issue.
Native `cover` replaces per-axis footage compensation. Measured audio is −16.0
LUFS, LRA 3.1 and −1.06 dBTP in the master; the AAC viewing copy measures −16.1
LUFS and −1.1 dBTP. Explicit brief-target QC passes with
one overlay-boundary warning at frame 315 (13.125 s); the automatic −14 LUFS
failure is retained. No sound or motion judgment is inferred from these checks.

Independent review inspected 46 master and three viewing-copy frame samples and
measured the 2.5-second transient and audio tail. Its bounded technical acceptance
is `review/integrated-20261009/commercial-output/acceptance.json` under the cycle
root; this does not establish continuous motion pacing or listening quality.

### Placement and editable packages

Five explainer media clips use native contain placement. The two social footage
clips and its 940 × 828 evidence image use uniform scales; the image's intended
extent is about 862 × 759.29 pixels. Clip-on/off sampled differences measure
386 × 162 for the explainer card, 628 × 262 for two monitor samples, 202 × 86 for
both social footage samples, and 864 × 760 for the evidence image, including
filter fringes. The other two monitor samples include adjustment-layer glow;
their composite bounds are diagnostic, not precise photo geometry. See
`rerender/PLACEMENT-MEASUREMENTS.md` for exact frames and methods. The trailer and
original 30-second vertical film remain dated baselines with their 28 existing
per-axis compensations; this cycle does not claim to migrate those films.

Each editable package includes referenced media/fonts, nested compositions,
licenses, journaled relinks, `PACKAGE.md` and `MANIFEST.sha256`. Independent review
decoded all 20 retained same-adapter source/package pairs as RGBA and found exact
pixel equality: seven explainer, seven social and six commercial frames. It also
verified the 46 recorded successful render/decode commands. Package references
and normalized timelines/assets pass inspection. Full OFL notices, CC-BY license
URIs, source attribution and edited-excerpt notes were checked. The historical
contract and social source notes now correctly identify `d1.mkv` as Sintel. This
is sampled equivalence, not an entire-film render comparison.
Imported absolute CLI font paths required a
journaled relative-path repair first; six same-adapter before/after samples are
identical. The path-sensitive text cache still causes unnecessary re-rendering
after relocation, so cache keys are not the equivalence oracle.

The final package acceptance is
`review/integrated-20261009/package-acceptance.json` under the cycle root. It binds
the complete manifests below; six additional before/after relative-font repair
samples were independently decoded and also match exactly.

| Editable package | Manifest SHA-256 | Manifest files |
|---|---|---|
| Explainer | `5155d06d71ff5afac8bb769961a742cab787642710e6e07eb15da0542060b5dd` | 187 |
| Social | `a8b31bb385795b2cb438b8f07ce21ab31ebf303b72dc6f2f8e4bab27865fb3cd` | 218 |
| Commercial | `593caf94f26b8bbce4c3c15ef0c313ee830cd2c1e31a10ddbae5d7e99c2ca1d7` | 69 |

## Remaining quality limits

The CLI style-file font-path defect and checker ignoring timeline loudness targets
remain concrete follow-up defects. The former required 49 font-path edits in these
two films; the latter rejects correctly normalized −16 LUFS films by default.
Neither is concealed by the successful workarounds. Native run-plate authoring,
non-ripple caption replacement and content-based font caching also remain open.

Audio duck pumping, incomplete audio observability, text plate/layout ergonomics,
and checker false positives remain demonstrated or reported follow-up work from
the [previous production evaluation](2026-10-08-claude-video-tests.md). Existing
checker work is not accepted merely because it is present in a worktree. A checker
pass does not excuse visible caption dropouts, and authored pictures must not be
changed only to satisfy an incorrect heuristic.

The evidence above consists of source inspection, signal measurements and sampled
frames. Continuous playback and listening have not been claimed. Gordon's viewing
and listening judgment remains the final creative acceptance.
