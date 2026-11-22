# x106 findings: report-tail speed and recorder-refusal noise

Three items, approved by the project owner on 2026-11-22. Items 1 and 3 change only how fast the reports are written;
every report file is the same as before. Item 2 changes only logging. No contract change, so no D-row (D99 is unused);
the D98 section of `docs/deviations.md` gets an x106 note because its x104 note described the supervisor's warning.

## Item 1 — U263: a linear `ExecutionReport::summary()`

**Defect** (verified at `crates/marathon-core/src/report.rs:104–160` on `c0425676`). Three quadratic parts, as x105
described: per final result, `compile_pool` scanned all of the pool's events for its other attempts; it looked the
result up among the retries keys with a linear structural `==`; and it deduplicated the passed, ignored and failed
name lists with `Vec::contains`. A fourth, smaller one: each pool filtered all test events again.

**Design.** `summary()` walks the connected events once (pools in first-seen order, devices distinct by serial,
first seen kept, through a `HashSet` per pool) and the test events once, grouping them by pool (events of a pool with
no connected device are dropped, as before). `compile_pool` indexes the pool's events by `Test` once. A final result's
attempts are its test's events minus its own allocation, in event order. The retries keep the `LinkedHashMap`
semantics of the Kotlin marathon's `toMap()`: an equal key replaces the value and keeps the first key's position.
Equal `TestResult`s have equal tests, so a key is compared structurally only with the earlier keys of its own test.
The name lists keep insertion order with a `HashSet` beside them.

**Invariants and the Kotlin semantics** (`ExecutionReport.kt`): pools are `deviceConnectedEvents.map { poolId }.distinct()`;
devices `distinctBy { serialNumber }`; `passed`/`ignored`/`failed` are `toSet()` (`LinkedHashSet`, first-seen order)
of the final, non-incomplete results; `failed` is "not passed, ignored or assumption failure" of those, which is
`Failure` only; the raw lists keep duplicates; `retries` is `tests.map { … }.toMap()`. The Rust output was already
this; the rewrite keeps it, order and tie-breaks included. The remaining cost per final result is its own test's
events, which is the size of its output (its attempts).

**Witness** (`crates/marathon-core/tests/execution_report_summary.rs`, Rust-only, no parity row):

| test | what | on the base `c0425676` |
|---|---|---|
| `summary_keeps_first_seen_order_and_map_replacement_semantics` | two pools whose connected events interleave, a repeated device, an orphan pool, a non-final then final attempt, two equal final results in distinct allocations (one key at the first's position, the second's attempts), one allocation final twice, incomplete, ignored and assumption-failure results, and durations with zero times; the rendered summary is pinned | passes (written first, run on the old code to show the pinned output is what the old code computes; my first hand-derived durations were wrong and were corrected from that run) |
| `summary_of_fifty_thousand_results_finishes` | 50 000 events, two pools, retries and mixed outcomes; a 10 s hang guard only (docs/testing.md) | fails: about 34 s in a debug build; after the fix 0.13 s |

**Measured** (release build, a scratch micro-benchmark not committed: one pool, 8 devices, unique passed results;
median of 5; load 3.6 → 2.2 during the runs): `summary()` took 428 ms at 10 000 results and 18.6 s at 50 000 before,
5.4 ms and 22 ms after. In the CLI runs below, the JUnit reporter (the first to ask for the summary) went from 549 ms
to 71 ms at 10 000 results and from 10.0 to 2.8 ms at 1 000.

## Item 3 — fewer blocking jobs in the reporters

**What the code did** (verified on `c0425676`):

- `TestJsonReporter` awaited `create_file` (one blocking job: plan, create the directories) and then `write` (a second
  one) per final result. As the brief says.
- The HTML reporter made five blocking jobs per test: the screenshot's `create_file`, the screenshot and video
  lookup, the log's `create_file`, the log lookup with the page data, and, in a second loop over all tests, the page
  writes. As the brief says.
- **The Allure reporter already made one blocking job per result** (create the results directory and write the file);
  the x105 perf doc's "create + write" for Allure is not what the code does. Its only other hop is one blocking job
  per attachment for the empty check (none in the measured scenario). Left unchanged: there was nothing cheap to
  combine, and its tail did not change in the measurement (below).

**Design.**

- `OutputFiles` gains `create_file_blocking(&self, &FileTarget) -> OperationResult<PathBuf>`: the same plan,
  directories, error log and errors as `create_file`, made on the calling thread, for a caller that already runs as a
  blocking job on the I/O runtime. `FileManagerOutput::create_file` and it share `materialize_now`.
- `ReporterFiles::create_and_write(target, text)` does `create_file_blocking`, then `text()` (the JSON serialisation,
  which used to run between the two awaits), then the write, in one blocking job. The steps, their order and every
  error are those of `create_file` + `json` + `write`; only the hop between them is gone. `TestJsonReporter` uses it.
  `DeviceInfoJsonReporter` (one file per device) is unchanged, and so is its failure-order test.
- The HTML reporter keeps its two phases (every test's lookups, then every test's pages) and does each phase in one
  blocking job per chunk of 64 tests (`TESTS_PER_JOB`), each test's steps in their old order. One job per test that
  did both lookups and pages would change the order of file operations between tests (a page written before the next
  test's directories are created), and with it which error a run with two failures reports first; chunking keeps the
  exact order and costs fewer hops. A chunk is about 64 × 0.12 ms of file work, so a job stays a few milliseconds and
  cancellation still takes effect between chunks (docs/design/lifecycle.md: blocking work on the I/O runtime only).
- Reporters stay serial; `ExecutionReportGenerator::close` is unchanged.

**Measured.** `tools/perf/dispatch_report.py matrix tail --repeat 5` (8 devices, batch 20, 2 ms tests, fake ADB),
local disk, every run gated on load < 4 and valid. Three series to cancel drift: base `c0425676`, head, base again.
Release builds of both; the fake ADB is the head's (unchanged). Median (min–max) in ms; load before the runs at most
3.93, 2.62, 2.50 (1 000) and 3.77, 3.00, 3.18 (10 000).

| reporter | 1 000: base | 1 000: head | 1 000: base again | 10 000: base | 10 000: head | 10 000: base again |
|---|---|---|---|---|---|---|
| device info | 5.4 (4.2–6.1) | 5.5 (5.4–6.2) | 5.4 (4.2–7.3) | 9.6 (4.7–10.4) | 9.7 (5.5–12.1) | 9.8 (9.5–10.7) |
| billing | 1.6 | 1.7 | 1.6 | 1.5 | 1.6 | 1.6 |
| **JUnit** (incl. the summary, item 1) | 10.0 (6.1–11.1) | **2.8** (2.4–3.4) | 9.9 (8.0–11.0) | 549 (510–644) | **71** (61–74) | 532 (515–566) |
| timeline | 1.3 | 1.4 | 1.5 | 4.2 | 4.2 | 4.0 |
| raw JSON | 0.7 | 0.7 | 0.7 | 2.8 | 2.8 | 2.8 |
| **test JSON** | 88.6 (80.3–94.0) | 88.0 (84.3–91.2) | 91.4 (90.0–106.9) | 753 (730–810) | **685** (672–702) | 740 (736–759) |
| Allure (unchanged) | 68.6 | 67.0 | 66.7 | 674 | 680 | 667 |
| **HTML** | 180 (176–182) | **121** (120–127) | 178 (175–182) | 1 782 (1 751–1 795) | **1 249** (1 231–1 269) | 1 800 (1 786–1 825) |
| stdout summary → exit | 1.4 | 1.5 | 1.5 | 5.4 | 5.4 | 5.3 |
| **last test end → exit** | 354 (352–364) | **293** (283–299) | 361 (352–366) | 3 782 (3 713–4 004) | **2 691** (2 679–2 760) | 3 764 (3 732–3 817) |

The tail drops by about 1.1 s (−29 %) at 10 000 results and 65 ms (−18 %) at 1 000. HTML saves about 0.54 s at
10 000 (−30 %, a little more than x105's 0.45 s estimate) and 58 ms at 1 000. Test JSON saves about 60 ms at 10 000
(−8 %), about half the 115 ms of x105's prototype, and nothing measurable at 1 000. JUnit saves 0.47 s at 10 000.

**Output unchanged.** The reporter goldens and every `marathon-core` reporter test pass unchanged. A whole-tree
byte compare of two CLI runs cannot be exact: run to run, the fake's timestamps, Allure's UUIDs and the sandbox path
differ. So three 1 000-result runs were kept per build (`run --keep`, base twice, head once) and compared after
replacing digit runs and UUIDs (paths and contents): 4 027 files each; head and base differ in exactly the two files
in which two base runs differ from each other, Allure's `environment.xml` (it carries the sandbox's output path) and
`html/timeline/index.html` (equal length and words; the order of tied timestamps varies). Neither reporter is
touched by this WP. Every other file (test JSON, JUnit, raw JSON, Allure results, all HTML pages) is equal under that
normalisation, file names included.

## Item 2 — U264: one line per failed recorder, decided by the recording

**Facts checked in code.**

- Each test start launches a recorder (`ScreenRecordingListener::test_started` → `begin_recording`); on a supervised
  SSH worker through the D96 supervisor. Since D98 a recorder gets INT only after its start marker.
- `Peer::interpret` (supervisor `mod.rs:685–715` on the base) logged a WARN for every interpreted receipt carrying
  stderr and appended the restart advice for the busy text. Every receipt goes through `Lease::watch`, and the watcher
  ends at its first terminal receipt (a `Proof`, or `Unconfirmed`), so in the current code one lease logs at most one
  such line. x104's note that one recorder could warn more than once (a probe and a control request) is not
  reproducible from the code; the farm's two lines on one simulator had different tokens, i.e. two recorders. The new
  logging is tied to the set-once stderr, so a repeat cannot happen either way.
- D98's decision (`recording.rs` `recorder_busy`): status 16, the busy text, no start marker, and no other launched,
  unfinished video recording of marathon's own on the simulator's device object.
- **Not confirmed: how marathon's own recorders overlap.** Within one `ScreenRecordingListener` the next recorder
  cannot launch before the previous one has finished: its `ready` future awaits `previous.finalization()`, which
  completes only after the previous recorder's process ended; by then the previous recording counts as ended for D98.
  So two live recordings on one device object need two listeners (or captures) at once, which the batch flow does not
  obviously produce. The farm run (refusals inside the last 0.3 s of a first batch of five skipped tests, D98 not
  firing, the simulator recording fine later) shows that D98's other three conditions held and so its own-recorder
  check must have; the code path that made two recordings live together is not established here. The
  classification below uses D98's own check, so whatever made it hold on the farm now gives the INFO line.

**Design: one decision point.** `Recording::report_end` (in `recording.rs`), called once at the end of every video
recorder's run with its outcome, whether it confirmed its start, D98's busy flag, its stderr and its termination:

| case | line (target `marathon_ios::busy_video`, visible with `debug: false`) |
|---|---|
| status 0 | none |
| busy text, no start, another recorder of marathon's own live (D98's own check, unchanged, under the device lock) | **(b)** one INFO: `Simulator <serial>: the video recorder for <path> overlapped marathon's previous recording on this simulator; this test's video is not recorded.` No advice; video stays on |
| busy text, no start, status 16, none of marathon's own live | **(a)** D98: marks the simulator busy and logs its existing WARN (text unchanged) the first time; nothing on a simulator already marked, as before |
| any other end with non-empty stderr | **(c)** one WARN: `Simulator <serial>: the video recorder for <path> failed (status N / signal S / an unconfirmed exit). stderr: "…"`, with `. Stop the active recording or restart the simulator before recording again.` only if the stderr has the busy text |

The serial is `<udid>@<worker host id>` (`user@host:port` on SSH, `local` on this machine), so every line names the
simulator and the worker. **(c) with the busy text** remains reachable: the busy text with a status other than 16 and
no own recorder (a future simctl that changes its exit status, or an unconfirmed exit), or after a start. That is the
case in which the service may really be stuck while D98 cannot decide, so the advice stays there.

INFO, not DEBUG, for (b): the line explains why a test has no video, which matters with `ON_FAILURE` when such a
short test fails; it replaces a WARN the farm showed about three times per video run, and states that no action is
needed. An owner preferring silence can ask for DEBUG; it is a one-word change.

**Sources of the stderr.** On a supervised worker the exit receipt's stderr (the supervisor bounds it at 8 KiB) in
`WorkerReport::stderr`, now also kept from an `Unconfirmed` receipt (the supervisor warned for those too before).
On a local simulator (the controller reads simctl's output; the brief's "legacy, non-supervised" path) the recording
now keeps the first 8 KiB of the stderr it already reads line by line (`keep_stderr`), and reports the same way. This
adds lines a local run did not have: before, a failed local recorder was reported only in D98's busy case. The
Perl-less "legacy recorder" on an SSH worker (`run_legacy`, pidfile) sends simctl's output to `/dev/null`, has no
stderr and no exit status, and is unchanged.

**The supervisor** no longer logs recorder failures at WARN and no longer gives advice. When the watcher first keeps
a recorder's stderr it logs one DEBUG line with the token, `simulator <udid>` and `host:port` (U261's naming). The
target and its WARN floor are unchanged (other supervisor warnings use it), so x104's filter test is unchanged.

**2b (avoiding the overlap) — not implemented.** Within a listener the next recorder already waits for the previous
one's finalization, so there is no identified wait to add; a wait keyed on "marathon's previous recorder on this
simulator" across device objects or listeners would need the mechanism above to be established first, and could
delay a normal-length test's recorder behind another listener's stop window. The cost of not doing it is the video of
very short tests that overlap, which `ON_FAILURE` discards unless such a test fails; the INFO line now says so.

**Witnesses** (each run on the base `c0425676` in a scratch worktree with the new tests copied in; all fail there):

| test | scenario | on the base |
|---|---|---|
| `a_refusal_that_overlaps_marathon_s_own_recording_is_one_info_line_and_video_stays_on_u264` (`marathon-ios/tests/ios_listeners.rs`, virtual time, debug off) | two listeners on one simulator: the first one's recorder keeps running; the second's is refused with Xcode 26.6's busy text and status 16; local and supervised-receipt paths | fails: no line (0 ≠ 1) |
| `any_other_recorder_failure_is_one_warning_with_its_stderr_u264` (same file) | every recorder exits 1 with another refusal, and with the busy text; both paths | fails: no warning on either path |
| `a_busy_video_service_turns_video_off_for_the_simulator_once_d98` (same file, extended) | D98's busy case: exactly one line, the busy WARN; a status 16 without the busy text: one WARN per recorder without advice; now also on the supervised-receipt path | fails at the new count (0 ≠ 2) |
| `a_refused_recorder_is_one_debug_line_and_its_stderr_reaches_the_controller_u261_u264` (`marathon-apple/tests/ssh_supervisor.rs`; renamed from x104's `…_with_the_busy_hint_u261_u262`) | a recorder refused on a fake supervised worker: debug off → no supervisor line; debug on → exactly one DEBUG line naming simulator and worker, no advice; the receipt's stderr reaches `WorkerReport` | fails: the WARN with advice shows with debug off |
| `a_busy_simulator_on_an_ssh_worker_turns_video_off_once_d98` (`marathon-cli/tests/ios_production.rs`, extended) | the real CLI and supervisor protocol on the fake SSH worker: busy → no recorder-failure line besides D98's; status 16 without the busy text → exactly three `failed (status 16)` WARNs for three recorders; no restart advice anywhere | fails at "no restart advice" |

The (a)/(b)/(c) witnesses run on both stderr sources: the local stream and the supervised exit receipt
(`exiting_recorder` builds the session with a `WorkerReport`, setting the stderr before the exit status as the
supervisor does). The overlap needs two live recordings on one device; the test makes them with two listeners.

Stress (the head's debug test binaries): the three iOS witnesses and the supervisor witness 50/50
each, then 20/20 each with twelve busy-loop processes running.

## What was run

- `cargo fmt --check`; `cargo clippy -p marathon-core -p marathon-apple -p marathon-ios -p marathon-cli --all-targets
  -- -D warnings`: clean.
- `cargo test -p marathon-core`: 934 passed. `cargo test -p marathon-apple -p marathon-ios -p marathon-cli
  -p marathon-logging` (CLI end-to-end and differential suites included): 1 256 passed, 0 failed.
- `tools/differential/make_registry.py --check`, `tools/differential/test_evidence.py`, `scripts/check_plan_dag.py`:
  pass. `scripts/parity_matrix.py --check` fails, identically on main `7362d27c` (incidental, below); this WP does
  not touch the matrix.
- Not run (the orchestrator's gates): `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Not verified

- A real farm run, and Linux. The overlap mechanism on the farm (above).
- Allure and HTML with attachments (screenshots, videos, logs) in the timing runs: the scenario has none. The golden
  tests cover their output.

## Golden diffs

None.

## Incidental bugs

- `scripts/parity_matrix.py --check` against the Kotlin checkout at `e47352f2` reports `docs/parity-matrix.csv` out of
  date ("row no longer found, was ported" for `CodeCoverageTargetsTest` and `SkipCopyingDerivedDataTest` rows), on
  main `7362d27c` as well: `CodeCoverageTargetsTest.kt` is absent from that checkout, and the
  `SkipCopyingDerivedDataTest` case those rows name is not found in its file. Not changed here.
- The x105 perf doc says the Allure reporter awaits `create_file` and `write` separately per result; it makes one
  blocking job per result (plus one per attachment for the empty check).
