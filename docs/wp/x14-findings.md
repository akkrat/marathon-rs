# X14: a failed simulator setup is retried (D33)

The project owner asked on 2026-11-06 for this fix to a bug the port had kept for parity (A81). It is an approved
deviation in PLAN §3.11. No configuration field, CLI option or exit code changed. Everything ran against the fake
simulator worker on a paused clock. No real simulator or SSH worker was used.

## The ledger row

A81 is accurate. `WorkerTracker::update` records a target when it emits `Connected` and never looks at the outcome, so
only a removal followed by an addition produces another setup. It is also wider than the row suggests: a static
devices file has exactly one snapshot, so a fix driven by snapshots would do nothing there. The retry is timed.

What the row calls "explicit 5.5b transient retries" are two other policies, which this one does not multiply:

| Policy | Trigger | Bound | Owner |
|---|---|---|---|
| Worker connection retry | The worker or a simulator's connection is unreachable (`missing`, `connect_failed`) | `workerConnection`: attempts, backoff, `retryInterval`, cut-off | `retry` in `recovery.rs`, unchanged |
| Recovery of a lost simulator | A registered simulator is lost | `max_recoveries` (3) losses, 30 s × (loss − 1) | `recover`, unchanged |
| **D33** | Setup failed while the worker answered | `max_recoveries` (3) retries, 30 s × retry | `recovery/setup_retry.rs` |

## The policy

- **How many:** three retries per target for the run, so four setups at most. This is `max_recoveries`, the count a lost
  simulator already gets. It is not configurable, as that count is not.
- **How long between them:** 30, 60 and 90 s after each failure (`recovery_pause_millis` × retry), through the vendor
  clock. With instant failures the setups run at 0, 30, 90 and 180 s.
- **After the last one:** an error, `Simulator a on worker w: setup failed 4 times, it stays out of the run`. The target
  stays tracked and out of the run, as it does today, until a snapshot removes it and adds it back. That starts a new
  budget, since the devices file changed and its owner bounds how often.
- **Off switch:** `attempts: 1` with `lateJoin: false`, the existing way to say a worker's devices get no retries.

No new setting was needed, so nothing awaits an owner's decision on configuration.

### What counts as a failed setup

| Failure | Who takes it |
|---|---|
| Factory or device setup, `simctl create` failing or returning nothing, registration failing | D33 |
| Planning fails while the worker stays connected (`Failed to initialize worker`) | D33, all targets of the cycle |
| The worker or the simulator's connection is unreachable, or the connection was lost | Worker retry, as before |
| The connection fails for a reason a retry doesn't change, such as authentication | Nobody, as before (new `refused` flag on `FailedSetup`) |
| `udid … is not available`, profile not available on the worker | Nobody, as before: not a failed setup, and the plan says so |
| Rejected by registration because the simulator left the devices file or is out of the run | Nobody: filtered when scheduling |
| A registered simulator's own recovery fails (`provisioner.existing`) | Recovery, unchanged; see the last section |

A failure of a late worker's setup counts too, and the budget is kept by target for the run. So an attempt that finds the
worker unreachable goes to the worker retry, and a failure there spends the same budget. The test
`an_unreachable_worker_during_a_retry_goes_to_its_connection_retry_without_refilling_the_budget` pins this. The worker
retry's own attempts are bounded by its own settings and are additive, not multiplied.

The instances of a profile share one key. Those failing in the same cycle count once and are retried together, so a
profile of N costs at most N × 4 setups.

### Races

- **One planned cycle per worker.** A new per-worker gate in `AppleSimulatorProvider::setup` covers the first, late
  and retried cycles. Without it, a retry could reuse a simulator another cycle is setting up, or shut it down under
  `shutdownUnused`, since it isn't registered yet. The lock is polled first, so an uncontended gate changes nothing.
  A recovery's re-provisioning (`provisioner.existing` in `recover`) is **not** gated. It has no plan, and it sets up
  simulators that are registered and marked recovering, which plans leave out.
- **Removal during the pause.** `forget_setup` bumps the target's generation, clears its budget and empties it from the
  due list. A timer that fires later drops the stale entry, even if the simulator has been added back.
- **Removal during an attempt.** `register` rejects it under the state lock, as it already did, so there is no connect
  event. The failure isn't scheduled again.
- **Added back during an attempt.** The snapshot's setup waits at the gate. The attempt's registration succeeds, and the
  plan then keeps the registered simulator out, so there is one setup and one actor.
  `a_simulator_added_back_during_its_retry_has_one_setup_and_one_actor`.
- **Event order.** Events used to be sent by one spawned task each, so a disconnect could overtake a connect. They now
  pass through an outbox, in the order raised, and each task pops and sends under one lock acquisition, so a task
  can't be overtaken between the two on a multi-threaded runtime. `try_send` only pushes to a queue, so the lock is
  never held across a wait. A lost device's disconnect goes through the outbox too. Before this it was sent directly
  and returned a closed channel's error to monitoring. That error cannot occur, because the channel closes after
  monitoring is joined, and termination now joins the publishing tasks before it closes the channel.
- **Removal right after a first registration.** The connect is queued inside the critical section that publishes the
  device, so a removal that sees the device raises its disconnect later.
- **Removal while a replacement waits for its predecessor's disposal.** The connect has to wait for the disposal, as in
  Kotlin. Publishing the disconnect first would let the later `AddDevice` put a removed, disposed simulator into a pool
  that other devices keep alive. Plans replace only simulators whose disposal has completed, so in production the window
  is the gap between the two critical sections on a multi-threaded runtime; a slow disposal is only reached by calling
  `register` directly, as the tests do. So the replacement is marked as pending inside the same critical section. A
  removal in the window only flags it, and the connect and disconnect go out together when the disposal step ends. If
  that step fails (only the identity text can fail it, as `dispose` itself reports no error), only the disconnect goes
  out. This gives `AddDevice` then `RemoveDevice` for the same instance. If another instance holds the id by then, this
  one raises neither: the pool never saw it, and its events carry the newer device's serial. Only that device's
  disconnect waits, so another device's events aren't held up. The recovery scan skips a pending replacement until the
  next tick, for the same order.
- **Admission.** A retried simulator is admitted as a late one. The mark is a task-local around the retry attempt's
  setup, since registration runs in that task. It isn't a transport, which would make a healthy simulator of a
  snapshot late while a retry waits at the gate, and it isn't an id, as a profile's simulator has none until the attempt
  creates it.
- **Cost to other workers.** Gates are per worker, so another worker's setup in the same snapshot runs alongside. A
  snapshot that adds a device to the busy worker waits for its attempt, and later snapshots queue behind that one, as
  they do behind any slow setup. The wait is one attempt, bounded by the command timeouts.

### Resources from a failed attempt

**Cleaned up, then replaced.** The provisioner already deletes a simulator it created when its setup fails (`create`),
and that stays. The retry plans afresh and creates one replacement, so `simctl create` runs once per attempt but the
worker never holds more than one per instance: 3 creates and 2 deletes for two failures. If the delete failed, the
leftover is available and matches the profile, so the plan reuses it and creates nothing:
`a_managed_simulator_that_couldn_t_be_deleted_is_reused_instead_of_creating_another`. A reused simulator is not deleted
on failure, as in Kotlin, and is planned again. The device and its lease are disposed and released on each failure, and
each attempt's planning lease is released even when the attempt fails.

### Ending a run

Provider termination cancels `retry_cancel`, which ends the pauses and in-flight attempts, and then joins them. So it
doesn't wait out a pause, but it does wait for an attempt in flight to stop, including that attempt's cleanup, such as
deleting a simulator it created: `terminating_cancels_a_pending_retry_without_waiting_for_its_pause`. An attempt blocked
on its connection doesn't hold it up, as the connection is detached:
`terminating_doesn_t_wait_for_a_retry_blocked_on_its_connection`.

The no-devices timeout needs no change. `Scheduler::execute` waits for a pool for `deviceInitializationTimeoutMillis`
whatever the provider's tasks are doing, then fails with `NoDevicesException`. The default is 180 s, so with the default
settings the retries at 30 and 90 s can still rescue a run with no devices, while the one at 180 s races the timeout and
only matters when another device has opened a pool. Two tests run this through the real `Scheduler` with every setup
failing (`crates/marathon-ios/tests/simulator_pool_recovery.rs`, `every_device_failing_ends_in_no_devices_*`): one with
the retries run out before termination, one with a retry still pending. Both end in `NoDevicesException`, not before
the timeout, with no pool, and the second terminates without waiting out its pause. They use a 300 ms timeout and real
time, with the lower bound asserted. Once a run has what it needs, the late-join cut-off stops further
retries: 600 s after the start by default, or once a simulator has waited 30 s for a batch. Reaching it from a retry
latches it as any other caller does (approved P2).

### Other vendors

The macOS provider does not share this tracker: `DeviceTracker` and `WorkerTracker` are used only in
`crates/marathon-ios`, and `crates/marathon-config/src/devices.rs` merely mentions them. macOS is out of scope and unaffected.

## Output and evidence

**A healthy run is unchanged:** the same commands in the same order, the same TimingLog lines. The only new code on its
path is the uncontended gate, the outbox and the map lookups. The differential cases (`tools/differential/vendors.py`) all
set up successfully, so no recording reaches this path, and **no comparator allowance is needed**. No recording was
touched. The JVM row `A81-failed-setup-remains-tracked` remains the witness of the old behaviour, and its value still
holds at snapshot level, so the renamed test still asserts it.

**With a failing setup**, per failed round: one WARN `Worker w: setup of a failed, setting up again in 30 s (retry 1 of 3)`
and the existing ERROR for the failure. One TimingLog line
`Worker w: set up again after a failed setup, planned N simulators in A ms, provisioned them in B ms` after each attempt
that completes its provisioning round, **whether or not any simulator was set up**, so an attempt in which all fail logs
it too; an attempt whose planning fails does not. Also the setup's usual commands and timing lines. At most one final
ERROR per target. Nothing runs or logs on snapshots.

## What was verified

- `crates/marathon-ios/tests/simulator_setup_retry.rs`: 26 tests on the policy, the races, the managed path, termination,
  several workers, the cut-off, admission and event order. Positive counts wait on the harness's `eventually`, for a
  log line or the count itself, and only checks that nothing more happens run after `advance`.
- Two unit tests in the provider and in `setup_retry.rs`, and the two scheduler tests above.
- A temporary revert of each mechanism fails the test made for it. These use explicit barriers rather than timing:

| Reverted | Test that fails |
|---|---|
| The per-worker gate | `a_simulator_added_back_during_its_retry_has_one_setup_and_one_actor` |
| Outbox order (`pop_back`, which gives the old order when the newer task runs first) | `a_connect_raised_before_a_removal_is_published_first_whatever_order_their_tasks_run_in` |
| The send under the pop's lock | `an_event_being_sent_holds_back_the_next_one_on_another_thread` (real threads, multi-thread runtime) |
| The lost device's disconnect through the outbox | `a_lost_device_s_disconnect_doesn_t_overtake_its_queued_connect` |
| The disconnect waiting for a replacement's connect | `a_removal_during_a_predecessor_s_disposal_follows_the_replacement_s_connect_and_holds_up_nobody_else` |
| Admission by the attempt's task, not the transport | `a_healthy_simulator_added_while_a_retry_waits_for_its_turn_is_not_a_late_one` |
| Scheduling despite a failed planning-lease release | `a_failed_release_of_the_planning_lease_keeps_the_retry_of_the_first_cycle` |
| Re-checking `due` before a failed runner stops | `a_runner_stopping_on_an_error_leaves_running_if_a_batch_arrived_meanwhile` (the decision under the lock; the window itself is not driven) |
| `forget_setup` | `a_retry_from_before_a_removal_doesn_t_act_on_the_simulator_added_back_during_its_pause` |
| The whole retry | 17 of the 22 tests that existed then |

## What was not verified

- A real simulator or SSH worker failing setup and recovering.
- A whole run through the CLI with every device failing: the scheduler tests stop at `Scheduler::execute`.
- The whole provider on a multi-threaded runtime. The real-thread test covers the outbox itself. The provider-level
  order tests force their interleavings on one thread, through a holding runtime or a gate.
- A replacement's failing disposal: no test makes the predecessor's `dispose` fail, so the branch that raises the
  disconnect alone was read, not run.
- `scripts/check.sh` and the remote gate, which the batch's gate rule leaves to the orchestrator.

## Known limits

- A profile target has no removal: tracking keeps profiles at their high-water mark (A52). So a profile's retries carry
  on after its worker leaves the snapshot, until the budget or the cut-off ends them, like the worker retry's pending
  profiles. **Such a retry can still create a simulator on the removed worker** if it is reachable. This was read from
  the code, and no test pins it.
- Instances of a profile added by a later snapshot share the budget the earlier ones have spent.

## Incidental bugs

No bug of our own code was found. Behaviour seen nearby and left alone:

- **A registered simulator whose re-provisioning fails** in `recover` isn't set up again until a snapshot adds a device
  to its worker, or it is lost again. That is recovery's policy, not A81.
- **`udid … is not available` is final for the run**, even if the simulator appears on the worker moments later. As in
  Kotlin, and not a failed setup.
