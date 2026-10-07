# ferrocut-html

HTML/CSS/JS layers for Ferrocut, rendered by Chromium through the
[Chromium Embedded Framework](https://bitbucket.org/chromiumembedded/cef)
(CEF, BSD-3-Clause) in **offscreen (windowless) mode**, in a separate host
process. `HtmlNode` implements `RenderNode`. The frame at graph time `t` is a
function of `(page, params, t)` only: every clock the page can observe follows
the graph's `RationalTime`, never the wall clock.

## Build

```sh
crates/ferrocut-html/scripts/fetch-cef.sh   # once: pinned prebuilt CEF (~400 MB download, ~1.5 GB unpacked)
cargo build -p ferrocut-html                # build.rs builds host/ with CMake into OUT_DIR
cargo test  -p ferrocut-html
```

| Component | Version | License | Where |
|---|---|---|---|
| CEF minimal distribution, Linux x64 | `154.0.34+g14c5a08+chromium-154.0.8037.98` (sha1-pinned in `fetch-cef.sh`) | BSD-3-Clause (CEF), Chromium's BSD-style + third-party notices | `third_party/` (gitignored) |

Without CEF, the crate builds as a stub (only `color` is exposed) so fresh
clones stay green. `FERROCUT_CEF_ROOT` points the build at another CEF
distribution. `FERROCUT_HTML_HOST` overrides the host binary at run time.

The host's runtime files (`libcef.so`, paks, ICU, V8 snapshot, locales) are
**relative** symlinks next to the executable, and its RPATH is exactly
`$ORIGIN`. CEF's default `-Wl,-rpath,.` is removed, so libraries are never
loaded from the current working directory, and moving the checkout does not
break anything.

## Determinism model

The browser normally takes time from three independent sources: the
compositor's frame clock, DevTools virtual time (timers, `Date`), and
`performance.now()` (which Chromium also jitters with a random per-process
secret). Ferrocut pins all three:

1. **Virtual time, paused at a fixed epoch.** Before navigating, the host sets
   `Emulation.setVirtualTimePolicy {policy: pause, initialVirtualTime:
   2026-01-01T00:00:00Z}` through CEF's DevTools message API. Afterwards virtual
   time only moves through `pauseIfNetworkFetchesPending` budgets. Loading
   advances it 1 ms at a time, waiting in real time for the browser to see the
   load event after each step, so the virtual load time is independent of
   machine speed (measured: 1 ms for the fixture in every run). After that,
   frame `k` is reached with budgets that sum to exactly
   `round(k / fps * 1e6)` µs. `setTimeout`, `setInterval` and `Date` follow this
   time. Resources in flight freeze virtual time, so network latency cannot
   change what the page sees.
2. **External BeginFrames.** The browser is created with
   `external_begin_frame_enabled`. Nothing paints unless the host calls
   `SendExternalBeginFrame`.
3. **An injected time shim** (`host/src/shim.h`, installed with
   `Page.addScriptToEvaluateOnNewDocument` before any page script runs).
   `SendExternalBeginFrame` stamps frames with the browser's wall clock, so
   the shim takes over everything frame-driven:
   - `requestAnimationFrame` callbacks run exactly once per graph frame, with
     the exact graph time as their timestamp.
   - Every CSS animation/transition and Web Animation is paused. Its
     `currentTime` is set to (graph time − graph time of the first frame that
     saw it), which is browser semantics on the frame grid.
   - `<video>`/`<audio>` are muted, paused and seeked to their age (best
     effort, see below).
   - `performance.now()` returns the exact graph time during a step, and
     virtual `Date.now()` minus the epoch between steps. That replaces
     Chromium's jittered clamp.
4. **Every grid frame is stepped.** CSS animations start at the first frame
   that sees them, so state at frame `k` must not depend on where rendering
   started. The session steps every grid frame from 0 to `k` and captures only
   `k`. A backward seek respawns the host and replays from 0. Graph times
   between grid points snap to the nearest grid frame (`HtmlParams::fps`,
   normally the sequence rate).
5. **Commit-fenced capture.** OSR paints lag the main thread by a variable 2–4
   BeginFrames (and can be stale, CEF issue #4166). `CAPTURE` first awaits
   `__ferrocutFence()`, which resolves in the second native rAF after the
   step, i.e. once the frame carrying the step's DOM changes has committed.
   The host keeps sending BeginFrames until then, and accepts a frame once
   ≥ 3 post-fence paints were seen and the last two are byte-identical.
6. **Process flags** (`host/src/main.cpp`):
   - Rendering: software compositing (`--disable-gpu`,
     `--disable-gpu-compositing`), `--ozone-platform=headless` (no X/Wayland
     needed), and `--run-all-compositor-stages-before-draw`.
   - Threads and timing: threaded animation/scrolling and checker-imaging are
     off, as are PaintHolding and background throttling.
   - Output color and text: `--force-color-profile=srgb`, no LCD text, no
     subpixel positioning.
   - Randomness: a fixed V8 `--random-seed`, so `Math.random()` repeats run
     to run.

## Color

Chromium composites in encoded sRGB and paints BGRA8, premultiplied. By default
(`OutputEncoding::AcesCg`) the node unpremultiplies, applies the exact sRGB
EOTF, converts Rec.709 → ACEScg with OCIO 2.5's matrix, re-premultiplies, and
tags the frame `ACEScg`. This is the same table-driven conversion as
ferrocut-lottie. `SrgbEncoded` passes the encoded values through, tagged
`sRGB Encoded Rec.709 (sRGB)`, for an OCIO node to handle. The page
background is transparent unless the page paints one.

## Errors

| Situation | `NodeError` kind |
|---|---|
| Host or renderer crash, OOM kill, hang (timeout), protocol confusion | `Retryable`. The next render starts a fresh host and replays to the frame. |
| Missing host binary, page load failure, script exception in a step, paint never settled | `Permanent` |
| `CancelToken` / deadline (checked between steps of a pre-roll) | `Cancelled` |

## Network policy

By default a page can load only:

- `file://` URLs under its own directory (symlinks are resolved, so a link
  pointing outside doesn't count), plus any `NetworkPolicy::extra_roots`;
- in-memory `data:`, `blob:` and `about:` URLs.

Everything else is refused: `http(s)`, `ws(s)`, `file://` outside the roots,
popups, navigations away, and OS protocol handlers. A refused request fails
in the page the same way every run. `fetch` rejects; images, scripts and
stylesheets fire `error`; XHR ends with status 0; EventSource and WebSocket
fire `error`. No connection is attempted, so frames can't depend on the
network.

```rust
use ferrocut_html::{HtmlNode, HtmlSource, NetworkPolicy};
// Default: no network, page dir only.
let node = HtmlNode::new(HtmlSource::File(page), params)?;
// Also allow loading shared assets from another directory (hashed too).
let node = HtmlNode::with_policy(HtmlSource::File(page), params,
    NetworkPolicy { allow_remote: false, extra_roots: vec![assets_dir] })?;
```

A remote page URL with `allow_remote: false` is rejected when the node is
built (`Permanent`). So is a missing extra root.

How it's enforced (in `host/src/main.cpp`):

- `NetGuard`, a `CefResourceRequestHandler` installed both per browser and as
  the default request-context handler, cancels requests in
  `OnBeforeResourceLoad`. That covers sub-resources, fetch/XHR, fonts,
  EventSource, beacons and workers.
- `OnBeforeBrowse`, `OnOpenURLFromTab` and `OnBeforePopup` cancel
  navigations.
- `OnProtocolExecution` refuses OS handlers.
- **WebSocket:** CEF 154 has no hook for WebSocket handshakes. In deny mode
  they are stopped instead by `--host-resolver-rules="MAP * ~NOTFOUND"`,
  which (verified by the test) also covers IP literals and `localhost`.
  WebSockets therefore don't show up in `BLOCKED`.
- `HtmlSession::blocked_requests()` (host command `BLOCKED`) lists what was
  refused, which helps when a page renders incomplete.

The host reads the policy from the environment (`FERROCUT_HTML_NET`,
`FERROCUT_HTML_FILE_ROOTS`; see the header of `main.cpp`).

### Node hash

The hash (`ferrocut.html/2`) covers:

- the page's file name and bytes;
- the params and the policy;
- a digest of **every file under the allowed roots**: `(relative path,
  blake3)`, sorted, with absolute paths left out.

The page can load nothing else, so editing an image, stylesheet, font or
script it uses changes the hash, while moving the project keeps cache keys.
Roots holding more than 20,000 files or 2 GiB are an error: give the page its
own directory. With `allow_remote` on, remote content is outside the hash.

The digest is a `ferrocut_types::FileManifest` per root. The node reports
`AccessPattern::Sequential`, so the scheduler keeps each worker on contiguous
chunks (a backward seek replays the page from frame 0).

## Tests

`tests/network.rs`:

- `remote_and_outside_requests_are_blocked_deterministically` runs 16 probes
  against a local TCP listener:
  - fetch, XHR, img, CSS, script, WebSocket (IP and `localhost`), FontFace,
    EventSource, iframe, beacon, a DNS name;
  - `../outside` and absolute `file://` outside the page dir;
  - an allowed local image.

  Each probe paints a square green on the expected outcome. `gate.js`
  re-inserts itself until every probe has settled, which holds back `load`
  (and therefore OPEN), so frame 0 already shows every outcome without
  waiting on a clock. The test asserts:
  - the listener accepts **0 connections**;
  - all squares are green;
  - the `BLOCKED` list matches exactly;
  - frames and lists are byte-identical across two hosts.
- `allow_remote_reaches_the_listener`: the control. With `allow_remote`, the
  same listener sees the fetch and the WebSocket, but files outside are still
  refused.
- `extra_roots_allow_files_and_feed_the_hash`: editing a sub-resource in the
  page dir or an extra root changes the node hash.
- `bad_policies_fail_permanently_up_front`

`tests/determinism.rs`:

- `two_runs_render_identical_frames`: 45 frames from two independent hosts
  are byte-identical, and all 45 are distinct (the page really animates).
- `chunk_boundary_matches_sequential`: a fresh host starting at frame 18
  matches the sequential render. Three chunks rendered in reverse order on
  one session also match, with 3 host spawns.
- `every_clock_follows_graph_time`: CSS animation, rAF, `setInterval`,
  `Date.now` and a `setTimeout`-triggered CSS animation each move by graph
  time (invisible at 466 ms, visible by 800 ms). The background stays
  transparent.
- `host_crash_is_a_clean_retryable_node_error_and_recovers`: SIGKILLing
  the host mid-pre-roll yields `Retryable` "html host died during STEP:
  killed by SIGKILL (9)". The next render recovers byte-exact.
- `missing_host_or_bad_page_are_permanent`
- `render_node_through_core_on_gpu`: GPU upload round trip is bit-exact,
  and a cancelled token gives `ErrorKind::Cancelled`.

The fixture is `tests/data/anim.html`.

## Security notes

- **No Chromium sandbox.** The host runs with `no_sandbox = true`. CEF's
  sandbox needs `chrome-sandbox` installed setuid root (or unprivileged user
  namespaces), and we don't run sudo. Page JavaScript therefore runs in an
  unsandboxed renderer with the user's privileges. Isolation is only the
  separate process tree. **Only render trusted pages** until the sandbox is
  enabled (needs `sudo chown root:root chrome-sandbox && sudo chmod 4755
  chrome-sandbox`, or an AppArmor/userns policy).
- **Network and files: blocked by default.** See "Network policy" below.
- The host disables core dumps (`RLIMIT_CORE=0`, override with
  `FERROCUT_HTML_CORE_DUMPS=1`). Each host gets a client-owned temporary
  Chromium profile that is removed even if the host was killed.

## Limitations / follow-ups

- **GPU path.** Frames come back through a CPU paint buffer (shared memory),
  then get uploaded. Follow-up: CEF's accelerated OSR
  (`shared_texture_enabled`, `OnAcceleratedPaint` dmabuf) imported into wgpu
  via Vulkan external memory. That needs GPU compositing, which in turn needs
  re-validating determinism.
- **Cross-machine determinism.** Text rendering depends on installed fonts
  and fontconfig. Results are byte-identical on one machine, not guaranteed
  across machines. Bundle fonts with the page for portable renders.
- **Media.** CEF's minimal build has no proprietary codecs (no H.264/AAC).
  WebM/VP9 and Ogg play. Frame-accurate `<video>` seeking is best effort.
- **Pre-roll cost.** About 1–2 ms per grid frame (STEP), plus about 5 ms per
  capture. Rendering frame 3000 from a cold host is several seconds. Engine
  chunking should keep chunks contiguous per worker (each worker's session
  keeps its host).
- WebGL is unavailable with software compositing. Canvas 2D works.

## Debugging

`FERROCUT_HTML_DEBUG_PAINTS=1` makes the host log a hash of every OSR paint
during `CAPTURE` to stderr. That shows the paint pipeline lag and when the
commit fence resolves. The host protocol is plain text over stdin/stdout, so
it can be driven by hand.

## Layout

- `host/`: the C++ CEF host (protocol in `main.cpp`'s header comment) and
  the JS time shim (`shim.h`).
- `src/host.rs`: host config, version handshake and `HostSpec`. Process
  supervision, the line protocol, timeouts, the stderr tail, the private
  profile dir and the shared-memory buffer come from `ferrocut-ipc`.
- `src/session.rs`: frame grid, budgets, pre-roll, and respawn on seek or
  crash.
- `src/policy.rs`: `NetworkPolicy`, canonical roots, the host environment
  and the page's `FileManifest`.
- `src/color.rs`: BGRA8 premultiplied → ACEScg f16 (math in `ferrocut-colorspace`).
- `src/adapter.rs`: `HtmlNode`. This is the only module touching
  ferrocut-core.
- `scripts/fetch-cef.sh`: the pinned CEF download.
