# WP x56: plain diagnostic text

Base: main `584462eb`. Original implementation: `75977f02`; original audit: `0ebf2d76`. Approved
2026-11-11 under PLAN §3.12.5. The owner's follow-up explicitly limits
cancellation changes to diagnostics: generated failure data and its existing
assertions retain their bytes. No D-row is added.

## Sites changed

Paths below are relative to `crates/`. All levels were audited, including owned
log records and service-log closures that eventually become tracing events.

| Site | Before → after |
|---|---|
| `marathon-core/src/marathon.rs`, `Marathon::run_under` error mapping | `kotlin.Unit` → `Marathon run failed`; original error field retained |
| `marathon-cli/src/application.rs`, failed `marathon.run` | `kotlin.Unit` → `Marathon execution crashed`; original error information retained |
| `marathon-cli/src/application.rs` startup and `args.rs::CliConfiguration` | `CliConfiguration: MarathonRunCommandCliConfiguration(marathonfile=…, analyticsTracking=true, bugsnagReporting=true)` → `command: run marathonfile=… analyticsTracking=true bugsnagReporting=true`; parse lists its file/output/flaky fields; version says `command: version` |
| `marathon-apple/src/apple_device_test_runner.rs`, cancellation catch | ERROR `Tests […] got stuck… error=job cancelled: LazyDeferredCoroutine was cancelled` → DEBUG `Batch cancelled: job cancelled: task was cancelled`; typed timeout retains the original ERROR and timeout advice |
| `marathon-apple/src/listener/{mod,composite,debug}.rs`, cancellation callback | INFO `testRunFailed … errorMessage = Tests […] got stuck…` → DEBUG `testRunCancelled … reason = job cancelled: task was cancelled`; the default callback still delivers the unchanged message to result/artifact listeners, in the same order |
| `marathon-apple/src/xctest_parser.rs`, cancelled collection | ERROR `Test parsing got stuck…` → DEBUG `Test parsing cancelled: …`; typed timeout retains the original ERROR; swallowed cancellation/partial discovery behavior is unchanged |
| `marathon-core/src/error.rs::Failure::for_log` and `logging.rs::LogRecord::emit` | Known generated job messages and the exact generated iOS/macOS device-loss wrappers render without coroutine classes, recursively through causes; ordinary Display, external frames and stored failures are unchanged |
| `marathon-core/src/execution/device/actor.rs`, prepare error message | `Error DeferredCoroutine was cancelled` → `Error task was cancelled`; only the diagnostic's interpolated message changes |
| `marathon-core/src/execution/device/state.rs`, invalid-transition diagnostics | `DeviceState.Running(batch=…) event DeviceEvent.Execute(batch=…)` → `running batch=… event execute batch=…`; other state/event names become plain lower-case values |
| `marathon-core/src/execution/pool.rs::pool` | `DevicePoolId(name=omni)` → `omni` in all 16 pool-message uses (queue termination, queue-closed sends, add/remove/terminate, completion/return/reject, readiness and no-active-device handling) |
| `marathon-core/src/execution/queue.rs::spawn`, logger metadata | `QueueActor[DevicePoolId(name=omni)]` → `QueueActor[omni]` |
| `marathon-core/src/execution/pool.rs::diagnostic` | Rust map Debug `states={"preparing": 126}` → `states=[preparing=126]` |
| `marathon-core/src/executor/completion_logging.rs` | `CoroutineContext finished/cancelled/closing due to unrecoverable error` → `Task finished/cancelled/stopped due to unrecoverable error` |
| `marathon-android/src/provider.rs`, monitor completion and device-setup completion; `device.rs`, logcat completion | `CoroutineContext closing due to unrecoverable error` → `Task stopped due to unrecoverable error` at all three production call sites |
| `marathon-core/src/analytics/provider.rs`, execution-time history | Rust tuple Debug rows → `Execution time history: [test.name=7000 ms, …]` |
| `marathon-ios/src/device/simulator_provider.rs`, dynamic plan | `Using dynamic device provider at Dynamic(host=…, port=…)` → `Using dynamic device provider at …:…` |
| `marathon-analytics/src/influx2.rs`, bucket creation | `retention: RetentionPolicyConfiguration(everySeconds=…, shardGroupDurationSeconds=…)` → `retention: everySeconds=… shardGroupDurationSeconds=…` |
| `marathon-android/src/runner.rs`, selected tests | Rust vector Debug → comma-separated test names in brackets |
| `marathon-android/src/listeners.rs`, debug event identifier and target | `TestIdentifier(className=pkg.Tests, testName=testOne)` → `pkg.Tests#testOne`; wrong `marathon_apple::listener::debug` target → `marathon_android::listeners` for every Android debug event |
| `marathon-android/src/artifact_listeners/file_sync.rs`, missing app package | `Unable to pull FileSyncEntry(relativePath=…, pathRoot=APP_DATA, aggregationMode=…)` → `Unable to pull … from APP_DATA (aggregation …)` |
| `marathon-android/src/artifact_listeners/profiling.rs`, perfetto completion | `ShellCommandResult(output=…, exitCode=…)` → `output=… exitCode=…`; external command output bytes are not rewritten |
| `marathon-android/src/provider.rs`, track-devices outage warning | Empty-header EOF formerly attached `ADB request rejected: Unexpected string length: ` and an extra blank line. It now says `Lost device tracking on adb server …: the adb server closed the connection; reconnecting`, without an attached throwable. The projection matches only that typed empty-header failure; other failures keep their cause and message |
| `marathon-core/src/error.rs::Failure::for_log`; `marathon-apple/src/cmd/remote/ssh/sshj/{error,config}.rs`, SSH promise/transport diagnostics | Generated `Timeout expired: 30000 MILLISECONDS` → `no response after 30 s`; the 1 ns polling timeout similarly uses `1 ns`. Typed SSH inherited messages and generated device-loss wrappers project the same wording; stored failures retain the TimeUnit spelling. Promise logs retain SSH reason codes and channel-opening context |
| `marathon-apple/src/device/connection_factory.rs`, log-only transport renderer; iOS `device/{simulator_provider,simulator_provisioner,provisioning_plan,detached}.rs` and `simulator_provider/recovery.rs`; macOS `provider.rs` | Connection/update/provisioning/recovery diagnostics now resolve authentication with the same transport-before-vendor precedence as the real connector. `authentication=none` with vendor key auth becomes `authentication=public key (user=…, key=…)`. Passwords remain omitted. Review S-x56-1 added the previously missed planner diagnostic using the vendor default auth with transport overrides; trait wrappers forward the other diagnostic lookups; transport keys, reachability-failure data, credentials and connection inputs are untouched |

Expected batch/discovery cancellation uses DEBUG, consistent with task-completion
logging. D52's signal notice remains ERROR so CI sees it with debug off. Actual
timeouts retain ERROR, `got stuck` and the timeout-setting advice. The filter,
progress/summary formatters, exit codes and worker commands are unchanged.

The sweep leaves config/annotation/data renderers, DEX error data, recorded input
class names, safe Debug implementations that are not used by log call sites, and
opaque external tool/error/stack text alone. No replacements with Rust struct or
tuple Debug dumps were introduced. C34 was checked at both implementations before
its ledger row was retired; the renamed CLI witness keeps the original error,
stream and exit assertions. The x54 findings now explain the default panic hook's
stderr output and record closure of the old base/changelog/range notes.

## Result text left unchanged

If the Apple run-failure message below becomes `TestResult.stacktrace`, it reaches
JUnit `<failure>` CDATA (no `message` attribute), Allure `statusDetails.message`
(first line) and `.trace` (whole string), per-test
`test_result/<pool>/<device>/<test>.json` `stacktrace`, and HTML's embedded
`window.test.stacktrace` and rendered trace. `test_result/raw.json` has no
failure/stacktrace field. Existing assertion/xcresult traces remain authoritative.

| Exact retained value | Situation and report reach |
|---|---|
| `Tests [{names}] got stuck. You can increase the timeout in settings if it's too strict` | Requested Apple cancellation **and** typed timeout still call result listeners with this string. `{names}` joins ordered `package.Class#method` names with `, `. A test with no temporal result receives it through `create_uncompleted_test_results` and can reach the trace-bearing fields above if that result is reported. Started-but-unfinished temporal results do not acquire this message automatically. Retries/final-result selection can supersede an attempt. The unchanged `cancellation_branch_keeps_the_exact_unquoted_message_in_missing_test_results` pins the data. |
| Failure messages `LazyDeferredCoroutine was cancelled` and `DeferredCoroutine was cancelled`; Display prefixes each with `job cancelled: ` | The first cancels retained iOS/macOS batch jobs (including disposal/D52); the second cancels core initialize/execute jobs and retained iOS preparation. These remain typed failure-chain data. Preparation has no test result; execution cancellation terminates the actor. Returned-batch synthesis uses `stacktrace: None`, so these values reach none of JUnit, Allure statusDetails, raw.json, per-test JSON or HTML through these paths. Apple listeners use the separate stuck message above. |
| macOS device-loss message `job cancelled: LazyDeferredCoroutine was cancelled; job=LazyDeferredCoroutine{Cancelled}`, with cause message `LazyDeferredCoroutine was cancelled`; iOS wrapper message `job cancelled: LazyDeferredCoroutine was cancelled` | A cancelled batch whose caller is not cancelled can become device loss, including disposal. The macOS chain remains pinned by `dropping_a_disposal_waiter_does_not_abandon_the_shared_completion`. Core logs DeviceLost, terminates the actor and returns the batch with no stacktrace; no listed report receives this chain. Results already completed by listeners keep their own traces. |
| SSH timeout message `Timeout expired: 30000 MILLISECONDS`, or polling message `Timeout expired: 1 NANOSECONDS`, including inherited SSH messages and nested causes | Authentication/key exchange/channel waits retain these values in returned failures and ordinary Display; SSH data assertions remain unchanged. Setup happens before reporting; execution connection-loss and returned-batch paths do not copy these chains into test traces; remote-file-manager commands can log and swallow them. These values also remain in suppressed-frame data and the actor-generated returned-batch reason strings, whose log copies now use the projection. No production path found puts these generated values in the listed reports. External runner text with the same spelling stays opaque and could independently reach trace-bearing fields. |

D52 stops scheduler/run actors before reporting: the in-flight batch cannot
publish late results through its closed queue. The retained 0.13.0-rc.1 local
Apple evidence confirms this: iOS SIGTERM/SIGINT cases 08/09 each contain seven
raw entries, seven JUnit cases, seven Allure results and seven per-test JSON
files; the eighth test is absent. Those reports and HTML contain none of the
stuck/coroutine wording, although both old log lines appear. The two macOS signal
runs have empty raw arrays and no JUnit cases or Allure results. This does not
remove timeout results already accounted for before a signal.

The retained remote-iOS timeout run contains `30000 MILLISECONDS` in its log but
zero matches in its raw file, JUnit file, four Allure results, two per-test JSON
files or HTML pages. These are inspections of retained real-run evidence, not
fresh device runs. The Android addendum's underlying ADB failure data also stays
unchanged.

A later owner decision could extend D59 with a D-row for **plain cancellation text
in generated failure messages**: distinguish requested cancel from real timeout
in stored Apple run failures, and replace generated coroutine/SSH TimeUnit text
in failure chains. Any affected report expectations and goldens would then need
review. x56 makes none of those data changes.

## Tests and verification

Regression coverage includes both run-failure log sites, cancelled batch DEBUG
versus typed-timeout ERROR, composite/debug-listener dispatch, stored failure
immutability, rendered cancellation causes, external class-looking text, Android
listener target/test-name rendering, the real track-devices EOF warning, SSH
plain timeout units and effective authentication precedence/password masking.
The original Apple missing-result text and macOS cancellation-chain tests keep
their original data assertions.

Temporary reverts made both real concurrency witnesses fail with exit 101:
`a_cancelled_batch_doesn_t_lose_the_simulator` rejected the restored stuck log,
and `track_eof_resumes_tracking_and_keeps_a_listed_device_d22` rejected the old
EOF warning. Sources were restored before positive verification. The cancellation
witness uses the existing paused-clock/observable-start/finished-cleanup boundary;
the EOF witness counts logs only after reconnection has been observed. Neither
adds timing assumptions or a new stress loop.

Changed integration-test/support files (paths relative to the repository):

- `crates/marathon-analytics/tests/clients.rs`
- `crates/marathon-android/tests/android_runner.rs`
- `crates/marathon-android/tests/provider_test.rs`
- `crates/marathon-apple/tests/apple_device_test_runner.rs`
- `crates/marathon-apple/tests/connection_factory.rs`
- `crates/marathon-apple/tests/ssh_session.rs`
- `crates/marathon-cli/tests/cli_matrix.rs`
- `crates/marathon-cli/tests/run_outcome.rs`
- `crates/marathon-cli/tests/tracing_output.rs`
- `crates/marathon-core/tests/analytics_factory.rs`
- `crates/marathon-core/tests/analytics_oracle.rs`
- `crates/marathon-core/tests/device_actor.rs`
- `crates/marathon-core/tests/device_actor_oracle.rs`
- `crates/marathon-core/tests/live_output.rs`
- `crates/marathon-core/tests/pool_behaviour.rs`
- `crates/marathon-core/tests/pool_scheduler.rs`
- `crates/marathon-core/tests/queue_oracle.rs`
- `crates/marathon-core/tests/run_flow_witnesses.rs`
- `crates/marathon-core/tests/runtime_behavior.rs`
- `crates/marathon-core/tests/runtime_review.rs`
- `crates/marathon-core/tests/support/queue.rs`
- `crates/marathon-ios/tests/simulator_batch.rs`

`marathon-core/src/error.rs`, `marathon-cli/src/args.rs` and
`marathon-android/src/provider.rs` also contain changed/new unit tests.
Historical JVM fixtures are unchanged. Their adapters project only the known
startup, pool, queue-handler, state/event or analytics **diagnostic** fields;
measurements, queue state, throwable data and result assertions remain exact.
No parity-matrix test is removed or marked n/a.

Original verification (implementation rebased as `75977f02`):

- `cargo fmt --check`: passed.
- `cargo clippy` for the seven changed crates, `--all-targets -- -D warnings`: passed.
- `cargo test` for `marathon-core`, `marathon-cli`, `marathon-apple`, `marathon-ios`,
  `marathon-macos`, `marathon-android`, `marathon-analytics` and dependent
  `marathon-test-support`, with `--no-fail-fast`: exit 0; 2,195 passed, 0 failed,
  33 existing ignored tests across 214 test/doc-test targets.
- `python3 -I tools/differential/make_registry.py --check`: passed.
- `python3 -I tools/differential/test_evidence.py`: 107 passed.
- `tools/differential/run native-vendors`: all 16 captures passed against the
  original goldens.
- Documented `run bless`: all 18 views and 1,141 entries unchanged; `diff-golden
  --against 584462eb` shows only capture provenance. See
  [the golden audit](x56-golden-diff.md).

Builds used `CARGO_BUILD_JOBS=6`, `CARGO_INCREMENTAL=0` and
`CARGO_PROFILE_DEV_DEBUG=0`. The workspace/remote gates were not run, as directed;
no fresh real-device, remote-worker or Linux check was performed. The owner rebased the branch onto main `584462eb` after x54 merged.
The rebase changed no tree: `a284d0d9` became `75977f02`, and `effc3453`
became `0ebf2d76` (verified by comparing both old/new trees).

The initial broad run found six test targets with old diagnostic expectations;
focused corrections/reruns preserve all their data comparisons. An initial Python
self-test run lacked its YAML helper; after the documented helper build, all 107
self-tests passed. One early Rust invocation was superseded while implementation
was still changing; the final run uses the restored, completed source.

## Incidental bugs

No neighboring behavior fix is included. Cancellation/result wording above is
explicitly deferred to an owner decision. XCTest discovery still swallows
cancellation and registers partial tests (A59); only its diagnostic wording and
level change here.

## Review round 1

- **S-x56-1:** `PlanCalculator` now receives the vendor's default authentication
  and calls `transport_log_text` for the missing-udid ERROR. Its regression checks
  inherited key auth, a transport password override, password masking, unchanged
  transport and unchanged empty planning result. The site table above now includes
  `provisioning_plan.rs`; the original implementation had missed this site.
- **S-x56-2:** the accumulator's logger is `marathon_core::execution::result`;
  Influx 2 provider/tracker registration and events use
  `marathon_analytics::influx2`; xcresult folder materialization uses the simple
  `FileManager` component. The production component-name sweep found no other
  `com.malinskiy`/`kotlin.`/`java.` logger names. Remaining occurrences are source
  headers, protocol/metric keys, metadata, recorded data, failure data or tests;
  the ADB logger constants already use `marathon_adb::client::log`. Simple
  components such as `ScreenRecorder` remain supported by the same emitter.
- **S-x56-3:** `Failure::for_log` now projects known generated headers in copied
  `stacktrace` fields, including nested Suppressed/Caused-by headers. The header
  recognizer uses the same generated message mapping as typed failures; unknown
  messages, external frame text and explicitly External failure stacks stay
  verbatim. External failure stacks have no built-in production producer and
  represent supplied tool data, rather than the generated suppressed headers.
  SSH timeout producers use Connection/Transport/Authentication chainers,
  all recognized here. The original `flatten_suppressed` data stays unchanged.
  Actor and queue return-reason log sites share the same projection, restricted
  to the two actor-generated failure envelopes; their transmitted/stored reason
  string is preserved. Unit coverage exercises stacktrace-field projection and
  opaque text; actor coverage asserts the original returned reason byte-for-byte
  for both envelopes, while actor and queue logs reject coroutine/TimeUnit text.
- **S-x56-4:** base/source/audit pointers reflect the owner rebase above, which
  changed commit identities but no tree. No rebase remains pending.

Review fixes: `a75a5780` on the rebased `0ebf2d76` head.

- `cargo fmt --check`: passed.
- Clippy for the seven affected crates, all targets, `-D warnings`: passed.
- Tests for the same eight changed/dependent packages listed above: exit 0;
  2,199 passed, 0 failed, 33 existing ignored tests (214 test/doc-test targets).
  All four new regressions passed.
- Differential self-tests: 107 passed, with the documented YAML helper built first.
- Registry generation check: passed without changes.

No result data, exit policy or worker command was changed. The refreshed golden
comparison is summarized in [the golden audit](x56-golden-diff.md). Workspace and
remote gates remain the orchestrator's checks; they were not run here.
