# x112 findings: 0.15.4-rc.1 release notes

Release documentation for 0.15.4-rc.1, a release candidate that ships x110 (run-end timing lines) and x111 (the SSH
worker supervisor waits on events) on top of 0.15.3. The owner chose a release candidate first, so that a private CI
farm runs it before it becomes the latest release. No code changes.

## Changes

- `CHANGELOG.md`: the `## Unreleased` bullets become `## 0.15.4-rc.1`, under an intro and an "iOS and macOS workers"
  and a "Logging" heading. Evidence and Known limits are carried forward from 0.15.3 and changed to what is true for
  this release candidate. Evidence: the x110 and x111 findings, a real SSH-worker check against 0.15.3, CI, and what
  was not exercised. Known limits: x111's two timing edges and the zero-byte short iOS video are added.
- `README.md` is unchanged until 0.15.4 itself, as for earlier release candidates.

## Real-host evidence

A real SSH-worker check of main `3e83abaa` against 0.15.3 on throwaway simulators (Xcode 27.2): healthy runs, an early
and a mid-run stop, a crash with lease expiry, D98's busy recovery and the 0.15.3 overlap scenario all passed; the
supervisor used about a third less CPU per run, and the per-test times matched. It found one pre-existing,
low-severity issue: a recording only a few hundred milliseconds long is sometimes published as a zero-byte video with
no warning when its transcode fails (2 of 5 overlap runs on 0.15.3, 3 of 5 on the candidate). Not changed here; noted
for the stability work.

## Review

One review by another model (REQUEST_CHANGES), all findings taken: the logging entry says that only iOS has the
artifact finalization line and that the slowest device is named only when every disposal finished; "report files
unchanged" became "report formats unchanged"; the supervisor entry and limit describe the 25 and 100 ms values as caps
on its wait, the wake-up pipe as opening after about 250 ms, and the exit-check rule at expiry and KILL; the cutoff
wording says when TERM and KILL are due; the evidence is scoped to what was measured; the zero-byte short video is a
known limit; the findings link no longer claims review counts.

Re-check (REQUEST_CHANGES), all taken: the paused-supervisor edge is a 100 ms cap on a wait started from an earlier
clock reading (it also covers a pause before the wait starts); the escalation rule says when the 2 s timer starts and
what makes TERM and KILL due, also in the carried D96 limit, which said "up to 3 s"; the intro qualifies unchanged
results by the timing edges; the x111 findings' summary uses the same exit-check rule and calls 100 ms a cap.

A last small check (another model) asked for three wording fixes, taken: the carried D96 limit states the full
escalation rule; the x111 findings say an exit visible at the exit check wins and otherwise the expiry and escalation
rules apply, and call 100 ms a cap on a wait chosen from a stale clock reading.

## 0.15.4

The private farm check of 0.15.4-rc.1 found nothing blocking: with video off, the candidate matched 0.15.3 in total
and job time and ran its tests 1.4 % shorter inside XCTest; with video on, every failed attempt had its video and no
recorder was refused. 0.15.4 is the candidate's code. The `## 0.15.4-rc.1` section becomes `## 0.15.4`, its Evidence
gains the farm run, a stub `## 0.15.4-rc.1` section follows it as for earlier candidates, and `README.md` names 0.15.4
as the current release.

## Incidental bugs

The zero-byte short video above.
