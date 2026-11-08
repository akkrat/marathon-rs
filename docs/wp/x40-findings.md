# x40 — documentation pass after the 2026-11-08 merges

This pass follows the merges of x26 (D43, U163), x27 (D44), x29 (D46), x31 (D48, D49), x35 (D52, U161), x37, x38 and x39.
It changed documentation, added one scheduler-level regression test and fixed one flaky test. No product code changed.

## Deviation numbering

Every number from D1 to D52 has a section in `docs/deviations.md`, a row in PLAN §3.11 and a dated decision line in PLAN §11,
in numeric order. None is missing, so the range is D1–D52 everywhere: CLAUDE.md, the deviations guide, the ledger's
introduction, PLAN's D5 row and the CHANGELOG's introduction and evidence.

PLAN §11 had no dated line for D14–D20, D46 or D50. Each line added takes its date and source from the deviation's own §3.11
row: D14 and D15 2026-10-25 (WP 6.5, 6.6a), D16 2026-10-26 (6.6b), D17 2026-10-31 (5.3), D18 2026-11-02 (x1, x2), D19
2026-11-03 (x9), D20 2026-11-04 (x11), D46 2026-11-07 (x29), D50 2026-11-07 (x32). "D6 rewritten" moved up next to D6.

## Ledger rows

| Row | Change |
|---|---|
| A7 | Kept, with a survey: in three large iOS repositories of one organisation, 257 Objective-C test files, none under a UI test path, and every marathon CI job seen there was a UI job. Paths and a sample, not proof |
| N33 | Checked on real API 33 and 36 emulators in WP x31: fine there, since toybox's `ps` prints `grep`'s name; lower APIs not checked |
| A102 | Already there for x28's nested Swift classes that `nm` drops; x28's findings now point to it |
| U169 | Already there for `failed (1.2.3 seconds)`; x28's findings now point to it |
| U171 | New, open: macOS `borrow` picks from an unfiltered snapshot, A80's shape (x27) |
| U172 | New, open: with `shareWorkerConnection: true`, abandoned sessions fill the shared connection's slots (x27) |
| U173 | New, open: `sshpass` on macOS sometimes leaves ssh with no `/dev/tty` (x26) |
| U174 | New, fixed: InfluxDB v1's close was unbounded (S-x34-1, fixed by x39) |
| U175 | New, fixed: the uncompleted count could wrap at the largest accepted quota (S-x30-1, fixed by x39) |
| U176 | New, fixed: the flaky pipe drain test below |

U171 to U176 were the next free numbers; no existing row covered them. The findings of x26, x27 and x39 that said a row was
missing now name it.

## The flaky pipe drain test

`cmd::local::kotlin_process_command_executor::tests::exited_child_pipes_are_drained_before_the_reactor_delivers_readiness`
failed once in 28 Linux gate runs, with its second `poll_read` not Ready.

The test checks two things. The first, that `PollingPipe` reads bytes already in the pipe without a runtime turn, is sound:
the read goes straight to the kernel. The second, that the next read returns EOF at once, is not. EOF needs every write end
closed. The child has exited and the parent closed its copies after spawning, but another test in the same binary may fork
at that moment, and its child holds a copy of every open descriptor until it execs, since `CLOEXEC` only acts at exec. For
that window the read returns `EAGAIN`, which `PollingPipe` reports as Pending. The product is not affected: its reader polls
on a timer, so a Pending there only means a later read.

The fix keeps the first check and retries the EOF read until it is Ready, sleeping 1 ms between tries, with a 60 s
hang-catcher (`docs/testing.md` rules 1 and 2). The race is reasoned from how fork and exec treat descriptors and was not
reproduced here.

## S-x28-1: silent batches on a pool's only simulator

**The claim.** The x28 findings, the CHANGELOG, D39 in the deviations guide and the migration guide said that a simulator on
which every batch goes silent is recovered three times and runs 12 batches. x28's scenario ran without a scheduler. Through
the real scheduler on one simulator, the third silent batch returns `DeviceLost`, which empties the pool, and before D48 the
pool ended there, after 3 batches.

**What happens now.** D48 keeps a pool that lost its last device for up to `deviceInitializationTimeoutMillis` (180 s by
default). A simulator has 3 recoveries: the first starts at once, the second after 30 s, the third after 60 s. A recovery
that is set up and ready within the wait finds the pool and its tests still there, and gets 3 more silent batches. With the
defaults, every recovery is back in time if setting the simulator up and making it ready takes less than about 2 minutes,
so the simulator runs 12 batches on its 4 setups. After the fourth loss it stays out, and the pool ends when the wait is
over, with its tests unrun. A recovery that misses the wait ends the pool after 3, 6 or 9 batches. While another device
keeps the pool alive, the pool never empties and it is 12 batches, as x28 said.

**The test.** `crates/marathon-ios/tests/simulator_pool_recovery.rs` runs the real scheduler, pool, simulator device, batch
failure policy and provider recovery on a fake worker, with one simulator whose every batch goes silent
(`TimeoutException` after a test has started) and `uncompletedTestRetryQuota: 100`, so the quota doesn't end the run first.

| Test | Pauses, wait | Batches | Devices that connected | Run passed |
|---|---|---|---|---|
| `silent_batches_on_the_only_simulator_run_twelve_when_each_recovery_returns_within_the_wait_d39_d48` | 0, 100, 200 ms; 5 s | 12 | 4 | no |
| `silent_batches_on_the_only_simulator_stop_at_the_first_recovery_that_misses_the_wait_d39_d48` | 0, 3, 6 s; 0.5 s | 6 | 2 | no |

Both passed on the first run, in about 5 s together. The clock is real, as in the other tests of that file, so the times
are scaled down. The first case keeps the order of the defaults (each pause well inside the wait), with a wide margin. The
second makes the second recovery's pause longer than the wait.

The pre-D48 count of 3 could not be reproduced by configuration: a very short wait also bounds the wait for the first
device, so the run ends in `NoDevicesException` before any batch. It rests on the reviewer's run and on D48's own tests.

**The texts.** D39 in `docs/deviations.md`, the D39 bullet of the CHANGELOG, the D39 row and Apple section of
`docs/migration.md`, and the x28 findings now say this, qualified "while another device keeps the pool alive" where that is
the condition.

## CHANGELOG and migration guide

- The two D44 bullets about iOS simulators were under Android. They now sit with D33's under iOS simulators, and the
  statements that the two repeated (setups that can't reach the worker aren't counted, a re-add starts over) appear once.
- D40 is the core pool's removal by serial (C27), not an iOS change; it moved to All platforms.
- The evidence names D43's real-tool runs (openrsync on a Mac, rsync 3 on Linux, rsync 3 under `sshpass` not run) and says
  D44 ran on the fake worker only.
- Nothing in Known limits is fixed now: U160, U168 and U103 are open, and the D22, D25, D30, D38 and D48 limits still hold as
  stated.
- The migration guide's action table gains D50 (batches can take longer; check `testBatchTimeoutMillis`) and D52 (a cancel
  takes up to 25 s and writes reports; read the exit code). D22 and D51 join the no-action list. Each of D1–D52 is now in
  exactly one of the table, the no-action list and the list of deviations with an action of their own, checked by a script.

## Found and not fixed

- The sections D14–D20 in `docs/deviations.md` carry no "Approved" date; their dates are only in PLAN §3.11. Left as is.
- CHANGELOG's "Check before you upgrade" is not a copy of the migration table: D33, D35, D39, D41, D50 and D52 are only in
  the table. That was already so for the first four; whether D52, which affects CI harnesses, belongs there too is for the
  owner.
- D38's known limit (on the last device, 6 to 13 minutes plus D48's wait) was not re-derived after D48 and D44. Done in
  [x42](x42-findings.md).
- x26's other incidentals have no rows: a stalled send during remote parsing ends the run even with a healthy second worker
  (Kotlin does the same, so it would be an A row), no test reads `fixtures/oracle/connections/deadlines.jsonl`, and the fake
  worker's `ssh` shim offers the developer's default keys on password runs.

## What was run

- `cargo test -p marathon-ios`: 36 test binaries, 328 passed, 0 failed. `cargo test -p marathon-apple`: 57 test binaries,
  535 passed, 0 failed.
- `cargo fmt --check`, `cargo clippy -p marathon-ios -p marathon-apple --all-targets -- -D warnings`.
- `python3 scripts/check_plan_dag.py`.
- Not run, as the brief says: `scripts/check.sh`, `scripts/remote-check.sh` and the workspace suite.

## Incidental bugs

None beyond the rows above.
