# Dispatch gaps and the report tail (WP x105)

2026-11-22. An outside review of 0.15.1 (`4de8c227`) claimed that (1) a pool's queue actor delays the next
dispatch while it processes a completed batch, and (2) the reporters at the end of a run are slow because they
run one after another and the test JSON reporter makes two blocking jobs per file. This WP was to measure both
before any change. No product code changed.

**Status: the measured series did not run.** The idle gate (1-minute load average below 4 before every run, as
in [runner-overhead.md](runner-overhead.md)) never opened: a bounded 30-minute wait saw 4.68 at the lowest. Four
stale simulator-runtime and diagnostics processes from earlier work, started two to three days before, each used
about one core the whole time (no simulator was booted), and other work ran an Android emulator. Nothing was
killed. What follows is what holds without the gated series. The code-path findings come from reading the code.
The indicative numbers come from functional trial runs of the driver, made **under load 5–9**. They show orders of
magnitude, not statistics: one run each. The driver below reproduces the full set with one command per item once
the machine is idle.

## Setup

| | |
|---|---|
| Machine | Apple M5 Pro, 18 cores (6 performance + 12 efficiency), 48 GiB RAM |
| OS | macOS 27.2.1 (build 26B131), arm64 |
| Python | 3.12.14 (driver only) |
| Rust | rustc 1.99.0 |
| Load | 5–9 during every trial run (gate: < 4, not met) |

| binary | identity |
|---|---|
| `target/release/marathon`, `cargo build --release -p marathon-cli` at `4de8c227` (0.15.1, clean tree) | sha256 `62284dcccb6bf42907971297c9b29d0593834904bb0fc23f6fdd7d2b35f72f19` |
| `target/release/fake-adb`, same tree | sha256 `5a448af4bc05c107082b44978f871698e312dac7470c51f96560ced98c4f5897` |
| 0.15.0 (`f4b6ecd6`), same command, scratch worktree; for the main-vs-0.15.0 series, which did not run | sha256 `a2aed852796df21a6f806a7bf42c0353e69f40b75695c0086a1965cb3bf9af1a` |
| trace build: `4de8c227` plus temporary `eprintln!` trace points and the combined-write prototype, scratch worktree, never committed (see "Instrumentation") | sha256 `2c4ae20a7876038c5c8aae7e2b1d5f69e5a609d05de5ab105b218f8ed7b80bb1` |

## Scenario and driver

[`tools/perf/dispatch_report.py`](../../tools/perf/dispatch_report.py) generates an Android scenario for one fake
ADB server with N devices:

- T tests (`com.example.perf.PerfNNNNTest#testNNNNN`, 50 per class), listed by the remote test parser
  (`testParserConfiguration: remote`), so no test APK of that size is needed;
- `fixed-size` batching with B tests. The queue hands out batches in a deterministic order, so each batch gets its
  own exact `am instrument … -e class <batch>` expectation in strict mode. A batch that differed from the
  prediction would fail the run. None did;
- each test "runs" on the fake device for a scripted time, uniform in [0.5, 1.5] × `--test-ms` with a fixed seed.
  The first trials used equal durations. Those kept all devices in lockstep, so that every batch completion
  collided with every other: an artificial worst case;
- reporters at their defaults (device info, billing, JUnit, timeline, raw JSON, test JSON, Allure results, HTML,
  stdout summary); `debug: false`; screenshots and video off; no retries.

Every run gets a fresh sandbox and fake. The CLI's stdout goes to a pipe drained by the driver. With
`--console normal` the reader reads as fast as it can; with `--console slow` it reads 1 KiB every 50 ms (about
20 KB/s). `--graphite` sets `analyticsConfiguration` to `graphite` on a local TCP sink that accepts and discards
the plaintext metrics. The Graphite read API is on port 80 of the same host, where nothing listens, so the history
query fails at once and the run goes on without history. A run counts only if the CLI exits 0, every test passed,
the fake recorded no warning and the adb tripwire is untouched.

What the driver measures, from what the **unmodified** CLI writes:

- **Device idle gap at a batch boundary**: the first test start of the next batch minus the last test end of the
  previous batch on the same device (`test_result/raw.json`, ms resolution). This includes the fake's latency for
  the next `am instrument`.
- **Report tail**: the last test end to the CLI's exit (wall clock in the driver). It is split per reporter by the
  newest file mtime of each reporter's output tree; reporters run serially in a fixed slot order. On a 200-test
  run this split agreed with the trace build's per-reporter spans within 0.05 ms per reporter.

Instrumentation (trace build only, `--trace`): µs wall-clock lines on stderr at `DevicePool::device_completed`, at
the start and end of `QueueActor::on_batch_completed`, at `on_request_batch` and when it sends `ExecuteBatch`, at
`DevicePool::execute_batch`, per `test_ended` (time in the two console flushes and in the tracker fan-out), and
around each reporter in `ExecutionReportGenerator::close`. The trace build is not part of this commit.

Reproduce (each item gates every run on load < 4, waits up to 30 minutes, and otherwise stops):

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cargo build --release -p marathon-cli -p fake-adb
P="/opt/homebrew/bin/python3.12 -I tools/perf/dispatch_report.py"
$P matrix tail --native target/release/marathon --fake-adb target/release/fake-adb --repeat 5 --output tools/perf/runs/x105/main
$P matrix dispatch --native target/release/marathon --fake-adb target/release/fake-adb --repeat 3 --output tools/perf/runs/x105/main
$P matrix dispatch-graphite --native target/release/marathon --fake-adb target/release/fake-adb --repeat 3 --output tools/perf/runs/x105/main
$P matrix compare --native <0.15.0 build>/marathon --fake-adb target/release/fake-adb --output tools/perf/runs/x105/v0150
$P summarise tools/perf/runs/x105 > tools/perf/runs/x105/summary.json
```

`dispatch` is 1, 8 and 32 devices × batch 1, 5 and 20 × normal and slow console. Tests run 100 ms ±50 %, and each
device gets max(12, 48/N) batches. `dispatch-graphite` is the normal-console set with Graphite, minus two
single-device cases. `tail` is 1 000 and 10 000 results on 8 devices, batch 20, 2 ms tests. `compare` is 32
devices, batch 5, normal console. `--trace` / `--combined` need the scratch trace build.

## Item 1: dispatch gap

### What the code does (verified at `4de8c227`)

- The queue actor handles one message at a time. `on_batch_completed` (`execution/queue.rs:476–505`) awaits the
  metrics observation, takes the progress lock, and awaits `PoolProgressAccumulator::test_ended`
  (`execution/progress.rs:379–425`) per result. A device's next `RequestBatch` is queued behind its own
  `Completed`, and behind those of every other device that completed first (`pool.rs:650–668`, `728–760`).
- **The console flushes do not wait for stdout.** `test_ended`'s two flushes hand the progress records to
  the vendor runtime, and the console sink calls `QueueWriter::write`, which only sends the bytes over an unbounded
  channel to the console writer thread (`marathon-cli/src/tracing_output.rs:97–116`). A slow stdout reader
  therefore cannot hold up the queue actor or dispatch. The writer thread's backlog grows in memory instead.
  The backlog is drained at exit, for at most the 60 s console completion deadline
  (`CONSOLE_COMPLETION_MILLIS`). So a slow console can lengthen the **exit**, not the gaps.
  The pipe itself also buffers up to 64 KiB before the writer thread blocks.
- The tracker fan-out (`artifact_io::on_io_scoped`) awaits every tracker's `test` call. That includes the remote
  metrics tracker's admission (`RemoteTracker::admit`, `analytics/event_pipeline.rs:464–504`, D51). While the
  backend's bounded FIFO has room, admission is a `try_reserve` and costs nothing. When a slow backend has filled
  the FIFO, one `test_ended` waits for a slot for up to `BackendLimits::send` (30 s). The first expiry opens the
  circuit for 5 minutes, and later events are rejected at once. So a slow backend can hold up a pool's queue
  actor, and with it every dispatch of that pool, for at most about 30 s per 5 minutes. This path was not run
  against a slow backend (see "Not measured").

### Indicative numbers (trial runs under load 5–9; one run each; not the gated series)

| run | devices × batch × tests | per-device idle gap p50 / p95 (raw.json) | runner part: device completed → next `ExecuteBatch` at the pool, p50 / p95 (trace) | queue handling of one completed batch, p50 / p95 (trace) |
|---|---|---|---|---|
| lockstep (equal 20 ms tests) | 8 × 5 × 200 | 3 / 6 ms | 2.8 / 5.9 ms | 0.57 / 0.72 ms |
| jittered 2 ms tests (completion rate ≈ 4 000 tests/s) | 8 × 20 × 10 000 | 2 / 5 ms | 1.5 / 4.3 ms | 1.38 / 2.35 ms |
| jittered 50 ms tests | 32 × 1 × 640 | 0 / 1 ms | – | – |
| same, slow console | 32 × 1 × 640 | 1 / 1 ms | – | – |
| jittered 20 ms tests, Graphite sink | 8 × 1 × 160 | 0 / 1 ms | – | – |

Per result, `test_ended` cost 40 µs p50 (78 µs p95) over 10 000 results: 23 µs in the two console flushes
(the cross-runtime hop, not stdout) and 16 µs in the tracker fan-out. The metrics observation per batch was about
22 µs. With Graphite configured, the run opened one TCP connection per test (160 for 160 tests; that is how the
client sends) and still showed no measurable gap.

Reading: the runner's own cost at a batch boundary is about **0.06–0.07 ms per result plus a few tens of µs per
batch**. A device waits behind the other devices' completions only when they arrive within the same
millisecond or so. Even at 4 000 completed tests per second on 8 devices, which no real farm reaches, the p95
from completion to next dispatch was about 4 ms. The slow console made no difference, because the progress
output of these runs (60 KB) fit into the pipe buffer, and the flush does not wait for the pipe anyway.

## Item 2: report tail

### What the code does

`ExecutionReportGenerator::close` (`analytics/event_pipeline.rs:170–194`) runs the reporters one after another on
the I/O runtime, off core. `TestJsonReporter` (`report/reporters.rs:90–103`) runs one blocking job for
`create_file` (plan and create directories) and a second for the write, per final result. The HTML reporter
runs about five blocking jobs per test (`reporters/html.rs:356–436`). The first reporter that calls
`ExecutionReport::summary()` (JUnit, by slot order) pays for computing the summary, which is **quadratic in the
number of results** (`report.rs:104–160`, see "Incidental bugs" in [x105-findings.md](../wp/x105-findings.md)).

### Indicative numbers (trial runs under load 5–9; one run each)

| reporter | 200 results, 8 devices (ms) | 10 000 results, 8 devices (ms) | 10 000, combined create+write prototype (ms) |
|---|---|---|---|
| device info | 2.7 | 1.0 | 1.7 |
| billing | 0.7 | 0.9 | 1.3 |
| JUnit (includes the summary computation) | 0.5 | 605 | 634 |
| timeline | 0.4 | 7.1 | 5.1 |
| raw JSON | 0.2 | 2.9 | 2.6 |
| test JSON | 16.5 | 820 | 672 |
| Allure results | 13.9 | 697 | 667 |
| HTML | 39.9 | 1 877 | 1 870 |
| stdout summary | 0.04 | 0.03 | 0.04 |
| **all reporters** | **74.8** | **4 011** | **3 854** |
| last test end → reports start | 4.7 | 3.3 | 4.7 |
| reports done → exit | 1.7 | 6.7 | 6.2 |
| **last test end → exit** | **82** | **4 023** | **3 868** |

HTML dominates at both sizes, at about 47 % of the tail. Test JSON and Allure follow with about 18–20 % each. JUnit
is negligible at 200 results but takes 605 ms at 10 000. Per result it goes from 2.5 µs to 60 µs: the quadratic
summary. The combined create+write prototype puts plan, directory creation and write for one file into one
blocking job (scratch only). It saved about 150 ms on test JSON at 10 000 results, about 15 µs per file
(−18 %), and about 4 % of the tail. The comparison is one run each under load.

The slow console adds to the tail whatever progress output is still queued at exit, at the reader's speed, for
at most 60 s. A 10 000-test run prints about 0.9 MB of progress lines. A reader at 20 KB/s would be about 45 s
behind by then. The gated `dispatch` slow-console series would have measured this; it did not run.

## Real farm (anonymised aggregate)

From the CI timestamps of saved real-farm job logs (0.15.1, `debug: false`). The analysis itself is kept outside
the repository. A real iOS UI job: 80 simulators, about 850 tests in about 180 batches (about 2.2 batches per
simulator, about 100 batch boundaries). The median device idle at a batch boundary, from the end of one
xcodebuild test run to the first app launch of the next, is:

| run | idle p50 / p90 | xcodebuild start-up p50 | dispatch + device pre-batch work + process start p50 | result pull and processing p50 |
|---|---|---|---|---|
| Rust 0.15.1, video off | 10.4 / 15.4 s | 7.7 s | 1.9 s | 0.37 s |
| Rust 0.15.1, video on | 12.8 / 21.3 s | 9.7 s | 2.1 s | 0.41 s |
| JVM control, video on | 11.0 / 42.5 s | 7.7 s | 2.0 s | 0.41 s |

The logs cannot split "dispatch + device pre-batch work" further (no debug lines). By the local trace, the
runner's own share of it is in the millisecond range: under 0.1 % of the idle time. Android jobs and the
0.15.0-rc.1 logs carry no per-line timestamps, so no gap can be derived from them.

## Conclusions and ranked fixes

1. **Dispatch gap: not worth changing.** The queue actor's work per boundary is about 0.07 ms per result, so
   1.4 ms for a 20-test batch even under load. The console cannot block it. On the real farm a boundary idles for
   about 10 s, which is almost all xcodebuild start-up and device-side work. Moving `test_ended` out of the actor would save at
   most a few ms per boundary, which is ≤ 0.05 % of a real boundary. It would also add a concurrency change to the most
   delicate actor. Expected win: none measurable on a real run. The one path that can stall dispatch for seconds
   is a metrics backend that has filled its FIFO: up to 30 s once per 5-minute cool-off, by D51's design. If that
   matters, the place to fix it is the admission wait, not the actor. Measuring it needs a slow sink
   (not done).
2. **Report tail at 10k results: small, worth it only as cheap clean-ups, in this order:**
   1. make `ExecutionReport::summary()` linear (index the events by test once; use hash sets for the
      unique name lists). That saves about 0.6 s at 10 000 results and grows quadratically beyond: about 2.4 s
      at 20 000 and about 15 s at 50 000, extrapolated, not measured;
   2. let the HTML reporter write a test's files in one blocking job instead of about five. Estimated from
      the test-JSON prototype (about 15 µs per saved hop): 0.5–0.7 s of its 1.9 s at 10 000 results;
   3. combined create+write for test JSON (and the same pattern in Allure): about 0.15 s at 10 000 results;
   4. running independent reporters concurrently would hide part of the remaining 2–3 s. It changes
      the order of output files and failure reporting, so it is a design change, not a clean-up.

   The whole tail at 10 000 results is about 4 s against a run of hours on a real farm. At 1 000 results it is
   in the low hundreds of ms (not measured; about 80 ms at 200).
3. Retry rescans (item 3 of the review): not measured, by the brief's default.

## Not measured

- Every gated series: the p50/p95 tables per device count and batch size, normal vs slow console, Graphite vs
  none, main vs 0.15.0, and the report tail at 1 000 results. The machine never became idle (see "Status").
- Apple vendors: the fake SSH worker runs every `ssh`/`rsync` through Python shims that cost about 58 ms each
  ([runner-overhead.md](runner-overhead.md)). They would dominate a dispatch gap of a few ms, so an Apple fake run
  would measure the shims, not the queue.
- A slow or unreachable metrics backend (only a fast local sink).
- Linux.
