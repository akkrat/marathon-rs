# WP x61: queue robustness

Base: `7090b228` (0.13.0). Approved 2026-11-15 by the project owner. D68 fixes
U168; D69 fixes C45, C46 and C47. C5 stays as it is.

## A. U168 → D68: a failed queue ends the run, a failed progress line does not

### Console failures (`progress.rs`)

`PoolProgressAccumulator` has one private `flush` used at all four flush
positions: before and after the line in `test_started`, after the line and
after the transition in `test_ended`. A `Cancelled` error propagates. Any
other error loses that output only: the method carries on, so the transition
and the Track still happen. The first such error in a run logs one WARN on
`marathon_core::execution::progress`, with the error as its cause:
`Cannot write progress to the console; the run carries on. Later failures are not logged`.
"Once per run" is a `Rc<Cell<bool>>` owned by the Scheduler (one per run) and
handed to every pool's accumulator through `ProgressServices::console_warned`.
That target is an ordinary `marathon_*` target, so the WARN shows with
`debug: true` only, like other WARN lines; adding a floor would be a
`marathon-logging` filter change, outside this WP.

`test_started` has no production caller, here or in the Kotlin marathon (C4:
`testStarted()` is never called, so `running` goes negative). Its "caller"
path is therefore only the tests; it shares the helper, and a witness covers
both of its positions.

Unchanged: `line()`'s NaN error, the run's own flushes in `Marathon`, D67's
final ordinary deadline and the signal path.

### Abnormal queue completion (`pool.rs`, `scheduler.rs`)

*Steps 1–4 below are the first round's design. Review round 1 replaced the
pool-side message with a Scheduler-side callback; see "Review round 1 (Sol)".*

The queue's job is a child of the pool supervisor, which isolates its failure:
before D68 a handler error logged `Task stopped due to unrecoverable error`,
closed the queue, and the pool read the closed queue as "no work" (`send_to_queue`
dropped messages, `queue_has_work` returned false), so the run hung.

Only a handler error (or a panic) completes the queue job as
`Completion::Failed`; draining (`on_terminate` → `close()`) and the
supervisor's cancel complete it as `Cancelled`. So:

1. In `started`, the pool registers a completion callback on the queue job. On
   `Failed(cause)` it sends the pool `DevicePoolMessage::QueueFailed(cause)`
   (weak handle; ignored if the pool has gone).
2. The pool handles it like `on_queue_terminated` — `Terminate` to every
   device, then the supervisor's cancel — and returns the cause as its handler
   error, so the pool actor fails with the original `Failure`.
3. **Fatal-error policy reused:** a failing pool actor already fails the
   detached Scheduler job (an ordinary job), which cancels every pool actor
   and supervisor, so the other pools are **stopped**, their in-flight batches
   cancelled as by D52's `stop_execution` (which cancels the same job). Today
   no other pool handler error is reachable as a failure (they are all
   cancellations), so this path was latent.
4. After its D65 join loop the Scheduler checks each joined pool's queue job
   (completed, since its supervisor was joined) in pool order; the first
   `Failed` cause is returned as `VendorError::Other(cause)` instead of
   aggregating progress. This also covers a pool that closed normally before
   the callback's message arrived (a terminate that raced the failure), and a
   failure after every test was already terminal, where the progress reads all
   passed: no false success.
5. `run_async` returns that error; `run_under` runs `on_finish` (reports,
   provider, analytics), logs `Marathon run failed` with the cause and maps it
   with the existing policy: `ignoreFailures` (false → `Ok(false)` → exit 1,
   "Marathon execution failed"; true → exit 0). No new exit code.

Bounded: no new wait. The failed pool's devices stop as on a drain; other pools
stop as on a D52 cancel. In the stub witness the run ends at 5,200 ms, the
moment of the failure, with `serial-1`'s batch due at 10,000 ms and pool B's
device ready only at 600,100 ms.

Handler errors this covers, all still fatal: NaN progress formatting,
return-batch clock reads, batching/bundle/replay errors, Tracker-seam
failures outside D51's backend guard (including mapping-tracker clock or
delegate failures), metrics observation errors.

The log shows the cause up to three times: the queue's and the pool's
`Task stopped due to unrecoverable error` records and `Marathon run failed`.
Logs are not contract; I kept the existing completion logging rather than
special-casing the pool.

### Witnesses (D68)

- `queue_behaviour::handler_failure_ends_the_queue` — the actor half, kept
  unchanged; its comment now points at the pool and run halves.
- `queue_failure_run::a_failed_queue_ends_the_run_with_its_cause_while_another_pool_is_alive_d68`
  — whole run (production graph, Stub vendor, real Scheduler, `run_under`):
  two pools, a batch running in pool A when its queue fails batching another
  device's request (a bundle identifier failing on its second call), pool B
  still preparing. Asserts completion, `Ok(false)` (exit 1 mapping), finish
  before 10,000 ms, `Marathon run failed` with exactly the original cause,
  `test_result/raw.json` written and the provider terminated.
  Without the callback the run never ends; without the Scheduler check the
  cause is missing (the outcome was a progress-derived `false`).
- `pool_scheduler::a_queue_that_fails_on_the_last_result_is_not_a_success_d68`
  — the only test passes and its Track fails: progress reads all passed, the
  Scheduler returns the Track error. Without the Scheduler check it returned
  `Ok(true)` (false success); without the callback it never ended.
- `queue_behaviour::a_console_failure_at_either_test_ended_flush_costs_only_its_output_d68`
  — first position: the write carrying A's line fails; second position: output
  injected while B's line is being written (a gated sink write, explicit
  handshake, no sleeps) goes out with the post-transition flush, which fails.
  The queue drains (`Terminated`), both tests are tracked final and Passed with
  exact counters, B's line is printed, A's is lost, and exactly one WARN.
  Reverting only the second position's helper call fails it.
- `queue_behaviour::a_console_failure_at_either_test_started_flush_is_not_fatal_d68`
  — both `test_started` positions, Started transitions applied, one WARN.
- `queue_behaviour::a_cancelled_console_write_still_cancels_test_ended_d68` —
  a cancelled sink write returns `Cancelled`, no transition, no Track, no WARN.

## B. C45–C47 → D69: finished tests are not run again

### C45 (`handle_failed_tests`)

After each retried result's `test_ended`, if its test is terminal
(`PoolProgressAccumulator::is_terminal`), the retry is not queued: the attempt
leaves the totals (`RemoveAttempts(1)`) and `RetryStrategy::refund` gives back
the total quota (`RetryWatchdog::refund`, +1). I check "terminal" rather than
"this call returned Complete": with two retried copies of one test in a batch,
the first can complete the test and the second's `test_ended` then returns
`None` on the terminal state; it must not be queued either. `process_test_action`
still runs first, so queued copies are removed as before.

**Departure (per-test refund).** The brief asks to refund the per-test quota
too. There is no per-test retry counter: the per-test limit compares the
number of recorded *failures* (`PoolTestFailureAccumulator`, plus flaky
copies) with `retryPerTestQuota`, and that failure did happen. Un-recording it
would be inaccurate and is unobservable anyway: a terminal test's `AddRetry`
returns Complete, so it can never be retried again. Only the total quota is
refunded; `RetryStrategy::refund`'s comment says why.

### C46

`RetryStrategy::retried` returns the positions in `failed` of the granted
retries (`process` keeps its signature and maps them). The queue ends every
other position, so each failed copy gets exactly one `test_ended`.

### C47 (`on_reject_batch`)

Under the progress lock, each copy whose test is terminal is dropped:
`remove_test(test, 1)` and not queued; the rest are queued with `add_all`
(D60 insertion keys and FIFO ties as before). `active_retries` pairs of
terminal tests are not restored to `retry_workers`. A dropped copy that was a
granted retry keeps its quota charge: the queue cannot tell a retry copy from
an original when the device had no worker id, and the brief does not ask for
it.

### Invariants

- A test that is terminal is never offered to the queue again by these paths.
- For each test, `total` equals the copies that will report, so after its
  last in-flight copy reports, `done == total` (exactly, except C5's phantom).
- Every failed result in a batch gets exactly one `test_ended`; finality flags
  are those of the state machine (a late copy after completion is non-final).
- A refunded retry restores `total_allowed_retry_left` exactly.

### Witnesses (D69), each with exact totals, running counts, Track calls with finality, quota, queue order and a late completion

- `a_retry_granted_to_the_attempt_that_completes_the_test_is_not_run_d69`
  (C45, strict mode, a copy in flight elsewhere ends late, quota back to 10).
- `no_retry_of_a_batch_runs_once_an_earlier_copy_completed_the_test_d69`
  (C45, two retried copies in one class-name batch).
- `every_failed_copy_beside_a_retried_copy_is_ended_once_d69` (C46: the second
  copy is over the per-test quota, so no C5 phantom; the retry's pass then
  completes the test at 3/3).
- `a_rejected_copy_of_a_completed_test_is_dropped_d69` (C47 with a retry copy
  on the rejecting device, its retry worker not restored, a late completion on
  a third device, ending at 3/3).

`running` is negative throughout (C4: nothing calls `test_started`); the
witnesses assert it as production computes it.

### JVM queue oracle

The three recordings stay unedited in `fixtures/oracle/queue/queue.jsonl`. Their
replay was converted, not patched: after D69, K1 and K4 take another course from
the changed message on (K1's later steps hand out different batches and K4
terminates earlier), so patches would have rewritten nearly every later step,
i.e. re-recorded them. Instead:

- `SUPERSEDED_D69` (K1, K2, K4) is left out of `replay`, so the full replay
  and every other row stay exact.
- `d69_supersedes_exactly_the_c45_to_c47_recordings` (negative test) replays
  just those three and asserts the exact set of differing steps: K1 steps 1–6
  and progress, K2 steps 1 and 3, K4 steps 3–5 and progress. If the native
  queue reproduced a recording, or differed anywhere else, it fails.
- The three old tests (`retried_attempt_that_completes_the_test_is_queued_again`,
  `second_failed_copy_of_a_retried_test_is_never_ended`,
  `rejected_batch_queues_a_test_that_completed_meanwhile`) are removed; their
  native-contract replacements are the `_d69` tests above.

Parity matrix: none of these tests is a row in `docs/parity-matrix.csv` (they
port no Kotlin test), so no row changes. The QueueActorTest/RetryPlacement rows
still pass unchanged.

## Goldens

No registered native golden changes: `differential_vendors`,
`differential_local` and `differential_parse` pass unchanged, and
`make_registry.py --check` is clean. No golden-diff note.

## What was verified and how

- Every fix's witness was run against a temporary revert of that fix and
  failed: console helper swallowing (both console witnesses), the second
  `test_ended` flush alone, the C45 terminal check (both C45 witnesses), C46
  position matching (Test-equality revert), C47 dropping, the pool callback
  (run-level and Scheduler witnesses: never end) and the Scheduler check
  (run-level: cause missing; Scheduler-level: `Ok(true)`).
- The tests that involve real concurrency (the gated sink write) use explicit
  handshakes, no sleeps.
- `cargo test` for marathon-core (with and without `--all-features`),
  marathon-test-support, marathon-cli (including the differential suites),
  marathon-android, marathon-apple, marathon-ios, marathon-macos and
  marathon-analytics; `cargo clippy -p marathon-core --all-targets
  [--all-features] -D warnings`; `cargo fmt --check`;
  `tools/differential/make_registry.py --check`, `tools/differential/run
  validate`, `tools/differential/test_evidence.py`,
  `scripts/check_plan_dag.py`, `scripts/release/test_release.py`.
  `scripts/parity_matrix.py --check` and `scripts/source_inventory.py --check`
  report their CSVs "out of date" against the e47352f2 checkout, identically on
  the untouched base `7090b228`; this WP changes neither CSV.

## What was not verified

- No real device or worker run. The run-level witness uses the Stub vendor; the
  console witnesses use a scripted sink, not the CLI's `TracingOutput`.
- The full gate (`scripts/check.sh`, `remote-check.sh`, `cargo test
  --workspace`) was not run, per the batch rules.
- The CLI's exit code for this path is asserted through `run_under`'s mapped
  `Ok(false)`, not by running the binary.

## Incidental bugs

- **Uncompleted copy of a finished test is queued again (Kotlin, now C86, fixed
  by D69 in review round 1).** `QueueActor.kt:141–148` / `queue.rs` `handle_uncompleted_tests`:
  an INCOMPLETE result (a returned batch, or `uncompleted` in a completed batch)
  for a test that another device has already completed gets `testEnded(final =
  false)`, which returns `null` on the terminal state, and the copy is
  `rerunTest`-ed: the finished test runs again. Confirmed with a throwaway queue
  test (fast ANY_SUCCESS, two copies: the first passes, the second comes back
  INCOMPLETE → queue `[C0]`, state `Passed{total: 2, done: 1}`). Same class as
  C45/C47 but not in this WP's brief; not fixed.

## Review round 1 (Sol)

### S-x61-1 (high): a queue failure could still leave the run hanging

The reviewer's race, reproduced exactly: pool A runs a batch that only a cancel
ends; pool B's only test passes and its Track waits. B's last device leaves, so
B (after the device's Terminated) asks its queue `IsEmpty` and waits. The Track
then fails: the queue job completes Failed, its callback queues `QueueFailed` in
B's mailbox, B's `queue_has_work` sees the queue's job end and reads "no work",
and `terminate_now` closes B's mailbox, discarding `QueueFailed`. A stays active; the Scheduler stays in
`joining-pools` for ever and its post-join check is never reached. On
`0b019b24` the new witness fails with the state dump
`scheduler=active phase=joining-pools`, pool A `running`, pool B completed.

**Fix.** The failure no longer travels through the pool. When the Scheduler
creates a pool it registers a completion callback on the pool's queue job
(`Inner::on_queue_failure`). On `Failed(cause)` the callback keeps the first
cause in a run-level slot (`queue_failure`) and cancels the Scheduler's
detached job with `Requested(cause)`. That is what a failed pool used to do
through `ChildFailed`, and what D52's `stop_execution` does: every pool actor
and supervisor is cancelled, so the batches in flight are cancelled before the
Scheduler waits for the pools. After the join loop the Scheduler returns the
slot's cause instead of aggregating progress. The pool-side `QueueFailed`
message, its handler and the pool's callback are removed, so `pool.rs` and
`pool_message.rs` now differ from the base only by `pool.rs`'s module comment.

- The callback holds the Scheduler job weakly (the queue job is its
  descendant) and the slot by `Rc`; it does not depend on the pool's mailbox.
- First cause wins (`get_or_insert_with`), in time order rather than pool
  order.
- Bounded: no new wait. A cancel completes the pools as D52 does.
- **Fatal-error policy:** unchanged in effect: all pools are stopped. The
  failed pool's devices are now stopped by the cancel rather than by a
  `Terminate` sent just before the same cancel, which was a no-op difference.
- The callback is registered right after `DevicePoolActor::spawn`, before the
  queue can handle a message, and `invoke_on_completion` also covers a job
  that has already completed.

**Cases re-checked.**

- Single pool, last result's Track fails:
  `pool_scheduler::a_queue_that_fails_on_the_last_result_is_not_a_success_d68`
  (unchanged, passes).
- Two pools, failure with active work while the other pool prepares:
  `queue_failure_run::a_failed_queue_ends_the_run_with_its_cause_while_another_pool_is_alive_d68`
  (unchanged, passes; still ends before 10,000 ms with exactly the cause once).
- The shutdown race: new
  `pool_scheduler::a_queue_that_fails_while_its_pool_shuts_down_still_ends_the_run_d68`,
  virtual time, a gated Track (no sleeps). Asserts B's queue completed Failed,
  the run ended at 1,500 ms (the moment the Track failed), the outcome is
  `VendorError::Other` with exactly `track failed`, both pools ended, and A's
  batch was stopped (one batch, none retried).
- D52 racing the failure: the same test's second case cancels the Scheduler
  job and the run context (what `stop_execution` does) in the same moment the
  Track fails. The run ends at once with `Cancelled`, as any D52 cancel does.
- Late messages: there is no pool-side message any more. A pool that closes
  first is harmless; a failure after the Scheduler's join (a pool admitted
  after the join closed admission, D65) only cancels an already finished job.

**Reverts.** On `0b019b24` the new witness fails (state dump above). With the
fix, removing only the callback's cancel fails both the new witness and the
single-pool one; removing only the slot check fails both (the single-pool run
returns `Ok(true)`, the race returns a progress-derived result).

### S-x61-2 (low): wording, and the D69 extension

(a) The CHANGELOG said "a retry that is not run returns its quota", which
includes C47's dropped retry, whose charge is deliberately kept. It now names
which drops return quota (C45) and which keep it (C47). The D69 section of
`docs/deviations.md` and the PLAN row already said so; `docs/migration.md`
makes no quota claim.

(b) The incidental bug became C86 and is fixed under D69.
`handle_uncompleted_tests` notes whether the test is terminal before its
`test_ended`; a non-final INCOMPLETE then cannot change that. If it was, the
copy is not `rerun_test`-ed (so not queued and its worker not added to
`retry_workers`) and `remove_test(test, 1)` takes it out of the totals. Its
`test_ended` still runs once: one progress line and one Track, not final,
status INCOMPLETE, as before. A non-terminal test keeps today's behaviour.

The quota-exceeded branch (`final = true`) is unchanged: it never requeued.
For a test that is already terminal it still tracks the copy as a non-final
FAILURE and leaves it in the totals, as the Kotlin marathon does; changing
that is not in this brief.

Witnesses (both failed before the fix where they should):

- `queue_behaviour::a_late_incomplete_copy_of_a_finished_test_is_not_run_again_d69`
  — fast ANY_SUCCESS, two copies of C0: A1's passes and completes it, then
  B1's comes back INCOMPLETE, once as a returned batch and once as
  `uncompleted` in a completed batch. Exactly one Track `(C0, serial-B1,
  INCOMPLETE, false)`, `Passed{total 1, done 1, running -2}`, queue `[C1]`, no
  retry workers, `Notify { retry_workers: [] }`; C1 then runs and the queue
  terminates with quota untouched. Before the fix: `Notify { retry_workers:
  ["worker-B"] }` and C0 queued again.
- `queue_behaviour::an_incomplete_copy_of_an_unfinished_test_is_still_run_again_d69`
  — the counterpart: C0 comes back INCOMPLETE before it finished, is queued
  again with `worker-A` marked, and the rerun's pass completes it at
  `Passed{1, 1, -2}`. Passes before and after the fix.

Docs: PLAN D69 row and the list entry (C86), `docs/deviations.md` D69 (a fourth
bug and a fourth native rule; the "reports can hold fewer executions" line),
`docs/migration.md`, CHANGELOG, and the new C86 row in
`docs/known-kotlin-bugs.md`. It is a Kotlin bug, so it is not in
`docs/found-bugs.md`.

**Contract and goldens (PLAN §3.12).** Result data changes only by having
fewer executions for runs that hit C86, as for the rest of D69. No JVM queue
recording covers it: the full `queue_oracle` replay and the
`d69_supersedes_exactly_the_c45_to_c47_recordings` negative test pass
unchanged, and the registered native goldens (`differential_*` in
marathon-cli) pass unchanged, so there is no golden-diff note.

### Owner decision

- C86 takes the next free C number on `main`; another WP in the batch may
  claim the same number before this merges.

