# Dispatch gaps and the report tail (WP x105)

2026-11-22. An outside review of 0.15.1 (`4de8c227`) made two claims. (1) A pool's queue actor delays the next
dispatch while it processes a completed batch. (2) The reporters at the end of a run are slow, because they run
one after another and the test JSON reporter makes two blocking jobs per file. This WP measured both before any
change, on current main `7362d27c` (0.15.2), whose run path is the same as 0.15.1: x104 changed log visibility
only, and none of the files cited below. No product code changed.

Two rounds. In round 1 the idle gate never opened: four stale simulator-runtime and diagnostics processes kept the
load above 5. Only functional trial runs under load were possible. Those processes were stopped before round 2, and
the gated series below then ran in about 33 minutes. Round 1's trace-build breakdown has no gated counterpart, so it
stays here, labelled **indicative (round 1, under load 5–9)**.

## Setup

| | |
|---|---|
| Machine | Apple M5 Pro, 18 cores (6 performance + 12 efficiency), 48 GiB RAM |
| OS | macOS 27.2.1 (build 26B131), arm64 |
| Idle gate | 1-minute load average < 4 before every run, as in [runner-overhead.md](runner-overhead.md). Observed before the measured runs: 0.75–3.60, per series in the tables. An Android emulator of other work stayed up (about 3 % of a core); nothing was killed |
| Python | 3.12.14 (driver only) |
| Rust | rustc 1.99.0 |

| binary | identity |
|---|---|
| `target/release/marathon`, `cargo build --release -p marathon-cli` at branch head on `7362d27c` (crates identical to `7362d27c`; clean tree) | sha256 `56dc0551793977b357d21582dc7c25d434b3ecd984ec0416f4994a8bb78a6691` |
| `target/release/fake-adb`, same tree | sha256 `5a448af4bc05c107082b44978f871698e312dac7470c51f96560ced98c4f5897` |
| `target/release/fake-worker`, same tree (built; not used, see "Not measured") | sha256 `41e3efb72d50fcdb9c29bc74f000b09ee6de21f961bc1801d0252173c52231a7` |
| 0.15.0 (`f4b6ecd6`), same command, scratch worktree; for the main-vs-0.15.0 series. Rebuilt in round 2 and identical to round 1's build | sha256 `a2aed852796df21a6f806a7bf42c0353e69f40b75695c0086a1965cb3bf9af1a` |
| combined create+write prototype: `7362d27c` plus a scratch-only change (see "Item 2"), never committed | sha256 `b0e9b94f30423b37a9fee579f0503e92dc52eb1f18fafd6c5a5f309660eb5770` |
| round 1 trace build: `4de8c227` plus temporary `eprintln!` trace points, scratch worktree, never committed (indicative numbers only) | sha256 `2c4ae20a7876038c5c8aae7e2b1d5f69e5a609d05de5ab105b218f8ed7b80bb1` |

## Scenario and driver

[`tools/perf/dispatch_report.py`](../../tools/perf/dispatch_report.py) generates an Android scenario for one fake
ADB server with N devices:

- T tests (`com.example.perf.PerfNNNNTest#testNNNNN`, 50 per class), listed by the remote test parser
  (`testParserConfiguration: remote`), so no test APK of that size is needed;
- `fixed-size` batching with B tests. The queue hands out batches in a deterministic order, so each batch gets its
  own exact `am instrument … -e class <batch>` expectation in strict mode. A batch that differed from the
  prediction would fail the run. None did;
- each test "runs" on the fake device for a scripted time, uniform in [0.5, 1.5] × `--test-ms` with a fixed seed.
  Equal durations keep all devices in lockstep, so that every batch completion collides with every other: an
  artificial worst case;
- reporters at their defaults (device info, billing, JUnit, timeline, raw JSON, test JSON, Allure results, HTML,
  stdout summary); `debug: false`; screenshots and video off; no retries.

Every run gets a fresh sandbox and fake. The CLI's stdout goes to a pipe drained by the driver. With
`--console normal` the reader reads as fast as it can; with `--console slow` it reads 1 KiB every 50 ms (about
20 KB/s). `--graphite` sets `analyticsConfiguration` to `graphite` on a local TCP sink that accepts and discards
the plaintext metrics. The Graphite read API is on port 80 of the same host, where nothing listens, so the history
query fails at once and the run goes on without history. A run counts only if the CLI exits 0, every test passed,
the fake recorded no warning and the adb tripwire is untouched. **All 121 measured runs were valid.**

What the driver measures, from what the **unmodified** CLI writes:

- **Device idle gap at a batch boundary**: the first test start of the next batch minus the last test end of the
  previous batch on the same device (`test_result/raw.json`, **1 ms resolution**). It includes the fake's latency
  for the batch's final instrumentation output and for the next `am instrument`, so it is an upper bound on the
  runner's share.
- **Report tail**: the last test end to the CLI's exit (wall clock in the driver). It is split per reporter by the
  newest file mtime of each reporter's output tree; reporters run serially in a fixed slot order. In round 1 this
  split agreed with the trace build's per-reporter spans within 0.05 ms per reporter.

Reproduce (each item gates every run on load < 4, waits up to 30 minutes, and otherwise stops; every CLI run has a
30-minute hang guard):

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cargo build --release -p marathon-cli -p fake-adb
P="python3 -I tools/perf/dispatch_report.py"
N="--native target/release/marathon --fake-adb target/release/fake-adb"
$P matrix tail --repeat 5 $N --output tools/perf/runs/x105b/main
$P matrix dispatch --repeat 3 $N --output tools/perf/runs/x105b/main
$P matrix dispatch-graphite --repeat 3 $N --output tools/perf/runs/x105b/main
$P matrix compare --repeat 3 $N --output tools/perf/runs/x105b/main-compare
$P matrix compare --repeat 3 --native <0.15.0 build>/marathon --fake-adb target/release/fake-adb --output tools/perf/runs/x105b/v0150-compare
$P summarise tools/perf/runs/x105b > docs/perf/x105-summary.json
```

`dispatch` is 1, 8 and 32 devices × batch 1, 5 and 20 × normal and slow console. Tests run 100 ms ±50 %, and each
device gets max(12, 48/N) batches. `dispatch-graphite` is the normal-console set with Graphite, minus the
single-device batch-1 and batch-20 cases. `tail` is 1 000 and 10 000 results on 8 devices, batch 20, 2 ms tests.
`compare` is 32 devices, batch 5, normal console. The raw results (516 KB) are not committed. Their `summarise`
output is [x105-summary.json](x105-summary.json) (gap statistics pooled over the repeats; per-reporter
statistics over the repeats, with min, p50 and max).

## Item 1: dispatch gap

### What the code does (verified at `4de8c227`; the files are unchanged at `7362d27c`)

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
- The tracker fan-out (`artifact_io::on_io_scoped`) awaits every tracker's `test` call. That includes the remote
  metrics tracker's admission (`RemoteTracker::admit`, `analytics/event_pipeline.rs:464–504`, D51). While the
  backend's bounded FIFO has room, admission is a `try_reserve` and costs nothing. When a slow backend has filled
  the FIFO, one `test_ended` waits for a slot for up to `BackendLimits::send` (30 s). The first expiry opens the
  circuit for 5 minutes, and later events are rejected at once. So a slow backend can hold up a pool's queue
  actor, and with it every dispatch of that pool, for at most about 30 s per 5 minutes. This path was not run
  against a slow backend (see "Not measured").

### Gated: per-device idle gap at a batch boundary (ms)

p50 / p95 / max over all boundaries of 3 runs (n per cell: 141 boundaries for 1 device, 264 for 8, 1 056 for 32).
The p95 of every single run equalled the pooled p95, except two runs 1 ms above it (of 81 runs, compare series included). Load before the
runs: 0.75–2.61.

| devices | batch | normal console | slow console | Graphite sink (normal console) |
|---|---|---|---|---|
| 1 | 1 | 1 / 1 / 3 | 1 / 1 / 3 | – |
| 1 | 5 | 1 / 2 / 2 | 2 / 2 / 2 | 2 / 2 / 2 |
| 1 | 20 | 3 / 3 / 4 | 3 / 3 / 4 | – |
| 8 | 1 | 1 / 1 / 2 | 1 / 1 / 2 | 1 / 1 / 1 |
| 8 | 5 | 1 / 2 / 2 | 1 / 2 / 2 | 1 / 2 / 2 |
| 8 | 20 | 3 / 3 / 4 | 3 / 3 / 4 | 3 / 4 / 18 |
| 32 | 1 | 1 / 1 / 2 | 1 / 1 / 11 | 1 / 1 / 2 |
| 32 | 5 | 1 / 2 / 3 | 1 / 2 / 4 | 1 / 2 / 7 |
| 32 | 20 | 3 / 4 / 6 | 3 / 4 / 5 | 3 / 4 / 11 |

Main vs 0.15.0, 32 devices × batch 5, normal console, 3 runs each (load 1.10–1.46): 1 / 2 / 7 ms on main,
1 / 2 / 3 ms on 0.15.0. The two are the same; the max is a single boundary.

Reading:

- The gap depends on the **batch size, not on the device count**: about 1 ms at batch 1, 2 ms at batch 5 and
  3 ms at batch 20, the same for 1, 8 and 32 devices. That is about 0.1 ms per result in the batch. It matches the
  cost of processing a completed batch, not queueing behind other devices: 32 devices completing up to about 300 batches a
  second (batch 1) wait no longer than one device alone. Round 1's trace put the runner's own part at 0.06–0.07 ms per
  result (below), the rest being the fake's output and the next `am instrument`.
- **Slow console: no effect on the gaps**, as the code reading predicted. It delays the exit instead. Where the
  progress output outgrows the reader, the backlog drains after the last test. At 32 × batch 20 (7 680 tests,
  708 KB of progress in about 29 s against a 20 KB/s reader), the time from the last test end to exit went from
  2.72 s to 11.53 s (median of 3; spread under 0.03 s). In every other cell the reader kept up, and the tails were
  equal within 7 ms.
- **Graphite: no effect on the gaps.** The occasional higher max (11–18 ms, one boundary per series) is not
  repeated in any run's p95. The client opens one TCP connection per test (7 679 connections for 7 680 tests). That
  costs about 12 % more CLI CPU (5.89 → 6.63 s at 7 680 tests) and 0.1 s of wall time. A **slow or full** backend
  was not measured (see "Not measured").

### Indicative (round 1, trace build under load 5–9, one run each)

| run | devices × batch × tests | device completed → next `ExecuteBatch` at the pool, p50 / p95 | queue handling of one completed batch, p50 / p95 |
|---|---|---|---|
| lockstep (equal 20 ms tests) | 8 × 5 × 200 | 2.8 / 5.9 ms | 0.57 / 0.72 ms |
| jittered 2 ms tests (≈ 4 000 tests/s) | 8 × 20 × 10 000 | 1.5 / 4.3 ms | 1.38 / 2.35 ms |

Per result, `test_ended` cost 40 µs p50 (78 µs p95) over 10 000 results: 23 µs in the two console flushes (the
cross-runtime hop, not stdout) and 16 µs in the tracker fan-out. The metrics observation per batch was about
22 µs. The trace build was not rebuilt for round 2. Its share of the gated gap above is consistent: 20 results ×
0.07 ms = 1.4 ms of the 3 ms gap at batch 20.

## Item 2: report tail

### What the code does

`ExecutionReportGenerator::close` (`analytics/event_pipeline.rs:170–194`) runs the reporters one after another on
the I/O runtime, off core. `TestJsonReporter` (`report/reporters.rs:90–103`) runs one blocking job for
`create_file` (plan and create directories) and a second for the write, per final result. The HTML reporter
runs five blocking jobs per test (`reporters/html.rs:356–436`: screenshot target, screenshot/video lookup,
log target, log lookup with the page data, page write). The first reporter that calls `ExecutionReport::summary()` (JUnit, by slot order)
pays for computing the summary, which is **quadratic in the number of results** (`report.rs:104–160`, U263 in
[x105-findings.md](../wp/x105-findings.md)).

### Gated: time per reporter, 8 devices, local disk (ms)

Median (min–max) of 5 runs. Load before the runs: 3.39–3.60 (1 000 results) and 2.75–3.49 (10 000 results).

| step | 1 000 results | share | 10 000 results | share | 10k / 1k |
|---|---|---|---|---|---|
| last test end → device info done (includes reporter start-up) | 5.9 (5.6–6.8) | 2 % | 8.2 (5.8–9.6) | 0.2 % | |
| billing | 1.7 (1.1–2.5) | 0.5 % | 1.3 (1.0–1.6) | 0.04 % | |
| **JUnit** (includes the summary computation) | 10.2 (7.9–15.6) | 3 % | **541 (516–573)** | 15 % | **53×** |
| timeline | 1.4 (1.0–1.5) | 0.4 % | 4.0 (3.9–4.1) | 0.1 % | |
| raw JSON | 0.6 (0.5–0.7) | 0.2 % | 2.8 (2.7–2.8) | 0.1 % | |
| **test JSON** | 89.9 (80.9–96.4) | 26 % | **733 (720–751)** | 20 % | 8× |
| **Allure results** | 63.1 (62.5–71.4) | 18 % | **645 (635–725)** | 18 % | 10× |
| **HTML** | **172 (170–176)** | **49 %** | **1 730 (1 723–1 765)** | **47 %** | 10× |
| stdout summary → exit | 1.3 (1.3–6.6) | 0.4 % | 5.4 (5.2–6.1) | 0.1 % | |
| **last test end → exit** | **351 (335–361)** | | **3 678 (3 618–3 805)** | | 10.5× |

HTML dominates at both sizes, with just under half the tail. Test JSON and Allure follow at about a fifth each.
They and HTML scale linearly: about 0.17 ms per result for HTML and 0.06–0.07 ms for test JSON and Allure.
JUnit is the only non-linear step: 10 µs per result at 1 000 and 54 µs at 10 000. That is the quadratic summary.
Extrapolated from these two points, it takes about 2.1 s at 20 000 results and about 13 s at 50 000 (not
measured). Round 1's indicative 10 000-result tail (4.0 s under load 5–9) was 9 % high; its ranking stands.

### Gated: combined create+write prototype (scratch only)

The prototype adds a method to `OutputFiles` that plans the path, creates the directories, opens and writes the
file in **one** blocking job. `TestJsonReporter` uses it in place of `create_file` + `write`, so it makes one
blocking job per file instead of two. It was never committed. To cancel drift, it ran between two blocks of the
shipped build, each block 5 runs of `matrix tail` (load 2.30–3.46):

| test JSON reporter | shipped (before) | prototype | shipped (after) |
|---|---|---|---|
| 1 000 results | 88.3 (80.6–88.9) | 74.6 (67.7–106.7) | 86.3 (79.2–88.6) |
| 10 000 results | 709.5 (707.9–716.7) | 599.1 (595.1–622.4) | 717.3 (711.3–719.2) |
| whole tail at 10 000 | 3 612 | 3 479 | 3 620 |

Saving: **about 115 ms at 10 000 results** (−16 % of test JSON, −3.5 % of the tail), about 13 ms at 1 000. That is
**about 11 µs per blocking job saved**. The other reporters were unchanged within noise. The driver checked the
prototype's runs for validity (counts and statuses). It did not compare the test JSON files byte for byte with
the shipped build's; a real change would be covered by the reporter goldens.

From that per-hop figure: one blocking job per test in the HTML reporter instead of five would save about
4 × 11 µs × 10 000 ≈ **0.45 s at 10 000 results** (about a quarter of HTML) and about 45 ms at 1 000. This is
an estimate, not measured. The same change in Allure (create + write per result) would save about 0.1 s at
10 000.

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

The logs cannot split "dispatch + device pre-batch work" further (no debug lines). By the gated fake runs above,
the runner's share of a boundary is about 0.1 ms per result in the batch (2 ms for a 20-test batch): **under
0.03 % of the real idle time**. Android jobs and the 0.15.0-rc.1 logs carry no per-line timestamps, so no gap
can be derived from them.

## Conclusions and ranked fixes

1. **Dispatch gap: not worth changing (confirmed by the gated series).** A device waits about 1–3 ms at a batch
   boundary (p95 ≤ 4 ms) with 1, 8 or 32 devices. The wait grows with the batch size, not the device count. Neither
   a slow console nor a fast Graphite backend changes it, and 0.15.0 is the same. On the real farm a boundary
   idles for about 10 s, which is almost all xcodebuild start-up and device-side work. Moving `test_ended` out of
   the actor would save at most the ~2 ms per boundary, ≤ 0.03 % of a real boundary. It would also add a
   concurrency change to the most delicate actor. Expected win: none measurable on a real run. Two paths remain:
   - a **slow console** lengthens the exit, not the run: by the backlog at the reader's speed, at most 60 s
     (+8.8 s measured for 708 KB of progress against a 20 KB/s reader). CI log collectors read far faster than
     that, so this is not worth a change;
   - a metrics backend that has filled its FIFO can stall a pool's dispatch for up to 30 s once per 5-minute
     cool-off, by D51's design. If that matters, the place to fix it is the admission wait, not the actor. It was
     not measured.
2. **Report tail: small (0.35 s at 1 000 results, 3.7 s at 10 000), so only cheap clean-ups, in this order:**
   1. **make `ExecutionReport::summary()` linear (U263, scheduled as x106).** JUnit's linear part is at most about 0.1 s at 10 000
      results (10 µs per result at 1 000), so this saves 0.45–0.53 s of its 0.54 s. It is the only step whose cost
      grows faster than the result count: about 2.1 s at 20 000 and about 13 s at 50 000, extrapolated;
   2. **one blocking job per test in the HTML reporter** instead of five: about 0.45 s at 10 000 results
      (estimated from the measured 11 µs per job; about 45 ms at 1 000);
   3. **combined create+write for test JSON**: measured −115 ms at 10 000 results and −13 ms at 1 000. The same
      pattern in Allure would save about 0.1 s at 10 000;
   4. running independent reporters concurrently would hide part of the remaining ~2.5 s at 10 000. It changes
      the order of output files and of failure reporting, so it is a design change, not a clean-up. Not
      recommended now.

   Fixes 1–3 together: about 1.0–1.2 s of 3.7 s at 10 000 results (−27 to −32 %), about 0.06 s of 0.35 s at
   1 000. On a real
   run the tail is under a second of a multi-minute job; only fix 1 also removes a growth risk.
3. Retry rescans (item 3 of the review): not measured, by the brief's default.

## Not measured

- Apple vendors: the fake SSH worker runs every `ssh`/`rsync` through Python shims that cost about 58 ms each
  ([runner-overhead.md](runner-overhead.md)). They would dominate a dispatch gap of a few ms, so an Apple fake run
  would measure the shims, not the queue. The queue and the reporters are vendor-independent.
- A slow or full metrics backend (only a fast local sink).
- A gated trace breakdown of the runner's share (round 1's trace build, under load, stands for it).
- The HTML and Allure one-job estimates (derived from the test JSON prototype, not prototyped).
- Linux, and network or slow disks for the report tail.
- Item 3 (retry rescans).
