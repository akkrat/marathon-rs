# X17: Apple readiness, device failure matching and device removal (D38–D40)

The project owner asked on 2026-11-06 for these fixes to bugs the port had kept for parity. Each is an approved
deviation in PLAN §3.11. No configuration field, CLI option, exit code or command sent to a worker changed.

## D38 / A41: the exit status of the readiness probe

### The measurement came first

The trap: Kotlin may have dropped the exit check because the probe exits non-zero on a healthy simulator. Its only
comment is `//Not sure if it's really 16+`, and its history gives no reason: the code arrived whole in the iOS rework
and has not changed since. So the probe was measured before any code changed:
`xcrun simctl spawn <udid> launchctl dumpstate user/foreground/com.apple.SpringBoard`.

| Runtime | Simulator | State | Exit | stdout | stderr | Marker line |
|---|---|---|---|---|---|---|
| iOS 27.0, Xcode 27.2 | Throwaway, created for this and deleted | Shutdown, before boot | **149** | Empty | 4 lines, `device is not booted` | None |
| iOS 27.0 | Same | Booting, 6 probes from the first one after `simctl boot` | **0** | 69,581 to 135,618 lines | Empty | `0x4e03 M D com.apple.springboard.services` |
| iOS 27.0 | Same | Booted, 6 probes up to 20 s after it settled | **0** | 142,448 to 170,066 lines (19 MB) | Empty | `0x4e03 M A com.apple.springboard.services` |
| iOS 27.0 | Same | Shutdown again | **149** | Empty | 4 lines | None |
| iOS 18.6, Xcode 26.6 | Already booted, on a second Mac, read-only | Booted, 1 probe | **0** | 145,192 lines | Empty | `0x4703 M A com.apple.springboard.services` |

Each output has a second marker line, `"com.apple.springboard.services" = {`, whose last token is blank, so it never
counts. On iOS 27.0 the older probe, `launchctl print system`, exits 0 with 232 lines and no marker, which is why the
runtime branch has to stay.

So **the ledger row's description is right**: a healthy simulator exits 0, on both runtimes measured and in every phase
of the boot. The check is safe to add, and a failed or timed-out probe is not running, as below runtime 16.

**Not measured:** runtimes 16, 17 and 26. No simulator of them was booted, because the brief allows one throwaway
simulator here and nothing booted on the second Mac. Runtimes either side (18.6 and 27.0) agree, but a runtime that
exits non-zero on a healthy simulator would never count as booted, and each of its simulators would take the bound
below and leave its pool.

The throwaway simulator was created with `simctl create`, shut down, deleted, and confirmed gone from `simctl list
devices`. No other simulator was touched.

### What callers do with `false`

`is_running` has one production caller, `simulator_state`, which runs only when the descriptor's state is 3. `false`
means `Booting`, nothing else.

| Caller | With `Booted` | With `Booting` | Effect of a false negative |
|---|---|---|---|
| `boot_failure`, first look | Done | Goes on to poll | One more poll. `simctl boot` is sent only for `Shutdown` |
| `boot_failure`, poll loop | Done | Polls again after `boot_poll_interval` (1 s), up to `bootWait` | See the bound below |
| `boot_failure`, after a failed `simctl boot` | Treated as booting | Treated as booting | None |
| `shutdown_device` | Shuts down | Shuts down | None |
| `erase_device` | Shuts down, then erases | Shuts down, then erases | None |
| `wait_for_state` | Only ever waits for `Shutdown` | Same | None |

**The bound.** No path from `false` erases a simulator. One failed probe costs one more poll, shown by
`one_failed_or_timed_out_probe_costs_one_more_poll_and_no_new_boot_d38`. Only a probe that fails for the whole of
`bootWait` (180 s by default) reaches the failure path: the simulator is shut down, after a 10 s pause it is booted
once more, and after a second `bootWait` it leaves its pool with `NotBooted`. A probe in progress at the deadline may
overrun it by up to `shell` (30 s). `a_probe_that_never_succeeds_is_bounded_by_boot_wait_and_erases_nothing_d38` shows
this path: one boot, two shutdowns, no erase.

**How much is new.** Kotlin already takes this path for a probe that times out before it prints the marker, which sits
about 55% of the way through the output (line 93,060 of 170,066). The new cases are a probe killed after the marker,
and a non-zero exit with the marker in stdout. Locally a probe took 0.3 to 1.8 s, so reaching the 30 s limit on the
whole of `bootWait` takes a far slower host or link than measured here.

### Tests

The JVM rows stay the witness: `sim.running/{16,16.0,+16.1}/{fail,nul}` in `fixtures/oracle/apple-bin/probes.jsonl`
return `true`. `deviation` in `apple_bin_oracle.rs` expects `false` for exactly those six, changing only the return
value, and `d38_allowance_covers_only_failed_dumpstate_probes_that_kotlin_reported_as_running` pins the list and shows
that the healthy, starting and older-runtime rows are not covered. The command transcript is unchanged, so no
differential comparator needed an allowance. With the check temporarily removed,
`every_probed_wrapper_call_matches_kotlin` and both `_d38` device tests fail.

## D39 / A48: device failure text in a test's own output

### The literals and their real forms

The trap: missing a real failure is worse than a false alarm, because the batch then waits for its timeout. So the
real form of each literal was looked for first, in every source on this machine: `fixtures/`, the worktrees' real-e2e
inputs, the Kotlin tests and history, one real xcodebuild log of an application's test run (15,804 lines, private and
not committed), local session records, and the bytes of Xcode 27.2's and CoreSimulator's binaries (11,509 files
scanned, in UTF-8 and UTF-16).

| Literal | Reason | Form found | Source |
|---|---|---|---|
| `Failed to install or launch the test runner` | FailedRunner | The bare string, wrapping unknown | Binary (`XCTHarness`) |
| `Software caused connection abort` | ConnectionAbort | **Not found** in Xcode; it is the system's text for `ECONNABORTED` | None |
| `Unable to find a destination matching the provided destination specifier` | InvalidSimulatorIdentifier | **Not found** | Kotlin test, bare |
| `Terminating since there is no system app` | SystemAppUnavailable | **Not found** in binaries; bare with a full stop | Kotlin test |
| `Exiting because the workspace server has disconnected` | SystemAppUnavailable | **Not found** in binaries; `XCTRunner[4242:4242] …` | Kotlin test |
| `Not authorized for performing UI testing PropertyActions` | Unknown | **Not found** | None |
| `Timed out waiting for automation session` | Unknown | **Not found** | None |
| `Failed to terminate` | Unknown | `Failed to terminate %@`, `… after %.1fs; state is still`, `… application %@: %@`, `UI testing error: (XCUITestingErrorDomain) … Failed to terminate %@`, and four more in other frameworks | Binaries (`XCUIAutomation`, `CoreDeviceUtilities`, `CoreSimulatorBridge`, `Subprocess`) |
| `Failed to launch app with identifier` | Unknown | `%@: Failed to launch app with identifier: %@ and options: %@ (error = %@)`, recorded as `iOSSimulator: <udid>: Failed to launch app with identifier:` | Binary (`IDEiOSSupportCore`), real JVM log |
| `Test runner exited before starting test execution` | Unknown | **Not found** in binaries; bare | Kotlin test |
| `Early unexpected exit, operation never finished bootstrapping` | Unknown | **Not found** in binaries; bare | Kotlin test |
| `Connection peer refused channel request` | UnreachableHost | `Connection peer refused channel request for "%{public}@"; channel canceled %@` | Binary (`DTXConnectionServices`) |
| `CoreSimulatorService connection became invalid` | ServicesUnavailable | `… became invalid.  Simulator services will no longer be available.` | Binary (`CoreSimulator`) |
| `CoreSimulatorService connection interrupted` | ServicesUnavailable | `… interrupted.  Resubscribing to notifications.` | Binary (`CoreSimulator`) |
| `Simulator services will no longer be available` | ServicesUnavailable | As above, and `Loaded CoreSimulatorService is no longer valid …` | Binary (`CoreSimulator`) |
| `Unable to locate device set` | ServicesUnavailable | `Unable to locate device set: %@` | Binary (`simctl`) |
| `Test runner exited` | Unknown | **Not found** in binaries; bare | JVM recordings |

**No recording here holds a real device failure in xcodebuild's stdout.** The real log has none, and the JVM rows are
synthetic bare lines. So for nine literals, flagged **Not found** above, the form is not established, and the others
are known only as Apple's strings, not as lines in that stream. Tightening any literal to a form would be a guess.

The forms found rule out the obvious fix. A literal at the start of a line would miss `iOSSimulator: <udid>: Failed…`,
`xcodebuild: error: Unable to find…`, `XCTRunner[…] Exiting…`, `Error Domain=… "Failed to install…"` and the Kotlin
test's `Test runner exited before starting test execution: Terminating since…`. Ignoring raw lines while a test runs
would miss the case that matters most, a runner dying in the middle of a test.

### The rule

So the literals keep today's matching, and D39 instead proves which *lines* are a test's or an application's text.
This reconciles the two requirements of the brief: an unknown form keeps matching, while no literal aborts a batch from
these lines. Each is a form recorded in the real log or in `fixtures/`:

| Line | Example | What can still report a failure |
|---|---|---|
| Test case | `Test Case '-[C name]' started.`, also finished and timeout | What follows the name |
| Test suite | `Test Suite 'name' started at …` | What follows the name |
| Assertion (`FAILING_TEST`) | `/a/T.swift:33: error: -[C m] : message` | Nothing |
| UI test activity | `    t =     4.20s Tap "text" Button` | Nothing |
| Swift Testing event | `◇ Test "name" started.`, `✔`, `➜`, `↳`, with zero-width spaces before them | Nothing |
| Log line of an application | `2026-11-06 12:08:08.118250+0300 App[1:2] [general] text` | Only XCTest's own sentences, see below |
| Log line of `xcodebuild`, `simctl`, `…Runner` | `… xcodebuild[1:2] [MT] text` | The whole line |
| Anything else | | The whole line, as in Kotlin |

Besides the application under test, those are the tools expected to write into this stream. **This list is reasoned,
not recorded:** a real failure logged with a date header by any other process would now be missed, and is the first
thing to check if a batch waits for its timeout. XCTest also runs inside a hosted test's application, whose name says
nothing, so there `Terminating since there is no system app`, `Exiting because the workspace server has disconnected`
and `Connection peer refused channel request for "` still count when they are the whole message.

`✘` and `⚠` are not in the real log, which had no failing Swift Testing event, nor are the macOS symbols in the private
use plane. They are included because they belong to the same set of markers and a failed test's line carries its
message.

**Still a false alarm, and flagged:**

- A raw `print` from a hosted test or application with no log header: it cannot be told from an unknown form.
- The later lines of a multi-line assertion message, for the same reason.
- A test name in xcodebuild's final `Failing tests:` summary.
- An application logging exactly one of XCTest's three sentences.

**Behaviour to know about:** `<unknown>:0: error: -[C m] : Failed to launch app with identifier: …`, or the same in a
`t =` line, used to end the batch as a batch-level failure. Now that test fails with this text as its trace, the runner
goes on, and the next test may fail the same way. No batch waits for a timeout here, as the runner is alive and reports
each result, but such a batch takes longer, and retries follow the test's failure and not the batch's.

The check runs only on a line holding a literal, so ordinary lines cost what they did.

### Both directions over the corpus

| Corpus | Result |
|---|---|
| `oracle/apple-logparser/probes.jsonl` | 307 strings detected (the 153 failure lines and their recorded events), 1 changed: the witness below |
| `oracle/simulator-device/events.jsonl` | 44 detected, none changed |
| `oracle/simulator-device/tests.jsonl` | 2 detected, none changed |
| `oracle/test-runner/probes.jsonl` | 3 detected, none changed |
| Forms from the table above, rendered | 21 detected, in `every_real_failure_form_is_detected_d39` |
| All 17 literals × 9 reporting forms | 153 detected |
| All 17 literals × 22 test and app forms | 374 not detected, in `a_test_name_an_assertion_or_an_app_log_line_is_not_a_failure_d39` |
| Real xcodebuild log, one temporary run, not committed | 15,804 lines, 5 held a literal, all `App[…] [general] Failed to terminate process: …`, 0 now detected. **Kotlin's parser reports a failure at the first of them** |

The only JVM row that changes is `Test Case '-[p.C Test runner exited]' started.` in `failure-literals`: recorded as
`TestRunFailed`, it now reaches the progress parser. `jvm_streams_replay_events_errors_and_debug_lines` names that one
line and checks the recorded value first, and `every_failure_line_in_the_recordings_is_still_detected_d39` pins the
counts. Commands and transcripts are unchanged, so the differential comparators needed no allowance.

`failures_preempt_progress_and_class_split_keeps_a5_a7` still shows a true failure going first: on a timeout line the
progress parser claims too, with the literal after the name. On device level,
`a_real_failure_after_a_tests_own_text_still_ends_the_batch_d39` ends the batch at the failure and keeps the earlier
result, and `a_tests_own_text_does_not_end_the_batch_d39` runs it to the end. With the rule temporarily removed, those
two, the corpus test, the JVM replay and both `_d39` line tests fail.

## D40 / C27: removing a device compares identity

### Which identity

The instance, `DeviceRef`, compared by pointer as readiness and termination do. The id string cannot work: a simulator
that comes back has the same udid and serial, and an Android device the same `host:port:serial`, so a stale event would
still match. The actor is not needed: the pool's map holds one actor per device, and `RemoveDevice` carries the device.

`remove_device` now removes only if the map's actor belongs to that instance. Its logs are unchanged, and a stale event
still evaluates `no_active_devices`, as any removal of an unknown device does in Kotlin.

### Removing the old device still does what it must

An older instance is out of `devices` only after one of two things: `RemoveDevice` told its actor to terminate and
released its request, or its actor reported `Terminated`. `add_device` rejects a same-serial device while the old one is
present, so a replacement is in the map only after one of those. By then the old batch is on its way back, so the stale
event has nothing left to do, and it no longer takes the replacement's actor, batch or request with it.

### Every sender

Every sender passes a clone of the reference it announced, so `DeviceRef` equality holds. If a vendor sent a fresh
instance on disconnect, a device would never leave its pool, which is why this was traced first.

| Sender | Instance sent | Can it arrive stale? |
|---|---|---|
| Android `provider.rs` | `Provided.reference`, a clone of what `Connected` carried. One map entry per `(server, serial)`, so one `Disconnected` per instance | No. One loop handles the tracker's updates in order, and `Disconnected` follows the setup task's join when it was not ready. With D18 the serial includes the server. After D22 and D24 a reconnect makes a new instance and a fresh actor, after the old `Disconnected` |
| iOS tracker, `simulator_provider.rs` | `device.device` from `state.devices`, the `Connected` reference | `state.removed` and the snapshot are taken under one lock, and `register` refuses a removed id, so no order found publishes the old one after the new one |
| iOS recovery, `recovery.rs` | `DeviceRef(handle.clone())`. `AppleSimulatorConstructor` builds `handle` and `device` from one `Arc`, so they compare equal | Published before re-provisioning, once per `recovering` entry. A tracker removal in between sends a duplicate, before the new device exists |
| macOS `provider.rs` | No `Disconnected` is ever sent | No |
| Scheduler `on_device_disconnected` | Forwards the same reference to every pool (B3) | Passes on what the vendor sent |
| `marathon-test-support` scenarios | `stub.reference()`, the same `Arc` | Only if a scenario scripts it. None changed |

So **no vendor path found today produces the stale order**, and the JVM case is the pool-level row. D40 makes the pool
right on its own, instead of relying on each provider's ordering. Android, iOS and macOS crate tests pass unchanged.

### The pool's last device, every order

| Order | Behaviour | Test (50 repetitions) |
|---|---|---|
| Old removed, new added, old removed again | The new device holds the pool open, and ends it when it goes | `the_last_device_ends_the_pool_with_the_old_device_removed_first_d40` |
| New added first | Rejected, as in Kotlin. Its disconnect changes nothing. The old device's ends the pool | `the_last_device_ends_the_pool_with_the_new_device_seen_first_d40` |
| Old removed twice, no replacement | The pool waits for the terminating device and ends once | `a_duplicate_disconnect_of_the_last_device_ends_the_pool_once_d40` |
| New removed twice | The first terminates it, the second finds nothing | In the old-first test |
| Disconnect of another pool's device (B3) | Nothing here. An empty pool still ends, as in Kotlin | `a_disconnect_of_another_pools_device_changes_nothing_here_d40` (one run), `unknown_disconnect_still_terminates_an_empty_pool` |

`stale_disconnect_leaves_the_replacement_d40` replaces `stale_disconnect_removes_the_replacement`, with the JVM row
`C27` in `fixtures/oracle/pool/scheduler.jsonl` as the witness that the old behaviour left only `keeper`.
`stale_disconnect_returns_the_old_batch_and_keeps_the_replacements_d40` shows the batch coming back and finishing on the
replacement. With the check temporarily removed, those two and the old-first and new-first tests fail.

## What was run

`cargo fmt --check`; `cargo clippy -p marathon-core -p marathon-apple -p marathon-ios --all-targets -- -D warnings`;
`cargo test` for `marathon-core`, `marathon-apple`, `marathon-ios`, `marathon-macos`, `marathon-android`,
`marathon-test-support` and `marathon-cli`; `tools/differential/make_registry.py --check`;
`tools/differential/test_evidence.py`. The gates (`scripts/check.sh`, `scripts/remote-check.sh`, the workspace suite) and
a real batch through xcodebuild were not run, as the brief asks: the orchestrator runs the gates.

## Incidental bugs

Kotlin behaviour seen nearby and left alone:

- **The readiness probe is huge.** Each `launchctl dumpstate` returns 145,000 to 170,000 lines (19 MB on iOS 27.0), and
  `boot_failure` holds it all in memory to read one line, once per poll. Over SSH it also counts against the flow
  control x16 changes.
- **A replacement seen before the old device's disconnect is dropped for the run.** `add_device` warns and returns, as
  in Kotlin, so the pool never gets the new device (`duplicate_serial_add_keeps_the_original_device`). The iOS and macOS
  `register` paths can do this by disposing a previous device with no `Disconnected`. The tracker only connects an absent
  key, so none was found reachable on iOS; there, a later disconnect of that new device no longer removes the old one,
  which then stays until its next batch fails or its queue ends.
- **`Not authorized for performing UI testing PropertyActions`.** Nothing in Xcode 27.2 matches `Not authorized for
  performing`. The literal dates from the pattern list's first commit, so it may never match on current Xcode.
- **`print system` cannot detect readiness on current runtimes.** On iOS 27.0 its output has no marker, so an unparsable
  runtime version never counts as booted. Only an unusual version string reaches this branch.
