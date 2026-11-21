# WP x103 findings: release documentation for 0.15.1

0.15.1 releases `main` after 0.15.0 with D98 (x99, x101, x102) and the test-only x100.

## Changes

- `CHANGELOG.md`: `## Unreleased` became `## 0.15.1`. Its intro names the one new worker command; a "Check before you
  upgrade" item covers SSH command allowlists; the D98 entry sits under iOS. Evidence summarizes the x99–x102 findings
  and the real runs: the real-simulator checks on this project's Mac recorded in the x99 findings, and two SSH-worker
  acceptance runs on CI builds of earlier `main` commits (with D98, and then with x101). The 0.15.0 known limits are
  carried forward; its iOS recorder limit is replaced by the limits D98 leaves (the legacy recorder without Perl, a
  busy simulator marathon could not shut down, the English-only busy text, a signal during the end-of-run shutdown,
  a replacement simulator whose registration failed).
- `README.md`: 0.15.1 is the current release.
- `docs/deviations.md`: the intro says D98 ships in 0.15.1.
- `docs/migration.md`: D98 "is new in 0.15.1"; the D98 note applies when moving from 0.15.0 to 0.15.1.

## Incidental bugs

None.
