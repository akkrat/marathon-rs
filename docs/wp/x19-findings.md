# X19: a grace period for a brief `offline` (D41)

The project owner asked for this on 2026-11-07, after the real emulator checks of WP x12 showed a run ending with exit 1
on a device that was back within 2 s. No configuration field, CLI option or exit code changed. A healthy run sends the
same commands and the same events.

## What Kotlin does

`DeviceStateTracker.processDeviceUpdate` turns any change from `device` to another state into `DISCONNECTED`, at once.
`AdamDeviceProvider` sends `DeviceDisconnected`, the actor terminates, and a pool ends with its last device. This comes
from reading the code. No recording shows it: no script, transcript or contract holds a device in that state (the word appears only in the HTML
reports' bundled script and in two notes about offline validation). So **no comparator allowance is
needed**. The Kotlin behaviour stays witnessed by `tracker_vanished_first_and_non_device_retention`, as the tracker
itself did not change. Ledger row: N40.

## What a real emulator shows

`host:track-devices` timestamped on a private adb server, one API 36 `google_apis` x86_64 image, emulator 37.1.11, adb 1.0.41.

| Fault | What the list says | Back as `device` after |
|---|---|---|
| adbd killed in the device, 3 times | `offline`; empty 250 ms later; `offline`; `device` | 1.06–1.12 s |
| `adb root`, `adb unroot` | The same | 1.25 s |
| `adb reconnect`, 3 times | `offline`; empty; `offline`; `device` | 50 ms |
| `adb reconnect offline`, 2 times | Nothing, the device being online | No change |
| A freshly booted emulator, unprovoked, once, about 25 s after boot completed (read from marathon's log) | `offline`, then back | 0.28 s |
| `adb reboot`, 3 times | `offline`; empty 250 ms later; `offline` at 1.25 s; `device` | 8.1–13 s, **still booting**: boot completed 15 s later where that was measured |
| Emulator process killed (SIGKILL) or stopped (SIGTERM) | `offline`; empty 250 ms later; nothing more | Never |
| Emulator process suspended with SIGSTOP for 3, 10 and 40 s | Nothing at all | No change |

Three things follow, two of them contrary to the brief's picture.

- **A real blip passes through an empty list.** The empty list lasted 50 ms to 1 s; the emulator's transport is found again
  by a scan once a second. If a device missing from the list were disconnected at once, as the brief asks of a device
  that vanishes, this fix would never work on an emulator.
- **A killed emulator looks the same up to that point.** So a device that goes missing during its grace has a shorter
  limit. One that leaves the list directly from `device` has no grace, as before.
- **A reboot can return inside any useful grace, while still booting.** It reads `device` at 8.1–13 s, with no
  `sys.boot_completed`. Treated as a blip, it would get batches that fail on a device with no package manager. So a device
  that returns is asked for its properties once, and one still booting goes through disconnect and connect, as it did
  before this change, and setup waits for the boot.

## The numbers

No new configuration field.

| Constant | Value | Why |
|---|---|---|
| `BLIP_GRACE_MILLIS` | 10 s | Eight times the slowest restart measured (1.25 s). adb's `ReconnectHandler` retries a lost TCP device after 250 ms and then every 3 s (`kDefaultTimeout`, `kMaxAttempts` 20 in `transport.cpp`), so this covers four attempts on a farm's network blip. It is also what a killed device on a listed `offline` costs, so it stays short |
| `BLIP_MISSING_MILLIS` | 3 s | Three scans of the emulator's transport; the longest empty list seen was 1 s. A killed emulator therefore costs 3.25 s before the retry path, measured |
| `BLIPS_PER_DEVICE` | 5 | Per serial, per adb server, per run. It is a count with no forgiveness, because a device that flaps slower than a test lasts could otherwise cut that test for ever under the default unlimited quota. The worst case of running out is today's behaviour |

## Design

`AdamDeviceProvider::offer` sits in front of the tracker, in each server's own tracking task. It shows a device in its grace
to the tracker as `device`, so the tracker, ported unchanged, sees nothing and there is one event at most. `passing` means
`offline`, `connecting` and `authorizing`; `unauthorized` waits for a person, and the bootloader, recovery and similar states
are a different mode altogether, so those disconnect at once.

| A device | Happens |
|---|---|
| Goes `offline`, has finished setup, has grace left | Held: batches wait, `borrow` skips it, `networkState` reads `DISCONNECTED`, `healthy` reads false |
| Returns as `device`, booted | No event, same object, same actor. Logcat and reverse forwards restored as in D22, then released |
| Returns as `device`, not booted or not answering | Normal disconnect, then normal connect |
| Stays passing for 10 s | Normal disconnect |
| Missing from the list for 3 s, or for less when the 10 s end first | Normal disconnect |
| Reaches a state that is not passing | Normal disconnect, at once |
| Goes `offline` with no grace left | Normal disconnect, at once |
| Goes `offline` during setup | Normal disconnect and connect, as ever: it has no actor to keep, and its setup has failed by now |
| Leaves the list directly from `device` | Normal disconnect, at once |

## The traps

**The race D22 had in `execute`.** A request caught by the blip fails with `RequestRejected`, which means a lost device, and
that used to retire the device. A `DeviceLost` now counts against the batch only if the server is away, the device is held,
a grace began during the batch, or the server's own list says the device is in a passing state, which covers the moment
until the tracker hears. That one `host:devices` replaces D22's `host:version` on this path: the failed request tells the
same, and it happens only after a device loss.

**Burning retry quota, found on the real emulator and not by the fake.** A gate at the start of `execute` is not enough.
When a batch ends during a grace, the actor asks for the next at once, and that batch waits at the gate. If the device never
returns, the terminating actor returns the batch, and the queue charges it to its tests, which never ran. With quota 0
and a healthy second emulator, a test that had not run failed. So a batch that ends during a grace now returns only when the
grace is over, and the actor asks for nothing meanwhile. The gate at the start stays for a batch given just before a blip,
and such a batch is the one case still charged if the device never returns, as it was for a batch given just before a
disconnect. A test running when the transport drops is a different matter: it really did not finish, and costs one
`INCOMPLETE`, as in D22.

**Remote parsing.** Three attempts with no delay would all go in the first moments of a blip. `wait_out_passing_loss`, which
D22 introduced for an outage, now covers a grace too. A blip costs one uncounted attempt.

**A run must still end, and flapping must not reset the grace.** See the table and the constant above. Each grace has a
fixed limit, a return closes it, and the count bounds how many open.

**D22, D24, D18, D40.** An outage during a grace closes it, as the outage's window then decides, and a device its window held
gets no grace on top when the window ends, so the bound is not doubled (`an_outage_does_not_add_a_grace_to_its_bound_d41`).
A server's grace periods live in its own task and its own tracker (`a_grace_on_one_server_leaves_the_other_alone_d41`).
A device stays in the map during its grace, as D24 removes it at the disconnect, and `borrow` skips it. D40 needs nothing:
the disconnect carries the same reference the connect did.

**Termination.** The timers belong to the tracking task, and go when its signal cancels it. The test runs with a clock that
never moves, so anything waiting for a timer would hang it.

## Tests

`crates/marathon-android/tests/offline_grace_test.rs` (10, with a stopped vendor clock so that a grace only ends by the
device's own state) and six `_d41` whole runs in `reconnect_test.rs` on the virtual clock, with the real scheduler.

**Proof by temporary revert.** Each change below was reverted alone, and the `_d41` tests run.

| Reverted | Tests failing |
|---|---|
| The grace itself | 12 of the 16 `_d41` tests |
| The device gate | `a_blip_fails_only_the_batch_and_later_batches_wait_d41`, `remote_parsing_waits_out_a_brief_offline_d41`, `a_device_that_stays_offline_is_disconnected_after_its_grace_d41` |
| The `DeviceLost` classification | The same three |
| The hold at the end of a batch | `a_blip_fails_only_the_batch_and_later_batches_wait_d41`, `a_device_in_its_grace_is_given_no_new_batch_d41` |
| The flap bound | `grace_periods_are_bounded_per_device_d41`, `a_flapping_device_runs_out_of_grace_d41` |
| No grace after an outage's window | `an_outage_does_not_add_a_grace_to_its_bound_d41` |
| The boot check | `a_device_that_returns_restarted_is_set_up_again_d41` |
| The grace surviving an empty list | `a_run_survives_a_brief_offline_on_its_last_device_d41`, `a_killed_emulator_is_disconnected_soon_after_it_leaves_the_list_d41` |
| The shorter limit when missing | `a_killed_emulator_is_disconnected_soon_after_it_leaves_the_list_d41` |
| The setup exclusion | `a_device_in_setup_has_no_grace_d41` |
| The parser's wait | `remote_parsing_waits_out_a_brief_offline_d41` |
| Restoring logcat | 4, among them the whole run |
| Releasing the gate | 7 |

## Real emulator checks

The native release binary of this branch's code on a Linux host with KVM, API 36 as above, a private adb server and private
emulators, USB guard passed. The tests are the fixture sample's `MainActivitySlowTest`: 5 tests of about 5.4 s, one to a batch.
Rows marked with `*` ran on a slightly earlier build of the branch, before the hold at the end of a batch, which does not
touch what they check. Quota is 3 unless stated.

| Check | Emulators | Outcome |
|---|---|---|
| No fault, twice (`*` once) | 1 | Exit 0, 5 of 5, no grace logged, about 40 s |
| adbd killed during a test, 3 runs (`*` two) | 1 | **Exit 0.** `Offline`, back in 1.0–1.35 s, no disconnect. 5 of 5, one `INCOMPLETE` retried. Before: exit 1 |
| `adb reconnect` during a test `*` | 1 | Exit 0. Back in 0.39 s, one `INCOMPLETE` retried |
| adbd killed during a test, remote parser `*` | 1 | Exit 0, as above |
| adbd killed during remote parsing `*` | 1 | Exit 0, 1 uncounted attempt, 5 of 5 |
| `adb reconnect` during remote parsing `*` | 1 | Exit 0, 1 uncounted attempt, 5 of 5 |
| adbd killed 8 times, 2.5 s apart `*` | 1 | Exit 0. 5 grace periods seen, 3 kills refused as `device offline`. 5 of 5 with 5 `INCOMPLETE`, at most 4 attempts of a test, within quota. `Batch failed because the device was briefly offline` logged |
| **Emulator killed during a test** (`*` once) | 1 | **Exit 1**, 12.5–12.8 s. Disconnected 3.25 s after `Offline`. Before: about 60 ms |
| **Emulator killed during a test** | 2 | **Exit 0**. Disconnected 3.25 s after `Offline`, the test retried on the survivor, 5 of 5 |
| **Emulator killed, quota 0** | 2 | Exit 1 with **one** `FAILURE`, the test that was running, its JSON `DISCONNECTED`, `healthy: false`; 4 passed on the survivor. On the build before the hold at the end of a batch: **two** failures, one of a test that never ran |
| adbd killed, quota 0 | 2 | Exit 1, one `FAILURE`, its JSON `DISCONNECTED`; the device returned and passed 2 more tests |
| `adb reboot` during a test | 2 | Exit 0. `returned as if restarted`, disconnected and connected at 8.1 s, setup waited for the boot, and it then ran 1 more test; 5 of 5 |
| `adb reboot` during a test `*` | 1 | Exit 1, at 8.5 s, as the last device left the pool. The same outcome as before D41, about 8 s later |
| adbd killed, then `adb kill-server` 0.1 s later `*` | 1 | Exit 0, 5 of 5, one `INCOMPLETE`. The outage took over; server back in 1.5 s |
| Emulator suspended with SIGSTOP for 15 s during a test `*` | 1 | Exit 0, 5 of 5. adb never reported anything, so no grace was involved; the test just took longer |

**Not verified.**

| Item | Why |
|---|---|
| A real TCP device or a USB device | None here. The farm figure above is from adb's source, not measured. A USB device may leave the list directly from `device`, which has no grace, as the brief asks. If so, this fix does not help it |
| The sixth `offline` on a real device | Three of the eight kills were refused while the device was down, so the run ended at 5. Covered by the fake, in both harnesses |
| A reboot that is complete by the time it returns, as from a quick snapshot | It would count as a blip, keep its actor and not be prepared again. A reboot took far longer here. D22 documents the same limit for an emulator restarting during an outage |
| A reverse forward returning after a blip | As in x12, an `emulator-` serial never has one. The fake covers the request path through D22's `resume_after_server_return` |
| JVM comparison | Not needed for any answer; the statement on Kotlin is from reading it |

One rebuild and the first five `*` runs (no fault, adbd killed twice, `adb reconnect`, adbd killed under the remote parser)
may have overlapped a workspace test run on the same host, which should have been waited out. It was noticed afterwards, and
every later batch waited first. The results of those runs match the later ones. That test run's own timing may have been
affected.

## Review

A second-model review, by reading the code plus three runs of `offline_grace_test`, found no hang, lost test or false pass that D41 introduces. Its limits, none reproduced:

- **A device listed `offline` in the first list after a server outage gets no grace.** `finish_outage` exempts every kept device, not only those the window held because they were missing. Not worse than D22, and bounded.
- **A batch can start on a device already `offline`, before the tracker has heard.** A few milliseconds wide, and it charges that batch, so with quota 0 its tests fail without having run. The same race existed before D41.
- **`offer` runs in one server's tracking task.** The boot check, the logcat wait and reverse-forward restores can make that server's other grace timers start late, so a grace can run longer than 10 s, never shorter.
- **The limit of 5 per run never resets.** Kept for this change: a sliding window would be fairer, but needs a clock source and its own tests. A device with 5 blips in a long run goes back to today's behaviour. Revisit it on evidence from farms.
- **D22's known charge is not fixed here.** A batch held at the start gate is still charged when its server or device is given up. It is narrower, since the hold at the end of a batch stops the actor asking for the next one.
- **Some negative tests are weak.** `offline_grace_test.rs` checks "nothing happened" after real 20–100 ms sleeps, which can't flake but can pass under load without proving the point.

## Incidental bugs

None new. Two things seen nearby and left alone:

- A batch waiting at a gate when its device is given up is released just before the actor's cancellation reaches it, so it
  can start on the lost device and fail at once. D22 has had the same since x12. It costs nothing beyond what the batch would
  have been charged anyway.
- `app-debug-androidTest.apk`'s fixture sample always fails some tests, so the checks above filter to one stable class.
