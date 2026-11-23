# x111 findings: a quieter worker supervisor

Base `281c4005`. Worker-side implementation detail of D96. The worker commands, their protocol, outputs and
receipts, the controller and the run outcomes are unchanged except in two documented windows: at an expiry or KILL cutoff
an exit status already visible at the owner's exit check wins; otherwise the owner applies its expiry and escalation
rules (base could accept an exit up to its 25 ms poll after the expiry; see [Exit or deadline](#exit-or-deadline-which-receipt)); and
an owner that resumes after a cutoff and waits using an earlier clock reading has that wait capped at 100 ms instead
of 25 ms (a cap on the wait, not a reaction guarantee). No D-row (orchestrator decision, round 6): the lease
expiry is the controller's deadline, so an exit after it is late by definition, and base's 25 ms was its poll
granularity, not a contract. No U row: no product bug was found.

## Problem

Every supervised SSH command and every supervised recorder runs under `run_owned` in
`crates/marathon-apple/src/cmd/remote/ssh/sshj/supervisor/worker.pl`. Its monitor loop read the token state with
`locked()` (4 to 6 file system calls) on every pass and slept 25 ms on the ready pipe (while starting) and 25 ms on
the status pipe, so an idle command or recorder passed the loop 40 times a second for its whole life. In a local
controlled experiment that was most of the extra worker CPU the Rust runner puts on an SSH worker compared with the
JVM runner (about +0.6 to +0.9 CPU-s per batch of five ~22 s UI tests).

## Design

Each pass of the loop still reads the state, applies force, start, signal forwarding and the recorder's INT/TERM/KILL
rules exactly as before. What changed is how it sleeps between passes and how it takes the payload's exit:

- **One `select` per pass** over every descriptor that signals an event: the payload's status pipe (exit), the ready
  pipe while starting, a started recorder's stderr pipe (the `Recording started` marker and the bounded diagnostic)
  until it reaches end of file, and the token's wake FIFO. Exit, the start marker, stderr and control requests wake
  the owner at once instead of on the next 25 ms tick.
- **Timeout:** the time to the next thing that has to happen, counted from the pass's one clock sample: the lease
  expiry until the pass has acted on it (force), a recorder's TERM deadline (2 s after the first stop or expiry while
  TERM is not sent) and KILL deadline (1 s after TERM when forced), a retry when the token lock was busy or a confirmed
  start is not yet published, and **the longest sleep, C = 100 ms** (`LONGEST_SLEEP`). Deadlines win ties; a deadline
  that is due or past gets the 10 ms floor, never the longest sleep. The busy retry is 1 ms right after a FIFO wake
  (the waking request holds the lock only until its commit, a moment later) and 10 ms otherwise. Before the wake FIFO
  opens (the first 250 ms) the longest sleep is 25 ms, the old cadence. The selection is the pure function
  `next_wake`, so a test can drive it with a controlled clock.
- **Why a cap and no timer** (round 5): `select`'s timeout is relative, so an owner descheduled after choosing its
  sleep sleeps the rest of it after resuming. Capping every sleep at C bounds that: after any descheduling, the owner
  starts no wait longer than C from a stale clock reading (the old loop: 25 ms). A wait chosen from a stale reading is
  capped at C; this is a cap on the wait, not a reaction guarantee. Round 4 instead ended the
  sleep with a kernel interval timer (`setitimer`, `SIGALRM`); the fourth review still found stale sleeps (a stall
  between the timer's clock readings left up to 1.5 s of stale sleep) and the timer's interactions with signal
  delivery kept producing edge cases. The cap needs no signal, no timer state and no re-arming, and nothing it adds
  can interrupt or be inherited by anything else. The cost is the idle wake rate: 10 passes a second instead of the
  old 40 (round 4: about 1), measured below.
- **Wake FIFO** (`<root>/<token>.wake`, mode 0600 in the private 0700 root): the owner creates it with
  `POSIX::mkfifo` once the command has run for 250 ms, opens the read end non-blocking and keeps its own write end
  open so the reader never sees end of file. `control` writes one byte to it for any request that changes a running
  or recording token (stop, INT, TERM, KILL, renewals), under the token lock and before it commits the state (below).
  The owner closes and unlinks it when its loop ends, before the receipt. Retirement accepts a `.wake` entry, which
  only an owner killed before its cleanup can leave.
- **Why the FIFO opens late:** loading POSIX and setting up the FIFO cost a short command more than its few 25 ms
  polls. With the FIFO from the start, 100 sequential supervised `/usr/bin/true` runs cost 10.6 ms CPU and 12.1 ms wall
  each against 7.9 and 9.6 ms for the old loop; with the 250 ms delay 8.1 and 9.8 ms against 8.0 and 9.6 ms (three
  runs each, round 1). Short commands therefore poll the state at the old cadence; their ready and exit events wake
  the owner at once, so they react sooner than before, never later.

### Exit or deadline: which receipt

Base rule (`281c4005`): each pass samples the clock and checks force (lease expiry, KILL) first, then waits up to
25 ms on the status pipe; an exit seen in that wait is taken at once. So an exit is reported if it reaches the owner
before the end of the status wait of a pass whose sample was before the expiry.

New rule (round 5): each pass checks the status pipe (non-blocking) right before its one clock sample. An exit status
already visible at that check wins: the pass takes its status and ends the loop before any expiry or KILL
decision. An exit reported after the sample is the next pass's; this pass decides from its sample exactly as base did
(force first). The pass's `select` also watches the status pipe, so an exit wakes the owner at once and the next
pass, which starts with the check, takes it.

How the two rules compare:

- **Owner not descheduled (the post-expiry window; accepted, round 6):** an exit before the expiry wakes the owner
  and is taken. At the expiry the owner wakes from its deadline timeout, checks the status pipe, samples the clock
  and applies the expiry unless an exit status was already visible at that check. So **an exit reported after the lease
  expiry wins only if it is visible at the exit check of the owner's first pass after the expiry**; otherwise the
  owner applies its expiry and escalation rules: the scheduling latency between the expiry and that check, including the 10 ms sleep floor when the expiry fell inside
  it, can still admit one. Base decided force at the start of a pass and then waited up to 25 ms on the status pipe, so an exit
  reported up to one such wait after the expiry (up to about 25 ms) could still be taken. The fifth review's probe:
  base accepted an exit 21.5 ms after the expiry (`done:0`); round 5 killed at the expiry + 3.1 ms (`done:137`).
  Outcomes in that window are not unchanged; everywhere else they are. The recorder's KILL deadline is decided the
  same way. This is accepted without a D-row (orchestrator decision): the expiry is the controller's deadline and
  base's 25 ms was its poll granularity, not a promise.
- **Owner descheduled:** an exit while the owner is stopped is attributed by where it was stopped. Stopped between
  the status check and the clock sample (a window of two statements), a pass that resumes after the expiry kills
  (`done:137`) even if the payload exited before the expiry; stopped anywhere else, the exit wins (`done:<status>`)
  even if it came after the expiry. Base has the same ambiguity at the corresponding points. A descheduled owner
  cannot tell when the exit happened, so either answer is possible for an exit near the expiry; that is the
  documented bound, as for base (whose window was its 25 ms wait).
- The forced and expired recorder rule of D96 (`done:0` after INT) is unaffected: that `done:0` is the recorder's own
  exit status after INT; the supervisor writes 137 only when it decides to kill (a forced command, a forced recorder
  that has not started, a recorder still running 1 s after TERM when forced).

The reviewer's two P1-b interleavings and the undisturbed cases, run against the base and the final script (scratch
copies with a probe that stops the owner at the given point; lease 1.5 s; 5 runs each, all identical):

| scenario | base | final |
|---|---|---|
| A: owner stopped after its state read, before its clock sample; payload exits before the expiry; resumed 200 ms after it | 137 | 137 |
| B: owner stopped before its status wait / `select`; payload exits 95 ms after the expiry; resumed 200 ms after that | 0 | 0 |
| C: not descheduled; payload exits about 50 ms before the expiry | 0 | 0 |
| D: not descheduled; payload exits about 95 ms after the expiry | 137 | 137 |
| E: not descheduled; payload exits about 21.5 ms after the expiry (fifth review's probe; the accepted window) | 0 | 137 |

The fourth review's own probes of round 4 (its exit check came before the state read) reported `done:137` for an
exit before the expiry with the owner stopped just after that check, and `done:0` for an exit 95 ms after the expiry
where base gave 137; base's 137 there means the owner was stopped outside its status wait (at the point of B, base
also takes the exit, as the table shows). Round 3 (the exit taken after the kill decisions) answered `done:137` for
an exit its `select` had already seen when the owner was then descheduled across the expiry (base: `done:0`; witness
(e) below). The final rule answers each of these as base does at the same point; round 4 was not re-probed here.

### Why no wake is lost

A control request takes the token lock, writes the pending state, writes its byte to the FIFO, renames the pending
state into place (the commit) and only then releases the lock (round 2; round 1 wrote the byte after releasing the
lock). The owner reads the state only with a non-blocking `flock` on the same lock; when the lock is busy it keeps
its previous view and retries soon (1 ms for the first retry after a wake, 10 ms otherwise).

The writer holds the lock continuously from before its byte is written until after its commit, or until it dies
without one. So any state read by the owner that succeeds after the byte was written follows the commit (or the
unchanged state of a writer that died before committing, which is correct: nothing was requested). Such a read
happens promptly:

- The owner is in `select`, or reaches it, after the byte was written: the byte makes the FIFO readable, `select`
  returns at once, the owner drains it and its next pass reads the state; if the writer still holds the lock, that
  read is busy and is retried (1 ms, then every 10 ms) until the lock is free.
- The owner is between its state read and its `select` when the byte arrives: the byte stays in the FIFO and the
  `select` returns at once, as above.
- The owner drained the FIFO between the byte's write and its next read: that read is after the byte, so it is busy
  or follows the commit.
- The FIFO did not exist yet (`ENOENT`) or had no reader yet (`ENXIO`) when the writer opened it: the owner creates
  and opens it after that moment, then reads the state in the same pass, so that read is after the writer took the
  lock and is busy or follows the commit. If the owner's loop had already ended, there is nothing left to wake.
- The writer dies at any point: before the lock, nothing changed; holding the lock before the rename, the rename never
  happens (it is atomic) and the kernel releases the lock, so the owner reads the old state, which is right because
  the controller saw the request fail; after the rename, the byte is already in the FIFO.
- The writer is stopped while holding the lock: every state read is busy until it resumes or dies. The owner keeps
  enforcing the deadlines it last read. This is the same as before x111.

The byte is written whole or not at all (one byte, below `PIPE_BUF`). `EAGAIN` (the FIFO is full, so a wake is
pending) and `EPIPE` (the owner closed its end; `SIGPIPE` is ignored for the write) are success; `EINTR` on the open
or the write is retried. Any other failure leaves the change to the owner's next pass, at most C later. No pid is
involved (no reuse problem); only the token's owner reads the FIFO; the root is private to the user. The anchor and
payload never hold the FIFO: it is created after the fork, and Perl opens it close-on-exec. With the cap, a lost
wake could only cost C; the witnesses below still show that the wake, not the cap, delivers a stop.

### Deployment

`worker.pl` is not installed on workers ahead of time: each run's bootstrap writes the controller's own copy into a
fresh private root (`bootstrap.pl`), and every supervised operation of that run uses that copy. Controller and
worker script are therefore always the same version and no mixed-version interoperation exists. The Rust side is
unchanged.

### Test hooks

The shipped `worker.pl` contains no test code (round 5): at each hook point it has only an inert comment line
`# x111-test-hook: <name>` (`table`, `clock`, `commit`, `loop`, `pass`, `poll`, `sample`, `term`, `sleep`, `woken`,
`end`).
The tests install a copy in which each such line is replaced by test code from `X111_HOOKS` in
`crates/marathon-apple/tests/ssh_supervisor.rs`, indented like it; the `table` code fills a hook table from the
`MARATHON_D96_TEST_<NAME>` environment variables. So no environment, argument, controller or added definition can
turn a hook on in production, and there is nothing to override: round 4's `sub hook` reader could be replaced by a
later `sub main::hook { 1 }` (fourth review, P3); round 3's table could be written through `$hook{x}++` and similar.

`the_shipped_worker_script_has_no_test_hooks_and_the_tests_run_its_logic_x111` pins both sides:

- The shipped script: every line that names a hook (any case) is a comment; the markers are exactly the hooks of
  `X111_HOOKS`, once each and in that order; no `MARATHON_D96_TEST` variable is named. Each of ten additions (`sub
  main::hook { 1 }`, `sub hook`, a `%hook` or `%Hook` table, a `$hook{..}` write, a `hook(..)` call, a glob
  assignment, a test variable, a duplicate marker, an unknown marker) and a removed marker must be reported.
- The tests' copy: line by line it equals the shipped script except that each marker line is replaced by its hook
  code; `x111_root` checks this on the file it installs, so the witnesses run the shipped logic. Changing the longest
  sleep, adding a line, changing hook code, appending a line or editing a comment in the copy must be reported.

The hooks:

- `wakes=<file>`: the owner appends one line to the file when its loop ends (before the receipt): one letter per
  pass, by what ended that pass's sleep (`e` a pipe, `w` the wake FIFO, `d` a deadline, `b` a busy retry, `p` the
  longest sleep), and `!` where another hook moved or stopped it.
- `fallback_ms`: the longest sleep once the FIFO is open, so a witness can tell a wake from a poll.
- `kill_after_commit`: a control request kills itself right after its commit, still holding the token lock.
- `parked=<file>`: on the first pass with the FIFO open, the owner creates the file right before its `select`, after
  it has read the state.
- `expiry_in=<ms>`: on that pass, the owner's clock moves so that its lease expiry comes `<ms>` after the pass's
  sample (with a long real lease, no real time decides whether the owner reaches a hook before its expiry).
- `stop_at=sample|sleep` and `stop_file=<file>`: on that pass, right after its clock sample or right before its
  `select`, the owner writes its pid and the ms left to its expiry to the file and stops itself with `SIGSTOP`.
- `late_after_exit=<ms>`: after the first `select` that found the status pipe readable, the owner's clock moves
  `<ms>` on, as if it had been descheduled that long right after seeing the exit.
- `kill_later=<ms>` (round 6): when the owner sends a recorder TERM, its KILL deadline moves `<ms>` later (it
  moves the TERM time the KILL deadline counts from).

For the selection test, `worker.pl` returns before its command dispatch when it is loaded with `do` (`return 1 if
caller`); run as a script, as every worker command runs it, it is unchanged.

## Invariants kept

- D96/D98 semantics: lease expiry kills a command; a forced or expired recorder gets INT only after its own start
  marker, TERM 2 s after the first request and KILL 1 s after TERM, keeping those times from the first request; a
  failed recorder keeps at most 8192 bytes of stderr; receipts, retirement and the control protocol are unchanged.
- Every decision is still taken on a pass that has just read the state and sampled the clock; the FIFO and the
  descriptors only decide when that pass runs.
- Reaction times: a stop is applied 1 to 2 ms after its wake (the first busy retry; a request's whole locked section
  takes about 0.14 ms median, measured in round 3, the FIFO adding about 12 µs) instead of on the old 25 to 50 ms
  loop, once the wake FIFO is open; in a command's first 250 ms, before it opens, a stop is seen on the next 25 ms
  poll of the state (old loop: up to 50 ms). A deadline is acted on when the owner's sleep towards it ends: at most the 10 ms floor late when the owner
  runs, at most C (100 ms) after it resumes when it was descheduled (old loop: 25 ms after resuming). Stderr, start
  and exit are seen as they happen.

## Witnesses (`crates/marathon-apple/tests/ssh_supervisor.rs`)

- (a) `a_stop_request_wakes_the_worker_supervisor_through_its_fifo_x111` and (a')
  `a_stop_request_killed_right_after_its_commit_still_wakes_the_worker_supervisor_x111`: a `run` payload that writes a
  `handler` file once its INT handler is installed and exits 3 on INT; an owner with a 600 s lease and a 120 s
  longest sleep (hook); INT as the only control request, sent once the payload's `handler` file and the owner's
  `parked` file exist (round 5, fourth review P2: without the handler wait, a request could reach a payload still
  without its handler and end `done:130`). In (a') the control process kills itself right after its commit, still
  holding the lock. Nothing but the wake can make the owner read the state again within the 60 s hang guard; the run
  must end `done:3`, the trace hold exactly one `w`, and the FIFO be gone.
- (b) `an_idle_supervised_command_and_recorder_wake_rarely_x111`: an idle `run` command and an idle started recorder
  over a 3 s window; each trace must have at most `15 × elapsed seconds + 20` passes (the old 40-a-second loop gives
  about twice that).
- (c) `the_worker_supervisor_sleeps_until_its_next_deadline_x111`: a controlled clock. It loads `worker.pl` with `do`,
  checks `LONGEST_SLEEP` is 100, and calls `next_wake` with given times: deadlines before and after the longest sleep,
  ties (the deadline wins), an expired lease the pass has acted on, a lease due exactly now and one 50 ms past (the
  floor, never the longest sleep), TERM due and KILL 50 ms past (the floor), the 10 ms floor, a deadline beyond the
  longest sleep, the busy retries, a 120 s longest sleep, TERM 2 s after the first stop request (none for INT alone),
  KILL 1 s after TERM when forced (none without force), and the nearest of several deadlines.
- (d) `an_owner_descheduled_after_its_clock_sample_acts_on_its_deadline_within_its_longest_sleep_x111` and (d')
  `an_owner_descheduled_before_its_select_acts_on_its_deadline_within_its_longest_sleep_x111` (round 5, replacing
  round 4's timer witnesses): a `run` command with a 600 s lease and `expiry_in=5000`; on its first pass with the FIFO
  open it stops itself right after its clock sample (`sample`: it then decides and chooses its sleep from a stale
  sample) or right before `select` (`sleep`: its sleep is already chosen). The test waits until `ps` shows it stopped
  and for the 5 s to pass plus 300 ms, then sends `SIGCONT`. The command must end 137 with `done:137` within 2.5 s of
  the `SIGCONT`; an uncapped sleep from that sample takes the full 5 s. The 100 ms itself is pinned by (c), not by a
  real-time bound (docs/testing.md rule 6).
- (e) `an_exit_seen_before_a_missed_deadline_keeps_its_own_status_x111` (round 4): with `late_after_exit=700000`
  (past the 600 s lease) and a 120 s longest sleep, (1) a `run` payload that exits 0 once the test opens its gate
  after the owner parked must end `done:0`; (2) an unconfirmed recorder given KILL, whose payload closed its stderr at
  once and exits 0 on TERM, must end `done:0`, not `done:137` for its KILL deadline. Each trace shows the hook once.
  Round 6 (fifth review, P2): the recorder half assumed the payload's exit after TERM is reported within the real
  1 s before KILL; stopping only the payload through that second gave `done:137`. With `kill_later=300000` its KILL
  deadline is 301 s after TERM, beyond the 60 s hang guard, so only the payload's own exit can end it before the
  `late_after_exit` jump carries the clock past that deadline; the order is decided by events, not by real time.
- `the_shipped_worker_script_has_no_test_hooks_and_the_tests_run_its_logic_x111` (round 5): see Test hooks.
- (c') `escalation_and_lease_expiry_end_with_their_receipts_x111`: the receipts only: an unconfirmed recorder that
  ignores TERM, given KILL, ends `done:137`; a `run` command with an unrenewed 1.5 s lease exits 137 with `done:137`.
- The timings themselves stay pinned by the existing tests, unchanged:
  `an_unconfirmed_recorder_gets_term_and_kill_at_the_existing_cutoffs_d98` (TERM at ≥ 2 s, KILL at ≥ 3 s),
  `lease_expiry_gives_an_unconfirmed_recorder_term_at_the_existing_time_d98` (expiry + 2 s),
  `worker_expiry_compacts_its_receipt_and_retirement_waits_for_owned_groups_d96` (expiry, TERM, KILL; the root holds
  only receipts and the script afterwards, so the FIFO is removed),
  `a_detached_recorder_gets_int_only_after_its_own_start_marker_d98`,
  `a_start_marker_split_across_writes_counts_only_when_its_line_is_complete_d98`,
  `forced_and_expired_recorders_finalize_and_failed_recorders_keep_bounded_stderr_d96`, and the loopback-sshd
  `worker_commands_and_senders_stop_over_independent_real_ssh_d96`. Only two comments in existing tests changed (they
  described the 25 to 50 ms read loop).

Round 5 dropped round 3's `a_pass_descheduled_across_the_lease_expiry_still_acts_on_it_x111`: it moved the owner's
clock between the pass's checks and a later clock reading, and the loop now has one clock reading per pass, so the
hook had nothing left to move; (c) and (d) cover a deadline found due or passed after the sample.

### Fails without the change (round 5, scratch worktree of the final tests)

- Round 3's script `b38c59e4` with the round-5 markers at the same places (its diff to the final script is exactly the
  longest sleep, the exit check and its elapsed-time correction): (e) fails (`done:137`). (d') passes there: its
  stale sleep is bounded by its own 1 s fallback poll (ended 1.007 s after `SIGCONT`); (d) passes because round 3 took
  the elapsed time off the sleep.
- Mutants of the final script: no cap (`LONGEST_SLEEP` 100 000 000): (d) and (d') fail (ended 5.01 s after
  `SIGCONT`) and (c) fails; `LONGEST_SLEEP` 25 (the old rate): (b) fails (98 passes in 3.09 s); without the `wake`
  call: (a) and (a') fail (no INT within the 60 s guard); the exit check moved after the kill decision (round 3's
  order): (e) fails (`done:137`).
- Round 4's script `cab478e1` fails the hook check (`my %hook;` is code that names a hook).
- Earlier rounds (round 1 to 4, recorded in their commits): the base script fails (a), (b) and (c) (no FIFO, no
  `next_wake`); round 1's `9c972f86` fails (a'); dropping a deadline or the floor from `next_wake` fails (c).

### Round 6 (scratch worktree of the final tests)

- The fifth review's race, reproduced: with a payload that sleeps 2 s in its TERM handler before exiting 0, round 5's
  test fails (`done:137`: KILL 1 s after TERM came first); the final test passes (`done:0`).
- The exit check moved after the kill decisions (round 3's order) fails both halves of (e): the command `done:137`
  and, with the command half's assertions turned into prints, the recorder `done:137` 2.4 s after the start of the
  test, that is right after TERM and the clock jump, not at its 301 s KILL deadline.
- The guard test passes with the new `term` marker and its hook in `X111_HOOKS`.
- (e) 50 times unloaded (load average about 1.4 to 1.6) and 20 times under 18 busy loops (load average rising to
  13.6): all passed.

### Stress (round 5 script)

All nine x111 tests 50 times unloaded (load average about 1.1 to 1.7), then 20 times under 18 busy loops on this
18-core Mac (load average rising to 23): all passed.

- (d), (d'): the command ended 103 to 148 ms after `SIGCONT` unloaded (median 111 ms), 102 to 127 ms under load
  (median 118 ms): C plus the owner's cleanup and the test's wait for its exit.
- (a), (a'): INT reached the payload 17 to 21 ms after the test started the control process unloaded (median 19 ms;
  this includes the control process's Perl start and up to one 10 ms poll of the test), 20 to 31 ms under load. Every
  trace had exactly one `w`; endings `wbe` 100 of 100 unloaded, `wbe` 31 and `we` 9 of 40 under load.
- (b): 35 to 43 passes in about 3.1 s per trace (ceiling 65).

## Measurements

Idle CPU, round 5: this Mac (M-series, 18 cores, shared with other sessions), the base script against the final one
(C = 100 ms) and the same with C = 250 ms, run side by side, each as one supervised `/bin/sleep` command and one
idle started recorder, both run directly (not over sshd); CPU time of each owner from `ps` after 60 s idle. Three runs,
load average 1.1 to 1.6; 10 ms resolution. CPU-seconds per supervised process per idle minute:

| script | command | recorder |
|---|---|---|
| base `281c4005` (40 passes/s) | 0.38 (all runs) | 0.42 to 0.43 |
| final, C = 100 ms (10 passes/s) | 0.15 to 0.16 | 0.14 to 0.15 |
| C = 250 ms (4 passes/s) | 0.08 | 0.07 |

The cap at 100 ms cuts the idle cost to about 40 % of the base (about 0.23 to 0.28 CPU-s less per supervised
process per minute; for a batch of about 110 s, about 0.45 CPU-s less for the xcodebuild command's supervisor and the
same again for each recorder supervisor beside it). It is less than the 4× fall in passes because a pass now also
checks the exit and builds a larger descriptor set. C = 250 ms would halve the remaining cost at the price of
2.5 times the cap on a wait chosen from a stale clock reading after a descheduling; C = 100 ms is kept because its
saving is not negligible and it keeps that cap near base's 25 ms.

The trade-off against round 4: its kernel timer allowed a 1 s fallback, which cost about 0.02 to 0.03 CPU-s per idle
minute (round 1 measured 0.01 to 0.02 directly and 0.02 over a loopback sshd with the 1 s fallback, against 0.21 to
0.44 for the base), so round 5 gives up most of that saving (0.15 instead of about 0.02) for a design with no
signals and a lateness bound that holds by construction.

## Review history

- Round 1: the design above with a 1 s fallback poll; the wake byte written after the commit.
- Round 2 (review P1): a control request killed between its commit and its wake left the stop to the 1 s fallback
  (941 ms late): the byte is now written under the lock before the commit. Stop and deadline witnesses made
  discriminating.
- Round 3 (review P1): a second clock sample for the timeout choice could drop a deadline found ahead and sleep the
  fallback (1 015 ms late): one clock sample per pass, and a due deadline gets the floor. Test hooks moved out of the
  shipped behaviour into a table the tests fill.
- Round 4 (review P1-a, P1-b): an owner descheduled before its `select` slept a stale relative timeout (586 ms late);
  an exit seen by the `select` lost to an expiry noticed after a descheduling (`done:137` for an exit 542 ms before
  it). Round 4 added a kernel interval timer and took the exit first in each pass.
- Round 5 (this design; fourth review): the timer still left stale sleeps in corner cases (a stall in the middle of
  arming it; an inherited blocked `SIGALRM`), the exit-first rule differed from base at the descheduled edges (A and
  B above), the stop witness did not wait for the payload's INT handler, and the hook reader could be overridden. The
  timer is gone in favour of the 100 ms cap; the exit check sits right before the clock sample; the stop witness waits
  for the handler; the shipped script has no hook code at all.
- Round 6 (fifth review): the post-expiry window (an exit reported after the expiry is now always treated as
  expired; base could accept one up to its 25 ms poll later) is accepted and documented, with no code change; the
  recorder half of (e) is ordered by events (`kill_later` hook); the findings and the CHANGELOG no longer claim
  unchanged outcomes for the two documented windows.

## Not verified

- A farm A/B (video off, alternating runs) is the decider for whether this closes the batch-start gap on busy hosts;
  it is to be run later.
- No real xcodebuild batch was measured with the new script (the idle measurements isolate the supervisor). Round 5's
  idle cost was measured directly only, not over a loopback sshd or under load; round 1 found the direct and sshd
  numbers close.
- Linux workers were not exercised here; the code uses only POSIX FIFOs, `select` and `mkfifo`, and the remote gate
  runs the witnesses there.
- The post-expiry window (an exit reported after the expiry is treated as expired) is documented and was probed by
  the fifth review (row E), not pinned by a witness (one would need real-time ordering against the owner's own
  clock).
- A control process stopped (SIGSTOP) while it holds the token lock is not covered by a witness; it blocks every state
  read until it resumes or dies, as before x111.

## Golden diffs

None.

## Incidental bugs

None in the product. Test hygiene, not investigated further:

- The loopback-sshd witness `worker_commands_and_senders_stop_over_independent_real_ssh_d96` leaves its worker root
  (the script and five compact receipts) in `/tmp` after every run, on the base as well; such roots accumulate on a
  developer machine.
- When an x111 witness fails on its bound, dropping the child kills the owner, but the owner's anchor runs in its own
  process group and is only ever killed by its owner, so it lingers (sleeping) and keeps the test's inherited stdout
  open, which holds a `cargo test | grep` pipeline until it is killed by hand. Only failing runs leave one (seen again
  in round 5's mutant runs).
