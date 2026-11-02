# Fake Apple scenarios

Run from the repository root with the reference JDK and a built Kotlin oracle:

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
tools/oracle/build.sh reference
tools/fake-worker/run-apple-scenario.sh ios-smoke
tools/fake-worker/run-apple-scenario.sh macos-smoke
tools/fake-worker/run-apple-scenario.sh ios-media
tools/fake-worker/run-apple-scenario.sh ios-screenshot
# Record a changed scenario explicitly:
tools/fake-worker/run-apple-scenario.sh ios-media --update
```

The runner builds the worker and launches the executable reported by Cargo,
including when `CARGO_TARGET_DIR` selects a different build directory.
A full scenario path is accepted too. `--update` validates the new recording
before moving an old expected directory into the ignored run directory and
publishing the new evidence. A failed attempt is retained with a failed manifest
and never replaces expected evidence. There are no authentication retries.

The runner starts the loopback fake first, generates throwaway Ed25519 host and
client keys, fills Marathondevices with its port, and supplies an isolated
known_hosts file. Key authentication is the baseline; sshpass/askpass are not
used. It invokes the **full reference CLI** at `e47352f2`, through
`tools/oracle/run-isolated.sh`, with isolated HOME/TMPDIR and disposable real
bundle archives. Both parse-time archive extraction and local plist/Mach-O
reads happen in Kotlin. The existing ssh/rsync shims retain original host argv,
and `/usr/bin/rsync` drives real binary transfers in both directions.

| Scenario | Discovery/setup | Scripted tests | Expected result |
|---|---|---|---|
| ios-smoke | nm Swift + empty ObjC scans; hosted unit bundle; generated WriteFile xctestrun; boot; keyboard; explicit simctl extra-app install and xcodebuild install | SampleTests.testPassing / testFailing / testSkipping | 1 passed, 1 failed, 1 ignored; exit 1 |
| macos-smoke | XCTest/dylib discovery; hosted unit bundle; rsync-generated xctestrun; explicit fake SIP/TCC profile | SmokeTests.testPass / testFail / testSkip | 1 passed, 1 failed, 1 ignored; exit 1 |
| ios-media | nm; reuse real UI runner + in-root symlink; lipo arch checks; recording and separate device log | FailingTests.testAlwaysFailing | 1 failed; H264 transcode attachment + device log; exit 1 |
| ios-screenshot | nm; hosted unit bundle; PNG capture and real Kotlin PNG→GIF path | SampleTests.testFailing | 1 failed; single-frame 64×96 GIF attachment; exit 1 |

The last two are separate configurations because Kotlin chooses a single
preferred recorder per device. Together they are the baseline iOS media
scenario. No claim is made that the actual app/test code ran on a simulator;
Kotlin executes the complete test-runner path against scripted worker commands.
The selected class/method names are present in the real fixture binaries.

Each run must end with exactly `Marathon execution failed` on stderr and exit 1,
with no incomplete tests. It must enrich the failure through root/id xcresult
JSON, pull a nonempty result tree, write JUnit/raw/Allure/HTML/device/bill/log
reports, clean the remote result source, dispose the simulator where relevant,
and finish every owned job. A test-failure exit is a completed reference run;
a timeout, crash, missing artifact or uncatalogued command form is a harness failure.

`fixtures/fake-worker/apple/<name>/` holds:

| Path | Contents |
|---|---|
| in/ | Marathonfile/Marathondevices templates; no private key |
| script/world.json | Versioned world and required command counts |
| expected-exit-code | The expected Kotlin exit |
| expected/raw/ | Declared manifest; lossless command/stream transcript, host argv, CLI stdout/stderr; consumed xctestrun bytes; final worker state; per-test semantic outcomes/attachments; report tree with file content hashes; media bytes and metadata; isolated-run manifest |
| expected/normalized/ | Only `ready.json` (typed endpoint/key identity normalized) and `semantic.json`; replay compares these small projections |

The schema-2 manifest declares the suite/artifact inventory before startup,
records terminal outcome and exit codes, and hashes the required artifacts.
The validator rejects a missing/truncated/failed/skipped recording, even against
itself. It verifies stored stream hashes, catalogue-form membership, required
counts, cleanup and empty active-process inventory, finished sessions, exact
per-test results/enriched failure text, JUnit counts, both macOS TCC databases,
pushed XML, result pulling/remote cleaning and actual attachment files.

Replay compares normalized readiness, semantic results, command-form counts, report-family counts,
static JS/CSS and scripted xcresult content hashes, media hashes/metadata and
final state and the complete retained virtual filesystem (file bytes/hashes,
modes and symlinks; directory inode sizes are excluded). Typed batch IDs are
mapped consistently only in these structured path fields. Raw reports retain their full path/size/content-hash manifest.
Opaque command and output bytes are **never** rewritten. UUIDs stay distinct;
references remain intact. Dynamic report clocks/UUIDs and opaque transcripts
are retained for WP 7.1 rather than declared byte-equal. Normalized semantic
results remove timing/identity fields; they do not apply global text regexes.
Catalogue membership uses broad wire patterns and host checks cover the version
and send/receive shapes. Replay does not validate each argv option: changing
host `--links`/`--rsh` or transcript `-destination-timeout` to
`-destination-timeoutX` can pass offline replay after the evidence digests are
updated. These witnesses belong to WP 7.1's byte/field comparison. Raw artifacts
are stored once under `expected/raw/`; there are no normalized transcript,
host-argv, stdout or media copies.

Kotlin's A20 log-buffer race permits one additional batch-tail log: the final
line may instead be part of the last test log. Semantic family counts exclude
that optional file, require all per-test log attachments, and reject two batch
logs. Both complete manifest shapes are retained under
`diagnostics/macos-log-partition/`; full raw content hashes are always kept.
The transcode's original `-temp.mp4` also remains on the worker (A19); it is
preserved in the final snapshot, not silently removed by the harness.

The offline verifier is in `scripts/check.sh` and requires neither Xcode nor a
JDK. Sixteen Python checks include missing artifacts/transcript, a changed test
status, an uncatalogued command form, missing cleanup, failed/interrupted evidence and an
unconsumed expectation. Rust contracts cover finite reservation rollback,
shared devices, file barriers, recording/log ownership and cancellation.

These are isolated **SSH fake-world runs**, including fake macOS permissions.
The Xcode 26.6 remote profile is documented in PLAN, was not contacted, and is
not an interchangeable golden for this Mac's Xcode 27.1. Real macOS host/VM
snapshot restoration and sequential comparisons remain required by PLAN
§6.6–6.7. Linux retains 0.4a's Samba rsync substitute; this WP records only the
local Apple openrsync profile.

## Deferred scenarios

| Owner | Deferred scope and reason |
|---|---|
| 4.1–4.3 | Fault/timeouts, cancellation tails, MaxSessions, debug/oversize WriteFile fallback, push cache races and password profiles. Reuse 0.4a faults; none is necessary for baseline key-auth runs. |
| 4.4 | Every catalogue success/error branch beyond the baseline; Xcode unknown/<14/14+/16+, SDK/version fallbacks, alternate facts and exact unused pipelines. Explicitly recognised failure now avoids invented successes. |
| 4.5a–b, 4.10b | Generated SDK runners, signing, thinning, alternate xctestrun versions, symlink writes, skip-copy modes, archives beyond the selected real products. Runner reuse and hosted XCTest satisfy the baseline. |
| 4.6–4.8 | Merge and multiple results, legacy/Xcode27 fixture matrices, ambiguous enrichment, ObjC discovered tests, partial/malformed/slow responses, parser cancellation. Only one batch/result per baseline needs scripting. |
| 4.9–4.10a | Multi-bundle artifact queue/admission, retention and pull failure matrices. The baseline pulls and cleans one result through the actual pipeline. |
| 5.1–5.6 | Delayed/failing boot transitions, create/delete/erase, app/container/privacy/media import, runner crashes, permission errors, inventory cache/recovery/dynamic updates, pidfile races, all screenshot formats/display/mask and HEVC/other transcode sizes. Add focused scripts when their production WPs exist. |
| 7.1–7.2 | Complete byte/field comparison, including per-argv checks: host `--links`/`--rsh` tampering and transcript `-destination-timeoutX` currently pass offline replay after digests are updated (R1-sol-3 / R1-wonder-2). Preserve these witnesses for WP 7.1. Real local/SSH/VM certification remains WP 7.2. |

No parity-matrix rows are marked ported: this WP has no assigned Kotlin tests,
and Phase 4/5 production-test ownership is unchanged.
