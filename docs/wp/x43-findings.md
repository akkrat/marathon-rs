# x43 — ledger rows and approval dates

Documentation only. It closes two items that the x40 docs pass left open.

## Rows for x26's incidental findings

WP x26's findings listed four incidental bugs. One already had a row (U173). The other three now have rows:

| Row | What |
|---|---|
| [A103](../known-kotlin-bugs.md) | Remote parsing borrows a simulator at random for each of its three attempts, so a stuck worker can fail the run while a second worker is healthy. Kotlin does the same, so the row is in the Kotlin ledger, kept |
| [U177](../found-bugs.md) | No test reads `fixtures/oracle/connections/deadlines.jsonl` |
| [U178](../found-bugs.md) | Password runs of the fake worker's `ssh` shim still offer the developer's default keys |

[x26-findings.md](x26-findings.md) now points to each row.

## Approval dates for D14–D20

These sections in [deviations.md](../deviations.md) had no "Approved" sentence; the dates were only in PLAN §3.11.
Each now has one, with the date from PLAN: D14 and D15 2026-10-25, D16 2026-10-26, D17 2026-10-31, D18 2026-11-02,
D19 2026-11-03 and D20 2026-11-04. D14–D17 came from their WPs' proposals, and D18–D20 were the project owner's
decisions. D1–D13 were left as they are; PLAN §3.11 dates them, mostly with the section's 2026-10-12 and D6, D8 and D13 later.

## Review round 1

gpt-6.1-sol: merge after fixes.

- S-x43-1 (minor): this file said every D1–D13 entry is covered by PLAN §3.11's 2026-10-12, but PLAN dates D6 and D8
  2026-10-23 and D13 2026-10-24. The sentence now says PLAN dates them.
- S-x43-2 (nit): A103 cited the parsers' own `withRetry(3, 0)`, which retries extraction on the device it was given.
  The borrow happens in the outer loop of `Marathon.kt:131-143`, which the row now cites.
