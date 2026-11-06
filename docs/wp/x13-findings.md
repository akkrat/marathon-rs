# X13: SSH and rsync bugs fixed (D29–D32)

The project owner asked on 2026-11-06 for these fixes to bugs the port had kept for parity. Each is an approved
deviation in PLAN §3.11. No configuration field or exit code changed; the only CLI-visible change is the log lines stated
below.

## D29 / A33: the rejected channel

**Ledger row verified.** `ExecChannel::exec` turned a `CHANNEL_FAILURE` into `Request failed` and returned, with the
channel still open on the peer. The transcript of `exec_rejection_is_request_failed_and_leaves_the_channel_open` had no
`client_close`, and the next command on `MaxSessions 1` got `OpenFailException`.

**Fix.** On rejection the channel sends CLOSE and waits for the peer's reply for up to 5 s, then returns the original
error. It is 5 s because no process runs behind a rejected request, so the reply takes one round trip, and because
waiting at all is what makes a slot free for the caller's next command. Without the wait, that command's channel open
could reach the peer first. The wait's result is unused, so neither a dead transport nor expiry can replace
`Request failed`, and no second `woke to` line is logged.

**The pump's comment.** The drain logic kept reading after the channel was dropped because a rejected channel stayed
open. It is still needed: a peer may never answer the CLOSE, and russh's queue must not hold up the connection. It now
also ends on the peer's late CLOSE, so that a task does not outlive the channel until the end of the connection.

**Not changed.** An `exec` that gets no reply in 30 s still leaves its channel open: see Incidental bugs.

**Comparators.** No allowance is needed. `vendor_transport.ssh` compares `exec`, output and exit rows, not `client_close`,
and no whole-vendor case rejects an `exec`. The Kotlin evidence under `fixtures/fake-worker/evidence/*/reject` is untouched
and stays the witness.

**Failure without it:** with the close removed, `exec_rejection_is_request_failed_and_closes_the_channel_d29` fails because
the next command is `OpenFailException`.

## D30 / A14: closing a session

**Ledger row verified.** `session.rs` did what the row says: TERM if open, `close()` with its 30 s, then `join()` with no
limit, and a later close after the timeout branch's failure reached that `join()`. No KILL.

**The order had to change: a finding the brief did not foresee.** The first version kept Kotlin's TERM and CLOSE together
and sent KILL at the end of the wait. Against the real sshd the close failed after 30 s, with the process still running and
only `session_signal_req: signal TERM` in sshd's log. russh removes a channel from its table when it sends CLOSE
(`session.rs`, `Encrypted::close`), and `client/session.rs::signal` then silently drops the request. Patching the vendored
russh to send it was rejected: a request on a channel the client has closed is not something a server has to accept, and a
server that treats it as a protocol error would take the shared connection with it. So the sequence is:

| Step | Wire | Waits for | Up to |
|---|---|---|---|
| 1 | TERM (if open) | The peer's exit report or CLOSE | 10 s |
| 2 | KILL, only if step 1 expired | Nothing | |
| 3 | CLOSE | The peer's CLOSE | 20 s |
| 4 | Nothing | Abandoned: one ERROR line, the close fails | |

**Durations.** They are not configurable, and they add up to sshj's 30 s, so no close takes longer than Kotlin's first wait
and `Timeout expired: 30000 MILLISECONDS` stays true. 10 s before KILL is short because of what it costs: on a server that
ignores signals, CLOSE may be all that stops the process, and it now goes out 10 s later than in Kotlin. The slowest process
measured to end on TERM took 8 s (T15t), so T07 and T15t stay as they were. 20 s for the reply is the rest, and only a peer
that has really stalled uses it.

**An ordinary fast exit is not slower.** The exit report ends step 1, so CLOSE follows within a round trip. The real sshd's
flood close is 2 ms, T07's 4018 ms against Kotlin's 3996 ms, and T15t's 8025 ms against 8078 ms. A process that ends on TERM
sees no KILL (`a_fast_exit_on_term_closes_at_once_and_sends_no_kill_d30`).

**Who pays for the later CLOSE.** Only `Xcodebuild::test_without_building` hands out a live session
(`bin/xcrun/xcodebuild.rs`); other commands are awaited and never closed. So a session is closed with its process still
alive only when a batch is cancelled or stopped, plus the total-timeout branch of any command. The delay does not touch normal
batches, and the spike already concluded that marathon cannot drive a worker over Tailscale SSH
(`docs/research/ssh-spike-results.md`), which also reports every exit as 0.

**Every path is bounded.**

| Path | Before | Now |
|---|---|---|
| First session close, process alive | 30 s, then failure | 30 s at most, then failure |
| Timeout branch (`exit_code`) | TERM, 30 s, then failure | Same escalation, same failure, so `safe_execute` still returns null |
| Close after the timeout branch failed (T15h) | Unbounded `join()` | Fails at once, with no new signal |
| Close during the timeout branch's wait | Unbounded `join()` | Waits for it, at most 30 s |
| Close after a failed close (P5) | Skips the backend, joins the scope | The same; the scope's tasks all end on its cancellation (below) |
| `execute`'s clock-hook failure | TERM, `close()`, 30 s | Same escalation |

`ExecChannel::join` is gone. `ExecChannel::close`, with its 30 s, remains for `suspend_for`, which only reaches it on a closed
channel. Each task in the session scope ends on cancellation: the readers select on it, the collectors select on it, and
`exit_code` waits only for this same bounded close.

**A departure from Kotlin that the bound requires.** `CommandSession::close` runs `base.close()` only if the backend
succeeds, and P2 leaves the exit code to the scope once a close has begun. So an awaiter of an abandoned session would wait for a
later close that may never come. A failed backend close now cancels the scope itself
(`an_awaiter_of_an_abandoned_session_is_released_without_a_second_close_d30`). The second close still skips the backend, and
still cancels and joins, as P5 says.

**Exit codes checked.** `autodetect_exit_code` and its 143 are untouched, and `terminate()` does not go through this path.
The timeout branch still returns null whatever the signal was. `safe_execute` still gets the same `ConnectionException` chain.
`is_open` now stays true during step 1, so the readers keep consuming output while the process ends, and `closing` (P2) still
keeps the exit code away from a concurrent awaiter: `await_concurrent_with_close_is_cancelled` is unchanged.

**The retry comment in `base_command.rs`** was right: the backend acts on the first call only. The retry that follows a
failed close now joins only bounded work, and the comment says so.

**Shared connection and slots after abandoning.** The channel stays open on the peer, in its `MaxSessions` slot, until the
process ends or the connection does. Later commands that find no slot fail with `OpenFailException`, which `safe_execute` turns into
null: they do not hang. The connection is not closed, because other sessions may be using it
(`abandoning_a_session_leaves_the_connection_and_its_other_sessions_alone_d30`). The pump keeps draining the abandoned channel,
and with no reader the window stops refilling after at most 2 MiB, so that output cannot grow without limit.

**KILL on the real sshd.** `session_signal_req: signal KILL, killpg(…, 9)` reaches the whole process group, so a child that
inherits the ignored TERM dies too (`t30_kill_ends_a_process_that_ignores_term`: close in 10,004 ms, no process left).

**Failure without it**, by temporary revert of one thing at a time, on `ssh_session.rs`:

| Reverted | Fails |
|---|---|
| KILL | `kill_ends_a_process_that_ignores_term_and_the_close_succeeds_d30`, `first_close_fails_…_d30`, `timeout_close_failure_…` |
| Exit report ending the TERM wait | `the_exit_report_ends_the_term_wait_when_the_peer_keeps_the_channel_open_d30` |
| Scope cancellation on failure | `an_awaiter_of_an_abandoned_session_is_released_without_a_second_close_d30` |
| The bound on a later close | `timeout_close_failure_…` |

**Comparators.** No allowance is needed. `vendor_transport.ssh` compares exec, output and exit rows, not signals or
`client_close`, and on a whole-vendor run every command has finished when it is closed.

## Incidental bugs

- `ExecChannel::exec`: an `exec` request that gets no reply within its 30 s also leaves the channel open on the peer. The
  brief covers rejection only, so this stays as it is, in Kotlin too.
