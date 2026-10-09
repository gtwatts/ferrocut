# Quality checker: grading against the master's own loudness target

Status: **combined source/build and installed consumer checks accepted.** The checker now grades a master against the target recorded by the render, with explicit source reporting.

## Behavior

Previously, ferrocut check, render --check and MCP quality_check defaulted to -14 LUFS ±1 LU and -1 dBTP, ignoring the render report's authored target and ceiling. A correctly rendered -16 LUFS master could fail, and a master with an authored -1.5 dBTP ceiling could be judged against -1 dBTP.

Each threshold now resolves independently, in this order: explicit flag, keys present in config, render report, timeline metadata for older reports, then the default. An explicit render null target means normalization is off and uses the default. Invalid recorded values are errors. The report and CLI/MCP output identify threshold sources, and a timeline edited after rendering produces a non-failing loudness_target_mismatch warning.

The measurement algorithms and default ±1 LU tolerance remain unchanged. Silent audio still fails loudness_off_target; missing audio remains missing_audio when required. The resolver is in [targets.rs](../../crates/ferrocut-perceive/src/targets.rs), with grading in [check.rs](../../crates/ferrocut-perceive/src/check.rs).

## Accepted evidence

Exact source 69c75cbdf7e47e1667865a30e1f106989adebcb7 passed formatting, all-target Clippy, 637 Cargo-reported workspace passes with two ignored doctests, eight delivery tests, release builds and unchanged render CI. Focused evidence covers seven target-provenance tests, 23 checker library tests, nine engine wrapper tests, GPU-schema tests on the CPU Vulkan driver, MCP schema/stdio tests, and the earlier accepted source reviews.

The fresh quality report measured 13 seconds and 312 frames, detected cuts at 120 and 240, integrated loudness -14.0 LUFS and true peak -2.18 dBTP. It resolved the -14 LUFS and -1 dBTP targets from the render and the ±1 LU tolerance from the default, with no problems or warnings. Existing one/two-worker CI masters remained byte-identical: 288-frame demo output and 312-frame demo-av output, 25 SSIM samples at 1.0, and exact reference video/audio digests.

## Installed consumer proof

The fresh configured installed CLI and MCP routes passed an authored -16 LUFS check and failed an explicit -14 LUFS override with the expected flag/default problem provenance. Both routes preserved the render-owned target, default tolerance and true-peak ceiling. Installation preserved configuration bytes, four recoverable binary backups and the existing Whisper executable/model links. The fresh MCP process exited cleanly; no existing-harness reload is claimed.

The pre-existing relative chunk_dir limitation remains: when a render made with a relative output path is checked from a different working directory, the checker cannot find the chunks. The accepted installed proof used absolute output/cache paths as a workaround and does not claim a fix.

## Boundaries

The combined run does not establish successful hardware parity, optional OpenH264-positive branches, universal codec/container behavior, normal-speed playback, listening or creative acceptance. The legacy unfinished checker redesign remains separate.
