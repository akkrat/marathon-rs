# WP x73: release documentation for 0.14.0-rc.1

Base: `f2a16d3e`. Documentation only: no code, goldens, fixtures or PLAN changes. The release candidate ships D68–D72 and
D74–D77 (D73 was reserved and not used) and the U-fixes recorded with them (WPs x61–x72).

## What was added

- `CHANGELOG.md`: `## Unreleased` became `## 0.14.0-rc.1`, structured like 0.13.0: an intro, "Check before you upgrade",
  the existing bullets grouped under Runs and queue, Android, iOS and macOS, and Flakiness and results, then Evidence and
  Known limits. `release.changelog_section('0.14.0-rc.1')` returns it; `## 0.13.0` and older are byte-identical.
  Changes to the existing bullets: U refs added where a bullet lacked them (D68 → U168, the outage bullet → U160, U166,
  D74's preparation bullet → U179, the KILL warning → U183), and the D68 bullet now says the progress-line warning is shown
  with `debug: true` (see Disagreements).
- `docs/deviations.md` intro: D68–D72 and D74–D77 ship in 0.14.0-rc.1; the 0.13.0 sentences and "covers D1–D77 (D73 was
  reserved and not used)" are unchanged.
- `docs/migration.md`: one sentence before the D68 notes says they apply when moving from 0.13.0 to 0.14.0-rc.1 as well
  as from Kotlin (D71 is in the table). The "need no action" list now has D64–D66, D68–D70 and D74–D77; the "action of
  their own" list gains D67, D71 and D72. The D72 note says 0.13.0 rejects `fixed` too and that it must be removed
  before rolling back.
- `README.md`: 0.13.0 stays the current release; 0.14.0-rc.1 is named as a release candidate with run-stability and
  device-handling fixes (D68–D77), tested only with fakes and test harnesses so far.

## "Check before you upgrade", verbatim

- **Probability flakiness without history (D71):** with `flakinessStrategy: probability`, history that cannot be read
  (analytics disabled, a backend that could not be set up, a failed success-rate query) no longer gives every test
  `maxCount` extra runs; it adds none and logs one warning saying why. If a job relied on disabled analytics to run every
  test several times, switch it to `flakinessStrategy: {type: fixed, count: N}` (D72).
- **`fixed` is native-only (D72):** the Kotlin marathon and 0.13.0 reject a Marathonfile that uses `type: fixed`. Remove
  it before rolling back to either.
- **Fewer executions in reports (D69, D70, D75):** a test that has finished is no longer run again, a batch whose
  Android device is lost keeps the tests that finished, and a JUnit3 test with an array-valued annotation in a multi-dex
  APK is listed once. Reports, dashboards and checks that count executions, uncompleted attempts or tests can show lower
  numbers. A run with a small `uncompletedTestRetryQuota` that used to fail after an adb outage can now pass.
- **`fast: false` reports follow the verdict (D77):** with `executionStrategy.fast: false`, the summary's first block, the
  per-test result files, JUnit and HTML show the run that decided a test's verdict (its first pass under `ANY_SUCCESS`,
  its first failure under `ALL_SUCCESS`), not its last run, so they agree with the exit code. A test's place in the JUnit
  and HTML order can move. Read `raw.json` for every run.
- **A failed queue exits instead of hanging (D68):** a run whose test queue fails now ends with exit 1 (0 with
  `ignoreFailures: true`), writes its reports and logs `Marathon run failed` with the cause, where it used to hang until
  killed. A console that refuses a progress line no longer stops the run; that line is lost.
- **Logs and worker checks:** an SSH session closed only after KILL now logs a warning with `debug: false` (U183). An iOS
  unit-test bundle built inside its app is pushed to the worker once, so a wrapper that counts transfers sees one fewer
  (D76). Disposing of an iOS simulator can take up to 70 s longer while it stops a stuck preparation (D74).

## Known limits: what changed from 0.13.0's list

Removed:

- "A batch that D22's hold kept waiting is charged an attempt when its server is given up, so with
  `uncompletedTestRetryQuota: 0` tests can fail without having run (U160)." Fixed by D70 (WP x63); U160 is `fixed`.
- "A queue that ends for a reason other than a metrics failure, such as a broken console, leaves the run hanging (U168)."
  Fixed by D68 (WP x61); U168 is `fixed`.

Rewritten:

- D30/U183: "… with only a debug line saying how long the close took" became "A close that needed KILL logs a warning
  naming the command, also with `debug: false` (D30, U183); the process itself is not ended." WP x65 added the WARN on
  `marathon_apple::session_close` with a WARN floor (checked in `crates/marathon-logging/src/lib.rs`); the process can
  still survive on a server that ignores signals.

Added (remaining limits the batch documents):

- A test whose end the listeners are still handling when its Android device is lost can lack its screen capture and
  profiling files (D70 text, x68 "Not verified" and the pinned test
  `a_test_ending_when_the_batch_is_lost_counts_without_later_attachments_u197`).
- A Graphite backend that cannot be set up still fails the run, where InfluxDB falls back to no history (D71 text, x62
  owner decisions: kept).
- A JUnit3 `TestCase` method that also carries `@Test` is still listed twice, as in Kotlin (x66 incidental, D75 text).

Kept unchanged, after checking: platform builds, signing, the static Linux binary (its sentence names 0.13.0-rc.1, which
is still accurate), `libxctest-parser` (D20), U103 (still `open`), no shadow/canary results, D22's restarted emulator
(D22 still calls it a known limit; x64 changed only how returning devices are restored), D38's probe timings, D48/D30's
retired simulator, D48's Android device that stays in adb's list (D48's text unchanged by the batch), D25 and A20.

Not added: x64's "a device that goes `offline` again during its own return probe may be set up again" (the old code
always did this; now only when the failure wins a race, so not a regression), x69's iOS note (a disposed new simulator
stays in the device map; only the generic device seam reaches it) and x71's "not covered" note (nothing exposed today).

## Evidence section: sources

- Reverted-fix witnesses: the "Verified" / "What was run" sections of x61, x62, x63, x64, x65, x66, x68, x69, x70, x71
  and x72 each record their witnesses failing with the fix reverted.
- Harnesses: Stub vendor whole runs (x61 run-level witness; D72/D77 scenarios in x70), fake adb and the virtual-clock
  reconnect harness (x63, x64, x68), the fake worker for Apple sessions and simulators (x65).
- JVM recordings: x61 "JVM queue oracle" (D69 sets aside K1, K2, K4 with an exact negative test), x70 (22 progress rows,
  13 queue flags), x66 (8 repeats of `abstract-class-in-second-dex.apk`).
- Goldens: x61, x62, x63, x65, x66 and x70 report none changed; x64 regenerated the registry (only a hash moved); x68
  added `fixtures/contract/virtual-runs/android-lost-after-start.json`.
- Reviews: one round each by Muse (x62, x64, x65, x66, x69, x70, x71, x72), two by Sol (x61, x68), one by Sol (x63).
- Gates: the merge commits of x61–x66 record local and remote (Linux build box) gates passing; those of x68–x72 do not
  record a gate, and x71/x72 say "the gates at merge". CI (`gate.yml`, macOS and Linux) passed on every merge commit of
  x61–x72 except `6bbe2c75` (x62 merge, U198) and `d5efdca1` (x65 merge, U199), and passed at `f2a16d3e`
  (checked with `gh run list` / `gh run view`).
- Real hosts: every findings document says nothing ran on a real emulator, adb server, simulator, Mac or worker.

## Disagreements and checks against the code

- **D68's progress-line warning.** The Unreleased bullet said "one warning is logged"; D68 says it is a WARN on
  `marathon_core::execution::progress`, visible with `debug: true`. The code agrees with D68: `progress.rs:353` logs
  it, and `marathon-logging` gives that target no floor, so it follows the ordinary threshold. The changelog now says
  "shown with `debug: true`". The D71 warning, by contrast, is shown with debug off (`marathon_core::analytics::provider`
  and `::noop` are always at DEBUG), so "logs one warning" stands there.
- **`fixed` and 0.13.0.** The migration note said only that Kotlin cannot read `fixed`. At `7090b228` (0.13.0)
  `bind_flakiness_strategy` knows only `ignore` and `probability`, and `polymorphic` turns an unknown id into
  `unknown type id`, so 0.13.0 rejects it too. Added to the D72 note and to "Check before you upgrade".
- **The migration lists.** D67 was in neither the "need no action" nor the "action of their own" list although D67's
  entry has a migration action (SIGKILL for a hard stop); it is now in the second. D68–D77 were in neither; D68–D70 and
  D74–D77 are now in the first, D71 and D72 in the second (D72: none for existing configurations, but rollback needs
  `fixed` removed).
- **A107 and U200 have no changelog bullet.** x69 says "no CHANGELOG entry (nothing a user can see)"; x72 added none.
  Kept that way. U198 and U199 are test-only and appear only under Evidence.
- **x63's "not verified" window** (between an instrumentation's end and the next wait) is closed by x68 per U198; not a
  conflict, as U198's row says so.

## Uncertain

- Whether D74's added disposal wait (up to 70 s) changes D38's quoted 26–30 / 33-minute probe costs; no findings
  document re-measured them, so the line is kept as it was.
- The "gate on macOS and Linux" claim rests on CI at `f2a16d3e` and the x61–x66 merge records; the release commit will
  be this documentation on top, which CI has not run yet.

## Verification

`release.changelog_section('0.14.0-rc.1')`, `python3 -I scripts/release/test_release.py` (81 tests, OK), a relative
link and anchor check over CHANGELOG.md, README.md, docs/deviations.md and docs/migration.md (no failures), and the local
check. PLAN.md is not touched, so `scripts/check_plan_dag.py` was not needed. `scripts/check.sh` and workspace tests were
not run, per the brief.

## Incidental bugs

None found.
