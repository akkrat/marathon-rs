# WP x68: a batch lost after it started keeps what ran

Base: `0c94a641`. Approved 2026-11-17 by the project owner. Fixes U197. Recorded as an extension of D70, not as D77
(see "Contract" below).

## Design

### What was verified first

U197's description matches the code. The core's `account_for` (actor, `leaving = true` on `Terminate`) asks the device
`withdraw_unstarted` only while the batch's result is not in. D70's Android record answered `false` for a `Started`
batch without completing the result, so the core sealed it, sent `ReturnTestBatch`, and the queue charged every test of
the batch one uncompleted attempt and ran them again, finished ones included. When the runner hears the loss first, it
closes the run (`after_test_run`) and the result keeps what ran (D28). Which side hears first is a race; in the fake adb
used by the tests the runner always wins, because the fake ends a device's sessions as the device goes.

### The fix (Android only; the core is untouched)

- `AndroidRunnerExecutor::execute` owns the batch's listener chain (now in an `Arc`) and, before the runner starts,
  hands the device a `Weak` to it: `AndroidDevice::record_results_source`. The suggested design had the runner pass it
  at `record_submission`; the runner only borrows the listener (`&dyn AndroidTestRunListener`, also used that way by
  `android_runner.rs`), so the owner registers it once instead. It is consulted only in the `Started` stage, which comes
  after the first submission, so the two are equivalent.
- `withdraw_unstarted` on `Started` takes the handle under the record's lock, releases the lock, upgrades the handle and
  calls the listener's `results_so_far` — the same call, and so the same results, as D70's `Parked` path and the runner's
  own close: `TestResultsListener::results` on a copy of the accumulated results. Finished and failed tests keep their
  status; the test in progress (`Incomplete` in the accumulator) and every test of the batch with no result, including
  those of later D28 instrumentations, are uncompleted. It then re-takes the lock and, if the batch is still `Started`
  (or parked meanwhile), marks it `Withdrawn` and completes the batch's result with the snapshot, and answers `false`, so
  `account_for` reports `CompletedTestBatch`. This is exactly what the Parked path does at a closed gate.
- Fallbacks: no handle (a test executor such as `outage_batch_test.rs`'s `Submitting`), a listener already dropped, a
  listener chain with no results (`results_so_far` returning `None`), or an error (only a replay-clock failure) leave the
  stage `Started` and answer `false` without completing anything: today's accounting. The error case logs one DEBUG line.
- `end_batch` clears the handle at `Done`.
- `wait_for_next_instrumentation`'s fast path (gate open, device not lost) now also refuses a withdrawn batch, so a
  batch the core ended between instrumentations sends nothing more, not even the next instrumentation's `pm clear`. A
  healthy batch is never withdrawn, so this only adds a lock read.

### After the core has completed the result

The runner goes on until the core's stop cancels it. Checked what it can still do:

- Its own completion (`TestResultsListener::after_test_run`) loses: `SharedDeferred::complete` is first-wins, and the
  actor reads the result once, in `account_for`.
- Test results reach the core only through the batch result. `TestRunListenerAdapter` feeds the core's `LogListener`
  (log files, attachments), and the Android artifact listeners write files and emit attachments into the batch's
  collector; none reports a test. Progress, the summary and analytics come from the core's handling of the
  `CompletedTestBatch`, so nothing is counted twice.
- The runner's close may still log `uncompleted = <test>, <serial>` WARN lines from its own, later view; logs are not
  contract, and the same lines follow a cancelled batch today.
- Record submission is refused once withdrawn (D70), and with the fast-path change above the next instrumentation does
  not get as far as its device commands.

## Invariants

1. One outcome per batch, decided once in `account_for`: the snapshot is completed into the result before
   `withdraw_unstarted` returns, so the core sees it; a runner completion that got in first is used instead (both are
   the listener's view of what ran).
2. No Arc cycle: the record holds the listener and the result by `Weak`, and drops the listener handle at `Done`. The
   listener holds the device and the result, so a strong handle would keep the device, listeners and result alive after
   the batch (the S-x63-1 lesson).
3. `withdraw_unstarted` stays synchronous and does no I/O. It holds no lock across the listener call; the listener takes
   its accumulator's and attachment collector's std mutexes and reads the device's state, all short and never held
   across an await. If the upgrade makes the core thread drop the last handle to the listener chain (the batch task
   ended meanwhile), the drop runs there; none of the listeners has a `Drop` implementation.
4. A healthy run (no device lost) sends the same commands and logs the same lines; the cost is an `Arc` around the
   listener chain and a lock read per instrumentation.
5. A batch's tests are never given back uncharged once an instrumentation was sent (D70 invariant 4 holds): unfinished
   ones are uncompleted and charged once.

## Contract

The result data of a run whose device is lost while a batch runs changes: finished tests of that batch count once
instead of being run again, and the batch's tests are charged once only if they did not finish. This applies D70's
already approved rule ("a batch partly run keeps what it ran", and D28's report when the runner sees the loss first) to
the moments D70 left out; there is no new behaviour class, no configuration field and no new contract surface. So it is
recorded as an extension of D70 (PLAN §3.11 row, `docs/deviations.md` D70 section, `docs/migration.md`, PLAN decisions
list), not as D77, which stays unused. The owner may prefer a separate D77; the text moves over unchanged if so.

## Verified, and how

Each witness was run with its part of the fix temporarily reverted, and failed:

| Witness | Revert | Result without |
|---|---|---|
| `outage_batch_test::a_batch_lost_during_an_instrumentation_keeps_its_finished_tests_u197` (batch `one`, `two` in one instrumentation, `three` in a second; the first instrumentation hangs in `two`) | `Started` uses no handle | fails at "completed with what ran" |
| `outage_batch_test::a_batch_lost_between_instrumentations_away_from_the_gate_keeps_what_ran_u197` (held as the second instrumentation is announced, gate open) | same | same |
| `outage_batch_test::a_batch_lost_as_an_instrumentation_ends_starts_nothing_more_u197` (held at the first instrumentation's `TestRunEnded`) | fast path ignores a withdrawal | the second instrumentation is announced (`announced == 2`) |
| the first two above, plus D70's `a_batch_given_up_between_instrumentations_keeps_what_the_first_gave_d70` | device keeps a strong handle to the listener | the `Weak` to the result (held by the listener) still upgrades |
| `reconnect_test::a_finished_test_of_a_batch_lost_while_it_ran_is_not_run_again_u197` (whole run) | `Started` uses no handle | runs of `T00`, `T01` = `(2, 2)`, expected `(1, 2)` |

The device-level tests call `withdraw_unstarted` as the core's `Terminate` would, check the completed result (finished,
failed, uncompleted by name), let the runner go on or cancel it as the core's stop does, check the result is unchanged,
that only one `am instrument` was sent, and that `Weak`s to the result and to the batch's probe listener no longer
upgrade once the test drops its handles. Ordering uses listener signals polled by the file's existing `until` helper,
never a sleep: the probe sits before the results listener in the chain, so when it reports `two` started, `one`'s end
has been fully handled.

The whole-run witness uses the virtual-clock reconnect harness with two new options: `batch_size` (`fixed-size`
batching) and `hold_at`. The parse lists only `T00` and `T01`, so they form one batch; its first run passes `T00`,
starts `T01` and hangs. `hold_at` puts a listener first in the chain that holds the runner at `T01`'s start until the
core has settled the batch's result: the fake ends the device's sessions when it goes `offline`, so without the hold the
runner would always report first and the race could not be seen. The `offline` event waits until the runner is held
there; the grace ends in a disconnect and the core's `Terminate`; the device returns at 40 s and runs `T01` alone. The
test asserts that the run passes, one disconnect, `T00` sent once and `T01` twice, and no `onReturnBatch`.

Run, all passing: `cargo fmt --check`; `cargo clippy -p marathon-android --all-targets -- -D warnings`;
`cargo test -p marathon-android` (all suites, including the full `reconnect_test.rs`, 26/26, and
`outage_batch_test.rs`, 7/7); `cargo test -p marathon-cli --no-fail-fast` (every suite passes, including the vendor
differential; 7 tests are ignored in the code as before); `python3 -I scripts/release/test_release.py` (reads the
changelog); `python3 -I scripts/check_plan_dag.py`. The core is unchanged, so its clippy and tests were not rerun.

Worker command transcripts: no differential or golden scenario loses a device while a batch runs with a runner that
could hear it second; with no device lost nothing changes, so no comparator allowance was needed.

## Not verified

- No real emulator or adb; fake adb only, and the whole-run witness forces the order with a listener hold.
- A snapshot taken while one `TestEnded` is being handled by the listener chain can count that test as finished before
  the listeners after the results listener (`AdamScreenCaptureTestRunListener`, `ProfilingRunListener`) have attached
  their files to it; that test's result then lacks those attachments. It is reported once, with the right status. Only
  runs with adam screen capture or profiling, and only in that moment, are affected.
- iOS and macOS are untouched (the trait default).

## Golden diffs

None.

## Incidental bugs

None found.
