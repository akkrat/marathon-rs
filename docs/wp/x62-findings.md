# WP x62 findings: flakiness history policy (D71) and `fixed` flakiness (D72)

Approved 2026-11-15 by the project owner, after a discussion of a stabilization workflow: (A) test history that
cannot be read adds no probability copies; (B) a deterministic flakiness type for stabilization jobs.

## A. D71: unavailable history adds no probability copies (C8)

### Design

- `MetricsLookup` gains `success_rate_available(limit, consequence) -> bool`, default `true` (test lookups and
  any lookup whose answers are real history). It is read through the same pinned view as the rates, so D34 still
  holds: every test of a shard is judged by one snapshot, including whether that snapshot has history.
- `MetricsProviderImpl`: a published measurement is now `Option<rows>`. `None` means the key's query failed and no
  refresh has answered it yet. A key with no entry at all means its prefetch never finished (a cancelled query
  publishes nothing). A successful answer, empty or not, is `Some`. Lookups treat `None` exactly as the old empty
  map, so sorting, batching, the execution-time defaults and the coverage line are unchanged.
- `NoOpMetricsProvider` (analytics disabled, or the InfluxDB/InfluxDB 2 backend could not be set up) answers
  `false`. Its success rate is still 0 for direct callers.
- `FlakinessStrategy::process` (probability) asks once per pass, on the pinned view; when the history is unavailable
  it returns the shard with no copies (an existing `flaky_tests` is replaced by the empty list, as probability always
  replaced it). It no longer reads per-test rates then, so the per-test `No success rate found` WARNs disappear in
  that case.
- One WARN per run, emitted by the provider that knows the reason (`marathon_core::analytics::provider` or
  `marathon_core::analytics::noop`, both visible with debug off per docs/design/logging.md):
  `Test history unavailable (<reason>); flakinessStrategy probability adds no copies`, reasons
  `analytics is disabled`, `the analytics backend could not be set up`, `the success rate query failed`,
  `the success rate query did not finish`. The flag lives in the provider's shared state, so pinned views share it.

### Invariants

- Working backend unchanged: empty window, partial history and tests missing from it take `defaults.successRate`
  (0.0 unless configured), so new tests still get copies; a genuine 0 still gets `maxCount` copies.
- Only the success-rate key of the strategy's `timeLimit` decides; a failed execution-time query does not.
- Recovery: a refresh that answers the key flips later views to available; a view pinned before it stays
  unavailable (D34). In a production run the flakiness pass runs once, right after its prefetch, so a recovery
  normally reaches only success-rate sorting, which never asked about availability.
- Graphite setup failure still propagates and fails the run (the brief allowed keeping it). Unifying it with the
  InfluxDB fallback would change Graphite from "fail the run" to "run without history", which is a separate decision.
- A cancelled prefetch in production still fails `prepare_test_shard` before `process` runs (unchanged); the
  "did not finish" reason covers a lookup on a provider whose prefetch was cancelled, which no longer trips the
  `debug_assert` in `measurement` for this check.

### Witnesses

The old `analytics_factory::c8_zero_success_rate_repeats_every_test_for_disabled_unavailable_and_empty_history` is
split into:

| Case | Test |
|---|---|
| disabled | `analytics_factory::d71_disabled_analytics_adds_no_probability_copies` |
| setup failure | `analytics_factory::d71_a_backend_that_could_not_be_set_up_adds_no_probability_copies` |
| query failure (and another key's failure does not count) | `analytics_factory::d71_a_failed_success_rate_query_adds_no_probability_copies` |
| query cancellation | `analytics_factory::d71_a_cancelled_success_rate_query_adds_no_probability_copies` |
| empty window, configured default, partial history, genuine zero | `analytics_factory::c8_tests_missing_from_working_history_take_the_configured_default_success_rate` |
| refresh/recovery, pinned view kept | `analytics_refresh::d71_success_rate_history_that_recovers_is_used_by_later_views_only` |
| end to end, Stub vendor, analytics disabled | `flakiness_scenarios::d71_probability_with_analytics_disabled_runs_each_test_once` |

Each `d71_*` test checks the copies, the single WARN (logger and text) across two passes, and the absence of
per-test `No success rate found` lines. Fail-without-fix, by temporary revert: disabling the strategy's
availability check failed all four `analytics_factory::d71_*` tests; publishing a failed query as an empty answer
again failed the query-failure and the recovery tests. Both reverts were undone.

`strategy_flakiness::a_success_rate_of_0_gives_every_test_max_count_copies` still holds (a genuine 0); its comment
now says so. No parity-matrix row changes: the ported Kotlin tests (`ProbabilityBasedFlakinessStrategyTest`,
`NoOpMetricsProviderTest`'s `should return 0 as success rate`) still hold as written.

## B. D72: `flakinessStrategy: {type: fixed, count: N}`

### Design

- `FlakinessStrategyConfiguration::Fixed { count: i32 }` and `FlakinessStrategy::Fixed`: `count` copies of every test
  of the shard, grouped per test in shard order (the order probability gives when every test is below the minimum),
  replacing any existing `flaky_tests`. No history keys, no lookup call (a panicking lookup proves it).
- Binding is strict, as for the types without a Kotlin hand-written deserializer: unknown fields are rejected;
  `count` is required, and missing, `null` or blank is a missing-parameter error (`int_boxed`); every other spelling
  coerces exactly as `shardingStrategy`'s `count` does (a test compares the two over nine spellings).
- `count < 0` is rejected at logical validation, like `observedDurationMinTests`:
  `flakinessStrategy.count must be at least 0, got -1`. Placed right after the complex-sharding rule.
- Rendering: YAML view (`type: fixed`, `count`), canonical dump (`{"count": N, "type": "fixed"}`), Allure environment
  text `FixedFlakinessStrategyConfiguration(count=N)`, and the test-support schema JSON.
- `fixed` is a "complex flakiness strategy" for the existing validator rules: rejected with count sharding and with
  `ALL_SUCCESS`, exactly as probability. With `executionStrategy.fast: true` (the default) the first pass completes the
  test and the queue drops its remaining copies (Kotlin's `QueueActor` behaviour, unchanged); a stabilization job sets
  `fast: false` to run all `count + 1`. Retry quotas are separate, as for probability's copies.

### Witnesses

- `strategy_flakiness::d72_fixed_gives_every_test_count_copies_grouped_as_probability_does`,
  `d72_fixed_count_0_makes_no_copies`.
- `analytics_factory::d72_fixed_copies_are_made_whatever_the_history` (disabled, setup failure, failed query,
  perfect history; no D71 warning).
- `flakiness_scenarios::d72_fixed_runs_each_test_count_plus_one_times_with_analytics_disabled`: production run graph,
  Stub vendor, `count: 2` and `fast: false` give three results (PASSED, FAILURE, PASSED) in `raw.json`; `count: 0`
  gives one; with `fast: true` a FAILURE then PASSED stops at two.
- marathon-config: `schema::d72_fixed_flakiness_binds_count_strictly`, `validation::d72_a_fixed_flakiness_strategy_parses_validates_and_renders`
  (the docs/deviations.md example verbatim: parse, validate, YAML round trip, dump, environment text),
  `validation::d72_a_negative_fixed_count_is_rejected`, `validation::d72_fixed_follows_the_complex_flakiness_rules`.
- marathon-cli: `config_dump_goldens::d72_config_dump_of_a_fixed_flakiness_strategy` (accepted dump and the logical
  rejection of `-1` through the binary). The input is written by the test, not added under `fixtures/`, so the
  configuration contract corpus (which collects every `Marathonfile*` there) is unchanged.

The JVM corpora contain no `type: fixed` flakiness input; the config differential, validation, schema and contract
corpora all pass unchanged.

## Golden diffs

None. No result-file golden, recording or contract corpus changed. `tools/oracle/scenarios/random-order.json` (probability with disabled
analytics, two PASSED answers for `maxCount: 1`) would now run once, but no Rust test runs it; it is only an input to
the old patched-JVM coverage tooling.

## What was run

- `cargo fmt --check`; `cargo clippy -p marathon-core -p marathon-config -p marathon-cli -p marathon-test-support --all-targets -- -D warnings`: clean.
- `cargo test --no-fail-fast -p marathon-config -p marathon-core -p marathon-cli -p marathon-test-support`: 13 suites
  failed on a fresh worktree only because the `snakeyaml` `yaml_tree` helper example was not built
  (`tools/differential/evidence.py` requires it); after `cargo build -p snakeyaml --example yaml_tree` all 13 passed on rerun.
  Everything else passed in the first run.
- `scripts/check_plan_dag.py`, `tools/differential/make_registry.py --check`, `tools/differential/run validate`,
  `tools/differential/test_evidence.py`: pass.
- `scripts/parity_matrix.py --check` reports the matrix out of date, listing Apple code-coverage and
  configuration-factory rows "no longer found". The script reads only the Kotlin sources and the CSV, and this WP
  changes neither, so the report does not come from it.

Not run, per the batch rules: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`. Vendor crates
were not tested: they do not construct flakiness strategies or analytics lookups.

## Owner decisions

- **`fixed` with `ALL_SUCCESS`.** The brief says `fixed` should work "with strictMode (all runs must pass) as copies do
  today". There is no `strictMode`; its successor, `executionStrategy.mode: ALL_SUCCESS`, rejects every complex
  flakiness strategy (Kotlin `LogicalConfigurationValidator`), so today's copies do not work with it, and `fixed`
  follows the same rule. A stabilization job therefore runs `ANY_SUCCESS` with `fast: false` and reads the per-run
  results (`Raw:` summary line, result files); its exit code says only whether some run passed. Exempting `fixed` from
  the `ALL_SUCCESS` rule would make "all runs must pass" the exit code, but it is a further contract change.
- **Graphite setup failure** still fails the run while InfluxDB's falls back to no history (kept, see A).

## Incidental bugs

None found.
