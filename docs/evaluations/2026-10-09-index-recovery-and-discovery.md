# Transcript index recovery and installed Whisper discovery

Source, combined checks and installed-route results below bind revision `df57e8debad37c48c07dc724971fb5d09c5b1537`. Combined source and fresh installed CLI/MCP probes passed independent review; their limits are recorded below.

While indexing narration with an installed `ferrocut`, transcription was unavailable and stayed unavailable after the
environment was partly fixed, so the index cache was deleted by hand. Reproduction narrowed the cause. Two repairs
followed: the index cache no longer keeps stale or transient transcription failures, and the installer exposes an
existing checkout's whisper.cpp build to the installed runtime.

## What actually happened (before)

Transcription needs `whisper-cli` and a ggml model. The index cache key covers the media bytes and, when both are
found, the model's content hash, language and decoding flags. When either was missing, every missing-dependency state
shared one key, and a cache hit returned the reason saved by the first run.

- Fixing only the CLI path (model still missing) hit that key and still answered "whisper-cli not found". Fixing the
  environment looked useless.
- Fixing **both** the CLI and the model already changed the key and transcribed. The broader claim that every repaired
  environment stayed pinned was wrong; the defect was the stale shared reason.
- Separately, a transcription error other than a non-zero whisper exit (unparsable or missing output, an audio decode
  failure) was cached under the valid model key and returned until the cache was deleted.
- An installed runtime (`~/.local/share/ferrocut/versions/<version>/`) holds the binaries, and its `third_party/`
  holds only the FFmpeg libraries. Discovery
  searches `third_party/` above the executable and above the working directory, so an installed `ferrocut` found the
  checkout's whisper only when run from inside the checkout. The configured MCP server runs there; the CLI from other
  directories did not find it.

## After

Index cache (engine):
- A cached entry whose key means whisper or its model is missing reports the **current** reason. Once both are found,
  the key changes and the media is transcribed; no cache deletion is needed.
- No transcription error is cached. An entry left by an older build, holding an unavailable transcript under a key
  whose whisper and model now exist, is retried by normal indexing, keeping its cached shot list, and rewritten on
  success. `transcript_search` and `shots_list` index on demand the same way. Only the engine's cached-only read
  returns the cached result without transcribing (a missing-dependency reason is still refreshed).
- Successful transcripts, including an empty one (no speech), are cached and reused as before. The cache schema is
  unchanged.
- An explicit `--whisper-cli`/`--model` or `FERROCUT_WHISPER_CLI`/`FERROCUT_WHISPER_MODEL` keeps precedence over
  discovery; a path that does not exist is an error naming it. The not-found messages list the directories searched
  and note that an MCP server reads those variables when it starts.

Installer (`scripts/install-local.py`):
- When the checkout has them, the new runtime gets symlinks at exactly the paths discovery searches:
  `third_party/whisper.cpp/build/bin/whisper-cli` (only if executable) and each of `ggml-medium.en.bin`,
  `ggml-medium.bin`, `ggml-small.en.bin`, `ggml-small.bin` that exists in `third_party/whisper-models/`. Nothing else
  from `third_party` is linked; nothing is copied or downloaded, and no model is bundled.
- Whisper is optional: without it nothing is linked and the install is otherwise unchanged. `--check` and
  `install.json` report the links. Each runtime directory has its own links, so other installed versions and the
  existing backup/rollback are untouched; reinstalling the same build refreshes its links and drops ones whose file
  left the checkout.
- The links are absolute and point into the checkout. Moving or deleting the checkout leaves them dangling; discovery
  then skips them and reports the variables to set. They are not portable.
- Order: explicit option, then environment variable, then discovery. Discovery walks the model names in the order
  above (the outer loop); for each name it checks above the executable (the runtime links) before above the working
  directory. So a better-ranked model above the working directory can win over a lower-ranked linked one. The CLI has
  one name: runtime link, then working directory, then `PATH`.

## Using it

```sh
# Transcribe (or read back) the index of a file; the summary says built or cached.
ferrocut index narration.wav --no-shots
# Explicit tools win over discovery (a missing path is an error naming it).
ferrocut index narration.wav --whisper-cli /path/to/whisper-cli --model /path/to/ggml-small.bin
# Review what the installer would link, without writing.
python3 scripts/install-local.py --check     # see "whisper": {"cli": ..., "models": [...]}
```

Over MCP, `index_media {"media": "...", "transcribe": true}` returns the transcript status and reason; the server's
discovery uses its own environment and working directory, fixed at launch.

## Evidence

Focused source evidence (reviewed):
- Index, branch `eng/index-unavailable-recovery` @ `de14c03` (integrated as `c44c60f`, `7767338`, `238f5ae`, `04c686d`):
  9 index tests pass. The three new tests fail on the base source (`3ad8372`). Clippy and formatting clean. The real
  whisper test skipped (its clip was absent). An installed-build reproduction with a fake whisper showed both
  defects; the branch binary passed the same controls outside any checkout: partial fix reports the current reason,
  full fix transcribes, success is reused, unparsable output is not cached, an entry pinned by the installed build is
  recovered.
- Validation note: a first run of that control was placed inside the checkout, where discovery found the checkout's
  real whisper and transcribed a 1 s synthetic tone once. That run was excluded as invalid and repeated outside the
  checkout; it is not part of the evidence.
- Installer, branch `eng/installed-whisper-discovery` @ `759e29d` (integrated as `8267aba`, `e532e8c`, `16efdda`;
  guidance aligned in `965b1eb`): 5 installer unit tests with `Path.home()` mocked; against the base installer four
  error on the missing new `whisper` summary field and one fails behaviorally (no links); 5 asserted installed-layout probes with a copied installed runtime and a fake whisper, outside any
  checkout and with a minimal environment: no links -> unavailable; links -> transcribed; through a `~/.local/bin`-style
  link -> transcribed; an environment override wins over the links; dangling links fall back to the not-found report.
  The host's install record and Codex configuration hashes were unchanged.
- Independent review receipts under `out/production-cycle-20261009/cycle3/review/`: `workflow-static-de14c03.json`,
  `workflow-focused-de14c03.json`, `whisper-discovery-759e29d.json`,
  `whisper-discovery-759e29d-contract-closure.json`, `combined-static-4570900-and-prepared-checks.json`.

## Combined and installed verification (2026-10-09)

At `df57e8debad37c48c07dc724971fb5d09c5b1537`, the required combined formatting, Clippy, release build and unchanged
render CI all passed. Cargo reported 622 workspace passes and two ignored doctests; the delivery unit/bin tests
reported eight passes, and all five installer unit tests passed. These counts do not establish that every optional
conditional test branch ran. Separate focused controls and output proofs retain their own scope.

The unchanged CI demos were deterministic with one versus two workers and fresh caches. All 25 SSIM samples were
1.0; video matched the local reference bytes, the audio reference hash matched, and the quality check passed.
The registered PP-067 native-title acceptance ran its actual command: 27 tests passed, with a current input digest.
The capability ledger passed its freshness check; no blanket feature-verification or Adobe-parity claim follows.

The already-built milestone was pushed to the existing feature branch, then installed at `20261009T154953Z` using
`scripts/install-local.py`. The four installed SHA-256s are:

| Command | SHA-256 |
| --- | --- |
| `ferrocut` | `506f3d0c353b81194a36275f1353dd84fd5b53189dfd59a80d0dbf84ed706b2f` |
| `ferrocut-mcp` | `d87e605173daa9ba7bbae0324800bddb6137adb693c5e1eeade610904e4e6a6c` |
| `ferrocut-perceive` | `3f502aa8aabb6fd58ac9f41cf31c93aeb9216f3a8c77484f94539eb48875ca28` |
| `ferrocut-deliver` | `897bd20cb1964b34c0593237e620bb50e589ad20b1b483bd1fe7b3cfd02ceeaa` |

The runtime is `~/.local/share/ferrocut/versions/df57e8debad3-506f3d0c353b`. Its optional links point to the existing
checkout's executable and `ggml-small.bin`; no new model was downloaded. The backup at
`~/.local/share/ferrocut/backups/20261009T154953Z` matches the four previous binaries, whose versioned runtime remains
available. The Codex configuration hash stayed identical to the pre-install snapshot and after both probes.

The complete installed workflow verification (`installed-workflow-proof/run-02/verification.json`, helper
`37a78bdb…`) passed five fake-whisper cache controls from outside the checkout, one actual default-discovery
transcription of a two-second synthetic clip, and four fresh configured MCP requests. The real run used the linked
`ggml-small.bin`; it establishes discovery and execution, not speech accuracy. MCP indexing without transcription
succeeded inside the root and rejected an outside-root source. The fresh server exited on EOF with status zero.

A retained earlier invocation (`run-01`) used a relative output directory and stopped at the helper's
absolute-root `relative_to` check before MCP. Its CLI controls and real synthetic transcription had run. The complete
retry used new absolute directories; neither the failed attempt nor its extra inference is omitted or counted as a
complete route pass. The helper bytes stayed unchanged, and the runbook now requires absolute paths.

The installed vector delta (`installed-vector-proof/run-01/verification.json`, helper `05c634e8…`) separately passed:
five CLI subprocesses and eleven fresh configured MCP requests, including two 128×96 CPU stills with the exact
accepted 544-pixel translated/rotated footprints, all four full chunk keys, bounded edit/undo, and root containment
controls. Undo restored canonical timeline bytes and keys. Its server also exited zero without forced cleanup.
This verifies the current configured command launched afresh; it does not claim an already-running harness reloaded.

Raw logs, fixtures, install preservation records and manifests remain under
`out/production-cycle-20261009/cycle3/{integration-r1,installed-workflow-proof,installed-vector-proof}/`.

## Limits

Fake-whisper controls prove the cache and discovery logic, not transcription quality. The real synthetic runs
prove installed discovery and execution only; it is not speech-accuracy or listening evidence. The MCP
route is not used for transcription in this verification (its working directory is the checkout, so it would run real
inference). Installing a model or building whisper.cpp remains the user's step (`scripts/build-whisper.sh`,
`scripts/fetch-whisper-model.sh`).
