# Changelog

The section for a version becomes the text of its GitHub release, which carries the date.

## Unreleased

Fixes of bugs the Kotlin fork has, each an approved difference. See the
[deviations guide](docs/deviations.md) and the [migration guide](docs/migration.md).

### Behaviour change to check

- **Device preparation no longer wipes external storage** (D27). Kotlin deleted everything under, for example,
  `/sdcard` on every prepare. A test suite that relies on an empty external storage must now clear it itself. Videos
  left behind by an aborted run, or by a recording that a lost connection cut short, are no longer removed by the next
  one.

### Fixed

- `socketIdleTimeout` counts milliseconds, not seconds, and bounds only requests with no timeout of their own, so a
  silent screen recording or install still works (D21).
- A run survives an adb server restart: device tracking reconnects, a loopback server is started again, and batches
  wait, as does remote test parsing. A server still away after `waitForDevicesTimeoutMillis` loses its devices, so the
  run still ends (D22).
- A file sync push whose md5 never matches fails after `pushFile`, naming the file, instead of waiting for the
  batch timeout (D23).
- A disconnected device can no longer be borrowed, and `networkState` reports `DISCONNECTED` once it is lost, which
  can show in that device's per-test JSON (D24).
- An instrumentation stream that fails, or stops before its result as it does when the adb connection drops, is
  reported as a test-run failure with its cause and retried, without retiring a healthy device. A remote parsing attempt
  fails and is retried instead of returning only the tests found so far, which could give a passing run with tests
  missing (D25).
- A remote parse whose instrumentation fails before it has listed every test it announced is retried, and fails the run
  with the reason if it keeps failing, instead of running only the tests listed so far. A failing exit code on a complete
  list is still accepted (D42).
- Test APKs in DEX 040 and 041 are accepted, with every class of a 041 container found (D26).
- In a batch that mixes test APKs, each instrumentation runs only its own tests, and the batch reports one result
  (D28).
- An iOS simulator whose setup failed is set up again, up to three times, 30, 60 and 90 s after each failure, instead of
  being out for the whole run. `attempts: 1` with `lateJoin: false` in `workerConnection` turns this off (D33).
- A history query that fails is no longer final for the run: it is retried in the background a limited number of times,
  and queues built and batches composed afterwards use the history it finds. A run never waits for this. With a
  failing analytics backend the log has more warnings and timing lines. If a query is stuck inside a blocking call, the
  run moves on after 3 s and writes its reports, but the process ends only when that call returns, which may be never
  (D34).
- A failed or timed-out simulator readiness probe now means the simulator is not running yet, on every iOS runtime
  (D38).
- A test name, a test's own assertion text or an application's log line that contains a device failure text, such as
  `Failed to terminate`, no longer aborts the batch (D39).
- A delayed disconnect for an old device no longer removes its replacement with the same serial from the pool (D40).
- A command that an iOS or macOS worker rejects no longer holds one of the worker's SSH session slots until the
  connection ends (D29).
- Closing an SSH session on an iOS or macOS worker takes at most 30 s: TERM, then KILL, then the session is abandoned
  with an error line, whatever the worker does. A server that ignores signals may leave the process running (D30).
- An rsync send that makes no progress for 300 s is stopped and fails with a clear error, instead of blocking that
  worker's sends for ever. A large bundle over a slow link is unaffected while it keeps moving (D31).
- xcodebuild on an iOS or macOS worker runs with `NSUnbufferedIO=YES`, as was always intended, so its output arrives as
  it is produced and the output idle timeout measures real silence. The worker's command line now starts with
  `env NSUnbufferedIO=YES` (D32).
- A worker that shares no key exchange, cipher, MAC or host key algorithm with marathon is no longer retried as if the
  network had failed; the error text is unchanged and real network faults are still retried (D35).
- A worker that drops the connection right after login is handled as a lost connection: commands that may fail return
  no result instead of aborting with `IllegalStateException: Not connected`. On iOS the usual reconnect takes over;
  macOS has no reconnect, so the device leaves the run, as for any lost connection (D36).
- A command that floods one output stream while the other is being read no longer overflows a buffer and loses the
  connection for every command on that worker; the command blocks until it is read, times out or is closed (D37).

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
