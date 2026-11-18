# WP x81: copies of different tests take turns in the queue (D83, U205)

Base: `3e53e6a4` (tag 0.14.0). A follow-up from the trial of 0.14.0-rc.2 on a CI farm, approved on 2026-11-18 by the
project owner. Deviation D83, refining D60; incidental bug U205. The 0.14.0 CHANGELOG lists the old behaviour as a Known
limit, which stays.

## Problem

On a CI farm of Android emulators, a job of 3 tests without history ran with `flakinessStrategy: probability`. Every
test got 4 copies (15 runs) on 8 emulators. 0.14.0-rc.2 made 15 executions and the JVM build 10, in the same wall time
(16m30s against 16m24s), with about 50 minutes of copy time against 35. rc.2's first wave held all five runs of one test,
each started before any had passed, so `executionStrategy.fast` (ANY_SUCCESS) had nothing to cancel.

Cause: `QueueActor::spawn` inserted `shard.tests` and then `shard.flaky_tests`, and the strategies list a test's copies
together (`FlakinessStrategy::process`, both `probability` and `fixed`, D72). Under D60's FIFO ties copies with equal sort
keys stay together: `t1 t2 t3 t1 t1 t1 t1 t2 t2 t2 t2 t3 t3 t3 t3`. Eight devices took `t1` five times. `fast` drops
only queued copies (`process_test_action` → `remove_all`), so copies that start together all run. The JVM's
`PriorityQueue` heap interleaved them by accident.

## Fix

`strategy::flakiness::queue_order(&TestShard)` gives the order a pool's queue takes the shard: round by round, where
round n holds the n-th occurrence of each test, in shard order (a stable sort of `tests ++ flaky_tests` by occurrence
number). `QueueActor::spawn` inserts that instead of the chain. Three tests with four copies each queue as
`t1 t2 t3 t1 t2 t3 …`.

Every place that builds or re-inserts copies:

| Place | Before | Now |
|---|---|---|
| Initial shard, `flaky_tests` (probability, fixed) | grouped per test | round by round |
| Initial shard, `tests` repeated by `shardingStrategy: count` | grouped per test (`repeat_n` in place) | round by round, the same rule: they are copies too, and the same race applies |
| Late pools (D65) | built through the same `QueueActor::spawn` | same |
| Retries (`rerun_test` → `offer`) | one copy, at the end of its priority | unchanged: a retry is made when an earlier copy failed, so it is already behind the others |
| Rejected batches (`on_reject_batch`), `fixed-size`'s rejected copies, `next_batch`'s avoided own retries | re-added in their order at the end of their priority | unchanged: these are copies that were already queued in order; re-adding them at the end keeps them apart from their test's other copies |
| `parse --includeFlakyTests` | lists `flaky_tests` grouped | unchanged: `FlakinessStrategy::process` still lists copies per test, and `parse` output is result data |

The order is a property of insertion only, so `OrderedQueue` (D60) is unchanged.

### Invariants kept

- Which copies exist and how many: `FlakinessStrategy::process` and `ShardingStrategy::create_shard` are untouched (D34,
  D71, D72).
- Different sort keys still order the queue; a longer test's copies still all come first under `execution-time`, as in
  the Kotlin heap.
- Seeded random order: `RandomOrder` draws once per distinct test at its first insertion. Round 1 holds every test's
  first occurrence in shard order, so the draws are the same as before for a given seed; only ties between equal draws
  order differently.
- Batching rules: `fixed-size` rejects a second copy of a test in a batch (`members.contains`), `class-name` takes every
  queued copy of its class, `isolate` takes one. These rules are unchanged, but the batches they form are not: a batch
  that spent D50's 2 × `size` rejected polls on one test's copies and went out short is now full, and other mixes give
  smaller and more batches (review round 1, finding 4).
- The rules are unchanged: `fast: false`, ALL_SUCCESS and ANY_SUCCESS, `fast` (drops only copies still queued when the
  test is decided), D69 (finished tests are not run again) and D77 (which attempt is the final record). Their outcomes
  are not: which device runs which copy changes, so a device-dependent verdict and the attempt D77 picks can change
  (review round 1, finding 1).
- Determinism: the order depends only on the shard.

## Contract (PLAN §3.12)

Worker commands: queue-derived test-selector order and grouping (`adb -e class`, xcodebuild `-only-testing`) change for
runs whose copies have equal keys, and so can `fixed-size` batch membership and the number of batches. Result data: runs
under `fast` can list fewer executions in `raw.json`, JUnit and HTML, and since the device that runs a copy can change, a
device-dependent verdict and the final record can change too (review round 1). Hence D83 (PLAN
§3.11 row, `docs/deviations.md` section with evidence and user impact, migration row, CHANGELOG `## Unreleased`).
Configuration, CLI, exit codes and pinned progress/summary text are unchanged. No configuration field was added.

## Tests

All new tests fail without the fix (checked by removing the round sort, so `queue_order` returned the old chain, and by
restoring the old `add_all` line in `QueueActor::spawn`).

- `flakiness_scenarios::d83_copies_of_different_tests_interleave_so_fast_drops_more_of_them`: a whole run (Stub vendor,
  production run graph on virtual time). 3 tests × `fixed` count 4 (15 runs), 8 devices connecting 1 ms apart, ANY_SUCCESS
  with `fast`, every run passes; the device that takes the third test's first run needs 30 s, the others 5 s. Now the
  first wave is `s1 s2 s3 s1 s2 s3 s1 s2`, and 11 runs happen. Before: `s1 s2 s3 s1 s1 s1 s1 s2` and 13 runs (step3's
  four copies all ran while its one run was on the slow device). With `fast: false` all 15 runs happen, 5 per test. The
  scenario uses `fixed` rather than `probability` because the Stub factory has analytics disabled, which D71 turns into no
  copies; both strategies hand the queue the same `flaky_tests` shape.
- `strategy_flakiness::d83_equal_priority_copies_of_different_tests_interleave`: `fixed`, uneven probability-style
  counts and count-sharding repeats, through a `no-sorting` queue.
- `strategy_flakiness::d83_different_sort_keys_still_order_the_queue`: `execution-time` with distinct durations (each
  test's runs together, longest first) and with a tie (the tied tests alternate, ahead of the shorter one).
- `strategy_flakiness::d83_a_seeded_random_order_draws_as_before_and_ties_interleave`: scripted draws `[5, 1, 5]` are
  drawn in shard order and consumed; the two tests drawing 5 alternate.
- `strategy_flakiness::d83_batching_rules_hold_for_copies_in_turns`: `fixed-size` 3 with 4 copies each gives 5 full
  batches of the three tests (before: 6, one of them `[t0, t1]`); `class-name` takes every copy, in turns.

Changed tests (native D69 tests in `queue_behaviour.rs`, no Kotlin counterpart, no parity-matrix row): three tests built
copies as repeated `tests` entries and relied on them coming out first.

- `a_retry_granted_to_the_attempt_that_completes_the_test_is_not_run_d69` and
  `a_late_incomplete_copy_of_a_finished_test_is_not_run_again_d69`: the copied test is made the longer one under
  `execution-time` sorting (`longest_first`), so its copies still go out first; assertions unchanged.
- `a_rejected_copy_of_a_completed_test_is_dropped_d69`: the queue now starts `C0 C1 C0 C0`, so serial-Y1 asks before
  serial-X1 and serial-Z1 and the active-batch listing reads `serial-Y1, serial-Z1`; every other assertion is unchanged.

No Kotlin-oracle test changed and no parity-matrix row changed.

## What was run

- `cargo fmt --check`: clean.
- `cargo clippy -p marathon-core --all-targets -- -D warnings`: clean.
- `cargo test -p marathon-core`: 856 passed, 0 failed (after `cargo build -q -p snakeyaml --example yaml_tree`, which a
  parse scenario needs and a fresh worktree lacks).
- `cargo test -p marathon-cli`: 95 passed, 0 failed (includes the native vendor golden cases).
- `python3 -I tools/differential/run validate`, `python3 -I tools/differential/test_evidence.py`: pass.

Not run, per the batch's gate rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`. The vendor
crates (`marathon-android`, `marathon-ios`, `marathon-macos`) were not run: none of their tests configures flakiness
copies or count sharding, so their queues hold no copies and the order is the old one.

## Golden diffs

None; see [x81-golden-diff.md](x81-golden-diff.md).

## Not verified

- The farm job itself was not rerun with the fix. The scenario test reproduces the mechanism deterministically; how many
  executions a real farm saves depends on device timing (the JVM's 10 of 15 was one sample).
- Real devices: the change is in the core queue and does not touch vendor code.

## Owner decisions

Both decided by the project owner in review round 1: D83 includes `count` repeats, and test discovery duplicates too.

- D83 covers `shardingStrategy: count` repeats as well as flakiness copies, under the same rule. The brief names
  probability and fixed; count sharding builds copies of a test the same way and had the same race (the JVM heap mixed
  them too). Narrowing D83 to `flaky_tests` only would be a one-line change in `queue_order` (chain `tests` unchanged,
  then the copies round by round); I chose the consistent rule.

## Incidental bugs

None new. U205 is this WP's own row.

## Review round 1

A cross-model review raised four findings; the owner decided that D83 includes `count` repeats and discovery duplicates.

1. **MUST, D83 can change verdicts and final records.** The docs said a test's result does not change. That is wrong:
   the order decides which device runs which copy, so where a test's outcome depends on the device the verdict can
   change (the reviewer's case: two devices, `count: 2`, ALL_SUCCESS, `fast: false`; grouped dispatch sent one copy to
   the device where the test fails, interleaved sent both to the device where it passes), and with the same verdict D77
   can pick another attempt as the final record (device, timing, attachments). Fixed in the D83 section, the PLAN §3.11
   row, migration, the CHANGELOG bullet, the findings' invariants and contract paragraphs: outcomes can change, the
   rules (D77, `fast`, ALL_SUCCESS/ANY_SUCCESS) do not. No code change.
2. **MUST, what `fast` drops.** The loader rejects `count` other than 1 with ANY_SUCCESS (`loader.rs`, logical
   validator), so the "first pass" explanation did not cover `count` repeats, and a batch already handed out runs even
   if its tests have not started. The docs now say: `fast` drops copies still queued when the test is decided, after
   the first pass under ANY_SUCCESS (flakiness copies) or the first failure under ALL_SUCCESS (`count` repeats, `fixed`);
   a started copy or a handed-out batch runs. New whole-run test
   `flakiness_scenarios::d83_count_repeats_interleave_so_fast_all_success_drops_queued_runs`: real `shardingStrategy:
   count` 5, flakiness `ignore`, ALL_SUCCESS, 3 tests on 8 Stub devices, step1 failing on every device. With `fast`, 13
   runs (step1 3, the others 5 each) and the first wave holds every test; without `fast`, 15. The hand-built shard in
   `strategy_flakiness::d83_equal_priority_copies_of_different_tests_interleave` that mixed `count` repeats with a flaky
   copy (a configuration the loader rejects) is replaced by the shard `ShardingStrategy::Count` 3 makes, through
   `FlakinessStrategy::Ignore`. The eight-device scenario setup is shared by both D83 scenarios (`on_eight_devices`).
3. **SHOULD, discovery duplicates (decided: include).** Code unchanged. New test
   `strategy_flakiness::d83_a_test_discovered_twice_takes_its_turn_like_a_copy`: discovered `A B A C`, `parallel`
   sharding, flakiness `ignore`, no sorting: queued `A B C A`. Stated in D83, PLAN, migration and CHANGELOG, and in
   `queue_order`'s doc comment.
4. **SHOULD, batches can get smaller and more numerous.** New test
   `strategy_flakiness::d83_fixed_size_batches_can_also_get_smaller_and_more_numerous` pins the reviewer's example
   (equal keys, size 2, A ×4 B ×2 C ×2: `AB|CA|BC|A|A`; with the fix reverted it gives the grouped `AB|CA|AB|CA`). D83,
   PLAN and migration now say batches can be fuller or emptier and their number can go up or down, and qualify the
   non-test operands: they are built as before, but a test in another batch gets that batch's generated Android
   `coverageFile` path (batch id and instrumentation index, `runner.rs`).

Revert check: with `QueueActor::spawn` back on the old chain and the round sort in `queue_order` removed, every D83 test
fails (all 8: the two scenarios, the six `strategy_flakiness::d83_*`); the count scenario then runs `[5, 5, 5]`
(15 runs) instead of `[3, 5, 5]`. Sources restored and re-run green.

Run in round 1: `cargo fmt --check` clean; `cargo clippy -p marathon-core --all-targets -- -D warnings` clean; `cargo
test -p marathon-core` 859 passed, 0 failed; `cargo test -p marathon-cli` 95 passed, 0 failed; `python3 -I
tools/differential/run validate`, `python3 -I tools/differential/test_evidence.py` and `scripts/check_plan_dag.py` pass.
Not run, per the batch's gate rule: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`. No golden,
recording or allowance changes (the new tests are Stub-vendor and unit tests).
