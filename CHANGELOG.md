# Changelog

The section for a version becomes the text of its GitHub release, which carries the date.

## Unreleased

This release fixes bugs of the Kotlin fork, each one an approved difference (D21–D42). Most Kotlin bugs remain; the
[bug ledger](docs/known-kotlin-bugs.md) marks the ones that are fixed. Read
[Changes that need action](docs/migration.md#changes-that-need-action) before you upgrade, and the
[deviations guide](docs/deviations.md) for each change in full.

### Check before you upgrade

- **Breaking: every Marathonfile must set a finite `uncompletedTestRetryQuota`** (D47). `marathon run` and `parse` exit 1
  when it is unset or 2147483647, on every platform, because nothing else ends a run in which a test keeps coming back
  uncompleted. Add this line. As a rule of thumb, not a measurement, use the number of devices + 2 if that is more than 5, and more with flakiness copies:

  ```yaml
  uncompletedTestRetryQuota: 5
  ```

  `maxBatchesWithoutProgress` stays optional and doesn't replace it. See
  [the migration guide](docs/migration.md#a-finite-uncompletedtestretryquota-d47).
- **Device preparation no longer wipes external storage** (D27). Kotlin deleted everything under, for example, `/sdcard`
  on every prepare. A test suite that relies on an empty external storage must now clear it itself. Videos left behind
  by an aborted run, or by a recording that a lost connection cut short in a run that still exits 0, are no longer
  removed by the next one.
- **`socketIdleTimeout` is now real milliseconds** (D21). Kotlin read it as seconds, so `PT30S` lasted 30,000 s. It now
  bounds only four requests: the adb server's version, a device's features, the property reads after boot and the removal
  of port forwards at dispose. Remove the setting, or keep it at 30 s or more; `PT2S` now fails those on a slow adb server.
- **xcodebuild on an iOS or macOS worker is sent as `env NSUnbufferedIO=YES xcrun xcodebuild …`** (D32), and sends carry
  `--timeout=300` (D31). On a host with rsync 3 the worker's `rsync --server` gets the flag too, and so does a wrapper set
  with `RSYNC4J_RSYNC`. A forced command or wrapper on the worker must match `env NSUnbufferedIO=YES xcrun xcodebuild`
  at the start of `SSH_ORIGINAL_COMMAND`, `env` must be on its `PATH`, and the flag must be accepted.
- **iOS 16 and 17 simulators on a slow host or link:** a readiness probe returns about 20 MB and must arrive within `shell`
  (30 s by default), or the probe counts as failed (D38). Raise `shell`. From iOS 18 the probe is about 100 KB (D46).
- **A forced command or filter on an iOS worker must allow `xcrun simctl spawn <udid> launchctl print user/foreground`** (D46),
  the readiness probe of iOS 18 and later, where it was `launchctl dumpstate user/foreground/com.apple.SpringBoard`. iOS 16 and
  17 still send `dumpstate`.
- **A lost iOS worker has a limit now** (D44). A simulator whose worker stays down is no longer tried every 16 ms until the run
  ends: it is tried at `workerConnection`'s pace for `lateJoinCutoff` (600 s) after the first failure, and then stays out. The
  end is noticed when the next pause is over, at 630 s with the defaults. With
  `attempts: 1` and `lateJoin: false` it gets one try. Set `lateJoinCutoff: 0` to wait until the run ends.
- **On a password-authenticated worker, a video or device log that takes over 30 s to pull is now missing** from the
  report, with `.rsync-partial/<name>` left in the output directory (D31, U155). Kotlin's rsync finished it in the
  background. Key authentication has always behaved this way.
- **A run that loses all its devices takes longer to fail** (D48). A pool with tests left waits up to
  `deviceInitializationTimeoutMillis` (180 s by default) for a device to return, so a killed emulator alone now exits 1 after
  about 180 s, not 12.5 s, plus whatever the devices leaving still spend cleaning up then. Lower that value if you need it
  sooner; it also bounds the wait for the first device.

### Android

- `socketIdleTimeout` counts milliseconds, not seconds, and bounds only requests with no timeout of their own, so a silent
  screen recording or install still works (D21).
- A run survives an adb server restart: device tracking reconnects, a loopback server is started again, and batches wait,
  as does remote test parsing. A server still away after `waitForDevicesTimeoutMillis` loses its devices, so the run still
  ends (D22).
- A device that goes `offline` for a moment is kept: it gets up to 10 s to return, with batches waiting, instead of being
  disconnected at once, which ended a run on its last device. A killed emulator now reaches the retry path after about
  3 s instead of at once. A device gets at most 5 of these per adb server in a run, and the count is never reset. Either
  limit can run longer while devices that returned are checked one after another: by up to the `shell` timeout (20 s by
  default) for each one that does not answer, and while their logcat and reverse port forwards are restored (D41, U170).
- An adb request rejected or closed before its reply no longer retires a device that is still listed and answers: only
  the batch fails, 3 times per adb server in a run, and the count is never reset (D49).
- A file sync push whose md5 never matches fails after `pushFile`, naming the file, instead of waiting for the batch
  timeout (D23).
- A disconnected Android device can no longer be borrowed, and `networkState` reports `DISCONNECTED` once it is lost, which
  can show in that device's per-test JSON. iOS devices still report `CONNECTED` (D24).
- An instrumentation stream that fails, or stops before its result as it does when the adb connection drops, is reported
  as a test-run failure with its cause and retried, without retiring a healthy device. A remote parsing attempt fails and
  is retried instead of returning only the tests found so far, which could give a passing run with tests missing (D25).
- A remote parse whose instrumentation fails before it has listed every test it announced is retried, and fails the run with
  the reason if it keeps failing. A failing exit code on a complete list is still accepted (D42).
- A failed re-provisioning of an iOS simulator is retried instead of being final, whether its worker was lost, it was lost with
  its connection up or it was added back. A refused connection is not retried, and a simulator that was in the run can return
  after the late-join cut-off (D44).
- Remote parsing on iOS no longer gets a removed, lost or disposed simulator, which could use up its attempts and fail the
  run at the start. It waits for one that can work (D44).
- Test APKs in DEX 040 and 041 are accepted, with every class of a 041 container found (D26).
- In a batch that mixes test APKs, each instrumentation runs only its own tests, the batch reports one result, and a test
  that a later instrumentation did not report no longer carries an earlier one's failure text (D28, U151).
- A folder left on the device by a run that was cut short is removed before a file sync push, so the first batch does not
  read the previous run's files. A folder that can't be removed fails the batch with `Could not delete remote file(s): …`.
  A pushed folder whose name holds a space no longer deletes a sibling when it is removed (U159, N41).

### iOS and macOS workers

- A command that a worker rejects no longer holds one of its SSH session slots until the connection ends. A command that
  gets no reply in 30 s, or a channel the worker opens after its 30 s are up, is closed too, with an error line that names
  it (D29, U154, U157).
- Closing an SSH session takes at most 30 s: TERM, then KILL, then the session is abandoned with an error line, whatever the
  worker does. A server that ignores signals may leave the process running. On iOS an abandoned session retires its
  simulator (D30).
- An rsync send that makes no progress for 300 s is stopped and fails with a clear error, instead of blocking that worker's
  sends for ever. A large bundle over a slow link is unaffected while it keeps moving (D31).
- An rsync receive at its 30 s deadline no longer leaves rsync and ssh running under `sshpass`, and can no longer wait for
  ever on a process that ignores TERM. A receive whose process ended can be held up by a child holding its output, such as
  a hung ssh, for at most 5 s (D31, U155, U158).
- xcodebuild on a worker runs with `NSUnbufferedIO=YES`, as was always intended, so its output arrives as it is produced and
  the output idle timeout measures real silence (D32).
- A worker that shares no key exchange, cipher, MAC or host key algorithm with marathon is no longer retried as if the
  network had failed. The error text is unchanged (D35).
- A worker that drops the connection right after login is handled as a lost connection instead of aborting with
  `IllegalStateException: Not connected`. On iOS the usual reconnect takes over; macOS has no reconnect, so the device leaves
  the run (D36).
- A command that floods one output stream while the other is being read no longer overflows a buffer and loses the
  connection for every command on that worker; the command blocks until it is read, times out or is closed (D37).

### iOS simulators

- A simulator whose setup failed is set up again, up to three times, 30, 60 and 90 s after each failure, instead of being out
  for the whole run. `attempts: 1` with `lateJoin: false` in `workerConnection` turns this off (D33).
- A failed or timed-out readiness probe means the simulator is not running yet, on every iOS runtime (D38).
- The readiness probe of iOS 18 and later is `launchctl print user/foreground`, about 100 KB where `dumpstate` returned 10 to
  20 MB, with the same marker line. It may report a booted simulator ready up to one poll earlier. iOS 16 and 17 keep `dumpstate`
  (D46).
- A test name, a test's own assertion text or an application's log line that contains a device failure text, such as
  `Failed to terminate`, no longer aborts the batch. After a prefix such as `XCTAssertTrue failed - ` that test fails and the
  batch goes on, and a simulator is no longer dropped after 3 such batches in a row (D39).
- Three output timeouts in a row with no test passed or failed now set a simulator up again, so a real failure that D39 misses
  no longer keeps a sick simulator in the run. This also reaches a test that stays silent past `testOutputTimeoutMillis`, which
  Kotlin repeated on the same simulator for ever. A simulator that does this after each of its 3 recoveries leaves the run (D39,
  U165).
- A test that hangs past its execution time allowance now fails, with the hang's line as its text, whatever form the allowance
  is printed in. With `1 minute`, which a 60 s allowance prints, Kotlin aborted the batch and repeated the test until CI killed
  the job (D45).
- A delayed disconnect for an old device no longer removes its replacement with the same serial from the pool (D40). No
  known path triggers this, so you are unlikely to have met it.

### All platforms

- A Graphite or InfluxDB backend that is down or slow no longer stops a run. With carbon down, a run used to hang at its first
  test result. The failure is now logged once per kind, and sends have time limits, with metrics skipped for 5 minutes after a
  slow failure. The backend's close at the end of a run gets 30 s, so an InfluxDB server that answers its last flush slowly
  can no longer hold the end of a run (D51, U167).
- A Graphite series with no points in the window no longer loses the whole history query (D51, C38).
- A cancelled run writes its reports and terminates its devices whatever `debug` says, so a cancelled CI job has results and leaves
  less behind on workers. SIGINT or SIGTERM stops the tests, writes the reports first, and exits with the same 130 or 143 within
  25 s. A second signal exits at once, however soon after the first; two of the same signal that arrive together count as
  one. Tests no longer bounce after the signal, which added false INCOMPLETE results to the reports (D52, C10, U161).
- A failed reporter or close no longer skips the ones after it, such as a later report or terminating the devices. Each failure
  is logged, and the exit code is unchanged (D51, C11, C25).
- A run whose filters removed every test logs an error line saying what each filter removed, so a mistyped filter is visible. It still exits 0 (D51).
- A pool that loses its last device waits for one to return, instead of ending with its tests unrun: an `adb reboot` on a
  single emulator now exits 0, and an iOS simulator recovered after its worker dropped finds its tests still there. The wait
  is up to `deviceInitializationTimeoutMillis` (180 s by default) from when the pool's last working device went away, which
  includes D22's and D41's waits for that device, or from a test that the device leaving reports after that. It runs on
  while the devices leaving clean up, and while devices come back without getting ready: a device that comes back counts
  only once it is ready. When it is up, the pool stops the devices still getting ready, takes no new ones, and ends once
  every device has finished cleaning up. A device that came back and got ready keeps the pool while it stays; when it
  leaves without a test having ended, the pool ends once it has cleaned up. Any test that ends gives the pool a whole new wait. macOS does
  not wait (D48).
- A history query that fails is no longer final for the run: it is retried in the background a limited number of times, at
  5, 10, 20 s and so on counted from the first failure, so a query that fails later joins the next round. Queues built and
  batches composed afterwards use the history it finds. A run never waits for this. With a failing analytics backend the log
  has more warnings and timing lines. If a query is stuck inside a blocking call, the run moves on after 3 s and writes its
  reports, and the process ends at most 3 s after the run (D34, D52).
- `fixed-size` batching counts only the tests a batch accepts, against both `size` and `durationMillis`, so flaky copies and
  tests of another test APK no longer end a batch early. With two test APKs in sorted order, 1,000 tests at size 10 take 100
  batches instead of 150. A batch rejects at most twice its size. A batch with nothing rejected is unchanged (D50).

### Evidence for these changes

- Everything was tested against fakes and against the recorded behaviour of the Kotlin program.
- D21–D28, D41, D42 and the stream-end fix of D25 also ran on Android emulators (API 33 and 36; D41 and D42 on API 36 only),
  with no real USB or TCP device. The merged Android fixes then ran together on emulators: 41 runs with adb server, adbd and
  emulator kills, reboots, a stale pushed folder and silent video, with no run exiting 0 with tests missing and no hang.
- D48's `adb reboot` and killed-emulator cases and D49's closed requests ran on an API 36 emulator, and the reboot also on
  API 33. D48's iOS recovery ran on the fake worker only.
- D29–D32 and D35–D37 ran against real OpenSSH on loopback.
- D38 and D46 were measured on iOS 18.6, 26.4 and 27.0; iOS 16 and 17 were not measured. D30's TERM timing was measured on one Mac only.
- D33, D34 and D40 had no real simulator, worker or analytics backend.
- D50 was measured through the real pool and queue on a stub device, with no real test APKs.
- D39's rule is reasoned from Xcode's formats and is not recorded from a real run.
- D52 ran through the built binary with real signals on the fake adb and fake worker, and with a test in flight on an Android
  API 36 emulator in both debug modes. It did not run on a real worker or Mac.
- D45 ran on real hanging tests: an iOS 18.6 simulator under Xcode 27.2, with allowances that print `1 minute` and `2 minutes`.
- The comparisons on real devices and private farms predate D21–D42.

### Known limits

- There is no Windows, Intel Mac, Linux on ARM or universal build. The macOS binary requests macOS 11.0 or later; versions
  older than the build and test hosts are not tested.
- The macOS binary is ad-hoc signed and not notarised; see the note on quarantine in the
  [migration guide](docs/migration.md#install-the-native-entry-point).
- The static Linux binary is checked at release time with `version` and `--help` in an Alpine container. The real Android runs
  recorded in this repository used a glibc build, and musl's allocator and DNS resolver differ from glibc's, so try it on
  your workload.
- The `xctest` test parser needs a `libxctest-parser` library that you supply (D20). The differential suites run it with a
  stand-in file, so what the library itself does is not covered by them. The archives do not contain it, and this project's
  licence does not cover it.
- The real macOS UI XCTest scenario could not be completed on macOS 27 and is not part of that evidence
  ([U103](docs/found-bugs.md)).
- No isolated shadow or canary results are recorded in this repository. Trial it in a disposable environment and keep your
  JVM distribution for [rollback](docs/migration.md#trial-and-rollback).
- An emulator that restarted during an adb outage (D22) repeats its failing batches until `uncompletedTestRetryQuota` runs
  out.
- A stalled rsync send costs 300 s, and a failed install is retried, so one simulator can spend about 7.6 hours on a worker
  that never answers, and N simulators on one worker about N times that. With the default keep-alive a dead link still costs
  about 1.6 hours per simulator. Another worker that finishes the queue ends the run. Kotlin waits for ever (D31, U163).
- A failed readiness probe can cost a simulator 26 to 30 minutes with the defaults while another simulator keeps the pool alive.
  On the last device the run ends after about 6 to 13 minutes plus D48's wait, with "remaining tests will not run". If only the probe hangs and
  needs KILL it takes about 33 minutes, and if every command hangs about 50 (D38).
- A failed xcodebuild close retires its simulator, and a pool whose last device it was waits (D48), then ends (D30).
- An Android device that left its pool but not adb's list, after a failed prepare or D49's limit, returns only if the list changes,
  so a pool waiting for it uses up the limit for nothing (D48).
- A batch that D22's hold kept waiting is charged an attempt when its server is given up, so with `uncompletedTestRetryQuota: 0`
  tests can fail without having run (U160).
- A queue that ends for a reason other than a metrics failure, such as a broken console, leaves the run hanging (U168).
- A remote parse cut after other text, before its first status atom, can't be told from output that is not the
  instrumentation protocol: it gives no tests for that bundle and is not retried. With several bundles the run goes on
  without them (D25).

## 0.11.0

The first public release of marathon-rs, a native Rust rewrite of the marathon test
runner for Android, iOS and macOS.

### What it is

- **The same interface.** It keeps the Marathonfile and Marathondevices formats, the
  command line, exit codes and report formats of the Kotlin fork it was ported from, apart
  from the approved [differences D1–D20](docs/deviations.md). The Kotlin fork's bugs are
  deliberately kept too, and are listed in the [bug ledger](docs/known-kotlin-bugs.md) (D5).
- **No JVM.** Each archive holds one executable. `JAVA_OPTS` and `MARATHON_OPTS` are
  ignored (D2).
- **Two targets.** `aarch64-apple-darwin` for Apple Silicon macOS, and a static
  `x86_64-unknown-linux-musl` for x86_64 Linux on any distribution.

Coming from the JVM distribution, read the [migration guide](docs/migration.md) first. The
differences most likely to need action are user regexes, which now use Rust `regex` syntax
and reject Java-only constructs such as lookaround and backreferences (D6), and crash and
usage telemetry, which is not sent (D1). Configured InfluxDB and Graphite backends stay
active (D4). If your Marathonfile selects `testParserConfiguration: {type: xctest}`, you must
now supply the third-party `libxctest-parser` library yourself, because this program does not
contain it ([D20](docs/migration.md#the-xctest-parser-needs-a-library-you-supply)). The default
`nm` parser is not affected.

### Evidence

- The Kotlin tests are ported, or marked not applicable with a reason
  ([parity matrix](docs/parity-matrix.csv)), and differential suites compare whole runs of
  the JVM and native programs against a fake worker and a scripted ADB server.
- Real end-to-end runs compared native and JVM results for iOS simulators, macOS and an
  Android emulator using this repository's sample apps
  ([results](docs/wp/7.5-findings.md#closure-sweep-results)). They ran on an earlier commit
  and were not repeated on this release's exact commit.
- Outside this repository, the maintainer compared earlier builds with the JVM build on real
  multi-host CI farms: the macOS build on iOS simulators, and this static Linux build on Android
  emulators, with results matching apart from flaky tests seen on both sides. Those runs are
  private and cannot be checked here, and they predate the latest fixes.

### Known limits

- There is no Windows, Intel Mac, Linux on ARM or universal build. The macOS binary requests
  macOS 11.0 or later; versions older than the build and test hosts are not tested.
- The macOS binary is ad-hoc signed and not notarised; see the note on quarantine below.
- The static Linux binary is checked at release time with `version` and `--help` in an
  Alpine container. The real Android runs recorded in this repository used a glibc build, and musl's
  allocator and DNS resolver differ from glibc's, so try it on your workload.
- The `xctest` test parser needs a `libxctest-parser` library that you supply (D20). The
  differential suites run it with a stand-in file, so what the library itself does is not
  covered by them. The archives do not contain it, and this project's licence does not cover it.
- The real macOS UI XCTest scenario could not be completed on macOS 27 and is not part of
  that evidence ([U103](docs/found-bugs.md)).
- No isolated shadow or canary results are recorded in this repository. Trial it in a
  disposable environment and keep your JVM distribution for
  [rollback](docs/migration.md#trial-and-rollback).
