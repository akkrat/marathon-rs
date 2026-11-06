# Changelog

The section for a version becomes the text of its GitHub release, which carries the date.

## Unreleased

Fixes of bugs the Kotlin fork has, each an approved difference. See the
[deviations guide](docs/deviations.md) and the [migration guide](docs/migration.md).

### Behaviour change to check

- **Device preparation no longer wipes external storage** (D27). Kotlin deleted everything under, for example,
  `/sdcard` on every prepare. A test suite that relies on an empty external storage must now clear it itself. Videos
  left behind by an aborted run are no longer removed by the next one.

### Fixed

- `socketIdleTimeout` counts milliseconds, not seconds, and bounds only requests with no timeout of their own, so a
  silent screen recording or install still works (D21).
- A run survives an adb server restart: device tracking reconnects, a loopback server is started again, and batches
  wait. A server still away after `waitForDevicesTimeoutMillis` loses its devices, so the run still ends (D22).
- A file sync push whose md5 never matches fails after `pushFile`, naming the file, instead of waiting for the
  batch timeout (D23).
- A disconnected device can no longer be borrowed, and `networkState` reports `DISCONNECTED` once it is lost, which
  can show in that device's per-test JSON (D24).
- An instrumentation stream that fails is reported as a test-run failure with its cause and retried, without retiring a
  healthy device; a remote parsing attempt fails instead of returning only the tests found so far (D25).
- Test APKs in DEX 040 and 041 are accepted, with every class of a 041 container found (D26).
- In a batch that mixes test APKs, each instrumentation runs only its own tests, and the batch reports one result
  (D28).
- An iOS simulator whose setup failed is set up again, up to three times, 30, 60 and 90 s after each failure, instead of
  being out for the whole run. `attempts: 1` with `lateJoin: false` in `workerConnection` turns this off (D33).
- A history query that fails is no longer final for the run: it is retried in the background a limited number of times,
  and queues built and batches composed afterwards use the history it finds. A run never waits for this. With a
  failing analytics backend the log has more warnings and timing lines (D34).

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
