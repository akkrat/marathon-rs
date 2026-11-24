# x115 findings: count the last failure; close recorder connections

Base `cbd0b8c7` (0.15.5). Follow-ups from a farm trial of 0.15.5-rc.1, an iOS project's UI job with 40 remote and
4 local simulators, fixed-quota retries and the screenshot recorder, approved by the project owner on 2026-11-24:

- **D99** (fixes the kept Kotlin bug C5): a failure that meets exactly 0 total retries left ends its test as failed.
- **U270**: runs with the screenshot recorder no longer end with about one `Dying because - Broken transport;
  encountered EOF` ERROR per remote simulator.
- **U271** (owner's go on the same day, after a related observation from another farm trial): marathon's own stop of a
  screenshot command that waits for its exec reply is no longer logged as `ERROR … Unable to start a remote shell
  session`.

## D99: a failure that meets no total retry quota left

### Cause

`FixedQuotaRetryStrategy::retried` (`crates/marathon-core/src/strategy/retry.rs`) ported Kotlin's three-step filter:
`retry_possible` accepted `left >= 0`, then `progress.retry_test` ran AddRetry (raising the test's total), then
`request_retry` decremented and refused at -1. At exactly 0 left the test gained a phantom total: in the farm run a test
was INCOMPLETE twice (re-queued, total still 1) and then failed at 0 left, which left it `Failing` at 1 of 2 with nothing
queued. `aggregate_result` logged `Expected to run … more, but no terminal events received`, the final attempt was never
tracked as final, and the summary counted the test as neither passed nor failed. The exit status was a failure anyway.

### Design

The watchdog has one entry point, `RetryWatchdog::grant(failures, add_retry)`, which decides and charges: it refuses
unless `left > 0` and `failures <= retryPerTestQuota`, then runs `add_retry` (AddRetry), and charges the total quota only
when AddRetry did not answer Complete. `retry_possible` and `request_retry` are gone, so a check and a charge on different
boundaries cannot be written again. `left > 0` is exactly the old grant condition (`left >= 0 && left - 1 >= 0`), so every
grant with quota left is unchanged, and so are the per-test quota, the order of grants within one `failed` batch (the
filter still walks the batch in order), flaky copies counted as failures, and AddRetry answering Complete (neither
granted nor charged). The counter no longer wraps: nothing is charged below 1.

Invariants: AddRetry runs only for a retry that is granted; the quota is charged once per granted retry and given back
once by D69's refund when that retry is not run. A refund after the quota ran out now restores a whole retry (Kotlin's
phantom had left -1, which a refund brought back only to 0, the C5 boundary).

Other strategies: `no-retry` returns no retries and never calls AddRetry. There are no others in the Kotlin marathon or
here (`RetryStrategyConfiguration` has `FixedQuota` and `NoRetry`).

### Tests

- Strategy level: `strategy_retry::at_zero_quota_left_add_retry_does_not_run_d99` (renamed from
  `at_zero_quota_left_add_retry_still_runs_and_the_decrement_refuses`), `a_refund_after_the_quota_ran_out_grants_the_next_failure_d99`,
  `a_negative_total_quota_grants_nothing_and_is_not_charged_d99` (was `watchdog_counters_wrap_like_java_ints`),
  `a_grant_refused_by_the_per_test_quota_runs_no_add_retry`. The ported RetryWatchdogTest cases now call `grant` through
  a `grants` helper; their assertions and parity rows are unchanged.
- JVM retry table: `retry_oracle_table_replays_with_d99` replays every row through a narrow allowance, `d99_expected`,
  which rewrites only a round whose quota goes from >= 0 to -1: it drops that round's last `retryTest` (asserting it was a
  non-terminal test), takes the phantom out of that test's total and keeps the quota at 0. Its negative test,
  `the_d99_allowance_covers_only_the_c5_phantoms`, checks that exactly the five C5 rows are rewritten, that each of them
  as recorded no longer replays, and that a quota starting below 0 is left alone. The recording is unedited.
- Real FSM: `progress_oracle::a_failure_refused_at_zero_quota_ends_the_test_d99`, split out of
  `ended_only_and_terminal_retry_keep_the_source_bookkeeping`, which keeps its C4 part and its name (C4's row cites it).
- Queue and progress: `queue_behaviour::a_failure_that_meets_no_retries_left_ends_the_test_d99`: two tests fail together
  with a total quota of 1; the second ends Failed (1, 1, -1), its failure is tracked as final, the pool's result is
  false and no ERROR is logged. The JVM recording of C5 is superseded like D69's three
  (`queue_oracle::d99_supersedes_exactly_the_c5_recording` pins that it differs exactly at step 1, step 3 and the
  progress); `d69_supersedes_exactly_the_c45_to_c47_recordings` now also lists `K2 progress`, because K2's second failed
  copy met 0 left as well: the phantom total is gone and the retry's pass completes the test (Passed 3, JVM Passing 4).
  `native_replay_rejects_queue_and_accounting_mutations` uses another recording.
- Run graph with the Stub vendor: `flakiness_scenarios::d99_a_failure_that_meets_no_retries_left_is_counted_failed`
  (quota 1 and quota 0): raw runs, `Some(Ok(false))`, the per-test result file's FAILURE, the summary's
  `0 passed, 1 failed` with the test under `Failed tests:`, the `Raw:` line, and no ERROR line at all.
- Pool scheduling: the JVM recording of
  `a_retry_of_one_of_two_batches_that_failed_together_runs_on_an_idle_device_of_another_worker` tracked the refused
  failure as non-final; a named D99 arm in `support/pool.rs` asserts the recording and expects that one flag moved
  (`FAILURE final`). The ported Kotlin test's own assertions are unchanged.
- No marathon-cli end-to-end test was added: the CLI suites run the core with stub runners, and the run-graph test above
  already shows the summary text the CLI prints.

Parity matrix: no row changes. The Kotlin tests involved (FixedQuotaRetryStrategyTest, RetryWatchdogTest,
DevicePoolActorSchedulingTest) still hold as written.

## U270: SSH connections left for the process's exit

### What the line means

`ConnectionShared::set_death` logs `Dying because …` unless the death is our own close
(`disconnect_locally`, `abandon`). `CoreExecutor::run` drops the core thread's run graph (devices, providers, the
connection factory) after the reports, while the vendor runtime still runs, and shuts that runtime down only after
(`crates/marathon-core/src/executor/mod.rs`). An `SshConnection` that is neither closed nor abandoned when its last
holder goes ends its russh session from the client side (the session's handle channel closes), which russh reports as
`Disconnect` and the port as `Broken transport; encountered EOF`. So each line is one connection marathon had not
closed before teardown.

### Ownership chain (Step A)

- A simulator's lease executor is an `SshCommandExecutor` that `ConnectionFactory` opened (one per simulator without
  `shareWorkerConnection`, one per worker generation with it). It owns the data `SshConnection` and its D96
  `Supervisor`, whose `Peer` holds the data connection again and dials a second, control connection when a supervised
  command needs a renewal or a signal (any command longer than the renewal interval, 15 s by default).
- The simulator holds the executor through `DeviceCommands` and its remote file manager. The provider releases the lease
  at the run's end (`release_lease` → `ConnectionFactory::dispose_before` → `executor.close()`: supervisor retire, then
  the data connection), unless D93 quarantined it.
- The screenshot listener (`ScreenCapturerTestRunListener`) runs its capture as a task on the vendor runtime that holds
  the simulator. It was cancelled only by a test end or `test_run_failed`; a batch that ended after `test_started` and
  before the test's result (xcodebuild exiting without one, the farm suite's usual outcome: "returned no results …
  xcodebuild exited with status 65") left it running for the rest of the run, taking screenshots and pulling them
  (extra sessions and rsync connections; the farm's `Unable to start a remote shell session … screenshot` lines just
  before the device release are this capture failing on the released connection). Kotlin gave this job a parentless
  `SupervisorJob()`, so the JVM leaked it too and exited quietly.

Three gaps, all reachable in that situation:

1. The leaked capture kept the simulator, and with it its lease executor, supervisor and connections, alive past the
   release, until the vendor runtime dropped the task. A diagnostic build (local only, not committed) printed every open
   `SshConnection` dropped: on the worker its backtrace showed exactly this chain (capture task → simulator → remote file
   manager → `DeviceCommands` → `SshCommandExecutor` → `Supervisor` → `Peer` → an open data connection).
2. A capture pull in flight at the release made D93 find the simulator's work unconfirmed ("a pull from it was still
   running"), which quarantines the lease: kept open "for the rest of the run" and never closed, so it was dropped open
   at teardown.
3. `SshCommandExecutor::close` retired the supervisor and closed the data connection but left the control connection
   open until the `Peer` was dropped (where it is abandoned without a log line), which gap 1 could postpone to exit.

Worker drops are not a necessary ingredient: an interrupted batch is enough. The farm's rsync `No route to host` and
`kex_exchange_identification` lines fit the leaked captures' extra pulls on a loaded worker; they were not needed to
reproduce the line.

### Fix

- The capture job belongs to its batch: `after_test_run` (the batch's `finally`) cancels and joins it, as the video
  listener stops its recorder there, and `Drop` cancels it, so it cannot outlive its listener even when an earlier
  listener's `after_test_run` fails (the composite stops at the first error). The file of a test that never ended is
  left as before: not attached.
- The factory that opened the connections closes what is left: `ConnectionFactory::close_remaining(deadline)` takes
  every lease still held after the provider has released its devices (a quarantined one included, since the rest of the
  run ends here, or one whose release never came) and closes each exactly as a final release does: within the release
  bound (`release_deadline`, the provider's termination deadline on iOS), abandoning one that cannot finish with the
  existing WARN. The iOS and macOS providers call it at the end of their termination, before the device-release timing
  line. `ProvisioningConnections` and `MacosConnections` have it as a default no-op for test doubles.
- An executor closes what its supervisor opened: `close()` now retires the supervisor, closes the control connection
  (`Supervisor::close_control`), then the data connection; the peer then dials no control connection again.

ERROR stays the level of a real loss; nothing is filtered.

### Tests

Each fails without its part of the fix (checked on a scratch copy with that part reverted, see below):

- `ios_listener_world::a_capture_whose_test_never_ended_stops_with_its_batch_u270`: in the fake-worker screenshot world,
  a test starts and the batch ends without its result; when `after_test_run` returns, the capture's GIF has its trailer
  (it is written only when the capture stops) and no screenshot command follows.
- `ios_listener_world::a_dropped_screenshot_listener_stops_its_capture_u270`: the same, but the listener is dropped
  without `after_test_run`; the capture stops (a 60 s hang guard bounds the wait for the trailer).
- `connection_factory::the_end_of_the_run_closes_every_connection_still_leased_u270` (shared and unshared): a quarantined
  lease, a second user of it and a never-released lease are closed once each by `close_remaining`, none abandoned; a
  connection released before is not closed again; a second call and late releases close nothing more.
- `connection_factory::the_end_of_the_run_leaves_a_shared_acquisition_under_way_intact_u270`: a shared acquisition that
  is still connecting a replacement when `close_remaining` runs completes, and its connection is its caller's to
  release. The first version of `close_remaining` cleared the workers' shared state, and that acquisition then
  panicked on its `expect("shared worker is registered")` (found in the self-review; now each worker keeps its entry
  and only loses its current connection, so a closed one is never handed out again).
- `ssh_supervisor::closing_an_executor_closes_its_supervisor_control_connection_u270` (loopback fake SSH worker with the
  supervisor fixture): a supervised command outlives its first renewal (1 s interval), which dials the control
  connection; `executor.close()` then ends both connections at the server while the executor is still held, no control
  connection is dialed after it, and no `Dying because` line is logged.

### Step B: real worker

The shared real-worker harness (a Mac reached over a dedicated port forward with a throwaway key and a forced command,
two simulators of our own, iOS 18.6, Xcode 27.2, the R01 UI sample) with the R01 configuration changed to the screenshot
recorder (`preferableRecorderType: screenshot`, JPEG, video off, `xcresult` `ALWAYS` with `remoteClean`), `debug: false`
unless stated. Control: the published 0.15.5 macOS build (zip checked against `SHA256SUMS`). Fix: release builds of
this branch (`F`/`H` runs before the U271 commit, `U` runs after it; both before the self-review change to
`close_remaining`, which only matters to an acquisition still under way at the end, which no run had). "Interrupted" runs stop our own `xcodebuild` for simulator A about 3 s after its first screenshot (SIGTERM,
or SIGKILL), which ends that batch with started tests and no results, as the farm's suite does; no network fault was
injected. "A shots" counts simulator A's screenshot commands on the worker.

| Run | Build | Setup | Wall s | `Dying because` at exit | A shots | Last A shot before exit | Quarantine WARN | Other |
|---|---|---|---|---|---|---|---|---|
| B1 | 0.15.5 | healthy, batch 1, shared | 52.7 | 0 | – | – | 0 | no control connection dialed |
| B3 | 0.15.5 | healthy, batch 3, shared | 34.8 | 0 | – | – | 0 | one control connection |
| B4 | 0.15.5 | healthy, batch 3, per device | 32.4 | 0 | – | – | 0 | one control connection |
| K1 | 0.15.5 | SIGKILL, batch 3, per device | 55.2 | **1** | 35 | 1.0 s | 0 | session not closed, device lost |
| K2 | 0.15.5 | as K1, debug | 56.1 | **1** | 39 | 1.1 s | 1 | lease quarantined, kept open |
| T1 | 0.15.5 | SIGTERM, batch 3, per device, debug | 48.8 | **1** | 35 | 3.7 s | 1 | "a pull from it was still running" |
| T2 | 0.15.5 | SIGTERM, batch 3, shared, debug | 35.6 | 0 | 35 | 0.6 s | 0 | capture leaked to the end |
| C3 | 0.15.5 | SIGTERM, batch 3, per device | 27.6 | 0 | 30 | 0.9 s | 0 | capture leaked to the end |
| C4 | 0.15.5 | SIGKILL, batch 3, per device | 28.5 | 0 | 27 | 1.8 s | 0 | capture leaked to the end |
| F1 | fix | SIGTERM, batch 3, per device | 28.4 | 0 | 15 | 4.2 s | 0 | capture stopped with its batch |
| F2 | fix | SIGKILL, batch 3, per device, debug | 27.7 | 0 | 13 | 3.9 s | 0 | |
| F3 | fix | SIGTERM, batch 3, per device | 25.9 | 0 | 15 | 2.5 s | 0 | |
| F4 | fix | SIGKILL, batch 3, per device | 26.8 | 0 | 14 | 4.1 s | 0 | |
| F5 | fix | SIGKILL after 1 s, per device, debug | 44.8 | 0 | – | – | 0 | "returned no results for 3 tests … status 137", both leases disposed |
| H1, H3, H7, H9 | 0.15.5 | healthy, batch 1, shared | 30.0, 30.1, 30.0, 28.4 | 0 | – | – | 0 | |
| H2, H4, H8, H10 | fix | healthy, batch 1, shared | 29.3, 38.2, 29.2, 29.2 | 0 | – | – | 0 | H4: preparation 5.4 s (others 2.4–3.1 s); one U271 line |
| H5 / H6 | 0.15.5 / fix | healthy, batch 3, per device | 26.0 / 26.0 | 0 | – | – | 0 | |
| U1, U3, U5 | 0.15.5 | healthy, batch 1, shared, 50 ms screenshot delay | 52.7, 34.0, 31.7 | 0 | – | – | 0 | 0 U271 lines |
| U2, U4, U6 | fix + U271 | as U1 | 53.5, 31.7, 35.7 | 0 | – | – | 0 | 0 U271 lines; U2: 5 of 6 GIFs (below) |

- With 0.15.5, after an interrupted batch the capture went on until about a second before exit in all six runs, and
  three of them ended with the line (whether a connection is left open depends on timing: a pull in flight at the
  release, or a session that does not close). With the fix the capture stopped with its batch and no run had the line.
- Deliveries: every healthy run of both builds published 6 screenshot GIFs and 6 xcresults (batch 1) or 6 GIFs and 2
  xcresults (batch 3), except U2 (below); the eight H batch-1 summaries are identical. In U2 one test's xcodebuild
  output reached marathon in a burst about 4 s late, so the test's start was parsed about when it ended; its capture was
  cancelled before its first screenshot command (the worker's log has none in that batch), took no frame, and an empty
  capture is not published, as before. Delayed output on a loaded worker can do this to any build. Wall time: medians 30.0 s (0.15.5) and 29.2 s
  (fix); H4's extra time is in its preparation.
- No new WARN or ERROR: besides the worker's shell profile echo and xcodebuild's stderr, the fixed runs logged only the
  expected `uncompleted` WARNs and the batch-outcome WARN of the interrupted batches, and one U271 line (H4, built
  before the U271 fix).
- The 0.15.5 runs whose connections were never closed also left their D96 supervisor roots on the worker (six, from
  B2, K1, K2, T1, C3 and the diagnostic D1); no fixed-build run left one. Those six were ours (none existed before) and
  were removed at cleanup.
- Two diagnostic runs not in the table: B2 (0.15.5 with `keepAliveInterval: PT1S`, which made keep-alive drop
  connections mid-run; two `Dying because - Did not receive any keep-alive response` lines mid-run, a different, real
  loss) and D1/D2 (the diagnostic build, used only for the backtrace above).

The U-runs used a second build of this branch with the U271 fix and a second worker session set up and cleaned up the
same way. Cleanup after each session: both simulators deleted, the worker's staging directory and the six supervisor roots removed, the throwaway
key removed from the worker, the dedicated forward stopped; the shared SSH master was never touched.

## U271: our own stop logged as a session that could not start

### Cause

Another farm trial of 0.15.5-rc.1 logged 0–10 `ERROR … Unable to start a remote shell session [xcrun, simctl, io,
<udid>, screenshot, …] error=SSH connection failure: Stopped while waiting for exec reply` per iPhone run, mid-run (the
JVM 0–1). The text comes from `ExecChannel::exec_gated` (`crates/marathon-apple/src/cmd/remote/ssh/sshj/channel.rs`):
on a supervised worker the exec's gate is the caller's cancellation (with its device's command gate), and the gate
fired while the exec waited for the worker's reply. For a screenshot the caller's cancellation is the capture job's:
`ScreenCapturer::capture` passes it to `source.screenshot(…)`, and the listener cancels the job at every test end
(`ScreenCapturerTestRunListener::ended` → `ListenerContext::cancel`). The channel was closed and the error was a
`ConnectionException`, which `CommandExecutor::safe_execute` logs at ERROR as a session that could not start and turns
into "no result". So marathon's own stop was reported as a failure; it is not a session limit (no `Opening session
channel failed` line). It is not U270's cause: it happens at test ends, mid-run, with no leaked capture, and the U270
changes neither cause nor remove it. In Kotlin the same stop is a coroutine cancellation, which `safeExecute` does not
catch, which matches the JVM's 0–1 lines.

On the real worker it appeared twice in 33 screenshot-recorder runs, each right at a test end of that simulator: once
with 0.15.5 (B2) and once with the first x115 build, which did not have this fix (H4). Six more runs with a 50 ms
screenshot delay (U1–U6, to make a pending exec reply at a test end likelier) showed none with either build, so the
real-worker counts are too small to show the fix; the witness below does.

### Fix

When its gate stops the wait for the reply, the channel records the gate's own reason (`ExecChannel::exec_stopped`,
beside D93's `exec_withheld`), and `SshCommandExecutor`'s `execute` returns `VendorError::Cancelled(reason)` instead of
the connection failure. The D93 "known to have started nothing" flag stays false for it, as before (the request was
sent; the supervisor ends the token). `safe_execute` passes a cancellation on without logging it; the capture loop ends
on it as on any cancellation. A real loss of the connection on the same wait is still a connection failure, logged at
ERROR. Nothing filters text or changes a level.

Witness: `ssh_supervisor::our_own_stop_of_an_exec_waiting_for_its_reply_is_not_an_error_u271` (loopback fake worker
whose supervised exec never gets its reply): cancelling the caller's context makes `safe_execute` return
`Err(Cancelled)` with no ERROR line; then, on the same kind of wait, shutting the worker down gives `Ok(None)` and one
`Unable to start a remote shell session [echo, fixture-no-launch-ack]` ERROR.

## Verification

Ran, all passing (details in the report): `cargo fmt --check`; `cargo clippy` on the changed crates with
`--all-targets -D warnings`; `cargo test` for marathon-core, marathon-apple, marathon-ios, marathon-macos and
marathon-cli; `scripts/check_plan_dag.py`, `tools/differential/run validate`, `tools/differential/test_evidence.py`,
`tools/fake-worker/test_apple_evidence.py`. Not run, per the shared rules: `scripts/check.sh`,
`scripts/remote-check.sh`, `cargo test --workspace`.

Each new witness fails without its fix, checked on a scratch worktree of this branch with only that part of the
production change reverted: the D99 witnesses (11, including both allowances) with the old check-then-decrement
semantics; the two listener witnesses without `after_test_run`/`Drop` (the dropped-listener one at its 60 s hang
guard); the factory witnesses with `close_remaining` closing nothing, and with the first version that cleared the shared state
(a panic); the control-connection witness without
`close_control` (at its 30 s hang guard); the U271 witness without the executor's `exec_stopped` branch. Stress: each
U270 and U271 witness 50 runs (the factory ones 100) and 20 more with every core busy, all passing.

A marathon-cli run started from a shell background job (`… &`) inherits SIGINT and SIGQUIT as ignored, and four
`shutdown_reports` tests then fail (exit 143 for 130, no state dump); started in the foreground they pass. Not a product
issue; worth knowing when running the gate by hand.

Not verified: a farm run (the farm suite is the owner's); a quarantine closed by `close_remaining` on a real worker
(the fix removed the quarantines that 0.15.5 produced in these scenarios, so the path ran only in the unit test); macOS
on a real worker (same factory call, unit-tested through the factory).

## Golden diffs

None. No registered native golden or JVM recording changed. D99 changes result files only for a test whose failure
meets 0 retries left, which no registered case reaches; the JVM recordings that do are compared through the named
allowances above.

## Incidental bugs

- The screenshot capture leak is a kept Kotlin behaviour (parentless `SupervisorJob`) that x115 fixes as part of U270;
  it has no row of its own in `known-kotlin-bugs.md`.
- `scripts/parity_matrix.py --check` reports a `SkipCopyingDerivedDataTest` row as no longer found in the Kotlin
  checkout; the same on the base commit, so not from this WP.
