# X21: the leftovers of x13 on Apple workers (U154, U155, U157, U158)

The project owner approved these on 2026-11-07. U154 and U155 come from the incidental bugs in `docs/wp/x13-findings.md`, and U157 and U158 from
this WP's own. No deviation number was assigned: U154 and U157 extend D29, U155 and U158 extend D31, and those sections and PLAN rows say so. No
configuration field, CLI option or exit code changed. Two log lines are new. The commands a healthy run sends are the same.

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
for the late reply and only then send TERM and CLOSE. The owner decided on 2026-11-07 to leave this as documented.

**How narrow the window is.** The stall has to begin between the peer's answer to the channel open and its answer to `exec`. A stall that
begins earlier fails at the open, which has its own 30 s: see U157. The first real test failed that way.

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
on key authentication; under `sshpass` it follows the listing. So a receive past its deadline, which had no limit, now takes at most:

| | Deadline | Listing, real time | TERM's step | x16's `ps` before KILL, real time | Total |
|---|---|---|---|---|---|
| Key authentication | 30 s | None | 5 s | None | About 35 s |
| `sshpass`, on this branch | 30 s | Up to 5 s | 5 s | None | About 40 s |
| `sshpass`, with x16's check | 30 s | Up to 5 s | 5 s | Up to 5 s for each survivor | About 45 s, 50 s with two survivors |

Its reader tasks were already cancelled once the deadline passed, and that is unchanged.

**What this costs on a password-authenticated worker.** In Kotlin the deadline did not stop a pull there. TERM reached `sshpass` alone, and rsync went on and finished. The pull returned `false`,
but its file arrived. Now it is cut, with `.rsync-partial/<name>` left, as key authentication has always done. What a user sees depends on the caller, none of which retries:

| Caller | Uses the result | Over 30 s under `sshpass`, Kotlin | Now |
|---|---|---|---|
| `crates/marathon-ios/src/listener/video.rs:136` | No | The video turns up in the report | Missing |
| `crates/marathon-ios/src/listener/device_log.rs:90` | No | The log turns up in the report | Missing |
| `result_bundle_run_listener.rs:186`, `apple_simulator_device/worker.rs:340` | Yes | Already treated as failed, though the files arrived | Treated as failed, files missing |

Measured with sshpass 1.06, openrsync and a throwaway sshd behind a loopback throttle, 24 MiB at 1 MiB/s, TERM at 5 s. The review measured the first two rows the same way, with the file complete 21.3 s later.

| | Child's status | Afterwards |
|---|---|---|
| `sshpass`, TERM to it alone (Kotlin) | -15 | Complete 27.4 s after the TERM |
| Key authentication, TERM to rsync (Kotlin and now) | 20 | Never complete, 3.8 MB in `.rsync-partial/video.bin` |
| `sshpass`, TERM to both (now) | -15 | Never complete, 3.8 MB in `.rsync-partial/video.bin` |

**For the owner: a no-progress limit on receives instead.** The 30 s is a total, so it also cuts a pull that is moving. Whether it should become a limit like D31's is the owner's decision. The behaviour here is unchanged. It would take:

- The flag on a receive's argv, as sends have, with a duration to choose. A pull has no installer's three attempts, but listeners pull one after another at the end of a batch, so a dead worker costs this much per pull, where it costs 30 s now. The keep-alive ends a dead link in about 60 s, but not a wedged process.
- Stall detection on a receive: `read_stderr` gets `reported`, and the deadline branch in `process` becomes the `stall.notified()` one. That is little code, since `stop_stalled_send` exists.
- A new deviation, as this changes what the worker sees: on an rsync 3 host the flag reaches `rsync --server`, and the fake worker's `rsync_args` allows only `--timeout=300`.
- A comparator allowance in `vendor_transport.host_argv`. `test_d31_*` pins that a flag on a receive differs, and `the_receive_deadline_match_kotlin` would change, with `deadlines.jsonl` staying the witness.
- It would help key authentication too, which has lost these files since Kotlin.

**Combining with x16.** x16 adds a `ps -o lstart=` check to the listing and the KILL in the same function, and edits its doc comment. So this
change leaves that comment and the signature where they are, and the receive gets the check with no more work. See the table below.

**A rough edge in the tests.** An rsync killed under `sshpass` has been orphaned, so `kill(pid, 0)` can still succeed for a moment until init
reaps it. `stalled_receive_runs` polls for that, where `stalled_send_runs` asserts it at once on a case that does not reach KILL.

**Failure without it**, with `stop_process` replaced by Kotlin's TERM and wait:

| Test | How it fails |
|---|---|
| `a_receive_past_its_deadline_under_sshpass_also_stops_rsync_u155` | rsync survives |
| `only_a_receive_under_sshpass_lists_children_u155` | Its `sshpass` round: rsync survives |
| `the_real_rsync_under_the_real_sshpass_is_stopped_at_a_receives_deadline_u155` | **Fails intermittently, so it is no proof of the fix.** Sometimes real rsync and ssh survive. Under this revert it failed in the first run here, among other tests at high load, passed 4 of 4 alone at a load of 4–8, and passed 6 of 8 at high load in the review, with both already gone when the receive returned. The cause is not established |
| `a_receive_past_its_deadline_that_ignores_term_is_killed_u155`, `rsync_that_ignores_term_at_a_receives_deadline_under_sshpass_is_killed_u155` | The process is not killed |
| `the_real_rsync_is_stopped_at_a_receives_deadline_u155` | Passes: TERM has always ended openrsync and its ssh. It pins that this path stays right |

These runs have a silent peer, unlike the measured pull above, where rsync carried on after `sshpass` had gone. The deterministic proof is the four stand-in tests above: `…under_sshpass_also_stops_rsync_u155`, `only_a_receive_under_sshpass_lists_children_u155` and the two `…ignores_term…` tests. The send's test, `the_real_rsync_under_the_real_sshpass_gives_up_on_a_hung_ssh_d31`, has the same shape, so x13's table may overstate it in the same way. That was not run here.

**Comparators.** No allowance is needed. `deadlines.jsonl` stays the witness of the deadline, with the same test, and no host argv changed: a
receive still has no `--timeout`.

## U157: a channel open with no reply

**What Kotlin and sshj do, from sshj 0.39.0's bytecode.** `PatchedSSHClient.startSession` calls `AbstractDirectChannel.open()`, which writes
the request and does `openEvent.await(conn.getTimeoutMs(), MILLISECONDS)`. `ConnectionImpl` takes that from `TransportImpl`, whose default is 30,000,
and the Kotlin code sets no other. The channel is attached to the connection in its constructor and stays attached after the expiry, so a late
confirmation opens it and nothing closes it. The connect timeout covers only the socket, so it does not apply here, and the keep-alive only
ends a connection that has died (60 s by default).

**The bound.** It stays those 30 s, which `start_session` already had, and the error stays Kotlin's. So a slow worker fails nowhere it did not
fail before (`a_slow_open_inside_its_30_s_still_works_u157`), and no new limit was needed.

**Why nothing can be closed at the bound.** A CLOSE names the peer's channel number, and only the confirmation brings that. So at the bound there
is nothing to close, and unlike U154 the caller does not wait for anything more: it returns at 30 s, with one new ERROR line:

```
No reply to opening chan#<n> in 30 s. If the worker answers later, that channel will be closed then
```

**The fix.** Dropping russh's future was what lost the channel, so the open now runs in a task of its own, which ends on the reply, the transport's
death or the client's own close (`close_ends_the_wait_for_an_open_that_was_abandoned`). The caller takes the reply through a oneshot. If it has gone, because it
timed out or its future was dropped (`a_caller_that_goes_away_during_an_open_leaves_no_channel_behind_u157`), the task owns the reply.
As first merged, that held only for a caller that went away before the reply reached the oneshot. One dropped after that, but before it was
polled again, dropped the channel without CLOSE. x38 fixed it: the reply stays the task's until the caller claims it
(`docs/wp/x38-findings.md`). What the task does with a reply nobody claims:

| Late reply | What the task does |
|---|---|
| Confirmation | CLOSE, then drains until the peer's CLOSE or the transport's end, as the pump does, so that russh's queue does not hold up the connection |
| Refusal | Takes its text off the queue. Otherwise the next refusal reads it as its own: `a_late_refusal_does_not_give_its_text_to_the_next_failure_u157`. Since x38 there is no queue: each open gets its own text from russh, so a refusal nobody takes leaves nothing behind |
| None | Nothing, until the connection ends |

A reply that gets in at the very moment of the bound goes to the caller: the receiver is closed first and then read once. Since x38 the task
still gets it back if the caller drops it unclaimed.

**One cost, and one thing it holds.** Each open now spawns a task, which ends with the reply. No wire order changes for sequential commands, and
concurrent opens already had no fixed order on a multi-thread runtime. While it waits, the task holds the russh handle, so a client that is dropped
without `close()` keeps its transport until the reply or the transport's end. The factory's clients are closed, so this needs a caller that forgets to.

**Real sshd.** With its session process stopped before the open and resumed after the failure. `u157_an_open_with_no_reply_is_closed_when_the_peer_resumes`
counts the slots free on the connection before and after, so it holds for any `MaxSessions`:

| Run | Result |
|---|---|
| Fix, macOS sshd, `MaxSessions 1` | Failure at 30,003 ms, slot back (1 of 1 free) |
| Fix, macOS sshd, default `MaxSessions` | Failure at 30,002 ms, 10 of 10 free |
| No CLOSE from the task, macOS sshd, `MaxSessions 1` | **0 of 1 free**: the late channel holds the slot |

**Failure without it**, with the task's CLOSE and the refusal's take removed: `an_open_with_no_reply_fails_with_its_timeout_and_a_late_reply_is_closed_u157`,
`a_caller_that_goes_away_…`, `a_late_refusal_…` and the real test. `an_open_that_is_never_answered_still_returns_u157`, `other_sessions_keep_working_while_an_open_waits_for_a_reply_u157` and
the task-lifetime test pass either way: they guard the bound, the connection and the task.

**The fake worker** has a new fault, `late_open`, documented in `docs/design/fake-worker.md`: the first open of a connection gets its reply late.

**Comparators.** No allowance is needed: a healthy open is the same on the wire.

## U158: a receive's pipes

**Verified.** After a process that ended by itself before the deadline, `process` joined its pipe readers with no limit, because x13's bound was for sends
only. A child that outlives it and holds a pipe could then hold up the receive for as long as it lived. The real case: rsync is killed, and the real ssh it
started, hung on a silent peer, keeps rsync's stderr.

**Fix.** The bound now applies to both directions: 5 s of real time after the process ended (`PROCESS_LIMIT`, rule 6 of `docs/testing.md`), then the readers are
cancelled, as for sends. On a healthy exit the pipes close at once, so it adds nothing.

| Stays as it was | How it is pinned |
|---|---|
| The process's status decides the result | `…succeeds…` returns `true`, `…fails…` returns `false` |
| What was read by then, which the failure log prints | `…fails…` asserts `what rsync said` in the error line |
| The `Timeout of 30 seconds reached` diagnostic, only when the deadline passes | The harness now asserts it for `stalled_receive_runs` and its absence for `early_exit_receive_runs` |

**"The same stop" means the readers' stop.** The child that holds the pipes is not stopped, on a receive as on a send. Its parent has gone by then, so `pgrep -P` cannot find it,
and it was never listed, since only a stop past a deadline or a stall lists children. So a real ssh in this state stays until it ends by itself. The tests assert that it is still
up when the receive returns, which shows that the bound and not the end of the pipes let it return.

**Failure without it**, with the bound back to sends only: all three `*_u158` tests, each at its 90 s catch. The real one is
`the_real_ssh_holding_the_pipes_of_a_killed_rsync_does_not_hold_up_a_receive_u158`, at factor 1.

**Comparators.** No allowance is needed: no argv or status changes.

## What was verified, and how

| What | How | Result |
|---|---|---|
| `marathon-apple`, `fake-worker`, `marathon-ios`, `marathon-macos` on this Mac | `cargo test -p …`, under load from other work | 821 passed, 0 failed, 19 ignored (the real-sshd suite, run separately) |
| `ssh_real_sshd.rs` in full on this Mac's sshd | A throwaway `sshd -D -e -f <temp config>` on a loopback port, `--ignored --test-threads 1` | 19 passed |
| On Linux with OpenSSH 10.2p1 and rsync 3.4.1, from a temp checkout with its own target directory, all removed afterwards | `rsync_file_bridge`, `ssh_session`, `ssh_task_lifetime`, and the U154 and U157 tests on a throwaway sshd there | 30, 35, 4 and 2 passed, U157 at 30,000 ms with 10 of 10 free. That box has no `sshpass`, so the two tests with the real one print their skip message. This run came before the review's changes to test timing, and was not repeated |
| The real tools, at factor 1 | sshpass 1.06, openrsync (protocol 29), OpenSSH_10.5p1 on this Mac, against a silent listener, as the send's test does | Passed, nothing left behind |
| Validators | `make_registry.py --check`, `test_evidence.py` | Clean, 66 tests |
| A trial merge with x16 | `wp/x16-ssh-connection` merged into the fixes in a temporary worktree, removed afterwards | No conflict. The `*_u154`, `*_u155`, `*_u157` and `*_u158` tests and the abandoned-open task test pass on it, at factor 1 for the real tools |

## What was not verified

- A real worker's own stall. The real sshd here is stopped with SIGSTOP, which stands for a stalled peer, not for any stall seen in use.
- `sshpass` with rsync 3: that box has no `sshpass`.
- Workers other than OpenSSH for U154. A server that handles requests concurrently could behave differently after CLOSE.

## Incidental bugs

The two found while doing U154 and U155 were fixed here at the owner's request: U157 and U158 above. None is left open.
