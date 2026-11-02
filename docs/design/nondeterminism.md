# Kotlin nondeterminism inventory

WP 0.2b; source commit `e47352f2`. The complete, line-addressed inventory is
[`nondeterminism-sites.csv`](nondeterminism-sites.csv). It has one row per `(file,line,search family)`, including reviewed
false positives, so a repeated search cannot silently lose an occurrence. Every nontrivial row has a label, kind, treatment,
and explanation. Hook labels are an interface for Rust replay; survey-only `.L<number>` labels identify frozen-source sites
and are not new runtime seams.

## Reproduce the search

```sh
python3 tools/oracle/hook_inventory.py search /path/to/frozen/kotlin/tree
python3 tools/oracle/hook_inventory.py check /path/to/frozen/kotlin/tree \
  tools/oracle/build/marathon-patched
```

`search` prints CSV. `check` requires exact coverage and matching source text, rejects missing labels/unreviewed rows, and
checks both directions between hooked labels and the patched main sources. `update` is a maintenance command: new hits are
marked TODO and cannot pass `check`. Do not run it against the patched tree as the reference input.

The scope is every main source in `docs/source-inventory.csv` except build logic and sample apps, plus the CLI logback XML.
The full regexes live in `FAMILIES` in the script. They cover wall/monotonic reads and method references; clock injection and
formatting; delays, timeouts, retries, blocking waits and polling; PRNGs, collection random/shuffle and crypto factories; UUIDs,
temporary paths and process IDs; launches, async/actor/produce, sends, completion callbacks, dispatchers, subscriptions,
vendor prepare/execute and shutdown; hash containers; filesystem traversal; and library calls/`toString` that can leak IDs.
Imports and comment-only lines are excluded. Rows retain the source expression for review rather than claiming a regex match
necessarily means nondeterminism.

The manual follow-through was from these hits into `SystemTimer`, the Scheduler → DevicePoolActor → QueueActor/DeviceActor
contexts, `notifyIsReady`, deferred vendor results, StubDeviceProvider, vendor thread pools/transport readers, reporters,
configuration `toMap`, and the generated Android outputs. That last check found two sources a time/UUID-only grep misses:
Logback's internal event clock and array/Object identity strings in report metadata. Whole-file report comparison exposed
the DEX array identity leak (C19). The library rows include ordinary value formatting false positives deliberately.

This is a reproducible audit of the frozen code, not a proof that the regexes can discover every future library behavior.
Source changes require re-review, especially new executors and serialization dependencies.

## Controls and boundaries

| Kind | Control |
|---|---|
| Wall | Injected production/test `Clock`; all direct `Instant.now`, `Date`, `currentTimeMillis`, method references and `measureTimeMillis` reads pass through labelled hooks. Formatter-only sites retain their original locale/timezone semantics; validation pins them |
| Monotonic | Direct reads, `System::nanoTime` defaults, artifact admission, simulator boot deadlines, recovery and worker inventory caches use labelled nanosecond hooks |
| Scheduler | Core context is `StandardTestDispatcher(TestCoroutineScheduler)`. Only explicit event-time entries advance it; inherited `delay`, timeout, retry and readiness work use that dispatcher |
| Random | Signed integer draws for random sorting; keyed collection picks for each Android/iOS/macOS provider |
| IDs | Per-site UUID/temp-file tapes, plus alias-preserving identity suffixes from actual objects in Configuration and RawJsonReporter |
| Core concurrency | One event at a time; drain runnable tasks, then verify registered actor/provider mailboxes are empty. Readiness launches are drained too |
| Live Android | Ordered setup/connection, prepare and execute barriers. Results still come over fake-adb. Scheduler/socket time remains real; the validation's wall/monotonic clocks are frozen |
| Vendor/worker concurrency | Core replay supplies explicit device completions instead of running SSH, processes, media capture or vendor background jobs. The CLI case exercises one real Android transport scenario, not arbitrary vendor schedules |
| Hash/container order | Retained JVM containers; fixed keys and scripted insertion order in covered scenarios. Identity-keyed or concurrently populated vendor containers do not acquire a general cross-runtime order guarantee |
| Filesystem order | Native iteration retained; the fixture snapshot is an input. Apple bundle enumeration and live host directory traversal are not virtualized |
| Library/environment | Hooked log event dates and report identities. Pin JDK, locale, timezone, working paths, JVM heap options and fixture files. SSH cryptographic entropy is deliberately live: plaintext worker operations, not encrypted bytes, are the comparison surface |

All non-hook treatments are explicit CSV rows. `via` means a named upstream clock/ID is already controlled; `dispatcher`
means inherited core scheduler control; `script` means an external boundary supplied by replay; `uncontrolled` names a retained
host/vendor behavior and its limitation; `n/a` is a reviewed non-source. In particular, treating a vendor operation as scripted
does **not** mean arbitrary real I/O can safely be run on the core virtual scheduler.

Log messages are already formatted strings: their literal identity-shaped text is preserved. Arbitrary debug identity
messages remain live and are outside this CLI variant's guarantee; only Logback event dates are controlled there.

Three clocks and read/draw/ID arrays are specified in [`replay-tapes.md`](replay-tapes.md). The service never substitutes host
elapsed time when a virtual clock is requested. Partial tapes can explicitly request real clocks. The CLI case uses that mode
for the scheduler and is separate from the core quiescence proof. A virtual scheduler tape on the ordinary CLI entry fails
without an installed driver.

## Patched call sites

The following table summarizes the runtime-labelled sites; the CSV also contains every indirect, scheduled, external,
container, filesystem and false-positive hit. Repeated reads at one logical site share a tape cursor (for example start/end
measurement reads); they are both listed in the CSV.

| Label | Frozen source location(s) |
|---|---|
| `android.AdamDeviceProvider.borrow` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt:191` |
| `android.AdamDeviceProvider.connected` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt:158` |
| `android.AdamScreenCaptureTestRunListener.testEnded` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/screenshot/AdamScreenCaptureTestRunListener.kt:65` |
| `android.BaseAndroidDevice.detectRealSerialNumber` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/BaseAndroidDevice.kt:170` |
| `android.BaseAndroidDevice.waitForRemoteFile` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/BaseAndroidDevice.kt:204` |
| `android.ProfilingRunListener.join` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/profiling/ProfilingRunListener.kt:145` |
| `android.ProfilingRunListener.pullTracing` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/profiling/ProfilingRunListener.kt:118` |
| `android.ProfilingRunListener.stop` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/profiling/ProfilingRunListener.kt:141` |
| `android.ScreenRecorder.startRecordingTestVideo` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorder.kt:27` |
| `android.ScreenRecorderTestBatchListener.join` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorderTestBatchListener.kt:126` |
| `android.ScreenRecorderTestBatchListener.pullLastBatchVideo` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorderTestBatchListener.kt:194` |
| `android.ScreenRecorderTestBatchListener.pullTestVideoFile` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorderTestBatchListener.kt:179` |
| `android.ScreenRecorderTestBatchListener.removeRemoteVideoFile` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorderTestBatchListener.kt:216` |
| `android.ScreenRecorderTestBatchListener.stop` | `vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/video/ScreenRecorderTestBatchListener.kt:122` |
| `apple.AppleDeviceTestRunner.closeSession` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleDeviceTestRunner.kt:102`; `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleDeviceTestRunner.kt:105` |
| `apple.ArtifactProcessor.await` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/ArtifactProcessor.kt:297` |
| `apple.ArtifactProcessor.clock` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/ArtifactProcessor.kt:35`; `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/ArtifactProcessor.kt:40` |
| `apple.ArtifactProcessor.closeAndJoin` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/ArtifactProcessor.kt:359` |
| `apple.KotlinProcessCommandExecutor.currentTimeMillis` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/local/KotlinProcessCommandExecutor.kt:69` |
| `apple.Reader.currentTimeMillis` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/extensions/Reader.kt:46` |
| `apple.ResultBundleRunListener.deliver` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/ResultBundleRunListener.kt:88`; `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/ResultBundleRunListener.kt:94` |
| `apple.ResultBundleRunListener.processBundle` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/ResultBundleRunListener.kt:76` |
| `apple.SshjCommandExecutor.currentTimeMillis` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExecutor.kt:50` |
| `apple.TestResultsListener.enhanceResults` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/TestResultsListener.kt:87` |
| `apple.TestRootFactory.generate` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/TestRootFactory.kt:49` |
| `apple.XCTestParser.parseTests` | `vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/XCTestParser.kt:62` |
| `config.Configuration.identities` | `configuration/src/main/kotlin/com/malinskiy/marathon/config/Configuration.kt:55` |
| `config.InstantTimeProviderImpl.referenceTime` | `configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/time/InstantTimeProviderImpl.kt:6` |
| `core.Actor.mailbox` | `core/src/main/kotlin/com/malinskiy/marathon/actor/Actor.kt:29` |
| `core.AllureReporter.uuid` | `core/src/main/kotlin/com/malinskiy/marathon/report/allure/AllureReporter.kt:54` |
| `core.BasicGraphiteClient.timestamp` | `core/src/main/kotlin/com/malinskiy/marathon/analytics/external/graphite/BasicGraphiteClient.kt:20` |
| `core.BillingReporter.defaultStart` | `core/src/main/kotlin/com/malinskiy/marathon/report/bill/BillingReporter.kt:21` |
| `core.BillingReporter.generate` | `core/src/main/kotlin/com/malinskiy/marathon/report/bill/BillingReporter.kt:66` |
| `core.DeviceActor.execute` | `core/src/main/kotlin/com/malinskiy/marathon/execution/device/DeviceActor.kt:191` |
| `core.DeviceActor.prepare` | `core/src/main/kotlin/com/malinskiy/marathon/execution/device/DeviceActor.kt:170` |
| `core.FileTransferExtensions.measureFileTransfer` | `core/src/main/kotlin/com/malinskiy/marathon/device/file/FileTransferExtensions.kt:13`; `core/src/main/kotlin/com/malinskiy/marathon/device/file/FileTransferExtensions.kt:9` |
| `core.FileTransferExtensions.measureFolderTransfer` | `core/src/main/kotlin/com/malinskiy/marathon/device/file/FileTransferExtensions.kt:34` |
| `core.HtmlSummaryReporter.date` | `core/src/main/kotlin/com/malinskiy/marathon/report/html/HtmlSummaryReporter.kt:55` |
| `core.MappingTracker.deviceConnected` | `core/src/main/kotlin/com/malinskiy/marathon/analytics/MappingTracker.kt:17` |
| `core.MappingTracker.deviceDisconnected` | `core/src/main/kotlin/com/malinskiy/marathon/analytics/MappingTracker.kt:21` |
| `core.MappingTracker.test` | `core/src/main/kotlin/com/malinskiy/marathon/analytics/MappingTracker.kt:33` |
| `core.Marathon.run` | `core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt:71` |
| `core.Marathon.runAsync.parsing` | `core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt:107` |
| `core.MetricsProviderImpl.executionTimes` | `core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt:106`; `core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt:114` |
| `core.MetricsProviderImpl.successRate` | `core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt:56`; `core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderImpl.kt:62` |
| `core.RandomOrderSortingStrategy.key` | `core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/sorting/RandomOrderSortingStrategy.kt:13` |
| `core.RawJsonReporter.identities` | `core/src/main/kotlin/com/malinskiy/marathon/report/raw/RawJsonReporter.kt:19` |
| `core.Scheduler.deviceEvents` | `core/src/main/kotlin/com/malinskiy/marathon/execution/Scheduler.kt:76` |
| `core.ScreenCapturer.capture` | `core/src/main/kotlin/com/malinskiy/marathon/device/screenshot/ScreenCapturer.kt:45` |
| `core.TemporalTestResult.startTime` | `core/src/main/kotlin/com/malinskiy/marathon/execution/result/TemporalTestResult.kt:7` |
| `core.TestBatch.id` | `core/src/main/kotlin/com/malinskiy/marathon/test/TestBatch.kt:5` |
| `core.Track.trackDevicePreparing` | `core/src/main/kotlin/com/malinskiy/marathon/analytics/internal/pub/Track.kt:39`; `core/src/main/kotlin/com/malinskiy/marathon/analytics/internal/pub/Track.kt:41` |
| `core.Track.trackProviderDevicePreparing` | `core/src/main/kotlin/com/malinskiy/marathon/analytics/internal/pub/Track.kt:47`; `core/src/main/kotlin/com/malinskiy/marathon/analytics/internal/pub/Track.kt:49` |
| `core.di.Clock` | `core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt:48` |
| `ios.AppleSimulatorDevice.bootForPreparation` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt:860`; `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt:862` |
| `ios.AppleSimulatorDevice.getScreenshot` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt:811` |
| `ios.AppleSimulatorDevice.nanoTime` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt:438`; `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt:509`; `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt:880`; `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt:905` |
| `ios.AppleSimulatorProvider.borrow` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:449` |
| `ios.AppleSimulatorProvider.nanoTime` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:647` |
| `ios.DeviceLogListener.pullLogfile` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/log/DeviceLogListener.kt:74` |
| `ios.ScreenRecordingListener.pullLastBatchVideo` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt:201` |
| `ios.ScreenRecordingListener.pullTestVideo` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt:192` |
| `ios.ScreenRecordingListener.removeRemoteVideo` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt:208` |
| `ios.ScreenRecordingListener.transcode` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt:170` |
| `ios.SimulatorProvisioner.create` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/SimulatorProvisioner.kt:178` |
| `ios.WorkerProvisioningContext.nanoTime` | `vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/WorkerProvisioningContext.kt:34` |
| `macos.AppleMacosProvider.borrow` | `vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/AppleMacosProvider.kt:122` |
| `macos.MacosDevice.grant` | `vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/MacosDevice.kt:510` |
| `test.ConfigurationFactory.outputDir` | `vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/factory/ConfigurationFactory.kt:24` |
| `test.MarathonFactory.clock` | `vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/factory/MarathonFactory.kt:51` |
| `usage.GrafanaCloud.id` | `analytics/usage/src/main/kotlin/com/malinskiy/marathon/usageanalytics/tracker/GrafanaCloud.kt:26` |
| `usage.Metric.time` | `analytics/usage/src/main/kotlin/com/malinskiy/marathon/usageanalytics/tracker/Metric.kt:6` |

Logback additionally uses `library.logback.wall` inside
`tools/oracle/hooks/src/main/kotlin/oracle/hooks/Logging.kt`; the application call sites are the Android/Apple
`PatternLayout` constructors and `cli/src/main/resources/logback.xml:3`. Their CSV treatment is `via`.

## Validation contract

Run `tools/oracle/validate-patches.sh` (default 10 repeats). `--runs 1` is an explicitly reduced run, not a ten-run pass.
`--parts a,b,c` selects checks; `--skip-build` uses existing distributions and is for iteration only. Every complete run builds
both distributions, checks the inventory, builds fake-adb, and writes `tools/oracle/build/validation/results.json` plus logs
and captured output trees. These paths are disposable evidence, not golden fixtures. A failed comparison exits nonzero and
retains its inputs; reference differences get readable diffs.

**(a)** The unactivated patched clone runs the root Gradle `test` task, including every module, plus the hook module tests.
The sole exclusion is `com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutorTest`, the exact `excludeTests`
value in the frozen fork's CI. Its command tests require a Docker sshd. Integration tests are a separate source set, not
quietly counted as passed unit tests. XML test totals and skipped counts are recorded.

**(b)** All 13 source scenario functions are represented, including the unannotated first DeviceFilteringScenarioTest case.
They retain source names, conditions, retry counts and assertions, and compare the unchanged files already in
`fixtures/core/output/raw`. The extra `scripted` input exercises explicit completions and the synchronous `runBlocking` seam.
Each case runs in a fresh JVM with a fresh sandbox, at the same working path between runs. Entire output directories,
transcript, stdout, stderr and exit code are compared **as raw bytes**, with no normalization or sorting of their contents.
The validation pins `-XX:-UseCompressedOops` because the JDK logs a host-allocation-dependent compressed-pointer mode.

The CLI case starts the strict fake-adb server on one selected port, copies
`fixtures/fake-adb/android-two-devices`, and supplies its original APK/instrumentation data. Its run-local Marathonfile adds
`debug: false` and `threadingConfiguration.bootWaitingThreads: 1`. This documented input variant avoids unconstrained verbose
vendor logs and worker-index selection; the copied fixture is not edited. The provider's early DEBUG lines still appear
(Kotlin bug C18). The tape orders both connections and both prepares, then lets device 5554 execute the batch. Frozen clocks,
ID tapes and Logback hooks stabilize the complete report tree and console bytes. Expected outcomes are independently checked:
marathon exits 1; exactly one passed, failed and ignored result; fake-adb exits 0 in strict mode. Socket connection transcripts are retained per run and compared logically as described below. Connection IDs and independent
interleavings are the only transport differences excluded from the byte-repeatability claim.

**(c)** Fresh patched/unpatched pairs compare every report file. Core event transcripts retain exact assignment order,
virtual event times, outcomes and expectations. The reference `scripted` case uses `runAsync` because its `runBlocking` has no
hook; the sole injected `runBlocking` transcript entry and hook diagnostics are removed for this comparison. The following
are the **only** comparison substitutions (implemented in `compare_replay.py`):

| Difference | Justification / retained checks |
|---|---|
| Batch/Allure UUIDs and filenames | Per-site allocated values; consistent IDs across files, no file deduplication. Allure IDs are paired by result contents |
| Object/array identity suffixes | Only named, typed configuration singleton slots in Allure environment XML (including the final Android local-parser slot), and the pinned CLI APK tests' `kotlin.Metadata` array members `bv`, `d1`, `d2`, `mv`. Exact class matches retain aliases and distinct allocations. Core metadata and all other user strings remain literal |
| Billing `start`, `end`, `duration` | Derived from hooked wall time; device/pool and all other fields remain |
| HTML generation footer | `HtmlSummaryReporter.date`; page data and assets otherwise remain |
| Core returned-batch timestamps | QueueActor synthesizes INCOMPLETE results from Timer. Absolute starts/ends are paired by raw result; status INCOMPLETE and the exact +1 ms duration are required. StubDevice counter timestamps and durations remain exact |
| CLI test timestamps/durations and derived HTML/timeline statistics, JUnit `time`/`timestamp` | Real device result accumulation reads the hooked Timer; fields are listed in the comparator. Test identity, statuses, metadata, failure text, counts and attachments remain |
| CLI runner/idle device identity and connection enumeration | Untaped vendor preparation may choose either otherwise identical fake device. Roles are mapped only at the typed device fields/filename positions listed below; core replay assignments are never role-normalized |
| CLI timeline data ordering | Timeline sorts by start time; frozen timestamps turn its order into a tie. Compare this array by metric type/test name, retaining every entry. Sort measure-keyed timeline arrays and device-enumeration arrays otherwise |
| Console diagnostic clock/duration strings and classpath addition | Injected clocks and the added hook jar. All message text and multiplicities remain checked; stderr is exact |
| Console JVM stack-frame lines and log ordering | Source offsets and generated suspend-lambda frames necessarily change with instrumentation. Exclude frame lines but keep exception messages. Live vendor order is an input; reference logger records compare as a multiset, while plain output retains sequence. Taped-repeat console comparison in (b) remains byte-for-byte |

These are reference comparison rules, not a license to normalize new changes away. Any unlisted difference fails. In
particular, the comparator retains the timeline's incorrect failed count (C16), lazy-field JSON leakage in the Kotlin
reference (C17/D3 for the eventual port), source fixture results, and DEX metadata identity-string shape (C19).

The hooks do not fix Kotlin execution behavior. Untaped unit tests and paired scenarios validate that assertion; future
vendor scenarios must add evidence before their replay output is treated as authoritative. Results and incidental findings
are recorded in [`../wp/0.2b-findings.md`](../wp/0.2b-findings.md).

## Round 1 coverage and limits

The search additionally includes injected `clock()` / `clock.millis()` calls, unqualified `send`, `withContext`, HTTP
connect/read/write timeouts and batching, `System.getenv`, `Mutex.withLock` and `Semaphore.withPermit`. Each is classified
in the CSV, including deliberately live calls. Environment reads remain uncontrolled: the validation snapshots its inherited
environment and pins HOME, timezone and locale for each run. Remote analytics deadlines/batching and vendor permit admission
remain live and are outside the covered core/Android variant.

Check (b) writes an aggregate `build/validation/tape-coverage.json` and the same per-label call counts in `results.json`.
Counts come from actual active replay hook reports; labels absent from this report have only inactive-path coverage under
(a), or remain outside the exercised source paths. `random-order` adds explicit committed draw/read entries, consumed without
unused entries. The queue contains repeated copies of one test, so the reference's live random key cannot alter assignments.
Check (c) also runs an independent **no-tape patched/reference CLI pair**, using the same scoped report and console comparison,
to exercise the Logback encoder's inactive path.

The CLI variant is `debug: false`, one boot-waiting thread, frozen clocks and live completion barriers. It is an acceptable
first golden input; it does not prove scheduling, timeout or log parity. WP 3.10 must record this variant in every derived
golden's provenance. Debug paths (C10, C18) and concurrent boot waiting remain uncovered until later scenarios. CLI golden
durations assert **stability only**, not correctness.

Reference normalization validates clock-field presence and numeric types before replacement and visits explicit report
paths only; metadata and unknown/user fields remain intact. The three sorted arrays are timeline metric data (metric type /
test name), timeline `measures` (measure key), and HTML device enumeration (serial key). Other array order remains exact.

Every CLI repetition retains `adb.jsonl` in its own `b-cli-run-NN` capture; reference and no-tape pairs each retain their own
transcripts too. `adb-logical.json` groups records into connections, preserving all fields and each connection's record order,
then preserves connection start order per device and host. Logical comparison removes only connection IDs and interleaving
between independent devices / late completion records of established streams. Check (c) also maps transport selectors and
serial fields to runner/idle roles; shell arguments and payload fields are never rewritten. Missing requests, changed
arguments/payload digests and changed per-device transaction ordering fail. Fake-adb records exact payload sizes and SHA-256 digests for streamed install stdin and sync writes; the comparator requires
that evidence for nonempty uploads. APK bytes are therefore checked as well as install arguments. Raw transport bytes are retained, while (b)'s exact byte claim applies
to report trees, event transcripts, console, exit code and the logical ADB capture, excluding raw connection interleaving.

## Round 2 validation constraints

Console identity-shaped strings are literal; they receive no identity substitution. Report substitution never scans whole
JSON/XML files for identity patterns. The CLI metadata exception above is specific to the three test methods in the pinned
APK used by this validation; it does not authorize identity normalization for arbitrary annotations or future scenarios.

ADB comparison retains two views: each connection's ordered records and each device's ordered request/sync-operation stream
across all connections. Opening multiple transports in advance cannot hide a reversed install/instrument request order.
Independent devices may interleave, and completion records on separate connections may interleave; connection-local record
order, request arguments, payload sizes and digests remain exact.

Check (a) removes existing unit-test XML results and uses Gradle `--rerun-tasks --no-build-cache`. Its reported counts therefore
come from tests executed in that validation invocation, including when the build distribution was already up to date.

## Round 3 normalization audit (complete comparator inventory)

UUID and device-role replacement is now structural. No whole-file or whole-console substitution remains. The following
list covers **every remaining transformation** in `compare_replay.py`, including serialization and grouping. These rules
apply only to fresh patched/reference pairs unless explicitly stated. Ordinary user strings (annotations, configuration
values, test names, exception text, shell arguments and payloads) remain literal even when they equal an allocated ID,
serial, JVM-looking identity, clock or duration.

| Surface / operation | Exact scope and reason |
|---|---|
| Batch allocation / references | Allocate `batch-NNNN` in transcript batch encounter order (core), or sorted per-test report order (CLI). Replace only per-test JSON `testBatchId`, transcript log entries' `batch`, and the UUID argument of the generated `DevicePool[...]_DeviceActor[...] - Returning batch <id>. Reason:` console message. The remainder of that message stays literal. UUID allocation is injected; aliases and reference order remain checked |
| Allure allocation / filename | Sort result contents with only top-level `uuid` removed to pair allocations, then replace that top-level `uuid` and the basename before `-result.json`. Other filename components and every other value are literal. Normalized filename collisions fail, including binary files; files are never deduplicated |
| CLI device roles in JSON | Select the sole raw-result runner, map the other fixture device to idle. Replace raw rows' `deviceSerial`; `serialNumber` and string-valued `safeSerialNumber$delegate._value` on device reports and per-test/billing `device` objects; and Allure labels named `host`. This permits either identical fake device to run; no recursive string replacement occurs |
| CLI roles in HTML data | On generated assignment lines only: `mainData.pools[]` / `pool` test `deviceId` and device `serial`; `test.deviceId`; `logs.device_id`; timeline `dataset.measures[].measure`. Test names, metadata, stacktraces, titles, paths and other fields remain literal |
| CLI device filename positions | `devices/<pool>/<serial>.json`, `bill/<pool>/<serial>.json`; the serial directory in `test_result`, `logs`, `screenshot`; the serial directory in `html/pools/<pool>/<serial>/...`. Pool names and test/attachment basenames stay literal |
| CLI console roles | Only the serial bracket of the generated `<percent>% \| [pool]-[serial] <test> <status>` progress line. No substitution in test names, failures or other messages |
| JVM identities | Exact classes in the named `CONFIG_IDENTITIES` environment XML slots; Android configuration's final typed local-parser slot; only the three pinned APK methods' raw `kotlin.Metadata` array members `bv`, `d1`, `d2`, `mv`. Encounter-numbered identities preserve aliases; these values are JVM object/array renderings. Other annotations and config slots are literal |
| Billing clock | Top-level numeric `start`, `end`, `duration` in billing reports. Require all fields and types; billing is derived from the injected wall clock |
| Core returned-result clock | Raw INCOMPLETE results with absolute timestamps must have duration exactly 1. Pair their starts and +1 ends by raw encounter order, replacing only raw `timestamp`, per-test `startTime`/`endTime`, Allure `start`/`stop`, and timeline row `startDate`/`endDate` when equal to those values. Ordinary StubDevice times remain exact |
| CLI result clocks | Numeric raw `timestamp`/`duration`; per-test `startTime`/`endTime`; Allure `start`/`stop`. These come from the hooked Timer; require fields and numeric types |
| CLI HTML clocks | Numeric main summary `total_duration_millis`, `average_duration_millis`, `max_duration_millis`, `min_duration_millis`; pool/test `duration_millis`; timeline execution/measure stats `idleTimeMillis`, `averageTestExecutionTimeMillis`, data `startDate`/`endDate`. Validate required shapes/types. These derive from the result/Timer clocks |
| CLI JUnit clocks | Only `testsuite` attributes `time`, `timestamp` and `testcase.time` under `tests/*.xml`. Require presence and numeric time / timestamp shape. Other attributes and element text remain |
| HTML generation clock | Only the complete generated `div class="copy content"` footer line, with its fixed generator prefix, in `html/*.html`. Replace the date between `by Juno at` and the closing div. Other occurrences of those words stay literal |
| CLI array order | Sort pool device enumeration by `serial`; timeline measures by `measure`; timeline data by `(metricType, testName)`. These are device enumeration or frozen-clock ties. Preserve every entry, duplicates and non-clock fields; other arrays retain order |
| JSON/XML serialization | Re-encode known report JSON (`test_result`, `devices`, `bill`, Allure result files) and whole-line generated HTML assignments for `window.mainData`, `window.pool`, `window.test`, `window.logs`, `var dataset` after typed edits. JSON whitespace/escape spelling is immaterial for these parsed reports; field order and array order otherwise remain. Re-encode environment XML and CLI JUnit XML after typed edits, allowing XML spelling differences. Transcript object keys are sorted. Unknown files, including arbitrary JSON attachments, remain byte-exact |
| Transcript instrumentation | Remove top-level `hooks`, `hooksReport`, and only the exact log event `{"t":0,"runBlocking":"core.Marathon.run"}`. These are injected diagnostics / the bridge marker; any additional fields or changed event remain visible. Retain all other event order and data |
| Console log clocks | Replace only the leading Logback/Android timestamp before the thread bracket, never message clocks. Clock source is injected |
| Console duration diagnostics | Only complete `Timings` messages for device-provider initialization, parsed test count, prepared shard count and scheduling; Marathon's generated `Parsing took <n> ms`; and the complete numeric `Total time` summary. Counts and other text stay exact. Durations derive from injected clocks; arbitrary `ms`, `Execution time:` and user clock strings are literal |
| Console classpath | Only Marathon's main-thread System Information `java.class.path` record: remove the injected `oracle-hooks.jar`, compare sorted jar basenames. Separate distributions have different install roots and classpath enumeration; jar multiplicities/names remain. User text containing `java.class.path:` is literal |
| Console frames / ordering | Remove only complete indented JVM `at method(location)` and `... N common frames omitted` lines (instrumentation changes source offsets and synthetic frames). Generated logger-envelope records compare as a multiset, preserving multiplicities because live vendor threads interleave. Plain output and exception continuations retain sequence; stderr is byte-exact. Thus reference console comparison does not certify ordering between logger records; check (b) still compares all console bytes exactly |
| ADB connections / cross-device interleaving | Remove socket `conn` IDs, group each connection's exact ordered records under its device, retain connection encounter order per device and global unconnected events. Independent devices and separate completion records may interleave. Retain a second per-device stream containing outer requests **and every non-QUIT sync operation** across connections, preserving dependencies of conflicting writes/reads even on preopened sync transports. QUIT only closes its own stream and remains in the connection-local view. This stronger order is checked in both (b) and (c) |
| ADB roles (reference pairs only) | Resolve role serials from raw/device reports' final serial component, replace only transport selector operands and the typed `serial` field, and group by role. Shell arguments, sync paths, operation fields, upload sizes/SHA-256 digests and all other data remain literal |

`exact()` and repeated report/console comparison in (b) normalize nothing. The logical ADB capture uses only the last two
rows above, without roles for (b). Unknown ADB records stay in the connection-local view; warnings and missing nonempty
stdin/SEND/SND2 payload digests fail. The live conflicting-write regression acknowledges each SEND, verifies final bytes
`B` versus `A`, and requires `compare_adb` to reject the reversed order.
