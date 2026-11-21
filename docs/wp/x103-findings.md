# WP x103 findings: release documentation for 0.15.1

0.15.1 releases `main` after 0.15.0 with D98 (x99, x101, x102) and the test-only x100.

## Changes

- `CHANGELOG.md`: `## Unreleased` became `## 0.15.1`. Its intro names the worker-command changes (no `recordVideo`
  while a service is busy; up to one end-of-run `simctl shutdown` with its state reads); a "Check before you upgrade"
  item covers SSH command allowlists; D98 has two entries under iOS (the early INT, the busy state). Evidence summarizes the x99–x102 findings
  and the real runs: the real-simulator checks on this project's Mac recorded in the x99 findings, and two SSH-worker
  acceptance runs on CI builds of earlier `main` commits (with D98, and then with x101). The 0.15.0 known limits are
  carried forward; its iOS recorder limit is replaced by the limits D98 leaves (the legacy recorder without Perl, a
  busy simulator marathon could not shut down, the English-only busy text, a signal during the end-of-run shutdown,
  a replacement simulator whose registration failed).
- `README.md`: 0.15.1 is the current release.
- `docs/deviations.md`: the intro says D98 ships in 0.15.1.
- `docs/migration.md`: D98 "is new in 0.15.1"; the D98 note applies when moving from 0.15.0 to 0.15.1.

## Review

Sol, round 1 (all taken):

| # | Finding | Resolution |
|---|---|---|
| 1 | The contract statement named only the shutdown, not the suppressed `recordVideo` commands and the state reads | Intro and this summary name both |
| 2 | Busy-state and shutdown promises too broad: the state clears on a confirmed shutdown or a boot from Shutdown; the shutdown has eligibility conditions; detection needs status 16, the busy text and no other live recorder | The iOS entry is split in two with those conditions; the migration note and the restart limit list the cases |
| 3 | "No longer wedges" overstated the start gate: TERM or KILL before confirmation can still wedge; a missing start line costs the cutoff | Heading narrowed to the early INT; the cutoff and the missing line are in the entry and Known limits |
| 4 | The real-simulator evidence mixed the supervisor's INT request times with the local stop times; only one remote SIGTERM run exercised the hold | Stated separately with their numbers; the held INT is attributed to one run |
| 5 | The test evidence overstated failure checks: some witnesses are controls; x100 reproduced the race with an injected delay | Reworded |
| 6 | "notes below" and "that evidence" lose their referents in the release body | Linked 0.15.0 by URL; the macOS UI limit names whose evidence it is |

Sol, round 2 (all taken):

| # | Finding | Resolution |
|---|---|---|
| 1 | The intro still read as permanent `recordVideo` suppression | "while marathon tracks the service as busy" |
| 2 | The shutdown is one bounded attempt that can fail; it is skipped when the on-dispose lifecycle shuts down or erases, even if that fails; restart only if still busy | iOS entry, Known limits and migration note say so |
| 3 | Start-gate wording: name SIGINT; TERM only for a recorder still running, KILL only if it survives TERM; the missing-marker limit applies to a recorder still running | Reworded |
| 4 | The "only by repeated runs" parenthetical read as x100's | It names the x99 fix |
| 5 | "No macOS code changed" too broad: the SSH supervisor is shared | "No Android- or macOS-specific code changed", with the shared supervisor named |

## Incidental bugs

None.
