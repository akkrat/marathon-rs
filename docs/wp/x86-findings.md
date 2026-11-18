# x86 — vendor supervision follow-ups of D84 (U210, U211, D88)

D84 made every internal failure end the run with its cause, after a bounded stop that waits for cancelled device work.
Its review left vendor shapes outside it: an Apple preparation that outlives what the stop waits for (U210) and vendor
device monitors whose failure never reaches the run (U211). This package closes both, decides the D84 integration
question left by x85 (a second wait at termination), and checks one Android acknowledgement path from x83. One
observable outcome changes, approved as D88 (2026-11-18, project owner's decision): Android device monitoring that ends
fails the run.

## The evidence, re-verified

Checked against d5b0181d.

1. **macOS, preparation after disposal (U210).** `MacosDevice::prepare` (`crates/marathon-macos/src/device.rs`) stored its
   preparation cell without looking at `disposal`, while `execute` refused a disposed Mac. `AppleMacosProvider::terminate`
   disposes of each Mac, then calls `join_preparation`, which reads the cell once, then releases the lease. A `prepare`
   that starts after that read runs its whole installation on a Mac whose lease is released. Confirmed.
2. **iOS, the bundle send (U210).** The Apple installer sends the test bundle inside `with_retry_send_gated`
   (`crates/marathon-core/src/executor/retry.rs`), which raced each attempt against the cancellation and dropped the
   attempt when it fired. The rsync bridge runs a send in a retained task (`RsyncFileBridge::owned`), which goes on when
   its awaiter is dropped and needs its own time to stop the process (`STOP_STEP_MILLIS` steps). The preparation then
   ended, completed its `done` cell, and every wait for it (`stop_and_join`, used by the stop's `join_cancelled_work` and
   by disposal) ended while the transfer was still stopping. Confirmed.
3. **Android, the tracker in tracking (U211).** In `AdamDeviceProvider::monitor`, `device_provider_init(...).await?`
   ended the monitor on an error. The monitor's task logged `Task stopped due to unrecoverable error` and nothing else:
   the device channel stayed open. A panic in the monitor, or in one server's tracking task, was not caught either (a
   panicking server task just dropped its sender; with one server the monitor then ended with `Ok`). Confirmed. D51 says
   analytics failures never stop a run. In production a tracker fails only on a cancellation or a panic: D51's
   `BackendGuard` absorbs backend errors, so the Android tracker case is rare. The general shape (any monitor end leaves
   the channel open) is not.
4. **iOS, the static Marathondevices loader (U211).** `source::blocking` waited on the result cell before joining the
   blocking task. A panic in the work (the loader logs `Using static device provider` through the run's sink) is caught
   by the runtime as the task's error but never completes the cell, so initialization waited for ever. Confirmed.
5. **The second wait at termination (D84 follow-up from x85).** After the stop's wait for a device runs out (its bound
   plus 5 s), the provider's termination disposes of the device and waits again within the same bound: on iOS
   `stop_and_join` before the on-dispose lifecycle, on macOS `join_cancelled` for a running batch and `join_preparation`
   before the lease release. Confirmed; see the decision below.
6. **Android, a replacement dropped with its acknowledgement (x83).** In the monitor's `Tracked::Replace` handling, a
   replacement for a serial that the tracker does not have in the `device` state is dropped with `continue`, and with it
   its `replaced` sender; the recovery task then gets `None` and `finish_replacement` returns without releasing the
   device's D22/D41 wait. Unreachable; see below.

## Design

**1. A disposed Mac refuses a preparation.** `prepare` now registers its preparation under the `execution` lock that
`dispose_inner` installs `disposal` with, and refuses when `disposal` is set. Disposal precedes `join_preparation` in
termination and in a replacement, so every preparation a later `join_preparation` could miss is one that never started:
the snapshot is complete by construction. The refusal is a cancellation (`Device <serial> has been disposed`), as iOS
refuses a disposed simulator's preparation (U179). The first version returned `DeviceLost`, as `execute` does; the
self-review found that the device actor retries that up to 30 times 10 s apart, each refused at once, so it became a
cancellation, which the actor does not retry.

**2. The installer's retry waits for its attempt.** `with_retry_send_gated` no longer races an attempt against the
cancellation. The body is given the same signal and ends on it (every step of the installer's body takes the context:
the directory commands, the shared-transfer wait, the send's `TransferStop`), so whatever it started has ended when the
helper returns. If the signal fired during the attempt, the helper returns the cancellation, as before, so its callers
see the same results; the installer still reports a stall (`LeavePool`) recorded by the attempt over the cancellation.
`with_retry_send` keeps racing: its one caller, the analytics prefetch, has a body that takes no cancellation. The
preparation's `done` cell now completes after the transfer has stopped, so the stop's `join_cancelled_work` and
disposal wait for the transfer within their existing bound (D74's 70 s); the preparation's caller still returns at once.
macOS runs its installation in a retained task with a context of its own that is never cancelled, so it is unchanged.

**3. Android monitoring (D88).** A tracker error is logged at WARN and tracking goes on; only the monitor's own
cancellation ends it there. The monitor task catches a panic, and each server's tracking task does too and sends it to
the monitor as an error. When the monitor ends with an error and its own signal was not cancelled (only the provider's
termination cancels it), the device channel is closed with the cause. Under D84 that is the run's cause: the run stops,
its reports are written, and it exits 1 (0 with `ignoreFailures`). The channel's close is first-wins, so a close after
termination's `close(None)` changes nothing.

**4. The iOS loader.** `blocking` joins the task first and then reads the cell; a panic is the task's error. A cell that
is somehow empty after a successful join is an `IllegalState` error, never a wait. The iOS test runtime's
`spawn_blocking` now reports a panic as the task's error, as the run's runtime (`marathon_core::executor`) does; before,
a panic there left the test task unjoinable.

**5. Decision: both waits stay.** The two waits guard different actions. The stop's wait guards the run's finish (the
reports, then the provider's termination); disposal's guards what disposal does next: an iOS simulator's on-dispose
lifecycle (terminating or shutting down the simulator), a Mac's lease release. The Kotlin marathon has the same pair for a
cancelled batch: `execute` waits for it within the cancelled-batch bound and gives up, and disposal waits again before
its lifecycle; its tests pin it (`AppleSimulatorDeviceDisposeTest` "a cancelled batch that has not finished in time takes
the simulator out of the run, and disposal still waits for it", `MacosDeviceBatchTest` "…is not waited for any longer, and
disposal still waits for it"; ported as `simulator_dispose.rs` and `batch.rs`). A single deadline per piece of stopped
work was implemented and tried: it made disposal start the on-dispose lifecycle under the still-running batch, and the
ported Kotlin test failed (`gate.raced`). It was withdrawn. What it costs: a failing run whose device outlives its bound
can exit up to one more bound (70 s by default) later, and only then. D84's known-limits text now says so.

**6. Unreachable, so unchanged (comment added).** A replacement with an acknowledgement comes only from `recovered`,
run by the device's server tracking task, which then awaits the answer and sends nothing more until it has it. The
tracker's state for that server changes only through the `Devices` lists that task sends (`trackers.update` in the
monitor). The recovery counted (`Recoveries::finish` returned it as current), so no list since it started showed the
device as anything but `device`: `offer` cancels (`obsolete`) the recovery of any serial a list does not show as
`device`, before it sends that list, and an outage abandons them all. The last list the monitor applied before the
`Replace` therefore had the device as `device` (a device in a grace period or a settle window is shown as `device` too),
so the branch never sees an acknowledgement. Replacements that reach it otherwise (`retry_failed_prepares`,
`finish_outage`) carry none. A comment at the branch says so.

## Invariants

- A Mac's preparation either is registered before its disposal, and so seen by every `join_preparation` after it, or
  never starts.
- An Apple preparation's `done` cell is completed only after the bundle send it started has ended.
- Android device monitoring ends only by the provider's termination, or with the device channel failed with its cause.
- An analytics call never ends Android device tracking.
- iOS initialization never waits for a blocking result that a panic dropped.

## Verification

Each witness fails on d5b0181d, checked in a scratch worktree of d5b0181d with only the new tests (and the test
runtime's change) copied in:

| Item | Witness | On d5b0181d |
|---|---|---|
| 1 | `marathon-macos/tests/provider.rs::a_preparation_after_the_macs_disposal_starts_nothing_u210` | the preparation was accepted |
| 2 | `marathon-ios/tests/simulator_dispose.rs::the_stop_waits_for_the_bundle_send_a_cancelled_preparation_was_making_u210` (a send whose own task takes 3 s to stop; virtual time) | the stop's wait ended before the send |
| 3 | `marathon-android/tests/provider_test.rs::a_failing_tracker_does_not_end_device_tracking_u211` (fake adb) | the later disconnect was never seen (60 s guard) |
| 3 | `marathon-android/tests/provider_test.rs::device_monitoring_that_ends_fails_the_device_channel_u211` (fake adb; a panicking tracker) | the channel never failed (60 s guard) |
| 4 | `marathon-ios/tests/simulator_panic.rs::a_panic_while_reading_the_devices_file_ends_the_initialization_with_it_u211` (virtual time) | initialization never ended (guard) |

Stress: each witness 50 runs in a row, all passed (4 × 50), and 20 more each with 12 busy processes on the CPU, all
passed (4 × 20).

Run on this Mac: `cargo fmt --check`; `cargo clippy -p marathon-core -p marathon-apple -p marathon-ios -p marathon-macos
-p marathon-android --all-targets -- -D warnings`; `cargo test --no-fail-fast` of those five crates and of
`marathon-cli` and `marathon-test-support` (the end-to-end and differential suites), all passing;
`scripts/check_plan_dag.py`. Not run, by the batch's rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test
--workspace`. Not verified: a real rsync transfer stopped mid-preparation (the witness models the retained task), a
real adb server whose tracking fails, the Linux gate.

## Golden diffs

None. No result file, worker command or pinned text changes; no oracle or differential recording is touched. D88
changes the exit status of a run whose Android device monitoring ends, which no golden or recording has.

## Incidental bugs

- **U212** (recorded in `docs/found-bugs.md`): on macOS a failing `device_provider_init` fails the Mac's connection and
  with it the provider's initialization; on iOS a failing `device_preparing` (in `Track::prepare`) fails a preparation
  that succeeded, which the actor then retries. Against D51, as Android's case was. Reached only by a panicking tracker
  or a replay clock error. Not changed here.
