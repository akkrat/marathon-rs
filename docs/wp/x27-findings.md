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

A parked handle is skipped by the scan and is eligible for a plan on its worker (`current()`) while a retry still owns it,
which is how that retry replaces it. A refused one, or one whose retry gave up, is not (review round 2). Registration ends the parking. Plans run one at a time per worker under D33's gate, and a recovering handle is never
eligible, so one simulator can't be set up twice at once.

### Attempts and bounds, with the defaults

`attempts: 3`, `backoff: 10 s`, `retryInterval: 60 s`, `lateJoinCutoff: 600 s`. Measured by
`a_worker_that_stays_down_is_tried_at_capped_pauses_until_the_limit_and_then_left_alone`, on two simulators:

| | Before | Now |
|---|---|---|
| Pace, worker down | Every 16 ms plus the attempt's own time, up to about 3,750 attempts a minute per simulator | One attempt a minute per worker, after the first pauses |
| Attempts, 600 s | Up to about 37,500 per simulator, and it never ends | One per simulator at once, then 11 per worker: 10, 30, 90, 150 … 570 s |
| Acquisitions, 600 s | One per simulator per attempt, with no end | N + 11 × (N + 1) for N simulators: one planning connection per worker attempt, plus one per waiting simulator when planning gets through (review round 2) |
| Connections per acquisition | Up to 4 (sshj's immediate retries), one login | The same |
| Worker answers, setup fails | Every 16 ms | 4 setups for the run (D33): 0, 30, 90, 180 s |
| Refused | Every 16 ms | 1 |
| Disconnect events | One per attempt | One |

Those attempts run one at a time on a worker however many simulators wait, so the count is per worker. **At the end** an ERROR
says `Simulator a@local has no connection 600 s after it lost its worker, it stays out of the run`, and the run carries on with
the rest. The limit is looked at when a pause ends and after an attempt connects, so that line comes at 630 s with the defaults,
and at most one pause plus an attempt in flight late (review round 2). On a pool whose last device that was, the pool has already ended (C50; x31 owns it).

Other limits: `lateJoinCutoff: 0` means the end of the run, at the same capped pace. `lateJoin: false` means `attempts`.
`attempts: 1` with `lateJoin: false` means the first attempt only. That last one is a real change for those users, so it is in
the migration table. Termination cancels a waiting retry.

**Not limited:** a connection that keeps dropping after a setup that succeeded. Each round is a whole setup, which sets its pace,
and nothing counts successes, since a worker may reboot twice in a long run. It wasn't asked for and a count would be a
trade-off for the owner.

## What stays final

| State | Way back |
|---|---|
| Refused connection | Removed and added back by a devices update |
| D33's budget spent (4 failed setups, counted in every plan that tried it) | The same; removal also clears the budget |
| Worker retry at its limit, or out of `attempts` with `lateJoin: false`, or none configured | The same |
| A non-transient error inside the worker retry, which ends it for all its targets | The same |
| Lost more than three times with its connection up (`exhausted`) | None |
| Retired: session not closed in 70 s, or abandoned (D30) | None |
| A late joiner denied admission at the cut-off | None |
| Left the devices file | Added back |

A later plan can pick up a parked simulator that a retry still waits for, as Kotlin's planning does with a disposed one. If that
setup fails with the worker answering, the failure counts against the simulator's D33 budget; a refusal marks it refused; an
unreachable result leaves it to its own retry (review round 3).

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

- `crates/marathon-ios/tests/simulator_reconnect_limit.rs`, 14 tests, and one new test in `simulator_pool_recovery.rs`. Review
  round 2 adds four, below.
- `cargo test -p marathon-ios`: 36 binaries, 299 tests pass. `cargo test -p marathon-cli`: 14 binaries, 39 tests pass. After
  review round 2, on main `c03c08a5`: 308 and 48.
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

## Review round 2

A cross-family review found three things. The first is a code fix, and each of its tests fails with it reverted, which was
checked. The other two are about the text, and their tests pin the numbers it now states.

**A refused simulator was taken along by other plans** (major). Every parked handle was eligible for any plan on its worker,
refused ones too, so after a worker loss a sibling's D33 retries asked for the refused simulator's connection again, once per
retry. A parked handle now records why it is parked (`ParkedFor`): a retry owns it, it was refused, or its retry gave up.
Only the first is eligible, also when the handle is otherwise free after a connected loss. "Gave up" covers each point that
logs a simulator as out of the run: D33's budget spent, the worker retry at its limit, out of `attempts` or ended by a
non-transient error, and no retry configured. A refusal seen in any later plan marks the handle, also when that plan only took
it along. A devices update that adds it back makes it owned again. So the way back from every final state in the table above is
that update, and no longer another plan on the worker. Tests:

- `a_simulator_refused_after_a_worker_loss_is_not_set_up_again_by_a_siblings_setup_retries`: a refused, b failing setup on its
  D33 retries. One connection for a, five in all; before the fix a was taken along at 30 s and set up.
- `a_simulator_given_up_on_is_not_set_up_again_by_a_later_plan_on_its_worker`: D33's budget spent, and the worker retry at its
  limit, each followed by a devices update that adds another simulator on the worker.
- `a_refused_simulator_that_is_removed_and_added_back_is_set_up_again`: the way back.

**The connection bound left out simulator acquisitions** (minor). An attempt whose planning connection gets through also
acquires one connection per waiting simulator. With the defaults a worker loss acquires at most N + 11 × (N + 1) times, 13
for two simulators whose worker is down and 35 for two whose worker answers while their own connections fail. "Up to four" is
per acquisition, sshj's immediate retries, with one login. D44's text now says so. Test:
`a_worker_that_answers_while_its_simulators_cannot_connect_acquires_at_most_n_plus_11_times_n_plus_1`, which counts 35.

**The 600 s limit is not a deadline** (minor). It is looked at when a pause ends and after an attempt connects, so with
attempts that fail at once the end comes at 630 s, and it can be late by one pause plus an attempt in flight. Enforcing a
deadline would need a timer racing the attempt and its setup, so the docs state when it is noticed instead.
`a_worker_that_stays_down_is_tried_at_capped_pauses_until_the_limit_and_then_left_alone` now pins no error at 625 s and one
by 635 s.

## Review round 3

The re-check found three more, two from round 2. The reviewer's four witnesses are in `simulator_reconnect_limit.rs` under
names in the file's style; three failed on `ddbc2929` and pass now, and the fourth passed already and stays as the test of the
re-add claim. Each fix below was reverted on its own, and the tests named fail then.

**A D33 retry from before a removal gave up the simulator added back** (major, from round 2). A D33 retry waiting for its
connection outlived a removal and re-add, and its authentication error marked the new simulator given up, which stopped the
worker retry that the re-add had started. A D33 retry now acts only for targets whose generation is still current, when its
connection fails, before it plans, and on what its plan didn't set up (`unfinished.missing` was not filtered before). The other
writers of the parking state were checked for the same: a plan of any kind that outlives a re-add (the update bumps a per-handle
epoch) drops its results for that simulator, so a refusal it saw can't mark the new one. The worker retry needs nothing: a
re-add while it waits is offered to it, so it stays the only owner, and its limit counts from the re-add. The recovery's own
parking runs under the lock that checks the handle is current. Tests:
`a_setup_retry_from_before_a_removal_cannot_give_up_the_simulator_added_back` (the witness; generation check reverted: no
replacement connects) and `a_refusal_seen_by_a_plan_that_outlived_a_remove_and_re_add_does_not_stop_the_new_one` (epoch check
reverted: times out waiting for the connect).

**A refusal was lost when the planning connection dropped** (major, round 2 incomplete). The refusal was recorded only with the
planning connection still up, so a refusal followed by a dropped connection sent the simulator back to the worker retry, which
asked again. A refusal by the simulator's own connection is now recorded whatever the planning connection did, and that
simulator is not sent back. Tests: `a_refusal_in_a_later_plan_is_final_even_when_the_planning_connection_drops` (the witness:
a refused twice) and `a_refusal_stays_final_when_the_same_plan_loses_its_planning_connection`, where a and b are named by
the same plan (a connected again at 30 s).

**A sibling's plan spent setups outside the budget** (minor). A parked simulator taken along by another simulator's plan had
its failure thrown away, as it isn't among that plan's targets, so two simulators lost 5 s apart got 6 and 7 setups. Of the two
fixes offered, keeping parked simulators out of unrelated plans looked simpler but breaks the ported Kotlin test
`a_device_update_sets_up_again_a_simulator_whose_recovery_has_failed_and_leaves_the_healthy_one_alone`
(`AppleSimulatorProviderReconnectTest`): Kotlin's planning takes a disposed simulator along, and that is parity. So the failure
is charged instead: a setup of a taken-along simulator that fails with the worker answering goes to D33 like its own, and once
the budget is spent the simulator is given up and no plan takes it again. Every such failure is counted, so a simulator gets at
most four. An unreachable result is left to its owner, and a refusal marks it as above. Test:
`two_simulators_lost_with_their_connection_up_get_four_setups_each` (4 and 4; 6 and 7 with the charge reverted).

**Docs.** D44's table now says the budget counts every failed setup of the simulator, also in another's plan, at most four
with the first; that refusal means the simulator's own connection, also with the planning connection dropped; and that
removing and adding back gives one new attempt with its limits started over, not a recovery.
`every_given_up_simulator_comes_back_when_removed_and_added_back` pins that attempt for each way of giving up: the worker
retry's limit, out of `attempts`, no retry, and D33's budget. CHANGELOG and PLAN say the same; `migration.md` makes none of
these claims and is unchanged.

`cargo test -p marathon-ios`: 36 binaries, 314 tests pass; `cargo test -p marathon-cli`: 14 binaries, 48 pass; clippy and
`cargo fmt --check` clean.

**Not verified:** two retries for the same simulator racing in one plan on a multi-threaded runtime; the D33 rounds a charged
failure schedules alongside the simulator's own are bounded by the count, and their timing is only what the tests show.

## Incidental bugs

No number was assigned for these, so they have no rows yet:

- **macOS `borrow`** has A80's shape: `AppleMacosProvider::borrow` picks from an unfiltered snapshot. Whether its map keeps
  disposed devices wasn't checked. Left alone, as A80 is an iOS row.
- **The shared connection** above.

Not a bug of ours: `Scheduler` doesn't report disconnects to the tracker, as `device_disconnected` has no caller in
`crates/marathon-core/src` outside the tracker pipeline's own wrappers. A search of Kotlin's `core` finds no caller there either.
