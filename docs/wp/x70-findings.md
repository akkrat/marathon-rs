# WP x70 findings: the final result of a test agrees with its verdict

Fixes C87 with D77 (approved 2026-11-17, project owner's decision). Base: main dbe540ed.

## C87, verified before the change

`PoolProgressAccumulator::test_ended` (`crates/marathon-core/src/execution/progress.rs`, port of Kotlin's
`testEnded`) passed `action == Some(TestAction::Complete)` to `Tracker::test` as the final flag. The state machine
(`advance`) completes a test when `done >= total`, or, with `fast`, on the first pass under ANY_SUCCESS or the first
failure under ALL_SUCCESS (`has_pass` / `has_fail`), or on a final INCOMPLETE. Without `fast` the completing event is
therefore the last run, and its own status is tracked as final while the verdict (`Passed`/`Failed`, which
`aggregate_result` and the exit code follow) can disagree. The ledger description was accurate; the old witnesses
(`c87_any_success_without_fast_tracks_the_last_run_as_final`, the C87 part of
`d72_fixed_under_all_success_without_fast_runs_every_copy_and_fails`) reproduced both directions.

## What `fast: true` tracks as final (verified in the code)

With `fast: true` the final flag is the completion of the real machine, so the final record is the first event that:
- under ANY_SUCCESS: is a pass (PASSED, IGNORED or ASSUMPTION_FAILURE map to `TestEvent::Passed`), or completes the
  test otherwise (`done >= total` with every run failed, or a final INCOMPLETE before any pass);
- under ALL_SUCCESS: is a FAILURE, or completes the test otherwise (`done >= total` with every run passed, or a final
  INCOMPLETE).
Once the test is terminal, no later event is final (terminal states return no action).

## Design

Each test keeps a second `TestState`, `fast`, advanced by the same `advance` function with `fast: true` on every event
(Started, AddRetry, RemoveAttempts, Passed, Failed, Incomplete). With `executionStrategy.fast: false` a result is
tracked as final when this shadow completes on its event; with `fast: true` the real machine's action is used, exactly
as before (the shadow would give the same answer, but the explicit branch keeps `fast: true` untouched by
construction). The real machine, its actions and `aggregate_result` are unchanged, so the queue, retries, copies and
exit code cannot change. The test-support seam `replace_state_for_test` seeds the shadow with the state `fast` would
be in: `Passing` under ANY_SUCCESS becomes `Passed`, `Failing` under ALL_SUCCESS becomes `Failed`, others are equal.

Invariants (argued, and checked by the witnesses and the oracle allowances):
- At most one tracked result per test is final: the shadow completes at most once on a tracked event (a terminal
  state never completes again on Passed/Failed/Incomplete; AddRetry's Complete on a terminal state is not tracked).
- The shadow completes no later than the real machine: while it is not terminal it has the same counters and
  `Passing`/`Failing` kind as the real machine (both start equal and take the same events; they first differ when the
  shadow becomes terminal), so any event that completes the real machine completes a non-terminal shadow too. A
  test that completes always has a final result.
- The final result agrees with the verdict: under ANY_SUCCESS a pass makes the shadow `Passed` and the real machine
  `Passing`/`Passed`, which `aggregate_result` counts as success; a shadow completion without a pass only happens
  when the real machine completes as `Failed`. Symmetrically under ALL_SUCCESS.
- The rule is "the run `fast: true` would have tracked as final for the same sequence of runs": with `fast: true` the
  runs after completion would not happen (the queue drops them), but everything up to the completing run is the same.

One edge the rule adds: a test whose verdict an early run decided but that never completes (the end-of-run
`Expected to run ... more, but no terminal events received` error, runs lost before D69) now has a final result, its
deciding run; before, it had none. It agrees with `aggregate_result`, which already counted such a test by its
`Passing`/`Failing` state. Documented in D77.

## Consumers of the final flag

Only `ExecutionReportGenerator` reads the flag (`TestEvent::final_result`); the analytics backends (InfluxDB,
InfluxDB 2, Graphite) send every event and ignore it, and the iOS vendor's `Track` only forwards.

Changed (only with `fast: false`, only for tests with several runs whose verdict an earlier run decided):
- `ExecutionReport::summary` (`PoolSummary::tests`, `passed`, `failed`, `ignored`, `retries`, `duration_millis`):
  the stdout summary's first block (`N passed, N failed, N ignored tests`, its `Failed tests:` list) and
  `Flakiness overhead` (raw minus final duration).
- Per-test result files (`TestJsonReporter`): status, timings, batch id, device and so the file's directory,
  attachments.
- JUnit: the test case's status and `time`, which other runs appear as `flakyFailure`/`rerunFailure`, the suite's
  `failures`, `time` and `timestamp`; and the HTML report (summary tests). Their case order follows when the final
  result was tracked, so a test can move.
- Under ANY_SUCCESS this includes a test whose runs all passed: its first pass, not its last run, is now the final
  record. This is what the brief's rule gives (fast and not-fast agree on the record); it is in D77's user impact.

Unchanged: `aggregate_result` and the exit code; the queue and its actions; retry and flakiness decisions
(`RetryProgress::retry_test` uses the real machine); the `Raw:` line and its failed/incomplete lists (every run);
`raw.json`; Allure (every test event); the timeline; device bills and device-info files; analytics backends; the
per-test progress lines and CI progress; every output with `fast: true`.

## Witnesses

Each fails with the `progress.rs` change reverted (checked by reverting only that file and running the four suites:
`flakiness_scenarios` 3 failures, `pool_progress` 1, `progress_oracle` 1, `queue_oracle` 4) and passes with it.
- `flakiness_scenarios::d77_any_success_without_fast_tracks_the_first_pass_as_final` (renamed from
  `c87_any_success_without_fast_tracks_the_last_run_as_final`): ANY_SUCCESS, `fixed` count 2, FAILURE PASSED FAILURE:
  exit 0, final PASSED, no JUnit `<failure>`, `1 passed, 0 failed`, `Raw: 1 passed, 2 failed` with `failed 2 time(s)`;
  and every run failing keeps the last run, FAILURE.
- `flakiness_scenarios::d72_fixed_under_all_success_without_fast_runs_every_copy_and_fails` (C87 part rewritten):
  ALL_SUCCESS, PASSED FAILURE PASSED: exit 1, final FAILURE, JUnit `<failure>`, `0 passed, 1 failed` with the
  `Failed tests:` list, `Raw: 2 passed, 1 failed` unchanged.
- `flakiness_scenarios::d77_fast_and_not_fast_track_the_same_final_record`: both modes, a deciding run and no
  deciding run: `fast: true` and `fast: false` write the same per-test result file (batch id masked: it is random per
  run) and summary first block and the same exit, with 2 and 3 runs respectively.
- `pool_progress::d77_the_final_run_is_the_one_fast_completes_the_test_on`: on the accumulator itself, which run
  (by batch id) is tracked final for six run sequences under both `fast` values. Its `fast: true` half shows that
  setting unchanged: it held before the change too (the four `fast: true` assertions passed on the reverted code; only
  the `fast: false` ones failed).

## JVM recordings (D77 allowance)

The recordings are unedited. Two replays compare with a narrow allowance, `tests/support/d77.rs`, which rewrites only
the final flag of tracked runs in `fast: false` rows: per test, the first tracked run that the JVM marked final or
that decides the verdict (a pass under ANY_SUCCESS, a FAILURE under ALL_SUCCESS) is final, none after it.
- `progress_oracle` (`fixtures/oracle/progress/pool.jsonl`, single events on seeded states): the allowance moves the
  flag on exactly 22 rows; `d77_moves_only_the_final_flag_of_non_fast_rows` pins the count, that only `final` fields
  differ, that every row is `fast: false`, and that each moved flag is a deciding event (true) or follows a decided
  state (false).
- `queue_oracle` (`fixtures/oracle/queue/queue.jsonl`): `d77_moves_only_final_flags_of_non_fast_recordings` pins the
  13 moved flags by case, step and test, asserts nothing else moves and no `fast: true` recording is touched, and shows
  that a recording compared with the JVM's flags still mismatches (2 steps) while the D77 flags match. One moved flag
  is in C46's recording "K2", which D69 already supersedes; `d69_supersedes_exactly_the_c45_to_c47_recordings` is
  unchanged.

Parity matrix: no row changes. The ported Kotlin `PoolProgressAccumulatorTest` cases assert the state and
`aggregate_result`, not the final flag, and all pass unchanged; the oracle replays are native tests without matrix rows.

## Golden diffs

None. No result-file golden, contract corpus or recording changed; the marathon-cli suites (end to end and
differential) pass unchanged, as none of their inputs runs a test several times with `fast: false`.

## What was run

- `cargo fmt --check`; `cargo clippy -p marathon-core -- -D warnings` and `cargo clippy -p marathon-core --all-targets
  -- -D warnings`: clean.
- `cargo test --no-fail-fast -p marathon-core -p marathon-cli -p marathon-test-support`: all pass (after
  `cargo build -p snakeyaml --example yaml_tree`, which the evidence tooling needs on a fresh worktree).
- `cargo test --no-fail-fast -p marathon-android -p marathon-ios -p marathon-macos -p marathon-apple
  -p marathon-analytics`: all pass.
- The temporary revert above.
- `scripts/check_plan_dag.py`, `tools/differential/make_registry.py --check`, `tools/differential/test_evidence.py`:
  pass. `scripts/parity_matrix.py --check` reports the matrix out of date with Apple code-coverage rows "no longer
  found"; the script reads only the Kotlin checkout and the CSV, and this WP changes neither (x62 saw the same).

Not run, per the batch rules: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Incidental bugs

None found.
