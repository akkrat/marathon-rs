# x107 findings: 0.15.3 release notes

Release documentation for 0.15.3, which ships x105 (measurement only) and x106 (U263–U266) on top of 0.15.2. No code
changes.

## Changes

- `CHANGELOG.md`: the `## Unreleased` bullets from x106 become `## 0.15.3`, under an intro, an iOS and a Reports
  heading. Evidence and Known limits are carried forward from 0.15.2 and changed to what is true for this release:
  - Evidence: the private CI farm's 0.15.2 video run whose refusals match the overlap pattern (not rerun on
    Xcode 26.6), the x106 witnesses and reviews,
    a real SSH-worker run of the overlap against 0.15.2, the x105 measurement, CI, and what was not exercised.
  - Known limits: the limit that the supervisor's warning repeats per reply is removed (x106 makes that line
    debug-only and reports a failed recorder once). Limits are added for tests that end while their recorder
    still waits (no video; with `ON_ANY` one not-delivered error, as in 0.15.2), for an unconfirmed recorder (no
    further video on that simulator until set up again) and for the Perl-less legacy recorder (no failure warning).
    The macOS UI XCTest limit is reworded so it covers this release.
- `README.md`: 0.15.3 is the current release, with one clause on what it changes.
- `docs/deviations.md`: the intro says what 0.15.3 changes for D98; the x106 note carries the version.
- `docs/migration.md`: the recorder-failure warning moved from the worker supervisor (now debug-only) to the iOS
  recording code; log alerts that matched the old line should match the new warning.

## Real-host evidence

A real SSH-worker check of main `5db150ed` against 0.15.2, on throwaway simulators (Xcode 27.2): the overlap
scenario (five instant tests, then a failing one, video on) gave 7 refusals in 4 runs on 0.15.2 and none in 4 runs on
`5db150ed`; two healthy video runs, D98's busy recovery with `debug: false` and an early stop passed. The overlap
could not be run under Xcode 26.6 (the test runner was built with Xcode 27.2). No bug was found.

## Review

One review by another model (REQUEST_CHANGES), all findings taken:
- "Report files are unchanged" became "report formats are unchanged": a run can now have videos 0.15.2 lost.
- The migration note lists all recorder messages on `marathon_ios::busy_video` instead of one, and says which have
  the token and the restart advice.
- The farm's refusals are stated as consistent with the overlap, not as proven (x106 left that unverified).
- The overlap guarantee is scoped to local simulators and supervised workers, and "tests are not delayed" became
  what the code does: a test does not wait for its recorder.
- The summary speed-up is described as indexing once (each final result still lists its own attempts), with the
  benchmark and fake-adb conditions named; the x105 gap is a median.
- The real-run evidence quotes the measured numbers instead of "took no longer".
- Known limits as above.

Re-check (REQUEST_CHANGES), all taken: the iOS entry no longer states the farm cause as fact; the recorder messages
moved to a new migration section, "iOS recorder warnings", listing every message, with a row in the action table;
"reports equal as data" dropped from the real-run evidence (videos and timings differ); the intro and README scope the
overlap fix to local simulators and supervised workers.

## Incidental bugs

None.
