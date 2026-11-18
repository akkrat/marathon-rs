# WP x81 golden diff (D83)

No golden, recording or comparator allowance changes.

D83 changes only the order in which a pool's queue first takes a shard's tests, and only where a test occurs more than
once: flakiness copies (`flaky_tests`), `shardingStrategy: count` repeats, or a test discovered twice (for example in two
test APKs). Where `tests` has no repeats and `flaky_tests` has at most one copy of each test, the round-by-round order
equals the old `tests` then `flaky_tests` order. The evidence below rests on that: each recorded or golden queue is
unchanged only because its insertion order has no repeated entry, not because D83 leaves repeated entries alone.

| Evidence | What it holds | Result |
|---|---|---|
| Registered native goldens (`fixtures/contract/native-v1`, 18 adapters) | Their Marathonfiles set no `flakinessStrategy` or `shardingStrategy`, so every run uses `ignore` and `parallel` (the vendor contracts that record the configuration list `IgnoreFlakinessStrategyConfiguration` and `ParallelShardingStrategyConfiguration`). No queue has a copy | `cargo test -p marathon-cli` (the native vendor cases compare against the goldens): 95 passed, unchanged |
| D60 comparator allowances | `tools/differential` has none: WP 8.7 re-blessed the six affected native views rather than adding an allowance | Nothing to update |
| JVM queue recordings (`fixtures/oracle/queue/queue.jsonl`, replayed by `queue_oracle` with its D60 native variants) | Seven rows have flaky copies; each lists at most one copy per test and no test twice in `tests`, so their construction order, key events and draws are the same under D83 | `queue_oracle` passes unchanged; no new variant |
| Other recorded scenarios with copies (`tools/oracle/scenarios/random-order.json`: one test, `probability` with `maxCount: 1`) | One test, at most one copy | `marathon_scenarios` passes unchanged |
| Configuration corpus and `parse` output | `parse --includeFlakyTests` lists `flaky_tests`, which D83 does not reorder | Unchanged |

`python3 -I tools/differential/run validate` and `python3 -I tools/differential/test_evidence.py` pass. No JVM recording
was re-recorded or edited.

Where a native run does have copies with equal keys, the worker commands' test-selector order and `fixed-size` grouping
change, as D83 describes; the Stub-vendor tests in `flakiness_scenarios` and `strategy_flakiness` pin that new order.
