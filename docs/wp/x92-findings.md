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
