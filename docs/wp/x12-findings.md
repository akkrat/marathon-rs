# X12: eight Android bugs fixed (D21–D28)

The project owner asked on 2026-11-06 for these fixes to bugs the port had kept for parity. Each is an approved
deviation in PLAN §3.11. No configuration field, CLI option or exit code changed.

Real emulators and farms were not available: everything below ran against the fake adb on private ports, with fake
`adb` binaries where a binary is started. No test touched the adb server on the default port or a real device.

## D21 / N12: the idle timeout, request audit

`Request::socket_idle_timeout` and `transport::idle_timeout` now count milliseconds; 0, negative values and
`Long.MAX_VALUE` mean no limit. A request bounded by its caller runs on `AndroidDebugBridgeClient::without_idle_limit`.
No limit, rather than a limit just above the timeout, is what makes "cannot fire before that watchdog" true whatever
the two values are. The timeouts below are the defaults.

| Request | How long it can be silent | Bounded by | Idle limit after D21 |
|---|---|---|---|
| `host:version` (provider setup, reconnect, D22's probe) | Moments | Nothing | `socketIdleTimeout` |
| `host:devices` (wait for devices) | Moments | `waitForDevicesTimeoutMillis` (30 s) | None |
| `host:track-devices` | For ever, by nature | Provider termination | None (`Long.MAX_VALUE`, as in Kotlin) |
| `host-serial:…:features` | Moments | Nothing | `socketIdleTimeout` |
| `getprop` during boot | Seconds on a loaded device | `boot` (30 s) | None |
| `getprop` after boot, `get_property` | Moments | Nothing | `socketIdleTimeout` |
| Shell via `safe_`/`critical_execute_shell_command`: `pm clear`, `rm`, `mkdir`, md5, `pm list packages`, `dumpsys`, `ps`, `kill`, `appops`, `pidof`, `logcat -c`, tar pulls of app data | Up to its timeout (`pm clear` and `rm -r` take long) | `shell` (20 s) | None |
| `execute_shell_command`: perfetto | Until its listener kills it | The listener's `killall perfetto`, batch cleanup | None |
| `execute_shell_command`: `echo quickhealthcheck` | Moments | 10 s `timed` in the video listener | None |
| v1 `screenrecord` | Up to 180 s (the N12 case) | `screenrecorder` (200 s) | None |
| `logcat -v long` | For ever, by nature | Device disposal | None (`Long.MAX_VALUE`, as in Kotlin) |
| `am instrument` (tests and remote parsing) | As long as a test | `testOutputTimeoutMillis`, `testBatchTimeoutMillis` | None (`Long.MAX_VALUE`, as in Kotlin) |
| `cmd`/`abb_exec` streaming install, split install | While dexopt runs | `install` (60 s) | None |
| Legacy `pm install` | While dexopt runs | `install` (60 s) | None |
| `pm uninstall` | Up to its timeout | `uninstall` | None |
| Stat and pull (sync) | Device I/O stalls | `pullFile` (30 s), in every caller | None |
| Push (sync), push folder | Device I/O stalls; the reply after `DONE` | `pushFile` in legacy install, `pushFolder`; file sync's single-file push by `testBatchTimeoutMillis` (it runs inside the batch scope) | None |
| List files (sync) | Moments | `listFiles` | None |
| Screen capture (framebuffer) | Moments | The capturer's timeout | None |
| Reverse port forward | Moments | `portForward` | None |
| Reverse port forward removal at dispose | Moments | Nothing | `socketIdleTimeout` |

So `socketIdleTimeout` now does what it says for what has no bound: a hung server cannot stall device disposal for
30,000 s. `crates/marathon-adb` has no other caller, and the other vendors do not use this client.

Kotlin's 1 "ms" lasted one second, so `client::oracle_tests::idle_timeout_unit` replays the recorded JVM rows
with 1000 ms. The old unit stays witnessed by those rows.

**Failure without it:** `silent_requests_under_a_timeout_outlast_the_idle_limit_d21` shows the same silent command
failing on the plain client with `No exit code delimiter found in `.

## D22 / N16: reconnecting

**Design.** One task per server (`AdamDeviceProvider::track_server`), sending device lists to the single loop that owns
device events. A failure on one task reaches no other. The sequence for one server:

1. The stream ends or fails. If the socket factory is closed, this is the end of the run (N19) and the task returns.
2. The server is marked away on its tracker, and `AndroidDevice::execute` waits for it before starting a batch.
3. `reconnect` retries every 1, 2, 4, 8, 8… s on the vendor clock, until `reachable` succeeds and a new stream gives its
   first list. All of this runs under one `waitForDevicesTimeoutMillis`, so a server that answers `host:version` but whose
   track-devices fails cannot extend it.
4. A settling window of `waitForDevicesTimeoutMillis` opens. Devices from before the outage count as present in the lists
   sent on, because a restarted server lists them late, first as nothing or `offline`.
5. When all of them are listed as `device`, or the window closes, the real list decides: see the reconciliation below.
   The logcat stream and reverse forwards of the devices that stay are restored, and batches continue.
6. If the server did not return in step 3, an empty list disconnects its devices, batches are released (cancelled by
   the disconnect), and reconnecting continues with no limit until termination cancels it.

**Loopback restart: yes, by the same path.** `reachable` is what initial setup calls: `host:version`, and on a connection
error from a loopback address `start_adb` and one more `host:version`. Kotlin's own comment says the loop is meant to
"survive `adb kill-server`", and its setup already starts a loopback server it finds missing. A remote server is only
retried. `start_adb` runs only the selected SDK's binary, and every test configuration audited below has a fake
one or none.

**Reconciliation, and a departure from the brief.** The brief says a listed device goes through the normal connect path
once only. That cannot work for a device that still has an actor: `pool.rs::remove_device` ends the pool when its last
device is removed, so a `Disconnected` plus `Connected` pair across an outage would end the run on a single-pool setup,
which is the very case this fix is for. The same goes for disconnecting devices when the server is lost. Sending the two
events together is a race against the old actor's `Terminated`, and changing the pool would change every vendor. So:

| Device after the reconnect | Goes through | Actors |
|---|---|---|
| Listed as `device`, had finished setup | No event; logcat and reverse forwards restored | Keeps its one actor |
| Listed as `device`, setup interrupted by the outage (never had an actor) | Normal disconnect, then normal connect | One new actor |
| New to the list | Normal connect, once | One |
| Not listed when the window closes | Normal disconnect | Its actor terminates |

**Keeping the device alive through the outage needs two more things.** Batches wait in `AndroidDevice::execute`, or
they would fail in milliseconds and burn `uncompletedTestRetryQuota` at full speed until the server returned. And a
`DeviceLost` from a batch while the server does not answer `host:version` becomes `BatchExecution`, so that a request
caught at the moment of the kill (`RequestRejected`, N15) does not retire the last device of a pool. This probe adds a
`host:version` only on a device loss, and no comparison showed it.

**Termination.**

| Case | Outcome |
|---|---|
| Server never returns | Devices disconnected after `waitForDevicesTimeoutMillis`; the pool ends with them; the run reports failure (`a_run_ends_when_its_adb_server_never_returns_d22`). With no bound, as an experiment reverting D22, the run did not end within the harness's 400 virtual seconds, retrying under the default unlimited quota |
| "noDevicesTimeout" | No such setting exists. `deviceInitializationTimeoutMillis` applies only until the first pool, unchanged; `waitForDevicesTimeoutMillis` is this bound |
| Provider termination | Cancels the tasks through their signals; the retry loop ends (`a_server_that_never_returns_loses_only_its_own_devices_d22`) |
| Queue finishes during an outage | The queue terminates the actors as usual, cancelling a batch waiting at the gate |

The test of this exposed a real deadlock, now fixed: a task waiting for the event loop's answer while that loop joined
the task at termination. The wait is now cancellable (`handled`).

**Several servers (D18).** Tested in `a_server_that_never_returns_loses_only_its_own_devices_d22`: the healthy server's
stream is never interrupted and goes on driving events.

**Proof by temporary revert.** Ending tracking at the stream's end failed the five provider tests then written. With the wait in `execute` removed as well, both whole-run tests failed. Removing that wait alone failed `a_batch_waits_for_a_lost_server_d22`.

## D23 / N5

Bound: `timeoutConfiguration.pushFile`, for every caller. Legacy install wraps the push in the same timeout started
earlier, so it fires first and keeps `Timeout transferring`. A file sync push previously had only the batch timeout, so
that path now fails sooner, with `TransferException`, which the actor treats as a recoverable batch failure.

## D24 / N27 retention, N29: readers traced

| Reader | Reads | Effect |
|---|---|---|
| `AdamDeviceProvider::borrow` (only caller: `extract_remotely`, under `deviceInitializationTimeoutMillis`) | Map | Waits for a live device; ends in the existing `NoDevices` timeout if none comes. It used to return a disposed device and fail on it |
| Scheduler, pool, actor | `serial_number` only | None |
| Pooling strategies | Model, manufacturer, OS | None |
| Queue, progress | `DeviceInfo` as an opaque value | None |
| Test results (`TestResultsListener`, uncompleted results): `test_result/**.json` | `networkState`, `healthy` | **Changes for a device lost by then:** `networkState` reads `DISCONNECTED`. `healthy` was already false, except for an old object after its serial returned, which now stays false |
| `devices/*.json` | `DeviceConnectedEvent`, captured at connect | Unchanged, `CONNECTED` |
| `bill/*.json` | `DeviceConnectedEvent`, captured at connect | Unchanged |
| HTML report | Serial number | None |

A healthy run's output does not change: the differential suites compare `networkState` and still pass. A frozen CONNECTED
was indeed reaching output files.

`healthy` now shares the check with `networkState` (`is_live`). Kept: **the event half of N27** (Disconnected is emitted
even if setup never emitted Connected), as it is not a consequence of this fix.

**Proof by temporary revert:** `multi_server_disconnect_keeps_identity_and_borrow_skips_the_lost_device_d24` fails.

## D25 / N38

| Failure | Today (Kotlin and the port before) | After |
|---|---|---|
| Parser or transformer exception (`NumberFormat`, malformed atom) | Vanishes: looks like a normal end; tests uncompleted with the generic text | `TestRunFailed` with cause, logged; uncompleted with that text; device kept |
| Rejected handshake (`host:transport`, EOF before OKAY, N15) | Vanishes likewise | Same, then the error is returned: `execution_error` makes it `DeviceLost`, as for a synchronous request; while the server is away, D22 fails the batch only |
| Connection refused, other I/O | Vanishes likewise | `TestRunFailed` with cause, logged; device kept, as a synchronous I/O error keeps it |
| Client closed by provider shutdown (N19) | Quiet | Quiet: the factory is closed, so `take_failure` returns nothing |
| Cancellation of the batch or the channel | Quiet | Quiet: not recorded |
| Test crash, `Process crashed`, non-zero `INSTRUMENTATION_CODE`, a stream that ends early | Handled by the transformer, no producer failure | Unchanged at first. WP x18 made a stream that ends without `INSTRUMENTATION_CODE` a failure as well |
| Remote parsing, any of the failing rows | Attempt "succeeds" with the tests found so far | `TestParsingException` with cause; the existing retries apply. Not named by the brief's listener wording, but the ledger row covers `AmInstrumentTestParser`, and silently parsing fewer tests is the worst form of this bug |

The message is `Instrumentation failed: <class>: <message>`. The synchronous path's `adb error while running tests [...]`
is unchanged. The JVM rows `runner:handshake-eof-events` and `runner:closed-client-events` stay as witnesses.

**Proof by temporary revert:** four runner tests and the parser test fail.

## D26 / N10

- **040:** only allows more characters in simple names (Unicode `Zs`), which the parser reads as it reads any name. A
  space in a method name was already escaped in `prepare_request`.
- **041:** per the [format documentation](https://source.android.com/docs/core/runtime/dex-format), the header is 0x78
  bytes with `container_size` and `header_offset`; `file_size` is the distance to the next header; `data_size` and
  `data_off` are unused; and **all offsets are relative to the file**, not the header. `DexFile::parse_all` therefore walks the headers and gives each image its own tables over the shared bytes.
- The documentation calls 041 experimental in Android 16.

**Fixtures.** Stable d8 does not emit 040 or 041 by itself: for every `--min-api` tried up to 40 it stops at 039, on
build-tools 35.0.0, 36.1.0 and 37.0.0. The jar has two undocumented system properties,
`com.android.tools.r8.dexVersion40ForApiLevel30` and `com.android.tools.r8.dexContainerExperiment`. The fixtures are d8's
unedited output with them; a two-image container comes from `--main-dex-rules` at `--min-api 20`, for which d8 warns
that it forces the format. The recipe is `tools/oracle/dex/build-dex-versions.sh`, and `fixtures/PROVENANCE.md` has the commands.

**Check:** the tests found, with annotations, equal those of the same sources in 039, for 040, 041 and the container,
in which two of the test classes are in the second image. A malformed container (bad header size, zero or oversized
`file_size`, wrong `container_size` or `header_offset`, a later image of another version, truncation) is a `DexException`.
The JVM row for `probe-dex040.apk` is the single row of `differential.rs` that is now treated differently.

## D27 / N4: what the wipe was cleaning

| Marathon's leftover on external storage | Name knowable by a later run? | After D27 |
|---|---|---|
| File sync pull directories, including Allure's | Yes | Removed and recreated before every batch, as before |
| File sync pushes | Yes | Replaced on push and removed after the batch, as before. After an aborted run, a file removed from the configuration stays |
| Videos (`<test>-<batch id>[-<chunk>].mp4`) of finished tests | Yes | Removed after each test, as before |
| **Videos of an aborted run** | **No: the name holds that run's random batch id** | **Accumulate.** Removing them would take a pattern over the storage root, which could hit user data, so they stay, and this is documented |

Not on external storage, and so never covered by the wipe: coverage files (app data), perfetto traces, `/data/local/tmp`.
No removal command was added.

`RemoteFileManager::remove_remote_directory` and `create_remote_directory(None)` remain, as ports with their ported tests;
production no longer calls the former. The fixture scripts for reconnection hang a device in `rm`, so the
harness moves that hold in memory to `logcat -c`, the next step of prepare. Since `logcat -c` only logs a failure, a device can
finish prepare while its Disconnected is on the way, and one batch can start on it and be returned; the harness does not
count such a batch.

## D28 / N35

| Question | Decision |
|---|---|
| `testBatchTimeoutMillis` | Per instrumentation, as Kotlin's loop has it and as a single-info batch has it. A batch of N can therefore take N timeouts |
| `testOutputTimeoutMillis` | Per receive, unchanged |
| Clearing data | Before each instrumentation, for its own packages. Two instrumentations of one application clear it twice, so the second cannot see the first's app state, as in Kotlin |
| Ignored callbacks | Once, in the first instrumentation's scope, where a single-info batch has them |
| `beforeTestRun` | Once, with the first instrumentation that reaches it (one can time out while its data is cleared). The brief names only ignored callbacks and `afterTestRun`, but the log listener registers a line listener in `beforeTestRun` and removes one in `afterTestRun`, so a pair is needed, and file sync's would wipe the pull directories before the single pull |
| New internal callback | `before_next_instrumentation_in`, for file sync to prepare and later collect each application's app data. `afterTestRun` runs once, so it pulls every package |
| A run failure, stuck output or batch timeout in one | Recorded; the next instrumentation runs |
| A lost device, an error, cancellation in one | The batch ends: one `afterTestRun`, the rest uncompleted, the error returned |
| Failure text | `TestRunResultsAccumulator::start_instrumentation` (a small addition to core, not used by other vendors): a later run's start would otherwise erase why an earlier one failed. Each instrumentation's failure is the text of its own tests (WP x20, U151). Until then the latest failure won |

The trap in the listener: results were built at the first `afterTestRun`, when a later partition's tests were still
missing, which would have counted them uncompleted. Now that is called once, after all ran.

Only batches mixing instrumentations are affected, and every differential case has one instrumentation.

**Proof by temporary revert:** four of the D28 runner tests fail.

## Comparators

| Deviation | Allowance |
|---|---|
| D27 | `vendor_transport.adb`: exactly `rm -r /sdcard` and `mkdir /sdcard`, only in JVM records and native records pinned from before D27 (`evidence.may_withhold`, the D20 scope). Negative tests: `test_d27_*`. `scenarios_test.rs`: `D27_WIPE`, applied to recordings only, negative test `the_d27_allowance_covers_only_the_root_wipe` |
| D26 | `differential.rs`: exactly the row `probe-dex040.apk`, whose recorded error stays asserted |
| D21 | Oracle tests replay the 1 "ms" rows with 1000 ms |
| D27 | `reconnect_test.rs::ran_on`: at most one batch per dropped device may start between its drop and its reconnect, and is not counted; more fail the comparison |
| D22, D23, D24, D25, D28 | None needed: the recordings hold no such event. JVM rows stay asserted where they were read |

No recording was changed. `tools/differential/registry.json` was regenerated by `make_registry.py`, as the evidence and
transport modules' digests changed.

## Test safety

Every path that can reach `start_adb` was checked. `tests/support/mod.rs` and `reconnect_test.rs` set `androidSdk:
/nonexistent`, so nothing is started. `provider_test.rs` builds its own fake `adb` where it needs one. The differential uses
its own `./sdk`, and its fake outlives the run. `run_outcome.rs` does not run the provider. `real_adb.rs` is existing, and
drives only the fake on a private port. The ANDROID_HOME fallback in `start_adb` is not reached by any test.

A fake server that is killed listens on the same port again, and a provider retries a port whose server never returned, so the tests that kill a server hold a lock exclusively (`docs/testing.md`, rule 3). Before this, a test sharing the binary could take the port: `track_eof_resumes_tracking_and_keeps_a_listed_device_d22` failed once in eight runs. After it, `provider_test` passed 20 of 20 and `reconnect_test` 6 of 6 on this Mac.

## What was not verified

No real emulator, device or farm. On a real emulator, the orchestrator should check:

| Fix | Check |
|---|---|
| D21 | A video test on API < 34 (a v1 silent `screenrecord`); `pm clear` and installs of a large app; a perfetto run; default `socketIdleTimeout` |
| D22 | `adb kill-server` during a test, between batches and during prepare: the run finishes, once on a single emulator. How long a real server takes to list emulators as `device` against the 30 s window; whether reverse forwards return; a server that never returns ends the run; duplicate logcat lines from a re-read buffer |
| D23 | A file sync push with verification on a slow device |
| D24 | Remote parsing while a device is lost; `networkState` in a lost device's JSON |
| D25 | Pulling the USB cable or killing an emulator mid-run: a real EOF's route (a stream that ends early versus a failure) |
| D26 | A real app built with a current toolchain; real 041 output only exists behind experimental flags, so this should be rechecked if AGP ships it |
| D27 | `/sdcard` keeps a marker file across runs; `mkdir` on nested file sync paths, whose parents the wipe never created either; a real device's `rm -r` of a missing pull directory |
| D28 | A batch across two test APKs, including coverage and Allure |

The Linux gate was not run: the orchestrator runs `scripts/remote-check.sh`.

## Review

A cross-family review of `92854034` found no false-green, lost-test or unbounded-hang path. Its findings:

- **Fixed: a fast server restart could retire the last device.** A batch's device loss was judged only by probing `host:version`, so a server that had already returned made it a real device loss, and the run ended. The tracker's flag now counts too, until the list has settled. `device_loss_counts_only_while_the_server_is_there_d22` covers it and fails without the fix.
- **Fixed: giving up on a server released waiting batches before the disconnects landed,** so they failed on the dead server and used retry quota. The flag is now set after the provider's loop has handled the disconnects (devices disposed, events queued); a released batch can still race the pool's removal, once per device actor.
- **Fixed: the `ran_on` allowance skipped any number of batches.** Now one per dropped device.
- **Documented, not fixed: an emulator that itself restarted during an outage keeps its actor without being prepared again.** It cannot be told apart from a plain server restart, and no test covers it. A boot-identity check on resume would be a follow-up.
- **Accepted, then fixed by WP x20: a first partition that times out before `beforeTestRun` loses its failure text** (D28). Its tests still counted as uncompleted and were retried, with the generic message. They now have its text (U151).
- **Accepted: a crafted DEX 041 container costs O(images × file) in table allocation.** No panic or overflow; each image's tables are bounded by the file rather than by its own `file_size`. The input is the user's own test APK.

A second review of `0a9b1266` tried to break the fixes with throwaway tests, on the fake adb. Nothing blocked:

- **D28 with D25:** 12 scenarios over three instrumentations through the real runner and listener, with exactly one final state per test and none reported passed that did not run. A single-instrumentation batch against `d4d842cb`: 16 of 18 cases identical, and the other two are D25's intended changes.
- **D22:** whole runs through the core scheduler: flapping, a device `offline` first or for ever, two servers sharing a serial with overlapping outages, kills at 11 points, and servers that never return. Every run ended, with no duplicate actors and no test requested twice.
- **D21:** a proxy silencing the fake server confirmed the audit for the kinds it reached.
- **D26:** 39,444 mutated inputs over the four fixtures, with no panic or hang and no large allocation.
- **Found:** a test that a later instrumentation did not report can show an earlier instrumentation's failure text (U151). Only the text is wrong. Fixed by WP x20.
- **Reasoning only:** remote test parsing has no D22 gate and retries three times without delay, so an outage landing exactly there could use all its attempts. Kills at 50–600 virtual ms with a restart all passed; the case was not produced.
- **Not reached:** the D24 races (disconnect during `borrow`, setup or a handed-out batch; repeated reconnects); a never-matching md5 through legacy install and file sync; cancellation while polling; prepare over stale files; silent-server runs for streaming, split and legacy install, `host:devices`, and `getprop` during boot; provider termination in the middle of an outage at whole-run level; flapping with a low `uncompletedTestRetryQuota`.

## Real emulator checks

The native release binary of `d744b672`, on a Linux host with KVM, on 2026-11-06, about 50 minutes of machine time. Android emulator 37.1.11, adb 1.0.41, a private adb server on a high port, no physical device (USB guard passed).

| Image | API | `screenrecord` path | Used for |
|---|---|---|---|
| `google_apis` x86_64, Android 16 | 36 | v2, one file per test | D22, D24, D25, a video run |
| `default` x86_64, Android 13 | 33 | v1, 180 s chunks | D21, D24, D25, D26, D27, D28 |

The sample app was built from the frozen source, with one added test class that logs a line per second for 5 to 200 s and prints nothing to instrumentation. D28's second test APK comes from the same sample with another test package.

**One bug found (the first row below), not a regression, since fixed by WP x18 (see "Re-check after the fix"):** an outage during remote test parsing can give a green run with tests missing. Everything else held, apart from what is listed as not checked.

| Check | Outcome | Evidence |
|---|---|---|
| **D25 / N38: stream ending during remote parsing** | **BUG.** A real EOF is not a producer failure, so `remote_parser.rs` takes `take_failure() == None`, leaves its loop and returns the tests found so far. D25's retry never runs. See below | Of 11 kills around parsing: 8 runs ended at once with `NoTestCasesFoundException`, exit 1; **1 run parsed 1 of 67 tests, scheduled 0 and exited 0**; 2 came too late to cut the list short |
| D22: real server's restart | **Well inside the 30 s window** | `kill-server`, then `start-server` by hand, 3 times: `device` in the first list, 0.45 s. Under marathon, from gone to `device`: 1.30–1.57 s over 6 runs, 1 s of it the backoff. `offline` never seen |
| D22 (a): kill during a test | Pass, exit 0 | `sleep20a` INCOMPLETE at 8.06 s, then PASSED in 20.01 s after the restart; other tests unaffected; 1 incomplete, quota 3 |
| D22 (b): kill between batches | Pass, exit 0 | Killed on the line reporting the previous test. The next batch had just started: INCOMPLETE at 38 ms, retried, PASSED. 3 of 3 passed |
| D22 (c): kill during prepare | Pass, exit 0, at each of 3 points | During the app's install and the test APK's: `InstallException` logged, the device connected again, the install ran again. Just after installation: `Could not clear logcat` logged, and the run went on. No test used quota |
| D22: kill during a video | Pass, exit 0 | `Unable to start screenrecord` logged, test retried, a video for each passed test. One `.mp4` stayed in `/sdcard`: see below |
| D22: a retried test really runs | Yes | 20 of 20 log lines and the full duration in the retry's logcat file |
| D22: logcat after the restart | Resumes, no duplicates | 0 duplicated lines in any per-test file of any kill run. A per-test file can lack the test's first line, also in fault-free runs (44 of 45), so that is attribution at the test's start and not the outage |
| D22: quota | Holds | With quota 0: one FAILURE and no retry, exit 1 |
| D22 (d): server cannot return | Pass, exit 1, no hang | `androidSdk` with no `adb`, and with a fake `adb` exiting 0: run ends 30.0 s after the kill with `did not return in 30000 ms; disconnecting its devices`, total 34.4 s |
| D21: silent v1 `screenrecord` on API 33 | **Pass: the known risk does not occur** | Silent 200 s test, default `socketIdleTimeout`: first chunk `exit code=0` after 180.3 s, second after 19.9 s, both pulled, test PASSED |
| D21: API 36 video, silent tests | Pass | 45 s and 70 s silent tests PASSED with recordings of about that length. `FilePushTest#testExternalFolderPresent` fails as the sample always has (7.2) |
| D21: `pm clear` | Pass | Marker in app data gone after a run with it, present after a control run without it |
| D21: install | Pass | Sample APKs 0.1–0.2 s. 505 MB APK 2.5 s: the disk here is too fast to make the install silent for long, so this does not show a silence over 30 s |
| D21: perfetto | Pass | API 33, 45 s test: trace written (1,187,402 bytes) and pulled as `.profile` |
| D24: `networkState` | Pass | Emulator killed with quota 0 and a second emulator: the lost device's `test_result` JSON reads `DISCONNECTED`, `healthy: false`; the survivor's reads `CONNECTED`. With retries left, no JSON is written for the lost device, as the retry's result is the final one |
| D25: killing an emulator mid-test | Run behaves, text absent | With a second emulator: disconnected within 60 ms, test retried on the survivor, 6 of 6 passed, exit 0. See the route below |
| D27: marker in `/sdcard` | Pass | Marker and a nested file survive two runs; both exit 0; 3 videos each, none stale; no video left on the device |
| D27: sync paths | Pass | A nested pull directory or one that cannot be created (external storage, local tmp, app data) gives an empty local folder and a passing run |
| D26: real APKs | Pass | The real test APK re-dexed by stable d8 8.10.9 (build-tools 36.0.0), to 039, 040 and 041, the last two by its system properties. `marathon parse` lists the same tests for the original (035) and all three |
| D26: real 040 run | Pass, exit 0 | The 040 APK on API 33: 2 of 2 passed. 041 was parsed only |
| D23: normal pushes | Pass | Pushes with verification `synced in` 21–28 ms. A never-matching md5 was not forced |
| D28: a real mixed batch | Pass | One `executeBatch`, two `am instrument`, each with its own tests; 3 passed and the intended failure failed, exit 1; the test APK identifies itself correctly in a test |

**The false green.** `AmInstrumentTestParser` needs only a closed channel to call an attempt finished. D25 turned a *failed* producer into a parse failure, but none of the routes below is one, as a dropped socket ends the shell stream normally. So when the connection goes during parsing:

- No test reported yet: 0 tests, `NoTestCasesFoundException`, exit 1, with no retry although the D22 restart succeeds moments later.
- Some tests reported: a partial list, taken as complete. The run seen had a filter that matched none of them, so it scheduled nothing and exited 0, printing only `Parsed 1 tests`. Without a filter it would be expected to run that part only; this was not run.
- An emulator killed during parsing, with another healthy one: exit 1 after 1.3 s, and the other device is not tried. Killing `am` on the device: the same.

The window is the parse at the start of a run, about 2 s here, and only with the remote parser. Kotlin has the same behaviour (N38), so this is not a regression, but D25's statement that an attempt "fails, and is retried, instead of returning only the tests found so far" holds for fewer cases than it says. A fix would treat a stream with no instrumentation result as a failed attempt.

**The route of a real EOF, D25's question.** `Instrumentation failed` appeared in none of the runs.

| Fault | Route | Text a final failure carries |
|---|---|---|
| Emulator killed | `host:track-devices` reports it first, the disconnect cancels the batch | None: `<failure></failure>` |
| adbd killed in the emulator | The same route | None seen: the test stayed incomplete, as the run ended |
| adb server killed during a test | Stream ends early | None |
| adb server killed as a batch starts | Stream ends early, no test started | `No test results` |
| `am` killed on the device | Stream ends early | None |

So on real faults the report does not show the cause; the log does (`Lost device tracking…`, `changed state to DISCONNECTED`). This matches D25's row "a stream that ends early: unchanged", and no test is lost, but the failure text stays as uninformative as in Kotlin for these faults. D25's text is for rejected handshakes and refused connections, which the D22 gate makes rare; none occurred here.

**Re-check after the fix (WP x18).** The native release binary of the x18 branch head, the same way: same emulator, adb and images,
on a private adb port, USB guard passed, about an hour of machine time including builds. API 36 alone unless stated; this sample has 63
tests, as the added class is smaller. Each parsing run has a filter for two tests, as the run that gave the false green had.

| Check | Runs | Outcome |
|---|---|---|
| **`adb kill-server` around parsing, the same points and more** | 15 | **No run exited 0 with tests missing.** All exit 0, 63 of 63 parsed, both tests passed. 11 cut the stream: one failed attempt, retried once the server was back, parse finished 3.2–4.2 s after the kill. 1 came during install, where the installer's own retry recovered. 3 came after parsing had finished |
| Two kills in one parse, the second in the next attempt | 2 | Exit 0, 63 of 63, in about 8 s. Two failed attempts, then success |
| Server back after a kill, over all these runs | 20 outages | 1.36–1.47 s from `Lost device tracking` to `is back`, inside the wait |
| `am` killed on the device during parsing: before any output, and during the listing | 2 | Exit 0, 63 of 63, one failed attempt each. Before the fix: `NoTestCasesFoundException` |
| **Emulator killed during parsing, with a healthy second** | 1 | Exit 0. Three failed attempts on the dead one, about 2 s each, then 63 of 63 on API 33 and both tests passed. Before the fix: exit 1 at 1.3 s, the other not tried |
| Server that cannot return, during parsing (adb hidden after the kill; `deviceInitializationTimeoutMillis` 60 s) | 1 | Exit 1, no hang. `did not return in 30000 ms` at 31.9 s, two more attempts fail, `NoDevicesException: Timed out waiting for a temporary device…` at 60.0 s |
| Kill during a test, quota 3 | 1 | Exit 0. `sleep20a` INCOMPLETE, then PASSED in the retry; the other test passed |
| Kill during a test, quota 0: **the report's text** | 1 | Exit 1. `Instrumentation failed: java.io.EOFException: the instrumentation stream ended before a result` in the JUnit `<failure>` and the JSON. Before: empty |
| Kill as a batch starts, quota 0 | 1 | Exit 1, the same text. Before: `No test results` |
| Kill with a finished test in the batch, quota 0 | 1 | Exit 1. `sleep05a` stays PASSED; `sleep20a` FAILURE with the text |
| `am` killed during a test, quota 0 | 1 | Exit 1, the text with `, with exit code 137` |
| Crash, quota 0 (control) | 1 | Exit 1, FAILURE with an empty text, as before and as in Kotlin |
| Failing test (control) | 1 | Exit 1, its stack trace; the other test passed |
| **Normal runs: remote parsing** | 2 | Exit 0, 63 of 63, no failed attempt |
| **Normal runs: a batch** | 2 | Exit 0, 3 of 3 |

Raw `am instrument` ends on API 33 and 36 are in `docs/wp/x18-findings.md`. Not rerun: everything else in this section, which x18 does not touch.

**Other observations.**

- **A device going `offline` for a moment ends a run on its last device.** adbd killed in the emulator: `offline`, disconnected at once, run over with exit 1 at 7.5 s, the device back as `device` within 2 s. D22 covers the server going away, not this, so it is unchanged.
- **Plain `fixed-size` batching never mixes test APKs**, as it locks a batch to one bundle: two APKs gave two ordinary batches. A mixed batch needed `class-name` batching and one class name in both APKs, so D28's fix applies to few configurations.
- **Coverage of a mixed batch keeps the last instrumentation's file only:** 2,809 bytes against 11,928 for the first APK alone, the case listed under "Incidental bugs".
- **D27 leaves an interrupted test's video on external storage, in normal runs too.** After the kill during a video, one `.mp4` stayed in `/sdcard` at the end of a run that exited 0, presumably the interrupted attempt's; its name was not recorded. The names hold a random batch id, so each interruption can add one and no later run removes it. The deviation documents this for aborted runs only. Not a failure, and small.
- **A pull directory sharing its name with a pushed folder.** In this configuration the pulled file is at `external-folder/external-folder/external-file-2`, which suggests the pushed folder lands one level deeper because the pull directory exists first, and so `testExternalFolderPresent` fails; 7.2 saw that failure on both sides. The layout on the device was not inspected.
- **Once a freshly booted API 36 emulator answered `cmd: Can't find service: package`** during install; setup's retry recovered in about 12 s and the run passed. The previous run on it had been the invalid two-serial run below, so this says little.

**Not checked.**

| Item | Why |
|---|---|
| D22: reverse forwards returning | Emulators with an `emulator-` serial never get one (`is_local_emulator`). A device reached by `adb connect` does, but an emulator re-registers with a restarted server under its own serial as well, so one VM became two devices; that one run was invalid (`Process crashed`) and proves nothing. Separating them means an emulator with another adb port, outside the rules for this check. A physical device or a TCP device on a farm is the way |
| D23: a never-matching md5 | Hard to force on a real device, as expected |
| D21: an install silent for over 30 s | The storage is too fast; a 505 MB APK took 2.5 s |
| Aborted runs leaving videos (D27) | Documented behaviour; not exercised |
| D28 with a failing or timed-out instrumentation, D28 with Allure pulled from two applications | The batch held one application. The Allure pull ran and results were written for all tests |
| An emulator that restarts during an outage | Known and documented in the review above |
| JVM comparison | Not needed for any answer. The statements on Kotlin come from N38 and 7.2 |

## Incidental bugs

Our own code: U151 in `docs/found-bugs.md`, from the review above, and the remote parsing false green under "Real emulator checks", which is N38 not fully covered by D25: U152, fixed by WP x18. Kotlin behaviour seen nearby and left alone:

- **Coverage of two instrumentations of one application.** `coverage-<batch>.ec` has the same path, so the second overwrites the first. D28 makes this no worse than before, and it is rare.
- **Profiling with several instrumentations.** `ProfilingRunListener` takes `pidof` from the bundle of the batch's first test, so a later instrumentation's trace targets the wrong package.
- **`logcat -c` swallows its failure** (`safe_execute_shell_command`), so prepare can succeed on a device that has just gone. D27 makes this the last step of prepare, so the case is visible; it existed before.
- **The remote-parser Kotlin messages.** N26's wording `Timeout transferring` for an install step remains, as the brief keeps it.
