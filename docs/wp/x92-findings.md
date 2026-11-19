# x92 — one Apple shutdown window per device; disposal only after confirmed termination (D93)

D84 gave a stopping run a bound per device for its cancelled call (the vendor's 70 s plus the core's 5 s), and D84's own
known limit said what remained: when the provider then disposed of an Apple device, the disposal waited again with a
window of its own, and afterwards ran the iOS on-dispose lifecycle, or released the Mac's lease, whatever that second
wait found. The design (Astra 6.2, section 4) asks for one window per device, started at the stop, shared by every wait,
and for the lifecycle and the lease release only when the device's work is confirmed to have ended. Approved as D93
(2026-11-18, project owner's decision); amends D74 and D84.

## Design note (review round 7): one gate, one cause

Rounds 5 and 6 closed the same race piece by piece (session registration, a registered-but-unstarted session, the
refusal's error type), each time at a different place. Round 7 replaces the pieces with two structural rules.

1. **One gate, closed by the seal.** Every command an Apple device sends goes through its `DeviceCommands`; the seal
   closes it, in the same critical section that records the seal (the device's state lock), and the gate checks it at
   the moment each command executes. Whatever registered before the seal and starts after it (a session's producer, an
   installer's retry, a listener's command) finds the gate closed and sends nothing; it is a refusal known to have
   started nothing. The seal also stops, at once and synchronously, what the device registered before it: its batch and
   preparation signals, its sends' receipts (so rsync or a local copy that has not started never starts), and its
   sessions that have not begun to open. Only the device's own on-dispose lifecycle and the work of the batch it ran when sealed (its
   cleanup and results, which the window waits for) pass the gate, inside scopes given to them; a quarantine (revoked
   commands) closes even that. (Refined while implementing; see review round 7 below.)
2. **The seal records its cause, and only the cause classifies a refusal.** `RunStop` (the Scheduler's stop, a D84
   failure, the owner's cancel, a D52 signal) refuses with a cancellation: orderly, no retry, no ERROR, no change to the
   exit code. `Disposal` (a disposal outside a run's stop: devices-file removal, D44 recovery, a replaced Mac, the
   provider's termination of a device the run's stop did not seal) refuses with a device loss: discovery takes another
   device (D53), a batch is retried as for a lost device. The caller's own cancellation state no longer decides anything:
   the "caller not cancelled" discriminator is gone, and a cancelled batch's call on a sealed device returns what the
   cause says.

Unchanged: the sticky verdict latch, the per-device list of sessions not confirmed ended, a pre-exec refusal counting as
confirmed, the window and its bounds.

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

## Review round 3 (Sol 6.2 re-check at 661e4d88: round 2 closed; two P2s from the declared behaviour change)

| Finding | Change | Witness (fails on 661e4d88) |
|---|---|---|
| 1 a transient channel refusal quarantined and retired a simulator, defeating D44, and could pin both D90 generations | `cmd::exec_outcome`: the SSH executor reports, in the caller's task, whether an execute failed knowing that nothing reached a shell (cancelled before it began, `start_session` failed: channel refused or no reply to the open, or the exec request refused); `DeviceCommands` reports its own refusals so. `XcodebuildSession` observes it around its opener: such a failure is not an open failure, so the session ends cleanly and the verdict can be confirmed (lifecycle, release, D44 as before). An exec sent with no reply, or a transport that died after it, stays unconfirmed. An executor that reports nothing (the local one, test fakes) leaves the outcome unknown: the safe side. | `simulator_world.rs::a_session_refused_before_its_exec_is_confirmed_ended_over_ssh_d93` (real SSH executor, fake worker with both `MaxSessions` slots held: `ended()` was `Some(false)`); `simulator_dispose.rs::a_session_refused_before_its_exec_lets_the_disposal_confirm_d93` (device level; needs the new report, so not buildable on the base) |
| 2 dropping a lifecycle command at the bound skipped its SSH cleanup | The lifecycle runs in a retained task of the simulator's (`spawn_published`); at the bound the disposal stops waiting, cancels it and quarantines, and returns; the command goes on to its executor's own bounded cleanup (D30's close; U154's CLOSE after the exec's 30 s), so its session slot is given back. | `a_lifecycle_command_cut_at_the_bound_is_left_to_its_own_cleanup_d93` (the held command reaches its end after the disposal returned at 75 s; on the base it was dropped and never ended) |

The reviewer's full chain (refusals on two generations, then recovery reprovisioning the simulator and a third
acquisition) is not reproduced end to end: the recovery harness uses scripted connections, not the factory over SSH.
The SSH witness covers the classification, the device witness the disposal that follows it, and the round-0 factory
witness that a release (not a quarantine) frees the lease.

**Round 3 verification.** On 661e4d88 (scratch worktree, new tests copied in): the SSH witness gives `Some(false)`, the
lifecycle witness finds the command dropped; the device-level refusal witness needs the new report and is not buildable
there. A first form of the SSH witness stopped the session before its opener ran (so the open was never tried) and
passed on the base; it now waits for the open's failure to reach the reader first. Stress, each alone: the three new
witnesses and the two round-2 witnesses whose mechanism changed, 50/50 each and 20/20 each with 12 busy processes.
`cargo fmt --check`; `cargo clippy` of apple, ios, macos, core, cli and test-support with `-D warnings`; `cargo test
--no-fail-fast` of those crates: 2,121 passed, 0 failed, 34 ignored. Not run, by the batch's rule: `scripts/check.sh`,
`scripts/remote-check.sh`, `cargo test --workspace`.

## Rebase onto main (1f4fd7cd: x89, x90 final, x91)

One code conflict, in `Scheduler::begin_stop`/`stop_routine` (`crates/marathon-core/src/execution/scheduler.rs`). Both
now hold: every device is sealed (`seal_for_stop`, D93) and has its vendor cleanup started (`begin_stop`, D92, spawned as
before) before any actor is cancelled; then all actors are cancelled; then all devices are joined together, each within
its own bound from the stop: the actor's job, then D92's cleanup job if the vendor returned one, otherwise
`join_cancelled_work` (D93's receipt for an Apple device). Apple devices keep the default `begin_stop` (`None`), so they
take the receipt path; an Android device always returns its cleanup and never takes the other, so D92's 60 s + 5 s is
unchanged. The shared guides took main's text during the rebase, and D93's entries were re-applied after D92 in one
commit.

After the rebase: `cargo fmt --check`; `cargo clippy` of core, apple, ios, macos, android, cli and test-support with
`-D warnings`; `cargo test --no-fail-fast` of those crates: 2,466 passed, 0 failed, 34 ignored, among them every D91
(14), D92 (10) and D93 (26) witness; `scripts/check_plan_dag.py`.

## Closing audit (Sol 6.2 max at 3c36e19c: REQUEST_CHANGES, three P2s, one invariant)

The invariant: no on-dispose lifecycle and no lease release while an xcodebuild may be live. Three rules make it
unexpressible.

| Finding | Rule and change | Witness (fails on 3c36e19c) |
|---|---|---|
| 1 SIGTERM bypassed the seal | R3: D52's `stop_execution` seals the Scheduler's devices (`Scheduler::seal_devices`, the same `seal_for_stop`) before it cancels any job. Report-first order and the 24 s limit are untouched; the provider's disposal after the reports reads the window already started. | `run_supervision.rs::a_signal_seals_the_devices_before_it_cancels_them_d93` (a batch raises SIGTERM: sealed before cancelled, exit 143; on the base only the cancel) |
| 2 a successful retry erased an unconfirmed exec | R2: each device keeps every session registered and not confirmed to have ended (`sessions`: iOS state, the Mac's list); a session leaves only once `ended() == Some(true)`, so neither iOS's replacement of the last session nor the Mac's prune can drop one whose exec or close was not confirmed. The seal's receipt is that list, and the verdict waits for and evaluates each. `XcodebuildSession::closed()` now counts an open failure, so the next session on the device is refused as after any unclosed one. | `simulator_dispose.rs::a_retried_session_does_not_erase_an_unconfirmed_one_d93`, `batch.rs::a_retried_session_does_not_erase_an_unconfirmed_one_on_a_mac_d93` (on the base: lifecycle ran / not quarantined) |
| 3 session admission raced a confirmed disposal | R1: session registration (iOS `execute_test_request`, the Mac's) checks the seal under the lock it takes, as batches, preparations and sends do; a sealed device's session is refused with a cancellation, and one registered before the seal is in the receipt. | `simulator_dispose.rs::a_session_asked_for_after_the_seal_is_refused_d93` (lifecycle held: xcodebuild refused; on the base it started beside it), `batch.rs::a_sealed_mac_opens_no_session_d93` |

Changed test: `simulator_admission.rs::recovery_waits_70_seconds_for_an_open_parsing_session_then_retires_only_that_device`
(Rust-only, no parity row) → `a_parsing_session_open_at_the_recovery_disposal_keeps_the_simulator_out_and_quarantined_d93`:
the open parse session is in the lost simulator's receipt, so its disposal stops it in its reserve and quarantines the
simulator at its window's end (200 ms in that harness) instead of returning at once. The test connections now count
quarantined leases (`provider_support::Connections::quarantined`, forwarded by `recovery_support::ScriptConnections`),
and the harness's lease balance is acquired = released + quarantined.

Follow-ups left as the audit lists them (non-blocking): a full D44/D90 recovery witness; reconciling iOS's latched-error
propagation with D93.

**Closing-audit verification.** Each of the five witnesses fails on 3c36e19c (scratch worktree, new tests copied in).
Stress, each alone: the five witnesses and the rewritten recovery test, 50/50 each and 20/20 each with 12 busy processes.
`cargo fmt --check`; `cargo clippy` of core, apple, ios, macos, android, cli and test-support with `-D warnings`; `cargo
test --no-fail-fast` of those crates: 2,471 passed, 0 failed, 34 ignored, among them the D52 and signal tests (35). Not
run, by the batch's rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Review round 6 (Sol 6.2 delta check at d20ecdcc: closing-audit findings 1 and 2 closed)

| Finding | Change | Witness (fails on d20ecdcc) |
|---|---|---|
| P2-a a session could be sealed between its registration and its start, and read as ended | `XcodebuildSession::claim()`: the device marks the session started while it still holds the lock it registered it under, so from registration until its producer ends `ended()` is `None` and a seal's receipt includes it. `refuse_unopened()`: the seal's `stop_all` refuses a receipt session that has not begun to open its command; its producer then ends at once with a device failure, having started nothing (no open failure recorded). A session already opening runs on and is waited for, as before. | `simulator_dispose.rs::a_session_registered_but_not_started_is_live_and_refused_at_the_seal_d93` (a runtime holds the producer back; the disposal seals in the gap: on the base xcodebuild started after the seal) |
| P2-b a sealed device's refusal read as an empty test list | A session refused under the seal is a device failure (`BatchLoss` on iOS, as a batch on a disposed simulator; `lost` on a Mac), as is a refused start: `XCTestParser::collect_tests` swallows only cancellations, so the error reaches core, whose D53 retry marks the device tried and takes another (`run_flow_witnesses.rs::a_parse_that_fails_on_one_device_takes_the_other_next_d53` pins that for any non-cancellation error). | the iOS and Mac refusal witnesses of the closing audit now assert `DeviceLost` (a cancellation on the base) |
| Availability (D44) recovery waited 70 s more for the session after the disposal | `recovery.rs`: the recovery no longer runs its own `await_session_closed(70 s)`; the disposal's verdict covers the parsing session within the one window, and a simulator whose session it could not confirm is retired by its quarantine. Siblings are set up again after that window. | `simulator_admission.rs::a_parsing_session_open_at_the_recovery_disposal_keeps_the_simulator_out_and_quarantined_d93` now asserts the sibling is set up within 70 s of the quarantine (on the base the recovery's own 70 s wait ran past the 60 s watchdog) |

Not reproduced end to end: discovery refused on a removed simulator with a healthy sibling through the real binary
(the chain is device refusal → parser propagation → core D53 retry, each witnessed or pinned separately).

The same holds for discovery's sends: a send that the device's seal or disposal stopped or refused, its caller not
cancelled, is now a device failure too (`leaving_send`, iOS and Mac `push_file`/`push_folder`), so a removal during
discovery's sends also takes the parse to another device. A batch's or a preparation's sends, whose callers the stop
cancels, keep their cancellation. This changes x90's `simulator_dispose.rs::a_send_to_a_disposed_simulator_is_stopped_before_it_starts_d91`
(now a `DeviceLost` whose cause is the `has been disposed` refusal; nothing starts, as before) and
`a_local_send_after_the_seal_copies_nothing_d93`; both fail on d20ecdcc as they stand.

**Round 6 verification.** Each round-6 witness fails on d20ecdcc (scratch worktree, new tests copied in; the recovery
witness there runs past the 60 s watchdog, its recovery waiting its own 70 s). Stress, each alone: the six witnesses
above, 50/50 each and 20/20 each with 12 busy processes. `cargo fmt --check`; `cargo clippy` of core, apple, ios, macos,
android, cli and test-support with `-D warnings`; `cargo test --no-fail-fast` of those crates: 2,472 passed, 0 failed,
34 ignored. Not run, by the batch's rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Review round 7 (Sol 6.2 delta at 396b8afe: D44 closed; P2-a, P2-b open; two regressions)

Implemented as the design note at the top says, with two refinements found while running the suites:

- **The gate lets a registered batch's own work through.** Closing it on every command cut a disposed batch's result
  collection (`batch.rs::concurrent_disposal_joins_one_completion_and_result_delivery_is_independent` lost its result).
  The batch a device runs is registered before its seal and is in the receipt the window waits for, so its own
  commands pass (`as_registered_batch`, a task-local scope around the batch's run); what it spawns does not, so a
  session sealed before it opens still never opens (its producer runs in a task of its own).
- **A cancelled batch call after a disposal outside a stop keeps the old rule.** The ported Kotlin
  `disposal_during_the_cancellation_of_a_batch_returns_only_after_its_xcodebuild_session_has_closed` (macOS) has the
  caller cancel the batch before the disposal and expects the cancellation. That is the call's own result, not a
  refusal; after the run's stop it is always a cancellation (the seal's cause), after a disposal outside a stop its
  caller's cancel decides as before. Every refusal (the gate, a session, a send, an admission) is classified by the cause
  alone; the "caller not cancelled" discriminator is gone from them.

| Item | Change | Witness (fails on 396b8afe) |
|---|---|---|
| P2-a | `DeviceCommands` is the gate: `seal(cause, why)` under the device's lock, checked as each command executes; the seal also calls `stop_all` at once (batch and preparation signals, send receipts, sessions not yet opening). | `simulator_dispose.rs::a_producer_released_between_the_seal_and_any_cleanup_starts_nothing_d93` (no cleanup runs between the seal and the release: on the base xcodebuild opened) |
| P2-b | A refused session's producer returns its refusal (by cause) before its own scope's cancellation; a disposal's cause is a device loss. | `a_held_producer_on_a_removed_simulator_is_a_device_loss_after_the_force_reserve_d93` (on the base: a cancellation, which discovery reads as no tests) |
| Regression 1 | The seal stops the Mac's preparation at once and closes its gate; its refused send is a cancellation (the run's stop), which the installer does not retry. | `batch.rs::a_preparation_sends_nothing_after_the_runs_stop_seals_the_mac_d93` (on the base: 16 commands, `mkdir` among them, sent to the sealed Mac) |
| Regression 2 | A cancelled batch call on a device the run's stop sealed is a cancellation, whatever its caller heard yet. | `a_batch_refused_by_the_runs_stop_is_a_cancellation_d93` (on the base: a `DeviceLost`) |

Tests that pinned round-6 classification now follow the cause: `a_sealed_mac_opens_no_session_d93` and
`a_local_send_after_the_seal_copies_nothing_d93` (the run's stop: a cancellation); the U179
`disposal_stops_a_running_preparation_and_returns_only_after_it_has_ended_u179` accepts the device loss the gate now
gives the stopped preparation's next command, besides the cancellation it gave before.

Limit, recorded in D93: a sealed device's listener tasks (a video recorder's stop, say) are refused at the gate like any
other new command; the on-dispose lifecycle (shutdown, erase) ends a simulator's recordings.

**Round 7 verification.** Each of the four round-7 witnesses fails on 396b8afe (scratch worktree, new tests copied in).
Stress, each alone: the four witnesses and the two Mac tests the gate first broke (`concurrent_disposal_joins_…`,
`the_macs_stop_stops_a_batch_session_…_d93`), 50/50 each and 20/20 each with 12 busy processes. `cargo fmt --check`;
`cargo clippy` of core, apple, ios, macos, android, cli and test-support with `-D warnings`; `cargo test --no-fail-fast`
of those crates: 2,476 passed, 0 failed, 34 ignored, among them the D52 (17) and signal (25) tests, x90's D91 witnesses
(14) and every D93 witness (37). Not run, by the batch's rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo
test --workspace`.
