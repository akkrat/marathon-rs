# X35: a cancel writes what it can, within a limit (D52, C10, U161)

The project owner approved this on 2026-11-07: a cancel always writes what it can. D52 is a new deviation in PLAN §3.11. No
configuration field, CLI option or exit code changed.

## What was wrong, checked against the code

- **C10.** `ShutdownHook.kt` installs the hook only with `debug: true`, and the port did the same, so a cancel with `debug: false`
  exited 130 or 143 and did nothing else. The row is accurate.
- **U161.** The row is accurate, though no test had reproduced it. Reproduced now: see the revert table. A batch only bounces if it
  ends after the factory has closed. A batch still in flight keeps its stream, so the fake adb's first version, a test that never
  ended, showed nothing. In the audit's scenario the window is short, because the process exits right after the reports.
  Making the provider wait longer on a cancel, as D30 can, made it matter.
- **The window at the end of a run.** `cleaned_up` is set first, so a signal during the run's own `on_finish` found nothing to do and
  the process exited mid-close. Confirmed by reverting: `a_signal_during_the_runs_own_finish_writes_the_reports_now_d52` fails.
- **Cancellation can't be the limit.** `finish_step` turns a `Cancelled` step into the first error and stops the later steps, so a
  deadline on the signal would skip the reports. The limit is an outer timer on the core clock.
- **What can stall.** The analytics close is bounded by D34's 3 s only for the history query; InfluxDB v1's close and the guard's
  `inner.close()` have no bound. The provider has D30's 30 s per session, and `close_and_join(300_000, 60_000)` for artifacts.
  A console that stopped reading blocks a flush for ever. All of these are now covered by a test or by the outer timer.

## The fix

### The order, and why

| Step | What | Bound |
|---|---|---|
| 1 | Cancel the Scheduler's job (pools, queues, devices) and the run's job | Immediate |
| 2 | Flush the console | 1 s |
| 3 | Reports and tracker close | Waited for 10 s, then keeps running while the rest starts |
| 4 | Provider terminate, history close and the usage slot, side by side | All of it: 24 s from the signal |
| 5 | Last flush | 1 s |
| 6 | `exit(128 + signal)` | |

A step runs once: `run_step` shares a started flag and a completion with the run's own `on_finish`, whichever gets there first runs
it, and the other waits. `on_finish` keeps Kotlin's order, and without a signal it does exactly what it did.

Each of the steps of 3 and 4 is its own task, so a step that doesn't return holds nothing else up. Side by side is safe because
nothing in the provider's termination uses the history, and the reverse.

### The values

| Limit | Value | Why |
|---|---|---|
| Total | 25 s (24 + 1) | A CI's grace is often 10 to 30 s, for example `docker stop`'s 10 s and Kubernetes' 30 s. The reports come first, so a shorter grace costs cleanup, not results, which makes staying under the longest the right bound. It covers D30's TERM + 20 s + KILL, which starts at step 1 |
| Reports' head start | 10 s | They take milliseconds to seconds. What can take longer comes last, a remote backend's close, after the files |
| One flush | 1 s | One write's worth of time. Stuck output loses log lines, not results |
| Vendor runtime after a normal run | 3 s | D34's allowance for a call that blocks its thread |

A CI that sends SIGINT and then SIGTERM some seconds later reaches the second signal rule, which is what it asks for: the reports
are already out.

### Stopping the actors (U161, A99)

Cancelling the detached Scheduler job stops every pool, queue and device, and it stops them before anything closes. That's the whole
of U161's fix: no batch can come back, whatever a vendor's closed client does, on any vendor. `take_failure` stays as it was.

**A99 gets better, not worse.** The race was the hook's snapshot of events against the main thread tracking the cancelled test as
FAILURE. The queue is what tracks it, and the queue is now gone before the snapshot. So a test in flight is never in the reports, which
is what Kotlin's captures show, and the 1 in 3 to 4 in 10 captures with the failure should go. Seen on a real emulator and the fake adb,
not on real macOS: the `macos-cancel` scenario should be repeated when a real capture round next runs.

The cost is that the finished tests of a batch still in flight are lost. Results reach the queue when a batch ends, so with a
batch size above 1 this is more than one test. Kotlin behaves the same way.

### Three traps found on the way

- **Reports over the last run's.** Making the hook unconditional would have let a cancel during parsing overwrite the last run's
  reports with empty ones, and made a cancelled `parse` write reports at all. The reports step needs a wiped output directory, and a
  stopped run no longer goes on to wipe it, which it did if the parser ignored cancellation and returned late. The provider is still
  terminated. With `debug: true` this differs from Kotlin, which writes them. The JVM row isn't affected, since it signals after the wipe.
- **The relay under the run.** A run that ended after the signal stopped the relay under the hook, and returned its own exit code. Now
  it waits for the hook, in production for ever, since the hook exits.
- **WARN doesn't show.** The Marathon logger shows nothing below ERROR with `debug: false`, so the line announcing the cancel is at ERROR.

### Exit after the run

`drop(vendor)` waits for every blocking call, so a stuck name lookup or D34's blocked source held the process after its reports were
written, with the relay gone and every signal ignored. `run_mode` now uses `shutdown_timeout(3 s)`. On the signal path `process::exit`
doesn't wait for any of this. In-process tests see the same, and a task that ignores the shutdown is left running until its process ends.

## Evidence

### Tests

| Test | What it shows |
|---|---|
| `signals_always_finish_reports_first_and_exit_like_the_jvm_d52` | All four JVM `signal-*` rows, status and calls, behind `matches_jvm_finish_d52` |
| `the_d52_allowance_covers_only_the_order_and_the_debug_flag` | The allowance's negative cases: the JVM's order, a missing, repeated or new call, another JVM row |
| `a_signal_during_the_runs_own_finish_writes_the_reports_now_d52` | Reports written mid-finish, and nothing twice |
| `a_provider_that_never_ends_cannot_hold_a_cancel_past_its_limit_d52` | Exit at exactly 24,000 ms on the virtual clock, reports written |
| `a_report_close_that_never_ends_does_not_keep_the_provider_from_terminating_d52` | A stuck report close leaves the provider its time |
| `a_signal_before_the_output_directory_is_wiped_writes_no_reports_d52` | Old files stay, no reports, provider terminated |
| `a_signal_terminates_the_provider_whatever_debug_says_d52` | Real signals, both debug modes, through a stub vendor |
| `a_stuck_provider_holds_a_cancel_only_up_to_the_limit_d52` | Real signal and clock: 143, no sooner than the limit |
| `a_stuck_console_does_not_hold_a_cancel_d52` | A console that stops taking output blocks neither cleanup nor exit |
| `a_second_signal_exits_at_once_d52` | Exit with the second signal's number, and no second termination |
| `a_blocked_vendor_thread_does_not_hold_the_exit_after_the_run_d52` | An hour's blocking call on a vendor thread doesn't hold the process |
| `a_cancel_mid_test_writes_the_reports_and_does_not_bounce_the_test_d52_u161` | The built binary on the fake adb: both debug modes, both signals, a finished test in the reports, the test in flight not, and it started once |
| `a_cancel_writes_reports_first_and_stops_a_slow_worker_process_d52` | The built binary on the fake worker with an `xcodebuild` that ignores TERM: reports before the KILL, TERM then KILL within the limit, 143, in both debug modes |
| `a_second_signal_leaves_the_slow_worker_process_behind_d52` | The same, with a second signal during TERM's wait: 130, and no KILL sent |

`signals_finish_only_with_debug_and_exit_like_the_jvm` was the old pin. It is renamed to the first test above; the rows are unchanged.

### Temporary reverts

Each was put back after the run.

| Taken out | Fails |
|---|---|
| `stop_execution`, with the provider closed first and a window after it, as the audit described | `a_cancel_mid_test…`: 1,002 report rows for one test with `uncompletedTestRetryQuota: 1000`, against 1 |
| The hook with `debug: false` | Three witnesses and `a_signal_terminates_the_provider_whatever_debug_says_d52` |
| Going on when `cleaned_up` is already set | `a_signal_during_the_runs_own_finish_writes_the_reports_now_d52` |
| The second signal's exit | `a_second_signal_exits_at_once_d52`: 143 at the limit instead of 130 |
| `shutdown_timeout` | `a_blocked_vendor_thread_does_not_hold_the_exit_after_the_run_d52` |
| The outer limit | `a_provider_that_never_ends_cannot_hold_a_cancel_past_its_limit_d52` |

`stop_execution` alone, on the fake worker, fails nothing: there the session's close ends the batch and the device is gone, so that
test's `started == 1` is only a check. U161 is an Android matter in practice, and the fix sits in core.

### Real emulator

The release binary of this branch on a Linux host with KVM, API 36 `google_apis` x86_64, a private adb server and a dedicated AVD. No
USB device with an ADB interface was attached. The fixture's 36-test sample APK, with default video settings. A signal 1 s after
`testTextSlow3` passed, with `testTextSlow2` (about 6 s long) in flight and `am instrument` confirmed running on the device.

| Case | Exit | Signal to exit | Results | After |
|---|---|---|---|---|
| Baseline, no signal | 1 (sample's own failures) | | 16 passed, 17 failed, 3 ignored, 250 report files | Emulator `device` |
| `debug: false`, SIGTERM | 143 | 64 ms | 13 passed, 2 failed, 0 incomplete, 15 rows, 110 report files, `testTextSlow2` absent | Emulator `device`, adb server up |
| The next run on that emulator, 5 slow tests | 0 | | 5 passed | Emulator `device` |
| `debug: true`, SIGINT | 130 | Not measured; its log ends within 1 ms of the signal's line | 13 passed, 2 failed, 0 incomplete, 15 rows, 111 report files, no `onReturnBatch` in 436 lines | Emulator `device`, adb server up |

**What a cancel leaves on the device.** `am instrument` is gone. `screenrecord` for the test in flight keeps running to its 180 s limit:
gone 185 s later. Its mp4 stays on `/sdcard` (2 after 2 cancels), as D27 says, and the app and test processes stay until the next run,
which was unaffected. The process leaving without D52 leaves the same, so this is not new. Stopping `screenrecord` would need a
request sent while closing; I left it out of scope.

## What was not verified

- A real worker and a real Mac: only the fake worker, with its SSH server, and earlier D30 measurements. Whether a process on a real
  worker dies if the process exits before KILL depends on the server, as D30 says. `macos-cancel` was not repeated.
- A real CI's kill sequence. The 10 to 30 s is the brief's figure and general knowledge, not measured.
- A wrapper's double delivery, process group plus forward, costs the cleanup and exits at once with the right status. It follows from
  the second-signal rule and has no test. I did not add a debounce: a second signal must exit at once, and the only cost is the old behaviour.
- A blocked core thread: the limit runs on it, so a future blocking core would defeat it, but it would have stopped the run too, and
  the rule is that nothing blocks core. A second signal still exits, since that runs off core, unless the vendor threads are also stuck.

## Incidental bugs

- **D51's filter warning doesn't show by default.** `No tests left to run after filtering` is logged at WARN by the Marathon logger,
  which `RunConsoleSink` registers at ERROR with `debug: false`, so a default run prints nothing and the green run stays silent. Seen by
  this WP's own first cancel line, at WARN, not appearing in a `debug: false` run. D51's tests read the log records, with `debug: true`
  or the recording console, so they don't catch it. Severity low. Not changed here, since x34's scope and D51's text are the owner's;
  moving it to ERROR is one line. Status: open.
- **`Device execution has been cancelled` and `Pool is closed…`** appear only with `debug: true`, for the same reason. That's fine; recorded so that nobody reads their absence as the path not running.
