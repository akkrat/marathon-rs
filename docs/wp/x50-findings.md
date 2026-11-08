# X50: rsync stall detection with rsync 3.5 and sshpass 1.10 (D31, D43, D54)

Cutting 0.12.0-rc.1 moved CI to GitHub's `macos-15` runner, where `brew install rsync sshpass` gives rsync 3.5.2 and
`sshpass` 1.10. Three real-tool tests in `crates/marathon-apple/tests/rsync_file_bridge.rs` failed there:

- `a_pull_that_stops_moving_is_stopped_and_leaves_its_partial_file_d43` and its `sshpass` variant: the bridge logged an
  ordinary receive error with rsync's `[generator] io timeout after 2 seconds -- exiting`, not `receive error: no progress
  in 300 s receiving …`;
- `the_real_rsync_under_the_real_sshpass_is_stopped_when_a_receive_stalls_d43`: the stalled receive returned `Ok(true)`.

**Result.** Neither failure is rsync 3.5's. The pulls fail with any rsync 3 on the host, 3.4.1 too, because the fake worker
runs `/usr/bin/rsync`, which on macOS is openrsync, and rsync 3 reports against openrsync; on the test's compressed clock
that report came too soon to count, which is what D31's rule says it should do. The bridge classifies those stalls correctly
with the real `--timeout=300`, and the test now uses a host `--timeout` that keeps its compressed clock faithful. The
`Ok(true)` is `sshpass` 1.10's: on a TERM that comes before it has seen anything from ssh, such as the password prompt, it
exits 0 at once without waiting for rsync (U181). That was a real bug in the bridge under password authentication: a send or
receive to a worker whose ssh hung before the prompt, stalled or stopped, counted as a success. The bridge now sends its
TERM past `sshpass`, to rsync and what it started, so that `sshpass` exits with rsync's own status.

These tests had never run on macOS CI before: every `macos-14` run since they were added was cancelled, and the last green
one, on 2026-11-04 (with rsync 3.5.0 and `sshpass` 1.10 already), predates them. On this Mac `PATH` has openrsync and
`sshpass` 1.06; the Linux gate box has rsync 3.4.1 and no `sshpass`.

## Versions

Built for this WP from the official source tarballs into a private prefix, checksums checked against the GitHub release
digest and Homebrew's formula: rsync 3.5.2 (protocol 33) and 3.4.1 (protocol 32), configured with `--disable-xxhash
--disable-zstd --disable-lz4 --disable-openssl --disable-md2man --disable-idn --with-included-popt`; `sshpass` 1.10 and
1.09. Also used: this Mac's openrsync (`/usr/bin/rsync`, protocol 29) and Homebrew's `sshpass` 1.06. The tests were pointed
at a version through `PATH`, which `real_tool` reads; `real_sshpass` now reads `PATH` first too.

## What the bridge sees

**A hung ssh** (a peer that accepts and never answers, `--timeout=5`, run by hand), the same for a send and a receive:

| rsync | Status | stderr | Time |
|---|---|---|---|
| 3.4.1 | 30 | `[sender] io timeout after 6 seconds -- exiting` (or `[Receiver]`), then `rsync error: timeout in data send/receive (code 30) at io.c(201) [sender=3.4.1]` | 9 s |
| 3.5.2 | 30 | the same, `io.c(266)` and `3.5.2` | 9 s |
| openrsync | 255, when the peer finally went | `rsync(<pid>): error: poll: timeout` at 5 s, then it waits for ssh | 898 s here |

Under `sshpass` 1.06 and 1.10 the statuses and text are the same while rsync ends by itself.

**A pull whose bytes stop mid-file**, through the bridge test's throttle and the fake worker's real `rsync --server`:

| Host rsync | Worker rsync | What rsync does |
|---|---|---|
| 3.4.1 | openrsync | `[generator] io timeout after 2 seconds -- exiting`, exit 30, about 3 s after the bytes stop with `--timeout=2` |
| 3.5.2 | openrsync | the same; with `--timeout=20`, exit 30 at 29.9 to 30.0 s after the bytes stop, at four stall points 8 to 22 s into the transfer, each saying `io timeout after 20 seconds` |
| 3.5.2 | 3.5.2 | nothing: still waiting 15 min after the bytes stopped with `--timeout=20`, on a real clock |
| 3.4.1 | 3.4.1 | nothing, as x26 found; here nothing before the bridge's look stopped it, 16 s after the bytes stopped with `--timeout=20` on the compressed clock |
| 3.4.1 / 3.5.2 | 3.5.2 / 3.4.1 | nothing before the bridge's look stopped them, the same way; whether they would report later was not measured |
| openrsync | openrsync | `poll: timeout`, then waits for ssh |

The `received SIGUSR1 (code 19)` in CI's text is the receiver child's line: the generator, which is the process the bridge
waits for, exits 30 and `kill_all(SIGUSR1)`s the receiver (`cleanup.c`, unchanged from 3.4.1). The receiver exits 19; the process the
bridge waits for exits 30 (review: S-x50-1). rsync 3.5
changed nothing here that matters to the bridge: `check_timeout`, `maybe_send_keepalive` and `_exit_cleanup` read the same in
both, and a pull that moves for 38 s with `--timeout=6` finishes on both.

So with the real `--timeout=300`, rsync 3 against openrsync reports about 450 s after the bytes stop, and the bridge's own
look at the files has stopped the pull at 300 to 310 s. Either way it is a stall. In the test, the host rsync got
`--timeout=2` while the bridge's clock ran 20 times fast, so rsync exited 60 clock seconds after the bytes stopped, short of
the 285 s a report needs. That is D31's rule working: a failure that comes sooner is never a stall. The test passed only
where rsync never reports (rsync 3 on both ends, as on the Linux gate) or reports and then waits, as openrsync does.

**`sshpass` and a TERM**, measured by hand, with the TERM to `sshpass` and with it to `sshpass`, rsync and ssh together, as
the bridge sent it:

| `sshpass` | Status | What happens |
|---|---|---|
| 1.06 (Homebrew here) | 143 | dies of the TERM; its child stays until the pty's hangup |
| 1.09 (Ubuntu 22.04's) | the child's | passes the TERM on, waits, and exits with its child's status: 23 after 6 s for a child that ignored TERM and exited 23 |
| 1.10 (Homebrew now) | **0** | exits 0 at once, without waiting, whatever rsync then does (openrsync, 3.5.2 or a shell), if the TERM comes before ssh has said anything on its terminal |

The cause, from 1.10's `main.c`: 1.10 moved the work out of its TERM handler ("Remove sig unsafe functions from signal
handler"). The handler now only records the signal, and the main loop forwards it and does `continue`, which in a
`do … while` jumps to the condition: `wait_id` has not been set yet if the TERM came during the first `pselect`, and
`status` is still 0, so the loop ends and `sshpass` returns `WEXITSTATUS(0)`. Its child gets the pty's hangup and, if it
survives that, is orphaned. Once the loop has gone round once, as it does when ssh prompts for the password, `wait_id` is 0
and 1.10 waits like 1.09: the bridge test's pull under 1.10, stopped well after authentication, kept rsync's status even
without the fix, where the hung ssh, which never prompts, got 0. A check by hand with a child that prompts first could not
be made: under 1.10 such a child had no `/dev/tty` here. Recorded as [U181](../found-bugs.md).

In the bridge that 0 meant success: a stall needs a status other than 0 (an rsync that finished in its 5 s keeps its
success), and so does D54's stop (an rsync that exits 0 on the TERM keeps its success). On the test's clock rsync had 50 ms
to exit after its report before the TERM, so the hung-ssh receive under `sshpass` 1.10 returned `Ok(true)`. In production
rsync 3 exits at once after its report, well within its 5 s, so a stall it reports keeps its 30. Under `sshpass` 1.10 and
password authentication, where ssh hung before the prompt, as with a worker that froze or a hung ssh (D31's case), the
TERM made:

- a stall that openrsync reports, since it then waits for ssh: a send counted as sent, a receive as received;
- a D54 stop, which is what a transfer to a worker that had already frozen when it started gets once keep-alive drops the
  device's connection: a send returned `Ok(true)` where it should fail with the loss, a receive `true`, and a cancelled
  send `Ok(true)`.

A transfer whose ssh got as far as the password was not affected, nor was any with key authentication, which has no
`sshpass`.

## The fix

`crates/marathon-apple/src/cmd/remote/rsync/rsync_file_bridge.rs`:

- `stop_child` takes `wrapped`, which `transfer` sets for password authentication, the only case in which the bridge's
  child is `sshpass`. Wrapped, the TERM goes only to the processes `Descendants` knows, which are rsync, ssh and what they
  started, and not to `sshpass`; `sshpass` of every version measured then waits for rsync and exits with its status: 20 for
  rsync 3 ending on TERM, 0 for one that finished, and for one killed by a signal 255 (1.09 and 1.10, by their source). The
  steps are unchanged: 5 s first if rsync reported, then the TERM, 5 s, and KILL to `sshpass`, after which
  `Descendants::end` deals with what is left.
- Only where no listing found any of them, as when `ps` fails or the stop comes before `sshpass` has forked, does
  `sshpass` get the TERM, and `stop_child` says so. A 0 from it is then counted as 143.
- Not wrapped, nothing changes: TERM to rsync first, then to what it started.
- The classification itself was right and is unchanged. It is now the function `is_stall`, so that a unit test can give it
  rsync 3.5.2's text and status.

D54's success for an rsync that exits 0 on the TERM now holds under `sshpass` too, where it used to be `sshpass`'s 0, right
or wrong. What changes for a user: under `sshpass` 1.10, with ssh hung before the password prompt, a stalled transfer is a
stall again (D31's `LeavePool` for a send, D43's message and `false` for a receive), and a stopped one returns its stop.
Under 1.06 and 1.09 the outcomes are the same as before, and rsync, not `sshpass`, is what gets the TERM.

## Tests

| Test | Without the fix | With it |
|---|---|---|
| `rsync_3_5s_report_from_the_generator_is_a_stall_only_after_the_limit_d43` (unit): 3.5.2's and 3.4.1's text against openrsync, statuses 30 and 19, at and after 285 s a stall, at 284.999 s and 40 s not, 0 never, the receiver's line alone never | passes: the classification was right; this pins it | passes |
| `a_stalled_send_under_sshpass_1_10_is_still_a_stall_u181`: a stand-in that exits 0 on TERM, as 1.10 does | the stalled send returns `Ok(true)` | `LeavePool`, and its queued send too |
| `a_stalled_receive_under_sshpass_1_10_returns_false_u181` | `Ok(true)` | `false`, D43's message |
| `a_cancelled_send_under_sshpass_1_10_is_cancelled_u181` | `Ok(true)` | `Cancelled` |
| `a_receive_under_sshpass_1_10_whose_connection_is_lost_returns_false_u181` | `Ok(true)` | `false` and the loss's line |
| `a_cancelled_send_under_sshpass_1_10_whose_rsync_exits_0_on_term_keeps_its_success_u181` | passes | passes: D54's success is kept now that `sshpass` passes rsync's 0 on |
| `the_real_rsync_under_the_real_sshpass_is_stopped_when_a_receive_stalls_d43`, with `sshpass` 1.10 | fails, `Ok(true)`, with 3.5.2, 3.4.1 and openrsync | passes |
| `a_pull_that_stops_moving_*_d43`, host `--timeout` 2 → 20 (`STALLED_PULL_TIMEOUT`) | with 2, fail with rsync 3.4.1 or 3.5.2 on `PATH` | pass |

The other changes to the test file: `real_sshpass` reads `PATH` first, so that a build of another version can be tried;
`stalled_receive` and the new `stopped_transfer_under` take a stand-in for `sshpass`; the comment of
`rsync_under_sshpass_gets_its_step_after_term_d31`, whose `sshpass` no longer gets the TERM where `ps` works.

## Runs

The whole of `tests/rsync_file_bridge.rs`, 60 tests, with the fix, once in each combination, sequentially on this Mac:

| Host rsync | `sshpass` | Result |
|---|---|---|
| openrsync | 1.06 | 60 passed, 39.7 s |
| 3.5.2 | 1.10 (CI's `macos-15`) | 60 passed, 39.9 s |
| 3.4.1 | 1.10 | 60 passed, 39.7 s |
| 3.5.2 | 1.06 | 60 passed, 39.7 s |
| 3.4.1 | 1.09 (as on Ubuntu) | 60 passed, 39.7 s |

The same file without the bridge's fix, with 3.5.2 and 1.10: 55 passed, and the five that fail are the four `_u181` tests
expected to and `the_real_rsync_under_the_real_sshpass_is_stopped_when_a_receive_stalls_d43`. The file as it was, before
x50, with 3.5.2 and `sshpass` 1.06: the two `a_pull_that_stops_moving_*` failed as on CI, and the `sshpass` receive passed
eight runs out of eight, since 1.06 doesn't return 0. The four `_u181` stand-in tests also ran alone against the old bridge
and the new one (4 failed, then 5 passed). The library's rsync bridge unit tests: 4 passed. The worker's rsync in all of
these is this Mac's openrsync, as on CI.

## Follow-up: an rsync that saw its ssh go first (ubuntu-22.04)

After merge review, GitHub's `ubuntu-22.04` gate (procps-ng 3.3.17, bash 5.1.16, rsync 3.2.7, `sshpass` 1.09) failed
`a_cancelled_send_under_sshpass_1_10_is_cancelled_u181` once: the cancelled send returned `Ok(true)` in 0.08 s. Every other
gate passed it, and so did the other four `_u181` tests there.

**The cause is the order of the stop's signals, not `ps`.** Under `sshpass` the bridge's TERM goes to the processes the
listing knows, and `Descendants::signal` sent it in a `HashSet`'s order, so the stand-in for ssh could get it before the
stand-in for rsync. The rsync stand-in waits for its ssh (`wait`); if the thread sending the signals is preempted between
the two, ssh dies, `wait` returns 0, and rsync runs to its end and exits 0 before its own TERM comes. `sshpass` passes that
0 on, which D54 keeps as a transfer that finished all the same. Without `sshpass` the child, rsync, gets its TERM before
anything else, so only the wrapped path had the race, and it needs the sending thread preempted, as on a busy CI runner.

- **Confirmed** with a temporary build that sent the TERMs children first with 100 ms between them: both cancelled-send tests
  under a stand-in `sshpass`, the `_u181` one and D54's `a_cancelled_send_under_sshpass_also_stops_rsync_d54`, returned
  CI's `Ok(true)` 3 times out of 3 each; parents first, they passed 3 out of 3. So D54's test had the same race and passed on
  ubuntu-22.04 by luck.
- **Ruled out:** procps-ng 3.3.17, built on the Linux gate box, prints `ps -A -o pid=,ppid=,lstart=` exactly as 4.0.4
  does, and both take `lstart` from `/proc/stat`'s boot time, unchanged over 60 calls for a new process. bash 5.1.16 and
  5.3, built or installed there, always die with 143 on a TERM while in `wait`, whichever of rsync and ssh is signalled
  first, 30 out of 30 each: without a trap, a non-interactive bash has no TERM handler (bash's `sig.c`), so a TERMed
  stand-in can't exit 0. On that 16-core box, with bash 5.1.16 and `ps` 3.3.17 first, the test passed 20 out of 20 alone
  in each of four combinations, the whole file 3 out of 3, and both cancelled-send tests 160 out of 160 at 16 at a time.
  The race was not reproduced without forcing the preemption.

**The fix.** `Descendants::signal` finds the processes a generation at a time and signals them in that order, so each
process gets its signal before the processes it started, as the child already did before them. An rsync then has its TERM
before its ssh can end. A real rsync whose ssh ended first fails anyway (connection closed); the stand-in's `wait` is
what made it a success, and the order makes the test independent of it.

**The test.** `a_stop_signals_each_process_before_the_ones_it_started_u181` (unit) starts a chain of five processes below
the child and checks that each one signalled (with SIGCONT) is the child of the one before. It failed 5 out of 5 with the
old order, which matches the chain 1 time in 120, and passes 5 out of 5. The cancelled-send tests are unchanged: with the
order fixed, the stand-in rsync is always TERMed first.

## Limits

- Where `ps` fails, under `sshpass` 1.10 a transfer that finished just as the TERM came counts as failed, since `sshpass`'s 0
  can't be told from its false one. A stall can't finish then, so this costs only that race.
- The bridge's TERM is not the only way to U181: a TERM from outside before ssh has prompted, such as a CI job's cancel
  reaching the process group, still makes `sshpass` 1.10 exit 0 at once. Marathon is then exiting too.
- The table's 1.5 times `--timeout` was measured with a stall in one direction (worker to host), the one the throttle makes,
  with a single file. A worker that stops both ways, or several files, were not measured with rsync 3.5.2; D43's look at the
  files covers them whatever rsync does.
- rsync 3.5.2 to 3.5.2 ran once, on a real clock without the bridge's look (a measuring build of the test); the other rsync 3
  pairs ran through the bridge on the compressed clock, which stopped them before a late report could show. The measuring
  build also timed the 3.5.2-to-openrsync reports; it was not committed.
- Neither the Linux gate box nor the GitHub runners were used for these runs; this Mac's builds stand in for Homebrew's.

## Incidental bugs

- **U181**, `sshpass` 1.10: exits 0 at once on a TERM that comes before ssh has said anything, without waiting for its
  child (above). Worked around in the bridge; not reported upstream.
- `crates/marathon-apple/tests/support/mod.rs`'s `Clock` has no `now_millis`, so it reads 0, as the trait's default says a
  clock with no time of its own does. A receive on it never looks at its files' age. Only `real_pull`'s moving runs use it,
  where that doesn't matter; noted because a test that wanted the look on a real clock would hang, as a measuring run here did.
