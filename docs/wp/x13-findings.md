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

## Incidental bugs

- `ExecChannel::exec`: an `exec` request that gets no reply within its 30 s also leaves the channel open on the peer. The
  brief covers rejection only, so this stays as it is, in Kotlin too.
