# X18: an instrumentation stream that ends before its result

Real-emulator checks found a hole in D25 and D22 (`docs/wp/x12-findings.md`, "Real emulator checks"): a dropped adb
socket looks like a normal end of the stream, so a run could pass with tests missing. The project owner asked on
2026-11-06 for N38 to be fixed, so this completes that request. No new deviation number: D25 and D22 are extended. No
configuration field, CLI option or exit code changed.

## The cause, verified

As the checks reported. `FallibleCompatShellStream` runs the request's `close` on any end of the shell stream,
and a closed socket is such an end, over shell v1 and v2 alike. `transform` then says `No test results`,
`Test run failed to complete`, or `TestRunEnded`, and the producer has no failure to record. `remote_parser.rs` found
`take_failure() == None` and returned the tests listed so far. Kotlin behaves the same way (`AmInstrumentTestParser.kt:143-146`).

## The marker

`INSTRUMENTATION_CODE`. `am instrument -r` prints it last for every instrumentation that ran, as every capture below
shows, and only the transformer's session-result path reads it. `INSTRUMENTATION_RESULT` is not usable: some recordings
end with the code and no result line (the fake's `discovery.txt` and `execution.txt`).

**Recordings in this repository**, each by whether it reaches the code. The test is
`recordings_end_with_their_code_except_the_ones_cut_off_d25`.

| Recording | Code | Note |
|---|---|---|
| adam `log_2`, `log_3`, `log_5`–`log_12`, `testrunner-request-test.input` | yes | Passing, failing, ignored, metrics. Non-zero session code in `log_6` |
| adam `log_8`, `log_13`: `Process crashed` | yes, 0 | `log_13` and three others have no line break after the code: such a final line counts |
| adam `log_1`, `log_4` | **no** | Cut-off fixtures of adam's parser tests. `transform` is unchanged, so what adam's tests pin stays. Such a stream on a device now fails, which is the fix; every test `log_1` finished stays finished |
| Fake `execution.txt`, `discovery.txt`, `crash.txt`, `android-two-devices`, the `android-reconnect` scripts' inline outputs | yes | JVM CLI recordings run over these unchanged |
| Fake `started.txt` | **no** | Only ever served with `hang: true`, the stuck-output scenario: it never ends |

**Real devices.** No recording here held a refusal, so `am instrument -w -r` was run by hand on emulators, API 33 and 36,
over shell v2 and over v1 (`adb shell -x`). Every row matched on both levels and both protocols unless noted.

| End | Exit code on v2 | `INSTRUMENTATION_CODE` | Treated as |
|---|---|---|---|
| Passing test | 0 | -1 | Normal |
| Failing test | 0 | -1 | Normal |
| No such class, no such method | 0 | -1 | Normal |
| Listing with `-e log true` | 0 | -1 | Normal |
| Crash in a test, `Process crashed.` | 0 | 0 | Normal |
| Crash with tests left | 0 | 0 | Normal |
| Listener class missing, `Process crashed.` | 0 | 0 | Normal |
| App force-stopped during a test, `Process crashed.` | 0 | 0 | Normal |
| **Unknown instrumentation or runner** | 1 | **none** | **`am` refused: not a cut stream** |
| **Target app missing** | 1 | **none** | **`am` refused: not a cut stream** |
| `am` killed during a test | 137 | none | Cut stream |

So there is one real normal end without the marker, which is the trap the brief warned of. `am` prints
`onError: commandError=true message=INSTRUMENTATION_FAILED: …`, usually with a status atom holding an `Error`, **in either
order**, and on v1 with its stack trace mixed in. The transformer therefore looks for `INSTRUMENTATION_FAILED` in the buffer
before atoms consume it, or an `Error` atom with no test, and only counts them if no test has started. Otherwise a test
that prints that word could hide a cut stream. These two rows keep exactly what they gave: all tests uncompleted with
the generic text, or `No test results` with the line quoted, and for a remote parse 0 tests and `NoTestCasesFoundException`.

Output that never was the protocol also keeps `No test results\n<output>`, as adam's `testReturnsContentOnFailure` pins.
It carries its own reason, and with 0 tests it cannot be a false green.

**No output at all is a cut stream, whatever the shell reports.** The first version let an exit code with no output through
as an ordinary end. The real check showed why that is wrong: `am` killed before printing gave `NoTestCasesFoundException`
with no retry, though the next attempt would have worked. No captured normal end has empty stdout.

## Remote parsing

An attempt whose stream has no marker fails with `Unable to parse test list using <serial>: java.io.EOFException: the
instrumentation stream ended before a result`, through D25's path, and takes the existing retries. `extract_inner` adds
tests to its result only from an attempt that succeeded, so a partial list cannot leave it. A stream with a marker kept its
tests even if the session failed (`INSTRUMENTATION_CODE: 0` with another message), as in Kotlin: a normal end, not a broken
stream. D42 (WP x20) has since made a session that fails short of its count a failed attempt.

**Do attempts move to another device? Yes, by the outer retry**, which is how Kotlin works too, so nothing was redesigned.
There are two levels: `AmInstrumentTestParser.extract` retries three times per bundle on its one device (`withRetry(3, 0)`),
and `Marathon.kt:132-136` (`extract_remotely` in `marathon.rs`) retries three times around `borrow` and `extract`,
so up to nine attempts. `borrow` picks at random among ready devices. Once D24 has removed a device, only the others are left. The
checks saw "a healthy second emulator isn't tried" because a cut stream was no failure, so neither level retried. With the fix,
the real run below parsed 63 of 63 on the second emulator after 7.2 s, and passed. While the first device is still ready,
the outer retry can pick it again, as in Kotlin.

## Parsing during an outage (D22)

`AndroidDevice::wait_out_server_outage` (since D41 `wait_out_passing_loss`) runs after a failed attempt and says whether that failure was an outage it has now
waited out; such an attempt does not count.

- **Gate closed:** it waits at the gate batches use (`DeviceStateTracker::server_available`), with cancellation. The gate has the
  provider's bound: `waitForDevicesTimeoutMillis` for the server, the same again for its devices.
- **Gate open, device given up:** false at once, so the attempts run out. This is what ends a run whose server never returns.
- **Gate open, device live:** the tracker may not have heard yet, since it sees the same dead server at about the same moment.
  It asks the server for its version: answered means no outage, and the attempt counts, as a fast third-party restart should.
  Refused means it looks again every 100 ms of vendor-clock time, for at most `waitForDevicesTimeoutMillis`.
- **Output gone silent:** no probe. A lost server ends the stream at once, and this keeps the transcript of a silent device
  as the JVM's. Probes otherwise appear only on failed attempts, which no recorded scenario has apart from that one.

A flapping server could keep an attempt uncounted for longer, but the whole parse stays under
`deviceInitializationTimeoutMillis` (`extract_remotely`), and cancellation ends the wait.

**A server that never returns at this stage** fails the run with `NoDevicesException: Timed out waiting for a temporary device
for remote test parsing` when `deviceInitializationTimeoutMillis` passes (180 s by default), not after 30 s. Its device is
disconnected after 30 s, its attempts run out, and `borrow` then waits for a device that does not come, as it does in
Kotlin when no device is there. That is the documented failure for no device at parse time, now stated in D22's text.

## The batch runner

| End | Before | Now |
|---|---|---|
| Socket dropped, no test yet | `No test results` | `Instrumentation failed: java.io.EOFException: the instrumentation stream ended before a result` |
| Socket dropped during a test | `Test run failed to complete…` for tests not started; **empty for the test in progress** | That text for all the unfinished ones |
| Socket dropped after the last test, before the code | `TestRunEnded`, a success | Run failure in the log; the finished tests stay finished, so nothing to retry |
| `am` killed | As a dropped socket | The same text, `…, with exit code 137` |
| Crash, non-zero session code | Unchanged: `Process crashed.`, and no text for the test in progress, as in Kotlin | Unchanged |
| `am` refused | Unchanged | Unchanged |
| Provider shutdown (N19), cancellation | Quiet | Quiet: `take_failure` returns nothing on a closed client |

**The test in progress.** Its report was `<failure></failure>` because the accumulator gives the run's text only to tests with
no record. D25's own promise, "their reports carry the real reason", did not hold for it either. `on_stream_failed_in`
is a new internal listener callback, in the manner of D28's `before_next_instrumentation_in`, for producer failures only.
So it also applies to the earlier D25 texts. Other run failures leave that test without text, which the crash test pins.

**Device.** `java.io.EOFException` is not in `means_device_loss`, so a healthy device stays. A device that is really gone is
reported by `host:track-devices`, as today.

**D28.** The text applies from the current instrumentation's first test on (`tests_of_earlier_runs`, set by `start_instrumentation` since WP x20), so
a crash in an earlier instrumentation keeps its empty text. The next instrumentation still runs, and tests finished before the cut
stay finished. A batch of one instrumentation that reaches its code has the same events as before, byte for byte: nothing in this
path runs unless the stream lacked the code.

## Comparators

None changed: `tools/differential/make_registry.py --check` and `test_evidence.py` pass. No JVM recording's stream lacks the code,
apart from the hanging one, and the `scenarios_test.rs` and CLI differential comparisons are unchanged. The JVM rows
`runner:handshake-eof-events` and `runner:closed-client-events` stay as witnesses.

## Proof by temporary revert

Each revert below was applied to the committed code, the named tests were run, and the source was restored.

| Reverted | Tests that fail |
|---|---|
| The check for the code | `a_run_does_not_take_a_cut_off_parse_for_the_list_d25` (the false green), `a_stream_cut_off_is_retried_and_the_list_is_whole_d25`, `a_stream_cut_off_on_every_attempt_fails_and_gives_no_list_d25`, `am_killed_before_any_output_is_retried_d25`, `a_killed_am_fails_the_attempt_with_its_exit_code_d25`, `a_kill_during_a_test_tells_the_listener_why_and_keeps_the_device_d25`, and four `a_stream_cut_off_*` of the batch runner |
| The outage wait in parsing | `remote_parsing_waits_out_an_outage_and_it_uses_up_no_attempt_d22` |
| The wait's bounds (device given up, time limit) | `remote_parsing_fails_when_the_server_never_returns_d22`, on its 60 s hang guard |
| No probe for silent output | `timeout_has_three_inner_attempts_and_unregisters_every_attempt` |
| The text for the test in progress | `a_stream_cut_off_explains_the_unfinished_tests_and_keeps_the_finished_d25`, `a_stream_cut_off_leaves_the_other_instrumentations_results_right_d25_d28` |
| The limit to the current instrumentation | `a_stream_cut_off_does_not_explain_an_earlier_runs_crash_d25_d28` |
| The exception for a refusal | `ordinary_ends_are_not_called_a_stream_cut_off_d25`, `ordinary_ends_are_not_retried_d25` |
| The exception for stray output | The same two |
| Quiet on provider shutdown | `a_stream_ended_by_provider_shutdown_stays_quiet_d25` |

Fake-adb notes. Over shell v1 a command's end and a dropped socket look alike, which is how most tests give a dropped socket with
the server alive. Killed-server tests use `FakeAdbServer::kill` with `hang: true` and hold the `PORTS` lock exclusively. No new
fake-server control was needed. The three-kill test proves outages cost no attempt: four attempts run, three of them cut, where
three failures would otherwise end the parse.

## Real emulator re-check

The results are in `docs/wp/x12-findings.md`, "Re-check after the fix". In short: no run exited 0 with tests missing, and every
server back within the window gave a correct run.

## What was not verified

- **Devices below API 33**, and any real device whose `am` lacks shell v2. Shell v1 was exercised with `adb shell -x` on API 33
  and 36, for the captures only, and with the fake for the behaviour. On v1, `am`'s stack trace mixes with its output, and I did not
  feed those exact captures through the parser. A refusal's text before the first atom could make the parser refuse it, which then goes
  D25's way (`Instrumentation failed: …`): retried, and a wrong text, on devices that old.
- **A run that selects no test at all** was not captured: the attempt meant to, with an unknown annotation, listed all 63. It would
  count as cut off only if it printed nothing, and no capture does.
- **`INSTRUMENTATION_ABORTED: System has crashed`** was not seen. It has no code, so it counts as a cut stream, and the text is
  true of it.
- **adbd killed during parsing, and a physical device.** The route is the emulator's, by the x12 table.
- **Whole-run JVM comparison** of these faults: Kotlin's behaviour here is N38, the bug being fixed.
- `scripts/check.sh`, `scripts/remote-check.sh` and the workspace test suite were not run, by the brief. The orchestrator runs the gates.

## Review

A second-model review of `7546d8b8` found no blocker and ran its own prefix sweep over 16 recordings, and every split of them, with no complete stream called cut.

- **Fixed:** a stream cut inside the protocol's first word, or after a lone line break, was still read as output that never was the protocol. Remote parsing then gave an empty list with no retry, and with several bundles a run could go green without that bundle's tests. Now blank output, or an unfinished last line that could still become the protocol's first word, counts as cut. A first version required text to end in a whole line; the gate refused it, since adam's `test_returns_content_on_failure` pins `something-something` with no line break as ordinary output.
- **Left, as a limit:** other text followed by a cut can't be told from "never the protocol" on a v1 shell. A v2 stream with no exit packet could be told apart, but no such output has been seen and the first write normally arrives whole.
- **Left, Kotlin's behaviour, for the owner:** a session that ends with code 0 and a message other than `Process crashed` kept its partial list (`ordinary_ends_are_not_retried_d25`). It isn't a broken stream, so it was out of this WP's scope, and it was the remaining way a parse could come back short. The owner approved fixing it as D42, by WP x20.
- **Noted:** `remote_parsing_waits_out_an_outage_and_it_uses_up_no_attempt_d22` needs the parser to reach the gate before the tracker has closed and reopened it, a window of the reconnect backoff plus the list settling. A stall longer than that would fail the test. Not seen. U151 could also carry the cut-stream text to a later instrumentation's unreported test; that was text only, and WP x20 fixed it.

## Incidental bugs

No new bug in our own code beyond U152, which is what this WP fixes. Seen nearby and left alone:

- **A remote parse attempt on a device already gone takes 2 s** (`DeviceSetupException: Unable to verify that package … is installed`),
  from the installer's own retries, so the move to a healthy device took 6 s. Harmless; from Kotlin's retry layout.
- **A crash leaves the test in progress with an empty `<failure>`**, as in Kotlin: seen again in the crash control. Only streams
  that failed get a text, by the brief.
- **`adam` reads the shell's exit byte as signed** (`v2.rs`, 137 as -119). Kept there; the request masks it for its own use.
- Recorded for the owner and not changed, by the brief: the brief `offline` of a last device ending a run; reverse-forward recovery;
  U151, since fixed by WP x20.
