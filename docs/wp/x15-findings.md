# X15: a failed history query is not final (D34 / C41)

The project owner asked on 2026-11-06 for this fix to a bug the port had kept for parity. It is an approved deviation
in PLAN §3.11. No configuration field, CLI option or exit code changed.

## The ledger row

C41 is accurate. `MetricsProviderImpl.kt:54-68` stores `emptyMeasurementValues()` under the key after `withRetry(3, 50)`
fails, and `:105-122` does the same through `getOrPut`. The `cache-fail` row of `fixtures/oracle/metrics/provider.jsonl`
records it: 3 calls, then none for the same key. The Rust provider copied this in `prefetch`.

## The policy

| Question | Answer |
|---|---|
| What is retried | Only a key whose query failed. An empty answer is a success, and stays cached without a refresh |
| The first query of a key | Unchanged: three attempts 50 ms apart, the same lines in the same order |
| Who retries | One task on the vendor runtime, started by the first failure, ended when no key is left. Never a lookup, a comparator or a prefetch |
| Delays | 5, 10, 20, 40, 80, 160, 300, 300 s through the vendor clock: about 15 minutes in all |
| Total | 8 re-queries per key, of one attempt each. A round queries each failed key once, serially, in first-failure order |
| A key that fails during a refresh | Joins at the next round, at the delays still to come and then at 300 s, and has its own total of 8 |
| A key that fails after the refresh ended | Starts a new one from the 5 s delay. Keys that ran out stay out |
| After the last re-query | One warning. The defaults answer for the rest of the run, which is today's behaviour |
| At the end of the run | `close` cancels and joins the task, then closes the source. Nothing waits for a delay or for a query in flight |

5 s is late enough for a backend restart not to use up the early attempts, and early enough that a device connecting
seconds into the run can get the history. Keeping the total small matters more than reaching a long outage: a run
of hours with a backend down for longer than 15 minutes keeps the defaults, as it would have in Kotlin.

Why one task with rounds, not a timer per key: the backend sees one refresh query at a time, and the order of the
queries depends only on the order the keys failed in.

A refresh does not hold `prefetch`'s lock across its query, so a hung refresh cannot hold up a queue build for another
key (`a_hung_refresh_blocks_neither_lookups_nor_prefetches_of_other_keys`). So a refresh query and a pool's first query
of another key can run at the same time. A prefetch of a key that has failed returns at once with what is cached: it never
waits for a refresh, so a failing backend adds no wait to a pool's build.

## When new history takes effect

Every synchronous read goes through `MetricsLookup::view`, an immutable snapshot of the history, taken by the reader.
Publishing a refresh replaces the provider's current snapshot with a new one. Snapshots already taken do not change.

| Reader | Thread | Takes its view | So a refresh reaches it |
|---|---|---|---|
| Sorting comparator (`SortingStrategy::process`) | Core | When the comparator is made, as its queue is built | Only at the next queue's build. A live queue keeps its order |
| Batch duration limit (`BatchingStrategy::process`) | Core | At the start of each batch | At the next batch. Never half of one |
| Flakiness (`FlakinessStrategy::process`) | Core, once | At the start of the pass | If it landed before the pass. It runs right after the first query, so in practice never |
| Coverage report (`report_execution_time_coverage`) | Vendor | After its prefetch | If it landed before. It is one log line, not repeated after a recovery |
| Async `MetricsProvider::{success_rate,execution_time}` | Vendor | Each call | At the next call. Only tests use these |

The comparator is the case the view exists for. `SuccessRate` sorting asks the provider on every comparison, so a value
changing under a `JavaPriorityQueue` would leave its heap unordered for good
(`a_success_rate_queue_keeps_its_order_and_the_next_queue_uses_new_history`). `ExecutionTime` sorting was safe for tests
already compared, since it memoises each key at first comparison; the view also covers a test first compared after the refresh.

Durations observed in this run stay live in every view, as they are in Kotlin.

**What this does not give:** in a run with one pool, once its queue is built, sorting stays on the defaults until the end
of the run. The queue is not rebuilt, because that changes which test runs next in ways nobody asked for. Batching
with a duration limit does take the new history.

## What the logs do

With a healthy backend: nothing changes, and no task starts.

With a failing backend, on top of the first query's lines, which are unchanged:

| Event | Lines added |
|---|---|
| Each failed re-query | `Timings` INFO `… failed in N ms`, and `MetricsProviderImpl` WARN `Cannot fetch … from database` with its cause |
| A re-query that answers | `Timings` INFO `… returned N tests in M ms`, the DEBUG list of rows for an execution time query, and INFO `… returned N tests after failing earlier. Queues built and batches composed from now on use it; a queue that already exists keeps its order` |
| The 8th failed re-query | One more WARN: `… failed 8 more times. Tests without history use the defaults for the rest of this run` |

So a key adds at most 17 INFO and WARN lines. A cancelled query logs nothing, as in `prefetch`. The per-test "No
execution time found" and "No success rate found" lines stay once per test, and a recovery does not repeat them.

Nothing else reads this cache: no output file, report or tracker. `report_execution_time_coverage` only logs, and
`queue.rs` only feeds it observed durations. Output files do not change, apart from the order tests run in once real
history reaches a queue.

## Determinism

For a given sequence of backend answers and vendor clock times, the run is the same
(`the_same_backend_answers_give_the_same_run`): the queries, their order, the log and the answers. Under the real
clock, whether a particular queue or batch comes before or after an answer depends on when that answer arrives, in
the way a device's arrival time already does. Within a queue or a batch it cannot vary.

## Evidence

No recording changed, and no comparator allowance was needed. Every difference comes at least 5 s of vendor clock
after the first failure. The test harness's clock holds a refresh delay until the test releases it, so the JVM
`cache-fail` row still replays unchanged through
`analytics_oracle::provider_sequences_match_jvm_bits_calls_and_ordered_logs`. It remains the witness of Kotlin's
behaviour. The differential suites do not configure analytics, so they run the no-op provider and never see this.

`scripts/check.sh`, `scripts/remote-check.sh` and `cargo test --workspace` were not run, by the rule for this batch;
the orchestrator runs them. See the final report for what was.

### Failure without the fix

Each change was reverted temporarily, and these failed:

| Reverted | Failed |
|---|---|
| `schedule_refresh` not called (Kotlin's behaviour) | 17 of the 18 tests in `analytics_refresh`, `queue_prepare::a_queue_keeps_the_history_…`, and both new `startup_timings` tests |
| The comparator reads the live provider | `a_success_rate_queue_keeps_its_order_…`, `a_descending_success_rate_comparator_never_changes_its_answers`, `a_batch_a_shard_and_a_comparator_each_read_one_view_of_the_history`, `queue_prepare::a_queue_keeps_the_history_…` |
| Batching, then flakiness, read the live provider | `a_batch_a_shard_and_a_comparator_each_read_one_view_of_the_history` |
| `close` does not stop the refresh | `close_ends_a_pending_refresh_…`, `close_drops_a_refresh_query_in_flight_…`, `startup_timings::a_run_against_a_backend_that_stays_down_…` |
| `close` cancels but does not join | `close_ends_a_pending_refresh_…`, `close_drops_a_refresh_query_in_flight_…` |

`an_execution_time_queue_keeps_its_order_…` passes with the comparator unpinned, because of the memoisation described above.

### Tests

- `crates/marathon-core/tests/analytics_refresh.rs` holds the recovery, the schedule and total, single flight, close, the
  run's cancellation and dropping the provider, a hung query, the views and determinism, plus lookups on another
  thread while a refresh lands.
- `queue_prepare::a_queue_keeps_the_history_it_was_built_on_and_the_next_pools_queue_gets_the_refresh` drives real queue
  actors, from `prepare` to the batches.
- `startup_timings::{a_run_against_a_backend_that_stays_down_ends_without_waiting_for_its_refresh,
  history_that_comes_back_reaches_a_queue_built_after_the_refresh}` run the whole flow on the real vendor runtime and
  the virtual clock. A run with a failing backend ends at the same moment as one with a healthy backend.
- `analytics_concurrency::failed_queries_answer_defaults_until_a_refresh_close_errors_propagate_and_duplicate_names_count`
  is the renamed `failed_queries_cache_defaults_…`. Its assertions still hold, since its clock never reaches a refresh.

### Not verified

No real InfluxDB or Graphite was used. The refresh goes through the same `RemoteDataSource` calls as the first query,
whose transports `crates/marathon-analytics/tests` cover against fake peers, but no test takes a real or fake transport
from failing to answering.

`close` assumes that dropping a query future ends it. That holds for the HTTP clients here, which are async with no
blocking section; a source that blocks inside its future would hold up `close`.

## Incidental bugs

None recorded in `docs/found-bugs.md`. Two observations, neither a bug:

- `MetricsContext.cancellation` is a fresh signal in production that nothing cancels (`construction.rs`). A pool's
  prefetch is cancelled through the pool's own signal instead, so the run-wide one only matters in tests. D34's
  shutdown therefore relies on `close`, on dropping the provider and, last, on the vendor runtime's `shutdown_background`.
- C25's short circuit can skip `MetricsProvider::close` when the flush before it fails. The refresh then ends with the
  provider or the runtime, whichever goes first, and the source stays unclosed as it did before.
