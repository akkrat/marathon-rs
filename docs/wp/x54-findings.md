# WP x54 — SIGQUIT state dumps (D66)

Approved 2026-11-11 (project owner's decision). Base: `65c70cd8`.

## Design

Unix SIGQUIT is independent of D52's SIGINT/SIGTERM relay. The CLI installs its
handler before reading configuration; `ProductionRunner` also installs it
idempotently before vendor construction for embedded callers. A named process-life
thread owns a small Tokio signal driver. An atomic counter retains every observed
delivery until the core consumes it; notification adds no extra coalescing. It does no
console I/O and never calls an exit function. The handler stays installed during
configuration, after normal completion and during bounded cancellation. Before or
after a run it may be silent. Non-Unix builds install nothing.

A child of the run root handles requests on core. It is outside the run child and
the detached scheduler job that D52 cancels, so shutdown can still be inspected.
Pre-run requests are discarded when the snapshot task starts. Normal completion
cancels and joins this task before completing the root. Snapshot
construction is synchronous and sends no actor, adb, SSH or worker command. The
normal output pump forwards one owned WARN event through `tracing`, target
`marathon_core::diagnostics`. The target has a WARN filter exception for every
configurator, including debug off. The console writer sends it to stdout,
consistent with the existing logging design. The explicit header timestamp is
present in compact mode too.

The retained scheduler view exposes its job and phase, and the existing pool
index. Pools keep a small device-handle view, including devices waiting for
termination acknowledgement. Device handles weakly observe their existing state
machines. A monotonic timestamp records each state transition; `since_ms_ago`
measures state age, including running-batch age. Existing prepare/execute job states are included. Queue counters are copied at
message/send and requeue boundaries, with an explicit `handling_message` flag so
a blocked result/tracker delivery does not prevent inspection. No diagnostic
callback is sent through the potentially suspended actor mailbox.

D52's relay, limits, exit path and statuses are unchanged. No configuration field,
result path, command construction, progress line, summary line or comparator
allowance changes. No goldens or copied fixtures were edited.

## Format and bounds

Illustrative regular-output event (timestamp, batch ID and elapsed time vary):

```text
13:24:22.456 WARN marathon_core::diagnostics: SIGQUIT state dump at unix_ms=1794403462456
run=active
scheduler=active phase=joining-pools pools=1
pool=omni
  actor=active supervisor=active devices=1 states={"running": 1}
  queue=active tests_left=0 worker_retry_tests_pending=0 active_batches=1 handling_message=false (counts at last queue boundary)
    device=emulator-5554 state=running since_ms_ago=1000 prepare_task=completed execute_task=active batch=batch-id tests=1
      com.example.MainActivityTest#testText
```

A dump includes at most 16 pools, 32 device details across all shown pools and
eight test names per running device. Identifiers retain at most 160 UTF-8 bytes
plus an ellipsis, and control characters cannot introduce newlines. Each pool
still shows total device counts by state; omitted pool/device/test counts are
printed. A final 64 KiB message cap preserves UTF-8 and appends a truncation marker.
A 126-device preparing farm therefore shows 32 devices and `94 devices omitted`.

## Tests and evidence

The affected-crate run completed with 2,191 passed, 33 ignored and no failures:

```sh
cargo test -p marathon-core -p marathon-cli -p marathon-logging \
  -p marathon-analytics -p marathon-apple -p marathon-android \
  -p marathon-ios -p marathon-macos -p marathon-test-support
```

The ignored tests require an opt-in real SSH server or multi-minute stall runs;
they were not enabled. Final focused checks after restoring all negative witnesses
are recorded below. The new tests do not change any fixture or comparator.


| Test | Assertion | Without-change witness |
|---|---|---|
| CLI `shutdown_reports::sigquit_dumps_each_time_and_the_run_finishes_normally_d66` | Two observed SIGQUIT dumps in each debug mode, running test/batch and queue content, normal exit 0, two passed results, HTML and normal summary, one instrumentation per test | Remove early handler installation: process dies from SIGQUIT before its first dump |
| CLI `shutdown_reports::sigquit_then_sigterm_keeps_d52_reports_and_status_d66` | SIGQUIT dump, about one second, then SIGTERM; exit 143, HTML and exactly the already-finished passed test, no bounced test | Remove early handler installation: process dies from SIGQUIT before SIGTERM |
| CLI `signal_windows::sigquit_before_the_run_starts_does_not_end_the_process_d66` | Vendor construction is held before the run; SIGQUIT survives, subsequent SIGTERM keeps its pre-run OS disposition | Remove early handler installation: SIGQUIT ends the process instead |
| CLI existing `shutdown_reports::a_cancel_writes_reports_first_and_stops_a_slow_worker_process_d52` | SIGQUIT after SIGTERM and report creation, while worker cleanup is held by a TERM-ignoring process; dump says `run=finishing`, exit remains 143, D30 still sends KILL | Existing D52 test expanded with shutdown SIGQUIT; removing installation kills the process at this point |
| CLI `signal_windows::sigquit_after_the_run_finishes_does_not_end_the_process_d66` | Run owner dropped, SIGQUIT, stdin release, normal child exit | Remove early handler installation: SIGQUIT ends the process instead |
| Core `pool_scheduler::state_dump_summarizes_a_large_farm_without_sending_work_d66` | 126 preparing devices, 500 queued tests, exact state totals, 32 details and 94 omitted; snapshot leaves runtime observation unchanged | New API absent at base; removing detail bounds makes the count assertion fail |
| Core `pool_scheduler::state_dump_bounds_the_number_of_pools_d66` | 40 pools, 16 shown, 24 omitted | Removing pool bound fails |
| Core `pool_scheduler::state_dump_bounds_running_test_names_and_reads_suspended_devices_d66` | Suspended running batch of 50 tests, eight names shown, 42 omitted; active queue batch retained | Removing test-list bound fails |
| Core `diagnostics::tests::observed_signals_are_retained_until_the_core_takes_each_one` | Two observed deliveries before the core consumes either retain two requests | Replacing the counter increment with a single pending flag fails the second-consumption assertion |
| Core diagnostic unit tests | UTF-8 byte cap and one-line, bounded identifiers | New native helpers absent at base; direct focused coverage |

Real-time waits use 60 s to catch hangs. Tests gate fake work and observe requests,
reports or dump headers before the next action. The wrapper sequence's one-second
delay is part of the scenario, not a performance assertion. The existing D52
production deadlines remain 24 s for cleanup and 1 s for its final flush. Tests do
not require the host to complete within a tighter wall-time margin.

## Validation

- The affected-crate command above passed. It includes U194's Carbon witness,
  D52's existing signal/report tests, the expanded shutdown SIGQUIT witness,
  native/JVM transcript comparisons and the configuration corpus.
- Final `cargo test -p marathon-core --lib --test pool_scheduler`: 78 passed.
- Final `cargo test -p marathon-cli --test shutdown_reports --test signal_windows`:
  21 passed, including the shutdown SIGQUIT witness in both debug modes and all
  existing D52 signal windows.
- `cargo clippy -p marathon-core -p marathon-cli -p marathon-logging
  -p marathon-analytics --all-targets -- -D warnings` and `cargo fmt --check`: pass.
- The handler negative checks disable both CLI installation entry points. The
  mid-run tests and the shutdown witness fail with the process killed by SIGQUIT.
  The pre-run/post-run negative checks in their own integration binary also fail
  with SIGQUIT (signal 3), instead of the expected later SIGTERM or normal return.
- Removing the pool/device/test caps fails all three `pool_scheduler::state_dump*`
  tests. Collapsing the observed-delivery counter to one fails its unit witness.
  Every temporary mutation is restored before positive validation.
- `check_plan_dag.py`, `make_registry.py --check`, source inventory (507 files)
  and parity matrix (1,166 rows) pass. Inventory/parity use an extracted read-only
  archive of the recorded release source, as the gate does; the baseline checkout
  alone lacks the later source tests and does not match those inventories.
- Differential `test_evidence.py`: 107 passed, after building its required
  `snakeyaml` `yaml_tree` example. No comparator allowance was added.
- `scripts/check.sh`, `scripts/remote-check.sh` and the workspace test command were
  deliberately not run, as this implementation brief assigns gates to the orchestrator.

## Limits and integration notes

- This is an application-state snapshot, not Rust/Tokio stack unwinding. The core
  executor must still be able to poll. A core-thread blocking bug cannot be
  diagnosed by this task while that thread remains blocked.
- Queue counts describe the last safe queue boundary. `worker_retry_tests_pending`
  is the number of distinct pending tests in the existing worker-affinity map.
  Android retries without a worker ID and other unlabelled retry copies are
  included in `tests_left`, but do not have a separate exact pending-retry count.
  Adding such a distinction would duplicate scheduling bookkeeping solely for
  diagnostics. Worker commands and their start times have no shared cheap registry;
  they are not newly tracked. Device preparation/execution state is shown instead.
- Ordinary Unix signals can coalesce before the OS driver observes them.
- Installation failure is an ordinary CLI I/O failure before execution, rather
  than continuing with a default SIGQUIT disposition.
- Config loading is protected by installation at the start of `main`; the gated
  pre-run integration witness holds vendor construction. No filesystem loader
  test seam was added. Post-run protection is the same process-life listener;
  the post-run integration witness sends SIGQUIT after the run owner has been
  dropped, then opens a stdin gate and observes a normal child exit.
- No Linux, non-Unix build or real device/farm was exercised here.
- The x53 documentation branch is not in this base. There is no Unreleased or
  0.13.0 section in this checkout's changelog, so no changelog section was added
  or renamed. There are no ranges ending at D65 at this base to replace. After x53 is
  merged, the orchestrator must advance its new ranges to D1–D66 and add the D66
  bullet to its top 0.13.0 section. D66's section and plan row are additive.
- No migration table row is needed: wrappers need no action; the D66 guide says so.

## Incidental bugs

U194: the macOS Carbon connect-deadline witness can successfully connect after its
accept-queue probe timed out. The gate saw a panic at the former line 839; six of
six reruns passed. A successful send now takes the existing unsaturated-queue skip
path. A blocked or refused connection still checks the real production deadline
and diagnostic. This is a test-harness fix only, in its own commit.
