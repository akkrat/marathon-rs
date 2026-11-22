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
  none reaches the selection. The sleep ends at that sample plus the chosen delay, an absolute time (next bullet). A
  zero sleep cannot spin: the next pass acts on the due deadline or recomputes from its own sample.
- **A kernel timer ends the sleep** (round 4): `select`'s timeout is relative, so an owner descheduled after it chose
  its sleep but before it entered `select` still slept all of it after resuming (round 3 took the time spent so far
  off the sleep, which only moved that window; the review saw a gated owner act 586 ms past the expiry, where the old
  loop is bounded by 25 ms after resuming). Before each sleep, `arm` sets the process's `ITIMER_REAL` (Time::HiRes
  `setitimer`, core Perl) to fire at the chosen absolute time; the timer runs on while the owner is descheduled or
  stopped. Its `SIGALRM` handler only sets `$alarmed`; the `select` (timeout: the time left, or 0 when `$alarmed`)
  returns with `EINTR` when the signal arrives during it, and an owner resuming past the time finds `$alarmed` set
  and does not sleep. `disarm` cancels the timer right after `select`, and once more after the loop whatever ended
  it. Details:
  - The timer is set from a clock reading taken just before `setitimer`; a reading just after bounds how late that
    can make it, and the timer is set again (up to five times) if more than 1 ms passed between them, so it fires at
    most 1 ms after the target. A target already due is not armed: the sleep is 0.
  - Perl runs a signal handler only between operations, so a `SIGALRM` that lands after the `$alarmed` check but
    before the `select` system call would be left for the next wake. The timer therefore repeats every 10 ms after
    it first fires until it is disarmed: that case costs at most 10 ms. A deadline is acted on at most 10 ms after
    the owner resumes (usually at once), against 25 ms for the old loop.
  - Only this timer sends `SIGALRM`, and only between `arm` and `disarm`, which enclose nothing but building the
    descriptor set and the `select` (and test hooks). The state reads and writes, `flock`, the pipe reads and writes,
    the FIFO, the fork and the receipt all run with the timer disarmed, so none of them can see `EINTR` from it (the
    owner has no other handled signal: `CHLD` is default and `PIPE`, `HUP`, `INT`, `TERM` are ignored, as before).
    After `setitimer(0)` returns, a timer that had already fired has delivered its signal, so none arrives later.
  - The handler is installed after the anchor's fork and restored when `run_owned` returns; the anchor and payload
    never have it, and a forked child starts with no interval timer, so the payload never inherits an armed timer or
    a changed `SIGALRM` disposition.
  - If `setitimer` fails, `arm` returns the relative time left and the owner sleeps on `select`'s timeout alone, as
    in round 3.
  - Cost: two `setitimer` calls and up to four clock readings per pass; an idle owner passes about once a second.
- **An exit already reported wins** (round 4): each pass first checks the status pipe (non-blocking) and, if the
  anchor has reported the payload's exit, takes that status and ends the loop, before it reads the state or the
  clock. This is the only status read (round 3 read it at the end of a pass, after the expiry and KILL decisions), so
  an exit is taken either by the next pass's first step or, when it happens during a pass, by the pass after its
  `select` returned. Before, a `select` that saw the exit was followed by a pass that checked the expiry first, so an
  owner descheduled across the expiry in between reported `done:137` for a payload that had exited 0 (the review: its
  status was queued 542 ms before the expiry). The same applies to a recorder: a forced recorder that exited on its
  own (for example on TERM) before its KILL deadline keeps its own status instead of `done:137`. How this meets D96's
  rule that forced and expired recorders finalize `done:0`: that `done:0` is the recorder's own exit status after
  INT, never a value the supervisor writes; the supervisor writes 137 only when it decides to kill (a forced command,
  a forced recorder that has not started, a recorder still running 1 s after TERM when forced). The new rule only
  stops that decision from replacing a status the anchor reported before the pass that made it, so every forced or
  expired recorder that finalizes on INT still ends `done:0`, and one that the supervisor kills still ends
  `done:137`. An exit reported after a pass's first step and before its kill decision in that pass is still reported
  137, as before x111: it happened at the deadline itself.
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

The hooks live in one table, `my %hook;`, declared empty near the top of `worker.pl`, named in one other place, its
reader `sub hook { return $hook{$_[0]} }`, which returns a copy (round 4; round 3 read `$hook{...}` at each site). So
in the shipped script every hook branch is dead and no environment, argument or controller can reach one. The tests
install a copy of the script in which that one line is replaced by `my %hook = map { ... } keys %ENV`, filling hook
`<name>` from `MARATHON_D96_TEST_<NAME>` (`x111_root`). A test pins the shipped script's side
(`x111_hook_violation`): the declaration and the reader occur once each; apart from calls of the reader, `hook`
with a sigil (`$`, `@`, `%`, `&`, `*`, `\`, also across blanks, `{`, `^` and package qualifiers) occurs only in
those two places, so no assignment, increment, `delete`, `undef`, slice, `local`, alias (`for`), reference or glob
can reach the table; there is no other `sub hook`; there is no string `eval`; and no `MARATHON_D96_TEST` variable is
named. Round 3's check looked only at the text after each `$hook{...}` and missed `$hook{name}++` and slices. The
test also adds each of 22 such writes to the script (`$hook{kill_after_commit}++`, `++$hook{x}`, `.=`, `||=`, `//=`,
`=~ s///`, `delete`, a hash slice, a list assignment, `undef %hook`, `local`, a `for` alias, `\%hook`, `$ hook{x}`,
`${hook}{x}`, `%{hook}`, a glob assignment, a second `sub hook`, two string evals, a test variable) and to a doubled
declaration, and requires the check to report each. Round 2 read the variables from the
environment directly; SSH does not forward them, but a worker-side environment export could still have turned on
the self-kill hook for production control requests. A test-only argument was the other option; it would leave the
hook code reachable by anyone who passes the argument, and the copy keeps the shipped script's behaviour independent
of anything a caller supplies.

- `wakes=<file>`: an owner appends one line to that file when its loop ends (before the receipt): one letter per
  pass, by what woke it (`e` a pipe, `w` the wake FIFO, `d` a deadline, `b` a busy retry, `p` the fallback poll).
- `fallback_ms`: the owner's fallback poll (default 1000 ms), so a witness can tell a wake from a poll.
- `kill_after_commit`: a control request kills itself right after its commit, still holding the token lock.
- `pause_across_expiry` (round 3; round 4 moves the clock instead of sleeping): on the first pass with the wake
  FIFO open whose checks find the lease expiry ahead, the owner's clock moves to 5 ms past the expiry before it
  chooses its timeout (round 2's second clock sample fell there), as if it had been descheduled that long, and the
  trace gets a `!`.

Round 4 adds a clock offset `$skew` (0 in the shipped script; only these hooks move it) and:

- `parked=<file>`: on its first pass with the wake FIFO open, after reading the state and arming its timer, the owner
  creates the file right before its `select`, so a witness can send a request strictly after that state read.
- `expiry_in=<ms>`: on the first pass with the FIFO open, the owner's clock moves so that its lease expiry comes
  `<ms>` after that pass's sample, as if the lease had been that short. With a long real lease, no real time decides
  whether the owner reaches a hook before its expiry.
- `stop_at=arm|select` and `stop_file=<file>`: on the first pass with the FIFO open, after choosing its sleep, the
  owner writes its pid and the ms left to its lease expiry to the file and stops itself with `SIGSTOP`, either just
  before arming its timer (`arm`) or just before `select` (`select`), until the test sends `SIGCONT`.
- `late_after_exit=<ms>`: after the first `select` that found the status pipe readable, the owner's clock moves `<ms>`
  on, as if it had been descheduled that long right after seeing the exit; the trace gets a `!`.

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
  as soon as the owner is scheduled after them (the kernel timer: at once on resuming, at most 10 ms later in the
  one case its handler misses the sleep); a deadline less than 10 ms ahead at a pass is slept on for the 10 ms
  floor, so it can fire up to 10 ms late (the old loop could be up to 50 ms late). Stderr and exit are seen as they
  happen instead of on the next tick.

## Witnesses (`crates/marathon-apple/tests/ssh_supervisor.rs`)

- (a) `a_stop_request_wakes_the_worker_supervisor_through_its_fifo_x111` and (a') the P1 witness
  `a_stop_request_killed_right_after_its_commit_still_wakes_the_worker_supervisor_x111` (round 2): a `run` payload
  that exits 3 on INT, an owner with a 600 s lease and a 120 s fallback poll, and INT as the only control request,
  sent once the owner has created its `parked` file: it has read the state on its first pass with the FIFO open and
  is about to sleep (round 4). In (a') the control process kills itself right after its commit, while it still holds
  the token lock. Nothing but the request's wake can make the owner read the state again within the 60 s hang guard,
  so INT reaching the payload at all shows the wake; the run must end `done:3`, the trace must hold exactly one `w`
  and the FIFO must be gone. History: round 1 waited only for the FIFO path, and a later force's own wake could
  satisfy it; round 2 waited until the owner had the FIFO open for reading, but a request committed before the
  owner's first state read after that was seen without a wake (the review reproduced it with the wake disabled), so
  round 3 had to accept zero wakes and could pass without one. The `parked` gate orders the request after that read.
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
- (c'') `a_pass_descheduled_across_the_lease_expiry_still_acts_on_it_x111` (round 3's P1 witness): the round-3
  interleaving made deterministic with `pause_across_expiry` and a 120 s fallback poll: a `run` command must exit 137
  with `done:137`, and a confirmed recorder must get INT and finalize `done:0`, each within 30 s (far below the
  fallback); each trace must show the hook once and no fallback pass after it. Round 4 (review P2): the lease is 60 s
  (still nearer than the fallback poll, so the paused pass chooses the expiry) and the hook moves the owner's clock
  past the expiry instead of sleeping until a real 1.5 s expiry, so an owner descheduled through a short lease
  before reaching the hook can no longer fail it.
- (d) `an_owner_descheduled_before_arming_its_timer_still_wakes_at_its_deadline_x111` and (d')
  `an_owner_descheduled_before_its_select_still_wakes_at_its_deadline_x111` (round 4, P1-a): a `run` command with a
  600 s lease, a 120 s fallback poll and `expiry_in=5000`, so its first pass with the FIFO open chooses to sleep 5 s
  until its expiry; it then stops itself (`stop_at=arm`, or `stop_at=select` after its timer is armed). The test
  waits until `ps` shows it stopped and for the 5 s to pass plus 300 ms, then sends `SIGCONT`. The command must end
  137 with `done:137` within 2.5 s of the `SIGCONT`; a stale relative sleep takes the full 5 s.
- (e) `an_exit_seen_before_a_missed_deadline_keeps_its_own_status_x111` (round 4, P1-b): with
  `late_after_exit=700000` (past the 600 s lease) and a 120 s fallback poll, (1) a `run` payload that exits 0 once the
  test opens its gate, which it does after the owner's `parked` file appears (so the exit can only be seen by that
  `select`), must end `done:0`; (2) an unconfirmed recorder given KILL, whose payload closed its stderr at once and
  exits 0 on TERM, must end `done:0`, not `done:137` for its KILL deadline 1 s after TERM (the owner sends TERM in
  the pass before its `select`, so that `select` sees the exit). Each trace must show the hook exactly once.
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

Round 4, in a scratch worktree with the round-4 tests (removed afterwards):

- Round 3's head `b38c59e4`, with the round-4 hooks added at the same places (the reader `hook`, the clock offset,
  `expiry_in` at the pass's sample, both `stop_at` points just before its `select`, which there follows the chosen
  sleep directly, `parked`, `late_after_exit`, and `pause_across_expiry` as a clock move): (d) and (d') fail (the
  command ended 5.01 s after `SIGCONT`: the stale sleep), and (e) fails in both halves (`done:137` for the command,
  and, run alone, `done:137` for the recorder). The stop witnesses, (c''), the idle witness and the hook check pass
  there.
- Mutants of the final script: without the wake, (a) and (a') fail (no INT within the 60 s hang guard: the `parked`
  gate makes the wake the only way); with the status read moved back after the kill decisions, (e) fails; without
  the timer (`setitimer` treated as failed), (d') fails; with the timer but neither the 10 ms repeat nor the
  `$alarmed` check, (d') fails; with either one alone, everything passes (see Not verified).

Stress (final script, round 4): the ten x111 tests 50 times unloaded (load average 1.5 to 2.0), then 20 times under
18 busy loops on this 18-core Mac (load average rising to 21), all passed. (d) and (d'): the command ended 1.5 to
38 ms after `SIGCONT` (median about 4.5 ms; under load 1.5 to 43 ms, median about 10 ms), against the stale 5 s;
this includes the owner's cleanup and the test's wait for its exit. Stop witnesses: exactly one `w` in every trace,
endings `wbe` 89 and `wbbe` 11 of 100 unloaded, `wbe` 23, `wbbe` 5 and `we` 12 of 40 under load; INT 6 to 31 ms after
the test started the control process unloaded (median 19), 9 to 42 ms under load. Idle traces had 12 to 19 passes in
about 3 s.

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

Round 4 (the kernel timer adds two `setitimer` calls per pass): the same direct 60 s idle measurement, twice, the
base, round 3's and round 4's script side by side (load average 1.0 to 1.2), CPU-seconds per supervised process:

| script | command | recorder |
|---|---|---|
| base `281c4005` | 0.39 (both runs) | 0.44 (both runs) |
| round 3 `b38c59e4` | 0.02 | 0.02 |
| round 4 | 0.03 | 0.03 |

The difference between rounds 3 and 4 is one 10 ms tick of the measurement, so the timer keeps the saving (about 13
to 15 times less than the base here).

## Not verified

- A farm A/B (video off, alternating runs) is the decider for whether this closes the batch-start gap on busy hosts;
  it is to be run later.
- No real xcodebuild batch was measured with the new script (the idle measurements above isolate the supervisor;
  xcodebuild's own CPU is unaffected by it).
- Linux workers were not exercised here; the code uses only POSIX FIFOs, `select` and `mkfifo`, which behave the same
  on Linux, and the remote gate runs the witnesses there.
- The table above is round 1's script. Round 2 changes only when a request writes its byte and how soon a woken
  owner retries a busy lock, and round 3 only which clock sample chooses the sleep, not how often an idle owner
  wakes. Round 4 adds two `setitimer` calls per pass, so its idle cost was measured again (direct, 60 s; see
  Measurements), but not over sshd or under load.
- The two backstops of the timer, the `$alarmed` check before `select` and the timer's 10 ms repeat, cover each
  other: (d') fails only when both are removed. The window the repeat alone covers (a `SIGALRM` after the check and
  before the `select` system call) is too narrow to hit from a test, so each is argued, not pinned, on its own.
- The timer's re-arm when more than 1 ms passes between the readings around `setitimer` is not pinned by a witness:
  a hook there would have to stop the owner inside `arm`, between two statements.
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
