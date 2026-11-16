# WP x65 findings: Apple preparation, lease and session-close cleanup

Fixes U179 and the lease leaks A90, A89 and A82 under D74, and adds the U183 warning (a log line, no D-row). Base: main
7090b228.

## U179: disposal stops and waits for a running preparation

**Before.** `prepare_device` ran the preparation as a retained task and, when its caller was cancelled, cancelled the
preparation's signal and returned at once. Disposal knew nothing of it: it joined only the running batch. `join_retained`
had no production caller, and could not have been called from disposal anyway, since the same `RetainedTasks` holds the
batches and the disposal task itself (a self-join). A preparation whose caller had not been cancelled was not even
cancelled by disposal, so it kept sending commands to a simulator already disposed of.

**Design.** Preparation ownership is separate from the retained tasks:

- `State::preparations` lists one `PreparationJob` (signal + completion cell) per running preparation. `prepare_device`
  registers it under the state lock and returns `Cancelled` ("Device … has been disposed") if `disposal` is already set.
  There is no await between registration and the spawn, so admission is sealed: disposal either sees the preparation or
  the preparation sees disposal.
- The preparation task completes its cell and removes itself from the list when its body ends, whatever the outcome.
- `dispose_and_join` takes the running batch and the preparations in the same critical section that installs the
  disposal cell. The disposal task calls `stop_and_join`, which cancels the batch (as before) and every preparation, then
  waits on their completion cells under **one** `with_timeout_or_null` of `cancelled_batch_millis` (70 s by default),
  the bound the batch join already had. On expiry the batch keeps its old WARN and retirement; an unfinished preparation
  gets one WARN (`Cancelled preparation of <serial> has not finished within <limit>, disposing of the simulator anyway`)
  and the disposal carries on. `join_cancelled` (the classification path) now calls the same function with no
  preparations, so its behaviour is unchanged.
- Waiting on cells, not on `RetainedTasks`, means disposal never joins itself. `join_retained` stays for tests; its comment
  no longer claims that a run owner calls it.

**Invariants.** No preparation starts after disposal began. Disposal returns only after every preparation it saw has
ended, or after the bound (logged). A stopped preparation's later commands carry a cancelled context, which the SSH and
local executors refuse before sending (`executor.rs`, `kotlin_process_command_executor.rs`), so nothing reaches the worker
after disposal even when the bound expired. The witnesses below show this only through the fake worker's opt-in
`refuses_cancelled`, which mimics those executors; the preparation's file-bridge and transfer steps are not executor commands,
and stop on a cancelled call by D54's existing pin (`simulator_prepare::a_receive_never_takes_its_callers_cancellation_but_a_send_does_d54`). The disposal context stays uncancellable, so D52's cancel cannot cut it short;
D52 bounds the provider termination that awaits it (the disposal task is owned separately and survives an abandoned
caller), so the 25 s limit is unchanged.

**Not done, on purpose.** A preparation still running at the bound does not retire the simulator (a batch does). The brief
asks for a logged bound only, and retirement would change which simulators a recovery sets up again.

**Witnesses** (`crates/marathon-ios/tests/simulator_dispose.rs`, paused clock, a preparation held in its `pkill` command
by a gate that ignores cancellation; the fake worker is set to refuse cancelled requests as the real executors do, via the
new opt-in `FakeSimulatorWorker::refuses_cancelled`):

- `disposal_stops_a_running_preparation_and_returns_only_after_it_has_ended_u179`: disposal still pending 10 s later,
  completes after the gate opens, the preparation ends `Cancelled`, and no command reaches the worker after the held one.
- `disposal_stops_waiting_for_a_preparation_that_has_not_ended_in_time_u179`: with a 1 s bound disposal returns at exactly
  1 s with the one WARN; opening the gate afterwards sends nothing more, 60 s on.
- `concurrent_disposals_both_wait_for_the_stopped_preparation_u179`.
- `a_cancel_during_disposal_ends_the_callers_at_once_and_disposal_still_stops_the_preparation_u179`: the D52 shape. A
  dropped dispose future (abandoned termination) and a cancelled preparation caller return at once; the disposal task still
  waits for the body, completes on its own, and nothing is sent after.
- `a_device_that_is_being_disposed_of_starts_no_preparation_u179`.

With the three source files reverted to main, all five fail (the other four tests in the file pass either way).

## A90, A82: iOS leases

**A90.** Registration added a replaced simulator to `replaced` (released at termination) only if it was not `recovering`,
or if it was parked with `retained`; recovery's settle added it afterwards only for a connected loss or a re-add. A
worker-loss predecessor was therefore never released. Now registration adds every replaced handle, and the settle-time
push and the `Parked::retained` flag are gone. A handle leaves the device map only by replacement in `register`
(termination clears the map after releasing what is in it), so each handle is added at most once, and current and replaced
handles are disjoint: each acquired device lease is released exactly once, at termination as before. Shared connections
(`shareWorkerConnection`) keep their user counts in `ConnectionFactory::dispose_inner`, unchanged.

**A82.** `dispose_then_release` now releases whatever the disposal returned. Errors were already discarded there.

**Tests.**

- `simulator_acquisition::a90_worker_loss_and_connected_loss_both_release_the_predecessor_lease_d74` (renamed from
  `a90_worker_loss_leaks_predecessor_lease_while_connected_loss_retains_and_releases_it`): with the real factory, the old
  executor is closed once by termination in both cases. The JVM recording is unchanged and read as before; the narrow
  allowance is that the test asserts its `oldRemaining` (true for the worker loss, false for the connected loss) next to
  the native release, instead of equating them.
- `simulator_provider::tests::a82_throwing_disposal_still_releases_the_device_connection_d74` (renamed from
  `a82_throwing_disposal_skips_the_device_connection_release`): the recording's `released: 0` is asserted as the JVM's,
  the native count is 1.
- The recovery harness's `finish` now asserts acquired == released leases after every termination (it used to skip that
  because of A90). With the A90 change reverted, 20 recovery tests fail through it, besides the two above; with it, all
  marathon-ios tests pass.

## A89 (and A82's macOS counterpart): macOS leases

- `initialize_transport` splits the work after the lease into `set_up` (UDID probes, device setup, tracker). If it fails,
  the lease is released at once and the setup's error returned. A failed release is logged at ERROR and never replaces
  that error.
- `connect`: a replaced Mac is disposed, then its lease released whether or not the disposal failed; the disposal's
  error is still returned, and a failed release is logged.
- Termination releases after a disposal that failed; the disposal's error comes first (`disposed.and(released)`).
- Tests: `null_executor_setup_failure_and_udid_fallback` now pins 1 release for `setup-failure`;
  `static_host_lifecycle_identity_queue_and_released_leases_d74` (renamed from `…_retained_leases`) pins 2 releases for
  the replace. Both fail with the provider reverted. The macOS disposal error at termination is witnessed since review
  round 1 (below).

No JVM oracle replay compares these counts besides the two iOS tests above; the macOS numbers were transcribed by hand
from the JVM mock-factory run, which stays in A89's row.

## U183: a close that needed KILL warns

`ExecChannel::close_escalating` records whether the TERM wait expired and KILL was sent. If the close then completes
(not abandoned), it logs one WARN: ``Session `<command>` did not end within 20 s of TERM and was closed after KILL. The
process may still be running on the worker``. That includes a close that completed with an error after KILL, on purpose: the
process may be running all the same, and the error still takes its usual path (the `woke_to` line and the failed close). An
abandoned close, which sent KILL too, keeps D30's ERROR line and gets no WARN. D30's line shows
with debug off (ERROR on the client logger), so this one must too: it goes to a new target, `marathon_apple::session_close`,
with a WARN floor (DEBUG with debug on) like `batch_outcome`, added to the filter, the directive inventory test,
`session_close_warns_in_every_configurator_with_debug_off`, the CLI subscriber test and `docs/design/logging.md`. No
behaviour change; D30's text now mentions the warning.

Witnesses in `crates/marathon-apple/tests/ssh_session.rs`:
`a_confirmed_close_of_a_process_that_ignored_term_and_kill_warns_once_with_debug_off_u183` (logging configured as a
`debug: false` Apple run) and `kill_ends_a_process_that_ignores_term_and_the_close_succeeds_d30` (one WARN); the fast-exit
and abandoned tests assert none. Both new assertions fail with `channel.rs` reverted, and again with only the floor
removed.

## Contract and golden diffs

D74 covers the observable change: worker commands a stopped preparation would have sent after disposal are no longer
sent, a disposing simulator starts no preparation, and leases are released (connections closed) that used to stay open
until exit. Result files, the Marathonfile and the CLI are unchanged. **No golden changed**; the marathon-cli end-to-end
and differential suites pass unmodified, and no comparator allowance was needed there.

## What was run

- `cargo fmt --check`; `cargo clippy -p marathon-ios -p marathon-macos -p marathon-apple -p marathon-test-support
  -p marathon-logging -p marathon-cli --all-targets -- -D warnings`: clean.
- `cargo test` for marathon-ios, marathon-macos, marathon-apple, marathon-test-support, marathon-logging and marathon-cli:
  all pass. `differential_parse` needs `cargo build -p snakeyaml --example yaml_tree` first, as `check.sh` does.
- The temporary reverts above, one fix at a time.
- `scripts/check_plan_dag.py`, `tools/differential/make_registry.py --check`, `tools/differential/run validate`,
  `tools/differential/test_evidence.py`, `tools/fake-worker/test_apple_evidence.py`: pass.
- `scripts/parity_matrix.py --check` and `scripts/source_inventory.py --check` report their CSVs out of date, identically on
  an untouched checkout of 7090b228, so it is not from this WP. No parity-matrix row names a changed test.
- Not run, per the batch rules: `scripts/check.sh`, the remote check, `cargo test --workspace`. Nothing ran on a real
  simulator, Mac or SSH server that ignores signals.

## Incidental bugs

- **Double release after a failed predecessor disposal (iOS).** `AppleSimulatorProvider::register` inserts the new
  simulator before disposing of its predecessor, and returns the disposal's error. The provisioner then treats the setup as
  failed and disposes of and releases the new simulator, which stays in the device map, so termination disposes of and
  releases it again. Kotlin does the same. **Fixed in review round 1** (below).
- **macOS replacement whose old Mac fails to dispose.** `AppleMacosProvider::connect` puts the new Mac in the map and then
  returns the old one's disposal error before registering or announcing the new one, which therefore never joins a pool
  but is disposed of at termination. Kotlin does the same; a disposal fails only on a fatal error. Not fixed.

## Review round 1 (Muse)

Four findings, all fixed.

**M-x65-1 (Medium): the iOS double release after a failed predecessor disposal.** The incidental bug above made D74's
"every lease released exactly once" false: with `shareWorkerConnection`, the second release decrements the connection's
users (`ConnectionFactory::dispose_inner`), taking another simulator's use and possibly closing the connection under it.
The concrete device cannot fail `dispose_and_join` while `register` holds a clone, but the generic device seam can, which is
what A82 injects. Fix: one owner per lease. An error from `register` already left the lease with its caller on every
earlier refusal; now it does on this one too. On a failed predecessor disposal `register` records the new simulator in
`State::handed_back`, and termination releases no handle listed there, whether it is still in the map or a later
registration moved it to `replaced`. The map keeps the simulator, as before and as in Kotlin, so recovery's view of the id
is unchanged; termination still disposes of it (a simulator's second disposal waits for the first). The provisioner's cleanup is
unchanged: it disposes of the simulator and releases its lease, once.

Witness: `simulator_acquisition::a_replacement_whose_predecessor_fails_to_dispose_releases_its_shared_lease_once_d74`. Real
`ConnectionFactory` with `shareWorkerConnection`, simulators `a` and `b` on one worker, a predecessor of `a` registered
with a throwing `DeviceRef`, a connected loss, and recovery setting `a` up again through the provisioner; the test checks
that the path ran (the throwing disposal was called and the provisioner released), then that termination brings releases
level with acquisitions, the shared connection closes once, and the factory logs no "not in use" release. With the
provider change reverted it fails with 6 releases for 5 acquisitions.

**M-x65-2 (Low): a repeated macOS termination after a fatal disposal.** `terminate` returned a fatal error through
`result?` before clearing the map, and has no once-guard, so a second call disposed of and released the same Macs again;
this WP widened that by releasing after a failed disposal. Fix: termination takes the map out before disposing, so a
repeated call finds nothing. Witness:
`provider::a_fatal_disposal_still_releases_the_lease_and_a_repeated_termination_releases_nothing_more_d74`. A Mac's
disposal fails only fatally, and only in its wait for a cancelled batch, so the test runs a batch and wraps the clock to fail
that wait with a fatal error once armed. It pins the release after the fatal disposal (the macOS A82 line, unwitnessed
until now) and no second release on a repeated termination. With only this fix reverted it fails with 2 releases instead
of 1; with the provider at main, with 0.

**M-x65-3 (Low): D30's wording of the U183 warning.** D30 said any close that needed KILL warns; an abandoned close, which
also sent KILL, gets only the ERROR line. D30 now says that a close that needed KILL and then completed gives the warning,
also when it completed with an error (kept on purpose, stated in the U183 section above and in `docs/design/logging.md`), and
that an abandoned close gives the error line instead. CHANGELOG and migration did not describe the case and are unchanged.

**M-x65-4 (Low): what the "no command after disposal" witnesses rely on.** Stated in the U179 invariants: the fake's opt-in
`refuses_cancelled` stands in for the real executors, and the file-bridge and transfer steps rely on D54's existing pin.

**D74 text.** The PLAN row, D74 in `docs/deviations.md` and the A82 row now say that each lease has one releasing owner and
name both cases; the A82 row records the Kotlin double release and the two new witnesses.

**Run for this round.** `~/.config/marathon-rs/local-check.sh`; `cargo fmt --check`; `cargo clippy -p marathon-ios
-p marathon-macos -p marathon-apple -p marathon-logging --all-targets -- -D warnings`; `cargo test` for marathon-ios,
marathon-macos, marathon-apple and marathon-logging, and `cargo test -p marathon-cli --test tracing_output`. The temporary
reverts above. Not run, per the batch rules: `scripts/check.sh`, the remote check, `cargo test --workspace`.
