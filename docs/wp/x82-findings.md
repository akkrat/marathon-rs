# WP x82: every unexpected actor or task exit produces a run outcome (D84, U206)

Base: main at 3e53e6a4 (0.14.0). D68 (U168) made a queue that fails stop every pool and end the run with its cause. A
read-only design review of 3e53e6a4 listed four more ways an internal failure could leave a run hanging, lose its cause
or run on beside the reports. Each was checked against the code before any change; all four hold, and checking (d)
found more sites of the same shape.

## What was verified in the code

- **(a) Actor handler cancellation** (`executor/actor.rs`). The receive loop's result is the receiver task's outcome.
  A handler returning `Cancelled` that the receiver was not given (its signal unset) completes the receiver
  `Cancelled`. A cancelled child does not fail its parent, so the owner job stays active, the mailbox is closed
  (`is_closed_for_send`), and nothing that waits on the owner hears of it. For the queue, D68's hook on the queue job
  never fires; the pool's sends fail (`send_to_queue` → false) and `request_batches` parks the devices
  (`pool.rs:694–699`), so the pool never ends. One reachable source: a `TestBundleIdentifier` that returns
  `VendorError::Cancelled` goes through `batching_error` → `vendor_error` as an `OperationError::Cancelled`.
- **(b) Scheduler** (`execution/scheduler.rs`). A pool whose handler panics fails its actor job; that propagates as
  `ChildFailed` to the Scheduler's job, which cancels every pool. `execute` then joined the pools without looking at
  their outcome and aggregated progress, so the run got `false` (or `true`) and no `Marathon run failed` cause. The
  device subscription runs under the run's context, not the Scheduler's job: a failed subscription fails the run task,
  `execute`'s `join_in` returns `Cancelled`, and the run finishes and writes reports while the detached pools and devices
  run on. Found while checking (b): a subscription ending on a cancellation it was not given (a cancelled device
  channel) completes `Cancelled`, which nobody hears of. With a pool still preparing a device the run then hangs (the
  witness below hangs without the fix).
- **(c) Run owner** (`marathon.rs`, `run_under`). With an empty result cell the outcome was built from the join/state
  match, whose fall-through mapped `Completed(Failed(cause))` to `requested("Marathon run was cancelled")`. The failure
  handler logged the panic as an unrecoverable task failure, but `Marathon run failed` named a cancellation.
- **(d) iOS** (`apple_simulator_device/prepare.rs`, `execution.rs`). Preparation, batch and disposal run in
  `RetainedTasks` tasks that publish to a `SharedDeferred` after awaiting their work. The vendor runtime catches a panic
  and completes the task's own result, but the caller waits on the deferred, which is never completed: the preparation's
  caller and disposal's `stop_and_join` (bounded) wait on `done`, the batch caller waits on `job.done` until cancelled,
  and every disposal caller waits on the disposal cell for ever. The device actor's own accounting on owner completion
  (`device/actor.rs:686–700`) is not involved: the core never hears that the call ended.

## Same shape elsewhere

Checked as the brief asked, by reading every `RetainedTasks::spawn`, `spawn_cancellable` and `SharedDeferred` publish
in `marathon-apple`, `marathon-ios`, `marathon-macos` and `marathon-android`:

| Site | Shape | Done |
|---|---|---|
| `marathon-apple/src/xcodebuild_session.rs` `produce` | Opener and consumer awaited, then `sender.finish` and `finished`; a panic leaves the reader and `close` waiting | fixed, witnessed by the macOS test (panic in `xcodebuild`) |
| `marathon-macos/src/device.rs` `execute` | `running.result.complete(outcome)` after `run` | fixed, same witness (the test panics on every command, so it hits both; each revert hangs) |
| `marathon-macos/src/device.rs` `dispose_inner` | cells completed after `join_cancelled` | fixed the same way; `join_cancelled` only logs and waits on a bounded timer, so no witness could make it panic; listed under "not verified" |
| `marathon-macos/src/device.rs` `prepare` | waits on the task's own `join`, which carries a panic | not affected |
| `marathon-ios/src/device/simulator_provider.rs` `process_update`, `dispatch`, `terminate` | `result.complete(work.await)` with an uncancellable `done.wait()` | fixed with `RetainedTasks::spawn_published`; the update is witnessed, `dispatch` and `terminate` share the helper |
| `marathon-apple/src/device/connection_factory.rs` `create_owned`, `marathon-ios/src/device/detached.rs`, `rsync_file_bridge.rs` `owned` | oneshot sender dropped by a panic: the caller gets an `IllegalState` "channel closed" error | no hang; the panic's message is lost. Incidental, not fixed |
| `marathon-android/src/device.rs` logcat, `runner.rs` instrumentation channel | watch flag never set / mpsc sender dropped | no hang (the logcat wait is bounded by `LOGCAT_END_WAIT_MILLIS`; a dropped sender ends the stream). Not the same defect |

## Design and invariants

1. **An actor ends with a cause whenever it ends on its own.** In the receiver's body, a `Cancelled` result while the
   receiver's own cancellation signal is unset becomes a failure: `<label> stopped: its handler was cancelled, but the
   actor was not`, with the cancellation's failure as its cause. The receiver then fails its owner as a panic does. A
   cancellation the actor was given (its owner's cancel, `close`, D52's stop, D68's stop, the pool's own
   `terminate_now`) reaches the receiver's signal synchronously in the cancel cascade, so it stays a cancellation;
   `runtime_review2::receiver_only_cancellation_interrupts_handler_delay_and_awaits_cleanup` and the new
   `a_receiver_cancelled_with_its_actor_stays_cancelled_d84` pin that. The device actor's handler never returns an error,
   and its tasks (e.g. `notify_is_ready`'s closed-channel cancellation) are not receivers, so device behaviour, the
   readiness oracle and D70 are unchanged.
2. **One first fatal cause per run.** `Scheduler::Inner::fatal` (D68's `queue_failure`, generalised) is set by the
   completion of a queue job, a pool actor job or the subscription task with `Completion::Failed`; the first one wins.
   It is never taken: `Scheduler::failure` reads it after the reports too (review round 1, P1). How the run stops is
   in invariant 3 (as revised in round 1). The subscription applies the actor's rule to itself: a cancellation its
   job was not given becomes `The device subscription stopped: it was cancelled, but the run was not`.
3. **The Scheduler stops in order, within a bound, and never returns a fatal cause while its pools run** (revised in
   review round 1). Recording the first failure logs it at once (ERROR, `The run's own machinery failed; stopping the
   run`), cancels the subscription and every device actor (the pool supervisors' children other than the queue), and
   cancels the job `execute` waits under, which ends its waits whatever they wait on. The pools and queues stay: a
   queue that replies to a closed pool ends on that send with its mailbox, so the pools must outlive the drain.
   `execute` then sends every live queue an `IsEmpty`, which a queue answers after every message delivered before it,
   cancels the Scheduler's job and joins it, all within `FAILURE_STOP_LIMIT_MILLIS` (D52's 24 s); past it, an ERROR is
   logged and the run finishes anyway. After `execute` has returned there is no waiter, and a failure cancels the
   Scheduler's job at once. A `StopOnUnwind` guard cancels the Scheduler's job if `execute` unwinds. With no fatal cause
   the result is unchanged, so a D52 cancel (which records nothing) takes its own path.
4. **The run owner reports a failed run task as a failure.** `run_under` maps an empty result cell with
   `Completed(Failed(cause))` to `VendorError::Other(cause)`; the ordinary failed-run path follows (onFinish, reports,
   `Marathon run failed` with the cause, exit 1 / 0 with `ignoreFailures`).
5. **A retained Apple task that publishes to a waiter turns a panic into its error first.**
   `marathon_apple::cmd::retained_tasks::panic_as_error` wraps the awaited work (catching a panic in `poll`, as the
   runtime does, with `marathon_core::executor::panic_failure`, now public, for the same failure), so the publication
   after it always runs. `RetainedTasks::spawn_published` is the common case (publish the work's outcome to a cell). The
   batch's panic becomes `RunError::from(VendorError::Other(..))`, which `classify` returns as an ordinary vendor error;
   the core then treats it as it treats a panic thrown straight out of `Device::execute` ("treating it as recoverable").
   A preparation's panic is retried by the core as any other preparation error (K2). A disposal's panic completes the
   disposal cell with the error, so every caller gets it, and is logged at ERROR as `Disposal failed`.

Not changed: D52 (signal path), D65 (completion order; `actor_receiver_seals_owner_and_supervisor_before_callbacks_d65`
still checks that both parents are sealed before the receiver's callbacks, now with the owner failed), D68 (the queue
path, now one of three sources), D74 (disposal stops and joins preparations within the cancelled-batch bound), D70
(device accounting). No configuration, timeout or exit code was added.

## Contract changes (PLAN §3.12)

Observable run outcomes change only for runs where one of these failures happens: a run that hung now exits 1 (0 with
`ignoreFailures`); a run whose pool panicked now exits through the failed-run path with the cause instead of returning a
result read from stopped pools (that result was already `false` in the witness, so the exit code there is the same, and
the cause is new); a run whose subscription failed now stops its pools before writing reports; an Apple panic is now a
vendor error the core already has a policy for. D84 records this (PLAN §3.11 row, `docs/deviations.md`,
`docs/migration.md`, `CHANGELOG.md` `## Unreleased`). Result files, worker commands and the pinned progress/summary
text are unchanged for healthy runs; no golden changed (no golden diff).

## Tests (witnesses)

Each was run with its fix reverted (patch saved, file checked out, test run, patch re-applied) and fails then:

| Witness | Without the fix |
|---|---|
| `marathon-core/tests/run_supervision.rs::a_queue_handler_cancelled_on_its_own_ends_the_run_with_the_cause_d84` | run does not end (actor.rs reverted) |
| `::a_pool_that_panics_ends_the_run_with_the_panic_d84` | no `Marathon run failed` line (scheduler.rs reverted) |
| `::a_failed_device_subscription_stops_the_pools_before_the_run_ends_d84` | `DevicePoolActor`, `QueueActor`, `DeviceActor` still alive after the run (scheduler.rs reverted) |
| `::a_cancelled_device_subscription_ends_the_run_with_the_cause_d84` | run does not end (subscription conversion disabled) |
| `::a_run_task_that_panics_fails_the_run_with_the_panic_d84` | cause reads `Marathon run was cancelled` (marathon.rs reverted) |
| `marathon-ios/tests/simulator_panic.rs` preparation, batch, disposal | each "did not end" (iOS device files reverted) |
| `simulator_panic.rs::a_provider_update_that_panics_ends_with_the_panic_d84` | "the update did not end" (simulator_provider.rs reverted) |
| `marathon-macos/tests/batch.rs::a_batch_that_panics_ends_with_the_panic_and_does_not_hold_disposal_d84` | times out with either `device.rs` or `xcodebuild_session.rs` reverted |

The run witnesses use the Stub vendor and the real scheduler through `Marathon.run` on virtual time; the 120 s scenario
end is the hang guard. Each asserts a bounded end (before the running test would have ended at 11 s), the mapped
outcome `false`, the exact cause chain of `Marathon run failed`, test1 `PASSED` in `raw.json` (the result committed
before the failure), the provider terminated, and no actor open for sends (`ControlledReplay::open_mailboxes`, new
test support, exposed as `scenario::Outcome::open_mailboxes`). The iOS tests run on paused time with a one-hour
`tokio::time::timeout` as the hang guard; the macOS test follows its file's real-time 60 s guards.

Changed tests: `runtime_review::cancelled_receiver_keeps_actor_owner_active` pinned the JVM probe's "owner stays active"
(the U206 behaviour) and is replaced by `a_receiver_cancelled_by_its_handler_fails_the_actor_owner_d84`, which still
reads the probe's values for the closed mailbox and the owner's children and asserts the owner now fails;
`actor_receiver_seals_owner_and_supervisor_before_callbacks_d65` expects the owner `Failed` instead of `Success`. Neither
has a parity-matrix row (they are review witnesses), so no row changes. The JVM oracle file is unchanged.

## What was run

- `cargo fmt --check`: clean.
- `cargo clippy -p marathon-core -p marathon-test-support -p marathon-apple -p marathon-ios -p marathon-macos --all-targets -- -D warnings`: clean.
- `cargo test -p marathon-core -p marathon-test-support -p marathon-apple -p marathon-ios -p marathon-macos --no-fail-fast`: all pass.
- `cargo test -p marathon-android -p marathon-cli --no-fail-fast` (Android and the CLI end-to-end/differential suites): all pass.
- The revert checks in the table above.
- Not run, per the batch's gate rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Not verified

- The macOS disposal's `join_cancelled` panic: fixed as the others, but no seam makes that bounded wait panic.
- The iOS provider's `dispatch` and `terminate` panics: they share `spawn_published` with the witnessed update, but have
  no witness of their own.
- A real panic inside `execute`: no seam reaches its code, so the unwind guard is witnessed by dropping `execute`'s
  future mid-run, which drops the same state an unwind does.
- Real devices: not run. The changes only act on a panic or an unexpected cancellation, which the farm runs do not
  produce on demand.
- (Superseded in round 2: a draining queue no longer replies to its pool, so a failed pool no longer ends its queue.)
- The pool's round-2 guards on `terminate_now` and `start_return_wait` (a handler already running when the stop begins)
  have no witness of their own: no scenario I could build loses a result through them, since a pool's own `IsEmpty` is
  answered after what was delivered before it. They make I2's "does not end itself, waits for no device" hold.
- The P4 witness calls `Scheduler::stopping`, which is new in round 2, so it cannot be run on 384f96de; there the guard
  only cancelled the tree, and nothing could wait for it.
- Android's cancelled-call bound: Android declares none (its cleanup after a cancel runs listener steps under an
  uncancellable signal, each ADB request bounded by its configured timeout, with no overall figure to derive), so a
  stopping run waits for an Android device until it ends, as D68 did. A vendor call that waited for the provider's
  termination would still hold such a run; no Android call does.
- The drain waits for each queue to handle what it holds; a queue's calls are bounded by D51 (tracker, 30 s per send)
  and D67 (console, 60 s), not by the stop itself.
- Other vendors' device monitors (Android, macOS) were not changed; the reviewer's P3 was about iOS, whose monitor
  also sets up the first simulators.

## Review round 2: the shutdown as one owned state machine

Round 2 (Sol 6.2) found the round-1 shutdown still had windows: a pool failure cancelled sibling queues through the
Scheduler job's ordinary propagation (R2-1), the 24 s deadline cut across vendor cleanup that D68 used to wait for
(R2-2), a draining pool could still admit a device and dispatch (R2-3), a cleanup error could replace a stored cause
(P1) and the unwind guard did not wait (P4). Instead of patching each window, the shutdown is now one state machine
owned by the Scheduler (`Inner::begin_stop` and the stop routine), and the invariants below are enforced where the
defect would occur, not by timing.

**States.** `Running` → `Stopping` (the routine runs) → `Stopped` (the routine's job has completed). `begin_stop` is the
only transition out of `Running`; it is synchronous and idempotent. It is entered by the first fatal cause (a queue, a
pool or the subscription ending with an error), by `execute` unwinding (P4), or by a fatal cause after `execute` has
returned (P1, a late pool). A D52 signal does not enter it; D52 keeps its own path and deadline.

- **I1 First cause wins.** `fatal` is written once, by the first fatal cause, and is the run's error: `execute` returns
  it, `run_async` returns it after `on_finish` whatever `on_finish` returned (a cleanup or termination error is then
  logged, never returned), and a later fatal cause is logged at WARN and dropped.
- **I2 Admission closes first.** The first thing `begin_stop` does is set the draining flag of the Scheduler, every
  pool and every queue. Each is read by the handler that would act: a draining pool's handler forwards a device's
  completed results to its queue and answers the drain barrier, and drops everything else (no `AddDevice`, no device
  actor, no `ExecuteBatch`, no request, no return wait, no self-termination); a draining queue hands out no batch or
  retry and sends nothing to its pool; the Scheduler creates no pool. A message buffered before the flag is still
  handled under it, so no ordering race can admit work.
- **I3 Delivered results are drained in every pool before any pool or queue receiver is cancelled.** The Scheduler's
  job is a supervisor, so a failed pool or queue no longer cancels its siblings through ordinary propagation; only the
  routine cancels. A draining queue never replies to its pool, so a failed pool cannot end its queue (R2-1). The routine
  sends each live pool a barrier, then each live queue an `IsEmpty`, each answered after everything delivered before
  it; only then does it cancel the Scheduler's job. What can still be lost is stated in D84: a queue or pool whose own
  handler failed (a panic, a D68 error) loses what is left in its own mailbox, and a result still inside a failed pool's
  mailbox never reaches its queue.
- **I4 No live device work while the provider terminates, within the vendors' own bounds.** `begin_stop` cancels every
  device actor (stopping their work as a D52 cancel does), and the routine joins each one within that device's own
  bound for ending a cancelled call, `Device::cancellation_cleanup_millis`: iOS `cancelled_batch_millis` (70 s by
  default, the bound a cancelled batch is joined within, which also covers its session's stop), macOS
  `CANCELLED_BATCH_TIMEOUT` (70 s), plus 5 s for the core's own handling after it. Android and the Stub vendor declare
  no overall bound (each ADB request of a cancelled batch's cleanup is bounded by its own configured timeout), and are
  joined until they end, as D68 joined them. A device still running past its bound is logged at ERROR and left; that is
  the only work that can still be live when the provider terminates. The bound exists only to break a call that waits
  for the provider's termination (P5). The unwind guard and a late failure run the same routine; the run owner joins it
  before `on_finish`, so the same rule holds on those paths (P4).
- **I5 iOS monitoring.** A panic in the monitor, or an error when the provider has no device a run could use, fails
  the provider's device channel with the cause. An ordinary error while devices are connected is logged at ERROR and
  the run carries on with them, as before D84.

## Gate (remote Linux): the round-5 signal witness made event-driven

The gate failed `run_supervision::a_signal_while_the_run_settles_keeps_the_reports_first_d84` on Linux. Cause: the
witness held the run through a Graphite backend's `device_connected`, but the analytics backend sits behind a
`RemoteTracker` queue that forwards only test results ("The production remote trackers only implement test
metrics"), so no device event ever reached it and nothing held the settle. On macOS the round-5 witness passed for
another reason (the backend's 5 s hold of its test-result queue made the analytics close wait), and that timing
depended on the backend's worker thread. A run's listener cannot in fact be held by a backend.

The witness now holds the settle where it really waits, in the stop routine, and orders everything by events:
the console starts a late pool (a new model's device) when it is given the output only the finish flushes ("terminate
pool modelA"), and holds that flush until the late device's batch is running; the settle then stops that pool, whose
cancelled batch raises SIGTERM and cleans up until the signal's reports have started and 4 s more. Assertions are
unchanged in strength: the provider is terminated only after the reports end, exit 143 within 24 s of the signal's
reports. With the round-5 hand-over checks disabled (the 9f744841 finish), it fails: provider terminated at 15,000 ms
while the reports ran to 17,500 ms.

Audit of the other timing-scripted D84 witnesses: they order virtual timers only (device prepare delays, stub test
times, bounds), which the replay driver advances in order since vendor calls are tracked work; none depends on a
backend's worker thread. One other test used the Graphite backend: `the_reports_hold_the_results_a_stop_drains_d84`
claimed the backend held the queue's Track, which it cannot; its assertions held for another reason (the drain). The
backend is removed from it and its comment corrected; the held-queue drain is witnessed in `pool_scheduler`, where the
tracker can be held.

## Review round 6 (Sol 6.2 delta): per-finding status

| Item | Status | Fix | Witness |
|---|---|---|---|
| 1: the parse deadline discarded a cause published during the borrow's sleep | fixed | the iOS borrow, when cancelled without a simulator, returns the cause published by then; `DeviceProvider::published_failure` (default none; iOS reports its monitoring's cause) is read when the remote parse's deadline ends without a device, in place of NoDevices | `simulator_panic::a_borrow_ended_by_its_deadline_ends_with_the_published_failure_d84` (sleep from 1,000 ms, publication at 1,020 ms, deadline at 1,050 ms; fails on 9d891389); run level `run_supervision::a_parse_deadline_ends_with_the_providers_published_failure_d84` (needs the new provider hook, so compile-only on 9d891389) |
| 2: the cached cause aborted a parse that could succeed | fixed | the borrow no longer returns early on the cached cause; it keeps polling as before D84 | `simulator_panic::a_borrow_still_takes_a_simulator_registered_after_a_published_failure_d84` (fails on 9d891389) |
| D84 text | fixed | the remote-parse sentence (deviations, PLAN row) | — |

## Review round 5 (Sol 6.2, REQUEST_CHANGES): per-finding status

| Item | Status | Fix | Witness (fails on 9f744841) |
|---|---|---|---|
| 1: a signal during settle let the run's finish start analytics' close and the provider's termination beside D52's reports | fixed | `on_finish` checks `signalled` before every step it would start (after the first flush, after the settle, before analytics, before the provider, after the channel read, before reports and usage) and hands over at once; a step already running when the signal came runs on, and D52's finish waits for it as before; D52's 24 s and exit 128 + signal unchanged | `run_supervision::a_signal_while_the_run_settles_keeps_the_reports_first_d84` (settle waits on a Graphite backend's Track of a late connection, which raises SIGTERM at 11,500 ms and ends at 15,500 ms, inside the reports' head start; reports take 6.5 s; the provider must be terminated only after they end; exit 143 within 24 s) |
| 2: an iOS channel failure lost to NoDevices during remote parsing | fixed | the iOS provider keeps the cause its monitoring published to the channel; a borrow with no simulator available returns it at once. Chosen over reading the channel without a Scheduler because the cause is the provider's own, and the borrow is the only place that waits before a Scheduler exists | `simulator_panic::a_borrow_ends_with_the_monitorings_failure_when_no_simulator_is_available_d84`; run-level pin (passes on 9f744841, the core already maps the borrow's error): `run_supervision::a_monitoring_failure_a_remote_parse_hears_of_is_the_runs_cause_d84` (exit 0 with `ignoreFailures`, the cause in `Marathon run failed`) |
| D84 text | fixed | signal hand-over, pre-Scheduler scope, admission after the first console flush, a disconnect already read during the settle, a late pool's batch cancelled at the settle, direct `run_async` callers | — |

## Review round 4 (Sol 6.2 MAX, REQUEST_CHANGES): per-finding status

The orchestrator split the scope: F1, F2, F3 and F6 are fixed here; F4 and F5 move to a follow-up WP (below).

| Item | Status | Fix | Witness (fails on 8a9e7c1b) |
|---|---|---|---|
| F1 + two decision points | fixed | One decision point, in `run_under` after the (protected, resumable) finish: the first-cause cell beats any stored outcome, a NoDevices or a finish error, which is then logged ("The run also failed after its first failure"); `run_async` no longer decides. The finish reads how the provider's channel ended after termination (`settle_after_termination`): a failure, or a cancellation nothing in the run asked for, goes through `fail` | `run_supervision::a_device_channel_cancelled_while_the_provider_terminates_fails_the_run_d84`; `a_channel_failure_waiting_at_the_no_device_timeout_is_the_runs_cause_d84` (with `ignoreFailures`, exit 0 for the real cause where NoDevices gave false); `a_channel_failure_the_listener_still_holds_at_the_end_fails_the_run_d84` now fails if the decision point is removed (checked) |
| F2 | fixed | `on_finish`: settle → analytics → provider → channel end → reports → usage; a step's own error or cancellation is logged and the next steps run; only a requested cancellation stops the finish, and `run_under`'s protected finish then resumes it (`finish_completed`; a signal's finish still makes it a no-op) | `run_supervision::a_cancelled_provider_termination_still_writes_the_reports_d84` |
| F3 | fixed | `settle` runs first in the finish, so no failure or device work overlaps analytics' close or the provider's termination; it stops a pool if its actor or its supervisor is still running (a device of an ended pool still cleaning up) | `pool_scheduler::settling_waits_for_a_device_its_closed_pool_still_owns_d84` |
| F6 | fixed, documented | `settle` closes admission (`draining`) before it cancels the listener, and `on_device_connected` returns at once when draining, before and after the history prefetch, so a connection read after that reaches no pool; the drain of late pools stays and is deterministic (always before the reports); D84 lists the healthy-run changes | `pool_scheduler::a_connection_the_listener_reads_after_the_run_settles_reaches_no_pool_d84` (the listener is held in one connection's Track when the run settles; the next connection must reach no pool). Behaviour pins that also pass on 8a9e7c1b: `a_connection_waiting_when_the_run_settles_reaches_no_pool_d84`, `run_supervision::a_device_connecting_while_the_run_finishes_is_not_admitted_d84` |
| P3 (macOS sleep) | fixed | the lease witness waits on a clock gate that fires when termination begins its bounded preparation wait | `provider::termination_does_not_release_a_lease_under_a_running_preparation_d84` |
| D84 text | fixed | single decision point, settle-first finish, healthy-run changes, known limits (Android, macOS signal exit, U210, U211) | — |

Tests rewritten because the behaviour they pinned is gone by design: the round-1 "late pool fails while the provider
terminates" witness becomes `a_device_connecting_while_the_run_finishes_is_not_admitted_d84` (no late pool can start
during the finish now); the round-2 I1 witness now has the channel fail during termination and the termination fail
too; the round-3 R3-1b witness becomes `the_reports_hold_the_results_a_stop_drains_d84` (the drain of a stop that a
channel failure begins mid-run).

Goldens: the run-flow goldens and the differential suites (`marathon-cli` tests) pass unchanged; no golden diff.

Checked in a scratch worktree at 8a9e7c1b with the new test files: the F1, F2, F3 and F6 witnesses above fail there
(the I1 test fails there only on its new log line). With the decision point removed from the new code,
`a_channel_failure_the_listener_still_holds_at_the_end_fails_the_run_d84` and the other F1 witnesses fail.
Also fixed: the round-1 witness `a_result_delivered_to_the_queue_before_a_failure_is_still_tracked_d84` assumed which
device takes which test; it now checks one tracked result per device (it failed once in the full run).

## Out of scope / follow-ups (from round 4, for a new WP)

- **F4 → U210, Apple retained preparation after disposal.** macOS: `prepare` (`crates/marathon-macos/src/device.rs:646`)
  accepts work after the Mac's disposal; a late Mac can begin preparing after `terminate` has disposed of it and
  `join_preparation` found nothing, and the lease is released mid-installation (`crates/marathon-macos/src/provider.rs:431`).
  Scenario: a device connects during termination, its preparation starts after disposal. iOS: cancelling during the
  initial bundle push drops the retry helper's wait on the retained rsync task (`crates/marathon-core/src/executor/retry.rs:50`,
  `crates/marathon-apple/src/cmd/remote/rsync/rsync_file_bridge.rs:125`), so the preparation can report its end while
  rsync is still stopping.
- **F5 → U211, vendor monitor failures bypass supervision.** Android: a failing `device_provider_init` of the tracker
  (`crates/marathon-android/src/provider.rs:352`) ends device tracking and leaves the channel open; with a batch held
  behind the D22 outage gate on another server, the run can wait indefinitely. iOS: a panic in the static
  Marathondevices loader's log callback (`crates/marathon-ios/src/device/simulator_provider/source.rs:29`) is recorded
  by the blocking task, but initialization waits on the cell it never completed before it joins the task.

## Review round 3 (Sol 6.2 xhigh, REQUEST_CHANGES): per-finding status

Orchestrator decisions applied: the outcome and the report snapshot have a single authority (the first-cause cell,
read after the stop and the listener have ended, and the stop joined before the reports); I4 extends to Apple
preparations a cancel leaves running; iOS counts available simulators; Android keeps the unbounded join, stated in D84
as a known limit.

| Item | Status | Commit | Fix | Witness (fails on a5c09afd) |
|---|---|---|---|---|
| R3-1a: success stored before the listener read a channel failure | fixed | `e407d2d2` | `Scheduler::settle`, called in `on_finish` after the provider step and before the reports, cancels and joins the listener and reads what the provider's channel ended with (a failure there goes through `fail`); `run_under` decides the outcome after the run task and the stop from the first-cause cell, so a stored `Ok` cannot win | `run_supervision::a_channel_failure_the_listener_still_holds_at_the_end_fails_the_run_d84` (the listener is held in a late device's history prefetch; on a5c09afd the run never ends) |
| R3-1b: reports written before a late failure's drain | fixed | `e407d2d2` | `settle` also stops and drains pools still running and joins the stop before the reports; after it nothing of the Scheduler runs, so no failure can follow the snapshot | `run_supervision::the_reports_wait_for_a_late_failures_drain_d84` (pool modelY's Track held 5 s with a second result buffered, pool modelZ panics during termination; on a5c09afd serial-5's result is missing) |
| R3-2: Apple preparation outlives the joined actor; macOS lease released under it | fixed | `e407d2d2`, `b392bb6d` | `Device::join_cancelled_work`, awaited by the stop within the device's bound after its actor; iOS reuses D74's `stop_and_join` (stops and waits for preparations and a batch within `cancelled_batch_millis`); macOS records when its retained preparation ends and `join_preparation` waits for it within `cancelled_batch_timeout`; macOS termination and replacement call it before releasing a lease, logging WARN past the bound | `run_supervision::the_run_waits_for_a_preparation_left_running_before_terminating_the_provider_d84`; macOS `provider::termination_does_not_release_a_lease_under_a_running_preparation_d84` and `…stops_waiting_for_a_preparation_after_the_macs_bound_d84`; iOS `simulator_dispose::the_stop_waits_for_a_preparation_its_cancelled_caller_left_running_d84` (new API, so compile-only on a5c09afd) |
| R3-3: iOS counted retained simulators as usable | fixed | `b392bb6d` | the monitor's no-device test uses `borrowable()` (connected, not retired, leaving, removed, exhausted, recovering or parked) | `simulator_panic::a_monitor_error_after_the_last_simulator_left_fails_the_device_channel_d84` |
| Android unbounded join | kept | `this commit` | stated in D84 as a known limit | — |
| D84 text | fixed | this commit | PLAN, deviations, migration, CHANGELOG: outcome decided after the listener and stop; reports wait for the settle; preparations and the Mac lease; available simulators; Android limit | — |

Behaviour notes: `settle` cancels the device listener after the provider has terminated, so device events still
buffered then are dropped (they would have started pools against a terminated provider); its read of the channel's end
keeps a failure there. A late pool still running when the run settles is stopped and drained, where it used to run on,
detached, beside the reports. Run after round 3: `cargo test --no-fail-fast` on core, test-support, apple, ios, macos,
android and cli, all pass; clippy `-D warnings` and `cargo fmt --check` clean.

## Review round 2 (Sol 6.2, REQUEST_CHANGES): per-finding status

| Item | Status | Commit | Witness (fails on 384f96de) |
|---|---|---|---|
| P1 / I1: a finish error replaced the stored cause | fixed | `fb59853f` | `run_supervision::a_cleanup_error_after_the_first_failure_does_not_replace_it_d84` |
| R2-1 / I3: a pool failure cancelled sibling queues; a reply to the failed pool ended its queue | fixed | `fb59853f` | `pool_scheduler::a_pool_failure_leaves_every_queue_the_results_delivered_to_it_d84` (both pools hold a Track with a buffered result; pool A panics) |
| R2-2 / I4: the 24 s deadline cut vendor cleanup | fixed | `fb59853f`, `d476a606` | `run_supervision::a_cleanup_within_the_devices_own_bound_is_waited_for_d84` (30 s cleanup, 60 s device bound); `a_device_call_waiting_for_the_runs_end_holds_it_only_for_its_own_bound_d84` (P5 still broken, at the device's bound) |
| R2-3 / I2: a draining pool could admit a device | fixed | `fb59853f` | `pool_scheduler::a_device_added_after_the_failure_is_not_started_d84` (AddDevice buffered behind a busy pool B while pool A's drain is held) |
| P4 / I4: the unwind guard did not wait | fixed | `fb59853f` | `pool_scheduler::the_stop_an_unwind_begins_waits_for_a_slow_cleanup_d84` (uses the new `Scheduler::stopping`; see "Not verified") |
| I5: a mid-run iOS monitor error newly failed a run that could finish | fixed | `d476a606` | `simulator_panic::a_monitor_error_with_a_device_connected_lets_the_run_carry_on_d84`; also `a_monitor_error_with_no_device_fails_the_device_channel_d84` |
| D84 text overstated | fixed | this docs commit | PLAN row, deviations section (what is preserved, the bound, what can be lost), migration, CHANGELOG |

The round-1 verified list still holds: every earlier witness passes (the round-1 P5 witness was renamed and now expects
the device's own bound, 30 s + 5 s, instead of 24 s). Run: `cargo test --no-fail-fast` on core, test-support, apple,
ios, macos, android and cli, all pass; clippy `-D warnings` on the changed crates and `cargo fmt --check` clean.

## Review round 1 (Sol 6.2, REQUEST_CHANGES): per-finding status

| # | Finding | Status | Fix | Witness (fails without the fix) |
|---|---|---|---|---|
| P1 | A pool that fails after `execute` consumed the fatal slot leaves the run green | fixed, `6a847dcb` | `fatal` is never taken; `Marathon::run_async` reads `Scheduler::failure` after `on_finish`; a failure with no waiter cancels the tree at once | `run_supervision::a_pool_that_fails_while_the_run_finishes_fails_the_run_d84` (a device connects during the provider's termination; round 1 returned `Ok(true)`) |
| P2 | Fatal shutdown discards completed results already in a queue's mailbox | fixed, `6a847dcb` | devices and subscription stop first, pools and queues stay; each queue answers an `IsEmpty` after what was delivered to it, then the tree is cancelled. Closing the pools first was tried and failed the witness: the queue's reply to a closed pool ended it with its mailbox | `pool_scheduler::a_result_delivered_to_the_queue_before_a_failure_is_still_tracked_d84` (B's result waits behind A's held Track; round 1 never tracked it) |
| P3 | The iOS monitor's panic is lost; the run ends NoDevices | fixed, `28b2d23a` | the monitor runs under `panic_as_error`, and a monitor ending with any non-cancellation error logs it and closes the device channel with it, which the subscription turns into the run's failure | `simulator_panic::a_monitor_that_panics_fails_the_device_channel_d84` (the channel never ended) |
| P4 | An unwind out of `execute` leaves the detached pools running | fixed, `6a847dcb` | `StopOnUnwind` guard in `execute`, disarmed on return | `pool_scheduler::a_scheduler_that_stops_before_it_returns_stops_its_pools_d84` (drops `execute` mid-batch; round 1 left the pool running) |
| P5 | The protected join can hold the first failure for ever | fixed, `6a847dcb` | both options: the cause is logged when it is recorded, and the stop is bounded by D52's 24 s. The bound is needed, not only the log: a call waiting for the provider's termination deadlocks against a join that precedes it, with or without a signal | `run_supervision::a_device_call_waiting_for_the_runs_end_holds_it_only_for_a_limited_time_d84` (round 1 never ended; now it ends at 3 s + 24 s with both ERROR lines) |
| P6 | A panic caught outside cleanup leaves listeners and the session behind | fixed, `28b2d23a` | `InstalledLines` drop guard for the batch's line listeners; `execute_request` catches a panic around the run's work (and around `after_test_run`, `test_run_failed`/`cancelled`), so `after_test_run`, the channel's cancel and the session's stop run as for any error | `simulator_panic::a_batch_that_panics_…` now asserts no line listener is left (fails without the guard); `a_listener_that_panics_still_lets_the_batch_clean_up_d84` (a listener panics on a pass; without the runner fix the log listener `after_test_run` removes stays installed) |
| P7 | The tests prove mailbox closure, not task completion | fixed, `6a847dcb` | witness only | `run_supervision::the_run_waits_for_a_cancelled_batch_to_clean_up_before_it_finishes_d84`: a cancelled batch cleans up for 3 s ignoring cancellation; the provider's termination must see it done (fails with the join removed) |

Re-run after round 1: `cargo test -p marathon-core -p marathon-test-support -p marathon-apple -p marathon-ios
-p marathon-macos -p marathon-android -p marathon-cli --no-fail-fast` all pass; clippy `-D warnings` on the changed
crates and `cargo fmt --check` clean. The round-1 verified list still holds (all its witnesses pass).

## Incidental bugs

- `connection_factory.rs` `create_owned`, `detached.rs`, `rsync_file_bridge.rs` `owned`: a panic in the retained work
  drops the oneshot sender, and the caller gets `IllegalState: channel closed` (or the oneshot's error text) instead of
  the panic's message. No hang; the cause is lost from the error, though the vendor runtime still records the task's
  panic for `join_pending`.
- Android `start_logcat`: a panic in the logcat task leaves `logcat_ended` false, so `resume_after_server_return` does
  not restart logcat after an adb server return. Bounded; logcat lines are lost for the rest of the run.
- The macOS `dispose` logs a non-fatal disposal error, a caught panic included, only at DEBUG.
