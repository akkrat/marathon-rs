# X32: a rejected test does not use a batch slot (D50)

The project owner approved this on 2026-11-07. D50 is a new deviation in PLAN §3.11 and fixes C33. No configuration field,
CLI option or exit code changed. `class-name` and `isolate` batching are untouched.

## The row, checked against the code

C33 is right. `FixedSizeBatchingStrategy.kt` increments `counter` and adds the expected duration for every poll, whether or
not the test joins the batch. The Rust port did the same. The row's examples held in the old test.

## The fix

`fixed_size` in `crates/marathon-core/src/strategy/batching.rs`:

- Only an accepted test counts against `size` and `durationMillis`. A rejected test asks for no duration.
- A batch may reject at most 2 × `size` tests (`REJECTED_POLLS_PER_SLOT`), counted in `i64` because `size` can be
  `Integer.MAX_VALUE`. At the limit the batch goes out with what it has.
- The first test polled is always accepted, so a batch is never empty unless `size` is 0, which is unchanged.
- Rejected tests are re-added after the loop, in poll order, as Kotlin does. Re-adding them sooner would poll them again.

### Why 2 × size, and not an uncapped scan

Batch counts and polls from a throwaway model of the heap, not the product code. "Listed" is `no-sorting` over one APK's
tests, then the other's. "Mixed" is a random strict order. The whole-run table below is from the product code.

| APKs, tests, size, order | Kotlin | Limit 1 × size | **Limit 2 × size** | Uncapped |
|---|---|---|---|---|
| 2, 1000, 10, mixed | 150 batches, 1,493 polls | 104, 1,477 | **100, 1,504** | 100, 1,504 |
| 2, 1000, 10, listed | 101, 1,009 | 101, 1,010 | **101, 1,020** | 100, 26,000 |
| 2, 200, 5, mixed | 59, 293 | 44, 298 | **40, 296** | 40, 296 |
| 3, 300, 10, mixed | 62, 609 | 38, 580 | **33, 614** | 30, 591 |
| 4, 400, 10, mixed | 99, 978 | 62, 967 | **46, 1,009** | 40, 979 |
| 10, 1000, 10, mixed | 461, 4,579 | 392, 4,879 | **216, 5,199** | 100, 5,343 |

Two times the size reaches the ideal count for two evenly mixed APKs and most of it for three or four. It costs about the
polls Kotlin made, because Kotlin spent them on extra batches, and at most 3 × `size` polls per batch. An uncapped scan
costs 26 times as many on a listed queue and gains one batch. Above about four mixed APKs the limit leaves batches short:
ten APKs take 216 batches where 100 would do, still less than half of Kotlin's 461.

## Measured: a run with two test APKs

Through the real pool, device actor and queue, on one stub device with a test bundle identifier
(`crates/marathon-core/tests/pool_batch_slots.rs`). "Before" is `batching.rs` from origin/main, temporarily restored.

| Tests, size, order | Batches before | Batches after |
|---|---|---|
| 200, 10, `execution-time` | 32 | 21 |
| 1000, 10, `execution-time` | 150 | 100 |
| 1000, 50, `execution-time` | 31 | 20 |
| 200, 10, `no-sorting`, as listed | 21 | 21 |
| 1000, 10, `no-sorting`, as listed | 101 | 101 |

Flaky copies, one for each of 100 tests, in priority order, at size 10: 30 batches before and 20 after
(`flaky_copies_cost_fewer_batches_than_under_c33`, against the Kotlin rule in the test file).

**The gain depends on sorting.** `no-sorting`, the default, polls a listed queue from the array's end, so tests come in long
runs of one APK and hardly anything is rejected after the first batch. C33 cost little there and D50 changes nothing. That first
batch holds one test both before and after: its rejected run is longer than any limit worth having.

This wasn't run with real or fake-adb test APKs, since the repository has one fixture test APK. Each batch of one bundle is
one instrumentation or xcodebuild start, so batches are the measure.

## The traps

| Trap | Outcome |
|---|---|
| An uncapped scan per batch | 2 × `size`: `fixed_size_stops_after_two_rejected_polls_per_slot` counts 7 identifier calls for size 3 on a queue of 100 |
| `size` of `Integer.MAX_VALUE` | `fixed_size_of_integer_max_value_does_not_overflow_the_rejected_limit` |
| Tie order of tests not rejected | See below |
| The time limit | `fixed_size_adds_only_accepted_durations_and_breaks_at_the_limit`, `fixed_size_asks_no_duration_for_a_rejected_test` |
| The JVM rows stay as witnesses | See below |
| Nothing rejected must be byte-identical | `batches_with_nothing_rejected_match_kotlins_rule_operation_by_operation`: 200 seeded queues with many ties, comparing batches, the array and every comparison after each batch, plus every JVM row outside the allowance |

### Tie order: what changes

- **A strict order:** a poll always returns the minimum, so a rejected test returns to its place and every test keeps its
  order: `rejected_tests_keep_every_tests_place_under_a_strict_order`.
- **Ties:** the order among equals is whatever the heap's layout gives, and Kotlin's own re-adds disturb it too. D50 polls
  further, so it disturbs it differently after a batch that rejected a test: rejected tests go to the array's end in poll
  order, and the array's last test moves to the root at each poll. Tests never polled keep their relative order
  (`rejected_tests_leave_tests_never_polled_in_their_order_under_ties`). So tests can run in another order than in Kotlin
  after such a batch. A run with nothing rejected has the same order throughout.

### The allowance

- `strategy_batching.rs`: `D50_ROWS` names the exact 7 of 20 FixedSize JVM rows where the batches differ. Each must still
  replay byte for byte, batches, array and trace, through `kotlin_fixed_size`, C33's rule, kept only in the test file; each
  must have a rejected poll and must differ under D50's code. All other rows must match D50's code exactly. Rows with
  rejected polls whose queue ran out first, such as "leading null bundles", need no allowance and get none.
  `the_d50_allowance_hides_no_other_change` tampers with a row on each side and sees it fail.
- `queue_oracle.rs`: one row of the corpus, `execution-time comparisons with ConfirmOutcome and IsEmpty`. Kotlin's
  FixedSize:3 there spends one slot on a flaky copy, so its first three batches poll exactly what D50's FixedSize:2 polls,
  and the row's first 8 steps replay byte for byte at that size, including every comparison, ConfirmOutcome and IsEmpty. The
  fourth batch has three tests, so the row stops before it, and the final progress check goes with it. As recorded, the row
  differs at step 0, where D50 gives `[C1, C2, C3]`: `d50_fills_the_slot_kotlin_gave_to_a_flaky_copy`.
  `the_d50_allowance_covers_one_row_and_hides_no_other_change` has the negative tests.

No fixture changed. No comparator in `tools/differential` needed an allowance: `tools/differential/run check` passes with
the records as they are, so no recorded whole-vendor case has a rejected poll that changes a batch.

## What was verified and how

- `cargo fmt --check`; `cargo clippy -p marathon-core --all-targets -- -D warnings`.
- `cargo test` for `marathon-core` and every crate that depends on it: `marathon-analytics`, `marathon-android`,
  `marathon-apple`, `marathon-cli`, `marathon-ios`, `marathon-macos`, `marathon-test-support`.
- `tools/differential/make_registry.py --check`, `tools/differential/test_evidence.py`, and `tools/differential/run` with
  `validate`, `check` and `native-vendors` (15 captures).
- With `batching.rs` restored from origin/main: `two_apks_in_mixed_order_fill_every_batch_but_each_apks_last` fails, and 9
  tests of `strategy_batching.rs` fail, the pins of the rule among them.

Not verified: real devices, emulators or simulators, real or fake-adb test APKs, and more than one device in the two-APK run.
The batch counts in the first table are from a model of the heap.

## Incidental bugs

No new bug in our own code. Seen nearby and left alone:

- **A failing bundle lookup loses more polled tests.** When `identify` fails, the tests already polled are lost with the
  error, as in Kotlin, because rejected ones are only re-added at the end. D50 raises the most that can be polled from `size`
  to 3 × `size`. The error reaches the queue actor as it did before.

## Review

A second-model review, by reading the code and a throwaway property test over 3000 random queues (duplicates, null and mixed bundles, ties, a duration limit, `lastMileLength`): no lost, duplicated or starved test, within 3 x `size` polls per batch, and the union of all batches equal to the original. Notes, none blocking:

- The tests a failed `identify` drops are up to 3 x `size` now, against `size`, silently as in Kotlin. Its only realistic trigger, a test missing from the bundle map, ends the queue actor anyway, so re-adding them would restore tests nothing can poll. The real bug is the dead queue, recorded separately.
- For the 7 allowed rows, D50's own batches are only asserted to differ from the recording, and one row pins its first batch. A wrong result on the other six would pass this test, though not the property checks.
- The `queue_oracle` row replays a size-3 recording at `fixed-size:2` for 8 steps. It witnesses heap, comparison and outcome behaviour, not C33 at size 3.
