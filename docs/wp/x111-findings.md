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
- **One clock sample per pass** (round 3): the pass's checks (expiry, the TERM and KILL times) and its timeout
  choice use the same sample, taken right after the state read. Round 2 took a second sample for the choice: a pass
  whose checks ran 1 ms before the lease expiry and whose second sample fell at or after it dropped the expiry
  (`next_wake` only counted an expiry still ahead) and slept the 1 s fallback poll, so the kill, or a recorder's
  escalation that the expiry starts, came about a second late (the review saw `done:137` 1 015 ms late). Now
  `next_wake` drops the expiry only once the pass has acted on it (`force`), and a deadline that is due or past gets
  the floor, never the fallback; with one sample, every deadline the checks found due was acted on in that pass, so
  none reaches the selection. The time the pass has spent since its sample (the stderr drain, publishing a start) is
  taken off the chosen sleep, floored at zero, so the owner wakes at the chosen time even when it was descheduled
  after its checks; this reading changes only how long it sleeps, never what it waits for. A zero sleep cannot spin:
  the next pass acts on the due deadline or recomputes from its own sample.
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

### Test hooks

The hooks live in one table, `my %hook;`, declared empty near the top of `worker.pl` and never written anywhere in
it; every hook site only reads it (`$hook{...}`), so in the shipped script every hook branch is dead and no
environment, argument or controller can reach one. The tests install a copy of the script in which that one line is
replaced by `my %hook = map { ... } keys %ENV`, filling hook `<name>` from `MARATHON_D96_TEST_<NAME>`
(`x111_root`). A test pins the shipped script's side: the declaration occurs once, `%hook` nowhere else, no hook is
assigned or deleted, and the script names no `MARATHON_D96_TEST` variable. Round 2 read the variables from the
environment directly; SSH does not forward them, but a worker-side environment export could still have turned on
the self-kill hook for production control requests. A test-only argument was the other option; it would leave the
hook code reachable by anyone who passes the argument, and the copy keeps the shipped script's behaviour independent
of anything a caller supplies.

- `wakes=<file>`: an owner appends one line to that file when its loop ends (before the receipt): one letter per
  pass, by what woke it (`e` a pipe, `w` the wake FIFO, `d` a deadline, `b` a busy retry, `p` the fallback poll).
- `fallback_ms`: the owner's fallback poll (default 1000 ms), so a witness can tell a wake from a poll.
- `kill_after_commit`: a control request kills itself right after its commit, still holding the token lock.
- `pause_across_expiry` (round 3): on the first pass with the wake FIFO open whose checks find the lease expiry
  ahead, the owner sleeps until 5 ms past the expiry before choosing its timeout (round 2's second clock sample
  fell there), and the trace gets a `!`.

For the selection test, `worker.pl` returns before its command dispatch when it is loaded with `do` (`return 1 if
caller`); run as a script, as every worker command runs it, it is unchanged.

## Invariants kept

- D96/D98 semantics: lease expiry kills a command; a forced or expired recorder gets INT only after its own start
  marker, TERM 2 s after the first request and KILL 1 s after TERM, keeping those times from the first request; a
  failed recorder keeps at most 8192 bytes of stderr; receipts, retirement and the control protocol are unchanged.
- Every decision is still taken on a pass that has just read the state; the FIFO only decides when that pass runs.
- A stop wakes the owner at once (the FIFO). Its state read then finds the lock busy while the request finishes its
  commit and is retried after 1 ms, then every 10 ms. Measured here (2 000 requests, three runs, system Perl), a
  request's whole locked section, including opening the lock and checking for a receipt, took 0.13 ms median
  (p90 0.22 ms) without the FIFO and 0.14 ms (p90 0.23 ms) with it: the FIFO's open and write under the lock add about
  12 µs. So the first 1 ms retry almost always succeeds, and a stop is applied 1 to 2 ms after the wake (seen in the
  timestamped runs below) instead of on the old 25 to 50 ms loop; only a request descheduled while it holds the lock
  makes the owner wait longer, in 10 ms steps. Deadlines are slept towards from the pass's own clock sample and fire
  as soon as the owner is scheduled after them; a deadline less than 10 ms ahead at a pass is slept on for the 10 ms
  floor, so it can fire up to 10 ms late (the old loop could be up to 50 ms late). Stderr and exit are seen as they
  happen instead of on the next tick.

## Witnesses (`crates/marathon-apple/tests/ssh_supervisor.rs`)

- (a) `a_stop_request_wakes_the_worker_supervisor_through_its_fifo_x111` and (a') the P1 witness
  `a_stop_request_killed_right_after_its_commit_still_wakes_the_worker_supervisor_x111` (round 2): a `run` payload
  that exits 3 on INT, an owner whose fallback poll is set to 120 s, and INT as the only control request, sent once
  the owner has the FIFO open for reading (the test's non-blocking writer stops failing with `ENOENT`/`ENXIO`; it
  keeps that writer, writing nothing, so the owner's reader never sees end of file). In (a') the control process
  kills itself right after its commit, while it still holds the token lock. INT must reach the payload within the
  60 s hang guard, which with a 120 s fallback only a wake can do (or a commit just before the owner's first state
  read after opening the FIFO, which sees it at once); the run must end `done:3`, the trace must hold at most one `w`
  and the FIFO must be gone. Round 2 required exactly one `w`; the review reproduced a correct run with none: the
  request committed after the owner opened the FIFO but before its first state read, and the owner forwarded INT and
  saw the payload exit in that same pass, before any `select` (round 3 accepts 0 or 1). Round 1's version waited only for the FIFO path, which exists before its reader opens,
  and forced the payload afterwards, so the force's own wake could satisfy it.
- (b) `an_idle_supervised_command_and_recorder_wake_rarely_x111`: an idle `run` command and an idle started recorder
  over a 3 s window; each trace must have at most `elapsed seconds + 20` passes. Seen: 11 to 13 passes (command) and 15 to 17
  (recorder) in about 3 s; the old cadence gives about 95 per trace in the same window.
- (c) `the_worker_supervisor_sleeps_until_its_next_deadline_x111` (round 2, replacing round 1's trace check, which
  required a `d` pass before expiry and so failed when the owner was descheduled across the expiry after a fallback
  poll): a controlled clock. It loads `worker.pl` with `do` and calls the timeout selection `next_wake` with given
  times: lease expiry before and after the fallback poll, ties (the deadline wins), an expired lease the pass has
  acted on, a lease expiring exactly now and one 50 ms past that the pass has not acted on (both the floor, never
  the 120 s fallback; round 3), TERM exactly due and KILL 50 ms past (the floor), the 10 ms floor,
  a busy retry, a 120 s fallback, TERM 2 s after the first stop request (and none for an INT request alone), KILL 1 s
  after TERM when forced (and none without force), and the nearest of several deadlines.
- (c'') `a_pass_descheduled_across_the_lease_expiry_still_acts_on_it_x111` (round 3, the P1 witness): the reviewer's
  interleaving made deterministic with `pause_across_expiry` and a 120 s fallback poll: a `run` command with a 1.5 s
  lease must exit 137 with `done:137`, and a confirmed recorder with a 1.5 s lease must get INT and finalize
  `done:0`, each within 30 s (far below the fallback); each trace must show the pause once and no fallback pass
  after it. Seen: the command ends about 1.56 s after its start, the recorder gets INT about 1.53 s after its start.
- `the_shipped_worker_script_has_no_reachable_test_hooks_x111` (round 3): the inertness check described under Test
  hooks.
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

Round 3, in a scratch worktree with the round-3 tests (removed afterwards):

- Round 2's head `568b697a`, with the hooks moved into a `%hook` table at the same places and the
  `pause_across_expiry` hook added just before its second clock sample: the P1 witness (c'') fails (no exit within
  the 30 s bound: the owner slept the 120 s fallback), and the selection test (c) fails on the new due-deadline cases
  (`120000 p` for a lease expiring exactly now). The other x111 witnesses pass there.
- Mutants of the final script, run against (c''): the round-2 loop (a second clock sample and an expiry counted only
  while ahead) fails; either half of the fix alone passes (a second sample with the new `next_wake`, about 1.54 s; one
  sample with the old expiry condition, about 1.53 s); one sample without taking the elapsed time off the sleep passes
  the 30 s bound but ends about 1.2 s late (2.77 s), which (c'') does not catch: only a tight timing bound could, which
  docs/testing.md rules out.

Stress (final script, round 3): the seven x111 tests 50 times unloaded (load average 0.7 to 1.5), then 20 times under
18 busy loops on this 18-core Mac (load average rising to 23), all passed. Stop witnesses: one `w` in every trace (0 is
now allowed, not seen here); endings `wbe` 99 of 100 and `wbbe` once unloaded, `wbe` 26 and `we` 14 of 40 under load;
INT 17 to 21 ms after the test started the control process unloaded (median 19), 20 to 31 ms under load. The P1
witness: the command ended 1.52 to 1.56 s after its start (lease 1.5 s) and the recorder got INT 1.52 to 1.55 s after
its start, loaded or not; after the pause every trace went straight to a deadline pass (`d`, then the exit events).
Idle traces had 12 to 19 passes in about 3 s.

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
  woken owner retries a busy lock, and round 3 only which clock sample chooses the sleep, not how often an idle owner
  wakes (the idle witness's pass counts are the same), so they were not repeated.
- Taking the pass's elapsed time off its sleep (round 3) is not pinned by a witness; see the mutant above.
- A control process stopped (SIGSTOP) while it holds the token lock is not covered by a witness; as argued above, it
  blocks every state read until it resumes or dies, as before x111.

## Golden diffs

None.

## Incidental bugs

None in the product. Test hygiene, not investigated further: the loopback-sshd witness
`worker_commands_and_senders_stop_over_independent_real_ssh_d96` leaves its worker root (the script and five compact
receipts) in `/tmp` after every run, on the base as well; such roots accumulate on a developer machine.

Round 3: when an x111 witness fails on its bound (as the P1 witness does on `568b697a`), dropping the child kills the
owner, but the owner's anchor runs in its own process group and is only ever killed by its owner, so it lingers
(sleeping) and keeps the test's inherited stdout open, which can hold a `cargo test | tail` pipeline until it is
killed by hand. Only failing runs leave one.
