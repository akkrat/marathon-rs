# WP x57 — Explain an Apple batch with no results

Approved 2026-11-11 for 0.13.0 (first shipped in 0.13.0-rc.2), under
PLAN §3.12.5. Diagnostics only: no D-row, result-file update, worker command,
exit-policy change, configuration field or pinned progress/summary change.

## Emission and outcome decision

`AppleDeviceTestRunner::warn_empty_batch` emits one WARN on
`marathon_apple::batch_outcome`, after result listeners and session stop.
Both the iOS simulator and macOS device register their final result deferred
and their caller's cancellation signal with the shared runner. Finished or
failed results for a test in this batch suppress the warning (ignored tests are
finished results); output for unrelated tests cannot hide an empty batch.
If collection itself failed before publishing results, a completed test event
provides the fallback. A started test alone does not count. An empty batch or
a `check_closed` rejection before `produce` does not warn. A command-open failure
inside `produce` does warn, saying `xcodebuild could not be started` with its
known reason; it does not claim that Marathon ended a running command.

`XcodebuildSession::produce` records elapsed vendor-clock milliseconds from
launch to output completion or forced stop, before command cleanup. This is
session duration, including command acquisition and output draining, rather
than a remote process CPU/runtime measurement. Artifact collection can overlap
a still-running session; a slow final close does not inflate the duration.
The line renders seconds: one decimal below 10 s, whole seconds (truncated) at
10 s and above. For a command-open failure the elapsed value measures the launch
attempt, not a running xcodebuild process.
The serial is the same `udid@worker` value used by other device diagnostics;
the SSH worker id includes its user and `host:port`.

Command termination has diagnostic-only metadata alongside `ExitCode`.
The local reaper retains the actual status or Unix signal number; SSH retains
the actual status or protocol signal name. Both retain the existing timeout
branch separately from a missing exit status. Numeric command results are
unchanged: for example, local SIGKILL still returns 137, SSH KILL/SEGV still
return no numeric status, and SSH TERM still uses its historical 143 fallback.
The existing `Finished test batch execution with exit status N` DEBUG call in
`execute_test_request.rs` is untouched.

An output timeout uses the existing typed timeout from the output consumer and
its reason; a timeout from launch or a later listener is not called an output
timeout. A typed
cancellation timeout remains a timeout and includes its known reason. SSH
connection/transport failures use their existing typed error and reason;
other early session failures retain their plain diagnostic reason. A known
command/batch timeout takes precedence over a failure during its subsequent
close. Multiline failure reasons are flattened for this one-line diagnostic;
returned and stored failures keep their original text.

## Example lines

These messages are asserted by the paused-clock simulator tests (the regular
subscriber adds its timestamp; compact output does not). Each case has two
incomplete tests and exactly one event on the new target. The output-timeout and
lost-connection rows use injected failures, not the real idle timer or a real
SSH connection drop:

```text
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30 s: xcodebuild exited with status 65
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30 s: xcodebuild exited with status 0
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30 s: xcodebuild was terminated by signal KILL
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30 s: xcodebuild ended without an exit status
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30 s: marathon ended the session: batch timeout
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30 s: marathon ended the session: test output timeout (timed out: idle timeout 30 s reached)
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30 s: marathon ended the session: lost connection to the worker (SSH connection failure: worker connection closed)
```

The runner's injected typed-timeout test also asserts the suffix
`marathon ended the session: timeout (operation cancelled after timeout: batch deadline reached)`.
The macOS test asserts one warning for its one missing test, status 0 and a
measured duration in seconds. Local process signal metadata is separately
asserted as signal `9`, so it is distinguishable from an actual exit status 137.
SSH signal metadata is checked against the fake SSH worker's KILL and SEGV
reports, without changing the numeric status or sending an extra signal.

## Cancellation and visibility

Ctrl-C, SIGTERM and D52 stop in-flight batches without test results. Emitting
warnings for those batches would obscure the actual cancellation notice.
The guard therefore checks requested cancellation on both the retained batch
context and its original caller, and on the returned run error. The caller
check covers the interval before cancellation reaches the retained job,
including cancellation during result/artifact collection. Typed timeouts
are not requested cancellation and continue to warn.

The target has a WARN floor in every configurator, including startup,
Android, Apple regular and Apple compact. With debug on it permits DEBUG,
like `known_hosts`. Android has no emitter for this Apple-only diagnostic.
The logging directive inventory and registry checks cover the floor, and the
CLI subscriber test renders a WARN with debug off in each configurator.

## Tests and verification

New or extended witnesses:

- `marathon-ios/tests/simulator_batch.rs`: statuses 65 and 0, missing status,
  signal, batch timeout, output timeout, reachable worker-connection failure,
  exact 30 s session duration, one warning, partial passed/failed/ignored batches,
  unrelated-test results that must not suppress the warning,
  and requested cancellation with no warning. The existing live local-process
  batch-timeout witness also checks the new reason.
- `marathon-apple/tests/apple_device_test_runner.rs`: caller cancellation during
  `after_test_run` with an uncancelled retained context, typed timeout, and a
  later listener timeout that must not replace the session-end reason.
- `marathon-macos/tests/batch.rs`: final all-incomplete results with debug off,
  and cancellation of a nonempty batch with no warning.
- `marathon-apple/tests/local_executor.rs`: real SIGKILL metadata and actual
  total-timeout metadata, keeping command-result assertions.
- `marathon-apple/tests/ssh_session.rs`: fake-worker KILL/SEGV metadata and the
  batch-timeout marker even if timeout cleanup fails, keeping prior assertions.
- `marathon-logging`: complete directive inventories and
  `batch_outcome_warns_in_every_configurator_with_debug_off`.
- `marathon-cli/tests/tracing_output.rs`: the rendered target and WARN line
  under every configurator and both debug settings.

Temporary reversions proved the concurrency witnesses: disabling the warning
made the simulator's all-incomplete witness fail; removing cancellation guards
made both the simulator cancellation witness and the caller-during-collection
witness fail. All three returned exit 101 at their intended assertions.
Sources were restored before final checks. No stress loop was used.

Verification used `CARGO_BUILD_JOBS=6`, `CARGO_INCREMENTAL=0` and
`CARGO_PROFILE_DEV_DEBUG=0`.

- Initial explicit package run (`--no-fail-fast`): `marathon-logging`,
  `marathon-apple`, `marathon-ios`, `marathon-macos`, `marathon-cli`,
  `marathon-core`, `marathon-android`, `marathon-analytics`, `marathon-adb`,
  `marathon-test-support`, `fake-adb`. Exit 101: 2,490 passed, 15 failed,
  34 ignored across 225 test/doc-test targets. All 15 failures in eight targets
  reported the missing YAML parser helper; no product assertion was changed.
- Built the documented prerequisite:
  `cargo build -q -p snakeyaml --example yaml_tree` (passed).
- Reran the six failed core targets (`marathon_scenarios`, `parse_command`,
  `reporters`, `run_flow_oracle`, `run_flow_witnesses`, `yaml_output`):
  73 passed, 0 failed. The two failed CLI targets are included in the final
  package rerun below.
- Final package rerun (`--no-fail-fast`): `marathon-logging`, `marathon-apple`,
  `marathon-ios`, `marathon-macos`, `marathon-cli`, `marathon-test-support`.
  Exit 0: 1,091 passed, 0 failed, 33 existing ignored tests across 130
  test/doc-test targets. This includes fresh captures of all 16 native vendor
  cases against unchanged goldens, configuration evidence replay and every x57
  integration witness.
- Final runner-target check after setting the new real-time cleanup hang catchers
  to 60 s: 18 passed, 0 failed.
- `cargo fmt --check`: passed.
- Clippy for the five changed crates, `--all-targets -- -D warnings`: passed.
- `python3 -I tools/differential/make_registry.py --check`: passed unchanged.
- `python3 -I tools/differential/test_evidence.py`: 107 passed.
- Local repository check: clean.

The final Apple/CLI rerun covers the final result-membership and cleanup-reason
refinements as well as every new warning/cancellation witness.

## Limits and incidental findings

No neighboring behavior fix is included. No real Apple device/farm, live remote
worker, Linux build, new JVM recording or golden regeneration was run. The CLI
crate suite runs fresh native vendor captures against the unchanged goldens and
replays committed configuration evidence.
Historical fixtures and parity rows are unchanged. Workspace and remote gates
are reserved for the orchestrator by the implementation brief; neither is run
here. Only changed crates and their production dependents were tested in the
original implementation; this review run tests only the four changed crates.

## Review fixes (S-x57-1–4)

- **S-x57-1:** observations carry whether a command was opened. Missing xcrun,
  invalid workdir and SSH exec failure before launch now say `xcodebuild could
  not be started`, retaining the original returned error. The runner test injects
  these three open failures inside `produce`, checks exactly one warning and
  confirms that no command-close callback ran. The pre-produce `check_closed`
  rejection remains distinct.
- **S-x57-2:** elapsed time uses one decimal below 10 s and whole seconds from
  10 s onward. Paused-clock batch tests assert `0.0 s`, `2.4 s`, `9.9 s`, `10 s`
  and `1800 s`. The existing hours/minutes/seconds helper is pinned to summary
  output and does not provide this format, so that formatter is untouched.
  `Failure::for_log` now projects the exact canonical generated idle-timeout
  message to plain seconds, retaining nanosecond precision for the configured
  timeout reason. Typed data, ordinary Display and stack traces remain unchanged.
  Tests cover fractional, negative, zero and large durations; malformed,
  noncanonical, prefixed and External messages stay opaque. Cause chains, SSH
  inherited messages and generated suppressed/cause headers use the same mapping.
- **S-x57-3:** the partial-batch witness now includes a skipped test. Output-timeout
  and worker-connection warnings are explicitly documented as injected-failure
  witnesses. The batch test also asserts the original timeout/connection message
  remains in the returned failure chain.
- **S-x57-4:** the logging crate-mapping row now names the batch-outcome WARN floor
  and states that it has no JVM counterpart.

Additional examples asserted by tests (compact form):

```text
WARN marathon_apple::batch_outcome: Apple test batch on udid returned no results for 1 test after 0.0 s: xcodebuild could not be started: no such file or directory: xcrun
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 2.4 s: xcodebuild exited with status 65
```

### Other diagnostic sites reached by the idle-timeout projection

The rewrite is restricted to a typed Timeout/SSH failure whose complete message
matches the generated `idle timeout <canonical Duration> reached` form. The
production generator remains `marathon-apple/src/extensions/reader.rs`; it is
unchanged. Apart from the new WARN, the same failure can be rendered by these
existing diagnostic consumers (paths relative to `crates/`):

- `marathon-core/src/logging.rs::LogRecord::emit`: causes on queued vendor/core
  records, including device-actor execution, cancellation and critical-error
  diagnostics, task-completion logs and vendor disposal/setup error logs.
- `marathon-core/src/execution/device/actor.rs`: the prepare-error message and
  generated returned-batch diagnostic envelopes; `execution/queue.rs` renders
  those same envelopes. Their transmitted reason strings stay unchanged.
- `marathon-apple/src/apple_device_test_runner.rs` and `xctest_parser.rs`:
  explicit cancellation/timeout diagnostic projections when the failure is
  nested in their cancellation cause.
- `marathon-apple/src/cmd/remote/ssh/sshj/{error,config}.rs`: inherited SSH failure
  messages, transport `Dying because` and promise `woke to` diagnostics if the
  same typed generated failure reaches them.
- `marathon-cli/src/application.rs`: uncaught run-failure diagnostic rendering.

These are conditional consumers, not new emissions: only a matching generated
idle-timeout message changes. External frame/tool text stays unchanged. Core
unit tests verify both original failure equality and the ordinary stored trace;
the simulator witness verifies the returned original ISO message.

Review verification (same six-job, nonincremental, dev-debug-off build settings):

- `cargo test -p marathon-core -p marathon-apple -p marathon-ios -p marathon-macos
  --no-fail-fast`: exit 0; 1,778 passed, 0 failed, 26 existing ignored tests
  across 167 test/doc-test targets. Only the four changed crates were selected.
- Focused core diagnostic tests: 7 passed. Focused runner/simulator/macOS batch
  targets: 19 + 22 + 17 passed.
- Temporarily disabling the new command-open distinction made the open-failure
  regression fail at its intended warning assertion (exit 101). Restoring the
  exact source bytes and rerunning the runner target: 19 passed. No real-time
  timing assumptions were introduced; the new async tests use a paused clock.
- Clippy for the four changed crates, `--all-targets -- -D warnings`: passed.
- `cargo fmt --check`: passed.
- `tools/differential/run native-vendors`: all 16 complete vendor captures
  passed against unchanged goldens. No result-file, command or pinned
  progress/summary change was found. Stored timeout text remains ISO-8601.
- `python3 -I tools/differential/make_registry.py --check`: passed unchanged.
- Local repository check: clean.

No workspace/remote gate, unchanged-crate test suite, real Apple-device/farm or
Linux check was run in this review pass. No fixtures, parity rows, D-rows or
configuration fields changed.
