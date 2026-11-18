# x85 — one deadline across a device's preparation and its recovery retries (D87, U209)

A device that cannot be prepared could keep its slot in the run far longer than any one of its timeouts says, because
retries were nested and each layer counted its own attempts: the device actor's 30 attempts 10 s apart around a
preparation, the Android installer's three attempts per package inside it, and on iOS the two boot attempts of every
preparation times the three replacements a recovery sets up after a failed boot. This package gives each preparation one
deadline that every layer reads, so that the layers can no longer multiply. Approved as D87 (2026-11-18, project owner's
decision).

## The evidence, re-verified

Checked against e8cd80be (x82, which this branch is based on; the files quoted were not changed by x82 in the places
used).

- **Device actor.** `crates/marathon-core/src/execution/device/actor.rs` (`initialize`): `with_retry(30, 10_000, …)` around
  `Device::prepare`. A `VendorError::LeavePool` (iOS `NotBooted`, `NotAdmitted`, `TransferStalled`) and a cancellation
  are not retried; every other error is, 30 times.
- **Android installer.** `crates/marathon-android/src/installer.rs` (`reinstall`): three attempts per package, 1 s apart.
  An install that times out is `AndroidKind::Install` ("Timeout installing …", `transfers.rs`, `install_package`), mapped
  to `DeviceSetup`, so the actor retries the whole preparation: 30 × (3 × 60 s + 2 s) + 29 × 10 s = 5,750 s, about 96
  minutes, for one package whose install times out every time. Confirmed.
- **iOS boot.** `crates/marathon-ios/src/apple_simulator_device/boot.rs` (`boot_for_preparation`): two attempts, each a
  `bootWait` of polling, then a shutdown limited to `shutdown` + `shell`, 10 s apart. Two failures are `NotBooted`, a
  `LeavePool`: the actor does not retry it, the simulator is marked lost.
- **iOS recovery.** `crates/marathon-ios/src/device/simulator_provider/recovery.rs` (`reconnect`, `recover`): each loss of a
  simulator counts; up to three recoveries, after pauses of 0, 30 and 60 s, each re-provisions the simulator, whose new
  handle gets a new device actor and a whole new preparation. So a probe that never succeeds costs four preparations,
  about 1,570 s with a probe failing at once, 1,810 s with one timing out at `shell`, and 1,970 s with one needing KILL
  ([x42](x42-findings.md), "With another simulator alive"). Confirmed by the witness below, scaled: 25.4 s for what the
  deadline ends at 8.1 s.
- **Apple installer.** `crates/marathon-apple/src/apple_application_installer.rs` (`prepare_installation`):
  `with_retry_send(3, 1000)` around the transfer. A stalled transfer is `TransferStalled` (D54), a `LeavePool`, and is not
  retried by either layer; a transfer that progresses has no limit (D54).
- **macOS.** `crates/marathon-macos/src/device.rs` (`prepare`): no recovery replaces a Mac during its preparation, so the
  nesting is the actor's 30 attempts around the installer's three. A Mac whose worker leaves every command hanging fails
  each preparation at its first command, `shell` plus up to 20 s for KILL, so 30 × 50 + 290 = 1,790 s, about 30 minutes.

## Design

**One deadline per preparation episode.** A new `Device::preparation_time_left_millis(&Configuration) -> Option<i64>`
(`crates/marathon-core/src/vendor.rs`) opens the device's episode and says how much of it is left. The device actor calls
it once, when its preparation starts, and turns it into a deadline on its own clock; the vendor keeps the same deadline
on its clock. Passing a length rather than a time keeps the two clocks out of each other's way: in production both are the
run's timeline, in some tests they start at different instants. The default is `None`, no deadline: the Stub vendor and
test vehicles keep Kotlin's 30 attempts, so no oracle or differential recording changes (below).

**Its length** comes from existing configuration only (`preparation_budget_millis`): the greater of
`deviceInitializationTimeoutMillis` and one preparation with the retries inside it at their limits:

| Vendor | Envelope | Defaults |
|---|---|---|
| iOS | 2 × (`bootWait` + `boot` + `shutdown` + `shell`) + the 10 s pause between boot attempts | 2 × (180 + 30 + 30 + 30) + 10 = 550 s |
| Android | per package to install (the app if any, each extra app, the test app): 3 × (`shell` + `uninstall` + `pushFile` + `install` + `shell`) + 2 × 1 s; plus 3 × `shell` (mock location, `logcat -c`) | 2 × (3 × 180 + 2) + 60 = 1,144 s with one app and its test app |
| macOS | 10 × `shell` (three directory commands, two in each of the installer's three attempts, the test type's probe) + 2 × 1 s; the transfer is not counted, it has no limit while it progresses (D54) | 302 s |

With the defaults every envelope exceeds the actor's own schedule for a preparation that fails at once (30 attempts, 290 s),
so such a device keeps its 30 attempts. Only slow failures are cut.

**What reads it.**

1. The device actor (`with_retry_until`, `crates/marathon-core/src/executor/retry.rs`) makes a retry only if it would
   start before the deadline; otherwise it returns the last failure and logs `Device <serial> is not prepared within its
   preparation time of <n> ms, not retrying after <k> attempts` (WARN). An attempt already running is never stopped.
2. The Android installer (`AndroidAppInstaller::with_deadline`) retries a package only if the pause and another whole
   attempt fit before the deadline; otherwise it returns that failure (WARN `Not installing <package> on <serial> again:
   the device's preparation time would run out`). The first attempt of each package always runs. The remote parser's
   installation is not part of a preparation and has no deadline.
3. iOS keeps the episode on the simulator handle (`apple_simulator_device/episode.rs`):
   - The first boot attempt of an episode waits the whole of `bootWait`, whatever is left, so a simulator whose
     preparation waited long before its boot (for its predecessor's artifacts, say) is not cut short. Every later boot
     waits for at most what is left minus a reserve of `shutdown` + `shell`, the limit of the shutdown that follows a
     failed boot; with nothing left it boots nothing (`its preparation time has run out`), unless it is already booted.
   - A second boot attempt is made only if, after the pause, a wait would still fit with the reserve.
   - A recovery after a loss sets the simulator up again only if, after its pause, a boot would still fit with the
     reserve; otherwise it logs `Simulator <serial> has not got ready within its preparation time, not re-provisioning it
     again` and marks the simulator exhausted, the path the fourth loss already took.
   - The replacement continues the episode: the recovery hands it to the setup it starts (`carried`), and the provider's
     admission of the new handle, during that setup, gives it to the handle. Only a loss counted by a recovery (a
     failed preparation) carries it; a worker's loss does not, and a setup that registers later (a retry's) starts its
     own. A preparation that ends ready closes the episode, so a later loss in a batch starts afresh.
4. macOS has only the actor's layer.

**Departure** is through the existing paths: the actor's failed preparation (`Terminate`), iOS `NotBooted` and the
recovery's give-up. D48 still ends a pool that has no device sooner. Nothing new waits, so nothing can hang on the last
device of a pool.

**Not capped:** a transfer that keeps progressing (D54), and any step already running: the deadline stops new attempts and
shortens waits, it never cancels. A rsync that runs past the deadline finishes, and only then do the layers see that no
time is left. Cancelling in-flight work at the deadline would cap such a transfer, which is the owner's decision (below).

## Invariants

- No retry, boot attempt or replacement of a preparation starts after its deadline; the Android installer and the iOS
  boot also start none that could not end before it.
- The first attempt of everything in the first preparation runs as before; a healthy run makes no other.
- A device without an envelope (Stub, test vehicles) has no deadline.
- The iOS shutdown after a failed boot keeps its reserve inside the deadline.

## Worst cases, before and after

Defaults, a live sibling (so D48 does not apply), commands other than the probe or the install answering at once. "Before"
is 0.14.0 / e8cd80be.

| Case | Before | After |
|---|---|---|
| iOS, probe fails at once | 4 preparations, about 1,570 s (26 min) | preparation (fails at 371 s), one replacement whose boot waits the 119 s left minus the reserve: given up at about 490 s (8 min) |
| iOS, probe times out at `shell` (30 s) at each check | about 1,810 s (30 min) | about 520 s (9 min): the replacement's window is 60 s, and its last probe runs 30 s past it |
| iOS, probe needs KILL (50 s) | about 1,970 s (33 min) | about 540 s (9 min) |
| iOS, any of these, pool's only simulator | 9 to 11 min (D48, x42) | unchanged: D48's wait from the first departure ends it |
| Android, one package whose install times out at 60 s every time | 30 × 182 + 290 = 5,750 s (96 min) | eight preparations, the last three with one attempt: leaves at about 1,160 s (19 min) |
| macOS, every command hanging until KILL | about 1,790 s (30 min) | six preparations: about 350 s (6 min) |
| Any vendor, preparation failing at once | 30 attempts, 290 s | unchanged with default timeouts (each envelope ≥ 290 s) |

The general bound: the deadline, plus what was running when it passed, which finishes under its own limit (a probe: `shell`
plus 20 s for KILL; an Android preparation started just before the deadline: one attempt per package; a transfer: no limit,
D54). With `deviceInitializationTimeoutMillis` above the envelope the deadline is that instead.

## What was verified and how

Witnesses (each fails without its fix, checked by a temporary mutation; the guards pass either way, as they should):

- `crates/marathon-core/tests/device_actor.rs`, virtual time: `slow_failing_preparations_stop_at_the_preparation_deadline_d87`
  (60 s failures in a 200 s episode: 3 attempts, `Terminated` at 200 s, one WARN; 30 attempts without the fix),
  `an_attempt_that_starts_before_the_deadline_runs_to_its_end_d87`; guards
  `slow_failing_preparations_without_a_deadline_make_thirty_attempts_d87` and
  `a_transient_preparation_failure_is_still_retried_within_the_deadline_d87`. Mutation: the actor ignoring the deadline
  fails the first two.
- `crates/marathon-ios/tests/simulator_preparation_deadline.rs`, the real scheduler, pool, actor, provider, recovery and boot
  loop on the fake worker, real time with `bootWait` 3 s, `shell`/`boot`/`shutdown` 0.5 s (deadline 9.1 s), a sibling held
  in its preparation so that it is live:
  `a_simulator_whose_probe_never_succeeds_leaves_within_its_preparation_deadline_while_a_sibling_keeps_the_pool_d87`: given
  up at 8.1 s with the D87 message, two setups, three boots, every boot shut down, the last shutdown before the deadline,
  the replacement's boot ended inside its window; the sibling then runs the tests. Mutation (iOS opens no episode): given
  up at 25.4 s after three replacements, the test fails. Guard
  `a_replacement_within_the_preparation_deadline_still_recovers_a_simulator_d87`: a failed first preparation is replaced
  and the replacement runs the tests.
- `crates/marathon-android/tests/reconnect_test.rs`, the real provider, scheduler, pool, actor, installer and runner on
  fake adb in virtual time, `shell`/`uninstall`/`pushFile` 5 s, `install` 10 s (deadline 199 s), the pool's only device:
  `an_android_device_whose_installs_time_out_leaves_within_its_preparation_deadline_d87`: the run ends at 229.7 s (the
  device leaves inside the deadline, then D48's 30 s), 16 installs (the parser's 2, four preparations of 3, two of 1), one
  actor WARN, two installer WARNs. Mutations: no deadline: the harness's 400 s limit is reached; installer ignoring it:
  the run ends at 231.7 s, past the bound. Guard
  `an_android_install_that_times_out_once_is_still_retried_within_the_deadline_d87`.
- `crates/marathon-macos/tests/batch.rs::a_macs_preparation_deadline_is_the_greater_of_the_wait_and_its_commands_d87` pins
  the macOS budget (302 s with defaults, the wait when that is greater).

Suites run: `cargo test -p marathon-core`, `-p marathon-ios`, `-p marathon-android`, `-p marathon-macos`, `-p
marathon-cli` (see the report for results), `cargo fmt --check`, `cargo clippy` on the changed crates.

**Oracle and differential recordings.** None changes, so no comparator allowance is needed: the device-actor oracle
(`fixtures/oracle/device-actor`, including `thirty failing preparations`) runs the Stub vehicle, which has no deadline; the
fake-adb Kotlin comparisons (`fixtures/fake-adb/android-reconnect`) fail at most three preparations, quickly, far inside
any deadline; the CLI differential suites ran unchanged. A narrow allowance would only be needed if the Stub vehicle got an
envelope, which it has no reason to.

**Golden diffs:** none. Result files, the Marathonfile, the CLI and worker commands are unchanged except for the commands a
device no longer gets (later attempts, boots and replacements).

## Not verified

- No run on a real simulator, emulator or Mac.
- The macOS deadline is exercised only through the core actor's tests and a pinned budget, not a whole macOS run.
- The worst-case figures above are computed from the code and the witnesses' scaled timelines, not measured with the
  defaults.

## Owner decisions

- **Progressing transfers stay uncapped (D54).** The deadline never cancels a step in flight, so a slow rsync of a large
  bundle can still keep an iOS or macOS preparation past it. Capping it means cancelling at the deadline, which would also
  cut slow but healthy transfers; left to the owner.
- **Android's envelope is generous** (19 minutes with the defaults and two packages) because it counts the push and the
  install of every attempt; a tighter one would cut a slow but working device sooner.

## Incidental bugs

None found.
