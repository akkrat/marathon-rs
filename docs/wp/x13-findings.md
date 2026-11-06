# X13: SSH and rsync bugs fixed (D29–D32)

The project owner asked on 2026-11-06 for these fixes to bugs the port had kept for parity. Each is an approved
deviation in PLAN §3.11. No configuration field, CLI option or exit code changed. The log lines that are new are stated
below, as is the one command line that workers now receive differently.

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

## D31 / A3: the rsync send limit

**Ledger row verified.** `process` had a 30 s total deadline for a receive, and `child.wait()` alone for a send.
`fixtures/oracle/connections/deadlines.jsonl` is the JVM's witness: the receive fails at its deadline while a send stays
running. That test stays, under a new name, because a receive's deadline still applies and still leaves a send alone.

**Why rsync's own timeout and not a total one.** A total deadline cannot tell a large bundle over a slow link from a dead
worker. The `--timeout` flag can:

| Measured with a short `--timeout` | openrsync (macOS 27, "rsync version 2.6.9 compatible") | rsync 3.4.1 (Linux) |
|---|---|---|
| Flag accepted | Yes; also on a second Mac with macOS 27.2 | Yes |
| Passed on to the worker's `rsync --server` | No, so the worker's exec string is unchanged | Yes, among the options `{protocol-options}` covers |
| Worker's `--server` accepts it | Yes, read without an option error | Yes |
| Hung ssh, before the greeting | `error: poll: timeout` after 3 s of `--timeout=3` | `[sender] io timeout after 2 seconds -- exiting`, exit 30, after 3 s of `--timeout=2` |
| Stall in the greeting and in the middle of a file | The same report | Not measured apart |
| What it does next, if ssh sees the end of its input and exits | Exits with ssh's status | Exits by itself |
| What it does next, if ssh does not | **Waits in `wait4` for ever** | Exits by itself, and ssh is gone |
| TERM at that point | rsync exits with status 20, and ssh is gone too | |
| 15 or 30 MiB at about 1 MiB/s, moving but slow, with `--timeout=2` | Exit 0 after 83.6 s, identical bytes | Exit 0 after 16 s, identical bytes |

The first row is why the flag is enough on every rsync seen. rsync's older samba versions have had the flag since long before
the 2.6.9 that macOS once shipped; that version itself was not run.

**The trap beyond the brief: the flag alone does not bound a send on macOS.** Only the wrapper can see the state in the
table's bold cell, and it has no other signal than rsync's text, since rsync does not exit. So the stderr reader watches for
the two reports. Text is fragile, so this is arranged so that a miss costs little: rsync 3 exits regardless, and an rsync that
words its report differently keeps Kotlin's unbounded wait for a hung ssh only, not for a dead link that ends ssh. A false match
needs a stderr line with `poll: timeout` in it, on a send that is therefore already failing.

**Stopping.** After a report: 5 s for rsync to exit by itself (a healthy ssh takes a moment over its input's end), TERM, 5 s,
KILL, then the reap. These are short because they only have to outlast a process on its way out. A process group would have been
the simple way to reach everything, but it would take rsync away from a terminal's Ctrl-C on every send. So TERM goes to the
child and the children `pgrep -P` finds. That matters under `sshpass`, measured with the real one: TERM ends it at once, while
rsync, in the session of `sshpass`'s pty, stays in `wait4` with its hung ssh, both alive 2 s later. Those children get KILL
after the second wait, whatever became of `sshpass`. No `pgrep` only means the signals reach the child alone, and the send
still returns.

**300 s.** It is long because a false positive is worse than a late one: a healthy send that fails costs a device, while a dead
one has already cost the wait. Silent stretches that a healthy send can have are the worker's `--delete` scan and its
`--delay-updates` renames, both of which take longer the more it is under load, as it is when booting simulators. With the
installer's three attempts a dead worker fails in up to 15 minutes, and not never.

**Serialisation.** A stuck send held the per-bridge mutex for ever. It now holds it until it is reaped, and the next send then
runs (`stalled_send_runs` asserts the order). Receives never took that mutex. **Retries:** the send returns only after the reap,
so `with_retry_send` cannot overlap it. The worker's `rsync --server` is another matter: it ends on the end of its input if ssh
got that far. With rsync 3 on the host it also has the flag, and otherwise a process stuck on a dead link can linger, but a
retry cannot reach that worker either.

**Reader bound.** `process` joined its pipe readers without a limit, and a surviving ssh inherits stderr. A send now waits at most
5 s for them after the process ended. On a healthy exit the pipes close at once, so this adds nothing.

**Transcript and comparators.** `--timeout=300` shows in the host argv only on macOS hosts. `vendor_transport.host_argv` gives a
send of a JVM or pinned native record exactly this flag after `--times`, so a new native run must have it there. See
`test_d31_*`: a missing flag, another value, another place or a flag on a receive all differ, and an older record that already
has it is refused. The exec string needs nothing, because `{protocol-options}` covers rsync's server options.

**Failure without it**, by temporary revert:

| Reverted | Fails |
|---|---|
| The flag | Every `*_d31` test (the fake rsync refuses to run) and the argv tests |
| Stopping on the report | `a_stalled_send_that_waits_for_a_hung_ssh_is_stopped_on_term_d31`, `a_stalled_send_that_also_ignores_term_is_killed_d31`, `the_real_rsync_gives_up_on_a_hung_ssh_d31` (each at its 60 s catch) |
| KILL after TERM | `a_stalled_send_that_also_ignores_term_is_killed_d31` |
| TERM to the child's children | `a_stalled_send_under_sshpass_also_stops_rsync_d31`, `the_real_rsync_under_the_real_sshpass_gives_up_on_a_hung_ssh_d31` |
| The bound on the readers | `a_stalled_send_that_exits_while_ssh_holds_its_stderr_returns_d31` |

## D32 / the `NSUnbufferedIO` part of A2

**Ledger row verified.** `execute` built the exec string from `request.command` alone. `bin/xcrun/xcodebuild.rs:113` is the only
place that sets `NSUnbufferedIO`. Others set env too, such as `simctl boot`'s `SIMCTL_CHILD_*`; those stay dropped, and so does the
workdir.

**The exec string, from the real path** (`xcodebuild_runs_on_the_worker_with_the_unbuffered_prefix_d32`):

```
env NSUnbufferedIO=YES xcrun xcodebuild test-without-building -xctestrun /tmp/marathon/U/marathon.xctestrun -enableCodeCoverage NO -destination-timeout 30 -destination 'platform=iOS Simulator,id=U'
```

It is one simple command, not `a && b` or a `sh -c` wrapper, so the variable reaches `xcrun`. `xcrun` passes its environment on to
the tool it starts (measured: `env NSUnbufferedIO=YES xcrun env` prints it). A user's argument cannot make it compound, because
`shell_command` quotes the special characters.

**`env` against a prefix**, each shell's `-c`, as sshd starts a login shell:

| Login shell | `env NSUnbufferedIO=YES printenv NSUnbufferedIO` | `NSUnbufferedIO=YES printenv NSUnbufferedIO` |
|---|---|---|
| sh, bash, zsh, dash | `YES` | `YES` |
| csh, tcsh | `YES` | `NSUnbufferedIO=YES: Command not found.` |
| fish | Not installed here; `env` is an ordinary program there too | Not run |

So `env`. The cost is that `env` must be on the worker's `PATH`, which it is by default on macOS.

**Real sshd** (the throwaway one that `ssh_real_sshd.rs` uses, on this Mac, zsh login shell):
`t32_ns_unbuffered_io_reaches_the_process_and_nothing_else_does` gives `NS=YES F=unset`, and the home directory rather than the
workdir. With the prefix reverted it gives `NS=unset F=unset`.

**Local executions** are unchanged: the prefix is in the SSH executor. The request keeps the plain command and the env map
(`the_request_keeps_the_variable_in_its_env_and_out_of_its_command_d32`), and `local_executor.rs` already shows env reaching a local
process, so the variable arrives once.

**What unbuffered output changes downstream.** I read the path for it, and nothing needs changing:

| Where | Effect |
|---|---|
| `produce_lines_manually` | Each write now wakes it; it already copes with any chunk size, and `last_output` moves on each read |
| `LineBuffer` | Holds partial lines and partial UTF-8 until complete (D10); under buffering a chunk boundary fell in mid-line too, more often |
| The idle timeout | Fires later, as it should, on real silence. With buffering, a test that printed little could look silent until a block filled (`small_writes_of_unbuffered_output_hold_off_the_idle_limit_and_keep_lines_whole_d32`) |
| D19 | Reads what arrived with EOF, whatever the chunk size |
| The xcodebuild parsers | Get whole lines from the channel, as before |

Real xcodebuild output timing was not measured: see below.

**Fixtures and comparators.** The prefix is new on the wire, and the recordings stay as they are.

| Place | What it does | Tests |
|---|---|---|
| `vendor_transport.ssh` | Gives a JVM or pinned native record's `xcrun xcodebuild test-without-building ` exactly the prefix, so a new native must have it | `test_d32_*`: no prefix, `VAR=x` form, another value, prefix on another command all differ; an older record that has it is refused |
| `tools/fake-worker/src/apple.rs` | Routes the prefixed command as the command it precedes, keeping the raw bytes in the transcript | `the_unbuffered_prefix_routes_only_on_the_xcodebuild_test_command_d32` |
| `apple_evidence.py::command_counts` | Counts it as its catalogued form; `command-catalogue.json` is unchanged | `test_d32_a_prefixed_batch_counts_as_the_catalogued_form` |
| `test_evidence.py::as_live_native` | A test that changes a pinned native record makes it "a run of today's code", which must hold what today's code sends | The `test_d20_*` tests that use it |

The Apple whole-vendor cases, run natively through the real SSH executor and the fake worker against their JVM goldens (`ios-smoke`,
`macos-smoke`, `ios-media`, `ios-screenshot`, `ios-pass`, `macos-pass`, `ios-ignore`, `ios-parse`, `macos-ignore`, `macos-parse`):
all ten equal. With the prefix reverted, `ios-smoke` differs at `/control/14/wire`, which shows that the allowance does not hide a
missing prefix. The steps are those of `tools/differential/run native-vendors`, in a throwaway driver for these ten cases only, with
no JDK; the five Android cases were left out as no Android code changed.

**Failure without it:** `only_ns_unbuffered_io_reaches_the_worker_in_the_exec_string_d32`,
`xcodebuild_runs_on_the_worker_with_the_unbuffered_prefix_d32` and `t32_…` fail.

## What was verified, and how

Everything below ran on this Mac unless it says otherwise. The real sshd was throwaway: `sshd -D -e -f <temp config>` as the normal
user, on a random high loopback port, with its own host key and one temporary client key, removed afterwards; no system file was
touched. The Linux rsync measurements ran over SSH on a Linux box, in a temp directory that was removed.

| What | How | Result |
|---|---|---|
| `ssh_real_sshd.rs` in full, 17 tests | `MARATHON_SSH_REAL=… cargo test -p marathon-apple --test ssh_real_sshd -- --ignored --test-threads 1` | See the final report for the last run |
| Durations and exit codes against Kotlin's real-host rows | T07, T14, T15t, T16 in that suite | Match, within the milliseconds of earlier rows |
| rsync on macOS and Linux | Fake ssh shims, from a hang before the greeting to a slow link | The table under D31 |

## What was not verified

- **A real worker's real xcodebuild** under any of the four. In particular whether xcodebuild's output is in fact less bursty with
  the variable, and how long a real xcodebuild takes on TERM, against the 10 s before KILL. The 10 s comes from a measured `sleep`, not
  from xcodebuild.
- **SSH servers that ignore signals.** The escalation's behaviour is shown only on a fake peer that ignores TERM and KILL. Tailscale
  SSH was not run: per the spike, marathon cannot drive a worker through it anyway. Old OpenSSH without signal support was not run.
- **What D30's `KILL` does on OpenSSH on Linux.** `killpg` was seen in the log of the macOS build's sshd only. The Kotlin evidence
  rows for Linux are unaffected, but the suite was not re-run against Linux OpenSSH.
- **rsync 2.6.9 from samba**, which old macOS hosts ship: the flag and the report's wording come from its source history, not a run.
- **fish** as a login shell.

## Incidental bugs

- `ExecChannel::exec`: an `exec` request that gets no reply within its 30 s also leaves the channel open on the peer. The
  brief covers rejection only, so this stays as it is, in Kotlin too.
- `RsyncFileBridge::process`, receive: after its 30 s the wrapper sends TERM and then waits for the process without a limit. With
  openrsync that TERM ends it, and ssh; a receive under `sshpass` has the problem D31 solved for sends, because TERM ends only
  `sshpass`. Kotlin's wrapper does the same, and the brief covers sends only. The receive's reader tasks are cancelled, so it
  returns once `sshpass` has gone, and the rsync and ssh it leaves behind wait until ssh ends.
- `tests/ssh_real_sshd.rs::test_stderr` fails on any host whose login shell's startup prints to stderr, as the 4.2 findings say. It
  passes on this host.
