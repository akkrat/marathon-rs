# X41: real emulator checks of the Android changes since the last real run

The Android changes merged after the previous real-emulator check, checked on a real emulator at main `7731351f`:
D48, D49, D52 with U161, D27's folder push with U159 and N41 as revised in x37, D25's stream-end rule as revised in x37,
and D41. Verification only; no code changed.

## Setup

The native release binary built from `7731351f` on a Linux host with KVM, a private adb server on a high port started for
this work only, and one private emulator with its own AVD. The USB guard from `tools/real-e2e/worker.py` passed before
anything started. No gate held the host's lock and no other adb, emulator or build was running during the runs.

- Android 16 (API 36), `google_apis` x86_64 system image revision 7, 2 GB RAM, cold boot without snapshots.
- Emulator 37.1.11, adb 1.0.41.
- The APKs are the fixture sample's (`fixtures/vendor-android/apk/app-debug.apk` and `app-debug-androidTest.apk`), not
  rebuilt. Unless stated, the tests are `MainActivitySlowTest`: 5 tests of about 5.4 s, one to a batch, local parser,
  `uncompletedTestRetryQuota: 3`, `deviceInitializationTimeoutMillis: 60000` (lowered from 180 s to keep the waits short),
  `debug: true`.
- A run without a fault: exit 0, 37.6 s, 5 of 5.

## Results

| # | Check | Outcome | Evidence |
|---|---|---|---|
| 1a | D48: `adb reboot` during the second test | **Exit 0**, 60.4 s, 5 of 5, 1 `INCOMPLETE` | `Offline`, then `returned as if restarted` 7.9 s later; `no device left, waiting up to 52131 ms`; `has arrived` 8.4 s after that |
| 1b | D48: emulator process killed (SIGKILL) during the second test, nothing returns, limit 60 s | **Exit 1**, 70.7 s. 1 passed, 1 `INCOMPLETE`, 3 never ran | Disconnected 3.25 s after `Offline`, `waiting up to 56748 ms`; `no device returned within deviceInitializationTimeoutMillis (60000 ms)` exactly 60.00 s after `Offline`, then `remaining tests will not run` |
| 1c | D48: emulator killed, the same AVD started again 5 s later | **Exit 0**, 68.6 s, 5 of 5, 1 `INCOMPLETE` | Disconnected 3.25 s after `Offline`; the new emulator `has arrived` 23.9 s after `Offline`, within the limit |
| 2a | D49: 2 `am instrument` connections closed before their reply, by a TCP proxy between marathon and the adb server | **Exit 0**, 39.6 s, 5 of 5, 2 `INCOMPLETE` | `Batch failed because a request was lost, while the device is listed and answers; keeping the device` twice; no disconnect, one actor |
| 2b | D49: every `am instrument` closed, quota 10, limit 20 s | Exit 1, 22.3 s, 4 `INCOMPLETE`, no loop | 4 requests closed; 3 kept; the fourth gave `Device emulator-<n> lost 3 requests already; not keeping it` 2.3 s into the run; D48's 20 s wait, no return |
| 3a | D52: SIGTERM during the second test, `debug: false` | **Exit 143** 11 ms after the signal | The ERROR line naming signal 15. Reports written: JUnit (1 test case), per-test JSON and `raw.json`, HTML, Allure results, bill, devices, logs. 1 passed; the test in flight is absent. Device logcat: the in-flight instrumentation ran to its own end and no `run started` came after the signal |
| 3b | D52: SIGINT during the second test, `debug: false` | **Exit 130** 20 ms after the signal | As 3a, with signal 2 |
| 3c | D52: SIGTERM, then SIGINT 1 ms later | **Exit 130** 11 ms after the pair | The signal 15 line, then an immediate exit: JUnit, per-test JSON, bill and devices were written, the HTML and Allure output and the summary were not |
| 3d | D52: SIGTERM and SIGINT sent back to back | Exit 143 in 9 ms, no line, no reports | Linux delivers the pending SIGINT first, so it was the first signal and SIGTERM the second, which exited at once with its own number |
| 3e | D52: SIGTERM, then SIGINT 5 ms later | Exit 143, full reports | The first signal's cleanup was already done; without a stuck worker it takes a few milliseconds |
| 4 | x37, U159, N41: a pushed folder `payload data`, a file and a nested file in it; siblings `payload/` and `data/` beside it on the device, and a stale file inside the destination | **Exit 0** twice, 5 of 5 each, 5 pushes each | During a test: the stale files gone, the fresh content and the nested file present, both siblings intact. After each run: the destination removed, both siblings intact. The second run, with new content, a new stale file and a stale nested file, read the new content |
| 5 | x18 as revised in x37: `marathon parse` and a whole-sample run | Parse: 42 tests (remote parser) and 36 (local, parameterised tests not expanded), exit 0. Run with the remote parser, batches of 10: exit 1, 64.2 s, all 42 tests ran, 22 passed, 17 failed, 3 ignored, 0 `INCOMPLETE` | The remote list is the same set as the device's own `am instrument -r -e log true` (42 of 42). No `testRunFailed`, no `ended before a result`, nothing reported as a cut stream. The 17 failures are the sample's own: always-failing, flaky and test-access tests (test access off) |
| 6 | D41: `adb reconnect` during the second test | **Exit 0**, 41.5 s, 5 of 5, 1 `INCOMPLETE` | `Offline`, `is back` 0.35 s later, no disconnect; the next batch ran on the same actor |

After every signal the emulator stayed listed as `device`, and the adb server was the same process from the first run to the
last.

## Notes on the runs

- **An inherited ignored SIGINT stays ignored.** The first SIGINT attempt ran to the end with exit 0, as did a second with
  job control on: a process started in the background of a non-interactive shell inherits SIGINT ignored, and bash can't
  undo that. The binary then installs no SIGINT handler, deliberately, as HotSpot does
  (`crates/marathon-core/src/marathon.rs`). 3b ran with SIGINT reset to the default before the exec. Not a bug, but a
  harness that sends SIGINT to a backgrounded marathon gets no cancel, with the JVM as with the native build.
- **The first whole-sample run hit an emulator fault.** Android's own system_server watchdog fired during a batch
  (`WAITED_UNTIL_PRE_WATCHDOG`, then `DeadSystemException: The system died`). One test got stuck for the 60 s output
  limit, and the following batches ended within 0.5 s with `No test results` while the system was down, using up quota 1.
  This is `am` failing to start the instrumentation, which D25 leaves as it was, and none of it was reported as a cut
  stream. The run was repeated on a freshly booted emulator (row 5).
- **Pushed files carry a wrong mtime** (dates in 2049 and 2093 on the device). This is N20, adam's big-endian DONE
  timestamp, kept for parity.
- A lost request's failure text reads `Instrumentation failed: com.malinskiy.adam.exception.RequestRejectedException: `
  with an empty message. Cosmetic.

## Not checked

| Item | Why |
|---|---|
| Two emulators | Timebox. x19 and x31 covered the retry on a surviving device |
| API 33 or lower | Only the API 36 image is installed on the host, and the rules forbid adding to the shared SDK |
| D52's 25 s bound with a stuck worker | Nothing on Android stalls the cleanup; every cancel took milliseconds. The fake covers the bound |
| The JVM build | No question here needed the comparison |
| `tools/real-e2e`'s capture driver | The checks are fault injections outside its scenarios. Its USB guard was used, and the sample APKs are the repository's fixtures |

## Bugs

None found.

## Incidental bugs

None.
