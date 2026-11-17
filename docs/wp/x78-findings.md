# WP x78 findings: a simulator shut down during its boot wait (D79)

## Problem

A real 0.14.0-rc.1 run on a local iOS 26.4 simulator, shut down with `xcrun simctl shutdown` at the first poll of
marathon's boot wait, logged 237 `is still booting` polls over 240 s, then
`failed to boot: it has not booted within 240 s, its state is SHUTDOWN after 240010 ms, attempt 1 of 2`. The second
attempt booted it 15 s later (10 s retry pause plus the boot) and the run passed, 4 minutes late. The cause is
`boot_failure` in `crates/marathon-ios/src/apple_simulator_device/boot.rs`, a port of the Kotlin `bootFailure`
(`AppleSimulatorDevice.kt:879-911`): its loop ends only on Booted or at `bootWait`. Recorded as
[A108](../known-kotlin-bugs.md).

## Design

`boot_failure` keeps a `seen_booting` flag, set by any state read inside it that returns Booting (device.plist state 3
with the readiness probe not yet passing): the initial read, the read after a failed `simctl boot`, and each poll. A
poll that reads Shutdown while `seen_booting` is set returns the failure `it shut down while booting`. Everything
after that is the existing path in `boot_for_preparation`: the WARN line with the elapsed time and attempt number,
`shutdown_after_failed_boot` (which reads device.plist, finds state 1 and sends no `simctl shutdown`), the retry pause
(10 s), and attempt 2. The public `boot()` (true when booted) uses the same function.

Why "seen booting" and not a grace period: it adds no timing constant or configuration, and it tells apart the two
cases the brief names. A Shutdown read before any booting read is what a device.plist not yet rewritten after
`simctl boot` returns, or a Creating simulator becoming Shutdown, looks like; both keep the Kotlin wait. On a real
simulator `simctl boot` is expected to return once device.plist says state 3, so the first poll after it normally
sets the flag (not observed directly; see "Not verified").

The flag also applies when the simulator was already Booting before the wait (marathon sent no `simctl boot`): a
simulator seen booting that then reads Shutdown cannot boot by itself in that case either, and the retry's
`simctl boot` is what it needs. The brief speaks of "after the boot command was accepted"; this is the narrowest rule
that covers that case without a second branch, and it is stated in D79.

## Invariants

- A simulator that stays booting (state 3, probe failing) is waited for until `bootWait`, as before; D38/D46 probing
  is untouched, so the D38 times quoted in the CHANGELOG known limits do not change (the probe failure never reads
  Shutdown).
- Two attempts, the retry pause and the failed-boot shutdown are unchanged.
- D74: cancellation still interrupts the poll delay; a disposed simulator's preparation stops as before.
- Worker commands: the same commands in the same order, sent earlier. No configuration, result-file or CLI change.

## D-row decision

D79 is used. The commands sent to the worker are the same, but their timing changes, and the effective meaning of
`bootWait` for this case changes (the wait ends before it). Both are listed in PLAN §3.12 as contract (worker
commands, Marathonfile meaning), so the change gets a row: PLAN §3.11, `docs/deviations.md`, `docs/migration.md`,
CHANGELOG `## Unreleased`.

## Tests

In `crates/marathon-ios/tests/simulator_prepare.rs`, on the fake simctl and the paused Tokio clock (poll 20 ms, retry
pause 10 ms):

- `a_simulator_shut_down_during_its_boot_wait_is_booted_again_after_one_poll_d79`: `bootWait` 60 s; device.plist
  flips to Shutdown at the third poll of attempt 1. The second `simctl boot` follows the shutdown within 30 ms (one
  poll plus the retry pause), no `simctl shutdown` is sent, the WARN line reads
  `failed to boot: it shut down while booting after … ms, attempt 1 of 2`, and the simulator prepares.
- `a_simulator_that_stays_booting_is_still_waited_for_until_boot_wait_d79`: `bootWait` 2 s, never ready: `boot()`
  returns false after at least 2 s and 101 probes, with one `simctl boot`.
- `a_shutdown_state_before_the_first_booting_read_does_not_end_the_boot_wait_d79`: device.plist says Shutdown for
  the first two reads after `simctl boot`, then 3: the simulator boots with one `simctl boot`.

Proof that they bite, by temporary edits of `boot.rs` (restored afterwards):

| Variant | shut-down test | stays-booting test | initial-Shutdown test |
|---|---|---|---|
| base 573a962d (no fix) | **fails** (waits 60 s) | passes | passes |
| any Shutdown ends the wait | passes | passes | **fails** |
| any non-Booted read after a booting read ends the wait | **fails** | **fails** | **fails** |
| this fix | passes | passes | passes |

The stays-booting and initial-Shutdown tests pin kept behaviour, so they cannot fail on the unfixed base; they fail on
the plausible wrong fixes above. The existing tests of the file, ported from the Kotlin `StartupPhasesTest`, pass
unchanged, so no parity-matrix row changes.

## Verified

- `cargo fmt --check`; `cargo clippy -p marathon-ios --all-targets -- -D warnings`; `cargo test -p marathon-ios` (all
  suites pass).
- Differential and end-to-end suites: the fake worker (`tools/fake-worker/src/apple.rs`) sets a simulator Booted on
  `simctl boot` and never reads Shutdown after a booting read, and `tools/differential/vendor_transport.py` only
  matches the `simctl boot` command, so no recorded scenario can reach the new branch; no comparator allowance is
  needed. The marathon-cli suites were not run for that reason.

## Not verified

- No real-simulator rerun of the evidence scenario (run 06 of the rc.1 iOS checks); the fix is checked on the fake
  simctl only.
- Real device.plist timing right after `simctl boot`: the "Shutdown before the first booting read" case is modelled,
  not observed.

## Golden diffs

None. Result files and pinned progress/summary text are unchanged.

## Incidental bugs

- A simulator whose state is Creating when the boot wait starts gets no `simctl boot`; when it becomes Shutdown it
  cannot boot by itself and is polled until `bootWait`, as in Kotlin. Not changed here (it never read as booting).
  Whether preparation can meet a Creating simulator in practice was not checked.
