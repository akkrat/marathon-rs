# WP x64: a device coming back no longer holds the others (U170)

Base: main 60465854 (includes x63, D70). No deviation row: worker commands, results and the pinned text are unchanged,
and only the interleaving of commands across devices changes (see "Contract" below). D73 was not used.

## Design

### Where the wait came from

Each adb server has one tracking loop (`AdamDeviceProvider::follow`, formerly the body of `track_server`). It reads the
device list, ends D41 grace periods (`Blips::next_over`) and closes D22's settling window. Before this WP the loop also
did, in line and one device after another:

- for each device back from a grace: `finished_booting` (one `getprop`, bounded by `timeoutConfiguration.shell`), then,
  after the list was sent, `resume_after_blip` (waits up to 1 s for the old logcat stream to end, restarts it, and
  re-adds each reverse port forward, each bounded by `portForward`), and only then released the device's hold;
- after an adb server came back (`finish_outage`): `resume_after_server_return` for each device kept, and only after the
  last of them reopened the server's gate (`server_back`).

While any of these ran, no other grace could end and no list was read. Confirmed against the code before changing it:
the review's numbers (15 s against a 10 s limit; 50 s with two returns and `shell` at 30 s) follow from that order.

### What runs where now

- **Per-device recovery tasks.** A returned device gets a `Recovery::Blip` task (probe; if it booted, `resume_after_blip`),
  and a device kept through an outage a `Recovery::Outage` task (`resume_after_server_return`). Both run on the vendor IO
  runtime (`spawn_cancellable`), never on the core executor, and report a `Done { id, serial, outcome }` on a channel the
  loop selects on beside the stream, the settling window and the grace timers.
- **The loop keeps the decisions.** Everything that changes provider state is still done in the loop: sending lists,
  the restarted device's disconnect and connect, releasing a hold or opening a gate. A task only probes and restores.
- **Bounded, deduplicated, cancellable (`Ledger`).** Per server, at most `RECOVERIES_PER_SERVER` = 8 tasks run at once,
  counting tasks that were cancelled and have not reported yet, so the bound is on what actually runs. Queued work is at
  most one entry per device: a new request for a device replaces its queued or running one, the running one being
  cancelled. The number 8: a recovery is one `getprop`, a logcat stream and a few reverse forwards, where a server
  carries a logcat and an instrumentation stream for every device throughout a run, so 8 is light; the bound keeps a
  restarted server with dozens of devices from getting all their requests at once, and with one device that does not
  answer, 7 slots keep working. No configuration field.
- **Obsolete work is cancelled.** A device with a queued or running recovery that a new list no longer shows as `device`
  (it left, went `offline` again, or reached another state) has its recovery dropped and its task cancelled. If it went
  into a new grace, that grace's hold stays; otherwise the old hold is released, or its restoring gate opened, only after
  the disconnect has been handled, as before for devices that leave. A server outage (`server_lost`) abandons all of the
  server's recoveries; they run again after it. When the loop ends (shutdown, D52), `Recoveries::shutdown` cancels and
  joins every task, which each end at their next await; `Drop` cancels as a safety net.
- **Stale completions count for nothing.** A completion counts only if its id is still the device's current recovery
  (`Ledger::finish`: one cancelled, superseded or abandoned reports into nothing) and the provider still holds the same
  device object under that serial, ready (`AndroidDevice::is_same`). Only then does the loop release the hold, open the
  gate or treat the device as restarted.
- **Side effects cannot outlive a disconnect.** `resume_after_server_return` (also the tail of `resume_after_blip`) now
  takes the device's disposal lock, checks that its task was not cancelled and the device is not lost or disposed, and
  holds the lock while it restarts logcat and re-adds forwards. A disposal either comes after it and undoes what it set
  up, or came first and it does nothing. The loop cancels a task before it sends the list that disconnects its device, so
  the lock is held only briefly by a cancelled task.
- **Per-device gate after an outage.** `DeviceStateTracker` gets a `restoring` set. `finish_outage` now calls
  `server_back_restoring(serials)`, which in one step opens the server and closes each restoring device's own gate, and
  each opens with `restored(serial)` when its task is done. So a device whose reverse forward hangs keeps only itself
  waiting; before, the whole server waited for the slowest. `available`/`unavailable` (the batch gate),
  `is_gate_closed` (D70's wait between instrumentations, D22's `wait_out_passing_loss`) and `loss_cause` treat a
  restoring device as still in the outage, as it was while the server's gate stayed closed during restoration. D48's
  `away_since` keeps counting a restoring device from when it was first kept waiting. `info()` (and so `networkState`
  and `healthy`) is unchanged: it never looked at the server gate.

### Departures from the brief

- **The fake adb got a test hook.** Restoring after an outage only waits on reverse port forwards, which the fake could
  not hold. `server.reverse_gate` (a gate name; `reverse:forward` requests wait until the in-process test opens it) and
  `FakeAdbServer::close_gate` were added and documented in `docs/design/fake-adb-script.md`. Optional, off by default;
  existing scripts are unaffected (the marathon-adb suite passes unchanged).
- **The per-device gate after an outage** (above) is a choice the brief allowed but did not spell out. The alternative,
  keeping the server's gate closed until every restoration ends, would still let one device's hanging forward hold the
  whole server.
- **A stale *running* completion has no deterministic end-to-end witness.** The fake, like adb, drops a device's open
  sessions on any state change, so a probe cannot be held across the event that makes it stale. That case is pinned by
  the ledger's unit test; the end-to-end witness makes a *queued* recovery stale instead (below).

## Invariants

1. The tracking loop never awaits a device's probe or restoration.
2. At most 8 recovery tasks run per adb server, including cancelled ones not yet reported; at most one entry per device.
3. A hold is released, a restoring gate opened, or a device set up again only by the loop, and only for the device's
   current recovery on the same device object.
4. A device leaving during its recovery is released only after its disconnect has been handled (as before for `gone`).
5. A server outage cancels its server's recoveries; shutdown cancels and joins them all, a bounded join.
6. A healthy run, with no grace and no outage, takes the old fast path: same commands, same events, no extra round trip.

## Contract

Per device, the same commands are sent in the same order as before: one `getprop` for a return, then the logcat stream
and the reverse forwards; after an outage the logcat and the forwards. Results, report data and the pinned
progress/summary text are untouched. What changes is the interleaving across devices (recoveries overlap), when a
device's gate opens after an outage (its own restoration instead of everyone's), and log line order across devices.
Logs are not contract. No registered differential scenario or transcript comparison covers grace periods or outages:
`tools/differential` has none, and `fixtures/fake-adb/android-reconnect` scripts only connect and disconnect, which take
the unchanged fast path (their `reconnect_test` comparisons with the Kotlin outcome pass). So no comparator allowance
and no D-row were needed. D41's and D22's text, which described the old timing, now describe the new one.

## Verified, and how

Witnesses, `crates/marathon-android/tests/return_recovery_test.rs`, on a vendor clock the test moves itself
(`support::run_with_driven_clock`, which forwards `advance_to` from the test body to the replay driver on the core
thread) and fake-adb gates, with no real-time sleeps deciding the outcome (`docs/testing.md` rules 4, 5):

- `a_slow_return_probe_does_not_hold_another_devices_grace_u170`: B in a grace, A back with its probe held; at 10.5 s B
  is disconnected while A is still held; A recovers when its probe answers, same actor, one grace.
- `two_hanging_return_probes_do_not_add_up_u170`: `shell` 30 s; both probes are in flight at once, a third device's
  grace ends at 10.5 s, and at 30.5 s both returned devices are given up as restarted (disconnected and set up again).
- `a_recovery_made_stale_by_a_replacement_releases_nothing_u170`: 8 hanging probes fill the slots, so A's recovery is
  queued; A leaves and connects again as a new device; when the slots free up, nothing probes the new device and no
  event follows.
- `restorations_after_a_server_restart_are_bounded_and_leave_the_loop_free_u170`: 20 devices kept through a server
  restart; exactly 8 reverse forwards in flight; meanwhile a device going `offline` has its grace end at its limit;
  all others restored once the gate opens.
- `termination_during_recoveries_is_bounded_and_releases_nothing_u170`: terminate with three probes held (the clock
  never moves, so only cancellation can end them); it returns, and afterwards nothing was released or restarted.
- Unit tests in `provider.rs`: `a_stale_recovery_completes_into_nothing_u170` (superseded, dropped, abandoned
  completions return nothing) and `recoveries_are_bounded_counting_those_still_ending_u170`.

Each fails without the change:

- Full revert (`provider.rs`, `device.rs`, `tracker.rs` restored to 60465854, tests, fake and support kept): all five
  witnesses fail at the 60 s hang catcher (the loop sits in the first held probe or forward). Run 2026-11-16.
- Partial revert, `Recoveries::shutdown` joining without cancelling: the termination witness hangs and fails.
- Partial revert, a queued recovery kept when its device leaves: the stale witness fails (5 `getprop`s on the new
  device where 4 are expected). An earlier version of that witness, with the new device `offline`, passed under the same
  revert, because a probe to an offline device leaves no shell record and a lost device reads unhealthy anyway; it was
  changed to keep the new device online.

Existing suites, unchanged, pass: see the final report for the commands run.

## Not verified

- Real emulators and a real adb server restart: not run in this WP.
- The device-identity check in `recovered` has no witness of its own: with the queue drop and the id check in place it
  is never reached by a stale completion. It is a second layer.
- A device that goes `offline` again during its own return probe: adb drops the probe's session, the probe fails, and if
  that failure is read before the new list, the device is taken for restarted and set up again. The old code always did
  this (the loop read the list only after the probe); now it happens only when the failure wins the race. Not a
  regression, not fixed.

## Golden diffs

None.

## Incidental bugs

None found.
