# Plan review, round 3: Sol (gpt-6.1-sol, high), on revision 3

**Verdict:** not acceptable yet, but **no blocking issue left**.

## Round-2 items

| # | Status | Note |
|---|---|---|
| 1 | Partial | macOS host state (TCC) is not restored on sequential reuse |
| 2 | Resolved | |
| 3 | Resolved | |
| 4 | Resolved | |
| 5 | Partial | WP 1.6 still said "level-at-creation" |
| 6 | Partial | Apple dependency edges missing |

## Round-1 items that were partial in round 2

| # | Status | Note |
|---|---|---|
| 1 | Partial | macOS TCC |
| 4 | Resolved | |
| 5 | Resolved | |
| 9 | Resolved | |
| 14 | Partial | Wording in WP 1.6 |
| 21 | Resolved | |
| 23 | Partial | Missing 4.8 → 4.10b (`XCTestParser.kt:28` needs the installer); 4.7 and 4.9 need `RemoteFileManager` from 4.10a (`TestResultsListener.kt:33`) |

## New major issues

1. **Inventory.** The script skipped 15 of the 505 main files, including the xcresult codegen in `buildSrc`. The `Xcresulttool` wrapper was assigned to 4.6 instead of 4.4.
2. **Sequential macOS isolation.** `MacosDevice.kt:514` writes the system and user TCC databases, which live outside `/tmp/marathon`.
