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
  stop or expiry while TERM is not sent) and KILL deadline (1 s after TERM when forced), a 10 ms retry when the token
  lock was busy or a confirmed start is not yet published, and the fallback poll; never below 10 ms. Deadlines win
  ties with the fallback poll.
- **Fallback poll:** 25 ms (the old cadence) until the wake FIFO is open, 1 s after.
- **Wake FIFO** (`<root>/<token>.wake`, mode 0600 in the private 0700 root): the owner creates it with
  `POSIX::mkfifo` once the command has run for 250 ms, opens the read end non-blocking and keeps its own write end
  open so the reader never sees end of file. `control` writes one byte to it after any request that is not `status`
  and that found the token running or recording (stop, INT, TERM, KILL, renewals). The owner closes and unlinks it
  when its loop ends, before the receipt. Retirement accepts a `.wake` entry, which only an owner killed before its
  cleanup can leave.
- **Why the FIFO opens late:** loading POSIX and setting up the FIFO cost a short command more than its few 25 ms
  polls. With the FIFO from the start, 100 sequential supervised `/usr/bin/true` runs cost 10.6 ms CPU and 12.1 ms wall
  each against 7.9 and 9.6 ms for the old loop; with the 250 ms delay 8.1 and 9.8 ms against 8.0 and 9.6 ms (three
  runs each, identical within each variant). Short commands therefore run exactly the old cadence.

### Why no wake is lost

The owner drains the FIFO right after the `select` that reported it readable and reads the state at the top of the
next pass. A control request commits the state (rename under the token lock, lock released) before it writes its
byte. If the byte arrives before the owner's drain, the commit preceded the drain and so precedes the following state
read, which sees it. If it arrives after the drain, it stays in the FIFO and the next `select` returns at once. The
FIFO is opened at the top of a pass, before that pass reads the state, so a request committed while no FIFO existed
(its write fails with `ENOENT`, or with `ENXIO` between `mkfifo` and the owner's open) is seen by that read. No pid is
involved (no reuse problem); only the token's owner reads the FIFO; the root is private to the user, so on a shared
host no other user can open it.

Failure handling: the writer opens with `O_NONBLOCK|O_NOFOLLOW`, writes only to a FIFO (`-p`), ignores `EAGAIN` (a
full FIFO already holds a wake), `ENOENT`/`ENXIO` (no owner), and has `SIGPIPE` ignored for the instant between its
open and the owner closing its end. If the owner cannot create or open the FIFO, it keeps polling every 25 ms, as
before. The anchor and payload never hold the FIFO: it is created after the fork, and Perl opens it close-on-exec.

### Deployment

`worker.pl` is not installed on workers ahead of time: each run's bootstrap writes the controller's own copy into a
fresh private root (`bootstrap.pl`), and every supervised operation of that run uses that copy. Controller and
worker script are therefore always the same version and no mixed-version interoperation exists. The Rust side is
unchanged.

### Test hook

With `MARATHON_D96_TEST_WAKES=<file>` in its environment, an owner appends one line to that file when its loop ends
(before the receipt): one letter per pass, by what woke it (`e` a pipe, `w` the wake FIFO, `d` a deadline, `b` a busy
retry, `p` the fallback poll). Nothing else reads it. SSH does not forward the variable, so a controller cannot turn
it on for a worker.

## Invariants kept

- D96/D98 semantics: lease expiry kills a command; a forced or expired recorder gets INT only after its own start
  marker, TERM 2 s after the first request and KILL 1 s after TERM, keeping those times from the first request; a
  failed recorder keeps at most 8192 bytes of stderr; receipts, retirement and the control protocol are unchanged.
- Every decision is still taken on a pass that has just read the state; the FIFO only decides when that pass runs.
- A stop is noticed at once (the FIFO), not later than before; deadlines fire within the 10 ms floor of their time
  (the old loop could be up to 50 ms late); stderr and exit are seen as they happen instead of on the next tick.

## Witnesses (`crates/marathon-apple/tests/ssh_supervisor.rs`)

- (a) `a_stop_request_wakes_the_worker_supervisor_through_its_fifo_x111`: a `run` payload that survives INT; the test
  waits until the FIFO exists, sends INT, waits for the payload's INT marker, then forces; the owner's trace must
  contain a `w` pass, and the FIFO is gone afterwards. Ordering by events; the only time bounds are hang guards. INT
  arrived 6 to 8 ms after the request was sent (including the control process's Perl start).
- (b) `an_idle_supervised_command_and_recorder_wake_rarely_x111`: an idle `run` command and an idle started recorder
  over a 3 s window; each trace must have at most `elapsed seconds + 20` passes. Seen: 11 to 13 passes (command) and 15 to 17
  (recorder) in about 3 s; the old cadence gives about 95 per trace in the same window.
- (c) `escalation_and_lease_expiry_wake_the_worker_supervisor_on_their_deadlines_x111`: an unconfirmed recorder that
  ignores TERM, given KILL, must end on a `d` pass with at least two `d` passes (TERM and KILL); a `run` command with
  an unrenewed 1.5 s lease must end on a `d` pass.
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

Fails without the change, checked in a scratch worktree with the new tests: the base `worker.pl` fails all three
(no trace); removing the `wake()` call fails (a); a 25 ms poll throughout fails (b); dropping the expiry deadline or
the TERM/KILL deadlines fails (c).

Stress (final script): 20 runs of the three witnesses under 18 busy loops on this 18-core Mac (load average 16 to 20),
then 50 unloaded runs (load average falling from 20 to 1.9), all passed. Idle traces had 11 to 17 passes in about
3 s (ceiling 23); the INT in (a) followed the request by 5.8 to 7.8 ms. The first commit's script also passed 50 + 20.

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

## Golden diffs

None.

## Incidental bugs

None in the product. Test hygiene, not investigated further: the loopback-sshd witness
`worker_commands_and_senders_stop_over_independent_real_ssh_d96` leaves its worker root (the script and five compact
receipts) in `/tmp` after every run, on the base as well; such roots accumulate on a developer machine.
