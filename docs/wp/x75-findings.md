# WP x75 findings: JUnit lists only the runs of a passed test that did not pass (D78)

## C88, verified before the change

Kotlin `JUnitReporter.createTestCases` (`core/.../report/junit/JUnitReporter.kt:74–98`) takes a test's other runs from
`PoolSummary.retries`, drops incomplete runs, and drops passing runs only when the final result is a failure
(`isFailure(it)` → `!event.testResult.isSuccess`). The list then becomes `rerunFailure` for a failed test and
`flakyFailure` for a test that is neither skipped nor failed, so a passed test lists its other passing runs as empty
`flakyFailure` elements. The port (`crates/marathon-core/src/report/junit.rs`, filter
`status != Incomplete && (!failure || !is_success())`) kept this. Real runs of 0.14.0-rc.1 with
`flakinessStrategy: {type: fixed, count: 2}` (D72) showed two `flakyFailure` elements on tests that passed all three
runs. The JVM reporter recordings hold it too: `fixtures/oracle/reporters/probes.jsonl` rows `xml-edges` (one passing
run of the passed `retry`) and `fail-json` (two equal passed finals, each listing the other), and
`review-round1.jsonl` row `rv-retried` (one passing run of the passed `flaky`).

## Design

One predicate for both branches: a run is listed when it is not incomplete and did not pass
(`!TestResult::is_success()`, i.e. status other than `Passed`). For a failed test this is the old filter, so
`rerunFailure` is unchanged. For a passed test it drops passing runs and keeps failed runs with their traces.

Ignored and assumption-failure runs of a passed test stay listed as `flakyFailure`. The brief asks to drop *passing*
runs; these are not passing, and the `rerunFailure` branch has always listed them for a failed test, so one rule
serves both. They need a test whose runs were ignored on one device and passed on another, which is rare. If the owner
wants `flakyFailure` restricted to status `Failure` only, it is a one-line change to the filter and to
`d78_native_junit`; recorded as a possible owner decision, not done.

Unchanged: skipped finals (no runs listed, as before), incomplete runs (never listed), test-case order and attributes,
suite counts, every other reporter.

## Other reporters checked

- HTML (`reporters/html.rs`): lists `PoolSummary::tests` (final results) only; its `total_flaky` sums
  `PoolSummary::flaky`, a constant 0. No per-run listing, so not the same pattern.
- Allure, `raw.json`, per-test result files, timeline, stdout summary (`Raw:` line): record every run as data or only
  finals; left alone as the brief says.
- `tools/real-e2e/projection.py` reads only `failure`, `skipped` and `error` children of a JUnit case, so its
  projections are unaffected.
- `PoolSummary::retries` has no reader besides JUnit.

## Witnesses

`crates/marathon-core/tests/junit_reporter.rs`, cases built from the `fail-pass` row's event and rendered with
`junit_xml`:
- `passed_test_whose_copies_all_passed_has_no_flaky_failure`: runs PASSED, PASSED (empty trace), PASSED final → an
  empty test case. Fails without the fix (two `flakyFailure`).
- `passed_test_with_one_failed_copy_has_exactly_that_flaky_failure`: runs PASSED, FAILURE (`boom`), PASSED final →
  exactly one `flakyFailure` with `boom`. Fails without the fix (an extra empty one).
- `failed_test_with_mixed_copies_has_rerun_failure_only_for_failed_runs`: runs PASSED (`p`), FAILURE (`f1`), PASSED,
  FAILURE final (`last`) → `<failure>last</failure>` and one `rerunFailure` with `f1`. This branch was already
  correct, so this test pins unchanged behaviour and passes with and without the fix (a departure from "each failing
  without the fix", which cannot hold for a branch the fix does not change).

Temporary revert of the filter: the two passed-test witnesses and `reporters::all_six_reporters_match_jvm_bytes_and_complete_manifests`
fail; restored afterwards.

## JVM recordings (D78 allowance)

The recordings are unedited. `reporter_support::d78_native_junit(input, pool, jvm)` removes from a recorded JUnit file
exactly the `flakyFailure` elements of passing runs. It derives the runs from the row's own input the way the summary
pairs them (other events of the same test; an equal final shares the last equal one's runs; incomplete runs are not
listed), and asserts one test case per final and one `flakyFailure` per listed run of every passed case, so a
recording it does not describe panics instead of being edited. It is applied to every JUnit file in
`compare_scenario` (`reporters.rs`) and in `junit_reporter.rs`'s `parity`.

- `reporters::d78_allowance_removes_only_the_flaky_failures_of_passing_runs`: over all rows it changes exactly
  `fail-json` (2 removed), `xml-edges` (1) and `rv-retried` (1); test cases, `<failure>`, `<skipped`, `rerunFailure`
  and a failed run's trace keep their counts; the `xml-edges` `retry` case keeps its ignored run and its failed final
  run, in order.
- `reporters::d78_allowance_rejects_a_recording_that_does_not_match_its_runs`: with one run removed from the input,
  the allowance panics.
- Because native output is compared byte for byte with the allowance's result, it cannot equal the JVM bytes in those
  three rows; the revert above shows a widened native output fails.

Parity matrix: no row changes. The ported `JUnitReporterTest` cases (`passed`, `failed`, `traces`, `fail-pass`,
`pass-fail`) have no passing runs of a passed test and still match the Kotlin fixtures and the JVM bytes.

## Golden diffs

No golden file or recording changed. The reviewed difference is the allowance: in the three JVM rows above, the
expected native JUnit lacks these elements of the JVM bytes:

- `xml-edges`, case `com.example#retry` (passed): `<flakyFailure type="rerun exception"><stackTrace><![CDATA[]]></stackTrace></flakyFailure>`
  (the non-final PASSED run with an empty trace). Kept: the IGNORED run (empty element) and the FAILURE final run
  (empty trace).
- `fail-json`, both `p.C#nonfinite` cases: `<flakyFailure type="rerun exception"></flakyFailure>` (the other equal
  PASSED final).
- `rv-retried`, case `com.example#flaky`: `<flakyFailure type="rerun exception"></flakyFailure>` (the non-final PASSED
  run). Kept: the FAILURE run with its trace and the ASSUMPTION_FAILURE run.

The marathon-cli end-to-end, contract and differential suites, and the vendor crates, pass unchanged: none of their
inputs gives a passed test another passing run (see "What was run").

## What was run

- `cargo fmt --check`; `cargo clippy -p marathon-core --all-targets -- -D warnings`: clean.
- `cargo test --no-fail-fast -p marathon-core -p marathon-cli -p marathon-android -p marathon-apple -p marathon-ios
  -p marathon-macos`: all pass (207 test binaries, 2200 tests), after `cargo build -p snakeyaml --example yaml_tree`,
  which the evidence tooling needs on a fresh worktree.
- The temporary revert above (`-p marathon-core --test junit_reporter --test reporters`).
- `scripts/check_plan_dag.py`, `tools/differential/make_registry.py --check`, `tools/differential/test_evidence.py`:
  pass. `scripts/parity_matrix.py --check` reports the matrix out of date only with Apple code-coverage rows "no
  longer found"; the script reads only the Kotlin checkout and the CSV, and this WP changes no matrix row (x70 saw
  the same).

Not run, per the batch rules: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Incidental bugs

None found.
