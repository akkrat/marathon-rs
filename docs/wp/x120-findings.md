# WP x120 — Retire an Android device that keeps failing batches

## Audit and farm evidence

The audit claim holds at `f7fabfa0`. Apple has a three-failure default and D39 counts a silent timeout without test
progress. Android's runner can instead finish an instrumentation with a run failure and send ordinary incomplete
results. The device actor requests another batch. D82 prefers another device for those retries but lets the bad
device take other tests or reclaim its own retries when nothing else is available. The released changelog records
that gap. D49 limits lost adb requests, not these incomplete batches.

The older Android farm case recorded in U204 demonstrates the failure mode: an instrumentation exited before any
test started four times on one emulator in about two seconds. Other emulators were busy, and a later run succeeded
on a different emulator. The available 0.15.6 Android trial logs did not show that sequence. They did show one
emulator's APK install timing out during preparation, which is not a batch failure.

## Design and invariants

- Apple and Android share the counter and default of three. Apple retains its separate failure and no-progress
  rules. Android observes one outcome per batch across all instrumentations. A reported pass or failure resets
  the streak and publishes progress to the scheduler's Android pool. The provider owns the per-pool state across
  adb servers. A replacement at the same endpoint and serial cannot be its own peer.
- A timeout waiting for output or an unreported nonzero shell-v2 exit before a named test is eligible, but
  neither shows why the batch failed. Without fresh peer progress it adds no strike. Instrumentation-reported
  outcomes, valid skips alone, setup or listener timeouts, ambiguous output ends, host or transport errors,
  cancellation, D22/D41 outages and D49 losses are not strikes. No extra adb probe is made.
- A D103 loss seals the instance and starts its normal cleanup, then returns `DeviceLost` to the actor. The
  actor accounts for the original batch result once, then stops. The batch's tests go through the usual D82
  placement and retry accounting. No new scheduling or cleanup path is added to core.
- A still-listed retired instance is kept only as tracking identity, not offered for borrowing or D85 replacement.
  An actual removal can be followed by a new connection, with the usual D24 cleanup ordering. A pool without
  peer progress cannot evict on this path. The D48 wait when a pool runs out of devices is unchanged.

## Initial verification before review

The initial fake-adb whole-run witnesses held the clock for the three failed launches, checked all three incomplete progress
lines appeared once, then either let a healthy emulator pass the retry or let D48 expire at its configured deadline.
The provider witnesses cover resetting with both a pass and a test failure, and a two-device outage whose interrupted
batches return only after both restoration gates open. After the outage only the device with a genuine third strike
is refused for borrowing.

All four witnesses passed 50 consecutive stress runs each, then one more run each with four CPU load workers.
In a scratch worktree the two whole-run witnesses failed on the base because the device kept taking retries and the
pool did not wait for another device. With D103 present but the reset and the entry-to-exit epoch check disabled,
the other two witnesses failed by retiring a device on the wrong batch. The scratch worktree was removed.

No existing result golden or JVM recording was changed. The whole-vendor CLI differential passed without a new
allowance. The new scenarios assert actual run verdicts, progress and transport requests without pinning unrelated
report bytes. No physical emulator was used.

## Incidental bugs and baseline issues

The source inventory and parity checks already report drift at the base commit against the available Kotlin
checkout at `e47352f2` (including missing Kotlin Apple code-coverage test rows). This WP adds no Kotlin test rows
and does not change either generated file.

## Review round 1

All three findings reproduced on `f909cdc2`. The old code treated an incomplete result made by the listener as
evidence against the device. That lost the distinction between a real instrumentation outcome and a command
failure, allowed a deadline in the file-sync listener to count with no request submitted, and did not check for a
missing shell-v2 exit when D25 let plain output produce a result.

Round 1 required a candidate signal from the runner. A reported instrumentation outcome rules it out before any
listener sees its events. Round 1 let an unexplained nonzero shell-v2 exit or an output wait timeout count.
Setup and listener timeouts were excluded.
Absent an exit frame, an apparent end is unclassified even if D25 turns the output into incomplete results.
Unclassified outcomes neither add a strike nor reset the streak. A passed or failed test still resets it.
No retry accounting, D25 result text, result goldens, or JVM recordings changed.

Four provider witnesses cover a clean zero-test result, a reported crash after a named test starts, a 30-second
virtual deadline gated inside the file-sync listener with zero instrumentation commands, and a cut of just the
instrumentation socket after plain output. Each failed at the third batch with `DeviceLost` in an archive of
`f909cdc2` with only the witnesses and fake server's targeted close step copied in. On the fix, all four passed 50
consecutive runs and one more under four CPU load workers, alongside the reset and outage witnesses. The latter
now holds otherwise countable launch failures across an adb restart, and fails in a scratch copy that only checks
connection state at the end. Round 1 retained the initial whole-run unexplained launch failure and D48 wait witnesses.

## Review round 2

A timeout waiting for output can fire before adb has acknowledged the shell request (R1). It can also fire
mid-test after a shared slowdown. The device list says only what adb lists. A nonzero shell exit can come from
shared command construction, such as an unmatched quote in an instrumentation argument before `am` starts (R2).
The old wording overstated the guarantees of submission and the device-list probe (R3). The probe is gone.

The first eligible failure opens the streak at that batch's entry after the tracker gate. A strike consumes
the current pool generation and requires another device's pass or failure since that entry or the previous
strike. Progress before that entry or from a different pool never counts. A burst buys only one strike per
device, and a device's own progress clears both its count and its cursor. A short vendor-side lock holds
the pool generation and the last two distinct sources' position. It never spans an await. D22/D41's epoch
still excludes a batch that saw an outage even if both gates have reopened by the time it returns.
The peer rule is evidence of contrast, not fault attribution. A shared failure supplies no progress of its own.

The whole-run gates alternate three bad batches with three actual peer completions, starting with peer progress
while the first bad batch is in flight. The run reports success and the peer executes all three unfinished tests.
The single-device run uses all six ordinary attempts instead of evicting at three. Provider witnesses cover
shared crashes, shared command errors, both deadline paths, stale or wrong-pool evidence, fresh progress
consumption, streak reset, and the round 1 exclusions despite available peer evidence.

In a scratch archive of `dccf6e8f` with only the witnesses and fake-adb gate copied over, four witnesses
retired the device on its third batch: the pre-ack output timeout, mid-test batch timeout, shared unmatched
quote and stale-progress window. The shared reported crash already passed there as intended. The scratch copy
was removed. No JVM recording or existing golden changed.

All 15 D103 witnesses passed 50 consecutive runs and one under four CPU load workers (765 checks). The unchanged
CLI vendor comparison passed all 16 captures against the existing recordings and goldens, using prebuilt
binaries after the shared disk fell below the build cutoff.

Formatting and strict clippy for adb, Android, Apple and core passed, as did the plan DAG, differential registry
and 117 evidence tests. The compiled tests passed for Android (337), adb (285, one ignored), fake-adb (4),
CLI (103, seven ignored, excluding the vendor comparison above), and targeted core (68) and Apple (14) suites.
The remaining tests reused prebuilt binaries after the build stop. The CLI config comparison also reused its
prebuilt evidence helper. The full workspace and remote gates were left to the orchestrator.
