# X38: an open's reply until the caller takes it (U157)

A follow-up to x21's U157, from a post-merge review (finding S-x21-1, minor), with a second round below (S-x38-1). No configuration field,
CLI option, exit code, log line or wire message changed. Round 2 adds a fourth patch to the vendored russh.

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
| Dropped unclaimed: in the oneshot, in a caller's dropped future, or refused by a closed receiver | The guard's `Drop` sends it back. The task closes a channel and drains it, as it does for a reply that came after the bound. Since round 2 a refusal carries its own text, so there is nothing to clean up for one |
| The transport dies first | The task ends. There is nothing left to close |

Round 1 also took the text of an unclaimed refusal off the connection's queue, and claimed a refusal only once its text was off it. Round 2
removed the queue, see below.

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
| `a_refusal_dropped_unclaimed_does_not_give_its_text_to_one_read_after_it_u157` (round 2) | See round 2 | See round 2 |

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

- An `OpenReply` that is never dropped, as with `mem::forget`, keeps its reply from the task. The task still ends when the transport dies.
- Not run on Linux. The change has no platform-specific code.

## Review round 2: refusal texts on a shared queue (S-x38-1)

**The finding.** The review of round 1 showed that a refusal's text came from one queue for the whole connection, with nothing tying a text
to its open. Its witness: an `Administratively prohibited` refusal waits in one caller, the slot is filled, a `Resource shortage` refusal
waits in a second, then the first caller is dropped and the second is read before the cleanup runs. The second caller got
`Administratively prohibited`, and the cleanup then took its `Resource shortage`. It failed on round 1's head and on the base alike, so it was
there before this WP (see Incidental bugs).

**Why a fix on our side alone could not work.** russh 0.64.1 tells an opener only the reason code. The text goes to
`Handler::channel_open_failure` with russh's channel id, which the opener never learns, and russh wakes the opener before it calls the
handler. Order cannot stand in for the id either: two refusals can both be waiting before either caller reads, as in the witness.

**The fix.** Patch 4 of `vendor/russh` (`crates/marathon-apple/russh-open-failure-description.patch`, described in
`vendor/russh/README.marathon.md`) sends the text to the opener's own channel, just before the refusal. The new
`Handle::channel_open_session_described` returns it in `Error::ChannelOpenRefused`. `channel_open_session` skips it and still returns
`Error::ChannelOpenFailure(reason)`, which the fake worker's contract tests rely on. Nothing changes on the wire.

`start_session` uses the new method, and `ConnectionShared` loses the queue, its `Notify`, `take_open_failure` and the handler callback
that filled it. A refusal is now plain data in the reply. Dropping one, claimed or not, drops its text and touches nobody else's, and a
refusal no longer waits for its text after it wakes. The caller's error is the same as before: `OpenFailException` with the peer's
description, empty when the transport has died.

**The test.** `a_refusal_dropped_unclaimed_does_not_give_its_text_to_one_read_after_it_u157` is the reviewer's case. It runs on one thread,
so nothing else can run between dropping the first caller and reading the second, and the result does not depend on scheduling. The first
open gets the fake's late refusal after 1 s, `sleep 1000` takes the one slot, and the second open is refused at once. Both are held, polled
once, until their replies arrive.

| `client.rs` and `vendor/russh` at | Result |
|---|---|
| This round | ok, the second caller reads `Resource shortage` |
| Round 1's head, `ede2f067` | FAILED: `Administratively prohibited` |
| The base, `c03c08a5` | FAILED: `Administratively prohibited` |

A vendor test, `test_concurrent_refusals_each_carry_their_own_description`, refuses two concurrent described opens with different reasons
and checks that each text matches its own reason. It also checks that a plain open still returns `ChannelOpenFailure(reason)`.

| What | Result |
|---|---|
| `cargo test --manifest-path vendor/russh/Cargo.toml --lib client::` | 30 passed, 0 failed (29 before) |
| `cargo test -p marathon-apple -p marathon-ios -p fake-worker` | 830 passed, 0 failed, 26 ignored (the opt-in real-tool suites) |
| `cargo clippy -p marathon-apple -p marathon-ios -p fake-worker --all-targets -- -D warnings`, `cargo fmt --check` | Clean |

The real-sshd suite was not rerun in round 2. A real refusal goes through the same russh path as the fake's.

## Incidental bugs

One, fixed here in round 2: refusal texts came from a queue shared by the whole connection, so concurrent or abandoned refusals could take
each other's text. It came with the port, not from Kotlin: in sshj 0.39.0's bytecode, `ConnectionImpl` passes the packet to the channel it names, and that
channel's `AbstractDirectChannel.gotOpenFailure` reads the text into its own `OpenFailException`. Before x21 it took
two opens refused at about the same time. x21's task made it more likely, because a refusal nobody took was taken later, out of order.
