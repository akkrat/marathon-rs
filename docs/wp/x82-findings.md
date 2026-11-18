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
   completion of a queue job, a pool actor job or the subscription task with `Completion::Failed`; the first one wins and
   each cancels the Scheduler's job, as D68 did. The subscription applies the actor's rule to itself: a cancellation its
   job was not given becomes `The device subscription stopped: it was cancelled, but the run was not`.
3. **The Scheduler never returns a fatal cause while its pools run.** `execute` checks `fatal` after the inner wait ends,
   whatever ended it (a subscription failure cancels the run's context and with it every `join_in`), cancels its job and
   waits for it (a protected `join`: the context is already cancelled), then returns `Err(Other(cause))`. The wait is
   D68's: the pools, queues and devices stop as at a cancel, and device calls end as their vendors honour cancellation.
   With no fatal cause the result is unchanged, so a D52 cancel (which records nothing) takes its own path.
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
- A panic in the run task while the Scheduler is running (inside `execute` itself): the cause is now kept (c), but the
  pools and devices are not stopped before onFinish then; no seam reaches code inside `execute`, so I did not add an
  unwitnessed stop there.
- Real devices: not run. The changes only act on a panic or an unexpected cancellation, which the farm runs do not
  produce on demand.
- The stop-and-join after a fatal cause is bounded only as D68's is: by the vendors honouring cancellation (`call_vendor`
  joins its vendor task). A device call that ignores cancellation holds it, as it holds a D68 stop or a normal drain.

## Incidental bugs

- `connection_factory.rs` `create_owned`, `detached.rs`, `rsync_file_bridge.rs` `owned`: a panic in the retained work
  drops the oneshot sender, and the caller gets `IllegalState: channel closed` (or the oneshot's error text) instead of
  the panic's message. No hang; the cause is lost from the error, though the vendor runtime still records the task's
  panic for `join_pending`.
- Android `start_logcat`: a panic in the logcat task leaves `logcat_ended` false, so `resume_after_server_return` does
  not restart logcat after an adb server return. Bounded; logcat lines are lost for the rest of the run.
- The macOS `dispose` logs a non-fatal disposal error, a caught panic included, only at DEBUG.
