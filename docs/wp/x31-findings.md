# X31: an emptied pool waits (D48), a lost request keeps its device (D49)

The project owner approved both on 2026-11-07. No configuration field, CLI option or exit code changed. A healthy run sends the
same commands and logs the same lines. A run that lost a device can now log more, and a lost request adds one `getprop`.

## D48: what Kotlin does

`DevicePoolActor` ends the pool when its last device has terminated, with `all devices terminated, remaining tests will not
run`. The pool stays in `Scheduler.pools`, closed, so a device that connects later is dropped by `safeSend` and still tracked
(C50). The queue and its tests go with the pool.

## D48: the design

The pool asks the queue whether anything is left. If so, and time remains, it logs a warning and starts a timer under its
supervisor, which `terminate` cancels. `AddDevice` during that wait voids the timer, and the new device asks the queue for a batch as
any device does. The timer's end logs what Kotlin logged, and terminates the pool.

| Trap | What happens |
|---|---|
| A run must end when nothing returns | The timer. `a_run_ends_when_no_device_returns_d48` checks that it does not end a millisecond sooner, and ends exactly at its limit |
| Which timeout | `deviceInitializationTimeoutMillis`, 180 s by default. It is the patience a run already has for a device to appear, and is on every vendor. `waitForDevicesTimeoutMillis` is Android's alone and bounds something else, and the batch and output timeouts bound a running test. 180 s holds what was measured: the slowest return on a real emulator took 46.6 s from `offline`, and D33's retries are 30, 60 and 90 s after each failure. Its cost is that it bounds both, so a farm that needs an hour for its first device also waits an hour here |
| Doubling D41's grace and D22's bound | `Device::return_wait_used_millis`: what the vendor waited for this device before the disconnect. Android measures it from the first `offline` or the outage's start, on a new `VendorClock::now_millis`, carried through an outage that takes over from a grace. So a real killed emulator ended 180.00 s after its `offline`. Without this, the test's server that never returns would end 30 s later (`a_run_ends_when_its_adb_server_never_returns_d22`) |
| A flapping device holding the run | The time runs from when the pool first lacked a device until a test ends, passed or failed. Time with a device counts, and a return does not reset it, so a cycle cannot repeat past the limit. `a_device_that_keeps_returning_without_finishing_a_test_cannot_hold_the_run_open_d48` |
| The scheduler's lazy child join can miss a late pool | The pool's actor job stays active during the wait, so the traversal is still on it, and a pool created meanwhile is linked behind it and reached. `a_pool_created_during_a_wait_is_joined_and_the_run_waits_for_both_d48` drives this. A pool created after the traversal has finished is still missed, as the JVM row `late-pool` keeps |
| The queue's tests and their order | No queue operation runs during a wait. A device taking over after a 30 s wait gets the same batches in the same order as one that was in the pool all along: `a_device_that_returns_carries_on_with_the_tests_in_their_order_d48`. Retry counts stay |
| A queue terminating with no devices, against `remaining tests will not run` | The queue is asked after the messages sent before, so the last batch's outcome is counted. A queue that finished, or has no tests, ends the pool at once with Kotlin's lines: `a_pool_with_nothing_left_to_run_does_not_wait_d48`, `the_last_test_coming_back_with_no_retries_left_does_not_wait_d48`, `a_pool_with_no_tests_does_not_wait_d48`. The IsEmpty question now comes just before those lines rather than just after, so a queue line caused by the same last batch can precede them. No recording shows the difference |
| Several pools | One wait per pool. A `RemoveDevice` is broadcast, so a pool in its wait ignores other pools' disconnects: `another_pools_disconnect_does_not_restart_or_end_a_wait_d48`, `pools_wait_on_their_own_and_a_healthy_one_carries_on_d48` |
| A timer that fired just before a return | Waits are numbered, and a `ReturnWaitOver` of any other number is ignored. A stale timer needs no cancelling, as it does nothing and the supervisor ends with the pool |
| Cancellation | The timer is under the supervisor. `a_cancelled_run_does_not_wait_out_the_limit_d48` |
| macOS | Nothing there can bring a Mac back (`initialize_transport` connects each once), so `MacosDevice` says all the wait is used, and the run ends as it did |

**The bound.** The pool ends at the latest at `first lack of a device + deviceInitializationTimeoutMillis`, or if a device was present then, when
that device leaves, which its prepare retries, its batch timeouts and the quota limit. Any test ending starts it afresh.

**On iOS.** A worker lost for less than the limit recovers into the same pool. `a_replacement_within_the_limit_takes_over_the_pool_d48` shows that
through the real provider, recovery and scheduler on the fake worker. A recovery takes up to 70 s for its sessions to close, plus its connection attempts,
so a worker has to be back well within the limit; if it is not, the run ends with exit 1 as it did. WP x27 changes the reconnect's pacing in parallel, and that branch was not read here.
`last_device_loss_…_after_the_limit_does_not_resurrect_the_pool_d48` keeps the old behaviour at a small limit.

**U160, a batch held at the gate and charged when a server is given up: not fixed, and affected in one way.** The charge is the same. Before D48 a run on its last
device ended there, so only a second device saw it. Now the run can go on after the device returns and pass, with that attempt counted. With
`uncompletedTestRetryQuota: 0` those tests still fail without having run. The row stays open, with a note.

## D49: what Kotlin does

N15 is about the client: a connection closed before OKAY or FAIL reads as `RequestRejectedException("")`. `execution_error` makes
that `DeviceLost`, the actor terminates, and adb's list doesn't change, so the tracker never connects the device again.

## D49: the design

`means_device_loss` is unchanged, and D22 and D41 keep their paths. `AndroidDevice::execute`'s handling of `DeviceLost` has one question and one more case:

| The server says | Happens |
|---|---|
| Unreachable, or the gate is closed | D22: the batch fails, the device stays |
| The device is `offline`, `connecting` or `authorizing`, or a grace began during the batch | D41: the batch fails, the device stays |
| **`device`, and it answers `getprop` with `sys.boot_completed` within `shell`, fewer than 3 such losses so far** | **D49: the batch fails, the device stays, one is counted** |
| `device`, but it does not answer, or has restarted | Retired at once, as in Kotlin |
| `device`, 3 losses already | Retired, with no `getprop`, and `Device <serial> lost 3 requests already; not keeping it` |
| Anything else, or not listed | Retired, as in Kotlin |

**The limit.** `REQUEST_LOSSES_PER_DEVICE` is 3, per serial, per adb server, per run. It never resets, for the reason D41's doesn't: a device that loses a request
every so often would otherwise cut a test for ever under a large quota. 3 gets through a burst, and costs at most 3 more batches' attempts than Kotlin.

**What ends it.** The fourth loss, or the first on a device that doesn't answer. **No loop:** at most 4 batches per device, each charged once by the actor's
existing paths. If the device was the last of its pool, D48's wait follows, and as nothing changes on the list, nothing returns: `a_device_that_keeps_losing_requests_leaves_without_a_retry_loop_d49`. A
device retired this way stays listed and out of the run, which is what's left of N15.

**Not done.** A batch lost this way never started a test, so charging it is U160's question too.

## Tests

| Where | What |
|---|---|
| `crates/marathon-core/tests/pool_scheduler.rs` | 15 `_d48` whole runs through the real scheduler, pool, queue and device actors, on virtual time |
| `crates/marathon-android/tests/reconnect_test.rs` | D48 and D49 whole runs through the real provider and scheduler on the fake adb; the D22 and D41 runs now check the time of the disconnect and of the end |
| `crates/marathon-android/tests/offline_grace_test.rs` | Three `_d49` runs on a stopped vendor clock |
| `crates/marathon-ios/tests/simulator_pool_recovery.rs` | The two ends of the iOS recovery, as above |
| `crates/marathon-macos/tests/provider.rs` | A Mac has nothing to wait for |

**Evidence kept.** The JVM recordings are not edited. `all-lost` and `two-pools-disconnect` in `oracle/run-flow/scenarios.jsonl` and `broadcast` in `oracle/pool/scheduler.jsonl`
show Kotlin ending at once. Their allowances let only the end time differ, by exactly the wait, and only for those rows, and
`the_d48_allowance_hides_nothing_else` shows that an end at once, a moment off, or with another answer fails. `MarathonFactory` sets the limit to 60 s there, and `scenario::run_longer` advances the Kotlin tests' clock that much further, leaving the scripts as they are.
`all_devices_lost_during_execution_should_fail_instead_of_hanging` and `two_devices_in_separate_pools_disconnecting_one_after_another_should_terminate_the_run`, the ports of Kotlin's tests, keep their names, as what they state still holds.

**Proof by temporary revert.** Each change was reverted alone.

| Reverted | Tests failing |
|---|---|
| The wait itself | 13 in `pool_scheduler`, among them the JVM row's |
| The queue's answer, so that it always has work | `a_pool_with_no_tests_does_not_wait_d48` |
| A test ending resets the time | `a_test_that_ends_gives_the_pool_its_whole_wait_again_d48` |
| A return does not reset the time | `a_device_that_keeps_returning_…_d48`, `a_device_failing_prepare_after_it_returned_…_d48` |
| What the vendor waited is counted, in the pool | `what_the_vendor_already_waited_counts_against_the_limit_d48` |
| The same, in the Android provider | `a_device_that_stays_offline_…_d41`, `a_killed_emulator_…_d41`, `a_run_ends_when_its_adb_server_never_returns_d22` |
| A stale timer is ignored | `a_late_return_wait_timer_is_ignored_d48`, `a_test_that_ends_…_d48` |
| One wait at a time | `another_pools_disconnect_does_not_restart_or_end_a_wait_d48` |
| D49's keep | `a_run_survives_lost_requests_on_a_healthy_device_d49`, `a_lost_request_on_a_listed_device_that_answers_…_d49`, `a_device_that_keeps_losing_…_d49`, `a_blip_fails_only_the_batch_…_d41` |
| D49's limit | `a_lost_request_on_a_listed_device_that_answers_…_d49`, `a_device_that_keeps_losing_…_d49` |
| D49's `getprop` | `a_dead_device_that_stays_listed_…_d49`, `a_lost_request_on_a_listed_device_that_does_not_answer_…_d49`, `a_lost_request_on_a_listed_device_that_answers_…_d49`, `a_blip_fails_only_the_batch_…_d41` |

A first version also cancelled the timer at a return. Reverting that failed nothing, as the number already covers it, so the cancel was removed.

## Real emulator checks

The native release binary of this branch's code on a Linux host with KVM, a private adb server and private emulators, USB guard passed. adb 1.0.41, emulator 37.2.13, `google_apis` x86_64 images of
API 36 and API 33. The tests are the fixture sample's `MainActivitySlowTest`: 5 tests of about 5.4 s, one to a batch, quota 3 unless stated.

| Check | API | Outcome | Before |
|---|---|---|---|
| No fault | 36 | Exit 0, 40.6 s, 5 of 5 | The same |
| **`adb reboot` during a test, alone** | 36 | **Exit 0**, 62.4 s, 5 of 5, 2 `INCOMPLETE`. `returned as if restarted` 8.5 s after `offline`, the pool waits with 171,477 ms left, the device arrives 9.3 s later | Exit 1 at about 8.5 s (x19) |
| **`adb reboot` during a test, alone** | 33 | **Exit 0**, 57.8 s, 5 of 5, 1 `INCOMPLETE`. The wait had 171,035 ms left, and the device arrived 5.2 s later | Not run |
| **Emulator killed during a test, alone** | 36 | **Exit 1**, 183.64 s, exactly 180.00 s after its `offline` at 3.64 s. Disconnected 3.25 s after `offline`, wait of 176,748 ms, `no device returned`, then `remaining tests will not run` | Exit 1 at about 12.5 s (x19) |
| Emulator killed, a new one on its port 30 s later | 36 | **Exit 0**, 87.0 s, 5 of 5, 1 `INCOMPLETE`. Listed again 38.3 s after `offline`, setup waited for the boot, arrived at 46.6 s | Exit 1 |
| **2 `am instrument` connections closed before their reply**, by a proxy on a private port | 36 | **Exit 0**, 38.1 s, 5 of 5, 2 `INCOMPLETE`, `keeping the device` twice, one actor, no disconnect | Exit 1 in 0.85 s, the base commit on the same fault, 0 passed |
| Every `am instrument` closed, limit 20 s, quota 10 | 36 | Exit 1, 21.9 s. 4 batches, 3 kept, the fourth retires it at 1.93 s, 20 s wait, 4 `INCOMPLETE`, no loop | |

In the first reboot two tests were `INCOMPLETE`. The reboot cut the first test, the next batch started 2 ms later on a transport already going, and adb's `offline` arrived 23 ms after that. This is the
race x19's review recorded, and it's why the count is not 1. The quota bounds it.

Not run on a real emulator: D22's server that never returns with D48 (the fake adb covers it) and two emulators. iOS recovery with D48 ran on the fake worker only, with no real simulator or SSH.

## N33: does the kill target `grep`? Checked, not changed

The orchestrator asked for this on real API 33 and API 36, with a video run; no fix. On these levels, 26 and up, the code runs `ps -A | grep screenrecord`.

| API | `ps` | `ps -A \| grep screenrecord`, 30 asks during a recording | `ps \| grep screenrecord`, 30 asks | Video run, 5 tests |
|---|---|---|---|---|
| 36 | toybox 0.8.12 | Last line `screenrecord` 30 of 30, `grep` never listed | The same | Exit 0, 5 of 5, 5 videos. 10 `ps`, 5 `kill -2`: one kill per test, and the next `ps` finds nothing |
| 33 | toybox 0.8.6 | Last line `screenrecord` 30 of 30, `grep` never listed | The same | Exit 0, 5 of 5, 5 videos. 14 `ps`, 9 `kill -2`: 1 or 2 per test, the second while it's still finishing its file |

**So the kill doesn't hit `grep` on these levels.** toybox's `ps` prints the process name, not its arguments, so `grep`'s own line reads `grep` and doesn't match. No `ps` reply in a video run held a `grep` line,
and no stop came near five attempts. N33's reasoning and the JVM rows `pid:grep-last` and `stop:grep-last` are about what the code does with such a line, from a fake, and hold as a statement about the code.
Whether a lower API's `ps` shows arguments was not checked: there's no such image here. The `Did not kill any screen recording process` warning logged at each stop is the normal end of that loop and is what Kotlin's does too. The ledger row is left as it is.

## Incidental bugs

None new. Noted by the way:

- `avdmanager` reached through a symbolic link finds its SDK from its own path and ignores `ANDROID_SDK_ROOT`, so it could not see the private API 33 image, and the AVD was written by hand. A lab matter, not the product's.
- A device retired while it stays listed is out for the run, and D48 waits for it in vain (documented in D48 and in CHANGELOG's limits). Taking it back would be a fix of its own.
