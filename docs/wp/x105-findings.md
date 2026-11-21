# x105 findings: dispatch gaps and the report tail (measurement only)

Base `4de8c227` (0.15.1). No product change, no D-row, no CHANGELOG entry. Results and method:
[docs/perf/x105-dispatch-and-report-tail.md](../perf/x105-dispatch-and-report-tail.md).

## What was done

- New driver [`tools/perf/dispatch_report.py`](../../tools/perf/dispatch_report.py). It generates fake-ADB
  scenarios with N devices, T tests (remote test parser), fixed-size batches with one exact strict-mode
  expectation each, jittered test times, a normal or slow stdout reader, and an optional local Graphite sink. It
  derives per-device batch-boundary gaps from `raw.json` and splits the report tail per reporter by output mtimes,
  all from the unmodified CLI. `matrix` runs the x105 series and `summarise` aggregates them. Every run is gated
  on load < 4, with a bounded 30-minute wait.
- A scratch-only trace build (µs `eprintln!` points in the pool, the queue actor, `test_ended` and around each
  reporter), plus a scratch-only prototype that creates and writes each test JSON file in one blocking job.
  Neither is committed.
- Code reading of the claimed paths at `4de8c227`. Line references are in the perf doc.
- Real-farm cross-check from saved CI logs. The detailed analysis is kept outside the repository; the
  repository holds only the anonymised aggregate in the perf doc.

## What was measured, what was not

- **Not measured: every gated series.** The 1-minute load average never fell below 4 during a bounded
  30-minute wait (lowest 4.68). Four stale simulator-runtime and diagnostics processes, started two to three days
  earlier, each used about one core throughout, with no simulator booted. Other work also ran an Android emulator.
  Nothing was killed. The driver's `matrix` gate also expired without a run. The p50/p95 tables, the slow console,
  Graphite, the main-vs-0.15.0 comparison and the report tail at 1 000 results are therefore missing.
- **Indicative only**: functional trial runs of the driver under load 5–9, one run each (perf doc tables). They agree
  with the code reading. The queue actor costs about 0.07 ms per result at a boundary, so 1.4 ms per 20-test
  batch. Its console flushes cannot block on stdout. The report tail at 10 000 results is about 4 s: HTML about
  1.9 s, test JSON 0.8 s, Allure 0.7 s, and JUnit 0.6 s, which is the summary computation. The combined
  create+write prototype saved about 0.15 s.
- Farm aggregate (iOS, 0.15.1 and a JVM control): the median idle at a batch boundary is 10–13 s, of which about
  8–10 s is xcodebuild start-up. The runner's queue work is in the ms range. Android and 0.15.0-rc.1 logs have no
  per-line timestamps.
- Not attempted: Apple fakes (their Python shims cost about 58 ms per call and would dominate), a slow metrics
  backend, Linux, item 3 (retry rescans; the owner's default is to leave it).

## Verified claims of the outside review

1. Dispatch gap: the path exists as described (serial actor, progress lock, per-result `test_ended`). One part of
   the claim is wrong: "two console flushes" do not wait for the console. `QueueWriter::write` sends over an
   unbounded channel, so a slow stdout reader delays only the exit (by at most the 60 s completion deadline), not
   dispatch. The real multi-second stall path is the remote metrics admission when a backend's FIFO is full: at
   most 30 s per 5-minute cool-off (D51 design). It was not measured.
2. Report tail: the reporters are serial and test JSON makes two blocking jobs per file, as claimed. The
   larger costs are the HTML reporter, which makes about five blocking jobs per test, and the quadratic summary
   (below).

## Recommendation

No change to dispatch. For the tail, if anything: (1) a linear `ExecutionReport::summary()` (about 0.6 s at
10 000 results, quadratic beyond); (2) one blocking job per test in the HTML reporter (estimated 0.5–0.7 s at
10 000); (3) combined create+write in test JSON and Allure (about 0.15 s each at 10 000). The gated series
should be run once the machine is idle, before any of these is scheduled.

## Incidental bugs

- **Proposed U263: `ExecutionReport::summary()` is quadratic in the number of test events**
  (`crates/marathon-core/src/report.rs:104–160`). It has three O(n²) parts. For every final result,
  `compile_pool` scans all of the pool's events for its other attempts. It finds the result in `retries` with a
  linear `find` that compares `TestResult`s structurally. It deduplicates the passed, ignored and failed name lists
  with `Vec::contains`. The Kotlin marathon has the first part as well (`ExecutionReport.kt:78–80`,
  `filter` per test) but uses a map and `toSet()` for the other two. The cost lands in the first reporter that
  asks for the summary, which is JUnit. Indicative (under load): 0.5 ms at 200 results, 605 ms at 10 000
  (2.5 → 60 µs per result). Extrapolated: about 2.4 s at 20 000 and about 15 s at 50 000. It runs on the I/O
  runtime, not on core, so it is CPU time added to the tail and not a lifecycle violation. Not fixed (measurement
  WP). A fix is local: index the events by test once and keep the name lists insertion-ordered with a hash set
  beside them. Output must stay byte-identical; the reporter goldens cover it.
- The machine's stale simulator processes (above) are an environment issue, not a repository bug. They block
  every load-gated measurement on this Mac until someone removes them.
