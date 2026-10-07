# cutline-ofx

OpenFX (1.5) image effects as Cutline render nodes, **out of process**.

```
Cutline (Rust)                                   cutline-ofx-host (C++, one per worker+node)
OfxNode::render ──f16→f32──► /dev/shm/…-src ◄──mmap──┐
                 ◄─f32→f16── /dev/shm/…-dst ◄──mmap──┤  OpenFX HostSupport + plugin .ofx (dlopen)
        stdin  ── "RENDER\t…" ───────────────────────►│
        stdout ◄─ "OK\t<output premult>\trendered" ───┘   (plugin printf goes to stderr, never the protocol)
```

* **Isolation.** Plugins never load into the Cutline process. A segfault, abort, `exit()`, hang (render timeout, then SIGKILL) or external kill (OOM killer) becomes an `OfxError` (`HostDied` with the signal name and the tail of the host's stderr, or `Timeout`) and then a `NodeError` for that node only. The next render starts a fresh host. The host disables core dumps for itself (`RLIMIT_CORE=0` + `PR_SET_DUMPABLE=0`; set `CUTLINE_OFX_CORE_DUMPS=1` to keep them), so a crash costs milliseconds rather than a systemd-coredump capture. Leaky plugins are contained by `HostConfig::max_rss_bytes`: the host is recycled once its RSS passes the budget.
* **Frames.** Shared memory is a file under `/dev/shm` mapped by both sides; the pixels are RGBA f32, tightly packed, top row first. The host hands plugins a pointer to the bottom row with a **negative rowBytes** (legal in OFX), so the vertical flip costs no copy. Frames are always declared to plugins as `kOfxImagePreMultiplied`. The output clip's premultiplication comes from the plugin's clip preferences: `UnPreMultiplied` output is re-premultiplied in Rust and `Opaque` output gets alpha = 1, so frames leaving the node are always premultiplied.
* **Time.** The node gets a `RationalTime` and its clip `FrameRate`; the host gets `t` and `fps` as exact integer fractions and converts to the OFX double frame number only at the API boundary.
* **Depth.** The host offers float RGBA only. A plugin that can't render float returns a clean error.
* `OfxNode::new` starts a host once to check that the plugin exists. Its content hash covers the node version, plugin id plus version, context, frame rate and params.

## Control protocol (v1, tab-separated lines)

`HELLO`, `LIST`, `LOAD <id> <context>`, `PARAM <name> <v…>`, `RENDER <t num> <t den> <fps num> <fps den> <w> <h> <src shm> <dst shm> <src premult>` → `OK <out premult> rendered|identity`, `QUIT`. Errors: `ERR <msg>`. See `host/src/main.cpp`.

## Build

Requirements: a C++17 compiler, CMake ≥ 3.16, git (network access **once** to fetch the sources). No sudo or system `-dev` packages.

```sh
crates/cutline-ofx/scripts/fetch-deps.sh     # clones pinned OpenFX + libexpat into third_party/ (git-ignored)
cargo test -p cutline-ofx -- --nocapture --test-threads=1
```

`build.rs` uses the `cmake` crate to build `host/` (HostSupport, expat, `cutline-ofx-host` and the plugin bundles) into `OUT_DIR`, and exports `CUTLINE_OFX_HOST_EXE` and `CUTLINE_OFX_BUNDLED_PLUGINS` to the crate. At runtime `CUTLINE_OFX_HOST=<path>` overrides the host binary. Without the fetched sources, `build.rs` warns and builds a stub (`cfg(cutline_ofx_no_host)`).

## Plugins built here (all permissive, nothing GPL)

* `Invert.ofx.bundle`: `net.sf.openfx.invertPlugin`, the OpenFX SDK's Support-library Invert example (BSD-3-Clause), built from the pinned SDK.
* `CutlineTest.ofx.bundle` (`host/plugins/cutline_test_plugins.cpp`, ours): `org.cutline.test.Unpremult` (declares an unpremultiplied output clip), `.Crash` (SIGSEGV), `.Abort` (SIGABRT), `.Hang` (never returns) and `.Leak` (64 MiB per render).

openfx-misc is GPL-2.0, so it is deliberately not used or vendored.

## Pinned versions / licenses

| Component | Version | License |
|---|---|---|
| OpenFX SDK (headers, HostSupport, Support lib, Invert example) | `OFX_Release_1.5.1` | BSD-3-Clause |
| libexpat | `R_2_8_5` | MIT |
