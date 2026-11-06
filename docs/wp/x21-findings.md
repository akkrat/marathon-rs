# X21: the leftovers of x13 on Apple workers (U154, U155)

The project owner approved these on 2026-11-07. Both come from the incidental bugs in `docs/wp/x13-findings.md`. No deviation number was
assigned: U154 extends D29 and U155 extends D31, and those sections and PLAN rows say so. No configuration field, CLI option or exit code
changed. One log line is new. The commands a healthy run sends are the same.

## U154: an `exec` with no reply

**Verified.** `ExecChannel::exec` returned `ConnectionException` caused by `TimeoutException` when 30 s passed with no reply, and left
the channel open. With one slot, the next command could not run (the fake, with the close reverted).

**Fix.** It closes the channel as D29 does a rejected one: CLOSE, then at most 5 s for the peer's reply, whose outcome is unused. So the
wait is 30 s if the peer answers CLOSE at once, and 35 s at most. The error stays Kotlin's, which `safe_execute` already turns into null. The error
is clear from one new ERROR line on the `SshjCommandExecutor` logger, in the manner of D30's `Abandoning session` line:

```
No reply to exec `<command>` in 30 s: closing its channel. If the worker gets to the request later, the command may still start, and hold its session slot until it ends
```

No `woke to` line is logged, as before: nothing woke the wait. Other sessions on the connection are not held up, because the wait is in
the caller's own task and the pump keeps draining a channel that has been dropped
(`other_sessions_keep_working_while_an_exec_waits_for_a_reply_u154`).

**The first version sent TERM ahead of CLOSE, and the real sshd showed it does not work.** A real sshd gives no reply while its session
process is stalled, and on resuming takes the queued `exec` and whatever follows it in one go. So a command can start after its caller has
given up. TERM seemed the way to reach it, since a peer handles a channel's requests in order. It loses the race:
`session_signal_req: killpg(<pid>, 15): No such process` in sshd's log, because the new process has not reached `setsid`. A signal that
gets through 2 times in 30 is not worth a new command on the wire, so it is gone.

| On a real sshd, session process stopped after the channel opened, resumed after the failure | Result |
|---|---|
| `sleep`, TERM then CLOSE, 30 runs (macOS sshd, load about 4) | 2 ended, 28 still running 1.5 s later |
| `sleep`, CLOSE only, 6 runs (macOS sshd) | 6 still running 1.5 s later |
| `sh -c 'cat; echo …'`, CLOSE (macOS sshd, also with `MaxSessions 1`; Linux OpenSSH 10.2p1) | `cat` sees the end of its input and ends, sshd gives the slot back, the next command runs. 35,004–35,006 ms on macOS, 35,001 ms on Linux |
| The same with the close reverted (macOS sshd, `MaxSessions 1`) | `cat` still waits on its input 10 s after the resume |

**What is left.** A command that does not read its input, on a peer that stalls for the 30 s and then resumes, starts and runs to its end,
in its slot. That is why the log line says so, and the connection is unaffected. A real fix would keep the channel past the caller's error, wait
for the late reply and only then send TERM and CLOSE, so a task would outlive the caller's error. That is more than this brief asks, so it is
for the owner to decide.

**How narrow the window is.** The stall has to begin between the peer's answer to the channel open and its answer to `exec`. A stall that
begins earlier fails at the open, which has its own 30 s: see Incidental bugs. The first real test failed that way.

**Failure without it:** with the close removed, `an_exec_with_no_reply_fails_with_its_timeout_and_gives_its_slot_back_u154`,
`an_exec_with_no_reply_from_a_peer_that_never_answers_close_still_returns_u154` and, on the real sshd,
`u154_an_exec_with_no_reply_is_closed_when_the_peer_resumes`. The last fails first on its time window, and with only that widened, on the
`cat` that is still waiting. `other_sessions_keep_working_…` passes either way: it guards the connection, not the close.

**The fake worker** has a new fault, `silent_exec`, documented in `docs/design/fake-worker.md`: an `exec` containing that text gets no reply.

**Comparators.** No allowance is needed. `vendor_transport.ssh` does not compare `client_close`, and no whole-vendor case has a silent `exec`.

## U155: a receive past its deadline

**Verified.** At its 30 s a receive wrote its diagnostic, sent TERM to the child and waited for it without a limit. Under `sshpass` the
child exits on TERM, and rsync and ssh stay (the real-tool test below). With key authentication, a process that ignored TERM held the
receive for ever, which x13's note did not mention.

**Fix.** The receive now stops the process with what x13 built for sends. `stop_stalled_send` is split: its first step, 5 s for rsync to end
by itself after its report, stays with sends, and the rest is `stop_process`, which the receive calls at its deadline. The steps, all of them
existing code:

| Step | Bound | Under `sshpass` only |
|---|---|---|
| List children | 5 s, real time | Yes: a key-authenticated receive never runs `pgrep` (`only_a_receive_under_sshpass_lists_children_u155`) |
| TERM | | To rsync as well as `sshpass` |
| Wait for the child | 5 s, vendor clock | And for the listed pids |
| KILL | | To the listed pids still alive, and to the child |
| Reap | | |

A receive that ends after the stop still returns `false`, and its deadline, diagnostic and log lines are as they were. TERM is as prompt as it was
on key authentication; under `sshpass` it follows the listing. So a receive takes at most about 40 s now, instead of having no limit. Its reader
tasks were already cancelled once the deadline passed, and that is unchanged.

**Combining with x16.** x16 adds a `ps -o lstart=` check to the listing and the KILL in the same function, and edits its doc comment. So this
change leaves that comment and the signature where they are, and the receive gets the check with no more work. See the table below.

**A rough edge in the tests.** An rsync killed under `sshpass` has been orphaned, so `kill(pid, 0)` can still succeed for a moment until init
reaps it. `stalled_receive_runs` polls for that, where `stalled_send_runs` asserts it at once on a case that does not reach KILL.

**Failure without it**, with `stop_process` replaced by Kotlin's TERM and wait:

| Test | How it fails |
|---|---|
| `a_receive_past_its_deadline_under_sshpass_also_stops_rsync_u155` | rsync survives |
| `only_a_receive_under_sshpass_lists_children_u155` | Its `sshpass` round: rsync survives |
| `the_real_rsync_under_the_real_sshpass_is_stopped_at_a_receives_deadline_u155` | Real rsync and ssh survive |
| `a_receive_past_its_deadline_that_ignores_term_is_killed_u155`, `rsync_that_ignores_term_at_a_receives_deadline_under_sshpass_is_killed_u155` | The process is not killed |
| `the_real_rsync_is_stopped_at_a_receives_deadline_u155` | Passes: TERM has always ended openrsync and its ssh. It pins that this path stays right |

**Comparators.** No allowance is needed. `deadlines.jsonl` stays the witness of the deadline, with the same test, and no host argv changed: a
receive still has no `--timeout`.

## What was verified, and how

| What | How | Result |
|---|---|---|
| `marathon-apple` and `fake-worker` on this Mac | `cargo test -p marathon-apple -p fake-worker`, under a load of 20–80 from other work | 502 passed, 0 failed, 18 ignored (the real-sshd suite, run separately) |
| `ssh_real_sshd.rs` in full on this Mac's sshd | A throwaway `sshd -D -e -f <temp config>` on a loopback port, `--ignored --test-threads 1` | 18 passed |
| On Linux with OpenSSH 10.2p1 and rsync 3.4.1, from a temp checkout with its own target directory, all removed afterwards | `rsync_file_bridge`, `ssh_session`, and the U154 test on a throwaway sshd there | 26, 30 and 1 passed. That box has no `sshpass`, so the two tests with the real one print their skip message |
| The real tools, at factor 1 | sshpass 1.06, openrsync (protocol 29), OpenSSH_10.5p1 on this Mac, against a silent listener, as the send's test does | Passed, nothing left behind |
| Validators | `make_registry.py --check`, `test_evidence.py` | Clean, 66 tests |
| A trial merge with x16 | `wp/x16-ssh-connection` merged into the fixes in a temporary worktree, removed afterwards | No conflict. The six `*_u155` and three `*_u154` tests pass on it, at factor 1 for the real tools |

## What was not verified

- A real worker's own stall. The real sshd here is stopped with SIGSTOP, which stands for a stalled peer, not for any stall seen in use.
- `sshpass` with rsync 3: that box has no `sshpass`.
- Workers other than OpenSSH for U154. A server that handles requests concurrently could behave differently after CLOSE.

## Incidental bugs

- `SshConnection::start_session`: a channel open with no reply in 30 s drops russh's future and leaves the channel in russh's table. By
  reading: on a peer that resumes, the channel opens with nobody to close it, and holds a slot until the connection ends. This was not
  measured apart. It is U154's neighbour, and a stall is more likely to land here, since `exec` follows the open's reply at once.
- `RsyncFileBridge::process`: a receive whose process exits by itself before its deadline joins its pipe readers with no limit, so a child
  that outlived it and holds a pipe, such as a hung ssh, can still hold up the receive. D31 bounded this for sends only. By reading, not run.
