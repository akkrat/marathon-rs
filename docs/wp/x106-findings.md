# x106 findings: report-tail speed and recorder-refusal noise

Three items, approved by the project owner on 2026-11-22 (item 2b included). Items 1 and 3 change only how fast the
reports are written; every report file is the same as before. Item 2 changes the recorder logging and (round 2, 2b)
when a simulator recorder launches: never beside marathon's own previous recording on that simulator, which D80
already states ("one recording runs at a time") and D98's busy decision assumes. No contract change, so no D-row (D99
is unused): the change restores documented behaviour, and the videos it affects were lost to the bug (U265, U266).
The D98 section of `docs/deviations.md` gets an x106 note. Round 2 fixes the round-1 review's findings (below).

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
A hash map keyed by the `TestResult` finds an equal earlier key in O(1) (round 2, review P3: round 1 compared it
structurally with the earlier keys of its own test; `TestResult` and `Attachment` now derive `Hash`, consistent with
their derived `Eq`, and `Test`'s manual `Hash` already matches its `Eq`).
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

## Item 2 — U264, U265, U266: no overlap of marathon's own recorders; one line per failed recorder

**Facts checked in code.**

- Each test start launches a recorder (`ScreenRecordingListener::test_started` → `begin_recording`); on a supervised
  SSH worker through the D96 supervisor. Since D98 a recorder gets INT only after its start marker.
- `Peer::interpret` (supervisor `mod.rs:685–715` on the base) logged a WARN for every interpreted receipt carrying
  stderr and appended the restart advice for the busy text. Every receipt goes through `Lease::watch`, and the watcher
  ends at its first terminal receipt (a `Proof`, or `Unconfirmed`), so in the current code one lease logs at most one
  such line. x104's note that one recorder could warn more than once (a probe and a control request) is not
  reproducible from the code; the farm's lines on one simulator had different tokens, i.e. several recorders.
- D98's decision (`recording.rs` `recorder_busy` on the base): status 16, the busy text, no start marker, and no other
  launched, unfinished video recording of marathon's own on the device object.
- **How marathon's own recorders overlapped (U265; round 1 got this wrong).** Round 1 said that within one listener
  the next recorder cannot launch before the previous one has finished, because its `ready` awaits the previous
  recording's finalization. That holds only for the immediately previous recording. A recording whose stop is
  requested before its turn (its test ended while its `ready` was still waiting) returns `NotStarted` at once
  (`Recording::run`, the `deadline` arm before `ready`), and the listener's `stopping` slot then holds that one. The
  next test's `ready` waits only for it, so the next recorder launches while the first one is still live (for
  instance still starting). With five short tests and the first recorder still starting: test 2 does not start, and
  tests 3, 4 and 5 each launch and are refused: three refusals, as on the farm. The round-1 reviewer showed this with
  a scratch test; the round-2 witnesses below reproduce it on the round-1 head.
- **Consequences.** (a) A normal-length test after two or more short ones launched beside the live recorder and lost
  its video. (b) U266, pre-existing since D98: D98's own-recorder check looked at the device when the refused
  recorder's exit was handled. If that exit was seen after marathon's live recording had ended, it found none, logged
  the busy warning and turned video off on the simulator for the rest of the run, although the service was free.

**Design: one owner of the device's live recorder (U265, U266).** The device state holds `video_holder`: the one video
recording of marathon's own that may hold the simulator's video service. In `Recording::run`, after `ready`, a video
recording takes that turn under the device lock: if the holder's run has not ended (`done` not completed), it waits
for that, selected against its own stop request (`deadline`) and its batch's cancellation, either of which ends it
with `NotStarted`; then it looks again. Once the holder has ended it becomes the holder itself, and stays it until its
own run has ended, which is after its exit was seen and `report_end` ran (or after it gave up as `Unconfirmed`; the
existing check that refuses every launch on a device with an unconfirmed recording keeps applying, now inside the same
locked step). So a recorder never reaches simctl while another of marathon's own on the device is launched and not yet
ended: the invariant holds by construction, not by a check at reporting time. The listener's own chain (`ready`) is
unchanged; it still serves D80's path reuse and the unconfirmed-previous check.

**What waits on what.** `test_started` (the test's event path) only registers the recording and spawns its task; it
never awaits the turn, so no test is delayed. The turn wait runs in the recording's task. It ends at the latest when
its own test ends (its stop request) or its batch is cancelled; at that point it returns `NotStarted`, as a recording
stopped while waiting for `ready` already did. The things that wait for that recording (its test's video collection in
the D80 queue, the next recording's `ready`, `stop()` in `after_test_run`) therefore never wait longer than before for
it. The holder it waits for is bounded by the holder's own stop window, which started when the holder's test ended
(before this test started, on the same listener): INT as soon as the holder confirms its start (D98), then its INT,
TERM and KILL cutoffs. A normal-length test's recorder launches once the holder has ended, so its video can miss the
first moments of the test by the previous recorder's remaining start and finalization time, the cost D80 already
describes for the next test's recording; it is never truncated at the end (its own stop window is unchanged), and the
holder's video is not cut either. A short test that ends while waiting has no video ("Video recording did not start",
debug only; with `ON_FAILURE` or `ON_ANY` and a requested video, the existing "was not delivered: the recording did not
start" ERROR), which is what such tests mostly got before, minus the refused launch.

**Design: one decision point for reporting (U264).** `Recording::report_end`, called once at the end of every launched
video recorder's run with its outcome, whether it confirmed its start, D98's busy flag, its stderr, its termination
and its worker token (on a supervised worker):

| case | line (target `marathon_ios::busy_video`, visible with `debug: false`) |
|---|---|
| status 0 | none |
| busy text, no start, status 16 (D98) | marks the simulator busy and logs D98's existing WARN (text unchanged) the first time; nothing on a simulator already marked, as before. D98's own-recorder condition holds by construction (above), so the check is gone |
| an exit marathon stopped waiting for (`Unconfirmed`) | one WARN: `Simulator <serial>: the video recorder for <path> (worker recorder <first 8 of token>) did not report how it ended; marathon stopped waiting for it.`, with ` stderr: "…"` if any |
| any other end with non-empty stderr | one WARN: `Simulator <serial>: the video recorder for <path> (worker recorder <token8>) failed (status N / signal S / it did not start / no exit status). stderr: "…"` |
| any of the two above whose stderr has the busy text | the same line ending `. Stop the active recording or restart the simulator before recording again.` |

A recorder whose launch could not be confirmed (the open timed out, or failed without knowing that nothing started)
has no session and no `report_end`; `run` logs one WARN for it there (`… did not confirm its launch in time; marathon
stopped waiting for it.`, or `… failed to launch, and marathon could not confirm that nothing was started: <error>`).
A launch known to have started nothing stays `NotStarted` with its existing debug-only line.

**Round 1's INFO branch is gone.** The refusal it classified (busy text, no start, another recorder of marathon's own
live) can no longer happen: no recorder of marathon's own launches beside another. Keeping it as a defence would need
the own-recorder check it was built on, which U266 showed depends on exit order; without it, such a refusal (from a
recorder outside marathon's knowledge) is correctly D98's busy case. So (b) of the brief is unreachable, and the
witness for it (two listeners on one device, not the production path, review P2-2) is dropped rather than rewritten.

**(c) with the busy text** remains reachable: the busy text with a status other than 16, after a start, or on an
unconfirmed exit. That is when the service may really be stuck while D98 cannot decide, so the advice stays there.

**The token.** The supervisor's DEBUG line names the whole 32-hex token; the WARN names its first 8 characters
(`WorkerReport` now carries the token), enough to find the DEBUG line with `debug: true`.

**Exactly one line visible with `debug: false` per failed recorder.** Launched and ended: `report_end`, one of the rows
above. Launched but given up (open timeout, unknown open failure, stop window exhausted): one WARN from the recording;
a supervised receipt that arrives after that is the supervisor's DEBUG line only (review P3: before, nothing at default
level remained in that case). Never launched (stopped before its turn, or a launch known to have started nothing):
not a failed recorder; its existing debug-only line, unchanged.

**Sources of the stderr.** On a supervised worker the exit receipt's stderr (the supervisor bounds it at 8 KiB) in
`WorkerReport::stderr`, also kept from an `Unconfirmed` receipt. On a local simulator (the controller reads simctl's
output; the brief's "legacy, non-supervised" path) the recording keeps the first 8 KiB of the stderr it already reads
line by line (`keep_stderr`), and reports the same way. This adds lines a local run did not have: before, a failed
local recorder was reported only in D98's busy case. The Perl-less "legacy recorder" on an SSH worker (`run_legacy`,
pidfile) sends simctl's output to `/dev/null`, has no stderr and no exit status, and is unchanged; it takes the turn
like any other video recording.

**The supervisor** no longer logs recorder failures at WARN and no longer gives advice. When the watcher first keeps
a recorder's stderr it logs one DEBUG line with the token, `simulator <udid>` and `host:port` (U261's naming). The
target and its WARN floor are unchanged (other supervisor warnings use it), so x104's filter test is unchanged.

**Witnesses** (virtual time, `debug: false`, local and supervised-receipt paths; each run on the round-1 head
`e81e1209` in a scratch worktree with the new test file copied in, where every one fails as listed):

| test | scenario | on `e81e1209` |
|---|---|---|
| `short_tests_never_launch_a_recorder_beside_marathon_s_starting_one_u265` (`marathon-ios/tests/ios_listeners.rs`) | ONE listener; a fake video service that refuses a launch while another recorder is live (status 16, Xcode 26.6 busy text, at once). Test 1's recorder is still starting while tests 2–4 start and end within 50 ms each; test 5 is normal-length; test 1's recorder then starts and finalizes | fails: test 5's recording `Unfinalized` (its video lost); 3 refused launches |
| `a_refusal_seen_after_marathon_s_own_recorder_ended_cannot_turn_video_off_u266` (same file) | the same, but a refused recorder's exit is seen only after test 1's recorder has ended | fails: D98's busy WARN, video off |
| `a_recorder_marathon_stops_waiting_for_is_one_warning_u264` (same file) | a launch that never confirms (local), and a started recorder that never exits (local, and supervised with no receipt) | fails: no line (the supervised and open cases), or the wrong wording |
| `any_other_recorder_failure_is_one_warning_with_its_stderr_u264` (same file) | every recorder exits 1 with another refusal, and with the busy text; both paths | (round 1) fails on `c0425676`: no warning on either path |
| `a_busy_video_service_turns_video_off_for_the_simulator_once_d98` (same file, extended in round 1) | D98's busy case: exactly one line, the busy WARN; a status 16 without the busy text: one WARN per recorder without advice; local and supervised receipt | (round 1) fails on `c0425676` at the new count |
| `a_refused_recorder_is_one_debug_line_and_its_stderr_reaches_the_controller_u261_u264` (`marathon-apple/tests/ssh_supervisor.rs`) | a recorder refused on a fake supervised worker: debug off → no supervisor line; debug on → exactly one DEBUG line naming simulator and worker, no advice | (round 1) fails on `c0425676` |
| `a_busy_simulator_on_an_ssh_worker_turns_video_off_once_d98` (`marathon-cli/tests/ios_production.rs`, extended in round 1) | the real CLI and supervisor protocol on the fake SSH worker | (round 1) fails on `c0425676` at "no restart advice" |

The U265 and U266 witnesses assert in the order of what a user loses (D98's busy warning, then test 5's video), then
the cause (no line on D98's target, no launch beside the first recorder, exactly two launches, both finalized), then
that video stays on (a sixth recording launches and finalizes). On the round-1 head the local and the supervised
variants each fail at the first of those (checked separately).

## What was run

- `cargo fmt --check`; `cargo clippy -p marathon-core -p marathon-apple -p marathon-ios -p marathon-cli --all-targets
  -- -D warnings`: clean.
- `cargo test -p marathon-core`: 934 passed. `cargo test -p marathon-apple -p marathon-ios -p marathon-cli
  -p marathon-logging` (CLI end-to-end and differential suites included): 1 256 passed, 0 failed.
- `tools/differential/make_registry.py --check`, `tools/differential/test_evidence.py`, `scripts/check_plan_dag.py`:
  pass. `scripts/parity_matrix.py --check` fails, identically on main `7362d27c` (incidental, below); this WP does
  not touch the matrix.
- Round 2: `cargo fmt --check`; `cargo clippy -p marathon-core -p marathon-apple -p marathon-ios -p marathon-cli
  --all-targets -- -D warnings`: clean. `cargo test -p marathon-core -p marathon-ios -p marathon-apple
  -p marathon-logging`: 2 088 passed, 0 failed; `cargo test -p marathon-cli` (end-to-end, differential and
  `ios_production` suites): 104 passed, 0 failed. The three new witnesses (U265, U266, the stopped-waiting warning):
  50/50 runs each from the debug test binary, then 20/20 each with twelve busy-loop processes running. Revert checks:
  a scratch worktree at the round-1 head `e81e1209` with the new test file; every new witness and variant fails there.
  The local pre-commit check: clean.
- Not run (the orchestrator's gates): `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Not verified

- A real farm run, and Linux. That the farm's refusals were this overlap: the mechanism and the count match (above),
  but no farm log was rerun.
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
