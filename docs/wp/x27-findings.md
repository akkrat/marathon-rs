# X27: a lost simulator's reconnect has a limit (D44)

The project owner approved D44 on 2026-11-07. It fixes A101 and U164, makes any failed re-provisioning retryable, and folds in
A80 (`borrow`). No configuration field, CLI option or exit code changed. A healthy run sends the same commands, raises the
same events and offers `borrow` the same keys, so no comparator allowance is needed and no recording changed. No recording
reaches these paths; the JVM row `A80-stale-borrow` stays as the witness and the test still reads it.

Everything ran against the fake simulator worker, on a paused clock except where stated. **No real worker or SSH server was
used.**

## What was wrong

The rows are accurate. `recover` runs with `loss: None`, so no count or pause applies. `existing` threw its failures away. The
old handle stays in the device map, disconnected, so the next 16 ms tick starts again and raises another disconnect. One point
the rows don't state: the other two paths were final, as x14 noted. A simulator lost with its connection up, or added back,
whose re-provisioning failed stayed out until a devices update happened to plan its worker.

## The brief's route needed one change

The brief sends unreachable results to `retry_later`. That retry stops at the late-join cut-off, which is latched and by
default falls 600 s after the run starts, or as soon as any simulator has waited 30 s for a batch. Used as it stood, **any
worker outage in the rest of the run would have lost its simulators for good**, where Kotlin gets them back when the worker
returns. A simulator that has been in the run isn't joining late.

So the routing is as the brief says, but such a simulator is exempt from the cut-off, in the worker retry and in D33's
retry alike, and gets its own limit: `lateJoinCutoff`, counted from its first failure. There is no new setting. One still
awaiting its admission hasn't joined, and is treated as late.
`a_worker_that_comes_back_long_after_the_late_join_cut_off_gets_its_simulators_back_but_no_new_ones` pins both halves.

## The policy

`recover` keeps the first attempt at once. What `existing` couldn't set up is then **parked**, in the same critical section
that ends `recovering`, so no scan sees it in between:

| Failure | Goes to | Why |
|---|---|---|
| `unreachable`: no route, a dropped connection, D36's drop after login | `missing`, then `retry_later` | The worker is what failed |
| Failed with the worker answering | `schedule_setup_retry`, D33's budget | The budget is per target for the run, so this can't refill it |
| `refused`: authentication, configuration, D35 | Nobody, one ERROR line | Asking again changes nothing, and failed logins are what fail2ban counts |
| Registration rejected because the simulator left the file or the run | Nobody, not parked | Not a failed setup |
| Cancelled | Nobody | The run is ending |

A parked handle is skipped by the scan and is eligible for a plan on its worker (`current()`), which is how a retry can replace
it. Registration ends the parking. Plans run one at a time per worker under D33's gate, and a recovering handle is never
eligible, so one simulator can't be set up twice at once.

### Attempts and bounds, with the defaults

`attempts: 3`, `backoff: 10 s`, `retryInterval: 60 s`, `lateJoinCutoff: 600 s`. Measured by
`a_worker_that_stays_down_is_tried_at_capped_pauses_until_the_limit_and_then_left_alone`, on two simulators:

| | Before | Now |
|---|---|---|
| Pace, worker down | Every 16 ms plus the attempt's own time, up to about 3,750 attempts a minute per simulator | One attempt a minute per worker, after the first pauses |
| Attempts, 600 s | Up to about 37,500 per simulator, and it never ends | One per simulator at once, then 11 per worker: 10, 30, 90, 150 … 570 s |
| Connections per attempt | Up to 4 (sshj's immediate retries) | Up to 4, so at most 48 + 4 × (simulators − 1) |
| Worker answers, setup fails | Every 16 ms | 4 setups for the run (D33): 0, 30, 90, 180 s |
| Refused | Every 16 ms | 1 |
| Disconnect events | One per attempt | One |

Those attempts run one at a time on a worker however many simulators wait, so the count is per worker. **At the end** an ERROR
says `Simulator a@local has no connection 600 s after it lost its worker, it stays out of the run`, and the run carries on with
the rest. On a pool whose last device that was, the pool has already ended (C50; x31 owns it).

Other limits: `lateJoinCutoff: 0` means the end of the run, at the same capped pace. `lateJoin: false` means `attempts`.
`attempts: 1` with `lateJoin: false` means the first attempt only. That last one is a real change for those users, so it is in
the migration table. Termination cancels a waiting retry.

**Not limited:** a connection that keeps dropping after a setup that succeeded. Each round is a whole setup, which sets its pace,
and nothing counts successes, since a worker may reboot twice in a long run. It wasn't asked for and a count would be a
trade-off for the owner.

## What stays final

| State | Way back |
|---|---|
| Refused connection | Removed and added back by a devices update, or another plan on its worker |
| D33's budget spent (4 setups for the run) | The same; removal also clears the budget |
| Worker retry at its limit, or out of `attempts` with `lateJoin: false` | The same |
| A non-transient error inside the worker retry, which ends it for all its targets | The same |
| Lost more than three times with its connection up (`exhausted`) | None |
| Retired: session not closed in 70 s, or abandoned (D30) | None |
| A late joiner denied admission at the cut-off | None |
| Left the devices file | Added back |

A later plan picking up a parked simulator is read from the code and only tested through a devices update. If that setup fails,
the failure isn't among the plan's targets, so it schedules nothing.

## Exactly one actor

- `a_re_provisioning_that_fails_once_and_then_succeeds_gives_exactly_one_actor_after_a_worker_loss` and `…_with_the_connection_up`:
  one connect, one disconnect, the expected setups, and nothing more 300 s later.
- `a_re_provisioning_that_fails_once_gives_the_pool_exactly_one_replacement`: the same through the real `Scheduler` and pool on
  real time, with another device keeping the pool open. It counts connects and setups. `Scheduler` never calls the tracker's
  `device_disconnected`, so the disconnect count comes from the provider tests.
- `a_worker_that_comes_back_recovers_its_simulators_exactly_once`: two simulators, one replacement each.
- `a_scan_leaves_alone_a_handle_replaced_since_its_snapshot`: the stale snapshot at the scan, on real threads with the
  replacement held between snapshot and check, since that window has no await in it. The check is under the lock that marks
  `recovering`.
- A90 holds as before: the predecessor of a worker loss keeps its lease, and that of a connected loss is released at termination,
  also when a retry replaces it (`Parked::retained`).

## `borrow` (A80)

It was cheap. `borrow` now offers what is connected and neither retired, lost, on its way out, removed, exhausted, recovering nor
parked. With nothing left it waits at the same 200 ms. `extract_remotely` runs under `deviceInitializationTimeoutMillis`
(`crates/marathon-core/src/marathon.rs:544`), so a run with nothing usable ends in the existing `NoDevices` timeout, as D24 does
on Android. That bound is read, not run: no test here goes through a whole remote parse. In a healthy run every device passes
and the keys and their order are as before. `connected()` runs no command.

## The shared connection: documented, not fixed

With `shareWorkerConnection: true`, an abandoned session (D30) keeps its `MaxSessions` slot until the process or the connection
ends. Read from the code:

- Abandons are bounded: each retires its simulator, so there is at most one per simulator on the worker.
- A full connection gives `OpenFailException`, which `safe_execute` turns into no result, and that reads as state UNKNOWN. The
  connection is up, so nothing reconnects, and a simulator hit by this fails prepare and takes the connected-loss path. D44 makes
  its re-provisioning retry, but on the same full connection, so it ends out of the run after D33's budget.
- It needs a server that ignores signals and CLOSE, sharing turned on (the default is off), and enough simulators on one worker
  to fill the default 10 slots.

There is no loop and the bound is the budget, so this costs simulators, not a hang. A fix would have `ConnectionFactory` stop
sharing a connection with an abandoned session, in `crates/marathon-apple`, with its own connection accounting and a real-sshd
test. That isn't small for so rare a combination, so it is left for the owner to rank.

## What was verified

- `crates/marathon-ios/tests/simulator_reconnect_limit.rs`, 14 tests, and one new test in `simulator_pool_recovery.rs`.
- `cargo test -p marathon-ios`: 36 binaries, 299 tests pass. `cargo test -p marathon-cli`: 14 binaries, 39 tests pass.
- `cargo clippy -p marathon-ios --all-targets -- -D warnings` and `cargo fmt --check`.
- `tools/differential/make_registry.py --check` and `test_evidence.py` pass.

A temporary revert of each mechanism fails the tests shown:

| Reverted | Tests that fail |
|---|---|
| The scan skipping a parked handle | 7, including `a_refused_connection_is_not_tried_again` |
| The hand-over | 10 in the new file, and the scheduler test |
| The stale check under the lock | `a_scan_leaves_alone_a_handle_replaced_since_its_snapshot` |
| The exemption from the cut-off | 3, including `a_worker_that_comes_back_long_after_the_late_join_cut_off_…` |
| A parked handle being eligible for a plan | 6, and the scheduler test |
| The `borrow` filter | `a80_borrow_waits_…_d44` and both `borrow_…` tests |
| `Parked::retained` | `…_exactly_one_actor_with_the_connection_up` |
| The limit starting over on a re-add | `removing_a_waiting_simulator_ends_its_retry_and_adding_it_back_starts_over` |

## What was not verified

- A real worker, real sshd, fail2ban or sshd's penalties. The attempt counts are of the provider's calls, and "up to 4 connections"
  is read from `factory.rs`.
- A whole run through the CLI, or a whole remote parse with `borrow` waiting into the timeout.
- The whole provider on a multi-threaded runtime, apart from the stale-snapshot test.
- `scripts/check.sh` and the remote gate, which the batch's gate rule leaves to the orchestrator.

## Incidental bugs

No number was assigned for these, so they have no rows yet:

- **macOS `borrow`** has A80's shape: `AppleMacosProvider::borrow` picks from an unfiltered snapshot. Whether its map keeps
  disposed devices wasn't checked. Left alone, as A80 is an iOS row.
- **The shared connection** above.

Not a bug of ours: `Scheduler` doesn't report disconnects to the tracker, as `device_disconnected` has no caller in
`crates/marathon-core/src` outside the tracker pipeline's own wrappers. A search of Kotlin's `core` finds no caller there either.
