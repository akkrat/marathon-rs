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
  own close: `TestResultsListener::results` on a copy of the accumulator, ended as a run failure would end it (review
  round 1, below). Finished and failed tests keep their status; the test in progress (`Incomplete` in the accumulator)
  and every test of the batch with no result, including those of later D28 instrumentations, are uncompleted. It then re-takes the lock and, if the batch is still `Started`
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
list), not as a new D-row. The owner accepted that; D77 went to WP x70 (C87).

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
  runs with adam screen capture or profiling, and only in that moment, are affected. Since review round 1 this is in
  D70's text and pinned by a test (below).
- iOS and macOS are untouched (the trait default).

## Golden diffs

Round 0: none. Review round 1 adds a new golden, `fixtures/contract/virtual-runs/android-lost-after-start.json`; no
existing golden changed (`fixtures/contract/native-v1` is untouched). See "Review round 1 (Sol)", S-x68-2, for what it
holds and why it is not a whole-CLI case.

## Incidental bugs

None found.

## Review round 1 (Sol)

Verdict: merge after fixes. The D70 extension, the weak registration, the lock order, the withdrawal checks and
`hold_at` were judged sound, and D77 unnecessary.

### S-x68-1 (Medium): live snapshots kept zero end times — fixed

`results_so_far` built the snapshot from a plain copy of the accumulated results, so the test in flight kept the
`end_time` of 0 that `TemporalTestResult::new` gives it until a `TestEnded` or a run end. A snapshot that became the
batch's result therefore carried `start = <time>, end = 0`, a negative duration in every reporter that subtracts them.

What the runner's own close does for such a test: every instrumentation ends with `TestRunEnded` or `TestRunFailed` (the
parser's `transform` at the stream's end, or the runner's stuck, adb-error and producer-failure paths), and the
accumulator's `test_run_ended`/`test_run_failed` call `fill_end_time`, which ends every test without an end at the
timer's current time. The status stays `Incomplete` and the stack trace stays as it was (D25 adds one only for a broken
stream). The fix mirrors exactly that on the copy: `TestRunResultsAccumulator::ended_copy` (core, Rust-only) clones the
accumulator and runs `fill_end_time` on the clone; `results_so_far` builds the results from it. So the uncompleted
tests with no record start where the last test ended, as after a run failure. Completed results already have an end and
are not touched; the accumulator itself is unchanged. The only extra cost is a timer read per unended test, and only
when a snapshot is taken (device loss, or the gate).

D70's `Parked` path uses the same `results_so_far`, so it gets the same treatment. It had no such case in practice: a
batch parks between instrumentations, after the previous one's `TestRunEnded` or `TestRunFailed` has ended every test.

Witness: `outage_batch_test::a_test_in_flight_when_the_batch_is_lost_ends_then_u197`. The probe now can sit after the
results listener (`Reached::behind`, the profiling slot), so when it hears `two` start the accumulator has recorded it;
the accumulator's timer is the live `SystemTimer`, so times are nonzero. It asserts `two`'s end is at or after its start,
which is after `one`'s, and that `two` is its own record (no generated stack trace). With the copy's end times reverted
to the accumulator's, it fails: `the test in flight has no end: ... start_time: 1794909519235, end_time: 0`.

### S-x68-2 (Medium): no reviewed golden for the changed result contract — fixed, with a different harness

The native goldens (`fixtures/contract/native-v1`, `tools/differential/run bless`) are whole-CLI runs of the
`differential_cli` probe on the real scheduler clock. Its only ordering seam is a fixed list of entry barriers; it has
no listener hold and no virtual clock. The U197 path needs the core to hear of the loss before the runner, and the fake
adb ends a device's sessions as it goes offline, so in a CLI run the runner always hears first and the result is D28's,
not this one. So no CLI case can produce it deterministically without a new production seam. The narrowest sound
alternative: the virtual-clock reconnect harness, which already drives the real provider, core, runner, listeners and
the standard reporters, keeps the run's output files and compares them with a committed golden.

- `reconnect_test.rs` `Options::results`: the run uses taped hooks as the native probe does (wall clock frozen at
  2020-01-01T00:00:00Z, monotonic frozen, numbered `core.TestBatch.id` and `core.AllureReporter.uuid`, the provider's
  borrow draw answered with the only device) and returns the output directory's files. `Options::hold_behind` puts the
  hold after the results listener.
- `a_batch_lost_after_it_started_writes_what_ran_u197`: one batch of `T00` (passes), `T01` (fails), `T02` (in flight,
  held after its start is recorded) and `T03` (not started); the device goes offline, its grace ends in a disconnect
  and the core's Terminate, and it returns at 40 s to run `T02` and `T03`, which pass. The test checks the run's outcome
  (failed, because `T01` failed), one disconnect, no `onReturnBatch`, no fake warnings, and `raw.json`'s records in
  order with no negative duration; then it compares the normalized files with the golden.
- The golden, `fixtures/contract/virtual-runs/android-lost-after-start.json`: per-test JSON, `raw.json`, JUnit, Allure
  results, HTML pages, device and bill files; JSON parsed, the fake's port as `PORT`. Static report assets and the
  Allure environment (an echo of the configuration with local paths) are left out. `MARATHON_BLESS_VIRTUAL_RUNS=1`
  rewrites it. Registered as the `android-lost-after-start` row of `tools/differential/registry.json` (owner x68,
  regenerated with `make_registry.py`), and described in `fixtures/PROVENANCE.md`, `tools/differential/README.md` and
  `docs/design/differential.md`.

Golden-diff note (new file, reviewed): `raw.json` holds `T00` PASSED, `T02` INCOMPLETE, `T03` INCOMPLETE, `T01` FAILURE
(the core's order for one batch: finished, uncompleted, failed), then `T02` and `T03` PASSED from the second batch, all
with duration 0 under the frozen clock. `T00` and `T01` appear once each: the lost batch's finished and failed tests are
kept, not run again. JUnit has four test cases with one failure (`java.lang.AssertionError: expected`) and the final
`T02`/`T03` passes; HTML and Allure agree. `T02`'s uncompleted record has no stack trace (the accumulator's own record,
as after a run failure that is not a broken stream), while `T03`'s has the generic "Test didn't complete" text. The
per-test JSON of `T00`/`T01` shows the device as it was when the batch ended (`DISCONNECTED`, unhealthy), as D28's
runner-first report does.

Each fix fails the golden test when reverted: without S-x68-1, `raw.json` has `T02` INCOMPLETE with duration
`-1577836800000`; without the U197 snapshot (`Started` uses no handle), the batch comes back (`onReturnBatch` logged
once, expected 0). Three consecutive runs gave the same files.

### Attachments at the snapshot boundary — documented and pinned

D70's text in `docs/deviations.md` now says that a test whose end the listeners are still handling when the batch ends
counts with its status but may lack the screen capture and profiling files attached as it ends.
`outage_batch_test::a_test_ending_when_the_batch_is_lost_counts_without_later_attachments_u197` holds the runner in a
listener after the results one at `one`'s `TestEnded`, ends the batch, checks `one` is finished with no attachments,
releases the listener, which then attaches a file to `one`, and checks the completed result is unchanged.

### Run in round 1

`cargo fmt --check`; `cargo clippy -p marathon-android -p marathon-core --all-targets -- -D warnings`;
`cargo test -p marathon-android`, `-p marathon-core` and `-p marathon-cli` (results in the final report);
`python3 -I tools/differential/make_registry.py --check`; the registry's `validate_registry`;
`python3 -I tools/differential/test_evidence.py` (after building the YAML helper).

## Review round 2 (Sol)

S-x68-1 and the attachment item were closed. S-x68-2 stayed open because the golden scenario was nondeterministic; the
cause is S-x68-3.

### S-x68-3 (Medium): a loss could pass the listener hold — fixed

The reconnect harness applied a due loss when either of two branches held: the state/server branch, which also
required the `while_unstarted` and `hold_at` conditions, or the `logcat -c` branch (`HELD_NOW`), which required only
that setup had sent `logcat -c` and a batch had begun. Every device sends `logcat -c` in setup, so the second branch let
the loss through without the scenario's own condition. Round 1's claim that "the `offline` event waits until the runner
is held there" was therefore wrong whenever setup was slow.

Reproduced before the fix: the golden test failed in 1 of 5 and 1 of 4 standalone runs. With the requests and logs
dumped, the failing run's batch began at 11.2 s virtual (setup had taken long against the stepping clock), so the
14 s `offline` came during its four seconds, before anything was submitted. The batch was then the D70 unstarted case,
not the U197 one.

The fix, in `reconnect_test.rs` only:

- A loss (`Event::is_loss`: a disconnect, a server kill, or the states other than `device` and `listed`) waits for
  `while_unstarted` and `hold_at` whichever branch makes it ready. A return still takes either branch as before: D70's
  `a_batch_held_through_a_grace_that_ends_in_a_disconnect_is_not_charged_d70` depended on the `logcat -c` branch for its
  `device` event, because no batch is in its four seconds once the device is gone.
- While a loss waits for the hold, the clock keeps stepping instead of being held for `HOLD`: the runner reaches the hold
  only after its batch's four virtual seconds. The wait is bounded by `HOLD_LATE_MILLIS` (120 s virtual), a stall
  catcher, instead of `MAX_LATE_MILLIS`.
- When such a loss comes after its time, every later event moves by the same amount (`shift`), so the scripted
  intervals stay as written: with the worst case below the return came at 44 s instead of 40 s, still 26 s after the
  loss and so after the 10 s grace's disconnect.

The order is now fixed by the run's own state, not by host speed: the loss cannot come before the runner is held at
`T02`'s start (or `T01`'s), and the return cannot come before the loss.

Exposure of the other witnesses that use these gates:

- `a_finished_test_of_a_batch_lost_while_it_ran_is_not_run_again_u197` (`hold_at`): the same exposure, fixed by the
  same change.
- `a_batch_held_through_a_grace_that_ends_in_a_disconnect_is_not_charged_d70` (`while_unstarted`, state event on
  `emulator-5554`): exposed. Its `offline` could come through the `logcat -c` branch while the batch was between its
  four seconds and the runner's first submission, so the batch caught could have started. Fixed by the same change.
- `a_batch_held_by_a_server_that_is_given_up_is_not_charged_d70` (`while_unstarted`, server kill): not exposed. A kill
  has no serial, so the `logcat -c` branch never counted for it.
- The `outage_batch_test.rs` U197 witnesses do not use the harness. They hold their own listeners and call
  `withdraw_unstarted` directly, so no scheduled event can come early.

Worst case, forced: with a temporary 2.5 s real-time pause before the first batch starts (removed before commit), the
first batch began at the barrier just before the loss (13.95 s). The loss waited for the hold until about 18 s, the
return moved to 44 s, and both U197 witnesses passed. Both D70 witnesses passed with the same pause.

### Is there a product hang? No

The stall was "the re-run batch waits in the hold for a decision". To check that the product decides a batch whose
device goes offline and comes back before its first instrumentation, the golden scenario ran once without the hold and
with `while_unstarted`, which puts the loss exactly in that window (a temporary test, not committed). The run ended by
itself at 352 s virtual. The batch waited at the gate ("waits for its adb server or device before its next
instrumentation"). The 10 s grace ended in a disconnect, and the core's `Terminate` rejected the batch with no attempt
charged (`Rejecting batch …: device … left before starting it`, one `onRejectBatch`). The device returned at 40 s and
the batch ran again. Its scripted hang at `T02` ended by the 300 s output timeout ("Test got stuck"), with `T02` and
`T03` uncompleted, and a third batch ran them. So D70's withdrawal, D41's grace and the D22 gate all decided. Only the
test's hold waits with no limit, and it waits for a decision that, in that order, no later event would make.

### Reversal witnesses rerun

Each was run 3 times, all with the same result, and every run ended without a stall:

- U197 snapshot disabled (`withdraw_unstarted` answers `false` on `Started` without using the handle): the golden test
  fails at `assert_eq!(logged(&observed, "onReturnBatch"), 0)`, `left: 1, right: 0`. The batch is returned once.
  `a_finished_test_of_a_batch_lost_while_it_ran_is_not_run_again_u197` fails with `runs of T00 and T01`,
  `left: (2, 2), right: (1, 2)`.
- S-x68-1 reverted (`ended_copy` without `fill_end_time`): the golden test fails at the `raw.json` check:
  `("T02", "INCOMPLETE", -1577836800000)` where `("T02", "INCOMPLETE", 0)` is expected.

Round 1's "stalled" disabled-snapshot run was this flake: the loss came before the hold.

### Repeat runs (standalone, in a row, after the fix)

| Test | Passed |
|---|---|
| `a_batch_lost_after_it_started_writes_what_ran_u197` | 20/20 |
| `a_finished_test_of_a_batch_lost_while_it_ran_is_not_run_again_u197` | 10/10 |
| `a_batch_held_through_a_grace_that_ends_in_a_disconnect_is_not_charged_d70` | 10/10 |
| `a_batch_held_by_a_server_that_is_given_up_is_not_charged_d70` | 10/10 |

The golden is unchanged.

### Run in round 2

`cargo fmt --check`; `cargo clippy -p marathon-android --all-targets -- -D warnings`; `cargo test -p marathon-android`
(17 suites, 273 passed, none failed; `reconnect_test.rs` 27/27, `outage_batch_test.rs` 9/9); the local checks. Only the
test harness changed, so the core and CLI suites were not rerun in this round.
