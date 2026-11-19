# x90 — a preparation owns its rsync transfer; bounded, verified cancellation (D91)

The x86 fix for U210 made an iOS preparation wait up to 70 s for the send it was making when it was cancelled. Its
known limit: that bound ended the wait, not the send, which stayed with the rsync bridge's retained task, and nothing
but the run's end waited for that task. This package gives the device ownership of its sends and the bridge one bound on
a stopped transfer's cleanup, with an explicit outcome when the cleanup cannot be confirmed. Approved as D91
(2026-11-18, project owner's decision); amends D31 and D54.

## The evidence, re-verified

Checked against f7a02f59.

1. **Retained task** (`crates/marathon-apple/src/cmd/remote/rsync/rsync_file_bridge.rs`, `owned`, ~125). A send runs in
   a task the bridge retains; dropping the awaiter leaves it running. Only `join_pending` (tests, the end of the run)
   joins it. Confirmed.
2. **Unbounded waits** (same file). After KILL, `stop_child` awaited `child.wait()` with no limit (~495); on a failed
   wait `process` awaited `child.wait()` again with no limit (~357); after the readers' 5 s it cancelled them and joined
   both reader tasks with no limit (~371). Confirmed. While it waited, the bridge's `send` lock was held, so every later
   send to that worker waited too.
3. **Shared bridge** (`crates/marathon-apple/src/device/connection_factory.rs`, `file_bridge`, ~571). Bridges are cached
   by worker address and port; every simulator on a worker uses one. Draining the bridge for one device would wait for its
   siblings' transfers. Confirmed; the design does not drain it.
4. **Disposal** (`crates/marathon-ios/src/apple_simulator_device/execution.rs`, `stop_and_join`). It stops and waits for
   the batch and the preparations, not for sends. A send whose waiter has gone (discovery's installer at its timeout, a
   preparation's attempt after x86's 70 s) is waited for by nobody. Confirmed.
5. **Readers.** `read_stderr` put the pipe's read before the stop in a biased select. Measured by reading tokio: the
   cooperative budget lets the stop through after at most 128 reads, so this was not unbounded in practice; it is
   reordered anyway (a stop first), since a reader that was told to end has no reason to read on.

## Design

**Ownership: a receipt carried by the stop.** `TransferReceipt` (new, `crates/marathon-apple/src/cmd/transfer_receipt.rs`)
is a device's hold on one transfer: a stop signal of its own, an optional earlier deadline, and a completion cell with
`TransferCleanup::{Confirmed, Unconfirmed(Failure)}`. `TransferStop::owned_by(receipt)` attaches it. Whoever holds the
`TransferStop` owns the transfer; when it lets go (its `Drop`), the receipt ends, with `Unconfirmed` if the owner said so
(or if it is unwinding from a panic). For the rsync bridge the stop is moved into the retained task and dropped when that
task's body ends, after the cleanup; a send stopped while queued for the worker's lock drops it at once (it owned only its
own request). Nothing can end the receipt while the transfer's owner still holds the stop: ownership is structural, not
an observation window. A bridge that ignores the stop (a local copy) drops it at once, which is right, since nothing of
it is owned.

**Registration under the admission lock (iOS).** `AppleSimulatorDevice::send_stop` creates the receipt and, under the
device's state lock where `disposal` is checked (the lock U179 and U210 use for preparations), either registers it or,
after disposal, stops it before the send starts. `dispose_and_join` takes the unended receipts under the same lock as it
installs `disposal`; `join_cancelled_work` (D84's wait) takes them too. `stop_and_join` stops each (`Device <serial> is
being disposed`, with its own deadline as the device's end for the cleanup) and waits for them inside its existing
`cancelled_batch_millis` bound, after the batch and preparations, before the on-dispose lifecycle. The cancelled batch's
own join (`join_cancelled`) is unchanged.

**One deadline from a stop** (as first built; review round 1 below makes it a step runner that sees a stop in any phase, with the same limits). In `process`, the stop branch sets `Bound::at = now + 40 s` on the bridge's vendor clock
(`STOP_CLEANUP_MILLIS`: D54's 15 s of steps, 20 s of listings, 5 s for the last reap). `Bound::run(real, vendor, body)`
races a step against its own limit (real time for `ps`, the reap and the readers, vendor time for the 5 s steps) and what
is left of the deadline, and against the device's deadline in the receipt, read at every step, so one given after the
stop began still counts. A step with nothing left still polls its body once (a reap already done is seen); a listing
with nothing left is not started. A vendor clock that fails falls back to real time. The stall path (D31, D43) keeps its
own steps, now each with a limit (the reap after KILL `REAP_LIMIT`, 5 s real); it has no overall deadline, because its
tests run on a clock compressed 100 times and their `ps` listings must not be cut short. A cancellation is checked again
inside the blocking spawn, just before rsync starts.

**Order after a stop.** What rsync started is ended (`Descendants::end`) before the readers are waited for, since those
processes hold the pipes; the stall path keeps its order, since it needs rsync's stderr to decide the stall.

**Confirmation.** `Descendants::end` now returns which of the processes ever seen under the transfer are still running
(same pid and start time), looked at for up to 5 s while init reaps orphans, or `None` if no listing could be had. The
cleanup is confirmed only if rsync was reaped, both readers were joined and that list is empty (or no stop, stall or
failed reap called for it). Otherwise `process` reports an `Unconfirmed` failure naming rsync's pid, the worker and what
was left; `transfer` logs one ERROR line with it as the cause, records the destination (trailing `/` trimmed) as
quarantined, and marks the receipt. A later transfer into a quarantined destination does not start: a send fails as a
stalled one does (`LeavePool`, `TransferStalled`, so neither the installer nor the device actor repeats it and an iOS
simulator is retired), a receive returns `false` with an ERROR line. The stopped transfer returns what D54 says.

## Invariants

- A registered send's receipt ends only when the task holding its stop lets go of it, after the process's cleanup.
- An iOS simulator either registers a send before it starts, and then its disposal (and D84's wait) stops and waits for
  it, or is already disposed, and the send is stopped before it starts.
- From a stop to the end of the bridge's task: at most 40 s on the vendor clock (or the device's earlier end), plus the
  scheduling of one more poll. No wait in the bridge is unbounded, on any path.
- A cleanup is published as confirmed only after rsync's reap, both readers' end and a listing with nothing it started
  left; otherwise it is reported and its destination is never transferred into again by that bridge.
- One device's stop never waits for, or stops, another device's transfer on the same bridge.

## Preserved

D54's ordered signalling under `sshpass` (unchanged `Descendants::signal`; the existing `*_u181` tests pass), receives
that never take their caller's cancellation (not registered), discovery's prompt return (its installer still leaves the
send at its timeout; the device waits for it later), D74's disposal joins (one bound, now with the sends inside it), D87's
halt and refusal semantics (a quarantined send is a `TransferStalled` leave, which D87 already treats as terminal),
x86's opt-in bounded wait (the installer still waits up to 70 s; the bridge now ends within 40 s of the stop, so that
wait sees the end).

## Verification

| Witness | What it shows | On f7a02f59 |
|---|---|---|
| `marathon-apple/tests/rsync_file_bridge.rs::a_stopped_send_whose_output_is_never_joined_ends_at_the_bound_and_its_destination_is_not_used_again_d91` (child process; a fake rsync announces its pid and holds its pipes through a child; a test runtime never ends the joins of the first send's two reader tasks; vendor clock 10× real) | the stopped send returns `Cancelled` between 40 s and 70 s on the bridge's clock after the stop, with the ERROR line naming rsync's pid; the other simulator's send queued behind it runs; a later send to the same destination is refused without starting rsync (the fake's order file: `stuck`, `next`) | fails: the send never returned (60 s guard) |
| `marathon-ios/tests/simulator_dispose.rs::disposal_waits_for_a_send_whose_caller_stopped_waiting_d91` (virtual time; discovery's installer times out and lets go of a send whose task takes 3 s to stop) | disposal returns only after the send has ended, 3 s after it began | fails: disposed of before the send ended |
| `marathon-ios/tests/simulator_dispose.rs::a_send_to_a_disposed_simulator_is_stopped_before_it_starts_d91` (virtual time) | a send after disposal is stopped with `has been disposed` | fails: never stopped (guard) |
| `marathon-apple/tests/rsync_file_bridge.rs::a_stopped_send_whose_caller_has_gone_is_reaped_and_its_output_closed_before_its_receipt_ends_d91` (child process; rsync and its ssh both ignore TERM, ssh holds rsync's output; the caller is aborted after the stop; vendor clock 2× real) | when the receipt ends it is `Confirmed`, rsync's pid is reaped (ESRCH) and ssh is gone; another device's send on the same bridge, with a receipt of its own, then succeeds and ends its receipt `Confirmed` (the healthy case); no ERROR | not buildable there (it needs the receipt API); the behaviour it pins besides the receipt (reap, kill, sibling progress) also held there |

The "fails on f7a02f59" column was checked in a scratch worktree of f7a02f59 with only the new tests copied in (and the
fourth left out).

"Never-ready final wait" is witnessed through the readers' join, the last wait in the bridge, held by a test runtime: the
reap after KILL cannot be made to hang with a real process, and no production hook was added for it. The bound on that
reap is the same `Bound::run` and is covered by inspection.

Existing suites pass unchanged, including every D31, D43, D54, U158 and U181 test in `rsync_file_bridge.rs` (stalls,
real rsync and ssh, `sshpass` 1.06/1.09/1.10 behaviour) and the x86 U210 witnesses.

Stress, per witness, each run alone (`--exact`); "loaded" is 20 more runs with 12 busy processes on the CPU:

| Witness | Runs | Loaded |
|---|---|---|
| `a_stopped_send_whose_output_is_never_joined_ends_at_the_bound_and_its_destination_is_not_used_again_d91` | 50/50 | 20/20 |
| `a_stopped_send_whose_caller_has_gone_is_reaped_and_its_output_closed_before_its_receipt_ends_d91` | 50/50 | 20/20 |
| `disposal_waits_for_a_send_whose_caller_stopped_waiting_d91` | 50/50 | 20/20 |
| `a_send_to_a_disposed_simulator_is_stopped_before_it_starts_d91` | 50/50 | 20/20 |

The self-review then moved the survivor look's 5 s from the vendor clock to real time (`REAP_LIMIT`): D31's stall tests
run on a clock 100 times fast, where 5 s of vendor time gave init 50 ms to reap a killed orphan. After that change the
whole `rsync_file_bridge.rs` suite (64 tests, the D91 witnesses among them) passed three times in a row under the same
load.

Run on this Mac: `cargo fmt --check`; `cargo clippy -p marathon-apple -p marathon-ios -p marathon-macos --all-targets --
-D warnings`; `cargo test --no-fail-fast` of `marathon-apple`, `marathon-ios`, `marathon-macos`, `marathon-cli` and
`marathon-test-support` (1,173 passed, 33 ignored as on the base; the first run's four differential failures were the
missing YAML helper, built and rerun); `scripts/check_plan_dag.py`; `tools/differential/test_evidence.py`;
`tools/differential/run validate`; `tools/fake-worker/test_apple_evidence.py`. Not run, by the batch's rule:
`scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Not verified

A real worker: a real rsync over a real SSH link cancelled mid-transfer with a sibling simulator on the same worker
(the post-merge real-device acceptance). A kernel that delays the reap after KILL. The Linux gate.

## Golden diffs

None. No result file, worker command, pinned text or exit code of a run changes; no oracle recording is touched. The
only new outcome (a quarantined destination making a device leave its pool) needs a cleanup that could not be confirmed,
which no golden or recording has.

## Incidental bugs

None new (no U217). Out of scope and left as described: a Mac's sends are not registered with the Mac, whose
preparation runs under a context that is never cancelled; the Mac's disposal and lease release are x92's. An orphan
that `ps` never saw (between two of D31's listings) is neither signalled nor counted by the confirmation; containing it
needs rsync under a process group or a reaper of the bridge's own (a separate design).

## Review round 1 (Sol 6.2 xhigh, REQUEST_CHANGES: 6 P2, 1 P3)

The orchestrator asked for one primitive rather than six patches. It is two pieces, both in the bridge.

**One watcher, one step runner.** `Inner::transfer` turns the `TransferStop` into a `Stopping` that one future watches
for the transfer's whole life, beside the transfer's own work, which it owns: the stop's reason, its deadline (40 s from
the stop, or the device's earlier one) and a notification on every change. Every wait of the transfer is a step of
`Bound::run`: the look at the source and the binaries before rsync starts, its start, the run, a stall's steps, the
reaps, the readers' joins, every process listing. A step races its own limit and, once there is a stop, what is left of
the deadline, and it reads both again whenever the stop or the device's deadline changes (`TransferReceipt` notifies,
and keeps the earliest deadline it is given), so an earlier deadline ends a step already under way. A step marked as one
that a stop ends (the work before rsync starts, the stall's 5 s of grace, the readers' join before what rsync started
has been ended) returns at the stop. The end of a transfer that saw a stop at any point is confirmed only by the stop's
own cleanup: what rsync started ended (TERM, 5 s, KILL), rsync reaped, its output read or given up, and a listing that
finds none of it.

**One admission for rsync's start.** The thread that starts rsync checks, under the quarantine's lock, a gate (closed by
a stop whose deadline passes first), the stop, and the destination's quarantine, and starts rsync under that lock; the
quarantine is recorded under the same lock. rsync is started with `kill_on_drop`, so one that starts after its transfer
gave up on it is killed when it is dropped. A transfer whose end could not be confirmed never returns a success: a
stopped one returns its stop even when rsync exited 0, any other fails as a stall does (a receive returns `false`).
`SharedTransfers` keeps only successes, so it never keeps such a send as the bundle's, and the next simulator's send
reaches the bridge, which refuses the quarantined destination. `SharedTransfers` itself is unchanged; a destination
quarantined by a send outside it (not a shared one) is not seen by its cache, which only shared sends fill.

| Finding | Status | Witness (child process; fails on 7f5807d6) | On 7f5807d6 |
|---|---|---|---|
| 1 P2: a stop while an ended rsync's output is read confirmed the end with a listed child still holding the pipe | fixed | `a_stop_while_the_output_of_an_ended_rsync_is_read_ends_what_holds_it_before_confirming_d91` (rsync exits 0 after a listing has seen its child; the receipt is stopped once rsync is reaped): the receipt ends `Confirmed` only with the child gone; the send keeps its success | confirmed while the child ran |
| 2 P2: a stop during a stall's cleanup was not bounded by 40 s | fixed | `a_stop_while_a_stall_is_being_ended_ends_it_within_the_stops_bound_d91` (clock 100×; the stall's first listing, 2 s here, under way; rsync and a child holding its output ignore TERM): returns `Cancelled` within 3 s of the stop (0.4 s bound) | about 15 s |
| 3 P2: a send stuck before rsync starts held the worker | fixed | `a_stop_ends_a_send_stuck_before_rsync_starts_and_the_send_behind_it_goes_on_d91` (the look at the source never returns): `Cancelled` at once, receipt `Confirmed`, the queued send runs, rsync never started for the first | never returned (60 s guard) |
| 4 P2: an earlier device deadline did not shorten a step under way | fixed | `an_earlier_deadline_from_the_device_ends_a_step_already_under_way_d91` (the stop's first listing takes 8 s; the device then asks for an end 1 s on): returns within 3 s | about 5 s (the step's own limit) |
| 5 P2: a shared bundle whose send's end was not confirmed was reused | fixed | `a_shared_send_whose_end_is_not_confirmed_is_not_reused_d91` (rsync exits 0 on TERM; the readers' joins never end): the stopped send returns its stop; the next request for the bundle through `SharedTransfers` gets `LeavePool`, and rsync ran once | first send `Ok(true)`, kept and reused |
| 6 P2: a receive waiting to start could start into a folder quarantined meanwhile | fixed | `a_receive_waiting_to_start_when_its_folder_is_quarantined_does_not_start_d91` (the second receive's blocking start-up is held while the first is stopped with a 1 s device deadline and quarantined): the second returns `false` and rsync started once | the second started rsync (60 s guard) |
| 7 P3: the late-send witness proved only that the stop was delivered | fixed | `simulator_dispose.rs::a_send_to_a_disposed_simulator_is_stopped_before_it_starts_d91` now uses a bridge that starts a transfer unless its stop has come and counts what starts: none | passes on 7f5807d6, whose refusal it pins; fails on f7a02f59 (`Ok(true)`, a transfer started) |

Each was checked in a scratch worktree of 7f5807d6 with only the new tests copied in (the P3 one also on f7a02f59).
The test runtime `HeldJoins` now also holds the joins of chosen blocking calls and gates one until the test opens it;
`counting_ps` counts listings and can be made slow.

Note on the admission lock: it is a `std` mutex held while rsync is forked, inside the blocking thread that starts it;
the bridge's own task takes it only to check or record a quarantine, for as long as a fork takes at worst.

Stress after round 1 (the primitive changed every path, so all ten D91 witnesses were run again), each alone with
`--exact`: 50/50 for every one, and 20/20 for every one with 12 busy processes on the CPU. The first full run of the
crates' tests found one race in the stall witness's own stand-in (the test could read its listing count while the shell
was still writing it); it now moves a finished file into place.

Run after round 1: `cargo fmt --check`; `cargo clippy -p marathon-apple -p marathon-ios -p marathon-macos --all-targets
-- -D warnings`; `cargo test --no-fail-fast` of `marathon-apple`, `marathon-ios`, `marathon-macos`, `marathon-cli` and
`marathon-test-support` (1,184 passed, 33 ignored; the stall witness's race above, then `rsync_file_bridge.rs` again,
76/76). Not run, by the batch's rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Review round 2 (Sol 6.2 xhigh): findings 1, 4, 5, 6, 7 closed; 2 and 3 open through three P2s of one class

The class: a synchronous call that a filesystem that no longer answers can hold (a lookup on `PATH`, `Path::exists`, a
process start) either held a `std` mutex that the stop's own path needed, or ran inside the task before any timer was
polled. The structural rule now: no lock is held across a blocking call or a process start, and no async path takes a
lock that blocking work can hold.

- **Starting rsync** is a ticket (`SpawnTicket`, one atomic). The transfer's task, in one short check under the admission
  lock, refuses on a stop or a quarantine and records the ticket by destination; the start then runs on a blocking
  thread with no lock held: it begins only if the ticket was not refused, starts rsync, and commits only if it still was
  not. Giving up on a start (a stop before it began, or the deadline after) and quarantining a destination only mark the
  ticket; a start refused after it began is killed at its commit (`kill_on_drop`). Quarantining never waits for a start.
- **The look for the binaries** is a `tokio::sync::OnceCell` whose initialisation runs on a blocking thread and whose
  waiters wait asynchronously, each within its own stop. The command line reads it without waiting; a look held by a
  mount is left behind by a stopped send, and the next send looks again.
- **Starting `ps`** happens on a blocking thread inside the listing's step, so the step's 5 s and the stop's deadline
  cover the start too; a `ps` given up is killed when dropped. (Signals are syscalls that start nothing and do not look
  anything up on a filesystem; they are sent directly, as before.)

The witnesses need to hold a blocking call as a hung mount would, which no real process can be made to do on demand, so
the bridge has a test hook (`RsyncFileBridge::on_blocking_call`, `#[doc(hidden)]`), called on the blocking thread just
before each such call. The revert check put the same hook, at the same three calls, into a scratch worktree of 3d945010
with no other change, and copied the new tests in.

| Finding | Status | Witness (child process) | On 3d945010 with only the hook |
|---|---|---|---|
| R2-1: a hanging start of rsync held the lock its own stop needed to record the quarantine, and that other transfers needed to be admitted | fixed | `a_hanging_start_of_rsync_holds_up_neither_its_own_stop_nor_another_transfer_d91`: another transfer on the bridge (a pull elsewhere) starts and ends while the start hangs; the stopped send ends within 3 s of its device's 1 s deadline, its receipt `Unconfirmed`; when the start comes back nothing is left, and a later send there is refused | the other transfer never started (60 s guard) |
| R2-2: a hanging look for the binaries kept its lock after its send was stopped, and the next send waited on it building its command line | fixed | `a_hanging_look_for_rsync_holds_up_no_later_send_d91`: the stopped send ends, the next looks again and succeeds while the first look still hangs | the next send never returned (60 s guard) |
| R2-3: `ps` was started inside the task before its limit or the stop's deadline was looked at | fixed | `a_hanging_start_of_ps_does_not_hold_a_stops_cleanup_past_its_deadline_d91`: with every start of `ps` held, the stop's cleanup ends within 3 s of the device's 1 s deadline | the cleanup never ended (60 s guard) |

**Follow-up (not required, recorded here):** the public API can express a direct send (`push_file`, not through
`SharedTransfers`) into a destination whose shared send `SharedTransfers` has kept as done; if that direct send's end
were unconfirmed, the cache would not know of the quarantine. No caller in the tree does this: shared destinations are
only ever written through `SharedTransfers`.

Stress after round 2: all thirteen D91 witnesses 50/50 alone and 20/20 under load, except the R2-1 witness at first
(48/50, 19/20). Two causes, found by reading, since 330 further runs did not reproduce it: (1) the bridge's retained
task sent a transfer's result before dropping the body that holds its stop, so a caller could hear the result before the
device's receipt had ended; the body is now dropped first, so hearing the result means the receipt has ended (a real
ordering guarantee, not only a test fix); (2) the witness could end while the start it had let go of was still being
killed, and the process exit then left that start's rsync behind for the parent's check to find; the hook now also marks
the end of each start (`SpawnedRsync`), and the witness waits for it. Its time bound was also widened from 3 s to 20 s
(it ends in about 1 s; before the fix it never ended). After both: the R2 witnesses and the round-1 one that reads its
receipt at once, 50/50 alone and 40/40 under load.

## Review round 3 (Sol 6.2): R2-1 and R2-3 closed; R2-2 open (P2) and three follow-ups

| Item | Fix | Witness | On 0c243c99 |
|---|---|---|---|
| R2-2 P2: the look for the binaries wrote rsync4j's missing-`sshpass` warning to stderr itself, before publishing; with stderr a full pipe the look hung there, and each later look after a stop hung the same way | The look only reports that `sshpass` is missing. Once the look is published, the warning is written once, from a thread of its own, so a full stderr holds that thread only. The line stays on stderr, word for word: the differential comparison requires it there exactly once when the capture host lacks `/usr/bin/sshpass` (U123), so it is not moved to the WARN log the review suggested; `marathon-cli`'s vendor differential still passes | `a_full_stderr_holds_up_no_send_when_sshpass_is_missing_d91` (the child's stderr is a pipe filled and never read, `sshpass` missing; a send stopped while the look runs, then another): the later send succeeds | the child hung (120 s deadline) |
| (a) the admission lock was held while the configurable log sink was called | the quarantine is recorded in its own block; the lock is let go before the ERROR line | inspection; `a_stopped_send_whose_output_is_never_joined_ends_at_the_bound_and_its_destination_is_not_used_again_d91` still passes | n/a |
| (b) the blocking-call hook was compiled into production | `on_blocking_call` and the hook's slot exist only with the `test-support` feature; without it the slot is `()` and a call does nothing (`cargo clippy -p marathon-apple --lib` without the feature is clean) | n/a | n/a |
| (c) the hanging-start witness could not prove that a start killed before it recorded anything was reaped | the hook's end-of-start mark carries the started rsync's pid; the witness waits until that pid no longer answers signal 0, which a zombie of this process (its parent) still would | `a_hanging_start_of_rsync_holds_up_neither_its_own_stop_nor_another_transfer_d91` | n/a |

Run: `cargo fmt --check`; clippy for `marathon-apple` (all targets, and `--lib` without `test-support`), `marathon-ios`,
`marathon-macos`; `rsync_file_bridge.rs` 84/84, `marathon-apple` lib 62, `simulator_dispose.rs` 15; `marathon-cli`,
`marathon-test-support` and `marathon-macos` 171 passed. Stress (alone / with 12 busy processes): the full-stderr
witness, the three round-2 witnesses and the never-joined-output witness, 50/50 and 20/20 each.
