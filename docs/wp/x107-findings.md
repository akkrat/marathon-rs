# x107 findings: 0.15.3 release notes

Release documentation for 0.15.3, which ships x105 (measurement only) and x106 (U263–U266) on top of 0.15.2. No code
changes.

## Changes

- `CHANGELOG.md`: the `## Unreleased` bullets from x106 become `## 0.15.3`, under an intro, an iOS and a Reports
  heading. Evidence and Known limits are carried forward from 0.15.2 and changed to what is true for this release:
  - Evidence: the private CI farm's 0.15.2 video run that showed the overlap refusals, the x106 witnesses and reviews,
    a real SSH-worker run of the overlap against 0.15.2, the x105 measurement, CI, and what was not exercised.
  - Known limits: the limit that the supervisor's warning repeats per reply is removed (x106 makes that line
    debug-only and reports a failed recorder once). A limit is added for tests that end before the previous
    recording has ended: they have no video, and with `ON_ANY` each logs one not-delivered error, as in 0.15.2.
- `README.md`: 0.15.3 is the current release, with one clause on what it changes.
- `docs/deviations.md`: the intro says what 0.15.3 changes for D98; the x106 note carries the version.
- `docs/migration.md`: the recorder-failure warning moved from the worker supervisor (now debug-only) to the iOS
  recording code; log alerts that matched the old line should match the new warning.

## Real-host evidence

A real SSH-worker check of main `5db150ed` against 0.15.2, on throwaway simulators (Xcode 27.2): the overlap
scenario (five instant tests, then a failing one, video on) gave 7 refusals in 4 runs on 0.15.2 and none in 4 runs on
`5db150ed`; two healthy video runs, D98's busy recovery with `debug: false` and an early stop passed. The overlap
could not be run under Xcode 26.6 (the test runner was built with Xcode 27.2). No bug was found.

## Incidental bugs

None.
