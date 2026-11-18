# WP x87 — Index class-name batches

At `d5b0181d`, [batching.rs](../../crates/marathon-core/src/strategy/batching.rs) formatted the head's class name
and then formatted every entry's name during a scan. It cloned the matches and passed them to `remove_all`,
which compared every queued test with the list of matches. Draining 10,000 one-test classes took 50,015,000
name evaluations. One class with many distinct tests could also make the removal step quadratic within one
batch. The actor separately scanned for remaining copies when dispatching tests and when removing queued
copies of a completed test. Those scans also affected `fixed-size` and `isolate`.

## Design and invariants

- `OrderedQueue` keeps a primary entry for every copy at its D60 position (the insertion-time sort key and FIFO
  sequence). Both secondary indexes hold positions in that same order. Entries that survive a batch keep their
  stored sort keys. Requeues still extract new keys in the order in which the previous implementation reinserted
  them.
- The first `class-name` batch builds the class index. The index uses `Test::to_class_name('.')`, so dotted
  names and blank packages still match by their formatted names. The queue formats each distinct test once while
  it is present in the class index. Copies already queued share that name, and a test that returns after its last
  copy left is indexed on reinsertion. Each batch finds its class from the head and removes that class's positions
  in D60 order. For a class of *k* copies in a queue of size *n*, removal takes O(*k* log *n*) ordered-index
  work after the initial build. The indexes use O(*n*) additional space.
- The test-copy index uses `Test` equality and hashing. Each position retains its own metadata. D82 uses
  the copy index to find tests whose queued copies are all retries from the requesting place. The actor temporarily
  removes those copies in D60 order when there is other work, and reinserts them in that order even when
  batching returns an error. If every queued copy is excluded, the actor batches from the whole queue.
  D83's round-by-round initial insertion stays intact.
- `fixed-size` already polls from the head, tracks membership in a hash set, and bounds rejected polls to
  twice the requested size (D50). Last-mile selection compares the eligible queue length with the configured
  threshold and polls a single test below it. Both use the same copy index for the actor's shared bookkeeping.
  The actor still walks the retry records to find those belonging to a requesting place.

## Local timing

Run the [ignored timing test](../../crates/marathon-core/src/strategy/queue/class_batch_timing.rs) in release mode.

```sh
cargo test -p marathon-core --release --lib compare_scan_and_indexed_class_batches -- --ignored --nocapture
```

The harness compares the baseline ordered-map scan with the production index using synthetic inputs, a constant
sort key and one ASCII-named test per class. Both sides use the same `Test` and class formatter. Insertion time
covers queue construction, and drain time includes the indexed side's lazy class-index build. Timing covers
batch membership selection and removal. Device work, replay IDs and reporting are outside the sample.

Single local run on an Apple M5 Pro, arm64 macOS, with Rust 1.99.0.

| One-test classes | Baseline insertion | Indexed insertion | Baseline drain | Indexed drain |
|---:|---:|---:|---:|---:|
| 10,000 | 0.485 ms | 1.209 ms | 1.905 s | 5.865 ms |
| 50,000 | 2.304 ms | 8.288 ms | 52.268 s | 42.840 ms |

## Verification

- `queue_batch_selection::indexed_batches_match_scan_selection_with_retries_and_requeues_u213` drives
  the production actor over 96 seeded queues and compares it with the pre-index selection and retry rules.
  It compares ordered batches and their IDs, queued copies and their metadata, and worker and device retry
  placement. The cases exercise priority ties, D83 rounds, formatted class collisions, D82 exclusion and
  fallback, rejected batches, fixed-size duplicate rejection and last-mile selection.
  Fast seeds also require queued copies to be dropped after a pass in `ANY_SUCCESS` and a failure in
  `ALL_SUCCESS`. The test compares the copy index's count with the scan queue after every outcome so it can
  catch stale positions even when the ordered queue has already dropped the copies.
  In a scratch copy, removing entries from the ordered map without updating the indexes failed at seed 3,
  step 0, with 31 indexed positions for 25 remaining copies.
- `class_selection_formats_once_per_queued_test_u213` limits a full drain of 4,096 distinct one-test classes
  and one extra copy to 4,096 name evaluations. Reinstating the scan in a scratch copy failed the witness
  on the first batch, with 4,098 evaluations. A separate index test exercises forced sequence compaction,
  removal by equality, polling and clearing.
- Formatting, core Clippy across all targets, offline differential evidence validation and all 109 differential
  harness tests passed locally. The workspace and remote gates are assigned to the orchestrator.
- `cargo test --no-fail-fast` for `marathon-core`, `marathon-cli`, `marathon-android`, `marathon-apple`,
  `marathon-ios`, `marathon-macos`, `marathon-analytics` and `marathon-test-support` completed with
  2,439 passed, 33 ignored and zero failures. The review follow-up reran `marathon-core` with 911 passed,
  zero failures and the timing test ignored in the regular suite.

## Golden diff

Batch membership and order, batch ID allocation, result files and worker commands follow the existing contract.
D89 remains unused. The run-flow oracle passed all 17 tests with the committed JVM transcripts, report files
and comparator allowances. The CLI vendor differential passed with the committed Android, iOS and macOS
cases, including their worker commands and result files.

## Incidental bugs

The generated cases initially also collided on the complete formatted test name. The progress accumulator
uses that name as its key and lets later identities overwrite the state for an earlier identity. This predates
x87 and is pinned by `progress_oracle::grouping_retains_first_metadata_and_name_collisions_overwrite`. The
differential cases give distinct identities different method names so they isolate class-name collisions.
