# Runner overhead: Rust native CLI vs Kotlin JVM CLI

Measured 2026-11-01. The question: how much does the runner itself cost (wall
time, CPU, memory) on identical deterministic fake-worker scenarios? No real
hosts, simulators, emulators or devices are involved.

## Setup

| | |
|---|---|
| Machine | Apple M5 Pro, 18 cores (6 performance + 12 efficiency), 48 GiB RAM |
| OS | macOS 27.2 (build 26B123), arm64 |
| Idle gate | 1-minute load average < 4 before every run (highest observed before any run: 3.82); nothing else was started or killed |
| Python | 3.12.14 (driver only) |

### Binaries

| side | artifact | identity |
|---|---|---|
| native | `cargo build --release -p marathon-cli` of this worktree, `target/release/marathon` | commit `078eaa999c6b5e6344702e696960c30e44a8552c` (clean tree), sha256 `39bd2445144999dc5d0b43efcd004ca04a18b80e3f19377185eaf52afc0d5a00` |
| JVM | the 7.5 release-source reference (`installDist`, `bin/marathon`), built earlier with a receipt, not rebuilt | source `c95b212882c7ba7b1ad91b7981916ed175f1530e`, variant `reference`; all 139 jars in `lib/` re-hashed and equal to `receipt-reference.json` (combined sha256 of the jar map `dcac42f76e5192b2bddad4fd685148241d853ce1a448fae5e956ccb32405d120`) |
| fake SSH worker | `target/release/fake-worker` (release) | sha256 `c3ae2e06bfd8985870d485058b0b9355f78b3e7444fca6d5f9534ac43d026853` |
| fake ADB | `target/release/fake-adb` (release) | sha256 `9b3bd11218421a3087e0807013cb87956eb7dd20a02d90b5885a30e6b5f39434` |

JVM: `openjdk version "21.0.10" 2026-01-20`, OpenJDK 64-Bit Server VM (Android
Studio's JBR). The launcher script sets `DEFAULT_JVM_OPTS=""`, and the driver sets
no `JAVA_OPTS`/`MARATHON_OPTS`, so the JVM runs with ergonomic defaults on this
machine: G1, initial heap 768 MiB, max heap 12 GiB (a quarter of RAM),
`CICompilerCount=12`, tiered compilation to level 4, compressed oops. These are
what a user gets from the shipped launcher. The native side has no tunables.

## Scenarios

The same fake-worker / fake-ADB setups and fixtures that WP 7.1 uses (`tools/differential/vendors.py`
supplies the input preparation, `fixtures/fake-worker`, `fixtures/fake-adb`,
`fixtures/bundles`, `fixtures/vendor-android`). Each measured run gets a fresh
sandbox (HOME, TMPDIR, report dir) and a fresh fake process; both CLIs get the
same inputs.

| scenario | what runs |
|---|---|
| `startup-version` | `marathon version`: no configuration, no devices (startup cost only) |
| `android-smoke` | Android vendor, one fake ADB device, 1 test, local APK test parse, all reporters at their defaults |
| `android-two-devices` | Android vendor, two fake ADB devices, 3 tests (pass / fail / ignored), all reporters |
| `ios-smoke` | iOS vendor over SSH to the fake worker (key auth, rsync file transfer), 3 tests (pass / fail / ignored), screenshots |
| `macos-smoke` | macOS vendor over SSH to the fake worker, 3 tests |

Android scenarios are 7.1's `android-smoke` / `android-two-devices`; Apple
scenarios are 7.1's `ios-smoke` / `macos-smoke`, with `debug: false` as in 7.1's
input preparation. Retries are off in all of them (7.1's cases use none); the
scenarios are small by construction, so "larger" here means two devices and
three tests, not hundreds.

### Validity of a sample

A run counts only if (a) the CLI exits with the exit code 7.1 defines for the
case (0 for `version`/`android-smoke`, 1 for the others, which contain a failing
test), (b) the fake exits 0 and, for Android, the adb tripwire was not touched,
and (c) its output observation equals the first observation of that scenario on
**either** side. The observation is the sorted list of report-tree paths with
UUIDs, ports, sandbox paths and (for the two-device case) emulator serials folded,
plus each test's status from `test_result/<pool>/<device>/*.json`. All
50 measured and 5 warm-up runs per side passed, so no sample was excluded; across
sides every scenario produced one single observation digest.

This is weaker than 7.1's byte-level comparator: that one needs the patched JVM
build's tapes (frozen clocks, taped IDs and device choice), which the shipped
launchers do not have, and the rule was to measure the shipped launchers.
Report contents (timings, HTML, logs) are not compared here; 7.1 and 7.5 certify
those.

## Method

- Each run is `/usr/bin/time -l <cli> ...` (macOS), from which real, user, sys,
  maximum resident set size, peak memory footprint and instructions retired are
  read. The driver also takes `wait4` rusage (microsecond user/sys) and its own
  `perf_counter` wall around the whole `time` process: `/usr/bin/time` reports
  only 10 ms resolution, which is too coarse for the 6 ms native startup. Wall
  and CPU in the tables are the `perf_counter` and `wait4` values (they include
  the few ms of `/usr/bin/time`'s own exec, identical for both sides); the
  `/usr/bin/time` values are in the raw file.
- Per scenario and side: 1 warm-up run (discarded, but its exit and output are
  checked), then 10 measured runs, interleaved JVM/native with the first side
  alternating each round. Load average is checked before every run.
- Median, p90 (nearest rank: the 9th of 10), min and max per metric. Ratios are
  JVM/native, of medians and of p90s. With ten runs the p90 is descriptive only; it
  carries no tail-latency confidence.
- CPU is user+sys of the CLI process tree (including `ssh`, `rsync` and shim
  children that the CLI waited for). The fake worker/ADB is a separate process
  and is **not** in the CLI's rusage or footprint; its wall-clock latency is in
  the CLI's wall time. The raw file records the fake's own rusage (about 25 ms CPU
  per Android run, 45 ms for two devices, about 100 ms per Apple run, similar for both sides).

Reproduce:

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cargo build --release -p marathon-cli -p fake-worker -p fake-adb
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
/opt/homebrew/bin/python3.12 -I tools/perf/runner_overhead.py \
  --jvm-bin <build-c95b2128...>/marathon-src/cli/build/install/marathon/bin/marathon \
  --native target/release/marathon --fake-worker target/release/fake-worker \
  --fake-adb target/release/fake-adb --output tools/perf/runs/<name>
# raw samples: tools/perf/runs/<name>/results.json; slim summary + tables:
/opt/homebrew/bin/python3.12 -I tools/perf/runner_overhead.py --summarise \
  tools/perf/runs/<name>/results.json docs/perf/runner-overhead-summary.json
```

## Results

10 valid samples per side per scenario. Cells are median [p90] (min-max).

**wall time (real)**: median [p90] (min-max)

| scenario | JVM | native | JVM/native (median) | JVM/native (p90) |
|---|---|---|---|---|
| android-smoke | 1.96 s [2.04 s] (1.91 s-2.07 s) | 331 ms [334 ms] (321 ms-335 ms) | 5.9x | 6.1x |
| android-two-devices | 2.04 s [2.12 s] (1.97 s-2.17 s) | 332 ms [334 ms] (328 ms-334 ms) | 6.1x | 6.4x |
| ios-smoke | 2.87 s [3.14 s] (2.63 s-3.14 s) | 1.03 s [1.05 s] (992 ms-1.06 s) | 2.8x | 3.0x |
| macos-smoke | 3.16 s [3.30 s] (2.72 s-3.46 s) | 1.24 s [1.25 s] (1.20 s-1.25 s) | 2.6x | 2.6x |
| startup-version | 227 ms [229 ms] (225 ms-230 ms) | 6 ms [7 ms] (6 ms-7 ms) | 34.9x | 32.6x |

**CPU time, user+sys**: median [p90] (min-max)

| scenario | JVM | native | JVM/native (median) | JVM/native (p90) |
|---|---|---|---|---|
| android-smoke | 4.03 s [4.09 s] (3.96 s-4.11 s) | 231 ms [232 ms] (218 ms-233 ms) | 17.4x | 17.6x |
| android-two-devices | 4.22 s [4.29 s] (4.18 s-4.29 s) | 238 ms [240 ms] (234 ms-240 ms) | 17.7x | 17.9x |
| ios-smoke | 3.56 s [3.58 s] (3.47 s-3.59 s) | 350 ms [355 ms] (335 ms-366 ms) | 10.2x | 10.1x |
| macos-smoke | 3.48 s [3.56 s] (3.36 s-3.59 s) | 437 ms [450 ms] (430 ms-451 ms) | 8.0x | 7.9x |
| startup-version | 412 ms [433 ms] (408 ms-443 ms) | 4 ms [5 ms] (4 ms-5 ms) | 93.4x | 91.1x |

**max RSS**: median [p90] (min-max)

| scenario | JVM | native | JVM/native (median) | JVM/native (p90) |
|---|---|---|---|---|
| android-smoke | 697.7 MiB [708.1 MiB] (678.6 MiB-713.6 MiB) | 45.1 MiB [45.1 MiB] (45.0 MiB-45.3 MiB) | 15.5x | 15.7x |
| android-two-devices | 726.3 MiB [740.6 MiB] (711.7 MiB-746.3 MiB) | 45.5 MiB [45.6 MiB] (45.5 MiB-45.8 MiB) | 15.9x | 16.2x |
| ios-smoke | 257.6 MiB [261.4 MiB] (252.0 MiB-290.6 MiB) | 19.0 MiB [19.0 MiB] (18.5 MiB-19.0 MiB) | 13.6x | 13.7x |
| macos-smoke | 266.5 MiB [276.5 MiB] (257.3 MiB-290.1 MiB) | 18.1 MiB [18.2 MiB] (17.7 MiB-18.2 MiB) | 14.7x | 15.2x |
| startup-version | 110.0 MiB [111.4 MiB] (108.5 MiB-112.9 MiB) | 7.1 MiB [7.1 MiB] (7.0 MiB-7.1 MiB) | 15.6x | 15.7x |

**peak memory footprint**: median [p90] (min-max)

| scenario | JVM | native | JVM/native (median) | JVM/native (p90) |
|---|---|---|---|---|
| android-smoke | 599.0 MiB [610.4 MiB] (573.9 MiB-612.7 MiB) | 35.3 MiB [35.3 MiB] (35.2 MiB-35.6 MiB) | 17.0x | 17.3x |
| android-two-devices | 624.8 MiB [646.5 MiB] (610.9 MiB-658.7 MiB) | 35.6 MiB [35.7 MiB] (35.6 MiB-35.9 MiB) | 17.5x | 18.1x |
| ios-smoke | 202.5 MiB [205.5 MiB] (200.7 MiB-233.2 MiB) | 6.3 MiB [6.3 MiB] (5.8 MiB-6.4 MiB) | 32.3x | 32.4x |
| macos-smoke | 213.3 MiB [218.3 MiB] (202.2 MiB-237.4 MiB) | 6.0 MiB [6.0 MiB] (5.5 MiB-6.1 MiB) | 35.6x | 36.1x |
| startup-version | 85.3 MiB [87.3 MiB] (83.0 MiB-88.2 MiB) | 2.5 MiB [2.5 MiB] (2.5 MiB-2.5 MiB) | 34.3x | 34.9x |

**instructions retired (CLI process only, children excluded)**: median [p90] (min-max)

| scenario | JVM | native | JVM/native (median) | JVM/native (p90) |
|---|---|---|---|---|
| android-smoke | 39.92 G [40.68 G] (39.06 G-40.78 G) | 5.19 G [5.20 G] (5.19 G-5.20 G) | 7.7x | 7.8x |
| android-two-devices | 41.24 G [41.42 G] (40.51 G-41.58 G) | 5.23 G [5.24 G] (5.23 G-5.24 G) | 7.9x | 7.9x |
| ios-smoke | 29.04 G [29.26 G] (28.66 G-29.43 G) | 293 M [293 M] (291 M-294 M) | 99.2x | 99.7x |
| macos-smoke | 26.79 G [27.38 G] (26.40 G-27.72 G) | 214 M [215 M] (213 M-215 M) | 124.9x | 127.4x |
| startup-version | 3.94 G [4.10 G] (3.92 G-4.21 G) | 38 M [38 M] (37 M-38 M) | 104.4x | 108.2x |


## Caveats

- **Fake worker included in wall time.** The fake is shared by both sides and
  its latency (SSH handshake, scripted responses, rsync) is part of each run's
  wall time. The Android native wall time sits at about 330 ms in both Android scenarios, with CPU about 230 ms, and I did not
  determine how much of it is fake or protocol latency rather than runner work;
  treat the native Android wall time as an upper bound on the runner's own cost.
- **Apple wall times include the test harness's shims; how much is not measured.** On the Apple
  scenarios every `ssh`/`rsync` the CLI starts goes through
  `tools/fake-worker/shims/*`, a Python 3 wrapper that logs argv, then execs the
  real tool. A standalone shim call (`rsync --version`) costs about 58 ms wall on this
  machine (median of 20), and each run makes 10 (iOS) or 14 (macOS) calls. Whether
  those calls sit on the critical path, and so how much of the native 1.03 s / 1.24 s
  wall they account for, was not measured: that they set the Apple floor, and that
  the Apple wall ratios understate the runner difference, are hypotheses. The shim
  CPU of waited-for children is inside the CPU numbers of both sides.
- **Memory has two different scopes.** *Max RSS* as `/usr/bin/time -l` reports it is
  the largest high-water mark of any single process (the CLI or one of its
  waited-for children), not the simultaneous memory of the whole process tree.
  *Peak memory footprint* is that of the launched process only. The JVM launcher
  ends in `exec "$JAVACMD"`, so for the JVM both numbers belong to the java process
  itself; for the native Apple runs the 18–19 MiB max RSS is likely a child's (an
  `ssh`/`rsync` or shim process), which is why it exceeds the CLI's 6 MiB footprint.
- **JIT warm-up and one-shot runs.** Each CLI invocation is a fresh process; a
  JVM never reaches steady state in 2-3 s, which is what a user also sees, but
  the single warm-up run mostly warms file caches, not the JIT. JVM CPU (about
  4 s) exceeds its wall time (about 2 s) because JIT compiler and GC threads run
  in parallel on this 18-core machine; on fewer cores the JVM CPU-to-wall ratio
  and the wall ratios would change.
- **Launcher defaults.** JVM numbers use ergonomic defaults for this 48 GiB machine
  (12 GiB max heap, 768 MiB initial heap); a smaller machine or explicit `-Xmx`
  or `-XX:TieredStopAtLevel=1` would change them. Max RSS reflects what the JVM
  touched, not its heap limit.
- **Single machine, single session.** One Mac, one day, one OS build; no other
  OS or CPU. The macOS-only (Apple Silicon) result does not transfer to Linux.
  Load stayed below 4 but other users were logged in, so absolute numbers carry
  that noise; the tight min-max ranges suggest it was small.
- **Small scenarios.** Tests run for a few ms in the fake; this measures
  startup, configuration, device setup, scheduling, reporting and file output,
  not the cost per test on large suites. Real device latency would dilute all
  wall ratios.
- **Output equality is structural** (see above), not 7.1's byte comparator.
- Instruction counts come from `/usr/bin/time -l` and cover the CLI process only:
  unlike CPU time they exclude waited-for children (an offline probe showed about
  2.46 billion instructions for a workload run directly against about 197 million for
  a parent that waited for it). They are a CLI-process measure, not a tree total.

## Conclusion

On these fake-worker runs the native CLI needs about 6x less wall time than the
JVM CLI for the Android scenarios (0.33 s vs 2.0 s), about 2.6-2.8x less for the
Apple ones (1.0-1.2 s vs 2.9-3.2 s; how much of that floor the shared shim
processes set was not measured), and starts in 6 ms instead of 227 ms. CPU time is 8-18x
lower for a run and ~90x lower for startup, and memory is 14-16x lower by max RSS (the largest single process, see the
caveats) and 30-36x lower by the CLI process's own footprint on Apple and startup:
7-45 MiB against 110-730 MiB. Class loading and JIT/GC work in a short-lived JVM
are the likely main JVM costs (not profiled); the gap is largest where the run is
shortest and smallest on the Apple scenarios, where the harness's SSH, rsync and
shim work is shared by both sides. These are measurements of runner overhead
only; they say nothing about time spent in real devices, which would dominate real runs.

Raw samples (318 KB) are not committed: they are written to
`tools/perf/runs/<name>/results.json` (git-ignored); the committed
[`runner-overhead-summary.json`](runner-overhead-summary.json) holds all
statistics, hashes, machine data and the shim cost.
