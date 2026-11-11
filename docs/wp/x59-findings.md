# WP x59 — CI replay and console flush flakes

Scope: the two failures reported on the Ubuntu 22.04 CI runner on 2026-11-11.
The implementation changes tests only. Item 2 requires a product decision;
its proposal below is not implemented. No JVM evidence, comparator allowance,
configuration, exit status, pinned output or production deadline is changed.

## U195: same-deadline vendor continuations

`runtime_races::same_deadline_vendor_continuations_keep_registration_order`
registers two vendor delays at the same virtual deadline, advances to it,
receives `0` then `1`, and asserts quiescence, for 2,000 traces. The reported
failure was `scheduler: runnable work remains`, after both values matched.

The API distinction matters:

- `ControlledReplay::assert_quiescent` synchronously snapshots the shared
  runnable/running/blocking tracker, then checks registered mailboxes. It
  does not wait for work or perform a vendor join.
- `drain_current` runs the notification-driven cross-executor drain. It
  waits for tracker acknowledgements, releases due timers one at a time,
  and does not advance virtual time to future deadlines.
- A successful `SharedChannel::receive` only consumes the value. Sending it
  is before the vendor body's `Ok(())`, `IoRuntime` result completion and
  tracked-task teardown; it is not a task-completion fence.

`advance_to` already drains at the target, so the CI message alone does not
prove the vendor body was still returning. In particular, `Tracked::poll`
releases its running unit on return, while `Tracked::drop` separately counts
future destruction; a later snapshot can overlap that teardown. The failure
identifies outstanding tracked work, not an ordering failure. See
`executor/test_support.rs`, `executor/tracking.rs`, `executor/mod.rs` and
`executor/shared.rs`, and the equal-deadline/quiescence rules in
[the task design](../design/tokio-tasks.md).

A post-receive `drain_current` alone was tried and rejected: the same
stress reproduced the failure twice. The existing `VendorTask::join` would
not supply the missing fence either: `IoRuntime` seals its result cell before
returning its final poll, before `Tracked` is destroyed.

The test-support-only `ControlledReplay::spawn_vendor` seam therefore spawns
`Tracked` work on the same vendor Tokio handle with the same tracker, and
returns the physical Tokio `JoinHandle`. Tokio 1.53.2 replaces the future
with its output (destroying `Tracked`) before marking the task complete and
waking that handle (`runtime/task/core.rs::store_output` and
`runtime/task/harness.rs::{poll_future,complete}`). Awaiting both handles is
thus a teardown fence. This seam bypasses `IoRuntime`'s result-cell and
cancellation adapter; those are not the ordering contract this test pins and
remain covered by the other runtime tests. The feature-gated replay support
is the only changed source module; production executors and tracking stay
unchanged.

After receiving `0` then `1`, the test joins both physical tasks, checks both
outcomes, drains current work and asserts quiescence. It still uses the same
vendor clock and executor, tracked futures, one-at-a-time registration drains,
one-millisecond equal deadline, channel order and 2,000-trace loop. The joins
happen after the order assertions, so they cannot serialize or manufacture the
observed send order. No sleeps, extra guessed yields or wider timeouts are
used.

### Stress evidence

The unmodified test was built at base `5e7164a9`. A local harness ran its
compiled test binary 1,000 times with the arguments
`--exact same_deadline_vendor_continuations_keep_registration_order` and
`--test-threads=1`, alongside four CPU busy-loop processes. Every execution
retained the original 2,000-trace loop. The host has 18 logical CPUs; this is
additional scheduling pressure, not a claim to reproduce a two-core Linux
runner or saturate the host. Each process had a 60-second hang guard; all
busy workers were joined at the end.

Before the fix: 999 passed, one failed (execution 793), in 138.67 seconds.
The failure was exit 101 with the exact CI error at
`tests/support/mod.rs:23`: `ReplayError { label: "scheduler", message:
"runnable work remains" }`. No receive-order assertion failed. This is the
unfixed concurrency witness, obtained before editing the test.

The drain-only attempt: 998 passed, two failed (executions 463 and 977),
in 149.97 seconds, with the same quiescence error. That attempt was replaced
by the physical-task joins described above.

Final fix: 1,000 passed, zero failed, in 145.93 seconds, under the same
four busy workers and per-process guard. Every execution still ran all
2,000 traces: two million passing traces. This is bounded stress evidence,
not a claim about every possible Linux scheduling interleaving.

Temporary removal of just the final physical-task joins (retaining the
new spawn seam and post-receive drain) failed at execution 153 with the
same `scheduler: runnable work remains` error and exit 101. The harness
stopped at that first failure, after 22.33 seconds; 152 executions had passed.
The joins were restored before all final checks. This isolates the completion
fence rather than relying only on the original baseline failure.

## U196: ordinary console flush under host load

The differential test runs `tools/oracle/config-differential-lib/suite.py`
against the production binary. Its `command` helper rejects any stderr
unless explicitly allowed; production replay does not allow it. The reported
binary exited 0 and printed exactly
`Console flush failed: console flush deadline elapsed` to stderr. The writer
missing its acknowledgement under host load is consistent with the code;
the report does not distinguish scheduling delay from slow sink I/O.

### Call chain and output consequences

`run_cli` calls `CliOutput::flush` after every normal command result, including
help, errors and configuration dumps, then returns the command's original
status. `CliOutput::flush` calls `TracingOutput::flush`, which enqueues a FIFO
`Command::Flush` and waits with `recv_timeout(self.exit_flush)`. Production
construction sets that duration to `EXIT_FLUSH`, one second. A timeout is
printed directly to stderr and does not change the command's exit status.
This happens on the CLI thread, off core. `QueueWriter::flush` itself is a
no-op; it is not this acknowledgement barrier.

The gated test
`tracing_output::ordinary_flush_timeout_keeps_status_and_queued_output_u196`
uses the existing configurable test seam with a zero expiry, holds the writer
before the command starts, and checks exit 0 with the exact stderr diagnostic.
It then releases the writer and joins its drain with a 60-second hang guard,
checking the complete ordered version output and no added diagnostic. This
forces the acknowledgement timeout without relying on host load or a guessed
sleep; it does not alter production policy.

The timeout does not dequeue, cancel or discard any bytes. The console writer
can subsequently finish the preceding writes and flush the sink; sending its
acknowledgement to the dropped receiver then fails harmlessly. Thus the message
can be the only visible effect, even with complete stdout. It is not proof
that logs were lost, and it is not proof that all bytes were written.

There is also a genuine output-loss risk. `main` returns after `run_cli`,
and dropping `CliOutput` calls `shutdown`, which sends `Finish` and waits
another one-second budget through `OutputGuard::finish`. If that completes,
all admitted bytes have been drained and the writer joined. If it times out,
it reports `Console shutdown failed: console drain deadline elapsed (...)`,
detaches the writer and permits process termination; outstanding output can
then be lost. The guard's own `Drop` cannot retry after `finish` took the
handle. The two ordinary waits are sequential, not one shared deadline.

The signal route is separate: `exit_after_preparing` arms the
`marathon-exit` watchdog before the off-core preparation thread forwards an
`Exit` command. The watchdog waits the same configured one-second duration
and calls `exit_once` even when preparation or stdout is blocked. A second
signal calls `exit_once` immediately. The existing logging design documents
both the ordinary and signal one-second policies; the lifecycle design
points to that document as the authority for writer shutdown.

### Recommendation: a separate ordinary completion budget

Recommend a product change, rather than ignoring this stderr in the
configuration differential suite: give the ordinary `TracingOutput::flush`
and ordinary `OutputGuard` drain a separate 60-second budget, retaining the
one-second signal exit watchdog, second-signal immediate exit, queue ordering
and nonblocking producers. Do not increase `EXIT_FLUSH` globally, since it
currently also controls the signal watchdog. Update the ordinary-path wording
in `docs/design/logging.md` with the change; introduce no configuration knob.

Sixty seconds is a bounded allowance for a heavily scheduled host and a slow
sink, not a guarantee that arbitrary overload will finish. A permanently
blocked ordinary sink could then consume two sequential waits (up to about
120 seconds plus scheduling), which is the explicit trade-off for preserving
normal command output. The signal watchdog must still end that process after
its existing one-second preparation/flush budget when cancellation is requested.
An implementation should decide whether one shared ordinary completion
deadline is preferable to those two waits, and pin that choice in gated
writer tests before changing it.

A comparator exception would normalize away a production diagnostic and could
allow an incomplete configuration dump to appear acceptable. Merely retrying
the differential command could hide the same issue. Keep the differential
suite strict. A later product implementation should test delayed
acknowledgement followed by full recovery, a stalled ordinary sink reaching
its own deadline, real sink errors still being reported, and the unchanged
first/second signal bounds. Use gates to order the writer; short deadlines
belong only to tests that deliberately exercise expiry, with 60-second hang
guards for the other waits.

This is the requested stop before changing product code. U196 remains open
for the owner's decision; there is no test-side tolerance or deadline fix in
this branch.

## Verification

All builds use `CARGO_BUILD_JOBS=6`, `CARGO_INCREMENTAL=0` and
`CARGO_PROFILE_DEV_DEBUG=0`. The initial disk check had 144 GiB available,
above the brief's 20 GB stop threshold.

The first `python3 -I tools/differential/test_evidence.py` run failed
(107 tests, 91 errors) because the fresh worktree had not built the native
YAML helper. The documented `cargo build -q -p snakeyaml --example yaml_tree`
prerequisite and a clean rerun are included in the final checks below.
No assertions or comparator rules were relaxed to handle that prerequisite.

Final checks completed so far:

- `cargo fmt --check`: passed.
- `cargo clippy -p marathon-core -p marathon-cli --all-targets -- -D warnings`:
  passed.
- `python3 -I tools/differential/make_registry.py --check`: passed.
- Native YAML helper build: passed; subsequent
  `python3 -I tools/differential/test_evidence.py`: all 107 tests passed.
- `cargo test -p marathon-core -p marathon-cli --no-fail-fast`: passed,
  899 tests passed, zero failed, seven existing ignored tests across 85
  test/doc-test targets. Includes the strict configuration differential,
  runtime races, the new gated timeout witness, shutdown reports and signal
  windows.
- `git diff --check`: passed.


The brief's explicit gate override applies: `scripts/check.sh`,
`scripts/remote-check.sh` and the workspace test suite are not run. No Linux
CPU-affinity experiment, real device, remote runner or new JVM recording is
used. The stress harness is local temporary tooling and its worker processes
are stopped and joined; it is not added to the repository. The unfixed base
was exercised before editing, followed by a temporary removal of the final
physical-task joins. The drain-only attempt was rejected on measured
failures. The final fix adds a test-support physical-task fence because the
existing replay drain and result-cell join cannot supply it.

The worktree is retained and its `target/` directory is removed after checks;
nothing is pushed or merged.

## Incidental bugs

No additional independently verified bug beyond U195 and U196. The tracked
completion/destruction window is documented under U195; this branch does not
change the runtime's tracking contract.
