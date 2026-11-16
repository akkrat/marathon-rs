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
  queued; A leaves and connects again as a new device; a marker device's recovery is queued behind it and one slot is
  freed alone, so the marker recovers only after a kept stale recovery would have probed; nothing probes the new device
  and no event follows.
- `restorations_after_a_server_restart_are_bounded_and_leave_the_loop_free_u170`: 20 devices kept through a server
  restart; 8 reverse forwards in flight, checked again after a device whose restoration is queued goes `offline` and
  has its grace end at its limit; all others restored once the gate opens. The re-check is not ordered after a ninth
  request a broken bound would send, so the bound's deciding witness is the ledger's unit test.
- `termination_during_recoveries_is_bounded_and_releases_nothing_u170`: terminate with three probes held (the clock
  never moves, so only cancellation can end them); it returns, and afterwards nothing was released or restarted.
- Unit tests in `provider.rs`: `a_stale_recovery_completes_into_nothing_u170` (superseded, dropped, abandoned
  completions return nothing), `recoveries_are_bounded_counting_those_still_ending_u170` and
  `a_panicking_recovery_still_reports_and_frees_its_slot_u170`; in `tracker.rs`,
  `a_second_grace_during_a_recovery_keeps_the_first_holds_start_d48`.

Each fails without the change:

- Full revert (`provider.rs`, `device.rs`, `tracker.rs` restored to 60465854, tests, fake and support kept): all five
  witnesses fail at the 60 s hang catcher (the loop sits in the first held probe or forward). Run 2026-11-16.
- Partial revert, `Recoveries::shutdown` joining without cancelling: the termination witness hangs and fails.
- Partial revert, a queued recovery kept when its device leaves: the stale witness fails (5 `getprop`s on the new
  device where 4 are expected). An earlier version of that witness, with the new device `offline`, passed under the same
  revert, because a probe to an offline device leaves no shell record and a lost device reads unhealthy anyway; it was
  changed to keep the new device online.

Commands run, all passing: `cargo fmt --check`; `cargo clippy -p marathon-adb -p marathon-android --all-targets -- -D
warnings`; `cargo test -p marathon-adb -p marathon-android` (24 test binaries, including `reconnect_test`'s 25 whole-run
scenarios and the existing grace, outage and D70 suites, unchanged); `cargo test -p marathon-cli --no-fail-fast` (two
targets first failed because this fresh checkout lacked the `snakeyaml` `yaml_tree` example that `scripts/check.sh` builds
first; both passed once it was built); `python3 -I tools/differential/make_registry.py --check`,
`tools/differential/test_evidence.py` and `scripts/check_plan_dag.py`. The registry was regenerated because the fake's
`world.rs`, a pinned scenario input, changed; only its hash moved. Not run, per the batch's gate rule: `scripts/check.sh`,
`scripts/remote-check.sh`, `cargo test --workspace`.

## Review round 1 (Muse)

Verdict "merge", with three minor findings, all fixed.

- **M-x64-1, a panicking recovery never reported.** The task sent its `Done` at the end of its body, and only that
  message frees the ledger's slot and decides about the device; the runtime's catch of a panic is seen only by a join,
  which the loop does at its end. The body now runs under `AssertUnwindSafe(..).catch_unwind()` (`report`, which adds
  `futures-util` =0.3.34 to marathon-android, the version the workspace already locks), and a panic is reported as a
  failed recovery. Departure: the loop used to end on a failed recovery (`done.outcome?`), which would have ended the
  server's tracking and failed the run. It now ends only on a cancellation; any other failure (before this round only a
  timeout conversion of the configuration or the vendor clock's delay could fail this way) is logged and the device set up again, as when its
  probe fails, since its state is unknown. After an outage that means its own gate opens. Witness:
  `a_panicking_recovery_still_reports_and_frees_its_slot_u170` (a one-slot ledger, a panicking body: the report arrives
  as an error, the slot frees and the queued device starts). Without the catch, the panic unwinds out of the task and the
  test fails.
- **M-x64-2, a second grace during a recovery held the device twice.** The first grace's hold stays until its recovery
  reports, so a device going `offline` again meanwhile was held again, and `held_since` moved to the second grace,
  undercounting D48's `return_wait_used`. `hold` now keeps an existing hold and its start; the grace count still goes up.
  Witness: `a_second_grace_during_a_recovery_keeps_the_first_holds_start_d48`, which fails without the fix (away since
  12,000 instead of 1,000).
- **M-x64-3, fixed sleeps decided three negative checks.** The claim above, "no real-time sleeps deciding the outcome",
  now holds, with one qualification:
  - stale recovery: the first busy device's probe waits at its own gate, and a marker device's recovery is queued
    behind the replaced device's. Opening that gate frees one slot; a kept stale recovery would take it, probe and
    report before the marker starts, so the check made once the marker has recovered cannot pass early. With the queue
    drop reverted it fails at that check (5 `getprop`s on the new device, 4 expected), 3 of 3 runs;
  - termination: `terminate` joins the provider task, which joins each server's tracking task, which joins its
    recoveries, so the checks are made right after it, before the probes are let through, with no wait;
  - restorations: no event is ordered after a ninth request that a broken bound would send, so this one cannot be made
    strict end to end. The sleep is gone; the count is checked again after a later grace ends, and the deciding witness
    of the bound is `recoveries_are_bounded_counting_those_still_ending_u170`. The device sent `offline` there is now
    one whose restoration is queued: a running one's cancelled task reports and frees its slot, which made the re-check
    see 29 (seen while writing it).

  The 50 ms pause in that test's retry loop paces clock steps until the provider reconnects; it decides nothing.
  `return_recovery_test` passed 10 of 10 runs.

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
