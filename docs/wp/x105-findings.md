# x105 findings: dispatch gaps and the report tail (measurement only)

Base `7362d27c` (0.15.2; round 1 was on `4de8c227`, 0.15.1, and the measured paths are the same). No product
change, no D-row, no CHANGELOG entry. Results and method:
[docs/perf/x105-dispatch-and-report-tail.md](../perf/x105-dispatch-and-report-tail.md), summary data
[docs/perf/x105-summary.json](../perf/x105-summary.json).

## What was done

- New driver [`tools/perf/dispatch_report.py`](../../tools/perf/dispatch_report.py). It generates fake-ADB
  scenarios with N devices, T tests (remote test parser), fixed-size batches with one exact strict-mode
  expectation each, jittered test times, a normal or slow stdout reader, and an optional local Graphite sink. It
  derives per-device batch-boundary gaps from `raw.json` and splits the report tail per reporter by output mtimes,
  all from the unmodified CLI. `matrix` runs the x105 series and `summarise` aggregates them. Every run is gated
  on load < 4, with a bounded 30-minute wait and a 30-minute hang guard per run.
- Round 1: the idle gate never opened (stale simulator-runtime processes held the load above 5). There were only
  functional trial runs under load, a scratch trace build (µs `eprintln!` points in the pool, the queue actor,
  `test_ended` and around each reporter) and code reading of the claimed paths.
- Round 2, after the owner had the stale processes stopped: the full gated set on `7362d27c`, 121 runs, all valid,
  load 0.75–3.60 before every run. It covered `dispatch` (1/8/32 devices × batch 1/5/20 × normal/slow console),
  `dispatch-graphite`, main vs 0.15.0, the report tail at 1 000 and 10 000 results (5 runs each), and an A/B of a
  scratch-only combined create+write prototype for test JSON. Neither the trace build nor the prototype is
  committed.
- Real-farm cross-check from saved CI logs. The detailed analysis is kept outside the repository. The repository
  holds only the anonymised aggregate in the perf doc.

## Results (gated)

- **Dispatch gap**: p50 1–3 ms, p95 ≤ 4 ms per device at a batch boundary, for 1, 8 and 32 devices. It grows
  with the batch size (about 0.1 ms per result), not with the device count. A slow console and a fast Graphite
  sink leave it unchanged, and main equals 0.15.0. A slow console lengthens only the exit, by the progress
  backlog at the reader's speed (+8.8 s for 708 KB against a 20 KB/s reader; at most 60 s by design).
- **Report tail**: 0.35 s at 1 000 results and 3.7 s at 10 000 (median of 5, every run within 5 % of it). HTML takes 47–49 %,
  test JSON about 20–26 % and Allure about 18 %. JUnit takes 3 % at 1 000 but 15 % (0.54 s) at 10 000, because
  it is the first caller of the quadratic summary (U263). The combined create+write prototype measured −115 ms
  at 10 000 (−16 % of test JSON), which is about 11 µs per blocking job saved.
- Farm aggregate (iOS, 0.15.1 and a JVM control): the median idle at a batch boundary is 10–13 s, of which
  about 8–10 s is xcodebuild start-up. By the gated fake runs, the runner's share is about 2 ms per 20-test
  batch, under 0.03 %.

## What was not measured

- Apple fakes (their Python shims cost about 58 ms per call and would dominate). The queue and the reporters
  are vendor-independent.
- A slow or full metrics backend. That is the one path that can stall dispatch for seconds: up to 30 s per
  5-minute cool-off (D51).
- A gated trace breakdown of the runner's share of the gap. Round 1's trace build under load stands for it,
  labelled indicative.
- The HTML and Allure one-job savings are estimates from the prototype's per-job cost, not prototypes. The
  prototype's test JSON files were not compared byte for byte with the shipped build's.
- Linux, network or slow disks, and item 3 (retry rescans; the owner's default is to leave it).

## Verified claims of the outside review

1. Dispatch gap: the path exists as described (serial actor, progress lock, per-result `test_ended`), but its
   cost is about 0.1 ms per result. One part of the claim is wrong: "two console flushes" do not wait for the
   console. `QueueWriter::write` sends over an unbounded channel, so a slow stdout reader delays only the exit,
   not dispatch. The gated slow-console series confirms this.
2. Report tail: the reporters are serial, and test JSON makes two blocking jobs per file, as claimed. The larger
   costs are the HTML reporter, which makes five blocking jobs per test, and the quadratic summary.

## Recommendation (unchanged from round 1, now with gated numbers)

No change to dispatch. For the tail, cheap clean-ups only, in this order:

1. a linear `ExecutionReport::summary()` (U263, x106): saves 0.45–0.53 s at 10 000 results, and about 2 s at
   20 000 and 13 s at 50 000, extrapolated;
2. one blocking job per test in the HTML reporter: about 0.45 s at 10 000 (estimated);
3. combined create+write in test JSON: −0.115 s at 10 000 (measured), about 0.1 s more for Allure (estimated).

Together these save about 1.0–1.2 s of 3.7 s at 10 000 results, and about 0.06 s of 0.35 s at 1 000.

## Incidental bugs

- **Proposed U263: `ExecutionReport::summary()` is quadratic in the number of test events**
  (`crates/marathon-core/src/report.rs:104–160`). It has three O(n²) parts. For every final result,
  `compile_pool` scans all of the pool's events for its other attempts. It finds the result in `retries` with a
  linear `find` that compares `TestResult`s structurally. It deduplicates the passed, ignored and failed name lists
  with `Vec::contains`. The Kotlin marathon has the first part as well (`ExecutionReport.kt:78–80`,
  `filter` per test) but uses a map and `toSet()` for the other two. The cost lands in the first reporter that
  asks for the summary, which is JUnit. Gated: JUnit takes 10.2 ms at 1 000 results and 541 ms at 10 000 (10 →
  54 µs per result, 53× for 10× the results). Extrapolated: about 2.1 s at 20 000 and about 13 s at 50 000. It runs
  on the I/O runtime, not on core, so it is CPU time added to the tail and not a lifecycle violation. Not fixed in
  this WP (measurement only); x106 takes it. A fix is local: index the events by test once, and keep the name lists
  insertion-ordered with a hash set beside them. Output must stay byte-identical; the reporter goldens cover it.
- The machine's stale simulator processes (round 1) were an environment issue, not a repository bug. They have
  since been stopped.
