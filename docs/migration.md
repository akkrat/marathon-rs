# Migrating from the JVM distribution

Keep your existing Marathonfile, Marathondevices and command lines, subject to
[the approved differences](deviations.md). Packaging checks alone do not certify
end-to-end parity: see [release readiness](releasing.md#release-readiness) for what
the evidence covers, and check the release's notes before migrating.

## Install the native entry point

Choose the archive for the **runner host**, not the device architecture:
`aarch64-apple-darwin` for Apple Silicon macOS, or `x86_64-unknown-linux-musl` for
x86_64 Linux on any distribution. Download `marathon-<version>-<target>.zip` and
`SHA256SUMS` from the same version's GitHub Release. Versions and tags have no `v`
prefix. There is no Windows, Intel Mac, Linux on ARM or universal binary. Branch builds
do not publish. GitHub latest means the last successfully promoted release, not
necessarily the highest semantic version, and pre-releases are never latest. Use a
pinned version for reproducible CI entry points.

Replace the angle-bracket placeholders before running this; each release's notes carry
the same commands with its own version and repository filled in. It verifies the
selected asset against its exact entry in `SHA256SUMS`:

```sh
set -eu
version='<version>'
repo='<owner>/<repo>'
target='aarch64-apple-darwin'  # or x86_64-unknown-linux-musl
archive="marathon-${version}-${target}.zip"
url="https://github.com/${repo}/releases/download/${version}"
curl -fLO "${url}/${archive}"
curl -fLO "${url}/SHA256SUMS"
# Fail unless the checksum file contains exactly this asset once:
awk -v asset="$archive" '$2 == asset { print; n++ } END { if (n != 1) exit 1 }' \
  SHA256SUMS > selected.sha256 || {
    echo "Expected exactly one checksum for $archive" >&2
    exit 1
  }
[ -s selected.sha256 ] || { echo "Selected checksum is empty" >&2; exit 1; }
shasum -a 256 -c selected.sha256
unzip "$archive" -d native-runner
"./native-runner/marathon-${version}/bin/marathon" version
```

On Linux, replace `shasum -a 256 -c` with `sha256sum -c`. Extract into a fresh directory.
The archive contains `marathon-<version>/bin/marathon`, with Unix mode 0755, and the text
files `LICENSE` and `THIRD-PARTY-LICENSES`, with mode 0644. There is no launcher script or
JAR directory. The final stdout line from `version` must equal the version exactly. A
startup log precedes it. To check where an archive was built, run
`gh attestation verify "$archive" --repo "$repo"`.

**macOS quarantine.** The macOS binary is ad-hoc signed and not notarised, because
that needs an Apple Developer account. A browser marks its downloads as quarantined, and
extracting such a zip with `unzip` passes the mark to the binary. Gatekeeper then blocks
it: it does not start, and in an unattended session, such as a CI step, it hangs with no
output. `curl` and `gh`, as above, set no quarantine. For a browser download, clear the
mark from the extracted binary before the first run:

```sh
xattr -d com.apple.quarantine "native-runner/marathon-${version}/bin/marathon"
```

A copy already launched while quarantined can stay blocked after that, so extract it
again and clear the new copy.

Change your PATH or CI executable path to the extracted `bin/marathon`, retaining
the previous JVM distribution separately for rollback. Check `command -v marathon`
and `marathon version` after the switch. Preserve the CI working directory and
environment used for configuration substitution. Do not carry JVM arguments into
the native argv: `JAVA_OPTS` and `MARATHON_OPTS` are ignored (D2).

### Platform and workload prerequisites

The macOS release helper sets a macOS 11.0 linker floor; observed WP 7.3 metadata
matches it, but old-macOS execution is not certified. The Linux binary is static, so
it needs no particular libc: each release runs `version` and `--help` in an Alpine
container before it is published. The real Android runs in the
[release evidence](wp/7.5-findings.md#closure-sweep-results) used a glibc build, and
musl's allocator and DNS resolver differ from glibc's, so include your Android workload
and worker hosts in the trial below. See [operator instructions](releasing.md) for
precise limitations.

The runner needs no JVM. Android still needs the configured SDK, ADB server and
devices; SDK lookup uses `androidSdk`, then `ANDROID_HOME`, then
`ANDROID_SDK_ROOT`. Apple workloads need the appropriate Xcode tools on their
workers. SSH transport, system `rsync`, and `sshpass` for password-based rsync with
an SSH client older than OpenSSH 8.4 or not recognized as OpenSSH remain workload-dependent; stock `/usr/bin/perl`
on Apple workers is optional and recommended ([D96](deviations.md#d96--the-controller-owns-its-local-process-groups));
recording may require tools such as `ffmpeg`.
`version` and help require no worker or test configuration. A developer JDK for
oracle generation is independent of runtime installation.

## Changes that need action

Most differences need nothing from you. These are the ones that can change what you see or what you must set. Each
links to its entry in the [deviations guide](deviations.md) or the relevant Phase 8 evidence.
The Phase 8 rows apply when moving from 0.12.0 to 0.13.0 as well as from Kotlin.

| If you… | What changed | Do |
|---|---|---|
| grep or parse marathon diagnostics ([WP 8.8](design/logging.md), [WP 8.11](wp/8.11-test-changes.md#other-group-3-diagnostic-changes)) | `tracing` uses `HH:mm:ss.SSS LEVEL marathon_crate::module: message`, without a thread column; Apple compact output omits time. Targets, wording and debug representations change. WP x56 replaces `kotlin.Unit` with plain run-failure messages, uses plain command/pool values, and logs expected batch cancellation at DEBUG without timeout advice or coroutine class names; stored result text is unchanged. | Update matchers. Only per-test progress lines and the entire summary block, including `Marathon run finished` and `\tRaw:`, keep their text format. |
| match the worker supervisor's `Worker recorder … failed` warning in log alerts (x106, 0.15.3) | It is DEBUG now; marathon's iOS recording code logs recorder problems as WARN on `marathon_ios::busy_video`. | Update matchers as [iOS recorder warnings](#ios-recorder-warnings) describes. |
| depend on provider WARN/INFO lines with `debug: false` ([WP 8.8](design/logging.md#debug-off-visibility-c18-superseded)) | Android, iOS and macOS provider and core construction diagnostics now follow the ordinary ERROR threshold instead of retaining a construction-time DEBUG level. Host-key warnings remain visible. | Set `debug: true` when those diagnostics are needed; update log-based alerts. |
| classify generated failures or snapshot their traces ([D59](deviations.md#d59--generated-failures-use-plain-descriptions)) | Result files and logs use plain descriptions, such as `unexpected end of input`, without generated Java class names; external tool text stays verbatim. Hidden `config-dump` uses `error.kind` instead of `error.class`. | Update classifiers and snapshots; use structured statuses, and `error.kind` for developer dumps. |
| snapshot CLI help or match usage errors ([D61](deviations.md#d61--cli-help-and-usage-use-clap)) | clap replaces Clikt's help, usage, suggestions and error wording. Commands, flags, defaults, boolean meaning, streams and exit codes stay. | Update text expectations; use exit codes for usage failures. |
| consume Allure `environment.xml`, raw annotation strings or `annotationData.valueRegex` ([D58](deviations.md#d58--object-and-annotation-values-replace-identity-strings)) | Stateless objects use simple names, arrays use `[elements]`, decoded null uses `null`; filters match those values instead of identities. | Replace identity-based expectations and regexes with value-based ones. |
| compare screenshots or GIF bytes ([D63](deviations.md#d63--screenshots-and-gifs-use-native-image-handling)) | P3 colours, channel decoding, resize ties and partial alpha can change pixels; multi-frame GIFs have one loop extension. Paths, dimensions, timing and references stay. | Review visual baselines and refresh affected image hashes, including multi-frame GIFs. |
| rely on queue tie order, seeded order or exact batches ([D60](deviations.md#d60--equal-priority-tests-run-in-insertion-order)) | Ties are FIFO; keys are computed on insertion. Random seeds can assign another order; selector order/grouping, batch counts and report order can change. | Update order/batch snapshots and keep tests independent of execution order. |
| match socket errors or malformed UTF-8 ([D57](deviations.md#d57--native-decoding-socket-text-and-basic-credentials)) | Native addresses drop Java's slash and compress IPv6; encoded surrogate bytes can produce more U+FFFD characters. Device IDs keep their spelling. | Update text matchers and malformed-input expectations. |
| use non-ASCII HTTP Basic credentials for analytics ([D57](deviations.md#d57--native-decoding-socket-text-and-basic-credentials)) | Credentials now encode UTF-8 instead of ISO-8859-1 with out-of-range replacement. ASCII stays identical. | Check that the server accepts UTF-8 credentials. |
| match malformed Unicode or rare supplementary-character sort order ([D55](deviations.md#d55--text-uses-unicode-characters)) | Lone surrogates become U+FFFD at decoding; supplementary/BMP ordering follows Unicode scalar order. Valid-Unicode fragmentation and Allure hashes stay. | Update those edge-case expectations; no change for ordinary valid text. |
| predict artifact paths cut through an emoji or other supplementary character ([D56](deviations.md#d56--path-limits-keep-complete-characters)) | The cut stops before the whole character, one UTF-16 unit shorter; the path can now be written instead of failing. Empty components still fail. | Allow the shorter complete-character path. |
| compare serialized YAML or JSON as text ([WP 8.9](wp/8.9.md), [WP 8.4](wp/8.4.md)) | Serde preserves ordered source data; NEL in a YAML key now survives instead of becoming a space (U184). YAML formatting/quoting and JSON numeric/escape spellings may differ, including embedded report JSON. Input parsing stays. | Compare parsed data for `parse` YAML, serialized configurations and JSON reports. |
| set an Apple command output idle limit very close to expected silence ([D62](deviations.md#d62--apple-output-idle-limits-have-no-polling-margin)) | Expiry uses the configured limit exactly, without the old 20 ms polling margin; ready bytes still win. | Raise the limit if it relied on that margin. |
| **have any Marathonfile that doesn't set `uncompletedTestRetryQuota`** ([D47](deviations.md#d47--an-unlimited-uncompletedtestretryquota-is-refused)) | **Breaking.** `marathon run` and `parse` now exit 1 when it is unset or 2147483647, because nothing else ends a run in which a test keeps coming back uncompleted. | Add `uncompletedTestRetryQuota: 5`, or the number of devices + 2 if that is more. [See below](#a-finite-uncompletedtestretryquota-d47). |
| rely on an empty `/sdcard` ([D27](deviations.md#d27--prepare-no-longer-wipes-external-storage)) | Prepare no longer runs `rm -r $EXTERNAL_STORAGE`. Videos from an aborted run, or cut short by a lost connection in a run that still exits 0, stay on the device. | Clear what your tests need in the tests or in your device setup. |
| set `socketIdleTimeout` ([D21](deviations.md#d21--socketidletimeout-means-what-it-says)) | It is now real milliseconds. It bounds only four requests: the adb server's version, a device's features, the property reads after boot and the removal of port forwards when a device is disposed. Kotlin read it as seconds, so `PT30S` lasted 30,000 s. | Remove it, or keep it at 30 s or more. `PT2S` now fails those requests on a slow adb server. |
| start controller tools that fork background work ([D96](deviations.md#d96--the-controller-owns-its-local-process-groups)) | Local tools run in private groups. Cleanup ends registered groups, and a reaper covers controller death. ADB servers keep their independent lifetime. | Keep sessions open for background work that must finish. Use noninteractive SSH authentication and redirect inherited terminal output if `stty tostop` is set. [See below](#cancelling-a-run). |
| use SSH passwords for Apple transfers ([D96](deviations.md#d96--the-controller-owns-its-local-process-groups)) | From 0.15.0, OpenSSH 8.4 or newer uses askpass, which keeps rsync and its SSH client together in a private process group that marathon owns. An older or unrecognized SSH client uses `sshpass`, and marathon logs one warning per run that its cleanup then covers fewer processes. | Older or unrecognized SSH clients still need `sshpass`. |
| filter SSH exec or rsync senders on Apple workers ([D96](deviations.md#d96--the-controller-owns-its-local-process-groups)) | D96 is new in 0.15.0. Stock `/usr/bin/perl` with its core modules is optional and recommended. A successful pull verifies the sender through rsync's own status. The independent control connection keeps one exec open and can retry token requests on a spare command session. | Allow bootstrap, supervised requests, token controls and supervisor retirement. Allow `control-stream` with stdin intact. Use `exec` to run the original supervised command so a wrapper failure arrives as an SSH signal. |
| filter iOS video commands on the worker ([D96](deviations.md#d96--the-controller-owns-its-local-process-groups)) | A supervised recorder detaches and releases its SSH session after launch. Token control sends INT to finalize it and can escalate to TERM and KILL. `timeoutConfiguration.video` bounds the open. | Allow the supervisor bootstrap, detached launcher and token controls. Without Perl, allow the legacy `nohup` launcher, pidfile commands and PID-based stop. OpenSSH rejects channel signals for forced commands and subsystems. |
| filter iOS screenshot commands on the worker ([D100](deviations.md#d100--stream-ios-screenshots-over-the-existing-ssh-connection)) | A screenshot frame is captured and streamed on the existing SSH connection. No screenshot rsync sender is launched. | Allow the bounded screenshot shell wrapper. [See below](#ios-screenshot-streaming). |
| have a forced command or wrapper on an iOS or macOS worker ([D31](deviations.md#d31--an-rsync-send-that-stops-moving-fails), [D32](deviations.md#d32--xcodebuild-on-a-worker-runs-unbuffered)) | xcodebuild is sent as `env NSUnbufferedIO=YES xcrun xcodebuild …`. Sends and pulls carry `--timeout=300` ([D43](deviations.md#d43--an-rsync-receive-has-a-limit-on-progress)). On a host with rsync 3 the worker's `rsync --server` gets it too, and so does a wrapper set with `RSYNC4J_RSYNC`. | Match `env NSUnbufferedIO=YES xcrun xcodebuild` at the start of `SSH_ORIGINAL_COMMAND`, keep `env` on the worker's `PATH`, and accept the new flag. |
| run iOS 16 or 17 simulators on a slow host or link ([D38](deviations.md#d38--a-failed-readiness-probe-means-not-running)) | On runtimes 16 and 17 a readiness probe still returns about 20 MB, and a failed or timed-out probe now means "not running". It must arrive within `shell` (30 s by default). From runtime 18 on the probe is about 100 KB ([D46](deviations.md#d46--the-readiness-probe-is-launchctl-print-userforeground-from-runtime-18)). | Raise `shell` on 16 and 17. The time a simulator can then spend is in [D38](deviations.md#d38--a-failed-readiness-probe-means-not-running). |
| have a forced command or filter on an iOS worker that allows the readiness probe ([D46](deviations.md#d46--the-readiness-probe-is-launchctl-print-userforeground-from-runtime-18)) | On iOS 18 and later the probe is `xcrun simctl spawn <udid> launchctl print user/foreground`, where it was `… launchctl dumpstate user/foreground/com.apple.SpringBoard`. Runtimes 16 and 17 still send `dumpstate`. | Allow the new command too. A filter that refuses it makes every probe fail, and the simulator leaves the run after its setups (D38). |
| have UI tests that fail with a "Failed to terminate…" message ([D39](deviations.md#d39--a-tests-own-text-does-not-abort-its-batch)) | After a prefix such as `XCTAssertTrue failed - ` the test fails and the batch goes on. A simulator is no longer dropped after 3 such batches in a row. Three output timeouts in a row with no test passed or failed do recycle it, also for a silent test with no failure text. While another device keeps the pool alive, it leaves the run after 12 such batches, at the end of its 3 recoveries. On a pool's only simulator, a recovery that isn't ready within `deviceInitializationTimeoutMillis` ([D48](deviations.md#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)) ends the pool sooner. | If a batch waits out its timeout, or a sick simulator keeps taking batches, read the failing line in the log and consider `maxBatchesWithoutProgress`. Raise `testOutputTimeoutMillis` for a test that is silent for longer and passes. |
| depend on a simulator being set up once ([D33](deviations.md#d33--a-simulator-whose-setup-failed-is-set-up-again)) | A simulator whose setup failed is set up again, 30, 60 and 90 s after each failure, even on a removed worker's profile, until four setups have failed with the worker answering. A setup that can't reach the worker isn't counted (the worker's connection retry takes over), and removing the simulator and adding it back starts the count over. When another simulator's setup on that worker also fails it, the pauses can be shorter ([D44](deviations.md#d44--a-lost-simulators-reconnect-has-a-limit)). | `attempts: 1` with `lateJoin: false` in `workerConnection` turns it off. |
| have a worker with no algorithm in common with marathon ([D35](deviations.md#d35--a-failed-key-exchange-is-not-retried-as-a-network-fault)) | It is no longer retried. | Fix the worker's SSH configuration. |
| count on a stuck worker costing little ([D31](deviations.md#d31--an-rsync-send-that-stops-moving-fails), [D43](deviations.md#d43--an-rsync-receive-has-a-limit-on-progress)) | A send with no progress for 300 s takes its simulator out of the run, and sends waiting behind it to that worker fail at once. A pull with no progress for 300 s is stopped and is not retried, so each pull costs about 305 to 330 s where it cost 35 to 45 s. A pull whose files keep changing is no longer cut at 30 s. | Nothing, unless a worker can be silent for 5 minutes during a transfer. |
| set `attempts: 1` with `lateJoin: false`, or expect a lost iOS worker to return after any length of time ([D44](deviations.md#d44--a-lost-simulators-reconnect-has-a-limit)) | A lost simulator is no longer tried every 16 ms until the run ends. It is tried at `workerConnection`'s pace for `lateJoinCutoff` (600 s) after the first failure, and given up when the next pause ends (630 s with the defaults). With `attempts: 1` and `lateJoin: false` it gets one try. A devices update that removes it and adds it back starts this over. | Leave `lateJoin` on, and set `lateJoinCutoff: 0` to wait until the run ends. |
| use `flakinessStrategy: probability` with analytics disabled, or with a backend that can be down ([D71](deviations.md#d71--unavailable-test-history-adds-no-probability-copies)) | History that cannot be read no longer counts as a 0% success rate: with analytics disabled, a backend that could not be set up, or a failed success-rate query, probability adds no copies and one WARN says why. Working history is unchanged, including `defaults.successRate` for tests it does not name. | To copy every test regardless of history, as disabled analytics used to, switch to `flakinessStrategy: {type: fixed, count: N}` ([D72](deviations.md#d72--flakinessstrategy-fixed-for-stabilization-jobs)). |
| collect test metrics with Graphite or InfluxDB ([D86](deviations.md#d86--remote-metrics-use-bounded-buffering)) | D86 is new in 0.15.0. It waited up to 30 s when full. From [D105](deviations.md#d105--a-full-remote-metrics-queue-does-not-delay-tests), a full queue drops new events immediately without starting cool-off. Active cool-off, closed admission and a stopped delivery task still drop new events. Each contributes to the rejection count. Failed or skipped sends and expiry of the 30 s drain-and-close deadline can also lose metrics. | Use local reports for result counts. Look for the analytics warnings when dashboard data is missing or later runs' history changes flakiness copies or duration-based scheduling. |
| count `flakyFailure` elements in JUnit reports ([D78](deviations.md#d78--junit-lists-only-the-runs-of-a-passed-test-that-did-not-pass)) | A passed test's JUnit case lists as `flakyFailure` only its runs that did not pass, with their traces. Its passing runs, which Kotlin and 0.14.0-rc.1 listed as empty `flakyFailure` elements, are no longer listed, so a test that passed every run of `flakinessStrategy: fixed` or probability copies has none. A failed test's `rerunFailure` elements are unchanged. | If you counted them as the number of extra runs, read `test_result/raw.json`, which lists every run. Counting them as failed runs needs no change. |
| have devices that go `offline` often ([D41](deviations.md#d41--a-brief-offline-does-not-lose-the-device)) | A device gets up to 10 s to come back, at most 5 times per adb server in a run. The count is never reset. From the sixth `offline` on the device is disconnected at once, as in Kotlin. | Fix the transport. A device that drops more than five times in one run is lost for the rest of that run. |
| filter adb shell commands or run Android through intermittent adb outages ([D85](deviations.md#d85--android-checks-the-boot-before-resuming-tests)) | D85 is new in 0.15.0. Marathon reads `/proc/sys/kernel/random/boot_id` and writes, reads and removes a per-preparation marker under `/data/local/tmp`. During an adb outage or a brief `offline`, a changed boot ID or a confirmed missing or different marker (a full reboot, a snapshot from before preparation) triggers fresh setup. If the marker's creation could not be confirmed, recovery checks the boot ID alone; without readable evidence of a reset, ordinary recovery continues. An unchanged kernel ID and marker can hide a restart of a shared-kernel container that preserves the marker while resetting other state. Marathon gives a still-listed device one more setup after preparation exhausts its retries, within the existing return deadline. | Allow the boot ID read and marker operations if you filter adb commands. Existing Marathonfiles need no changes. |
| rely on a run failing quickly when it loses all its devices ([D48](deviations.md#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)) | A pool with tests left waits up to `deviceInitializationTimeoutMillis` (180 s by default), from when its last working device went away (or a test that device reports after leaving ended), for a device that comes back and gets ready before it ends, and then for the devices leaving to finish cleaning up. A killed emulator alone exits 1 after about 180 s, not 12.5 s. | Lower `deviceInitializationTimeoutMillis`, which also bounds the wait for the first device. |
| have devices that need many slow retries before they get ready ([D87](deviations.md#d87--a-devices-preparation-has-one-deadline)) | A device's preparation, its retries and an iOS simulator's replacements after a failed boot share one deadline: the greater of `deviceInitializationTimeoutMillis` and one preparation's limits (550 s on iOS, 1,104 s on Android with an app and its test app, 812 s on macOS, with the defaults). No retry or replacement starts after it; nothing running is stopped. A device that keeps failing slowly leaves in about 9 to 12 minutes on iOS, 19 on Android and 14 on macOS (scenario estimates) instead of up to 33, 96 and 30. A preparation that fails at once keeps its 30 attempts with the default timeouts; shorter timeouts and wait can make the deadline shorter than those 30 attempts (290 s). | To extend the preparation deadline, raise `deviceInitializationTimeoutMillis` above that calculated allowance (one preparation with its retries at their limits, from the timeouts; the figures above with the defaults); it also bounds the wait for the first device and D48's wait. |
| use `fixed-size` batching with flaky copies or several test APKs, and set `testBatchTimeoutMillis` close to what a batch took ([D50](deviations.md#d50--a-rejected-test-does-not-use-a-batch-slot)) | Only the tests a batch accepts count against `size` and `durationMillis`, so batches that used to end early hold more tests and take longer. | Check `testBatchTimeoutMillis`. [See below](#batches-hold-more-tests-with-flaky-copies-or-several-test-apks). |
| cancel runs with SIGINT, SIGTERM or SIGHUP, or take a run's reports as a sign that it finished ([D52](deviations.md#d52--a-cancel-writes-the-reports-and-cleans-up-within-a-limit)) | Whatever `debug` says, a cancelled run attempts to write its reports if the output directory is ready and starts provider cleanup. It hands off to exit by 24 s, with up to 1 s more for console output, and exits with 130, 143 or 129; reports or cleanup can remain unfinished. A second signal exits at once. | Allow 25 s before killing it, and read the exit code to tell whether a run finished. [See below](#cancelling-a-run). |
| look up batch-level artifacts by name or glob by extension ([D104](deviations.md#d104--batch-level-artifacts-use-their-type-suffix)) | Batch files use their type's suffix, such as `.log` or `.mp4`, rather than repeating the batch ID. | Update lookups from `<batchId>.<batchId>` to `<batchId>.<suffix>`. If a glob is meant to find only per-test files, filter out batch-level files. |

These deviations need no action: D1, D4, D5, D7, D13, D15–D19, D22, D23, D26, D28–D30, D34, D36, D37, D40, D42, D45, D49,
D51, D53, D54, D64–D66, D68–D70, D74–D77, D79–D84, D88, D90–D95, D97, D98, D99 and D102. D65 can include a previously missed late pool,
lengthen a run and correct its reports and existing exit status; it needs no configuration change. D66 turns a SIGQUIT, which used to end the run at once, into a
logged state dump; a wrapper that sends SIGQUIT and then SIGTERM now gets the dump, then D52's reports and exit status. D2, D3, D6, D8–D12, D14, D20, D24, D25, D67, D71, D72, D78 and D87 have an action of their own, described in their entries and in the
sections below.

D67 gives ordinary console completion one shared 60-second flush/drain deadline.
Signals during the run keep their existing bounds: a one-second output watchdog,
immediate second-signal exit and unchanged D66 SIGQUIT behavior. After the run
finishes, SIGINT/SIGTERM are not acted on during console completion. This window
already existed for about two seconds; a stalled console can now hold exit for
up to 60 seconds. A wrapper needing a hard stop then must use SIGKILL.

The D68–D81 notes apply to 0.14.0. The D82–D88 and D90–D97 notes apply when moving from 0.14.0 to 0.15.0 (D89 was not
used), D98 when moving from 0.15.0 to 0.15.1, D99 when moving from 0.15.5 to 0.15.6, and D100 after 0.15.6.
All of these notes also apply when migrating from Kotlin. D71 is
in the table above, and so are D85, D86, D87 and D96.

D68 and D69 need no configuration change. With D68 a run whose test queue
fails ends with exit 1 (0 with `ignoreFailures: true`), its reports and a
`Marathon run failed` line naming the cause, where it used to hang; a console
that refuses a progress line costs that line only. With D69 a test that has
finished is not run again, so strict-mode retries, flaky copies in class-name
batches, rejected batches and copies that come back incomplete after their test
finished can leave fewer executions in the reports.

[D70](deviations.md#d70--an-adb-outage-does-not-charge-or-break-tests-that-never-started) needs no action. After an adb
server outage or a brief `offline`, a batch that never started is no longer charged, so tests that used to fail without
running now run, and each instrumentation of a batch waits for the device as the first did. A batch whose device is lost
after it started keeps the tests that finished, so the reports of such a run can show fewer executions.

D72 adds a native-only `flakinessStrategy` type, `fixed`, for stabilization jobs that run every test a set number of
times, and allows it with `executionStrategy.mode: ALL_SUCCESS` (all runs must pass). Existing configurations keep
their meaning, and `probability` with `ALL_SUCCESS` is still rejected; a Marathonfile that uses `fixed` cannot be read
by the Kotlin marathon or by 0.13.0, so remove it before rolling back to either.

D74 needs no action. Disposing of an iOS simulator now stops its preparation and waits for it, up to 70 s, so a stopped
preparation no longer sends commands to a simulator that has been disposed of. iOS and macOS now release the worker
connection leases they kept until exit, those of replaced devices and of failed setups or disposals, at the latest when the
run ends.

D75 and D76 need no action. An Android test APK with several dex files runs each JUnit3 test once, also when it carries an
array-valued annotation such as Kotlin's `@Throws`, so such a run can have fewer tests. An iOS unit-test bundle built inside
its app is no longer pushed to the worker a second time after the app; the files on the worker stay the same.

[D77](deviations.md#d77--the-final-result-of-a-test-agrees-with-its-verdict) needs no action. With
`executionStrategy.fast: false`, a test with several runs is reported (summary's first block, per-test result file,
JUnit, HTML) by the run `fast: true` would have stopped at: its first pass under `ANY_SUCCESS`, its first failure under
`ALL_SUCCESS`, otherwise the run that completes it. These reports now agree with the exit code. A tool that read a test's last run
from them should read `raw.json`, which lists every run.

[D79](deviations.md#d79--a-simulator-shut-down-during-its-boot-wait-is-booted-again-at-once) needs no action. An iOS
simulator that is shut down or crashes while marathon waits for it to boot, once it has been seen booting, is booted again
within a poll and the 10 s retry pause, not after `bootWait` (240 s); one that stays booting is still waited for until `bootWait`.

[D80](deviations.md#d80--a-slow-video-transfer-doesnt-cost-the-next-test-its-video-or-its-time) needs no action. With iOS
video recording on a slow worker link, the tests after the first of a batch keep their videos and their durations; the
stop, transcode and pull of a test's video now run beside the next test instead of before its start is handled.

[D83](deviations.md#d83--copies-of-different-tests-take-turns-in-the-queue) is new in 0.15.0 and needs no action. The
repeated entries of a test in a pool's queue (the copies that `flakinessStrategy` `probability` or `fixed`, or
`shardingStrategy: count`, make, or a test discovered twice) now take turns with other tests' entries, so a wave of
devices spreads over the tests. `fast` drops a test's copies that are still queued when it is decided: after its first
pass under `ANY_SUCCESS`, or its first failure under `ALL_SUCCESS`; started copies and batches already handed to a device
still run, and with copies in turns more can still be queued. Runs can list fewer executions, send tests in another order
and grouping, and launch fuller or emptier, and more or fewer, `fixed-size` batches; with code coverage on, a test in another
batch gets that batch's Android `coverageFile` path. Which device runs which copy can change, so a test whose outcome depends on the
device can get another verdict, and with it the run's exit status (with `ignoreFailures: false`), and the final record
can come from another attempt; the rules for both are unchanged.

[D84](deviations.md#d84--an-unexpected-end-of-the-runs-own-machinery-ends-the-run-with-its-cause) is new in 0.15.0 and needs no action. A run
whose own machinery ends unexpectedly (an actor or the device listener on a cancellation of its own, a failed device
listener, a pool or the run task that panics) now ends with exit 1 (0 with `ignoreFailures: true`), its reports and a
`Marathon run failed` line naming the first cause, where it used to hang or name no cause; the cause is logged when it
happens. Before the reports are written the run waits for its devices to end their cancelled work (an Apple
preparation included), up to 75 s per iOS or macOS device and 65 s per Android device with defaults
([D92](deviations.md#d92--android-device-cleanup-has-one-deadline)),
and the results already reported reach the reports. An iOS or macOS task that panics now ends with the panic as its error, handled as any device error, where the run
used to hang. iOS device monitoring that panics, or fails with no simulator available, ends the run with its cause instead of
with no devices after `deviceInitializationTimeoutMillis`; with simulators connected the run carries on, as before. In runs without a failure, a device that connects while
the run finishes is no longer admitted, results that had reached a late pool's queue are now always in the reports,
and a failing provider termination no longer skips the reports.

[D87](deviations.md#d87--a-devices-preparation-has-one-deadline) is new in 0.15.0. It bounds how long a device that cannot be prepared keeps its
place. Its preparation, the retries inside it and an iOS simulator's replacements after a failed boot share one deadline,
the greater of `deviceInitializationTimeoutMillis` and what one preparation can take with its retries at their limits. No
retry or replacement starts after it, and nothing running is stopped by it. A simulator whose readiness probe never
succeeds now leaves after one replacement, in about 9 to 12 minutes, and an Android device whose install keeps timing out
in about 19 (about one more when D85 sets it up once more), where they took up to 33 and 96 minutes (scenario estimates
with the defaults). D85's extra setup continues the deadline but always gets its first attempt. Healthy runs and a device that
fails once are unchanged, and a preparation that fails at once keeps its 30 attempts with the default timeouts; with
shorter timeouts and a shorter `deviceInitializationTimeoutMillis` the deadline can be shorter than those attempts (290 s). To give slow devices longer, raise `deviceInitializationTimeoutMillis`.

[D88](deviations.md#d88--android-device-monitoring-that-ends-fails-the-run) is new in 0.15.0 and needs no action. Android device monitoring
that ends with an error or a panic, other than at the provider's termination, now fails the run with that cause (exit 1,
0 with `ignoreFailures: true`) where the run went on without device events and could hang; during remote test
discovery that cause is the run's error instead of a no-device timeout. A failing analytics call for a connected device
is logged and no longer ends it.

[D90](deviations.md#d90--a-saturated-shared-ssh-connection-is-replaced) is new in 0.15.0 and needs no action.
With `shareWorkerConnection: true`, an SSH connection that runs out of session slots after an abandoned channel can
be replaced while existing leases drain. A worker can have up to two connection generations at once.

[D91](deviations.md#d91--a-stopped-rsync-transfer-gets-40-s-for-its-cleanup-and-its-device-waits-for-it) is new in 0.15.0 and needs no action. A
stopped rsync transfer to an iOS or macOS worker now has at most 40 s for its cleanup and the confirmation of it, and an
iOS simulator's disposal waits for its sends, also those whose caller had stopped waiting, before its on-dispose
lifecycle. If the end of rsync and what it started cannot be confirmed, an ERROR line names the worker and rsync's pid,
and nothing more is sent into that exact destination in the run: a device that needs it leaves its pool. Healthy runs
are unchanged.

[D92](deviations.md#d92--android-device-cleanup-has-one-deadline) is new in 0.15.0 and needs no action.
Android device cleanup shares one deadline across in-flight work and teardown, 60 s by default. An interrupted staged pull discards its partial output.

[D93](deviations.md#d93--an-apple-devices-stop-has-one-window-and-its-disposal-needs-proof-that-its-work-ended) is new in 0.15.0 and needs no action.
When a run stops after a failure, each sealed iOS simulator and Mac shares one 70 s window across its work and its
disposal. The Scheduler allows 75 s for the device joins, and the device waits now total at most 80 s, down from up to
145 s that one stuck operation could hold the run; the iOS artifact drain is separate (D94). A device whose work could not be confirmed
to have ended gets no on-dispose lifecycle (a simulator is not shut down or erased by it) and its lease is
quarantined: its worker connection stays open and takes no new device until the run ends, while devices already
sharing it can go on using it, with one WARN naming the device. Look at that worker before the next run. Healthy runs
are unchanged.

[D94](deviations.md#d94--scheduler-drains-and-interrupted-ios-artifact-waits-have-deadlines) is new in 0.15.0 and needs no action.
All pool and queue drains share one 65 s deadline after the D93 device joins. The Scheduler warns about stalled
handlers and continues with committed results. An internal run failure or owner cancellation gives iOS artifacts
30 s to drain and 40 s for cancellation. Routine termination, including suites with failed assertions, keeps the
full 300 s drain and shares 60 s across its cancellation waits. The artifact log names bundles that could not
be retrieved.

[D95](deviations.md#d95--finish-deadlines-for-providers-connections-and-history) is new in 0.15.0 and needs no action.
Provider teardown, Apple lease release and history shutdown use bounded waits. See [Analytics and end-of-run failures](#analytics-and-end-of-run-failures)
for the shutdown and reporting details.

[D97](deviations.md#d97--publish-complete-artifacts-and-reserve-time-for-signal-cleanup) is new in 0.15.0 and needs no configuration change.
A failed, cancelled or timed-out receive leaves no partial artifact at the final path and preserves the previous
file, and marathon reports the failed delivery. On
a filesystem that rejects rename flags, replacing an Apple directory can leave the final path absent for a moment, and
if a failed publication cannot restore the previous tree, that tree is kept in a named backup. Report attachments
require a successful delivery. Android needs adb's DONE and a completed write, and rejects a pull when the received
count disagrees with STAT (low 32 bits with legacy STAT), as when a file grows during the transfer (a live app file
collected by file sync, say). An rsync receive whose output pipes are still open after its group's cleanup is also
undelivered, even if rsync exits successfully. Marathon's rsync SSH command ignores ControlMaster
and ControlPersist settings from SSH config.
After SIGINT, SIGTERM or SIGHUP, eligible artifacts drain through 12 seconds from the signal, and the exit takes over
by 24 seconds with up to 1 second more for console output. Read the undelivered-artifact diagnostics to identify missing files.

[D98](deviations.md#d98--a-simulator-recorder-is-never-interrupted-before-it-has-started) is new in 0.15.1 and needs
no configuration change. An iOS recorder gets INT only after simctl has printed `Recording started`; one that has not
by its INT cutoff gets TERM there, so a stop takes no longer than before. A simulator whose video service is busy
(simctl's "A recording is already in progress", which Xcode 26.6 words "Host recording is already in progress")
gets one warning and no further `recordVideo` command while its
service stays busy, also after it is set up again during the run; a confirmed shutdown or a boot from Shutdown clears
that. At the end of the run marathon makes one bounded attempt to shut it down, within its existing release window,
unless the on-dispose lifecycle already shuts it down or erases it, its work was not confirmed to have ended, it is
quarantined or no longer the run's, too little time is left, or a signal arrived. If its video service is still busy
after that, restart the simulator before it records again. The busy warning and the line reporting the shutdown, or
why it did not happen, print with `debug: false` from 0.15.2 (x104); in 0.15.1 they needed `debug: true`. If SSH command filters
on a worker list allowed commands, allow `xcrun simctl shutdown <udid>`. Workers without usable Perl keep the legacy
recorder, which this does not change.

[D99](deviations.md#d99--a-failure-that-meets-no-total-retry-quota-left-ends-its-test) is new after 0.15.5 and needs
no configuration change. With `retryStrategy: fixed-quota`, a failure that meets exactly 0 retries left of
`totalAllowedRetryQuota` ends its test as failed: it is counted in the summary and listed in `Failed tests:`, instead of
being left out with `Expected to run … more, but no terminal events received`. The tests that run, the retries and the
exit status are otherwise unchanged; only a retry that D69 gives back after the quota has run out can now let one more
failure be retried.

### iOS screenshot streaming

[D100](deviations.md#d100--stream-ios-screenshots-over-the-existing-ssh-connection) needs no Marathonfile changes.
If you filter `SSH_ORIGINAL_COMMAND`, allow the `/bin/sh -c … marathon-screenshot` command and its positional arguments.
It wraps `xcrun simctl io <udid> screenshot` with a per-frame file in that simulator's storage directory and
runs `/usr/bin/wc`, `/bin/rm` and `/usr/bin/base64`. With the supervisor the same command is the payload of a D96
`run` exec, not a detached recorder or rsync sender. Keep stdout unmodified and use `exec` in the forced command.
Without the optional supervisor the data connection runs the wrapper directly. Video and xcresult transfers
still require their existing allowlist entries.

[D102](deviations.md#d102--stop-android-video-once-and-check-recorder-artifacts) changes Android recorder stopping
and delivery. No configuration changes are needed. Android sends one INT and only uses KILL if it cannot
confirm that a known recorder exited. Unconfirmed or invalid movies and GIFs without a finished capture are not
included in new reports.

[D105](deviations.md#d105--a-full-remote-metrics-queue-does-not-delay-tests) is new after 0.15.6. Full remote analytics
queues now drop incoming metric events instead of delaying a batch. No configuration change is needed. Check for
`queue full` in the rejection summary when history is sparse. Local results and the shutdown deadline are unchanged.

### iOS recorder warnings

From 0.15.3 (x106) the worker supervisor's `Worker recorder … failed` line is DEBUG, and marathon's iOS recording code
emits at most one warning per recorder, as WARN on `marathon_ios::busy_video` (printed with `debug: false`). To alert
on recorder problems, match these messages on that target:

- `its video service is busy with a recording already in progress`: D98's busy warning, once per simulator until a
  confirmed restart clears the busy state; it tells you to shut the simulator down and boot it again.
- `did not confirm its launch in time`: marathon stopped waiting for the launch to be confirmed.
- `failed to launch, and marathon could not confirm that nothing was started`: the launch returned an error without
  establishing that no recorder started.
- `did not report how it ended`: marathon could not confirm the exit after the launch; it includes any captured
  stderr.
- `failed (`, the reason, then `). stderr:`: a recorder that failed with nonempty stderr. The reason is `status <n>`, `signal <n>`,
  `it did not start` or `no exit status`. It adds `Stop the active recording or restart the simulator before recording
  again` only when the stderr has simctl's busy text. This warning is omitted when stderr is empty; the unconfirmed
  warnings above can still appear.

The three unconfirmed cases stop further video on that simulator until it is set up again; they can appear without
any stderr. Every warning names the simulator. D98's busy warning names neither a video file nor a token. The launch warnings
name the video file but carry no token; only the exit and failure warnings include the first 8 characters of the worker recorder's token, when a worker session
supplied one.

### A finite `uncompletedTestRetryQuota` (D47)

Add one line at the top level of every Marathonfile, on Android, iOS and macOS alike:

```yaml
uncompletedTestRetryQuota: 5
```

Without it, or with 2147483647, `marathon run` and `marathon parse` exit 1. No other setting can stand in for it: nothing
else ends a run in which a test keeps coming back uncompleted. `retryStrategy` applies to failed tests only, the timeouts bound one batch, and on iOS a device-level limit only retires a simulator, never the run, and isn't charged for an unclassified error or a lost connection. That
setting stays optional, with 0 meaning off, and macOS and Android don't have it.

**Which value.** As a rule of thumb, not a measurement, use the number of devices + 2, and at least 5, and more with flakiness copies. Each time a test comes back uncompleted, each of its copies in that batch is charged once, and that includes a batch lost with its device, so a test that was only unlucky can be charged about once per device
during a bad outage. Too small a value turns an emulator restart or a lost worker into failed tests. The other side of the
choice is cost: a test that hangs every time takes (quota + 1) × the time it takes to give up, usually
`testOutputTimeoutMillis`. Any value other than 2147483647 is accepted, so a very large one keeps the old behaviour, if that is really what you want. A literal `null` or an empty value binds to 0, and 0 or a negative value gives a test no second attempt, including after device loss.

## Keep configuration and command placement

Use your existing valid configuration and bundles; a stub-vendor Marathonfile
is not a usable CLI quick start. These are examples for your configured test
environment, **not commands to run merely to validate installation**:

```sh
# Root invocation runs with ./Marathonfile:
marathon
# Root invocation with an explicit path:
marathon -m Marathonfile --analyticsTracking false --bugsnag false
# Equivalent explicit subcommand; its options follow it:
marathon run -m Marathonfile --analyticsTracking false --bugsnag false
# Lists discovered tests; can extract bundles and use workers:
marathon parse -m Marathonfile -o discovered-tests -f false
# Harmless installation check:
marathon version
```

The default file is `Marathonfile`. Root options are not inherited by subcommands:
`marathon -m custom-file run` does not select that file for `run`.
Boolean options need text arguments; only case-insensitive `true` is true,
so use explicit `true` or `false`, including `parse -f false`.
`--help`, `run --help` and `parse --help` show each command's own options.

Configuration keys, defaults and staged validation retain the Kotlin contract.
Environment substitution applies to Marathonfile, Marathondevices and each
dynamic update, including `${V}` and `${V:-default}`; preserve the environment
and consult [PLAN §3.2](../PLAN.md#32-configuration-three-deserialization-profiles-and-staged-validation)
for escaping, recursion and error rules. Relative Marathonfile `File` values
resolve against its directory; absolute file paths retain spelling at binding.
Apple post-processing separately canonicalizes devices, known-hosts and SSH key
paths. **Relative authentication keys in Marathondevices/dynamic updates resolve
when opened against process cwd**, not the Marathonfile directory (F4).
Keep cwd stable or supply absolute key paths.

Audit user regexes before switching (D6), including serial exclusions and test
filters. Supported string syntax such as `'^com\.example\..*Test$'` avoids
lookaround/backreferences; acceptance alone is not proof of equivalent matching.
Review malformed/exotic configuration exceptions in D9 rather than assuming all
Jackson edge cases are reproduced. The [schema](../crates/marathon-config/src/schema.rs),
[vendor schema](../crates/marathon-config/src/vendor.rs) and
[configuration evidence](wp/2.6-findings.md) are detailed references.

### Parse is discovery, with side effects

`parse` is not just configuration validation or a side-effect-free dry run.
Loading Apple bundles can delete the extraction directory and extract `.ipa` or
`.zip` contents before logical validation. Remote discovery initializes the
provider and borrows a worker; local discovery can skip provider initialization.
Requesting flaky tests can query history. Even with `-f false`, construction
can initialize configured analytics backends. Logical validation and provider-time
validation remain separate from configuration loading.

`parse -o discovered-tests` writes `<outputDir>/discovered-tests.yml`. It does not wipe `outputDir`.
The normal run path wipes `outputDir` later, after parsing and shard preparation.
Use a disposable, isolated environment for migration trials; never point both
implementations at the same active output directory or worker state.

## Outputs, logs and exit status

Reports keep the established directory layout: device/billing JSON, JUnit,
per-test and raw results, Allure, HTML/timeline, and vendor logs/media/artifacts.
The complete contract is [PLAN §3.4](../PLAN.md#34-output-directory-complete-manifest).
Account for the omitted JSON delegate field (D3), en-US formatting (D8), text
decoding (D10/D11, D55/D57), complete-character path cuts (D56), object/annotation
values (D58), generated failure wording (D59), execution-derived order (D60) and
image changes (D63). Serde JSON/YAML output preserves ordered source data rather
than text spelling; NEL in a YAML key now survives instead of becoming a space
([U184](found-bugs.md)). Video parity compares decoded content and metadata, not
identical encoded bytes. Under [PLAN §3.12](../PLAN.md#312-phase-8-contract),
per-test progress lines and the complete end-of-run summary block, including
`Marathon run finished` and `\tRaw:`, keep their text format. Other diagnostics
use [native tracing targets, formatting and visibility](design/logging.md).

The single maintained completion-message/stream/exit table is
[PLAN §3.1](../PLAN.md#31-cli-an-argument-and-exit-matrix). In particular,
`ignoreFailures` can give exit 0 with a failure message on stderr. Configuration
load and construction failures occur before the run catch; not every failure
prints `Marathon execution crashed`. `version` includes its startup log before
the bare version; scripts must not treat all stdout as a version string.

Crash/usage telemetry is disabled (D1), but configured Influx/Graphite remains
active (D4). **Do not rely on `readOnly` for isolation**: the Kotlin deserializers
ignore it (F3), and that bug is retained. Use isolated backends for comparisons.

## The xctest parser needs a library you supply

You need this section only if your Marathonfile selects the parser:

```yaml
vendorConfiguration:
  testParserConfiguration:
    type: xctest
```

The default is `nm`, which needs no library. If your tests are not generated at runtime,
switching to `nm` is the simple way out.

An `xctest` run injects `libxctest-parser.dylib` into a fake test run on the worker. The
native program does not contain this library
([D20](deviations.md#d20--the-xctest-parser-library-is-supplied-by-you)). Take a copy from a
Kotlin marathon distribution, under the terms of its
[EULA](https://github.com/MarathonLabs/marathon/blob/-/vendor/vendor-apple/base/src/main/resources/EULA.md),
and point the runner at it:

```sh
export MARATHON_XCTEST_PARSER_DIR=/path/to/libraries
```

The directory has the layout of the Kotlin resources (`libxctest-parser/` in its jar),
`<platform name>/libxctest-parser.dylib`. You need the platforms you test on:

| Devices | File |
|---|---|
| iPhone and iPad simulators | `iPhoneSimulator/libxctest-parser.dylib` |
| Apple TV simulators | `AppleTVSimulator/libxctest-parser.dylib` |
| Apple Watch simulators | `WatchSimulator/libxctest-parser.dylib` |
| Vision simulators | `XRSimulator/libxctest-parser.dylib` |
| macOS | `MacOSX/libxctest-parser.dylib` |

The variable is read on the machine that runs `marathon`, which pushes the file to the
worker as Kotlin did.

The library is third-party software and this project's licence does not cover it. Its EULA
says that it communicates with its server: it sends error reports, which cannot be disabled,
and anonymized usage data, which you may opt out of. Read the EULA before you use it, and
consider this if your workers have restricted network access.

Without a readable file the run stops with exit status 1 and a message naming the variable or
the file. macOS, and iOS with no simulator file at all, stop before any connection. An iOS run
with files for other platforms only stops once it has its first device, because only then is
the platform known; it does not retry or install anything.

## Retained behavior to account for

These statuses are from the [full bug ledger](known-kotlin-bugs.md) at the
reviewed SHA; the ledger links source locations and regression evidence.
Documentation adds no new reproductions.

| IDs | Consequence | Recorded status |
|---|---|---|
| A1 | Dynamic-provider EOF or errors other than connection refused stop updates permanently; this is distinct from recovery of an individual worker. | Reproduced; read-side oracle evidence exists, but completed native provider parity is not claimed here. |
| C1 / C2 | Batch filenames use `<batchId>.<batchId>`; webp/GIF screenshots carry a `jpg` suffix. Do not infer encoding solely from suffix. | Confirmed |
| A6 / N1 | The result-to-original-test lookup compares class with package and usually loses original test metadata. | Confirmed |
| F1 / F2 | An invalid execution-time `timeLimit` can report `bbb`; an invalid fixed-size batching limit silently becomes null. | Confirmed |
| F3 | Influx/Graphite `readOnly` is ignored, so it cannot protect comparison data. | Confirmed |
| F4 | Marathondevices relative keys are opened relative to process cwd. | Confirmed |
| F6 | macOS logical validation omits `MacosConfiguration.validate()`; successful validation is not proof that SSH/bundle settings are usable. | Confirmed |
| C3 | TeamCity auto-detection searches environment **values** for `TEAMCITY_VERSION`, rather than checking the key. A normally named variable alone does not ensure detection. | Confirmed |
| N4 | Kotlin's Android preparation clears the whole external-storage root, for example `/sdcard`. Native builds do not ([D27](#android-external-storage-is-no-longer-wiped)). | Fixed by D27 |
| C41 | Kotlin keeps a failed history query's empty answer for the whole run. Native builds query again in the background, a bounded number of times, and a queue already built keeps its order. With a failing backend the log has more `Cannot fetch … from database` warnings and timing lines ([D34](deviations.md#d34--a-failed-history-query-is-not-final)). | Fixed by D34 |

## Android external storage is no longer wiped

Kotlin's prepare step runs `rm -r $EXTERNAL_STORAGE` and `mkdir $EXTERNAL_STORAGE` on every device.
Native builds run neither ([D27](deviations.md#d27--prepare-no-longer-wipes-external-storage)).

- **Action for you:** if your tests rely on an empty external storage, for example because they count files
  or fail on leftovers, clear what they need yourself, in the tests or in your device setup.
- **What marathon still cleans:** each file sync pull entry is removed and recreated before every batch, and
  file sync pushes are replaced and then removed after the batch. A pushed folder that can't be removed first fails
  the batch with `Could not delete remote file(s): …`; give the shell user write access to the destination. Videos are removed after each test.
- **What can now accumulate:** videos left on the device by a run that was aborted, or by a recording that a lost
  connection cut short, in a run that still exits 0. Their names hold that run's batch id, which a later run
  cannot know, and marathon does not delete by pattern, because that could hit your data. Clear them with your
  device setup if your devices are long-lived.

## Other Android behaviour changes

These fix bugs that Kotlin has, and need no configuration change.

- **`socketIdleTimeout` is in real time** ([D21](deviations.md#d21--socketidletimeout-means-what-it-says)). It
  bounds only four requests without a timeout of their own (see the table above). A value picked with Kotlin's unit
  bug in mind is now taken as written.
- **An adb server restart no longer ends device tracking**
  ([D22](deviations.md#d22--a-run-survives-an-adb-server-restart)). A loopback server is started again.
  A server still away after `waitForDevicesTimeoutMillis` (30 s by default) loses its devices. Remote test parsing
  waits for the server too; if it never returns at that stage, the run fails with `no devices available` when
  `deviceInitializationTimeoutMillis` (180 s by default) has passed. An emulator that restarted during an outage is
  set up again when its boot ID changed or its preparation marker is confirmed missing or different
  ([D85](deviations.md#d85--android-checks-the-boot-before-resuming-tests)); if the marker's creation could not be
  confirmed, only the boot ID is checked. One whose restart keeps both, or that gives no readable evidence of a reset, is
  not prepared again, so its batches fail on the missing APKs, until `uncompletedTestRetryQuota` runs out
  ([D47](deviations.md#d47--an-unlimited-uncompletedtestretryquota-is-refused)).
- **A brief `offline` no longer loses a device**
  ([D41](deviations.md#d41--a-brief-offline-does-not-lose-the-device)). A device gets up to 10 s, 3 s if it also leaves
  the list, so a killed emulator takes about 3.25 s to reach the retry path, where it was immediate. A device that
  returned takes new batches once it has answered its `getprop` and its logcat and reverse port forwards are restored;
  that no longer holds up other devices (U170). A device has at most 5 of these per adb server per run, and the count is
  never reset.
- **A lost adb request no longer retires a device that is still there**
  ([D49](deviations.md#d49--a-lost-adb-request-does-not-retire-a-device-that-is-still-there)). A device listed as `device` that
  answers loses only the batch, 3 times per adb server per run, and the count is never reset. One `getprop` is sent per loss.
- **A file sync push that never verifies fails after `pushFile`**
  ([D23](deviations.md#d23--waiting-for-a-pushed-file-has-a-limit)).
- **`networkState` can read `DISCONNECTED`** in a lost Android device's per-test JSON
  ([D24](deviations.md#d24--a-lost-android-device-stays-out-of-use-and-says-so)). A consumer that assumed it was
  always `CONNECTED` should be checked. iOS and macOS devices still report `CONNECTED`.
- **Failed instrumentation streams are reported** with their cause
  ([D25](deviations.md#d25--a-failed-instrumentation-stream-is-reported)). Uncompleted tests may carry that text
  instead of the generic one, `No test results` or none, and a remote parse cut off by a lost connection is retried
  instead of giving a shorter test list. A parse cut before its first status atom, after other text, still gives no
  tests for that bundle, with no retry. A run that used to pass with tests silently missing can now run them all, or
  fail.
- **A remote parse that fails short of its list is a failed attempt**
  ([D42](deviations.md#d42--a-remote-parse-that-fails-short-of-its-list-is-a-failed-attempt)). A session whose
  `INSTRUMENTATION_CODE` isn't -1, which lists fewer tests than its `numtests`, is retried, and fails the run with its
  text if it keeps doing so. A custom runner that stops listing early and exits this way used to run the part it listed.
- **DEX 040 and 041 test APKs are accepted**
  ([D26](deviations.md#d26--dex-040-and-041-test-apks)).
- **Code coverage needs no `run-as`**
  ([D81](deviations.md#d81--android-code-coverage-needs-no-run-as)). The instrumentation writes coverage to
  `<external storage>/Android/data/<app>/files/coverage/`, so a non-debuggable app gets coverage too. The `.ec` files
  land in `device-files/<pool>/coverage/` as before; a missing one is an ERROR, and each test's result file lists its
  file as a `COVERAGE` attachment.
- **A retry goes to another device**
  ([D82](deviations.md#d82--a-retry-is-kept-off-the-device-it-came-from-also-without-a-worker)). A test that comes
  back incomplete or is retried after a failure is left to the pool's other devices while other tests are queued, so one
  emulator whose instrumentation keeps failing no longer takes the same test back until its `uncompletedTestRetryQuota`
  is used up. The device that gave it back runs its other queued tests first and takes the retry itself only when
  nothing else is queued for it, so in a pool of one device a retry runs at the end of the queued work. Another device
  takes it when it asks, by the queue's priority and batching. The quotas and exit-code rules are the same, but a test
  can need fewer attempts and get another verdict (a retry that passes on another device ends it), so with
  `ignoreFailures: false` a run can exit 0 where it exited 1, or the reverse. Reports name other devices: `raw.json`
  device serials and row order change, and so can the device of a test's final result. Unless a fresh copy of the same
  test is queued too (then `class-name` batching can still send both copies with fresh tests), a device's own retry no
  longer shares a batch with fresh tests, so `fixed-size` batching can start one more instrumentation, which adds its start-up time, and the
  `am instrument` commands change in number, order and `-e class` grouping.
- **Each instrumentation runs only its own tests**
  ([D28](deviations.md#d28--each-instrumentation-runs-only-its-own-tests)). This only matters for batches
  that mix test APKs, which takes `class-name` batching and the same class name in both.

## Batches hold more tests with flaky copies or several test APKs

`fixed-size` batching now counts only the tests a batch accepts
([D50](deviations.md#d50--a-rejected-test-does-not-use-a-batch-slot)). Batches that used to end early now take tests
until they hold `size` or their expected durations add up to `durationMillis`. That is where a batch stops, not a cap:
the test that reaches it is still in the batch, so two 4 s tests make an 8 s batch with a 6 s limit, as in Kotlin. A run
makes fewer instrumentation or xcodebuild starts and each batch can take longer. Check `testBatchTimeoutMillis` if you
set it close to what those short batches took. Under ties, as with `no-sorting`, tests can run in another order after a
batch that rejected a test.

## Other Apple behaviour changes

- **A failed readiness probe means not running**
  ([D38](deviations.md#d38--a-failed-readiness-probe-means-not-running)). On iOS 16 and 17 each probe returns about 20 MB of
  stdout (from 18 on it is about 100 KB, see D46 below). On a
  host or link too slow to deliver that within `shell` (30 s by default), every probe now times out, where Kotlin counted
  the marker once about 80% had arrived. A simulator then gets setups of two `bootWait`s each until its preparation
  deadline ([D87](deviations.md#d87--a-devices-preparation-has-one-deadline)) and leaves after one replacement, about
  9 to 12 minutes per simulator with the defaults (scenario estimates with the other commands answering at once), before
  it is out of the run. That holds while another simulator keeps the pool alive. On the last device the run ends 180 s
  ([D48](deviations.md#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)'s wait) after the simulator left the
  pool, or after a test result it reports later, with `remaining tests will not run`: about 9 to 11 minutes after its first
  boot, or about 3 minutes after a loss if the probe starts failing at a re-setup. Raise `shell` if that happens. A configured
  `onPrepare: [ERASE]` erases on each re-setup, as on any setup.
- **The readiness probe is `launchctl print user/foreground` from iOS 18**
  ([D46](deviations.md#d46--the-readiness-probe-is-launchctl-print-userforeground-from-runtime-18)). The answer is about
  100 KB instead of 10 to 20 MB, with the same marker line, and `dumpstate` stays for 16 and 17. A worker command filter
  that matches the old command must also allow `xcrun simctl spawn <udid> launchctl print user/foreground`.
- **A test's own text does not abort its batch**
  ([D39](deviations.md#d39--a-tests-own-text-does-not-abort-its-batch)). A failure text in a test name, an activity line,
  a Swift Testing event or an application's log line no longer reports. In an assertion it reports only when the message
  starts with it, which is how a failure the framework records reads. That rule is reasoned from Xcode's formats, not
  recorded. After a prefix such as `XCTAssertTrue failed - `, that test fails and the batch goes on. If a batch now waits
  for its timeout (`testOutputTimeoutMillis`, 300 s by default, once per batch, and three in a row with no test passed or
  failed set the simulator up again; on a pool's only simulator the pool then waits for it as D48 says), or a sick
  simulator keeps taking batches, look at the failing line: the
  [deviations guide](deviations.md#d39--a-tests-own-text-does-not-abort-its-batch) lists the known misses.

## Other Apple worker behaviour changes

- **A partition expires Apple worker leases** ([D96](deviations.md#d96--the-controller-owns-its-local-process-groups)).
  With or without SSH keep-alive, two consecutive 4 s windows without a control
  reply cause a reconnect. That takes at least 8 s plus the 1 s retry delay after a
  failed command-path fallback. A detached recorder's held INT is excluded from
  this count. The default lease is 79 s. The lease reserves at least the renewal interval
  plus 26 s for control recovery. One already issued renewal can extend the
  last observed deadline once, so an already-running group can survive up to
  two lease periods plus the worker poll interval, normally 50 ms. A recorder
  gets up to another 3 s to finalize and stop. A queued
  exec whose stop cannot reach the worker starts its lease when the worker
  admits it. A frozen worker can act after it runs again. If both paths stay
  down, the private supervisor directory and one small receipt per finished
  token can remain under `/tmp`. Ordinary close attempts to remove the
  directory while the worker is reachable.
- **Stopping an iOS recorder before launch acknowledgement can leave an undelivered movie.**
  Marathon requests a token stop when it cannot reach an acknowledged recorder
  session. If the recorder has started, the worker gives it INT before escalation.
  A failed detached recorder reports bounded stderr when available.
  If its movie cannot be confirmed finalized, D97 reports it as undelivered.
- **Xcode shared service hosts can outlive a run.** Helpers such as
  `DTServiceHub` use their own groups and can reparent to the system service
  manager. They are shared across Xcode sessions and remain available.

These fix bugs that Kotlin has, and need no configuration change.

- **A rejected command frees its SSH session slot**
  ([D29](deviations.md#d29--a-rejected-command-gives-its-session-slot-back)). The command still fails with
  `Request failed`; the commands after it no longer fail for want of a slot. A command with no reply in 30 s is closed the same way, and
  an error line starting `No reply to exec` names it. A channel that a worker opens after its 30 s are up is closed then, and the line
  starts `No reply to opening`.
- **Closing a session takes at most 30 s, and can kill the process**
  ([D30](deviations.md#d30--closing-an-ssh-session-has-a-limit)). A process that ignores TERM gets KILL after 20 s in
  ordinary cleanup. A run interrupted by a signal requests KILL by 17 s from that signal. A
  server that ignores signals can leave it running; an error line starting `Abandoning session` then names the command.
  On iOS an abandoned session retires its simulator, and a pool whose last device it was waits
  ([D48](deviations.md#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)), then ends.
- **A saturated shared SSH connection is replaced**
  ([D90](deviations.md#d90--a-saturated-shared-ssh-connection-is-replaced)). With `shareWorkerConnection: true`, a worker
  that runs out of session slots after an abandoned session can see an extra connection. The old connection stays up
  for its existing leases, and the next acquisitions can use the new one. Each shared transport has two generation
  slots. The SSH warning names the session-open refusal even with debug off.
- **An rsync send that stops moving fails after 300 s, and its device leaves its pool**
  ([D31](deviations.md#d31--an-rsync-send-that-stops-moving-fails)). If you wrap rsync with `RSYNC4J_RSYNC`, the wrapper now
  also gets `--timeout=300`. On a host with rsync 3, so does the worker's `rsync --server`: check any forced
  command or wrapper there that matches that string. A worker that is silent for more than 5 minutes during a send now fails
  that send. It isn't retried: the device leaves its pool, an iOS simulator for the rest of the run, and sends waiting behind it to that worker fail at once. A second worker
  carries on. If it was the last device, the pool waits ([D48](deviations.md#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)), then ends. Any other failed send keeps its retries, up to 90 sends. A link that dies during a send is noticed by keep-alive in about 75 s.
  A stall counts only after 285 s with no progress by the bridge's clock, and only when rsync prints its own report as a whole line (`rsync(<pid>): error: poll: timeout`,
  `[sender] io timeout after 300 seconds -- exiting`) or exits with 30 without `stopping at requested limit`; an rsync that words it differently, with a hung ssh, still waits.
  A process left holding a receive's output can hold it up for at most 5 s.
- **An rsync pull is no longer cut at 30 s**
  ([D43](deviations.md#d43--an-rsync-receive-has-a-limit-on-progress)). A pull finishes however long it takes, as long as its own files change at least once every 300 s:
  rsync reports a stall only after 300 s without I/O, and that report counts only 285 s after the files last changed.
  Otherwise it is stopped, usually with `.rsync-partial/<name>` left in the output directory. A link slower than about 1 KiB/s, or a wrapper that makes rsync write
  somewhere else, such as another `--partial-dir`, can look stalled. Each pull that hangs on a stuck worker now costs about 305 to 330 s, where it cost 35 to 45 s,
  and nothing bounds the total over a run.
- **A connection lost after login is a connection failure**
  ([D36](deviations.md#d36--a-connection-lost-after-login-is-a-connection-failure)). On iOS the simulator is
  reconnected; on macOS the device leaves the run. For a registered iOS simulator whose worker stays down, the reconnect
  has a limit ([D44](deviations.md#d44--a-lost-simulators-reconnect-has-a-limit)).
- **One unread output stream no longer kills the connection**
  ([D37](deviations.md#d37--one-unread-output-stream-no-longer-kills-the-connection)). The command blocks until its
  output is read, times out or is closed.
- **xcodebuild's command line on a worker starts with `env NSUnbufferedIO=YES`**
  ([D32](deviations.md#d32--xcodebuild-on-a-worker-runs-unbuffered)). Check anything on the worker that matches the command, such
  as a forced command. Output arrives as it is produced, so `testOutputTimeoutMillis` measures real silence.

## Analytics and end-of-run failures

Graphite and InfluxDB writes queue immediately while there is room. A full queue drops the incoming event without
waiting (D105). Only a slow send failure starts the five-minute cool-off. The final drain and close, including
InfluxDB's last flush, share 30 s. A warning
marks expiry and the remaining metrics are abandoned. Missing observations can affect later runs' flakiness and
duration strategies. A failed analytics timestamp or tracker call during iOS or macOS device setup or preparation
produces a warning and successful device work carries on. A failure in one close still allows later closes to run,
and the existing exit-code rules apply. A run whose filters removed every test exits 0 and logs what each filter removed.
See [D51](deviations.md#d51--metrics-and-cleanup-failures-do-not-stop-a-run) and
[D86](deviations.md#d86--remote-metrics-use-bounded-buffering).

[D95](deviations.md#d95--finish-deadlines-for-providers-connections-and-history) applies the same 30 s allowance to
history analytics close, after the existing 3 s refresh join. Android provider termination shares 65 s by default.
The iOS provider has 70 s for its own teardown after artifact finalization. The D94 artifact windows are 300 s
plus 60 s on routine termination, including failed assertions, and 30 s plus 40 s after an internal run failure
or owner cancellation, when the provider also applies that window to pulls still running from replaced simulators.
After SIGINT, SIGTERM or SIGHUP, D97's signal cutoffs apply instead.
Unconfirmed simulators skip on-dispose commands and keep their worker lease quarantined. The last Apple worker
lease has at most 40 s for disconnect.
Report generation logs the active reporter after one minute and repeats the warning at most once a minute until it finishes.

## Cancelling a run

The process reaper appears as a second `marathon` process with the same
arguments until the controller exits. It ignores INT, TERM and HUP. Scripts
that count matching processes or use `pgrep`, `pidof` or `killall` can see
both. Use the controller PID saved when starting Marathon to target that run.

On SIGINT, SIGTERM or SIGHUP, marathon attempts to write reports if the output directory is ready and starts
provider cleanup, whatever `debug` says. It hands off to exit by 24 s, with up to 1 s more for console output.
Reports or cleanup can remain unfinished. Internal failures and owner cancellation use D94's shorter artifact
window; signals use D97's cutoffs. A second signal exits at once, however soon after the first. Two of the same signal
that arrive together count as one. Exit statuses are 130 for SIGINT, 143 for SIGTERM and 129 for SIGHUP.
A cancelled run's output directory can hold partial results, so use the exit status to tell whether a
run finished. A harness that waits less than 25 s for exit should allow for it.
See [D52](deviations.md#d52--a-cancel-writes-the-reports-and-cleans-up-within-a-limit).

Tools on the controller run in their own process groups. Group-directed signals reach Marathon.
Its cleanup and exit sweep stop the registered tools, and the reaper ends their groups even if the
controller is killed before cleanup finishes. On macOS, a timed-out local batch can close normally
once the command group has exited, even while it still has zombie members.
ADB's `start-server` uses non-terminal standard streams.
A tool with inherited terminal output can stop under `stty tostop`, and SSH can stop if it tries to prompt
on `/dev/tty` for a password or a key passphrase. Use noninteractive authentication and redirect inherited
terminal output when that terminal setting is enabled.
See [D96](deviations.md#d96--the-controller-owns-its-local-process-groups).

## Trial and rollback

Retain the previous executable and its configuration, inputs and state snapshots.
Choose separate output directories and isolated analytics backends for trials.
For Apple, disjoint simulators on the same worker are not isolation: fixed paths
under `/tmp/marathon`, shared bundles, `marathon.xctestrun` and rsync `--delete`
can conflict. Concurrent comparisons require disjoint workers. Sequential reuse
requires restoring all of `/tmp/marathon` including `shared/`, resetting simulator
state and verifying that leftover test/recording/log processes have exited.
macOS comparisons require a full host snapshot restore, including TCC databases
and test-created state, or disjoint hosts. Android comparisons use disjoint
devices and clean tracing/recording state between runs. Follow the complete
[isolation contract](../PLAN.md#67-shadow-runs-on-the-device-farm-then-cutover).

To roll back, stop the native run and finish its cleanup, restore the isolated
state/snapshots, and point PATH or the CI executable back to the retained JVM
launcher with its original runtime prerequisites. Keep native artifacts separate
for diagnosis. Selecting the old executable alone does not undo worker changes.
Shadow and canary execution belongs to the release rollout, not this documentation
check; no concurrent shared-worker safety is claimed.

### Apple code coverage

Build the app and test bundles with `xcodebuild build-for-testing
-enableCodeCoverage YES`. Set `isCodeCoverageEnabled: true` and point
`vendorConfiguration.bundle.xctestrun` to the xctestrun produced by that build,
relative to the Marathonfile. The runner copies coverage targets into each
generated xctestrun, relocates products to their installed bundles and stages
static libraries on the worker. Each batch writes coverage into its xcresult;
merging coverage across batches remains a consumer step.

With coverage disabled, `bundle.xctestrun` is ignored, including a missing file.
With coverage enabled, a supplied path must be a file and must contain coverage
buildable infos. Enabling coverage without supplying that path emits one warning
per process; the generated xctestrun has no coverage targets.
