# x28 — hung Apple tests, and silent batches

## A47 / D45

### What real output shows

A real `xcodebuild test` ran on a throwaway iOS 18.6 simulator under Xcode 27.2 (27B52), created for the run and deleted after
it. It had `-test-timeouts-enabled YES`, tests with `executionTimeAllowance` of 60 and 90, a passing test and a nested class.
It exited 65. The stream is `fixtures/apple-logparser/xcode27-ios-hang.log.input`.

| Allowance set | Line prints | Kotlin | Now |
|---|---|---|---|
| 60 | `1 minute` | throws, batch returned uncompleted | failed, 60 s |
| 90 | `2 minutes` (XCTest rounds up) | failed, 120 s, no trace | failed, 120 s, the hang's line as trace |

- The hang's line is the only event for that test. No `failed` line follows it, and the runner restarts
  (`Restarting after unexpected exit, crash, or test timeout`). So the parser's event is the whole of the failure.
- xcresult has the same two failures, each with `Test exceeded execution time allowance of 1 minute` or `2 minutes`.
- The format string is in XCTestCore: `Test Case '%@' exceeded execution time allowance of %@. The test may have hung; …`. The
  `%g` sibling (`…however enforcement is disabled…`) doesn't match the parser's pattern, as in Kotlin.

### Forms

Rendered with `DateComponentsFormatter`, full style, on this Mac:

| Seconds | Text |
|---|---|
| 1.5 | `1 second` |
| 30 | `30 seconds` |
| 60 | `1 minute` |
| 61 | `1 minute, 1 second` |
| 90 | `1 minute, 30 seconds` |
| 120 | `2 minutes` |
| 3600 | `1 hour` |
| 3660 | `1 hour, 1 minute` |
| 3661 | `1 hour, 1 minute, 1 second` |
| 86400 | `1 day` |
| 90000 | `1 day, 1 hour` |
| 604800 | `1 week` |

That XCTest uses this formatter is an inference from these forms and the format string's `%@`. Only `1 minute` and `2 minutes`
are recorded from XCTest itself; a one-hour hang was not worth waiting for. Any text outside these forms counts as 0 s with a
warning, so the inference can't bring the abort back. That covers other languages and months or years.

### Decisions

- The Kotlin reading runs first, so everything it could read is unchanged, including odd forms such as `1H` and `-5 seconds`.
- The trace falls back to the hang's line on the plural forms too. Otherwise the same hang would read differently at 60 s and
  120 s. The copied `timeout_0` and `timeout_1` expectations stay unedited; the test states the difference.
- An earlier failing line in the same test keeps its place as the trace, as in Kotlin.
- A malformed result line, `failed (1.2.3 seconds)`, still escapes as `NumberFormatException`. Xcode's `%.3f` can't produce it,
  so it is outside this fix; see "Incidental bugs".
- `ParseError::Duration` and `DurationOverflow` are gone, since nothing returns them.

### Does A5's nested-class form reach the same endless retry?

No, not on the forms that were checked.

| Path | Checked how | Result |
|---|---|---|
| Log line of a nested Swift class | Real run | `-[_TtCC12TimeoutTests12TimeoutTests6Nested testNested]`: mangled, no dot, so A5's split doesn't apply. It reports as `pkg=<target>`, like any class without a dot |
| `nm` discovery, the default | Marathon's pipeline on a compiled nested class, then the code of `parse_symbols` | `Mod.Outer.Nested.testNested()` has four parts and is dropped, so the test is never scheduled and can't retry |
| `xctest` discovery | Code reading only | The batch holds the mangled class with `pkg` set to the bundle id, so it matches when the target name equals the bundle id, as for any class |
| A real three-part class name in a log line | Code reading only | With `xctest` discovery the logged `pkg` wouldn't match the batch, the test would count as missing, and it would come back uncompleted until the quota ends. No real output with such a name was seen |

## U165 / D39 extended

### What Kotlin does, and the choice

Kotlin's `AppleSimulatorDevice` handles `TimeoutException` with `onBatchCompleted`, which resets the count of batch failures. A
real failure text reached `onBatchFailed` instead, so Kotlin recycled a simulator for the text and never for the silence. D39
stopped matching some of those texts, which left the silence as the only sign.

Two rules were possible:

| | Only after an exempted literal | Any timeout with no progress (chosen) |
|---|---|---|
| Change | The classifier reports what it exempted, and that state crosses the producer and session to `classify` | One policy method |
| Catches a miss that isn't an exempted form, such as CRLF output | No | Yes |
| Differs from Kotlin on a silent hang with no text | No | Yes: recycled after 3 |

The cost of the chosen rule is the last row. It was accepted because a batch that shows nothing for 300 s three times in a row
is the same evidence `maxBatchesWithoutProgress` acts on, and because the alternative for a test that hangs every time was to
repeat it on the same simulator with no end.

A batch with progress is unchanged: it resets the count, as Kotlin does. `TimeoutException` reaches this branch only from the
reader's idle timeout. The session-close timeout arrives as a device loss, and the batch timeout ends the batch without an error.

### The scenario

The audit's logs aren't in this repository, so its 50 batches are rebuilt here: every batch goes silent, with the default 3
batch failures and the default 3 recoveries, through the real device, policy and provider recovery on a fake worker. The test
sets a short recovery pause, because the default's 60 s meets the harness's watchdog.

| | Batches before the simulator is lost (another device keeps the pool) | Recycled | Time at 300 s per batch |
|---|---|---|---|
| Before (fix reverted) | never, all 50 ran on one handle | 0 | 250 minutes and counting |
| After | 3, 3, 3, 3, then out of the run | 3 | 60 minutes, plus setups and 90 s of pauses |

Reverting the fix makes `fifty_silent_batches_recycle_the_simulator_until_its_recoveries_run_out_d39` and
`silent_batches_take_the_simulator_out_with_the_default_settings_d39` fail. `silent_batches_with_progress_keep_the_simulator_d39`
passes both ways, as it pins what must not change.

### What it does not do

- It adds no limit on a batch with progress that then goes silent. Such a batch still resets the count.
- The scenario above has no scheduler, so it is what a simulator gets while another device keeps the pool alive. On a pool's
  only simulator each recycle empties the pool. Before D48 the pool then ended at once, after 3 batches (S-x28-1, a review of
  this WP after merge). Since D48 the pool waits up to `deviceInitializationTimeoutMillis` for the recovery: one that is ready
  within it gives the same 12 batches, and the pool ends that long after the fourth loss with its tests unrun; one that
  misses it ends the pool after 3, 6 or 9. `simulator_pool_recovery.rs::silent_batches_on_the_only_simulator_*` (WP x40) pin
  both through the real scheduler. No real simulator went silent for this; it is all on a fake worker.

## Verified

- `cargo test -p marathon-apple --test apple_logparser --test batch_failure_policy` and the whole of `marathon-ios`; see the WP report.
- The real run above, replayed through `XctestEventProducer` in `a_real_stream_with_hung_tests_reports_every_test_and_no_batch_failure_d45`.

## Not verified

- Real output with a singular `1 hour`, a compound allowance or a non-English locale.
- macOS on this Xcode: the parser is shared, and the 60 s form is already recorded from macOS under Xcode 27.1.
- A whole Marathon run against the hanging tests. The real part stops at xcodebuild's stream.

## Incidental bugs

- Nested Swift test classes are never discovered by the default `nm` parser, as `parse_symbols` needs exactly three parts. Such
  a test silently does not run. Kotlin does the same (`NmTestParser.kt:84`). Not fixed; ledger row
  [A102](../known-kotlin-bugs.md).
- `failed (1.2.3 seconds)` still aborts a batch with `NumberFormatException`, and it would repeat as A47 did. Real Xcode can't
  print it. An application could, by logging a whole line of exactly that form. Not fixed; row [U169](../found-bugs.md).
