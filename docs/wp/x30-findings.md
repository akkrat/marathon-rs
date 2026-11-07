# X30: an unlimited `uncompletedTestRetryQuota` is refused (D47)

The project owner approved this on 2026-11-07 and then narrowed it: a configuration fails only when nothing bounds a test
that keeps coming back uncompleted, and an unlimited quota is fine if something else really does. This WP traced every
candidate in the code. **On Android, iOS and macOS, only a finite quota bounds the loop.** So the rule comes out the
same for every vendor, and `maxBatchesWithoutProgress` isn't required and doesn't count.

## The rule

`validate_uncompleted_retries_bounded` (`crates/marathon-config/src/loader.rs`) refuses the configuration when
`uncompleted_test_retry_quota == i32::MAX`. It looks only at the parsed value, so unset and an explicit 2147483647 are the
same. `execute` calls it after the vendor gate and before construction.

| Vendor | Quota | Other settings | Result | Why |
|---|---|---|---|---|
| Android | unset or 2147483647 | anything | refused | no setting bounds the loop |
| macOS | unset or 2147483647 | anything | refused | no setting bounds the loop; `maxBatchesWithoutProgress` isn't a macOS field and is rejected at binding |
| iOS | unset or 2147483647 | `maxBatchesWithoutProgress` 0 or unset | refused | no setting bounds the loop |
| iOS | unset or 2147483647 | `maxBatchesWithoutProgress` > 0 | refused | two paths aren't charged to it (below); the message says so |
| any | any other value: 0, negative, 2147483646 | anything | accepted | the queue ends the test at that count |
| stub, or none | anything | anything | its own "No vendor config" error | the vendor gate comes first |

Tests: the D47 section of `crates/marathon-cli/tests/run_outcome.rs`. Each accepted row is shown to reach construction.

## What each candidate does to the loop

The loop: `handle_uncompleted_tests` (`crates/marathon-core/src/execution/queue.rs:416`) compares the test's count with the
quota. Under the quota, `test_ended(.., false, ..)` returns no action and the test goes back in the queue.

| Candidate | Stops a repeating uncompleted result? | Evidence |
|---|---|---|
| `retryStrategy` and its quotas | No. Only `handle_failed_tests` calls `retry.process` | `queue.rs:719` |
| `executionStrategy`, `fast` | No. `Incomplete` with `final_result: false` never completes a test | `progress.rs:83-92` |
| Flakiness and sharding | No. They set the number of copies at the start | `queue.rs:303` |
| `testBatchTimeoutMillis`, `testOutputTimeoutMillis` | No. They end one batch, and it comes back uncompleted | |
| `deviceInitializationTimeoutMillis`, `waitForDevicesTimeoutMillis` | No. They apply to preparation and to device arrival | |
| Android: any device limit | No. No count of batches exists. D41's five holds and D22's outage bound concern availability, and a healthy device whose batches report nothing is never touched | `BatchFailurePolicy` is used only in `crates/marathon-ios` |
| macOS: any device limit | No. A run with a failure reason loses the device, but a run that ends normally with nothing reported, or an unclassified error, keeps it | `crates/marathon-macos/src/device.rs:540-575` |
| iOS: `maxBatchesWithoutProgress` | Only on some paths, below | |

### iOS's `maxBatchesWithoutProgress`

**What it stops:** one simulator, never the run. After K batches in a row with no pass or failure on that simulator,
`mark_lost` takes it out and recovery sets it up again with a new count (`BatchFailurePolicy` belongs to the handle). After
the third recovery (`SimulatorRecovery::new`, 3) the simulator stays out. Other simulators take the batch.

**With N simulators, on the paths it does count:** at most K × 4 × N batches, then the pool ends with "remaining tests will
not run". That is a real bound there, but it costs all the simulators: one poison test can take the whole pool out and leave
other tests unrun, which the owner may want to weigh against a quota. This product was derived from reading the code and
from the existing per-part tests (`batches_in_which_no_test_finished_take_the_simulator_out_of_the_pool`,
`a_simulator_that_keeps_getting_lost_is_set_up_again_a_limited_number_of_times_after_growing_pauses`). It wasn't run as a
whole through the scheduler, as the verdict doesn't depend on it.

**The paths it doesn't count**, each proved by a test that stays in the repository as the witness:

1. **An unclassified error.** `classify` (`apple_simulator_device/execution.rs`) charges the policy only for a normal end, a
   timeout, or a failure with a reason. Any other error, such as an `IOException` from a worker command, goes to the actor's
   "Unknown vendor exception" path, and the batch comes back with the same simulator and no count moved. Witness:
   `simulator_batch::an_unclassified_error_is_not_counted_as_a_batch_without_progress`, with K = 1 and five batches. A47's
   `DateTimeParseException` is one such error.
2. **A lost connection.** `reconnect` (`simulator_provider/recovery.rs:590-596`) checks `connected` before `lost`, and the
   disconnected group has `loss: None`, so neither the loss limit nor a pause applies, and the replacement has a new count.
   Witness: `simulator_recovery::a_worker_that_keeps_dropping_its_connection_is_not_charged_to_the_limit_on_losses`, with a
   limit of 2 and five drops.

**x27 and x28 don't change this verdict**, judging from their briefs, as neither branch had a commit to read when this was
written. x28 removes the duration forms from path 1, and makes a no-progress timeout count, which was already charged to
this setting. Other unclassified errors stay. x27 limits failed reconnects, while path 2 is a reconnect that succeeds.

**What it would take for iOS to qualify:** `classify` charges every error that returns a batch, and the no-progress count
survives a replacement or a dropped connection counts as a loss. The check would then accept an iOS configuration with
`maxBatchesWithoutProgress` > 0 in a single added arm, though the poison-test cost above remains. That is the owner's decision; it
isn't done here.

## Which commands apply it

| Command | Applies it | Why |
|---|---|---|
| `run` | yes | it is what can loop |
| `parse` | yes | it runs no test, but a Marathonfile that `run` refuses should fail at a CI job's first step, not after the build |
| `--help`, `version` | no | they load no configuration |
| `config-dump` (hidden) | no | it runs no test, and it is how the binding is compared with the JVM's |

A value from a template counts as what it resolves to, as substitution happens before binding (`${UNSET_QUOTA:-5}` passes,
`${UNSET_QUOTA:-2147483647}` is refused). No CLI option sets the field. A literal `null` binds to 0, as a Jackson primitive does, so it passes, and a test gets no second attempt.

## Fixtures and harnesses

There is no switch, environment variable or hidden flag. The check isn't in `parse_sync`, which stays Kotlin's binding with
its default, so config tests, goldens and copied fixtures needed no change. Tests that assert the default still hold, and are
the witness of the old behaviour along with ledger row C85. No test had to be weakened, and no fixture was edited.

- Marathonfiles under `sample/`: none exist. Named Marathonfiles in the repository: 7, all copied fixtures or oracle
  inputs that don't reach this check, unedited.
- Configs changed, all owned by tests or tools: two constants in `crates/marathon-cli/tests/run_outcome.rs`, one in
  `crates/marathon-cli/tests/xctest_parser_supply.rs`, and `tools/oracle/android-reconnect-probes/run`.
- Already set, so unchanged: `tools/real-e2e/configs.py`, `tools/differential/vendors.py`, `tools/perf/runner_overhead.py`,
  `tools/oracle/android-runner-probes/cli-scenarios`, `crates/marathon-cli/tests/ios_production.rs`.
- `android-reconnect-probes/run` gets 2147483646, for both CLIs, which is out of reach as the default was, so it should replay
  alike. It was **not** run, since it rewrites its expected files and needs the Kotlin build. Its recorded output doesn't
  contain the quota.

## The value to recommend

The number of devices + 2, and at least 5. A test is charged once per uncompleted return, including a batch lost with its
device, so an innocent test in a bad outage can be charged about once per device. The floor leaves a one-device pool room for a
couple of restarts. The cost of a test that always hangs is (quota + 1) × the time to give up. This is a judgement from
the charging rule, not a measurement.

## Verified and not verified

Verified: the tests named above, and the crates and validators listed in the WP's final report.
Not verified: a real device run, as the check fails before any device is touched; the K × 4 × N product as one whole run;
`android-reconnect-probes/run`, as above; and x27's and x28's final code.

## Incidental bugs

Not fixed here; both need the owner's decision above.

- iOS: an unclassified batch error isn't charged to `maxBatchesWithoutProgress` or anything else, so that simulator can repeat
  it for as long as the quota allows (`apple_simulator_device/execution.rs`, `classify`).
- iOS: a dropped connection that reconnects resets the no-progress count and isn't charged to the loss limit
  (`simulator_provider/recovery.rs`, `reconnect`).

The brief calls `maxBatchesWithoutProgress` an iOS and macOS setting. It exists on iOS only: `MacosConfiguration` has no such field.
