# WP x76: a slow iOS video transfer does not hold back the next test (U201, D80)

Base: `573a962d` (tag 0.14.0-rc.1). Approved 2026-11-17 by the project owner for 0.14.0. Deviation D80: the change
alters result data (videos and durations) for a behaviour the Kotlin marathon has too.

## Cause

Seen in the real remote iOS check of 0.14.0-rc.1, run 06 (two UI tests, video on, transcoding on, one simulator). The
log of that run shows the chain:

| Time | Event |
|---|---|
| 05.100 | xcodebuild: `testAlwaysFailing` failed; the listener handles `testFailed` at once |
| 05.162 | xcodebuild: `testButton` started |
| 08.511 | the video listener has stopped the first recorder (3.4 s: `cat` pidfile, `kill -s INT`, `lsof +r 1` until the recorder exits, `rm` pidfile) |
| 13.243 | xcodebuild: `testButton` passed |
| 13.604 | transcode finished (4.5 s) |
| 28.153 | pull finished (14.5 s, 197 KB at 13.5 KB/s) |
| 28.348 | the listeners handle `testStarted testButton`, then `testPassed testButton` in the same millisecond |

Two pieces of code make this happen, both ported as they are from the Kotlin marathon (commit `e47352f2`):

- `AppleDeviceTestRunner.processEvents` (`crates/marathon-apple/src/apple_device_test_runner.rs`, `process_events`) hands
  each event to the composite listener and awaits it before taking the next one.
- `ScreenRecordingListener.testEnded` (`crates/marathon-ios/src/listener/video.rs`, `ended`) awaits `stop()`, the
  transcode, the pull and the removal inside the callback.

The consequences:

- The results listener (`AccumulatingTestResultListener`) times a test from when its `testStarted` and its end are
  handled (Kotlin ignores the parsed start and end of a passed test), so `testButton` got 0 in its result file, JUnit
  `time="0.0"` and `raw.json` `duration: 0`. Correction to the brief: `raw.json` also has 0; the 7.7 s were the parsed
  timespan in the debug log line, not `raw.json`.
- The next recording starts in a task spawned when `testStarted` is handled. Handled after the test had ended, the
  recording was started and stopped at once (or never started), so no video.
- Even with a fast pull, the inline stop alone (3.4 s here) delays every later test's start; 0.13.0-rc.1's run showed
  the same test reported 2.3 s for a 6.1 s run.

Kotlin has the same structure (`ScreenRecordingListener.kt`, `testEnded` → `stop()` → `pullVideo()`;
`AppleDeviceTestRunner.kt`, `processEvents`), so this is a kept-Kotlin-behaviour fix and needs D80. It is recorded as
U201 in `docs/found-bugs.md`, as the brief assigned, rather than with a new A-number in `known-kotlin-bugs.md`; the row
says the Kotlin marathon shares it.

## Fix

Nothing on the event path waits for the worker at a test's end; ordering between the recorder, the transfers and the
reports is kept by tasks and one barrier.

- **Stop.** `ended` takes the test's recording job synchronously (so it cannot be confused with the next test's), clears
  `last`, and spawns the stop as its own task (`stop_recording`, the old `stop()` body). Each stop first joins the
  previous stop. The task is kept in `stopping`.
- **Next recording.** `test_started`'s start job joins `stopping` first (selecting on its own cancellation), then does
  the unchanged `await_removal` and `nohup` start. So one recording runs at a time and the recording starts when it did
  before (right after the previous stop), and the "Previous video recording didn't finish yet" warning keeps its meaning.
- **Transfers.** `PendingArtifacts` (`crates/marathon-ios/src/listener/pending.rs`) is a per-listener (so per-batch) chain:
  each item joins the previous item, then runs. A test's item joins its stop (a failed stop skips the pull, as the inline
  `?` did), then runs the unchanged transcode, pull, attachment and removal (`Media::collect`, with the same
  transfer-only catch).
- **Barrier.** `SimulatorListeners` gains `artifacts`, an `ArtifactBarrier` over the video listener's queue; it is the
  first listener of the composite, and its `after_test_run` settles the queue before `TestResultsListener` builds the
  batch's results and before `ResultBundleRunListener` pulls the xcresult. It never fails, so the results listener always
  runs.
- **Errors.** The queue keeps the first failure; the video listener's `after_test_run` (stop, then settle) returns it. A
  failure that is the batch's own cancellation is dropped, since the runner reports it.
- **Cancellation.** Items capture the batch context's cancellation (the batch job's signal); a queued item whose turn comes
  after a cancel does nothing. A running item's commands observe the same signal; a pull is ended, as before, only by its
  connection's stop (D54). Everything is awaited inside the batch's own retained job, so D74's cancelled-batch bound
  covers it as it covered the inline work.
- **Other callbacks.** `stop()` (before/after run, batch failure) joins `stopping` first. `test_run_failed` settles the
  queue before its own last-video pull, so a batch still transfers one video at a time and in test order.

## Invariants

1. One recording at a time per simulator: a recording starts only after the previous test's stop task has ended.
2. A test's stop takes only that test's start job; the next test's job is never cancelled by it.
3. A test's transfer starts only after its own stop ended; transfers of one batch run one at a time, in the order the
   tests ended (worker load per batch as before).
4. Every queued transfer has ended before the batch's results are built, before the xcresult pull, and before the batch's
   run returns.
5. No queued item starts after the batch's cancellation.
6. Xcodebuild events are handled without waiting for any worker command of the video listener.

## Verified

- New `crates/marathon-ios/tests/simulator_video_pull.rs`, whole batches through `AppleSimulatorDevice::execute` with the
  production listener factory, a fake worker modelling the recorder pidfile, and a fake xcodebuild paced by gates on the
  test clock (paused tokio time; no sleep decides an outcome — the only sleep is the second test's 7.7 s run, which
  happens in both variants):
  - `a_slow_video_pull_does_not_hold_back_the_next_test_d80`: the first pull is held until the second test has run 7.7 s
    and passed. Asserts both videos attached, both pulls done when the batch returns, test2's duration ≥ 7.7 s, and
    test2's recording started before it passed. With the fix reverted (stop and transfer inline again): fails with
    `the next test keeps its time: 0 ms`, 6 of 6 runs. With the barrier disabled: fails (test2's video missing from the
    results), so the barrier is load-bearing.
  - `a_slow_recording_stop_does_not_hold_back_the_next_test_d80`: the first stop's INT is held the same way. Asserts
    test2's duration, both videos, test2's recording started after the first stop removed the pidfile, and no
    late-stop warning. Fails with `0 ms` both with the full revert and with only the pull moved (stop inline), so it
    witnesses the stop half of the fix.
  - `a_cancelled_batch_skips_queued_video_work_d80`: test1's pull in progress, test2 ended with its item queued; the
    caller cancels. The batch returns `Cancelled` within the cancelled-batch bound, the device is not lost, and no worker
    command names test2's video after the cancel; only test1's video was pulled. This test is about the new queue and
    hangs without the fix (the second test's end is never handled), so it is not a revert witness.
- Adapted listener tests in `crates/marathon-ios/tests/ios_listeners.rs`: `video_policy_matrix_and_ignored_pull_boolean_emit_attachment`,
  `video_transfer_only_catch_and_missing_file_skip_removal`, `two_tests_keep_the_recorder_alive_until_stop_and_attach_both_videos`,
  `fast_recorder_exit_keeps_failed_stop_from_blocking_next_recording` and
  `failed_or_null_transcode_still_pulls_and_only_failed_result_logs_error` now call `settle()` before asserting, since the
  transfer runs after the callback; their expectations are unchanged. `non_transfer_failure_aborts_composite_before_next_listener`
  pinned the old abort and is renamed `non_transfer_failure_reaches_the_next_listener_and_fails_after_test_run_d80`: the
  next listener sees the test end, and `after_test_run` fails. None of these has a parity-matrix row.
- `ios_listener_world` (fake-worker media replays, exact transcode command and temp file) passes unchanged.
- Commands run: `cargo fmt --check`; `cargo clippy -p marathon-ios --all-targets -- -D warnings`; `cargo test -p marathon-ios`
  (352 passed, 0 failed); `cargo test -p marathon-cli` (see the final report for the run on the final code; the vendor
  differential `differential_vendors::full_vendor_manifests_streams_transports_and_final_state`, `ios_production` and
  `production_runner` pass on the final code after building the `snakeyaml` `yaml_tree` helper they need).
- The JVM vendor differential replays pass without any allowance: the `ios-media` scenario runs one test
  (`testAlwaysFailing`), so there is no next test for its commands to interleave with.

## Not verified

- No real-worker run. The expected effect on run 06: `testButton`'s start handled about 60 ms after `testAlwaysFailing`
  failed instead of 23 s later; its recording starting after the 3.4 s stop, as before; both videos pulled one after the
  other, the second after the first's 14.5 s pull, before the batch is reported. Worth checking in the 0.14.0 real-run
  check.
- Worker load: the first test's transcode now runs while the second test is recording. Before, it also ran while the
  second test executed (xcodebuild doesn't wait), only without the second recording. Not measured.

## Other platforms

- **macOS** (`crates/marathon-macos/src/device.rs`): its recorder listener is empty; nothing is transferred at a test's
  end. The xcresult is pulled once per batch in `after_test_run` through the artifact queue. Not affected.
- **iOS screenshots** (`crates/marathon-ios/src/listener/screenshot.rs`): the GIF is written locally by the capture job;
  the test end cancels and joins that job and attaches the file. A screenshot pull in flight when the job is cancelled is
  joined (the pull stops only with its connection); I did not check what bounds that wait. One screenshot at most, and no
  queue builds up behind it. Not changed.
- **Android** (`crates/marathon-android/src/artifact_listeners/video.rs`, `finish_test`): the same structure. `TestEnded`
  awaits the screenrecord stop, the pull of every chunk and their removal before the next instrumentation event is
  handled. Pulls go over adb, typically fast, so the effect is smaller, but a slow device link would delay the next test's
  start the same way. Separate code from the Apple runner, so not changed here; candidate for a later WP.

## Golden diffs

None. No golden or JVM recording changes; no comparator allowance.

## Incidental bugs

- The parsed start and end times of a passed Apple test are ignored (`AccumulatingTestResultListener::test_passed`, Kotlin's
  `testPassed` does the same); results are timed from when the events are handled. With D80 handling is prompt, so the
  difference is small, but any other delay on the event path would show in durations again.
- The full `cargo test -p marathon-cli` in a fresh worktree fails `differential_vendors` and `production_runner` with
  "missing YAML helper; run cargo build -q -p snakeyaml --example yaml_tree" until that example is built (the gate builds
  it). A harness convenience, not a product bug.
