# WP x83 — Android return reconciliation

## Evidence and design

- At `3e53e6a4`, D22 restored logcat and reverse port forwards for devices still listed after an outage and reopened
  their gates. D41 asked for `sys.boot_completed`, which is present again after a full reboot. The same serial could
  therefore receive more batches with an instance set up for a different boot.
- An actor that exhausted all 30 preparation attempts sent its termination to the pool. The Android provider saw
  only adb's unchanged list and kept its old instance. The pool could spend all of D48's wait with no replacement
  pending. D49 is separate. The batch actor's request losses already have their own count, and reaching that limit
  does not report preparation exhaustion.
- The provider reads `/proc/sys/kernel/random/boot_id` with `cat` during setup. The kernel generates the UUID once per
  boot. An adb server or adbd restart keeps it. AOSP's API 21 policy makes the original proc label readable, and modern
  policy allows reads of `proc_random`. The API 36 emulator read the ID from an enforcing, unprivileged shell. AOSP
  restricts the framework's `ro.runtime.firstboot` property on newer releases.
  [API 21 policy](https://android.googlesource.com/platform/external/sepolicy/+/refs/tags/android-5.0.0_r1/domain.te),
  [boot ID policy change](https://android.googlesource.com/platform/system/sepolicy/+/9d9c370f31556c33f62e7889f64ad3a2728f1863%5E!/),
  [modern policy](https://android.googlesource.com/platform/system/sepolicy/+/refs/heads/main/private/domain.te).

## Invariants

- The boot check runs in U170's per-device recovery task. D41 shares one `shell` deadline between its readiness
  check and the boot ID read. D22 uses the same bound for the ID read. Two distinct, valid IDs confirm a reboot.
  A missing baseline, unanswered probe, or invalid ID preserves the existing D22 restoration and D41 readiness
  rules.
- The provider accepts a replacement only for the current instance of that adb server and serial. A replacement
  sends the normal disconnect before starting setup. It marks the old instance disconnected before releasing
  the gate, so only the new actor can run more tests. D70 returns an unstarted batch with its quota intact.
- Preparation exhaustion wakes the server tracker even when adb sends no new list. It can request one replacement
  while the serial remains listed. The retry flag survives an interrupted setup and the D41 fallback for an unknown
  boot. A confirmed new boot or an actual tracker disconnect starts a new allowance. The pool retains its original
  D48 deadline. The D49 request-loss count still applies per serial and server for the run.

## Verification and goldens

- The fake-adb reboot witness keeps the serial and `sys.boot_completed` unchanged, supplies a different kernel ID
  on return, and makes execution with the stale actor fail. At `uncompletedTestRetryQuota: 0`, one instrumentation
  reaches adb, the replacement executes it, and the run passes. The same-boot and unanswered-probe witnesses
  observe the added probe before the original actor resumes.
- The prepare witness consumes all 90 installer attempts across 30 actor attempts with a fixed adb list. It then
  receives one disconnect and one replacement, and the only queued test passes on the replacement. With installation
  still failing on that replacement, D48 ends the last device's pool at its configured return deadline.
- All five D85 witnesses passed on the branch and failed with the production code from `3e53e6a4`. The base run kept
  the stale boot, emitted no checks for the same-boot and unanswered cases, and sent no replacement after failed
  preparation.
- The D85 differential allowance covers the setup command between its last property read and its feature request.
  The Python projection also requires the fake's exact built-in response. The negative checks retain duplicate,
  misplaced and different commands and unexpected replies. JVM recordings, native result goldens and parity
  classifications are unchanged. The generated differential registry was refreshed.
- The ADB, Android, core, CLI, Apple, iOS and macOS crate runs passed except for two Android targets whose assertions
  still counted the old setup commands. Both targets passed after the assertions were updated. The device actor,
  pool scheduler, Android recovery and whole-vendor CLI differential suites also passed on their follow-up runs.
  Formatting, changed-crate Clippy, differential validation and evidence tests, the plan DAG, source inventory,
  parity matrix and fixture checks passed. The orchestrator owns the workspace and remote gates.
- API 21 support was checked against AOSP policy. The API 36 emulator check was read-only. Full reboots and OEM
  SELinux variants were covered by the fake and source checks only.

## Incidental bugs

- A new outage clears the prior outage's per-device restoration timestamps in `DeviceStateTracker::server_lost`.
  Consecutive outages before a restoration finishes can therefore count from the later outage for D48. The existing
  outage accounting is outside this work package.
