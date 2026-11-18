# WP x80 findings: a retry is kept off the device it came from (D82, C90, U204)

Base: main at 3e53e6a4 (0.14.0). Branch `wp/x80-retry-other-device`.

## The farm case

A job on a CI farm of about 120 Android emulators ran 0.14.0-rc.2 with `sortingStrategy: execution-time` and
`uncompletedTestRetryQuota: 3`. About 4 minutes into the run, one emulator's first batch ended with
`Instrumentation failed: … the instrumentation stream ended before a result, with exit code 1`. It was the first result
of the run. Every other emulator was still busy with its first batch, and about a hundred tests were still queued. The
same test then came back incomplete from that emulator three more times within about 2 s and failed on the quota. The
emulator then passed other tests, and the job's retry passed the test on another emulator. `raw.json` and the failure
texts were not kept, so the farm run itself cannot be replayed.

## Step 1: what happens today

The code reading in the brief was right for this case.

- `QueueActor::rerun_test` queues the test again and records the device's worker in `retry_workers`. `next_batch` and
  `on_request_batches` keep a test away from a device only if every queued copy of it is a retry from that device's
  worker. Android devices return `worker_id() == None` (`crates/marathon-android/src/device.rs`), so an Android retry is
  recorded nowhere and nothing keeps it off any device.
- With `execution-time` sorting the test goes back to the head of the queue. The device that just gave it back asks next,
  once the queue has confirmed the outcome (`ConfirmOutcome`), and takes it again.
- The Kotlin marathon does the same. `QueueActor.kt:205–275` has the same `nextBatch`/`ownRetries`/`rerunTest`, and
  `Device.workerId` defaults to null (`Device.kt:20–25`). Only Apple devices override it.
- The Android runner reports a failed instrumentation stream as the batch's unfinished tests, incomplete, on the usual
  retry path (D25). `a_stream_cut_off_fails_the_run_and_keeps_the_device_d25` covers that mapping. A Stub device that
  answers INCOMPLETE gives the core the same input, so the witness runs the production run graph with the Stub vendor.

The first witness (now `retry_placement_run::a_device_that_fails_every_test_tries_each_once_before_its_own_retries_d82`;
review round 1 added the busy-device witness below) uses two devices in one pool. Device A answers every test INCOMPLETE after 100 ms and is ready first. Device B passes every
test in 2 s and becomes ready 550 ms after A. The run has ten tests, with `slow` first by execution time, and quota 3. On
the base code, `raw.json` shows `slow` on A as INCOMPLETE, INCOMPLETE, INCOMPLETE, FAILURE within 300 virtual ms,
before B was ready. Every other test then went the same way on A, except the two that B took. This is the farm's
pattern.

One part of the brief's description did not hold. When the other device is **idle**, it already takes the retry today:
on `Notify` the pool asks its parked devices first, before the device whose batch failed asks again.
`an_idle_device_takes_an_incomplete_test` passes on the base code and after the fix. The 0.14.0 Known limit says "while
other devices are free". On the farm they were busy, not idle. The released text stays as it is.

## Design (D82)

The narrowest sound change generalises the existing worker avoidance. A retry records its **place**: the worker of the
device it came from, or the device's own serial number when it has no worker (`enum Place { Worker, Device }`, private to
`queue.rs`). Everything that compared workers now compares places: `next_batch`, `own_retries`, `dispatched`,
`only_own_retries`, `on_request_batches`, and the active retries a rejected batch restores. Nothing else changes.

Alternatives considered and rejected:

- **Hold a device's own retry while another device is busy** (park the device instead of giving it its own retry). This
  would also cover a device that fails every test. It adds a wait state, though: the queue would depend on another
  device coming back, a removed or slow device would stall the retry, and a pool of one device needs a special case.
  The brief asks for no deadlock, no hang and a fallback to the same device. The Kotlin worker rule ("only if nothing
  else is queued") already gives that without any wait, so D82 keeps it.
- **Only for incomplete runs, not for retries of failed tests.** `rerun_test` serves both, and Apple's worker rule
  already covers both. Splitting them would need two kinds of retry record for no gain. Sending a failed test's retry to
  another device is also what a flaky-test retry wants.
- **Putting device serials into `Notify`.** The pool uses `Notify.retry_workers` only to order its parked devices.
  The device whose retry was just queued is never parked at that moment: it is waiting for its outcome. If it later asks
  together with others, `on_request_batches` serves it last. `Notify`, the pool, and every test and JVM recording that
  pins `Notify` contents therefore stay unchanged. The snapshot keeps `retry_workers` with worker places only and gains
  `retry_devices` for device places (test support only).

### Invariants

- **Fallback, no hang.** A device always gets a batch while the queue is non-empty and it has no active batch. D82 only
  changes *which* queued tests it gets, never *whether* it gets one. So a pool of one device, or one whose other devices
  are busy or gone, retries on the same device as before, and the D65/D68 completion conditions are untouched.
  Witnesses: `an_incomplete_test_goes_back_to_its_device_when_nothing_else_is_queued_d82` (whole run, the other device
  busy for 10 s) and `queue_behaviour::a_device_without_a_worker_gets_its_own_retry_when_nothing_else_is_queued_d82`.
  The single-device Kotlin scenarios (`uncompleted-1` to `uncompleted-4`) are unchanged.
- **No starvation.** A retry is only set aside for other queued tests. Once only a device's own retries are left, it
  takes them, so a device is never left without work while tests are queued for it. That is the only bound: an own
  retry can wait until the end of the queued work, even in a pool of one device. Another device picks it up when it
  asks, by the queue's priority and batching (it may get other tests first), and the extra launch described under
  "Batches" in review round 1 can add wall time.
- **Apple unchanged.** Apple devices always have a worker, so their places are `Place::Worker` with the same values as
  before. marathon-apple, marathon-ios and marathon-macos pass unchanged.
- **Quota accounting unchanged.** `charge_uncompleted`, the D69 refunds and the D70 paths are not touched. The device
  that runs a retry changes, and with it a test's number of attempts and possibly its verdict (review round 1).
- **Batching code unchanged.** `next_batch` sets aside the same kind of copies and restores them as before (Kotlin's
  `finally`). What an Android device's batches hold does change, since its own retries are now set aside (review
  round 1).
- **No configuration field.**

### Diagnostics

The state dump's `worker_retry_tests_pending` now counts tests with retries from devices without a worker as well. The
name stays (logs are not contract). The two tests that pin it see 0 in both meanings.

## What was verified

- Step 1 on the base code, as above.
- The witnesses fail without the fix (temporary revert of `queue.rs`: `slow` ran four times on A and failed) and pass
  with it (`slow` runs on A once, then passes on B). See review round 1 for the busy-device witness's rows.
- The three queue-level D82 tests (`a_retry_from_a_device_without_a_worker_is_left_to_another_device_d82`,
  `devices_without_workers_that_ask_together_leave_a_retry_to_the_other_device_d82`,
  `a_rejected_retry_keeps_its_device_d82`) fail on the base queue and pass with the fix. The fallback test passes on
  both, as it should.
- `cargo test -p marathon-core` (all targets, including `queue_oracle`, `queue_actor`, `queue_actor_retry_placement`,
  the pool and scheduler suites and the run-flow oracles): all pass.
- `cargo test -p marathon-android -p marathon-cli` (including the fake-adb whole runs and the differential suites): 378
  passed, 0 failed, 7 ignored.
- `cargo test -p marathon-apple -p marathon-ios -p marathon-macos -p marathon-test-support`: 1022 passed, 0 failed, 26
  ignored.
- `cargo fmt --check`, `cargo clippy -p marathon-core --all-targets -- -D warnings`, `scripts/check_plan_dag.py`,
  `tools/differential/run validate`, `tools/differential/test_evidence.py`, `tools/differential/make_registry.py --check`,
  and the local check.

### Oracle and differential suites

The JVM queue recordings (`fixtures/oracle/queue/queue.jsonl`) replay unchanged. Two cases use devices without a worker
(K4 and "execution-time comparisons with ConfirmOutcome and IsEmpty"), but in neither does such a device take back its
own retry while another test is queued, so their batches are the same. The snapshot comparison reads `retry_workers`,
which still holds only worker places. No comparator allowance was needed. No golden changed.

## What was not verified

- No farm run with the fix. The farm effect is reasoned from the log (first result of the run, about a hundred tests
  queued, all other emulators busy) and reproduced only offline.
- No real-emulator run. The change is in the core queue, and the Android side only supplies the incomplete results that
  the Stub device stands in for.
- `scripts/parity_matrix.py --check` reports `docs/parity-matrix.csv` as out of date against the local Kotlin checkout
  (Kotlin tests "no longer found" for iOS code-coverage targets). This WP does not touch the matrix, and the same check
  gives the same result on the base commit's file. No Kotlin test's row changes here: the Kotlin retry-placement tests use
  workers and still hold.
- The full workspace gate (`scripts/check.sh`) was not run, as the batch rules require.

## Possible follow-up (not in this WP)

An emulator whose instrumentation fails every time still uses one attempt of each test it takes. When only its own
retries are left and the other devices are busy, it takes those back too, so it can still fail tests on the quota (the
witness shows this for the tests B could not reach in time). Noticing a device whose instrumentation keeps failing
before any test starts, and keeping it out of its pool for a while, would address that. The brief excludes device
quarantine from this WP.

## Incidental bugs

None found.

## Review round 1

A cross-model review raised five points; all are addressed.

1. **MUST, document the observable changes.** The D82 section, the PLAN §3.11 row, the migration bullet and the
   CHANGELOG `## Unreleased` bullet said the number of runs stays the same. They now list what can change under the same
   quotas and exit-code rules: which device runs a retry; a test's number of attempts and its verdict; when an own retry
   runs; batches and instrumentation launches (with `fixed-size` size 2, an own retry and a fresh test that used to share
   one batch now go in two, if no other device takes the retry meanwhile), and with them the Android `am instrument`
   commands' number, order and `-e class` grouping; `raw.json` device serials and row order. The JVM recordings are
   untouched. The busy-device witness (point 2) gives these `raw.json` rows (method, device, status, in file order):

   | # | Before D82 (revert) | With D82 |
   |---|---|---|
   | 1 | slow, A, INCOMPLETE | slow, A, INCOMPLETE |
   | 2 | slow, A, INCOMPLETE | t2, A, PASSED |
   | 3 | slow, A, INCOMPLETE | t3, A, PASSED |
   | 4 | slow, A, FAILURE | t4, A, PASSED |
   | 5 | t2, A, PASSED | t5, A, PASSED |
   | 6 | t1, B, PASSED | t1, B, PASSED |
   | 7 | t3, A, PASSED | t6, A, PASSED |
   | 8 | t5, A, PASSED | t7, A, PASSED |
   | 9 | t6, A, PASSED | t8, A, PASSED |
   | 10 | t7, A, PASSED | t9, A, PASSED |
   | 11 | t8, A, PASSED | slow, B, PASSED |
   | 12 | t9, A, PASSED | |
   | 13 | t4, B, PASSED | |

   Before: 13 attempts, `slow` failed after the quota, the run failed (`Ok(false)`, exit 1). With D82: 11 attempts, every
   test passed, the run passed (`Ok(true)`). The size-2 split is pinned by
   `queue_behaviour::a_device_without_a_worker_runs_its_own_retry_in_a_batch_of_its_own_d82`: after `[slow, m1]` with
   `slow` incomplete, the device gets `[m2]` and then `[slow]`; before D82 it got `[slow, m2]`.
2. **SHOULD, model the farm's busy-device case.** New whole-run witness
   `retry_placement_run::an_incomplete_test_is_left_to_a_busy_device_while_other_tests_are_queued_d82`: A answers `slow`
   INCOMPLETE and passes every other test (100 ms each); B is ready 50 ms after A and is inside a batch with `t1` until
   2,600 virtual ms, after the baseline's four attempts at `slow` ended (2,400 ms) and while tests are still queued for A
   (until 2,900 ms). The test asserts the slow test's attempts (A incomplete, B passed), that B's batch ended after A's
   fourth result and before A's last, that every other test passed and that the run passed. With `rerun_test` reverted
   to record only workers, it fails on the slow test's attempts (A ×3 INCOMPLETE, then FAILURE) and the run fails. The
   timing is virtual (`FakeTimer` on the replay clock), per `docs/testing.md`. The earlier witness, where A fails every
   test, is kept as `a_device_that_fails_every_test_tries_each_once_before_its_own_retries_d82`.
3. **SHOULD, make the failure-retry assertion discriminate.** `m1` now sorts before `m2`
   (`execution_times` 1,500 against the default 1,000). The test checks that `m1`'s retry is at the head of the queue,
   that A1 takes `m2`, and that B1, once its batch is done, takes the `m1` retry. With failure retries recorded without
   a place (a temporary change to the failure path only), A1 takes `m1` and the test fails. The size-2 queue test is
   above.
4. **SHOULD, performance.** `own_retries` now first collects the retry records with this place and returns at once when
   there are none; only then does it count the queue, and only for those tests. The result is the same: a test with no
   retry from this place never qualified, since Kotlin's `count == copies[test]` needs a queued copy. The logic moved to
   a free function so the unit test `execution::queue::tests::own_retries_match_the_kotlin_count` can compare it with
   Kotlin's formula, including a test that is not queued, a test with a copy from elsewhere and a worker place.
5. **SHOULD, describe the scheduling bound.** The CHANGELOG and migration bullets, the D82 section and the PLAN row now
   say that an own retry runs after the other queued tests the device can take (at the end of the queued work in a pool
   of one device), that another device takes it by queue priority and batching when it asks, that a device is never
   left without work, and that the extra launch can add wall time. "As soon as it asks" under "No starvation" is
   replaced. C90 now cites where Kotlin gives parked devices the first request (`QueueActor.kt:195`, `Notify` after the
   outcome; `DevicePoolActor.kt:151–168`, `awaitOutcome`/`outcomeProcessed`) and says that Kotlin's worker-only
   placement is the policy the native queue had before D82.

The two whole-run witnesses, `a_retry_from_a_device_without_a_worker_is_left_to_another_device_d82` and the size-2
queue test fail on a temporary revert of `rerun_test` and pass with the fix; the narrower failure-path revert of point 3
fails the first queue test at the `m2` assertion. Both reverts were undone before committing.


## Review round 2

A second cross-model pass found three documentation points, fixed by the orchestrator in the D82 section, the PLAN
§3.11 row, the migration bullet and the CHANGELOG bullet:

1. The batching guarantee was too broad: the queue holds a test back from a device only while every queued copy of it
   is that device's own retry (`queue.rs`, `own_retries`). With a fresh copy queued too, the device can take the test,
   and `class-name` batching can send both copies with other fresh tests. The docs now say so.
2. A changed verdict changes the process status under the same rules: with `ignoreFailures: false` a run can exit 0
   where it exited 1, or the reverse. Stated in all four places.
3. Only `raw.json` names the device of every attempt; the per-test files, JUnit and HTML show the final result, whose
   device can change too. The D82 "Reports" bullet is scoped accordingly.

The reviewer found no new liveness or privacy issue; the `own_retries` shortcut keeps membership and order.
