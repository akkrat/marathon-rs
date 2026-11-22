# x111 findings: a quieter worker supervisor

Base `281c4005`. Worker-side implementation detail of D96; no D-row: the worker commands, their protocol, outputs,
receipts, the controller and every run outcome are unchanged. No U row: no product bug was found.

## Problem

Every supervised SSH command and every supervised recorder runs under `run_owned` in
`crates/marathon-apple/src/cmd/remote/ssh/sshj/supervisor/worker.pl`. Its monitor loop read the token state with
`locked()` (4 to 6 file system calls) on every pass and slept 25 ms on the ready pipe (while starting) and 25 ms on
the status pipe, so an idle command or recorder passed the loop 40 times a second for its whole life. In a local
controlled experiment that was most of the extra worker CPU the Rust runner puts on an SSH worker compared with the
JVM runner (about +0.6 to +0.9 CPU-s per batch of five ~22 s UI tests).

## Design

One `select` per pass now waits on every descriptor that signals an event, with a timeout that is the time to the
next thing that has to happen:

- **Descriptors:** the payload's status pipe (exit), the ready pipe while starting, a started recorder's stderr pipe
  (the `Recording started` marker and the bounded diagnostic) until it reaches end of file, and the token's wake FIFO.
  The pass itself is unchanged: it reads the state, applies force, start, signal forwarding, the recorder's INT/TERM/KILL
  rules and the status read exactly as before, with zero-timeout `readable` checks where it used to wait 25 ms.
- **Timeout:** the smallest of the lease expiry (while it is ahead), a recorder's TERM deadline (2 s after the first
  stop or expiry while TERM is not sent) and KILL deadline (1 s after TERM when forced), a retry when the token
  lock was busy or a confirmed start is not yet published, and the fallback poll; never below 10 ms except for that
  retry. Deadlines win ties. The retry is 10 ms, except the first one after a FIFO wake, which is 1 ms: the waking
  request holds the lock only until its commit, a moment later (round 2; a 10 ms retry sleeps about 15 ms on macOS).
  The selection is the pure function `next_wake`, so a test can drive it with a controlled clock.
- **Fallback poll:** 25 ms (the old cadence) until the wake FIFO is open, 1 s after.
- **Wake FIFO** (`<root>/<token>.wake`, mode 0600 in the private 0700 root): the owner creates it with
  `POSIX::mkfifo` once the command has run for 250 ms, opens the read end non-blocking and keeps its own write end
  open so the reader never sees end of file. `control` writes one byte to it for any request that changes a running
  or recording token (stop, INT, TERM, KILL, renewals), under the token lock and before it commits the state (see
  below). The owner closes and unlinks it when its loop ends, before the receipt. Retirement accepts a `.wake` entry, which only an owner killed before its
  cleanup can leave.
- **Why the FIFO opens late:** loading POSIX and setting up the FIFO cost a short command more than its few 25 ms
  polls. With the FIFO from the start, 100 sequential supervised `/usr/bin/true` runs cost 10.6 ms CPU and 12.1 ms wall
  each against 7.9 and 9.6 ms for the old loop; with the 250 ms delay 8.1 and 9.8 ms against 8.0 and 9.6 ms (three
  runs each, identical within each variant). Short commands therefore poll the state at the old cadence; their ready
  and exit events wake the owner at once, so they react sooner than before, never later.

### Why no wake is lost

A control request takes the token lock, writes the pending state, writes its byte to the FIFO, renames the pending
state into place (the commit) and only then releases the lock (round 2; round 1 wrote the byte after releasing the
lock, see below). The owner reads the state only with a non-blocking `flock` on the same lock; when the lock is busy
it keeps its previous view and retries soon (`b`: 1 ms for the first retry after a wake, 10 ms otherwise).

The argument: the writer holds the lock continuously from before its byte is written until after its commit, or
until it dies without one. So any state read by the owner that succeeds after the byte was written happens after
the writer released the lock, and sees the commit (or the unchanged state of a writer that died before committing,
which is correct: nothing was requested). It remains to show such a read happens promptly:

- The owner is in `select`, or reaches it, after the byte was written: the byte makes the FIFO readable, `select`
  returns at once (`w`), the owner drains it and its next pass reads the state; if the writer still holds the lock
  (it is between its byte and its rename), that read is busy and is retried (1 ms, then every 10 ms) until the
  lock is free.
- The owner is between its state read and its `select` when the byte arrives: the byte stays in the FIFO and the
  `select` returns at once, as above.
- The owner drained the FIFO between the byte's write and its next read: that read is after the byte, so it is busy
  or follows the commit, as above.
- The FIFO did not exist yet (`ENOENT`) or had no reader yet (`ENXIO`) when the writer opened it: the owner creates
  and opens it after that moment, then reads the state in the same pass, so that read is after the writer took the
  lock and is busy or follows the commit. If the owner's loop had already ended, there is nothing left to wake.
- The writer dies (killed, or crashes) at any point: before the lock, nothing changed; holding the lock before the
  rename, the rename never happens (it is atomic) and the kernel releases the lock, so the owner reads the old state,
  which is right because the controller saw the request fail; after the rename, the byte is already in the FIFO, so
  the owner wakes and reads the commit once the kernel has released the dead writer's lock.
- The writer is stopped (SIGSTOP) while holding the lock: every state read, the owner's and other requests', is busy
  until it resumes or dies. The owner keeps enforcing the expiry and escalation deadlines it last read. This is the
  same as before x111 and is not a lost wake.

The byte is written whole or not at all (one byte, below `PIPE_BUF`). `EAGAIN` (the FIFO is full, so a wake is
pending) and `EPIPE` (the owner closed its end; `SIGPIPE` is ignored for the write) are success; `EINTR` on the open
or the write is retried. Any other failure to open or write leaves the change to the owner's fallback poll (1 s),
which is the same behaviour as when the FIFO could not be set up. No pid is involved (no reuse problem); only the
token's owner reads the FIFO; the root is private to the user, so on a shared host no other user can open it. If the
owner cannot create or open the FIFO, it keeps polling every 25 ms, as before. The anchor and payload never hold the
FIFO: it is created after the fork, and Perl opens it close-on-exec.

Round 1 committed the state and released the lock before writing the byte. A control process killed or suspended in
that gap left a committed request with no wake, noticed only at the next fallback poll (the review saw an INT delayed
by 941 ms, against 24 ms). Writing the byte first costs a woken owner usually one busy retry: the request still holds
the lock for its rename when the owner wakes. That first retry is 1 ms, so the owner sends INT 1 to 2 ms after its
wake.

### Deployment

`worker.pl` is not installed on workers ahead of time: each run's bootstrap writes the controller's own copy into a
fresh private root (`bootstrap.pl`), and every supervised operation of that run uses that copy. Controller and
worker script are therefore always the same version and no mixed-version interoperation exists. The Rust side is
unchanged.

### Test hook

With `MARATHON_D96_TEST_WAKES=<file>` in its environment, an owner appends one line to that file when its loop ends
(before the receipt): one letter per pass, by what woke it (`e` a pipe, `w` the wake FIFO, `d` a deadline, `b` a busy
retry, `p` the fallback poll). Nothing else reads it. Two more test-only variables: `MARATHON_D96_TEST_FALLBACK_MS`
sets the owner's fallback poll (default 1000 ms), so a witness can tell a wake from a poll, and
`MARATHON_D96_TEST_KILL_AFTER_COMMIT` makes a control process kill itself right after its commit, still holding the
token lock. SSH does not forward these variables, so a controller cannot turn them on for a worker. For the selection
test, `worker.pl` returns before its command dispatch when it is loaded with `do` (`return 1 if caller`); run as a
script, as every worker command runs it, it is unchanged.

## Invariants kept

- D96/D98 semantics: lease expiry kills a command; a forced or expired recorder gets INT only after its own start
  marker, TERM 2 s after the first request and KILL 1 s after TERM, keeping those times from the first request; a
  failed recorder keeps at most 8192 bytes of stderr; receipts, retirement and the control protocol are unchanged.
- Every decision is still taken on a pass that has just read the state; the FIFO only decides when that pass runs.
- A stop wakes the owner at once (the FIFO); its state read then usually waits one 1 ms busy retry for the request to
  release the lock, so a stop is applied 1 to 2 ms after the wake instead of on the old 25 to 50 ms loop; deadlines are slept towards exactly and fire within the 10 ms floor (the old loop could be up to 50 ms late);
  stderr and exit are seen as they happen instead of on the next tick.

## Witnesses (`crates/marathon-apple/tests/ssh_supervisor.rs`)

- (a) `a_stop_request_wakes_the_worker_supervisor_through_its_fifo_x111` and (a') the P1 witness
  `a_stop_request_killed_right_after_its_commit_still_wakes_the_worker_supervisor_x111` (round 2): a `run` payload
  that exits 3 on INT, an owner whose fallback poll is set to 120 s, and INT as the only control request, sent once
  the owner has the FIFO open for reading (the test's non-blocking writer stops failing with `ENOENT`/`ENXIO`; it
  keeps that writer, writing nothing, so the owner's reader never sees end of file). In (a') the control process
  kills itself right after its commit, while it still holds the token lock. INT must reach the payload within the
  60 s hang guard, which with a 120 s fallback only a wake can do (or a commit just before the owner's first state
  read after opening the FIFO, which sees it at once); the run must end `done:3`, the trace must hold exactly one `w`
  and the FIFO must be gone. Round 1's version waited only for the FIFO path, which exists before its reader opens,
  and forced the payload afterwards, so the force's own wake could satisfy it.
- (b) `an_idle_supervised_command_and_recorder_wake_rarely_x111`: an idle `run` command and an idle started recorder
  over a 3 s window; each trace must have at most `elapsed seconds + 20` passes. Seen: 11 to 13 passes (command) and 15 to 17
  (recorder) in about 3 s; the old cadence gives about 95 per trace in the same window.
- (c) `the_worker_supervisor_sleeps_until_its_next_deadline_x111` (round 2, replacing round 1's trace check, which
  required a `d` pass before expiry and so failed when the owner was descheduled across the expiry after a fallback
  poll): a controlled clock. It loads `worker.pl` with `do` and calls the timeout selection `next_wake` with given
  times: lease expiry before and after the fallback poll, ties (the deadline wins), an expired lease, the 10 ms floor,
  a busy retry, a 120 s fallback, TERM 2 s after the first stop request (and none for an INT request alone), KILL 1 s
  after TERM when forced (and none without force), and the nearest of several deadlines.
- (c') `escalation_and_lease_expiry_end_with_their_receipts_x111`: the receipts only, separately: an unconfirmed
  recorder that ignores TERM, given KILL, ends `done:137`; a `run` command with an unrenewed 1.5 s lease exits 137
  with `done:137`.
- The timings themselves stay pinned by the existing tests, unchanged:
  `an_unconfirmed_recorder_gets_term_and_kill_at_the_existing_cutoffs_d98` (TERM at ≥ 2 s, KILL at ≥ 3 s),
  `lease_expiry_gives_an_unconfirmed_recorder_term_at_the_existing_time_d98` (expiry + 2 s),
  `worker_expiry_compacts_its_receipt_and_retirement_waits_for_owned_groups_d96` (expiry, TERM, KILL; the root holds
  only receipts and the script afterwards, so the FIFO is removed),
  `a_detached_recorder_gets_int_only_after_its_own_start_marker_d98`,
  `a_start_marker_split_across_writes_counts_only_when_its_line_is_complete_d98`,
  `forced_and_expired_recorders_finalize_and_failed_recorders_keep_bounded_stderr_d96`, and the loopback-sshd
  `worker_commands_and_senders_stop_over_independent_real_ssh_d96` (ran here, not skipped). Only two comments in
  existing tests changed (they described the 25 to 50 ms read loop).

Fails without the change, checked in a scratch worktree with the new tests (round 1): the base `worker.pl` fails all
three (no trace); removing the `wake()` call fails (a); a 25 ms poll throughout fails (b); dropping the expiry deadline
or the TERM/KILL deadlines failed round 1's (c).

Round 2, in a scratch worktree with the round-2 tests (removed afterwards):

- Round 1's head `9c972f86`, with the two new test hooks added at the same places (the kill right after the rename,
  which there comes before the wake; the fallback variable): (a') fails, 10 of 10 runs (no INT within the 60 s hang
  guard); (a) passes there, as it should (round 1 does wake when the writer lives); (c) fails (no `next_wake`).
- The final script with the wake moved after the rename (still under the lock): (a') fails, (a) passes.
- The final script without the wake: (a) and (a') fail.
- `next_wake` mutants, each failing (c): no expiry deadline, no TERM deadline, no KILL deadline, ties to the poll
  (`<` for `<=`), a 10 ms floor for the 1 ms retry.

Stress (final script, round 2): the five x111 witnesses 50 times unloaded (load average 1.9 to 3.9), then 20 times
under 18 busy loops on this 18-core Mac (load average rising to 16.5), all passed. In the 100 unloaded stop runs the
owner's trace ended `wbe` every time (wake, one 1 ms busy retry, exit); under load 26 of 40 `wbe` and 14 `we`. INT
reached the payload 17 to 21 ms after the test started the control process (median 19; 8 to 27 ms under load); the
test polls for the payload's marker every 10 ms, so this includes up to one such interval and the control process's
Perl start. Measured inside the owner (a temporary timestamped copy of the script, 10 runs), INT followed the wake by
1 to 2 ms. With the 10 ms retry first tried in round 2, the test saw a median of 31 ms. Idle traces had 12 to 18
passes in about 3 s (ceiling 23).

## Measurements

This Mac (M-series, 18 cores, shared with other sessions), old (`281c4005`) against new `worker.pl`, both run as
the user's private loopback `sshd` would run them: one supervised `/bin/sleep N` (rusage of the supervisor plus
payload from `/usr/bin/time`, so it includes Perl's start-up of ~10 ms) and one idle supervised recorder (a detached
payload that printed the start marker; CPU time of its supervisor from `ps` just before its stop). Three runs of each,
concurrently; 10 ms resolution.

| setup | window | old command | new command | old recorder | new recorder | load avg (1 min) |
|---|---|---|---|---|---|---|
| direct | 60 s | 0.25 | 0.01–0.02 | 0.28 | 0.01 | 3.0 → 2.6 |
| direct | 300 s | 1.56 | 0.07 | 1.71–1.72 | 0.07 | 2.4 → 1.7 |
| direct, 18 busy loops | 60 s | 0.22 | 0.02 | 0.23 | 0.01 | 3.7 → 21 |
| over loopback sshd | 60 s | 0.34–0.35 | 0.02 | 0.39 | 0.02 | 1.2 → 0.9 |
| over loopback sshd | 300 s | 1.84 | 0.08–0.09 | 2.06–2.08 | 0.08–0.09 | 0.9 → 1.5 |
| over loopback sshd, 18 busy loops | 60 s | 0.21 | 0.02 | 0.24–0.25 | 0.01–0.02 | 3.7 → 16 |

(CPU-seconds per supervised process over the window.) The direct rows ran the first commit (FIFO opened at
once); the sshd rows ran the final script, whose first 250 ms at the old cadence add at most ten passes. The old loop costs about 5 to 7 ms of CPU per second per
supervised process; the new one about 0.2 to 0.3 ms per second, Perl start-up included, about 20 to 25 times less.
The synthetic load did not raise the per-pass cost on this machine (the x108 farm-like effect was not reproduced). For a batch of
about 110 s (five ~22 s tests) that is about 0.55 to 0.65 CPU-s for the old supervisor of the xcodebuild command
against about 0.02 to 0.03 CPU-s, and the same again for each recorder supervisor running beside it. The x108
experiment saw a larger per-batch cost under real xcodebuild load (+0.6 to +0.9 CPU-s); the wake count falls by the
same factor there, so the saving should be at least proportional, but that was not measured with real batches here.

## Not verified

- A farm A/B (video off, alternating runs) is the decider for whether this closes the batch-start gap on busy hosts;
  it is to be run later.
- No real xcodebuild batch was measured with the new script (the idle measurements above isolate the supervisor;
  xcodebuild's own CPU is unaffected by it).
- Linux workers were not exercised here; the code uses only POSIX FIFOs, `select` and `mkfifo`, which behave the same
  on Linux, and the remote gate runs the witnesses there.
- The measurements above are round 1's script. Round 2 changes only when a request writes its byte and how soon a
  woken owner retries a busy lock, not how often an idle owner wakes (the idle witness's pass counts are the same), so
  they were not repeated.
- A control process stopped (SIGSTOP) while it holds the token lock is not covered by a witness; as argued above, it
  blocks every state read until it resumes or dies, as before x111.

## Golden diffs

None.

## Incidental bugs

None in the product. Test hygiene, not investigated further: the loopback-sshd witness
`worker_commands_and_senders_stop_over_independent_real_ssh_d96` leaves its worker root (the script and five compact
receipts) in `/tmp` after every run, on the base as well; such roots accumulate on a developer machine.
