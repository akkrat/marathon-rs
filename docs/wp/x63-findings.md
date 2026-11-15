# WP x63: an adb outage does not charge or break tests that never started

Base: `7090b228` (0.13.0). Approved 2026-11-15 by the project owner. Deviation D70 (U160, U166).

## Design

### Where the charge came from

Verified against the code before changing it. A device held at D22's gate or D41's hold keeps its actor. When the
server or the grace is given up, the provider marks the device disconnected and sends `Disconnected`; the pool removes
the device and its actor takes the `Terminate` transition while the vendor's `execute` is still waiting at the gate. The
core's `account_for` then seals the batch's result before the vendor has reported anything, so it sends
`ReturnTestBatch`, and the queue's `on_return_batch` charges every test one uncompleted attempt (U160). The same path
returned the whole of a D28 batch given up between instrumentations, so tests that had finished ran again. The runner
waited for the gate only once per batch, in `AndroidDevice::execute`, so a later instrumentation started on the dead
server or the offline device (U166). The ledger's line numbers had drifted; the descriptions were right.

### Core (minimal; what was touched)

- `vendor.rs`: `Device::withdraw_unstarted(&BatchRef) -> bool`, default `false`, so iOS, macOS and every test double keep
  their behaviour. Synchronous, no I/O, called on the core thread.
- `execution/device/actor.rs`: `account_for` takes `leaving`. Only on the `Terminate` transition (the device leaving),
  and only while the batch's result is not in, it asks the device. On `true` it seals the result and sends the new
  `FromDevice::RejectTestBatch`. On `false` the usual accounting follows, which also picks up a result the vendor
  completed inside the call. The `NotifyIsReady` path (a device that stays) never asks, so a live device that keeps
  failing before it starts is charged as before and cannot loop (brief, point 2).
- `execution/pool_message.rs`: the `RejectTestBatch` variant.
- `execution/pool.rs`: `device_rejected_test_batch` releases the assignment, sends the existing
  `QueueMessage::RejectBatch` and awaits the outcome, as `device_returned_test_batch` does with `ReturnBatch`. It does not
  touch D48's state (`departed`, `away_since`, `return_wait`): only `device_completed` with a finished or failed test
  renews the wait, so a rejected batch cannot.
- `execution/result.rs`: `TemporalResults` derives `Clone` (for the Android snapshot below).
- **`queue.rs` is not touched.** The reject goes through the existing `on_reject_batch`, so x61's C47 change to it
  (skipping terminal tests) applies to D70's rejects too with no merge interaction.

### Android

- `AndroidDevice` keeps one `BatchRecord` (a device runs one batch at a time): the batch, its result cell and a stage,
  `Unstarted`, `Started`, `Parked(results)`, `Withdrawn` or `Done`, all changed under one mutex.
- `execute` records the batch first (`take_batch`), then waits at the gate, then refuses a batch withdrawn meanwhile, and
  marks it `Done` when the executor returns.
- The runner calls `record_submission` immediately before `InstrumentationChannel::start` (and before an all-ignored
  batch is accounted for without an instrumentation). It is refused once the core has withdrawn the batch. This is the
  explicit record the brief asks for; test callbacks are not used to infer it.
- `withdraw_unstarted`: `Unstarted` (or a batch `execute` has not got yet, which is recorded as `Withdrawn` so `execute`
  refuses it when it arrives) gives `true`. `Parked(results)` completes the batch's result with `results` and gives
  `false`, so the core reports those results. `Started` and `Done` give `false`.
- U166: the runner calls `wait_for_next_instrumentation` at the top of every D28 partition, before any device command of
  that instrumentation and outside its `testBatchTimeoutMillis`. It returns at once when the gate is open and the device
  not given up, so a healthy run is unchanged. Otherwise, if an instrumentation of the batch has been sent, it takes the
  listener's results so far (`AndroidTestRunListener::results_so_far`, given by `TestResultsListener` from a copy of its
  accumulated results, as merging parameterised results mutates them) and parks with them. The wait is cancellable
  (D52); on an error after an earlier instrumentation ran it closes the batch's run with `after_test_run`, as the loop's
  other errors do.
- Both gates (`pass_gate`) wait on, until cancelled, for a device that was given up. Every provider path opens the gate
  only after the disconnect has been handled, which sets the device's `lost` flag first (`BlipOver`, `finish_blips`,
  the outage's `None` branch). Without this, a batch released by the given-up path could pass the gate, run listener
  setup and race the core's `Terminate` to its submission; the first whole-run witness showed exactly that (a stray
  `uncompleted = ` line after the reject).

### Departure from the brief, and why

Point 1 says a batch partly run keeps "today's accounting for the started part". Today's accounting on the given-up
path returned the whole batch, which reruns the finished part and contradicts the witness "completed part not repeated".
D70 instead ends such a batch with what ran: finished and failed tests count, and the tests of the instrumentations that
never started are uncompleted and charged once, exactly what a device loss there reports when the vendor sees it first
(D28). Giving those unstarted tests back uncharged as well would need a queue message that requeues part of a batch
uncharged next to a completion; that is a queue change overlapping x61, and is left as an owner decision.

## Invariants

1. Exactly one outcome per batch: the core decides once, in `account_for`, under the device's batch mutex; a submission
   and a withdrawal are ordered by the same mutex, so either the instrumentation is sent and the batch is charged as
   before, or it is withdrawn and nothing of it is sent.
2. A rejected batch is uncharged and not a test that ended: D48's bound and the retry quota bound every loop. A device
   that stays never rejects.
3. A healthy run (gate always open, no device given up) sends the same commands in the same order and logs the same
   lines; the only cost is a watch read per instrumentation.
4. Once an instrumentation was sent, the batch is accounted as before (charged), even if the device left a moment later.

## Verified, and how

Each fix's witness was run with the fix temporarily reverted and failed:

| Witness | Revert | Result without |
|---|---|---|
| `pool_scheduler::a_batch_its_device_never_started_goes_back_uncharged_d70` (quota 0) | core reject branch off | run failed, quota exceeded |
| `pool_scheduler::a_device_that_keeps_leaving_before_it_starts_cannot_hold_the_run_open_d70` | same | batches returned and charged |
| `outage_batch_test::a_batch_given_back_before_it_starts_never_starts_d70` (submission vs withdrawal, four orders) | submission ignores a withdrawal | fails |
| `outage_batch_test::each_instrumentation_of_a_batch_waits_for_the_device_d70` | per-instrumentation gate off | second `am instrument` sent while offline |
| `outage_batch_test::a_batch_given_up_between_instrumentations_keeps_what_the_first_gave_d70` | gate off; parked result not completed | fails both ways |
| `outage_batch_test::a_cancel_ends_an_instrumentation_waiting_at_the_gate_d70` | gate off | fails |
| `reconnect_test::a_batch_held_through_a_grace_that_ends_in_a_disconnect_is_not_charged_d70` (D41, quota 0) | Android withdrawal always `false` | run fails (`Ok(false)`) |
| `reconnect_test::a_batch_held_by_a_server_that_is_given_up_is_not_charged_d70` (D22 given up, quota 3) | same | batch returned, charged |

The whole-run witnesses use the virtual-clock reconnect harness. A new option holds a scripted outage or state change
until a batch on that server is in `VirtualDuration`'s four seconds, which come before the runner sends anything, so the
batch caught is provably unsubmitted (docs/testing.md rule 4); without it the event could land while an instrumentation
was in flight. They assert that the run passes, exactly one `onRejectBatch`, and no `onReturnBatch`, `uncompleted = `
or `uncompletedTestRetryQuota exceeded` line. The device-level tests wait for the runner's call to `results_so_far`
(made only at a closed gate) instead of sleeping, and the grace is opened from the listener handling the first
instrumentation's `TestRunEnded`, on the runner's own task, so the runner cannot pass the gate first.

Run: `cargo fmt --check`; `cargo clippy -p marathon-core -p marathon-android --all-targets -- -D warnings`;
`cargo test -p marathon-core -p marathon-android -p marathon-cli` (see the final report for results).

Worker command transcripts: no registered differential or golden scenario has an adb outage, a grace period or a
mixed-instrumentation batch caught by one (`tools/differential/registry.json`, the android-reconnect oracle scenarios),
and the reconnect oracle tests (`matches_kotlin`) pass unchanged. With the gate open nothing changes, so no comparator
allowance was needed.

## Not verified

- No real emulator or real adb server outage; fake adb only.
- The window between an instrumentation's end and the per-instrumentation wait (invariant 4) is not exercised; there the
  batch is accounted as before.
- iOS and macOS are untouched (the trait default); their suites were not run beyond what `marathon-cli` covers.

## Golden diffs

None.

## Incidental bugs

- A device lost while its runner was between instrumentations but not at a gate, or during an instrumentation, can still
  have its whole batch returned by the core's `Terminate` before the vendor reports, so finished tests of that batch run
  again. This is the existing Terminate-first race (Kotlin's too), now narrowed to the moments outside the gates; not
  fixed here.
