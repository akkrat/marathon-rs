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
| Test crash, `Process crashed`, non-zero `INSTRUMENTATION_CODE`, a stream that ends early | Handled by the transformer, no producer failure | Unchanged |
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
| Failure text | `TestRunResultsAccumulator::keep_run_failure` (a small addition to core, not used by other vendors): a later run's start would otherwise erase why an earlier one failed. The latest failure wins if there are several |

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

## Incidental bugs

No new bug in our own code outside this work. Kotlin behaviour seen nearby and left alone:

- **Coverage of two instrumentations of one application.** `coverage-<batch>.ec` has the same path, so the second overwrites the first. D28 makes this no worse than before, and it is rare.
- **Profiling with several instrumentations.** `ProfilingRunListener` takes `pidof` from the bundle of the batch's first test, so a later instrumentation's trace targets the wrong package.
- **`logcat -c` swallows its failure** (`safe_execute_shell_command`), so prepare can succeed on a device that has just gone. D27 makes this the last step of prepare, so the case is visible; it existed before.
- **The remote-parser Kotlin messages.** N26's wording `Timeout transferring` for an install step remains, as the brief keeps it.
