# X38: an open's reply until the caller takes it (U157)

A follow-up to x21's U157, from a post-merge review (finding S-x21-1, minor). No configuration field, CLI option, exit code, log line or
wire message changed.

## The finding

x21 runs a channel open in a task of its own and hands the reply to `start_session` through a oneshot. The task gave up the reply as soon as
`reply.send(opened)` succeeded. A caller whose future was dropped after the reply reached the oneshot, but before it was polled again, dropped
the channel with the oneshot: no CLOSE, and the slot stayed taken. The review's witness saw no `client_close`, and the next command under
`MaxSessions 1` failed with `OpenFailException: Resource shortage`. A refusal dropped the same way left its text on the queue, where the next
refusal reads it as its own.

x21's findings said that the task owns the reply whenever the caller has gone. That held only for a caller that went away before the reply
arrived. The claim is corrected there, and the U157 row in `docs/found-bugs.md` names this WP.

## The fix

The oneshot carries `OpenReply`, a guard holding the reply and a sender back to the task. The task keeps ownership until the caller claims it:

| What happens to the reply | Result |
|---|---|
| The caller claims it | The guard drops its sender, and the task ends without touching the reply |
| Dropped unclaimed: in the oneshot, in a caller's dropped future, or refused by a closed receiver | The guard's `Drop` sends it back. The task closes a channel and drains it, or takes a refusal's text, as it does for a reply that came after the bound |
| The transport dies first | The task ends. There is nothing left to close |

A refusal is claimed only once its text is off the queue. A caller dropped while it waits for the text, which russh delivers just after it wakes
the opener, leaves the task to take it.

The task drops the russh handle before it sends the reply, as before, so waiting for the claim does not keep the transport up. On a healthy open
the claim comes in the caller's next poll, and the task ends there.

## Tests

In `crates/marathon-apple/tests/ssh_session.rs`, on the fake worker's `late_open` fault with a 1 s delay and one session slot. Each polls
`start_session` once with a waker of its own, waits for the fake's `late_open_reply` and then for the wake that the reply's arrival in the
oneshot gives, and drops the future unpolled. They run at factor 1, so the open's 30 s bound is far behind the reply and cannot be the wake.

| Test | Asserts | Without the fix |
|---|---|---|
| `a_caller_that_goes_away_after_its_reply_came_leaves_no_channel_behind_u157` | A `client_close`, no `exec`, and the next command runs in the one slot | Fails: no `client_close` (0 of 1) |
| `a_caller_that_goes_away_after_its_refusal_came_leaves_no_text_behind_u157` | With the slot held, the next refusal reads `Resource shortage` | Fails: it reads the late refusal's `Administratively prohibited` |

"Without the fix" is `client.rs` as of the base commit with the new tests.

| What | Result |
|---|---|
| `cargo test -p marathon-apple -p marathon-ios` | 798 passed, 0 failed. The real-sshd suite is ignored by default |
| `cargo clippy -p marathon-apple -p marathon-ios --all-targets -- -D warnings`, `cargo fmt --check` | Clean |
| `u157_…` and `u154_…` in `ssh_real_sshd.rs`, on a throwaway macOS sshd on a loopback port with `MaxSessions 1` | Both pass: U157 fails at 30,002 ms with 1 of 1 slots free afterwards, U154 at 35,004 ms |

## Who could hit it

Only a caller that drops a pending `start_session`. In production that is nobody: the one caller, `execute` in `executor.rs`, runs in a task
that `SshCommandExecutor::execute` spawns on the vendor runtime and that runs to its end even when the caller's future is dropped. It could have
been hit by:

- Code that calls `SshConnection::start_session` directly, which is public. Today that is tests only.
- The executor's task stopped from outside, as when its runtime shuts down. The connection's own tasks go with it, so a slot left that way does
  not outlive the connection anyway.

## Limits

- The refusal whose caller is dropped while it waits for the text has no test of its own: the window is between russh's wake and its
  `channel_open_failure` call, which the fake cannot hold open. The code path is the one the refusal test covers.
- An `OpenReply` that is never dropped, as with `mem::forget`, keeps its reply from the task. The task still ends when the transport dies.
- Not run on Linux. The change has no platform-specific code.

## Incidental bugs

None found.
