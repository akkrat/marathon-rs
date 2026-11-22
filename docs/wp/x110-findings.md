# x110 findings: the end-of-run tail in the log

Base `5db150ed`. Logs only (§3.12.5): no D-row, no contract change.

## Design

The time between a run's last test result and its exit is spent in three steps, each now logged once per run as an
INFO line on `marathon_core::timing`, which every configurator shows at INFO with `debug: false`
(`marathon_logging::filter`; the target was already an exception there, so the x104 visibility test needed no
change). One module, `marathon_core::run_end`, words all three, so every vendor writes the same text:

| Line | Where | Clock |
|---|---|---|
| `Artifact finalization at run end took 1.5 s (3 items queued or running)` | iOS `AppleSimulatorProvider::terminate_inner`, around the artifact queue's close (`ArtifactProcessor::finalize`, the old `close_and_join` returning also the number of items admitted and unfinished when admission closed, read under the same lock) | the provider's `VendorClock` |
| `Device release at run end took 2.9 s (iOS, 4 devices; slowest <udid>, 2.9 s)` | iOS: disposal and lease release of the active simulators plus the replaced simulators' lease release; macOS: each Mac's disposal, preparation join and lease release; Android: each device's disposal beside the monitor/setup join, up to the provider's deadline | each provider's `VendorClock` |
| `Reports at run end took 312 ms (10 reporters; slowest AllureReporter, 250 ms)` | `ExecutionReportGenerator::close`, all reporters in order (not the metrics backend's close) | Tokio's `Instant`, as the close's existing 60 s notices |

Durations are whole milliseconds under a second, else seconds with one decimal. The slowest part is named only when
there are several; names are those existing lines use (`Disposed device <udid>` on Apple, Android's
`serial_number()`, the reporter's type name without its path). An Android device still disposing when the provider's
deadline ends the wait is counted, and then no device is named slowest. macOS logs on its first termination only (a
repeated termination, D74, releases nothing). A generator with no reporters, which a production run never has, logs
nothing. Only iOS has an artifact queue (macOS and Android pull within each batch; Android's run-end removal of its
pull staging is not timed), so only iOS has the first line.

No line carries `(+N ms since start)`: that suffix reads the labelled wall clock (`core.di.Clock`), which replay
tapes would then have to script; the line's own timestamp says when it was written.

## Invariants

- Nothing waits differently: the only changes on the paths are clock reads, one count read under the queue lock,
  a short `std::sync::Mutex` (Android, never held across an await) and the log calls. Disposal concurrency, the
  order of steps, deadlines and returned results are unchanged (`close_and_join` is a wrapper over `finalize`; macOS
  folds results with `and`, which keeps the first error as the old `?` loop did).
- Nothing runs on the core executor: provider termination and the reports' close run off core through
  `call_vendor`; logging pushes to the non-blocking queue.
- The reports line is logged after every reporter, so it follows the stdout summary block and never enters the
  pinned summary text; the native differential comparison (`contract_console`) selects only progress lines and the
  summary, and is unchanged.

## Witnesses

Each fails without the change (the line is absent); each runs on virtual time with debug off:

- iOS `crates/marathon-ios/tests/simulator_provider.rs::run_end_logs_artifact_finalization_and_device_release_timings_without_debug`:
  an artifact item released at termination and taking 1.5 s, and a simulator whose shutdown takes 2 s, give exactly
  `… took 1.5 s (1 item queued or running)` and `… took 2.0 s (iOS, 2 devices; slowest <it>, 2.0 s)`; the harness's
  registry is Apple's with debug off and its sink takes only enabled records.
- Android `crates/marathon-android/tests/provider_test.rs::run_end_logs_the_device_release_timing_without_debug`:
  one of two devices' remote cleanup waits for a gate opened at 1.5 s on the driven clock.
- macOS `crates/marathon-macos/tests/provider.rs::run_end_logs_the_device_release_timing_without_debug`: termination
  waits the Mac's 0.3 s bound; `300 ms (macOS, 1 device)`, and a repeated termination logs no second line.
- Reports `event_pipeline::kind_tests::the_reports_close_logs_its_timing_on_the_timing_target_without_debug`
  (`126.5 s (2 reporters; slowest SlowReporter, 125.0 s)`), and in a whole core run
  `crates/marathon-core/tests/startup_timings.rs::a_run_logs_the_start_of_scheduling_without_debug` (now five timing
  lines, the last the reports line).
- Wording: `run_end::tests`.

Local repeat loop of the four run-level witnesses: 50 runs each, 0 failures; 20 runs each beside four busy-loop
processes, 0 failures.

## Tests changed

- `a_slow_reporter_warns_every_minute_and_still_completes_d95` now expects the INFO line after its two WARNs.
- `the_execution_time_history_is_queried_while_the_run_starts_before_scheduling` expects the reports line last.
- Comparisons with JVM recordings leave out exactly the x110 lines through
  `marathon_test_support::run_end::is_x110_run_end_timing` (INFO, timing target, no cause, whole message of one of
  the three shapes), with negative tests for other timing lines, other targets and levels, a cause, raw lines and
  near misses of each shape: `run_flow_witnesses.rs` (`rust_events`) and `live_output.rs` (the finish's last flush,
  which had nothing to write before, now writes only that line and is not counted as a flush point).
- Two `event_pipeline.rs` integration tests that capture every log record now ignore the timing target (the reporter
  failure test) or keep WARN and ERROR only (the queue overload test, whose capture is named `warnings`).

No golden, oracle recording, fixture or parity-matrix row changed.

## The JVM's artifact-queue summary (no U267)

The JVM prints `Artifact queue admission: …` (INFO) and `Artifact finalization took … ms` (DEBUG) with
`debug: false` only because the provider, and with it the queue's logger, is constructed before
`configureLogging()`: C18, a logger last requested before then keeps DEBUG. WP 8.8 superseded C18 under §3.12.5, so
Rust's `marathon_apple::artifact_processor` lines follow `debug`. That is a recorded, approved log difference, not a
parity bug, so no found-bugs row. The x110 artifact line now gives the wait and its size with debug off; if the owner
wants the admission statistics too, it is a one-line filter exception (`marathon_apple::artifact_processor=info`
with debug off) and would also show that target's other INFO lines.

## Verified, not verified

Ran: `cargo fmt --check`; `cargo clippy -p marathon-logging -p marathon-core -p marathon-apple -p marathon-ios
-p marathon-macos -p marathon-android -p marathon-test-support -p marathon-analytics --all-targets -- -D warnings`;
`cargo test` of marathon-logging, marathon-core, marathon-apple, marathon-ios, marathon-macos, marathon-android,
marathon-test-support, marathon-analytics and marathon-cli; `tools/differential/make_registry.py --check`,
`tools/differential/run validate`, `tools/differential/test_evidence.py`, `tools/fake-worker/test_apple_evidence.py`,
`tools/real-e2e/run validate`, `test_real_e2e.py`, `test_outcomes.py`. Not run (gate rule): `scripts/check.sh`,
the remote gate, `cargo test --workspace`. Not verified on real devices: the lines' values on a farm.

## Incidental bugs

None found.
