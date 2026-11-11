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
provides the fallback. A started test alone does not count. An empty batch
or a request that never starts a session does not warn.

`XcodebuildSession::produce` records elapsed vendor-clock milliseconds from
launch to output completion or forced stop, before command cleanup. This is
session duration, including command acquisition and output draining, rather
than a remote process CPU/runtime measurement. Artifact collection can overlap
a still-running session; a slow final close does not inflate the duration.
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
incomplete tests and exactly one event on the new target:

```text
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30000 ms: xcodebuild exited with status 65
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30000 ms: xcodebuild exited with status 0
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30000 ms: xcodebuild was terminated by signal KILL
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30000 ms: xcodebuild ended without an exit status
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30000 ms: marathon ended the session: batch timeout
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30000 ms: marathon ended the session: test output timeout (timed out: idle timeout PT30S reached)
WARN marathon_apple::batch_outcome: Apple test batch on a@local returned no results for 2 tests after 30000 ms: marathon ended the session: lost connection to the worker (SSH connection failure: worker connection closed)
```

The runner's injected typed-timeout test also asserts the suffix
`marathon ended the session: timeout (operation cancelled after timeout: batch deadline reached)`.
The macOS test asserts one warning for its one missing test, status 0 and a
measured millisecond duration. Local process signal metadata is separately
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
  exact 30000 ms session duration, one warning, partial passed/failed batches,
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
here. Only changed crates and their production dependents are tested.
