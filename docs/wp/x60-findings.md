# WP x60: ordinary console completion and diagnostic text

Base: `19ec3fbc` (`0.13.0-rc.2`). Approved 2026-11-11 by the project owner
for 0.13.0. D67 changes the ordinary completion allowance; the diagnostic
changes follow PLAN §3.12.5.

## A. U196 / D67

`TracingOutput` and `OutputGuard` own the same `Arc<CompletionDeadline>`.
Its `OnceLock<Instant>` starts on the final ordinary flush, or on guard
shutdown/drop if there was no flush. The acknowledgement and drain each
compute the remaining time immediately before waiting. An explicit shorter
guard timeout is clamped to that same deadline. Neither stage can restart
its allowance. The deadline is not measured from construction or run startup.

The ordinary allowance is 60 seconds. `EXIT_FLUSH` remains one second and
still bounds the signal watchdog, including preparation, during the run.
Second-signal immediate exit during the run and D66 SIGQUIT are unchanged.
After the run finishes, SIGINT/SIGTERM are not acted on during console
completion, with no second-signal escape. This window already existed for
about two seconds; D67 lengthens it to up to 60 seconds on a stalled console.
A wrapper needing a hard stop then must use SIGKILL. Queue messages, admission,
ordering, nonblocking producers and the existing expiry diagnostics are
unchanged. No configuration knob or differential stderr tolerance was added.

Tests:

- `ordinary_delayed_flush_recovers_with_status_and_ordered_output_u196`
  replaces the x59 witness: gate the sink, observe ordinary flush starting,
  keep it gated past the former one-second budget, then release it and assert
  complete ordered version output, exit 0 and no stderr diagnostic. Its
  one-second-plus window is a negative check; successful waits use 60-second
  hang guards.
- `completion_tests::stalled_flush_and_drain_share_one_deadline_u196`
  gates the sink and deliberately injects an expired ordinary deadline,
  avoiding a real minute of waiting. Flush must report expiry, and drain
  must retain that expired deadline and finish before a fresh 60-second
  allowance could expire. The sink remains gated until shutdown returns;
  the test then releases and awaits the writer. Its 30-second bound
  deliberately separates immediate expiry from a restarted minute.
- `shutdown_reports_sink_failure` now checks both the flush and drain
  diagnostics for a genuine sink failure.
- Existing signal-window and cancellation subprocess tests retain their
  first/second-signal assertions during the run.

The lifecycle design delegates writer deadline policy to logging.md and
states no ordinary numeric bound, so it needs no edit. D67 is recorded in
PLAN, deviations, migration, the changelog and U196's fixed row. The
current deviation ranges were advanced through D67 as requested.

## B. Log text and data boundaries

The catch-all DeviceActor ERROR is now
`Batch on <device> failed (<error>); treating it as recoverable`, using
`Failure::for_log` for the error and retaining the original throwable in the
record. It does not promise retries: the existing queue and retry quota decide
what happens to returned tests.

The separate `Unknown vendor exception caught:\n` prefix is part of the
cancellation/returned-batch reason: `execute_batch` seals the deferred with it,
`account_for` forwards it through `FromDevice::ReturnTestBatch`, and the pool
passes it to `QueueMessage::ReturnBatch`. The queue uses the reason only for
its DEBUG diagnostic; it builds incomplete results with `stacktrace: None`.
Thus this prefix currently reaches internal reason data and DEBUG logs, not
result-file traces or pinned progress/summary lines. Its producer, the
returned-batch projection envelopes and the tests pinning that reason remain
unchanged, keeping this edit limited to the standalone ERROR wording.

Startup comes from `CliConfiguration::log_description` in `args.rs`, called
before Marathonfile loading. The run description now labels the two booleans
`cliFlags: analyticsTracking=... bugsnagReporting=...`; defaults and explicit
flags still have exactly their previous parsing/merge behavior. A new
`run_outcome` test uses CLI defaults with both Marathonfile values false and
checks that construction sees false while the startup line identifies the CLI
values. D1's ignored-telemetry notice is unchanged.

The release helper is actually `scripts/release/release.py::check_cli`.
It checks version, status, empty stderr, a versioned startup prefix and help
commands, not the exact run description. `CliSmokeTests` stays unchanged and
passes. Existing `run_outcome` diagnostic assertions and the CLI matrix's
startup translation now expect the `cliFlags` label. No committed oracle or
pinned progress/summary golden was edited or re-recorded.

### Duration sites changed

All plain durations use the existing x57 convention, seconds with ` s`,
including exact fractions (`120.5 s`, `0.3 s`) and negative values. The
integer/nanosecond formatter was factored out of the existing idle-timeout
projection as `duration_for_log`; no floating point is used.

| Site | Diagnostic change | Data boundary |
|---|---|---|
| `marathon-apple/src/bin/xcrun/xcresulttool.rs::describe_timeout` | Effective xcresulttool, shell and fallback durations use seconds | Called by both iOS `simulator_provider::initialize_inner` and macOS `provider::initialize`; configuration is unchanged |
| `marathon-ios/src/apple_simulator_device/execution.rs::join_cancelled` | Cancelled-batch wait uses seconds | WARN only |
| `marathon-macos/src/device.rs` cancelled-batch wait | Cancelled-batch wait uses seconds | WARN only |
| `marathon-ios/src/apple_simulator_device/boot.rs::boot_for_preparation` | Boot-wait duration uses seconds in the WARN | Its original reason and elapsed-time hook reads are unchanged |
| `marathon-ios/src/apple_simulator_device/boot.rs::shutdown_after_failed_boot` | Shutdown budget uses seconds | WARN only |
| `marathon-core/src/error.rs::Failure::for_log` boot failures | Known generated boot-timeout reasons use seconds, including the later NotBooted failure diagnostic | Stored Boot/NotBooted messages retain ISO durations; external simctl text remains opaque |
| `marathon-core/src/error.rs::Failure::for_log` session close | `xcodebuild_session::stop`'s generated close timeout uses seconds when logged, including nested causes | Stored close failures retain ISO durations |

The sweep covered all crates' log/tracing calls, duration imports and duration
format sites, including inferred `wait`/`budget` values. Reader idle timeouts
were already projected by x57. SSH timeout units already have their x56
projection; keepalive warnings already use seconds. Numeric milliseconds,
worker command durations, parsing, configuration renderers, result data and
external tool text are unchanged.

The `apple-bin` oracle contains five `describeTimeout` rows pinning the old
pure helper's diagnostic string. The recordings remain unchanged: only those
five Rust assertion expectations now name the exact new diagnostic strings,
including the fractional case. Provider capture tests independently assert
the new text. Boot, shutdown, cancelled-batch, core catch-table and CLI capture
tests assert the new logs; projection tests assert stored ISO messages and
opaque external text remain unchanged.

## Verification

All Rust builds use `CARGO_BUILD_JOBS=6`, `CARGO_INCREMENTAL=0` and
`CARGO_PROFILE_DEV_DEBUG=0`. The initial disk check had 144 GiB available,
above the 20 GB stop threshold.

Part A verification before its commit:

- `cargo test -p marathon-cli --lib --test tracing_output`: passed (3 unit
  tests and 14 console integration tests).
- Counterfactual checks: restoring the one-second ordinary flush failed the
  delayed-recovery witness; restarting the guard with a fresh 60-second
  allowance failed the shared-deadline witness. Both exited 101; the source
  was restored and the witnesses passed again.
- `cargo clippy -p marathon-cli --all-targets -- -D warnings`: passed.
- Plan DAG and differential registry validators: passed.
- `tools/differential/test_evidence.py`: initially failed because the YAML
  helper had not been built; after building `snakeyaml --example yaml_tree`,
  all 107 tests passed.
- An initial full CLI run hit that same missing helper in vendor differential
  and production-parser checks. It also overlapped the temporary counterfactual
  binary, so its console witness failed as expected for the old flush policy.
  The final full crate verification is run after the log changes, with the
  helper present and no counterfactual builds overlapping it.

The configuration differential test and signal-window tests passed in that
initial run. No fixture, golden or stderr comparator was changed. Full gates,
remote checks and workspace-wide tests are reserved for the orchestrator by
the brief.

Final verification after both parts:

- Changed crates: `marathon-core`, `marathon-apple`, `marathon-cli`,
  `marathon-ios`, `marathon-macos`. The test run also includes their normal
  dependents `marathon-android`, `marathon-analytics`, `marathon-test-support`.
- `cargo clippy -p marathon-core -p marathon-apple -p marathon-cli
  -p marathon-ios -p marathon-macos --all-targets -- -D warnings`: passed.
- Targeted catch-table, duration projection, Apple helper, CLI matrix/outcome,
  provider, macOS batch and iOS preparation/disposal tests: passed. The first
  new iOS boot log assertions needed an explicit WARN capture level in the
  debug-off harness; after that test-only correction, all 22 preparation and
  four disposal tests passed.
- `python3 -I scripts/release/test_release.py CliSmokeTests`: 3 passed.
  The unchanged `check_cli` also passed against the built binary's actual
  `version` and `--help` output (the untagged build's fallback version).
- `python3 -I scripts/check_plan_dag.py`,
  `python3 -I tools/differential/make_registry.py --check`, and
  `python3 -I tools/differential/run validate`: passed.
- Final `python3 -I tools/differential/test_evidence.py`: 107 passed.
- Full crate verification:
  `cargo test -p marathon-core -p marathon-apple -p marathon-cli
  -p marathon-ios -p marathon-macos -p marathon-android -p marathon-analytics
  -p marathon-test-support --no-fail-fast`: exit 0; 2,218 passed, 0 failed,
  33 ignored across 214 test targets, including doc tests. Configuration and
  vendor differentials, report generation, ordinary-output witnesses and
  first/second-signal/SIGQUIT checks all passed with strict comparisons.
- `cargo fmt --check` and `git diff --check`: passed before each commit.

No committed fixture or golden changed. No comparator gained a stderr
allowance. The normal suite's 33 ignored tests were not forced; no new real
farm/hardware run was performed. `scripts/check.sh`, `scripts/remote-check.sh`
and workspace-wide tests were not run, as the brief reserves them for the
orchestrator. No approval decision remains open for this WP.

## Review fixes (S-x60-1–4)

S-x60-1 is a documentation correction, as chosen by the orchestrator.
`Marathon::run_under` calls `relay.stop()` before returning; Tokio's installed
handlers remain. The later `run_cli` flush and console drain therefore have
no SIGINT/SIGTERM handling or second-signal escape. D67 lengthens this existing
post-run window from about two seconds to up to 60 seconds. The public docs now
distinguish it from signal cancellation during the run and tell wrappers to
use SIGKILL when they need a hard stop. Signal handling code is unchanged.

S-x60-2 joins D67 to the PLAN §3.11 table. S-x60-3 widens the deliberate
expiry test's upper bound from five to 30 seconds, preserving its distinction
from a fresh 60-second drain with more margin for loaded runners. S-x60-4 uses
`Instant::checked_add` for an explicit drain timeout, falling back to the shared
completion deadline on overflow. `huge_shutdown_timeout_drains_queued_output`
checks that `Duration::MAX` drains queued output without panicking.

Review-fix verification:

- `cargo test -p marathon-cli --lib --test tracing_output`: passed, 3 unit
  tests and 15 console integration tests, including the shared-deadline,
  delayed-recovery and huge-timeout regressions.
- `cargo clippy -p marathon-cli --all-targets -- -D warnings`: passed.
- `cargo fmt --check`, `git diff --check` and
  `python3 -I scripts/check_plan_dag.py`: passed.
- Builds kept the brief's resource limits. Full gates, remote checks and
  workspace tests were not run, as requested. No signal-handling code,
  fixture, golden or comparator was changed by this follow-up.

## Incidental bugs

None independently verified beyond the assigned U196 and diagnostic leftovers.
