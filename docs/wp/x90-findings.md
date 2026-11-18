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

**One deadline from a stop.** In `process`, the stop branch sets `Bound::at = now + 40 s` on the bridge's vendor clock
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

Stress: see the table at the end (counts per test, each run alone with `--exact`).

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
