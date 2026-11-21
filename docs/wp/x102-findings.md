# x102 findings: busy-video follow-ups

WP x102 has two items: a load flake in a D96 supervisor witness (test-only), and the D98 known limit recorded in
[x99 findings](x99-findings.md): the busy video state belonged to the device object, not to the simulator (U259). No
new D-row: item 2 completes D98's own promise that the next run boots the simulator clean, and adds no command beyond
D98's one `simctl shutdown` per busy simulator. The D98 section of `docs/deviations.md` has an x102 note.

## Item 1: busy answers in the worker supervisor witnesses

`forced_and_expired_recorders_finalize_and_failed_recorders_keep_bounded_stderr_d96` sent its force request
(`<token>:3`) once and checked only that `control` exited 0. The worker answers `<token> busy` when the token lock is
held for that instant (by its own loop, which reads the state every 25–50 ms) and then does not apply the request. The
force was then lost, and the forced recorder (lease 10 s) ended only at its lease, after the test's 6 s receipt guard.

Reproduced with the base worker script: with the token lock held by another process, the force answered
`<token> busy` and the recorder still reported `recording:` 6 s later; it ended only after a second force.

Fix, in `crates/marathon-apple/tests/ssh_supervisor.rs`:

- That test sends every control request through `d98_control`, which repeats a request while the worker answers
  `busy`, and reads receipts through `d98_receipt`. The forced recorder's lease is now 120 s, longer than every hang
  guard, so only the force can end it in time; each receipt is also bounded at 30 s from the request. The expired
  recorder's lease went from 1.5 s to 3 s, so that a slow start under load still prints the start marker before the
  lease expires. What it pins is unchanged: forced and expired recorders finalize with `done:0`, a failed recorder's
  receipt carries exactly 8192 bytes of its stderr, and the root retires.
- Sweep of the same pattern in the file (no other test file drives `worker.pl`; the real-sshd and fake-worker tests go
  through the product's lease watcher, which retries `busy`):
  - `retire` answers `MARATHON_D96_1 busy` while a detached supervisor that has already published its receipt still
    holds the installation lock until it exits. The six places that retired right after a receipt now use
    `d98_retire`, which repeats while busy (30 s hang guard).
  - `stock_perl_detaches_the_recorder_and_returns_its_own_exit_proof_d96` drove `control-stream` with fixed ids and
    expected exact reply order; a `busy` answer to an INT or TERM request would have dropped the request and shifted
    the replies (the TERM to the foreground payload was not even read back, so the test would have hung to its 60 s
    guard). `ControlStream::request` now sends each request with a sibling status behind it, reads up to the sibling's
    reply, and resends under a new id while the request was answered `busy`, as the lease watcher does.
  - `worker_expiry_compacts_its_receipt_and_retirement_waits_for_owned_groups_d96`: its 8 s guard (expiry at 2.2 s,
    TERM and KILL by about 5.2 s) became a 30 s hang guard.
  - Not changed: requests on a token with a published receipt (answered from the receipt without the lock) and
    requests with no concurrent lock holder (`cancellation_before_launch_prevents_worker_payload_d96`), which cannot
    be answered `busy`; status polls already in retry loops.

Stress, 36 busy processes on 18 cores: the fixed test 50 of 50, the control-stream witness 20 of 20 and the expiry
witness 20 of 20. Test-only: no CHANGELOG entry and no ledger row.

## Item 2: the busy state belongs to the simulator

### When a simulator gets a new device object during a run

Verified in `crates/marathon-ios/src/device/simulator_provider.rs` and `simulator_provider/recovery.rs`. A device
object is made only by `SimulatorFactory::create` (the `AppleSimulatorConstructor`), from the run's one
`SimulatorServices`, and enters the provider's map through `register`, keyed by `apple_id` (UDID and worker). A second
object for the same key replaces the first (`register` moves it to `replaced`, disposes of it, and logs `replaced
existing device AppleSimulatorDevice with new AppleSimulatorDevice.`). The paths:

1. **Recovery after a loss** (`SimulatorRecovery::recover`). A simulator is marked lost by a failed boot in its
   preparation, a batch failure the D33 policy counts as a loss, a batch timeout, or a connection, transport or
   illegal-state error (`mark_lost`, execution.rs and prepare.rs), including after a worker connection drops. The
   monitor's reconnect scan finds it, the recovery disposes of the old object (a `Disposal` seal) and sets the same
   UDID up again on the same worker (`provisioner.existing`), which registers a new object. Up to the loss limit, so
   a simulator can have several objects in a run.
2. **Added back to the devices file** (`recover` with `readded`, and D44's `recover_again`). A removal disposes of the
   object; an add-back sets the same UDID up again as a new object.
3. **Setup retries** (D33) and the late worker's retry. The failed setup's object was disposed of by the factory
   before it registered, so it ran no test and recorded nothing; the retry's object is the first one that serves.
4. Not paths: a different worker is a different key, so a different simulator; profile targets create simulators with
   new UDIDs; the lifecycle has no mid-run restart; a quarantined or retired simulator is never set up again.

A simulator whose current object was disposed of during the run and not replaced stays in the map until termination,
which disposed of it again (a no-op: the disposal is latched) and released its lease. That happens after a recovery
whose new setup failed, after the loss limit, after D87's preparation deadline, and after a removal from the devices
file.

### Design

- **One owner of the state.** `BusyVideoServices` (`apple_simulator_device/busy_video.rs`) is a run-scoped map from the
  simulator's `apple_id` to its state, built once per run in `ios_vendor.rs` and shared through
  `SimulatorServices::busy_video` by every device object the run constructs. The per-object `video_unavailable` field
  is gone, so no object can hold a stale copy. Each object computes its key once at construction (`video_key`).
- **Video off.** `recorder_busy` marks the simulator under the device's lock, as a capture registers, so a capture
  registered after the mark sees it, as before; only the first mark logs the warning. `begin_capture` and the
  recording's post-`ready` check read the registry. Lock order is device state, then registry, never the reverse.
- **Restart ends it.** `shutdown_device` clears the simulator's entry when it returns a confirmed shutdown: the
  on-dispose and on-prepare lifecycles and D98's own shutdown go through it. A boot from `Shutdown` in a preparation
  (`boot_failure`'s successful `simctl boot`) clears it too, which covers the simulator that went down without
  `shutdown_device` (a crash, or `shutdown_after_failed_boot`, which calls `simctl shutdown` directly). Either way the
  simulator gets a fresh video service, so a new object for it records again (as every new object did before x102). Without this, an on-dispose `SHUTDOWN` at a
  recovery would have kept video off on a simulator that was already clean.
- **Exactly once at the run's end.** `shut_down_busy_video_by` claims the simulator (`claim_run_end`, `Off` to
  `Claimed` under the registry lock) after the silent skips (configured shutdown or erase, not connected, quarantined)
  and before anything is logged or sent, so one line and at most one `simctl shutdown` per simulator, whichever object
  gets there.
- **Who shuts it down.** The object that serves the simulator at the run's end, through its own command gate
  (`as_lifecycle`, D93) and bounds:
  - a live object: its run-end disposal (`RunStop` or `Termination`), exactly as in D98;
  - an object disposed of during the run: the provider's termination, which calls
    `shut_down_busy_video_after_disposal` for each object in its map after that object's (latched) disposal and
    before releasing its lease. The device does nothing unless its own disposal was a mid-run `Disposal` that has
    completed; the window is the termination's deadline (set by `seal_for_termination`, 70 s), clipped by D97's signal
    finish, less the force reserve, with x101's start floor; after a signal, the x101 warning instead. The provider
    skips a simulator that left the devices file (no longer the run's; the owner's D98 decision already ruled out a
    shutdown at its removal) and one whose lease it does not hold (`handed_back`). A quarantined simulator, including
    every one after an abandoned provider shutdown, is skipped by the device.

  The provider decides whether (it alone knows which object currently serves the simulator, whether the simulator is
  still in the devices file, and which leases it holds and when it releases them); the device decides how, with the
  same gate, task, cancellation and bounds as D98, and no new connection. Replaced objects never act: their simulator
  is served by the object in the map.
- **Never a simulator another live object serves.** At termination the map holds one object per key, and the late
  step acts only on an object already disposed of. A setup still running at termination either ended before the
  devices are disposed (termination joins recovery and setup tasks; a registration after termination starts is
  refused and the setup disposes of its object) or was abandoned, which quarantines every simulator.

Unchanged: the warning and log texts, the busy-text match, the bare status 16, the legacy recorder, the skips (on-dispose
shutdown or erase, quarantine, signal), the window and floor, and that no disposal during the run sends a shutdown.

### Witnesses (`crates/marathon-ios/tests/ios_listeners.rs`, virtual time)

| Witness | Shows | Reverted | Failure |
| --- | --- | --- | --- |
| `a_simulator_set_up_again_in_the_run_keeps_video_off_without_a_second_warning_d98` (a) | object A busy and disposed as a recovery does; object B for the same simulator launches no recorder (`Skipped`), no second warning; B's run-end release shuts it down once | state per object (a key of its own per object) | B launched a recorder (not `Skipped`) |
| `a_busy_simulator_disposed_during_the_run_is_shut_down_once_at_its_end_d98` (b) | provider; A busy, disposed during the run, no later object; termination sends one `simctl shutdown a`, one INFO, not quarantined, every lease released | no termination step | no `simctl shutdown a` |
| `a_busy_simulator_disposed_during_a_signal_stopped_run_is_not_shut_down_d98` (c) | as (b) after a signal: no shutdown, one x101 warning | no termination step | no line |
| `a_busy_simulator_leaves_another_simulator_on_the_worker_recording_d98` (d) | simulator `b` on the same worker records; only `a` is shut down | (control) | — |
| `a_busy_simulator_still_serving_at_the_end_is_shut_down_once_by_its_own_release_d98` | provider control: the live object's release shuts it down; the termination step adds nothing | (control) | — |
| `a_busy_simulator_removed_from_the_devices_file_is_not_shut_down_d98` | provider control: a removed simulator is not shut down | (control) | — |
| `busy_video::tests::a_busy_simulator_warns_once_and_is_claimed_once_until_it_restarts` | registry unit test | — | — |

The existing D98 witnesses pass unchanged; `simulator_with_video_off` now uses the extracted `turn_video_off`.

No CLI-level witness: the replacement path needs a simulator lost and set up again mid-run, and no CLI test or fake
world can lose a simulator; the busy CLI witness (`a_busy_simulator_on_an_ssh_worker_turns_video_off_once_d98`) still
passes and covers the live-object path end to end.

Each revert was applied in a scratch worktree of the branch head, with only the named part removed: (a) fails with
the state per object; (b) and (c) fail without the termination step; the controls pass either way. (d) would fail
with a state shared by every simulator rather than per simulator. Stress: the 19 D98 witnesses of `ios_listeners`
(the 6 new ones included), 50 of 50 runs, and 5 of 5 with 36 busy processes on 18 cores. They run on virtual time and
are ordered by events, not by clock races.

## Run locally

`cargo fmt --check`; `cargo clippy -p marathon-ios -p marathon-apple --all-targets -- -D warnings`; `cargo test` for
marathon-ios (461 tests) and for marathon-apple's `ssh_supervisor`; marathon-cli's `ios_production`,
`differential_vendors` and `production_runner`. Not run: `scripts/check.sh`, `remote-check.sh`, `cargo test
--workspace` (orchestrator gates). `make_registry.py --check` and `check_plan_dag.py` pass. `source_inventory.py
--check` and `parity_matrix.py --check` report both CSVs out of date against the local Kotlin checkout, and do the same
on the base ffc6a1e5: that checkout's working tree differs from the frozen sources. This WP changes neither CSV.

## Golden diffs

None. No result file, golden or JVM recording changed; no differential registry change.

## Verification limits

- Not run against a real simulator or worker: the recovery path was exercised with the fake worker only.
- A simulator whose replacement object's registration failed after it entered the map (D74 `handed_back`) is not shut
  down at the run's end: its lease went back to the setup that made it, and its predecessor is in `replaced`. Narrow
  (it needs a failed disposal of the predecessor); recorded, not fixed.
- A simulator that left the devices file keeps a busy video service; the warning given when it was found busy says how
  to restart it.

## Incidental bugs

None found outside this WP's scope.
