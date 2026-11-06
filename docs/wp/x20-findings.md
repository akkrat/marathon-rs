# X20: partial lists in remote parsing (D42) and failure text per instrumentation (U151)

The project owner approved both on 2026-11-07. D42 is a new deviation in PLAN §3.11. U151 was our own bug, from D28, so it
needs none. No configuration field, CLI option or exit code changed, and no command sent to a worker changed on a healthy run.

## D42: what a failing session code means for the list

### What Kotlin does

`AmInstrumentTestParser.kt:177-187` looks at a `TestRunFailed` only for `Process crashed`. Anything else falls through, and
`tests` is returned. The transformer reaches that with any `INSTRUMENTATION_CODE` other than -1: 0 usually, but also 1 or
an unreadable value. So a session can stop after one test of twenty, and the run goes on with one test and passes.

### The trap: sessions that end this way on a whole list

Treating every such ending as a failure would break runs that work, since a retry would only repeat it. So the endings came
first. Every recording with code 0:

| Recording | Tests | Message | Reaches the new check? |
|---|---|---|---|
| adam `log_6` | 9 of 9 | none: `Unexpected INSTRUMENTATION_CODE: 0` | Yes. **A whole list with code 0, from a real device.** It stands |
| adam `log_8`, `log_13`, fake `crash.txt` | any | `Process crashed` | `log_8` and `crash.txt` take the fallback. `log_13` has no line break, see below |
| `remote-scenarios` probe | none | `Could not find extra class` in a `TestRunFailing` | No: the fallback is taken first |

No recording has such a session in a remote parse, and no recording has a code other than -1 or 0. Then real sessions on a
real emulator, API 36, by real `am instrument -w -r -e log true`:

| Runner and ending | Listed | `am` printed | Now |
|---|---|---|---|
| Fixture AndroidJUnitRunner APK, plain | 42 of 42 | code -1 | 42 tests, 1 attempt |
| Same, a listener throwing in `testRunFinished` | 42 of 42 | `FAILURES!!!`, code -1 | 42 tests, 1 attempt |
| Same, a listener throwing in `testFinished` | 42 of 42 | `FAILURES!!!`, code -1 | 42 tests, 1 attempt |
| Same, a listener throwing in `testRunStarted` | 42 of 42, and one more atom | `FAILURES!!!`, code -1 | 43 tests, 1 attempt, as before |
| Same, a listener calling `System.exit` after the last test | 42 of 42 | `Process crashed.`, code 0 | Fallback, unchanged |
| Same, a listener calling `System.exit` after the first test | 1 of 42 | `Process crashed.`, code 0 | Fallback, unchanged |
| Same, no such listener class | none | `Process crashed.`, code 0 | Fallback, unchanged |
| Bare runner, uncaught exception, `System.exit`, `killProcess` | any | `Process crashed.`, code 0 | Fallback, unchanged |
| Bare runner, its own `finish(0, shortMsg=Other)` or `finish(0, null)`, every test listed | 3 of 3 | that text, code 0 | 3 tests, 1 attempt |
| Bare runner, its own `finish(0, …)` or `finish(1, null)`, one test listed | 1 of 3 | that text and code | **Failed attempt** |
| Bare runner, its own `finish(-1, null)`, one test listed | 1 of 3 | code -1 | 1 test, 1 attempt, as before |
| Bare runner, its own `finish(0, shortMsg=Other)`, nothing listed | none | that text, code 0 | **Failed attempt** |

So on this API level a process that dies always says `Process crashed.`, and a test annotation producer that fails after
listing everything never reaches D42: a throw is code -1 and an exit is the fallback. A message that isn't `Process crashed`,
or a bare code 0, comes from a runner's own `finish`. `numtests` agreed with the list in every session.

### The decision

A session fails the attempt if it reached a failing code and did not list what it announced:

| Session | Before | Now |
|---|---|---|
| Code -1, whole or short | Keeps the list | Keeps the list. Code -1 is success |
| Failing code, every announced test listed | Keeps the list | Keeps the list. A retry would only repeat it |
| Failing code, fewer than announced | Keeps the partial list | **Failed attempt** |
| Failing code, nothing announced, or `numtests=0` | Keeps what it listed, usually nothing | **Failed attempt**: nothing shows that the list is whole |
| Text with `Process crashed`, whole or short | Fallback, as in Kotlin | Fallback, as in Kotlin |
| `Could not find extra class` | Fallback | Fallback |
| No code: cut stream, `am` refused, never the protocol | D25 | D25 |

The brief asks for a failed attempt on code 0 with another message. **The allowance for a whole list is narrower than Kotlin
and wider than the brief's sentence, on purpose.** The brief's own trap asks what ends this way on a complete listing, and
`log_6` and the real run show it exists. Nothing is lost: a list that reached its count is not partial.

The count is `TestRunStartedEvent`, from `numtests` in the first status atom, against `TestEnded` events, counting a test
reported twice. `numtests` counts distinct tests, so a re-report can hide a missing one: a runner announcing 3 and sending A, A, A with a failing code passes as whole, and a class-level `test=null` atom inflates the count the same way. Counting distinct tests instead could fail duplicate-named Cucumber outlines, so this stays.

**The failing code's other route.** A last line with no line break, the shape of `log_13`, isn't an atom, so the transformer
never reads its code. If the list is short it answers `Test run failed to complete`, and that used to keep the list too.
With a count, and past D25's check, that can only be this case, as `am` refusing to start prints no count, so it fails the attempt, whichever code the line holds. A
test found this. `am` ends its lines, so only a stream cut in the last byte can look like this, and then a retry is right.
Adam's `log_13` itself has no count and no `TestRunFailed`, and stays as it was.

### The retry path and a failure that repeats

The existing path, unchanged: three attempts per bundle on the device, three times around `borrow`, so up to nine, and no
delay. The run then fails, with exit 1, by the existing route:

```
com.malinskiy.marathon.exceptions.TestParsingException: Unable to parse test list using <serial>: Other, tests listed: 1 of 3
```

A `TestRunFailing` in the same session adds its text, for example `Unexpected INSTRUMENTATION_CODE: 0: Bad argument, tests
listed: 0`. There is no underlying exception to carry as a `cause`: the session's text is the reason, and the message holds it.

**Limits, from review, nothing reproduced.** `extract_remotely` (`marathon.rs:535-575`) puts all bundles and all 9 attempts inside one `deviceInitializationTimeoutMillis` scope, and the outer retry restarts at bundle 1. If the scope expires first, the result is `NoDevicesException` with no D42 text, as with D25. A runner whose `numtests` disagrees with what it lists fails every attempt if its code isn't -1. One bundle with a failing code and nothing announced now fails a multi-bundle parse that the others would have carried; an APK with no tests probably ends with code -1, which isn't affected, though no device confirmed that.

**D22.** No outage wait and no `host:version` probe follow such a failure. The session reached its code through that server,
so it says nothing against it, and it would otherwise add a request to the transcript. `output_went_silent` became
`server_is_not_in_question`, for the same reason it had: both failures have no cause.

**The batch runner is unchanged:** `crates/marathon-android/src/runner.rs` has no D42 code. A failing code there keeps its
tests, a text for those it lacks and one `am instrument` (`a_batch_of_one_instrumentation_keeps_its_texts_u151`).

### Comparators

None changed. No JVM recording has this session in a remote parse. `make_registry.py --check` and `test_evidence.py` pass.

## U151: the text by test

In a batch across instrumentations the accumulator held one failure, plus `earlier_run_failure`, and used it for every test
with no record. A test could show the reason of an instrumentation it was never part of, and the latest failure won.

The runner now tells the listener each instrumentation's tests (`on_instrumentation_tests_in`), before it clears data or
reaches `beforeTestRun`, and only when the batch has more than one. `TestRunResultsAccumulator::start_instrumentation` closes
the previous one with its failure. The text of a test with no record is then:

| The test belongs to | Its text |
|---|---|
| An instrumentation that is over | That one's failure, or the generic text if it had none |
| The current one | The current failure, or the generic text |
| One that never ran, because the current one ended the batch, or none (an ignored test) | The current failure, the only reason there is |

This removes "the latest failure wins" from x12's findings, and x18's cut-stream text leaking to another instrumentation's
test, since that uses the same field. It also ends a limit x12 accepted: a first instrumentation that times out before
`beforeTestRun` lost its text to the next one's start, and now keeps it, as nothing depends on reaching that callback.

**Only the text changes.** The states, the retries and every `am instrument` are the same in every row of the test and in the
real runs below.

**A batch of one instrumentation, and other vendors, are byte-identical.** The callback is never made for them
(`only_a_batch_of_several_instrumentations_tells_the_listener_their_tests_u151`), so both new fields stay empty, and the text
is `run_failure_message` or the generic text, which is what it was with `earlier_run_failure` unset. Apple never called
any of this. `a_batch_of_one_instrumentation_keeps_its_texts_u151` covers each kind of end and **passes on the source from
before the fix**, run with only that source restored.

## Proof by temporary revert

Each revert was applied to the code, the tests run, and the source restored.

| Reverted | Tests that fail |
|---|---|
| U151: the three source files back to `41109474` | `a_failure_is_the_text_of_its_own_instrumentations_tests_only_u151`, `an_instrumentation_that_never_reached_before_test_run_keeps_its_text_u151`, `only_a_batch_of_several_…_u151`. `a_batch_of_one_instrumentation_keeps_its_texts_u151` passes, as it must |
| D42: no failure from a short list | `a_session_that_fails_short_of_its_count_is_retried_and_the_list_is_whole_d42`, `a_session_that_keeps_failing_ends_in_a_parsing_error_with_its_reason_d42`, `a_failing_code_with_no_count_fails_the_attempt_d42`, `a_run_does_not_take_a_parse_that_failed_short_for_the_list_d42`, `a_run_ends_on_a_parse_that_keeps_failing_short_d42` |
| D42: no allowance for a whole list | `a_failing_code_on_a_whole_list_keeps_the_list_d42` |
| D42: the last line with no line break | `a_session_that_fails_short_of_its_count_is_retried_and_the_list_is_whole_d42` |
| D42: no count, or a count of 0, taken as whole | `a_failing_code_with_no_count_fails_the_attempt_d42` |
| D42: a probe of the server after such a failure | `a_session_that_keeps_failing_ends_in_a_parsing_error_with_its_reason_d42` |

Tests that pin what stays: `code_minus_one_stays_success_on_a_short_list_d42`, `process_crashed_stays_the_fallback_path_d42`,
`without_instrumentations_a_new_run_clears_the_failure_u151`, and `a_lost_device_ends_the_batch_with_one_result_d28`, which now
also pins the text of the instrumentation that never ran. The `session code 0` row left `ordinary_ends_are_not_retried_d25`,
which pinned the old behaviour.

## Real emulator checks

One `google_apis` x86_64 emulator, API 36, on a private adb server. The native binary was built from this branch, and from
`41109474` for the "before" column. The test APKs are a bare `Instrumentation` of about 100 lines, built with the SDK's tools,
and the repository's fixture AndroidJUnitRunner APK, re-signed, with small listeners added.

**D28 with a failing instrumentation.** One batch, by `class-name` batching with one class name in two test APKs. The first
instrumentation has `oneFirstPasses`, `oneCrashes` and `oneAfterCrash`, the second `twoPasses`, `twoFails` and `twoNotReported`,
which it doesn't report. Two `am instrument`, each with its own tests. Exit 1, 2 passed and 4 failed, in every row.

| Test | Crash, or process killed | Own `finish(0, "Runner gave up")` | `am` killed, exit 137 |
|---|---|---|---|
| `oneFirstPasses` | Passed | Passed | Passed |
| `oneCrashes`, in progress | Empty text, as in Kotlin | Empty text | `Instrumentation failed: java.io.EOFException: … exit code 137` |
| `oneAfterCrash` | `Process crashed.` | `Runner gave up` | The same `Instrumentation failed: …` |
| `twoPasses` | Passed | Passed | Passed |
| `twoFails` | Its own assertion | Its own assertion | Its own assertion |
| `twoNotReported` | **Generic text.** Before: `Process crashed.` | **Generic text.** Before: `Runner gave up` | **Generic text** |

The states were the same before the fix. The `am` kill wasn't run on the old build. With `uncompletedTestRetryQuota: 2` the
crash took the retry path: six `am instrument` in all, each retried batch asking only for the unfinished tests of each
instrumentation, with the same final states and texts.

**D42.** `marathon parse` over two test APKs, and whole runs where stated.

| Session | Before | Now |
|---|---|---|
| Every test listed, `finish(0, null)` or `finish(0, "Other")` | Not run | Exit 0, 6 tests, 2 attempts (one per APK) |
| One of three listed, `finish(0, "Other")` | Exit 0, **2 tests of 6**. A whole run passed, running only those 2 | Exit 1, 9 attempts, 3.6 s: `Other, tests listed: 1 of 3` |
| One of three, `finish(0, null)` or `finish(1, null)` | Not run | Exit 1, 9 attempts: `Unexpected INSTRUMENTATION_CODE: 0, tests listed: 1 of 3`, or 1 |
| One of three, `finish(-1, null)` | Not run | Exit 0, 2 tests of 6: code -1 is success |
| Nothing listed, `finish(0, "Other")` | Exit 1, `NoTestCasesFoundException` | Exit 1, 9 attempts: `Other, tests listed: 0` |
| A crash, whole or short or nothing listed | Not run | Exit 1, 9 attempts, bare `TestAnnotationProducerNotFoundException`, unchanged: see below |
| A whole run, short for its first 2 s and whole after | Not run | One failed attempt, then `Parsed 6 tests`, and the batch had all 6 |

The same real captures were also fed through the parser on the fake server, with the same outcomes, and the AndroidJUnitRunner
ones, which marathon itself did not run, gave the "Now" column of the table above.

**Teardown:** the emulator, the private adb server and the lab directory are gone. Port 5037 was not touched and nothing listened on it, and another agent's emulator and adb server were left alone throughout.

## What was not verified

- **Other API levels.** The checks ran on API 36 only, the one image installed there. An older level that reports a crash by
  the exception's class in `shortMsg` would go by the table above: a whole list stands, and a short one is now retried with
  that text, where it used to be kept. Neither takes the fallback, as in Kotlin.
- **A runner that announces a wrong `numtests`.** One that announces too few and stops short passes as whole; one that announces
  too many and ends with a failing code now fails the run every time. Neither was seen: every real session agreed. The fake
  `android-reconnect` scripts do say `numtests=1` on each of twenty tests, which is why the two whole-run tests state a true count.
- **Whether a real device ever gives `log_6`'s ending today.** It's an older runner. No current one gave it, and the allowance
  costs nothing if none does.
- **D28 with an emulator killed outright.** WP x19's area; `a_lost_device_ends_the_batch_with_one_result_d28` covers the text on the fake.
- `scripts/check.sh`, `scripts/remote-check.sh` and the workspace test suite were not run, by the brief. The orchestrator runs the gates.

## Incidental bugs

No new bug in our own code. Seen nearby and left alone:

- **A parse that crashes every time ends in a bare exception.** `Process crashed` on all nine attempts gives
  `TestAnnotationProducerNotFoundException` with no message, and the attempt's logcat is logged only as a warning. It is
  Kotlin's behaviour, and D42 leaves that path alone by the brief, but it's the same "useful error" gap. With no listener
  argument to remove, the fallback also has nothing to change on its next attempts.
