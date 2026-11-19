# x92 — one Apple shutdown window per device; disposal only after confirmed termination (D93)

D84 gave a stopping run a bound per device for its cancelled call (the vendor's 70 s plus the core's 5 s), and D84's own
known limit said what remained: when the provider then disposed of an Apple device, the disposal waited again with a
window of its own, and afterwards ran the iOS on-dispose lifecycle, or released the Mac's lease, whatever that second
wait found. The design (Astra 6.2, section 4) asks for one window per device, started at the stop, shared by every wait,
and for the lifecycle and the lease release only when the device's work is confirmed to have ended. Approved as D93
(2026-11-18, project owner's decision); amends D74 and D84.

## The evidence, re-verified (on 3d945010)

1. **Three windows.** `Scheduler::stop_routine` (`crates/marathon-core/src/execution/scheduler.rs` ~600) waited for each
   device's job and then `join_cancelled_work`, within `cancellation_cleanup_millis + 5 s` from the routine's start, one
   device after the other. The iOS batch call's `classify` → `join_cancelled` → `stop_and_join` had its own
   `cancelled_batch_millis` (execution.rs ~308); `join_cancelled_work` another (mod.rs ~359); `dispose_and_join` another
   (execution.rs ~435) before `handle_lifecycle` (~421 in the design's numbering). Mac: `execute` → `join_cancelled` (70 s),
   `dispose_inner` → `join_cancelled` (70 s), the provider's `terminate` → `join_preparation` (70 s) → `release`
   (provider.rs ~429). Confirmed: one stuck batch costs 75 s in the Scheduler and 70 s more in disposal.
2. **Lifecycle and release regardless.** iOS ran the lifecycle after `stop_and_join` whatever it found (only a WARN);
   the Mac provider released the lease after `join_preparation`'s WARN. Confirmed.
3. **Sequential joins.** `stop_routine` called `join_cancelled_work` for device 2 only after device 1's wait: a device's
   leftover preparation was not even stopped until its predecessor in the loop was done. Confirmed.
4. **Mac sends** were not registered (x90's note), and a Mac's preparation ran under a context nothing cancelled.
   Confirmed.
5. **Tests.** The two ported "second wait" tests (`a_cancelled_batch_that_has_not_finished_in_time_…_disposal_still_waits_for_it`
   on iOS and Mac) cancel a direct caller and dispose directly; neither involves the Scheduler. The Mac virtual-time test
   `default_budgets_and_fresh_disposal_wait_use_virtual_time` pins two full windows for direct callers. The ported iOS
   `disposal_stops_waiting_for_a_batch_that_has_not_finished_in_time` ran the lifecycle while the batch was still blocked
   (row 755): it conflicts with the invariant, and the owner accepted replacing it.

## Design

**Seal (core).** `Device::seal_for_stop` (new, default no-op; core thread, no I/O). `begin_stop` now collects the device
actors, seals every device, and only then cancels the actors, so a device's window has started before any cancel reaches
it. `stop_routine` joins all devices concurrently (`join_all`), each within its bound from the routine's start, so one
device's wait no longer delays another's stop.

**One window (iOS and Mac).** Sealing sets `shutdown_by = now + cancelled_batch_millis` on the vendor clock, once, under
the lock that admission and disposal take; from then on the device refuses a batch, preparation or send as a disposed one
does (`Device <serial> is stopping`). Every wait passes that one deadline: the batch call's `join_cancelled` (when
sealed), `join_cancelled_work` and disposal. A device the run's stop did not seal (a provider-only device, a replaced
Mac) is sealed by its first disposal. An unsealed batch call (a direct caller, outside a stopping run) keeps its own
window, so the Kotlin-ported second-wait tests and the Mac two-windows test hold unchanged.

**One receipt.** `finish_work(batch, deadline)` reads, under the lock, the batches (the one a cancelled call ran and the
current one), the preparations, the sends not ended (D91's receipts; Macs now register theirs exactly as iOS does), and,
live, the xcodebuild sessions that were told to stop; it stops them all (batch signal, preparation signal, `TransferReceipt::stop`
with the deadline), waits until `deadline − reserve`, then, if anything is left, stops a session whose producer still
runs and waits to the deadline. The reserve is a fourteenth of the window (5 s of 70). It returns
`Termination::Confirmed` only if every batch and preparation ended, every send ended `TransferCleanup::Confirmed`, and every
stopping session's producer ended with its command closed (`XcodebuildSession::ended() == Some(true)`); otherwise
`Termination::Unconfirmed(what)`.

**Unconfirmed.** Disposal logs one WARN naming the device and what was left, skips the on-dispose lifecycle, revokes the
device's commands (`DeviceCommands`: every command of the device goes through it, built in the factory/provider so `bin`
uses it too; a revoked device's command fails before reaching the worker, or returns its caller's cancellation if it has
one), retires it and marks it `quarantined`. The providers then call `quarantine(&executor)` instead of `release`
(`ProvisioningConnections`, `MacosConnections`; default: keep). `ConnectionFactory::quarantine` keeps the device's use (so
the connection never reaches zero users and never closes under that work) and, for a shared connection that is the
worker's current one, makes it not current, so the next acquisition connects anew; siblings keep theirs (D90).

**Mac preparation.** Runs under its own stop signal (it still goes on after its cancelled caller returns, D84); the Mac's
`finish_work` stops it. Before, nothing could stop it.

## Invariants

- From the run's stop, every wait for an Apple device's work ends at one deadline per device, set before any of its
  actors is cancelled; the Scheduler adds its 5 s margin. No disposal or `join_cancelled_work` starts a second window for
  a sealed device.
- The on-dispose lifecycle runs, and a lease is released, only after `Termination::Confirmed`.
- A device whose work was not confirmed sends no further command through itself and its lease is never closed or handed
  out again in the run; other users of a shared connection are unaffected.
- A sealed device starts no new batch, preparation or send (checked under the lock the seal takes).

## Departures from the brief

- **The forced phase** stops an xcodebuild session whose producer still runs; there is no "forced local task
  cancellation" beyond the stop signals already given at the start of the window. `VendorTask::cancel` is cooperative and
  there is no abort; aborting a task would not make its end any more confirmed (a dropped transfer owner would even end a
  receipt `Confirmed`), so the reserve gives the session's close its time instead.
- **Sessions in the receipt** are those told to stop (every batch's, by its runner at the end of the batch). A session
  nobody stopped (a parse's) is its owner's: recovery's own 70 s wait for it (`simulator_admission.rs`
  `recovery_waits_70_seconds_for_an_open_parsing_session_…`) keeps its timing.
- **iOS lease quarantine** is applied where termination releases a simulator's lease (the map and the replaced handles).
  The provisioner's release after a failed setup is unchanged: a simulator whose setup failed has no batch, preparation,
  send or session, so its disposal is always confirmed.
- **Witness shape.** "Stub scheduler" is read as: the core's own Scheduler with Stub devices for the seal order and the
  concurrent joins (`run_supervision.rs`), and a test driver that makes the Scheduler's calls in its order on real iOS
  simulators (fake worker) and a real Mac provider (macOS fake), in virtual time.

## Verification

| Witness | What it shows | On 3d945010 |
|---|---|---|
| `marathon-ios/tests/simulator_dispose.rs::one_window_from_the_runs_stop_and_lifecycle_only_after_confirmed_termination_d93` (paused time; two simulators; seal both, cancel both, join both together, dispose both; `a` released at 69 s, `b` held to 80 s) | joins end at 70 s and disposal adds nothing (total 70 s ≤ 75 s, no second window); lifecycle only for `a`; one WARN, naming `b`; `b` quarantined; `b`'s batch, released later, sends nothing | fails: 140 s (seal calls removed to build) |
| `simulator_dispose.rs::disposal_that_cannot_confirm_the_batch_ended_skips_the_on_dispose_lifecycle_d93` (replaces row 755's test) | direct disposal ends at its 300 ms bound, no lifecycle, one WARN, quarantined, no later command | fails: lifecycle ran (1) |
| `marathon-macos/tests/provider.rs::a_mac_whose_work_outlives_the_runs_one_window_keeps_its_lease_d93` (paused; session close never returns) | call + `join_cancelled_work` end at 70 s; termination adds nothing; 0 releases, 1 quarantine, 1 WARN | fails: the call alone ended at 60 s and termination took another window |
| `provider.rs::a_healthy_mac_is_released_once_at_termination_d93` | healthy: 1 release, 0 quarantines, no WARN | passes (unchanged-behaviour witness) |
| `marathon-macos/tests/batch.rs::disposal_stops_and_waits_for_a_send_whose_caller_stopped_waiting_d93` (paused) | Mac disposal waits 3 s for a send whose caller was aborted | fails: disposed before the send ended |
| `batch.rs::preparation_continues_after_its_caller_is_cancelled_until_disposal_stops_it_d93` (renamed from `preparation_continues_after_its_caller_is_cancelled`) | disposal stops the preparation and waits for it; it sends nothing afterwards | (Rust-only test; pinned the old behaviour) |
| `marathon-apple/tests/connection_factory.rs::a_quarantined_lease_keeps_the_shared_connection_open_for_its_sibling_and_out_of_reuse_d93` | sibling keeps the connection, its release does not close it, the next acquisition gets a new connection | not buildable (new API) |
| `marathon-core/tests/run_supervision.rs::the_stop_seals_every_device_before_it_cancels_any_d93` | both devices sealed before any cancel | fails: no seal (hook removed to build) |
| `run_supervision.rs::the_stop_waits_for_its_devices_together_d93` | two devices with 40 s of leftover work: run done at 43 s, no device left | fails: `serial-3 did not stop within 75000 ms` |

Changed existing tests (Rust-only unless said): `simulator_dispose.rs::disposal_stops_waiting_for_a_preparation_that_has_not_ended_in_time_u179`
(the one disposal WARN replaces the preparation WARN); `provider.rs::termination_does_not_release_a_lease_under_a_running_preparation_d84`
(the stopped preparation ends with its stop); `termination_stops_waiting_for_a_preparation_after_the_macs_bound_d84` →
`…_and_keeps_its_lease_d93` (quarantine, not release); `provider.rs`'s `NotifyJoin` recognises the disposal's wait by its
length before the reserve (65 s). The Kotlin-ported Mac `default_budgets_and_fresh_disposal_wait_use_virtual_time` (two
windows for direct callers, two WARNs with `not waiting for it any longer` and `within 70 s,`) passes unchanged.

**Parity.** Row 755 (`AppleSimulatorDeviceDisposeTest`, `disposal stops waiting for a batch that has not finished in time`)
→ `n/a`, `x92: D93 (…)`, by the owner's decision; **reviewer agreement needed**. `scripts/parity_matrix.py` needs no
exception: it carries 5.2 rows over from the CSV, and a regeneration keeps the row as written.

Stress, each witness alone (`--exact`); "loaded" with 12 busy processes:

| Witness | Runs | Loaded |
|---|---|---|
| the nine above | 50/50 each | 20/20 each |

Run on this Mac: `cargo fmt --check`; `cargo clippy -p marathon-apple -p marathon-ios -p marathon-macos -p marathon-core
--all-targets -- -D warnings`; `cargo test --no-fail-fast -p marathon-apple -p marathon-ios -p marathon-macos -p
marathon-core -p marathon-cli -p marathon-test-support` (2,107 passed, 0 failed, 34 ignored; built the YAML helper
first); `scripts/check_plan_dag.py`; `scripts/parity_matrix.py --check` (fails identically on 3d945010: rows of the
Kotlin checkout that it no longer finds, unrelated). Not run, by the batch's rule: `scripts/check.sh`,
`scripts/remote-check.sh`, `cargo test --workspace`.

## Not verified

Real devices: a stuck xcodebuild close on a real worker, a quarantined shared SSH connection with a sibling simulator
running (the post-merge acceptance). The Linux gate. A Scheduler run with the real iOS provider end to end in virtual
time (the witness drives the Scheduler's calls itself).

## Golden diffs

None. No result file, worker command of a healthy run, pinned text or exit code changes; no oracle recording is touched.

## Incidental bugs

None new (no U219). Noted, out of scope: the iOS provider's termination drains artifacts (`close_and_join(300_000,
60_000)`) before it disposes of the simulators, so a failing run's exit can still wait up to 360 s there (bounded, but
outside D93's window; x93's barrier work is the place). `parity_matrix.py --check` reports rows "no longer found" against
the Kotlin checkout on this machine, on the base too.

## Review round 1 (Sol 6.2 xhigh, REQUEST_CHANGES: 6 P2, 1 P3; parity row 755 `n/a` agreed)

Orchestrator decisions A–F, applied on top of 20002c62.

| Finding | Change | Witness (fails on 20002c62) |
|---|---|---|
| 1 (A) disposal resampled the receipt: a send unconfirmed at 40 s, or a batch ending after 70 s, read as confirmed | One verdict per sealed device (`ShutdownWindow`, `crates/marathon-apple/src/cmd/device_commands.rs`): decided once, by one task spawned at the seal, on the receipt taken at the seal (batches, preparations, and every send not `Confirmed`, also one that ended `Unconfirmed` earlier, which `send_stop` now keeps), when all of it has ended or at the window's end. Every caller (cancelled call, `join_cancelled_work`, disposal, provider, recovery) stops the work and reads the latch. | `simulator_dispose.rs::a_send_unconfirmed_during_the_stop_keeps_the_simulator_from_its_lifecycle_d93` (lifecycle ran: 1); `work_that_ends_after_the_window_does_not_turn_the_verdict_confirmed_d93` (lifecycle ran: 1) |
| 2 (B) local sends ignored the seal and dropped the receipt before copying | `JvmFileBridge::send_with`: a stop already given (seal, disposal, caller) starts nothing; the copy owns the `TransferStop` in its blocking task, so the receipt ends when the copy has ended; the copy checks the stop between files and every 1 MiB. | `a_local_send_after_the_seal_copies_nothing_d93` (destination written); `a_local_copy_in_progress_keeps_its_receipt_until_it_ends_d93` (a FIFO source blocks the copy in its open; the test acts once the destination's parent folder exists, which the copy creates after its last look at the stop; not quarantined on the base) |
| 3 (A) an error while confirming released the Mac's lease | An error or panic while deciding latches `Unconfirmed` (`ShutdownWindow::fail`); disposal quarantines and still returns the error (a fatal one still ends the run); a sealed batch call returns it too. A disposal that panics quarantines (iOS too). | `provider.rs::a_fatal_disposal_quarantines_the_lease_and_a_repeated_termination_releases_nothing_d93` (renamed from `a_fatal_disposal_still_releases_the_lease_…_d74`; releases: 1 on the base) |
| 4 (C) the factory learned of a quarantine only at termination | `DeviceCommands::on_quarantine`: the provider gives each device its factory's quarantine (iOS at `register`, Mac at setup); `DeviceCommands::quarantine` applies it when disposal decides, once. The provider no longer releases a quarantined device's lease; siblings keep theirs (D90 witness from round 0). | `a_disposal_quarantines_the_lease_when_it_decides_to_d93` (needs the hook API, so not buildable on the base) |
| 5 (D) a confirmed simulator's lifecycle (and the release wait) could run past 75 s | After the run's stop (`by_run_stop`), the on-dispose lifecycle runs within what is left of the window, with the reserve (5 s) as a floor; cut there, the simulator is quarantined with one WARN. The providers' wait for a release is held to what is left (the close goes on in the factory's task). D93 now states the worst case: 80 s from the stop, the iOS artifact drain (x93) aside. Disposal outside a stop is unchanged (its own commands' limits). | `the_on_dispose_lifecycle_after_the_runs_stop_is_held_to_the_window_d93` (80 s on the base, 75 s now) |
| 6 (E) the Mac's reserve missed a batch session still running | A Mac batch's sessions are recorded on its `BatchJob` (a task-local set around the batch's run, read in `execute_test_request`; a parse's sessions are not); the receipt waits for them and the reserve stops them. | `batch.rs::the_macs_stop_stops_a_batch_session_while_the_batch_is_stuck_in_its_pull_d93` (session not stopped within 69 s on the base; at 65 s now) |
| 7 (F) D74/D84 texts | D74: leases released at most once, quarantined when D93 says so. D84: the Mac's preparation is stopped and an unconfirmed lease quarantined. | docs |

**Departure from D's preferred form.** Holding the lifecycle strictly to what is left of the window would quarantine a
confirmed simulator whenever a sibling held the stop to the window's end (disposal only starts after the stop routine):
the two-simulator witness's `a` would lose its lifecycle. The floor of one reserve (5 s) avoids that and keeps the bound
honest (80 s). Outside a run's stop (recovery, removal, a healthy run's termination) the lifecycle and the release are
not held to the window: a recovery harness with a 200 ms window cut every lifecycle
(`simulator_preparation_episode.rs::an_old_recovery_out_of_time_…_d87` hung), and those paths are not covered by the
published bound.

Other changed tests (Rust-only): `provider.rs::a_replacement_whose_predecessor_fails_to_dispose_is_announced_and_its_lease_quarantined_a107_d93`
(was `…_and_released_once_a107`).

Not done: a provider-level iOS witness for the hook (a simulator removed by an update while stuck, and a later
acquisition on the same worker); the device-level witness and the round-0 factory witness cover the two halves.

**Round 1 verification.** Each witness above was run against 20002c62 in a scratch worktree with the new tests copied in
(the hook witness left out, as it needs the new API): each fails as listed. Stress, each alone (`--exact`): the eight
round-1 witnesses and the two round-0 witnesses they lean on (`one_window_…`, `a_mac_whose_work_outlives_…`) 50/50 each,
and 20/20 each with 12 busy processes. A first form of the FIFO witness acted when a non-blocking writer could open the
FIFO; the disposal's stop could then land between the copy's open and its first read, the copy ended correctly, and the
witness failed spuriously (2 of 3 runs). The witness now waits for an event after the copy's last look at its stop.

Run: `cargo fmt --check`; `cargo clippy -p marathon-apple -p marathon-ios -p marathon-macos -p marathon-core -p
marathon-cli -p marathon-test-support --all-targets -- -D warnings`; `cargo test --no-fail-fast` of those crates: 2,092
passed, 0 failed, 34 ignored, with `simulator_dispose` (whose first FIFO witness hung there) run on its own after the fix,
22 passed, 20 times in a row. Not run, by the batch's rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test
--workspace`.

## Review round 2 (Sol 6.2 xhigh re-check at 58288ef2: 2, 3, 4, 6, 7 closed; 1 and 5 open, one new)

Owner decisions confirmed: the 5 s lifecycle floor stays; disposal outside a run's stop stays outside the bound. Rule
applied throughout: a timeout or an error anywhere in confirming yields only `Unconfirmed`, and nothing after a deadline
is awaited.

| Finding | Change | Witness (fails on 58288ef2) |
|---|---|---|
| 1 (was 1) the decider discarded its final wait's timeout and looked again | `await_receipt` (iOS and Mac): the verdict is the waits' outcome; a wait that expired latches `Unconfirmed` with no further look, and `wait_for`'s own look counts only before the deadline. What was left is still listed for the WARN. | `simulator_dispose.rs::a_wait_that_expired_is_unconfirmed_whatever_ends_after_it_d93` (lifecycle ran: 1); `provider.rs::a_macs_wait_that_expired_is_unconfirmed_whatever_ends_after_it_d93` (lease released: 1). A clock hands an expired wait back 2 s late: expiry at 70 s, the work ends at 71 s, the decider resumes at 72 s. |
| 2 (was 5) the lifecycle's timeout cancelled and then awaited a command deaf to cancellation | `run_on_dispose` races the lifecycle against the bound and drops it there (then cancels its signal and quarantines); no `with_timeout_or_null`, which joins its body. | `a_lifecycle_command_deaf_to_its_cancellation_is_abandoned_at_the_bound_d93` (80 s on the base, 75 s now) |
| 3 (new) a failed exec read as a closed session | `XcodebuildSession` records an open failure (any error from the opener; a session cancelled before it opened anything has none); `ended()` is `Some(false)` with one, so the device's verdict is unconfirmed. `closed()` and `check_closed` are unchanged. | `a_session_whose_open_failed_is_not_confirmed_closed_d93` (lifecycle ran: 1). The reviewer's reproduction used the real SSH executor against the fake worker; this witness makes the fake worker's exec fail. |

Behaviour to note: a batch whose xcodebuild could not be opened now makes the device's disposal unconfirmed (no
lifecycle, lease quarantined), outside a run's stop too; recorded in D93's limits.

**Round 2 verification.** Each witness above fails on 58288ef2 (scratch worktree, new tests copied in). Stress, each alone:
50/50, and 20/20 with 12 busy processes, for all four. `cargo fmt --check`; `cargo clippy` of apple, ios, macos, core, cli
and test-support with `-D warnings`; `cargo test --no-fail-fast` of those crates: 2,118 passed, 0 failed, 34 ignored. Not
run, by the batch's rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.
