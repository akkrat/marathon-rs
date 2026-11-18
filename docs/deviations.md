# Approved differences from Kotlin marathon

[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) is
canonical. This guide covers D1–D81 (D73 was reserved and not used); implementation availability
is qualified by the [README](../README.md). Each entry describes the Kotlin
behavior, native behavior and migration action. Pending proposals are not
shipped differences. D55–D67 ship in 0.13.0. D55–D65 remove JVM artefacts under
[the Phase 8 contract](../PLAN.md#312-phase-8-contract), including the behaviour
changes described below; D66 and D67 change signal and console handling. They are separate from 0.12.0's Kotlin bug
fixes. D55–D65 first shipped in 0.13.0-rc.1 and D66 in 0.13.0-rc.2.
PLAN §3.11 records no per-row approval dates for D55–D65. Its heading date,
2026-10-12, is the section's original approval date. D68–D81 ship in 0.14.0 (D68–D72 and D74–D77
first in 0.14.0-rc.1, D78–D81 in 0.14.0-rc.2); they fix run-stability and device-handling bugs, add a native-only
flakiness strategy (D72), and change JUnit retry elements, the iOS boot wait and video transfers, and Android coverage
without `run-as`. See the
[changelog](../CHANGELOG.md).

## D1 — Crash and usage telemetry

Kotlin enables Bugsnag crash reporting and Grafana Cloud usage metrics by
default. The native build accepts `--bugsnag`, `--analyticsTracking`,
`bugsnagReporting` and `analyticsTracking` but sends neither telemetry stream.
In the startup phase, after configuration loads, one INFO line says these
options are ignored, replacing Kotlin's reporter-start notice. Existing flags
can stay. This does **not** disable your configured InfluxDB or Graphite backend.
[CLI evidence](wp/2.5-findings.md).

## D2 — JVM launcher and Windows

Kotlin's launcher consumes `MARATHON_OPTS` and `JAVA_OPTS` and provides a Windows
start script. The native binary ignores those variables and has no Windows
build. Remove JVM tuning from your runner entry point; do not pass `-Xmx` as a
native CLI option. Use one of the two documented native targets.
[Packaging evidence](wp/7.3-findings.md).

## D3 — Leaked JSON field

Kotlin's Gson leaks nondeterministic `safeSerialNumber$delegate` values into
device, billing and per-test result JSON through the embedded device. Native
output omits that field everywhere; `isTimeInfoAvailable` stays. Adjust
consumers or comparisons that require the leaked field, including per-test
`test_result/**.json`, not just `devices/` and `bill/`.
[Reporter evidence](wp/3.9b-findings.md).

## D4 — Analytics clients

Kotlin uses Java Influx/Graphite clients; native HTTP/TCP clients replace them.
The database/retention-policy and organisation/bucket lifecycle, requests,
timeouts, retries, caching and setup-failure fallback retain the
[§3.8 contract](../PLAN.md#38-analytics-backends). Keep backend configuration;
client replacement does not itself disable writes or require a service migration.
[Backend evidence](wp/3.4b-findings.md).

## D5 — Retained defects

Most Kotlin defects remain, including metadata lookup, dynamic-provider EOF and
CI detection quirks. D14–D19 and D21–D54 fix the ones described in this guide;
D55–D65 remove JVM artefacts and also retire some retained quirks. The
[ledger](known-kotlin-bugs.md) marks each affected row. Consult the
ledger and its per-row status, and the
[migration shortlist](migration.md#retained-behavior-to-account-for).

## D6 — Regular expressions

Kotlin uses `java.util.regex`; native configuration uses Rust `regex` syntax,
Unicode classes and linear-time matching. `matches` is still a full match;
`find`, `split` and `replace` keep Kotlin semantics around the match. Java-only
constructs such as lookaround, backreferences and possessive quantifiers are
rejected at configuration load with the pattern and field identified.
Audit all regex fields, including test filters and `excludeSerialRegexes`.
A string pattern such as `^com\.example\..*Test$` uses supported syntax (in
YAML, use single quotes to preserve the backslashes). There is no general
Java-regex translator; review meaning as well as acceptance.
[Regex evidence](wp/1.4-findings.md).

## D7 — SSH after a non-TERM signal

Kotlin's sshj can send a stale TERM on an already-closed channel after another
signal; OpenSSH then drops the connection and unrelated simulator work.
Native execution omits that stale request. The command still has no exit code,
but other channels survive. Do not expect a whole-worker reconnect as a side
effect of this failure. [SSH evidence](research/ssh-spike-results.md).

## D8 — Locale-dependent formatting

Kotlin uses the machine's JVM locale for HTML footer month names, progress and
file-transfer percentages/decimals. Native output always uses en-US; it matches
Kotlin on en-US hosts. Already pinned formats such as JUnit and Graphite are
unaffected. Update locale-sensitive output comparisons.
[Formatting evidence](wp/1.2-findings.md).

## D9 — Malformed or exotic configuration

Kotlin accepts some malformed values or fails only when they are used. Native
configuration rejects out-of-range dates/durations, object-form regexes, invalid
UTF-8 keys/paths/regexes and most null collection elements at load. Duplicate
keys use the last value before nullability checks; quoted numbers and ordinals
accept ASCII digits only. Null filter values are dropped, including from dumps.
Malformed-input exception classes can differ.

Use unique keys, in-range values, string regexes and non-null collections.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[binding findings](wp/2.1-findings.md), [schema findings](wp/2.2-findings.md)
and [whole-config findings](wp/2.6-findings.md) for edge cases and waivers.

## D10 — Apple command text

Kotlin accepts JVM charsets and can corrupt characters split across reads.
Native command output uses UTF-8 with replacement for invalid text and preserves
characters across reads. Ensure command text is UTF-8. As in Kotlin, a timeout
does not always kill its process.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and [executor findings](wp/4.1-findings.md).

## D11 — ADB streamed text

Kotlin can corrupt UTF-8 characters and split logcat lines at read boundaries.
Native shell/logcat text preserves characters and partial lines across reads;
shell v1 bytes remain raw. No configuration change is needed; update comparisons
that expected corrupted characters or artificial line breaks.

Kotlin's adb transport also takes its buffers from a process-wide pool of 1,024
that waits without a limit once it is empty. Native requests use their own
buffers, so there is no pool to run out. Seen in the field: a JVM run on a CI
farm stalled with all threads waiting on the exhausted pool.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[ADB findings](wp/6.1-findings.md) and [logcat findings](wp/6.5-findings.md).

## D12 — Native CLI and process diagnostics

Kotlin emits an SLF4J provider line, JVM stack frames and run-specific identity
hashes. Native output omits that line and internal frames. Through 0.12.0 it kept
exception class names and stable identity suffixes; D59 now replaces generated
class names with descriptions, and D58 replaces identities with values. Messages,
`Caused by:` lines and external tool traces stay. `System Information:` lists OS
facts instead of JVM properties. Native signal handling retains captured JVM exit statuses.

Kotlin expands `@argfile`, provides `_MARATHON_COMPLETE` completion and wraps
help to the terminal. Through 0.12.0 native help used 79 columns; D61 now uses clap
text. There is still no argfile expansion or completion script. Pass arguments
directly, replace completion integration and update diagnostic parsers.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[CLI findings](wp/2.5-findings.md) and [run findings](wp/3.10-findings.md).

## D13 — Metrics query timing

Kotlin queries strategy history when first needed for comparison or batching.
Native queues fetch that history during queue construction. Query and timing-log
counts normally stay the same. Extra queries/timing lines can occur for empty
filtered shards, or SuccessRate sorting of a pool with fewer than two tests
when its limit differs from the flakiness limit (or flakiness is Ignore).
No configuration change is needed; allow these timing and transcript differences.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and [strategy findings](wp/3.2-findings.md).

## D14 — Android tasks and disposal

Kotlin uses `bootWaitingThreads` and `installThreads` to size thread pools.
Native builds accept these settings, including zero, but they no longer control
concurrency. User file-sync entries still include Allure and coverage files.
Kotlin can abort disposal on reverse-removal errors; native builds log those
errors and continue cleanup. Remove reliance on the thread-count settings or
on reverse cleanup failures aborting disposal.
Approved 2026-10-25, from WP 6.5.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and [provider findings](wp/6.5-findings.md).

## D15 — Android instrumentation cleanup

Kotlin can leave log collection and instrumentation running after remote-parsing
failure or a failing execution after hook. Native execution cleans them up on
every exit. No configuration change is needed; do not rely on leaked
instrumentation continuing after failure.
Approved 2026-10-25, from WP 6.6a.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and [runner findings](wp/6.6a-findings.md).

## D16 — Android artifact cleanup

Kotlin can race recording operations and leave profiling running after run
failure. Native execution coordinates recording and waits for artifact tasks
to stop during cleanup. On failure, perfetto stops but its trace stays on the
worker without being downloaded. Video/GIF cancellation adds no remote cleanup
commands; profiling setup failures still fail the batch. No configuration change
is needed; account for retained remote traces.
Approved 2026-10-26, from WP 6.6b.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and [artifact findings](wp/6.6b-findings.md).

## D17 — iOS recording stop

Kotlin reports the iOS recording stop as failed when the recorder has already
exited by the time `lsof` first looks, which is the usual case on current
Xcode. It keeps the pidfile, so the next test's recording waits 10 s and is
lost. Native builds check the recorder pid with
`sh -c "ps -p $(cat <pidfile>) -o pid="` after a failed stop; if the pid no
longer exists the stop counts as successful and the pidfile is removed. A
recorder that is still alive keeps the Kotlin warning and pidfile. No
configuration change is needed; expect videos for later tests that Kotlin
loses, and one extra worker command after a failed stop.
Approved 2026-10-31, from WP 5.3.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and [follow-up findings](wp/5.3-findings.md).

## D18 — Android devices are keyed by adb server and serial

Kotlin's Android provider keeps its devices by adb serial alone. With several adb
servers that expose the same serials (every host has `emulator-5554` and
`emulator-5556`), a drop on one host can dispose the healthy device of another:
its running batch is returned and it never works again, while the dropped
device's own actor stays in the pool and its reconnect is ignored. Native builds
key devices by adb server and serial, so a state change on one server affects
only that server's device: the dropped device is disposed and, when it returns,
gets a fresh actor that receives work. No configuration change is needed; expect
all devices of a multi-host farm to keep working through reconnects. A device
that disconnects stayed in the provider's map as in Kotlin until D24 removed it. When several servers
expose the same serial, the replay key of `borrow` is `serial@server` instead of
the bare serial.
Approved 2026-11-02 (project owner's decision).
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[x1 evidence](wp/x1-findings.md) and [x2 findings](wp/x2-findings.md).

## D19 — Apple command output that arrives with EOF is read

Kotlin's Apple command reader (`Reader.kt`) asks whether output is available
and, when none is, whether it may keep reading (over SSH: the channel is open
and has no EOF; locally: the process is alive and has not exited). If the
command's output and its EOF or exit arrive between those two checks, the
reader stops without reading that output. The command then succeeds with an
empty or truncated stdout, so for example simulator setup can fail because a
successful `env` lost its `HOME` line. It is rare (the reader must be
descheduled at that point), but it happens on loaded hosts. Native builds read
whatever is still buffered after the second check before finishing, so every
byte the worker sent before EOF is parsed. Idle timeouts, cancellation and the
other reader behaviour are unchanged. No configuration change is needed;
expect fewer spurious setup and parsing failures on busy workers.
Approved 2026-11-03 (project owner's decision).
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A100](known-kotlin-bugs.md) and [x9 findings](wp/x9-findings.md).

## D20 — The `xctest` parser library is supplied by you

Kotlin marathon ships five copies of `libxctest-parser.dylib` in its resources
(`XCTestParser.kt:61`) and pushes the one for the device's platform to the
worker. The library is third-party software under
[its own EULA](https://github.com/MarathonLabs/marathon/blob/-/vendor/vendor-apple/base/src/main/resources/EULA.md),
not part of the GPL-licensed code. The native program neither contains nor ships
it. `testParserConfiguration: {type: "xctest"}` stays valid, and a run that
selects it reads the file from the directory named by
`MARATHON_XCTEST_PARSER_DIR`, in the layout of the Kotlin resources:
`<platform name>/libxctest-parser.dylib`. Without a readable file the run fails
before it installs anything, with exit status 1 and a message that says what to
supply. The push to the worker and the fake test run are unchanged. The default
`nm` parser needs no library and never reads the variable, so only a Marathonfile
that selects `xctest` needs action; see the
[migration guide](migration.md#the-xctest-parser-needs-a-library-you-supply).
Approved 2026-11-04 (project owner's decision).
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x11 findings](wp/x11-findings.md).

## D21 — `socketIdleTimeout` means what it says

Kotlin's adb client hands the idle timeout in milliseconds to a library that counts
seconds, so the default of 30 s lasted 30,000 s and nothing ever timed out. Native
builds count milliseconds. Requests that a configured timeout already bounds (shell
commands, screen recording, installs, file transfers, screen capture, port forwards) and
streams that are silent by nature (device tracking, logcat, instrumentation, perfetto) have
no idle limit, so a silent 180 s screen recording still works and the idle limit
cannot fire before their own timeout. `socketIdleTimeout` now bounds the four requests with no
timeout of their own: the adb server's version, a device's features, the property reads after boot and the removal of port
forwards when a device is disposed. A value you chose assuming seconds is now taken as written: `PT30S` used to last
30,000 s and now lasts 30 s, and `PT2S` now fails those requests on a slow adb server. Remove the setting, or keep it at
30 s or more.
Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N12](known-kotlin-bugs.md) and the [x12 findings](wp/x12-findings.md).

## D22 — A run survives an adb server restart

Kotlin stops following devices for good when `host:track-devices` fails because the
adb server went away. Native builds reconnect, with a backoff of up to 8 s, and start a
loopback server again the way startup does. Devices keep their work during an outage,
and new batches wait. A test running when the server went away fails as uncompleted and is retried.
Once the server is back, a device still listed carries on, with its logcat and reverse port
forwards restored: each takes new batches once its own are, up to 8 devices per adb server being restored at a time, so
one that does not answer holds up no other ([U170](found-bugs.md)). One not listed within `waitForDevicesTimeoutMillis` is disconnected. An emulator
that itself restarted during the outage looks the same when it is listed again under the same serial:
it is not prepared again, so its batches fail on the missing APKs. `uncompletedTestRetryQuota`, which must be finite
([D47](#d47--an-unlimited-uncompletedtestretryquota-is-refused)), ends them when it runs out, and a small one fails the tests. This is a known limit. A plain adb server restart does not restart emulators. A server
that does not return within `waitForDevicesTimeoutMillis` has its devices disconnected, so the
run ends as it does when devices are lost, after what is left of [D48](#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)'s wait, and other servers are not affected. If adb restarts
take longer on your hosts, raise that value.
Remote test parsing waits in the same way: an outage does not use up its attempts. If the server never
returns at that stage, the run fails with `no devices available` when `deviceInitializationTimeoutMillis`
(180 s by default) has passed, as no device is left to parse on.
Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N16](known-kotlin-bugs.md) and the [x12](wp/x12-findings.md) and [x18](wp/x18-findings.md) findings.

## D23 — Waiting for a pushed file has a limit

With verification on, Kotlin polls a pushed file's md5 with no limit of its own, so a
file sync push whose checksum never matches waits for the whole batch timeout. Native builds
stop after `timeoutConfiguration.pushFile` with an error that names the file. The legacy install
keeps its `Timeout transferring` message. Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and
[N5](known-kotlin-bugs.md).

## D24 — A lost Android device stays out of use and says so

Kotlin keeps disconnected devices in its map, so remote test parsing can borrow a disposed one,
and reports `networkState: CONNECTED` for ever. Native builds drop the device from the map and
report `DISCONNECTED` once it is lost, also after its serial returns as a new device. The change
reaches output only for a device lost by then: `networkState` in its per-test
`test_result` JSON. iOS and macOS devices still report `CONNECTED`
([A72](known-kotlin-bugs.md)). Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N27 and N29](known-kotlin-bugs.md) and the [x12 findings](wp/x12-findings.md).

## D25 — A failed instrumentation stream is reported

In Kotlin, an instrumentation stream that fails (a rejected handshake, a closed connection, output
the parser refuses) looks like a normal end: tests go missing with a generic text and
nothing says why. So does one that merely stops, which is what a dropped adb connection looks like: remote
parsing then takes the tests listed so far for the whole list, and a run can pass with tests missing.

Native builds treat a stream that does not end with a whole `INSTRUMENTATION_CODE: <n>` line, which `am instrument -r`
prints last for every instrumentation that ran, crashed or not, as failed too. A stream cut inside that line, after the
word and before its number, counts as cut. They report a test-run failure with the cause and log it, so
the unfinished tests are retried and their reports carry the real reason, for example `Instrumentation failed:
unexpected end of input: the instrumentation stream ended before a result, with exit code 0`. That includes the test in progress. A test
that finished before the cut stays finished. A device is retired only for a failure that retires it in any other adb
request. A remote parsing attempt fails, and is retried, instead of returning the tests listed so far. One cut can't be
told apart: a stream that stops after other text, before the protocol's first status atom, looks like output that never
was the protocol. It gives no test for that bundle, with no retry; with several bundles the run goes on without that
bundle's tests.

Test crashes and non-zero exits are unchanged, and so are the two ends without the code that are not a cut stream: `am`
refusing to start the instrumentation, and output that never was the protocol. What a failing session code means for
a remote parse is [D42](#d42--a-remote-parse-that-fails-short-of-its-list-is-a-failed-attempt). Approved 2026-11-06 (project owner's decision).
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N38](known-kotlin-bugs.md) and the [x12](wp/x12-findings.md) and [x18](wp/x18-findings.md) findings.

## D26 — DEX 040 and 041 test APKs

Kotlin's parser rejects test APKs newer than DEX 039 with `Invalid dexMagic`. Native builds accept
040 and 041, including 041 containers of several images, and find every class in them. Anything newer is
still rejected with the same message. Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and
[N10](known-kotlin-bugs.md).

## D27 — Prepare no longer wipes external storage

Kotlin runs `rm -r $EXTERNAL_STORAGE` and `mkdir` on every device prepare, deleting everything
under, for example, `/sdcard`. Native builds leave it alone. **This changes behaviour for existing
users: a test suite that relies on an empty external storage must clear it itself.** A folder that file sync pushes is removed first, so
a leftover from a run cut short isn't what the tests read. If the device can't remove it, for example for lack of
permission, the batch fails with `Could not delete remote file(s): <path>: exit code <n>: <rm's output>` and its tests
come back as uncompleted, as when any other device command fails, instead of pushing the folder inside the leftover.
After the batch the pushed folder is removed with Kotlin's command, its path quoted only if it holds a character the
device's shell would split or expand, so a name with a space no longer takes a sibling with it ([N41](known-kotlin-bugs.md)). A video can stay behind on
external storage: from an aborted run, and from a recording cut by a lost connection, in a run that still exits 0. See the
[migration guide](migration.md#android-external-storage-is-no-longer-wiped). Approved 2026-11-06
(project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N4](known-kotlin-bugs.md) and the [x12 findings](wp/x12-findings.md).

## D28 — Each instrumentation runs only its own tests

With test APKs of different instrumentations in one batch, Kotlin gives each instrumentation the whole
batch, repeats the ignored-test callbacks and the end-of-run step, and keeps only the first result.
Native builds run each instrumentation on its own tests, in the order first seen, and report one result
for the batch. If one instrumentation fails or times out the others still run; a lost device ends the
batch, and its unrun tests are retried. `testBatchTimeoutMillis`, `testOutputTimeoutMillis` and clearing
package data apply to each instrumentation. Batches of one instrumentation are unchanged. Few configurations mix
test APKs in a batch: `fixed-size` batching never does, so it takes `class-name` batching and the same class name in
both APKs.
Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N35](known-kotlin-bugs.md) and the [x12 findings](wp/x12-findings.md).

## D29 — A rejected command gives its session slot back

When a worker rejects an `exec` request, Kotlin fails the command with `Request failed` and leaves the
session channel open, so on a shared connection each rejected command holds one of the worker's
`MaxSessions` slots until the connection ends, and later commands fail to open a channel. Native
builds close the rejected channel. The command still fails with `Request failed`, after the worker's reply
or at most 5 s. No configuration change is needed. Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A33](known-kotlin-bugs.md) and the [x13 findings](wp/x13-findings.md).

**An `exec` with no reply** gets the same (U154, approved 2026-11-07). After its 30 s the channel is closed, and the command fails
with Kotlin's error after at most 5 s more, with an error line starting `No reply to exec` that names the command. A worker that stalls and later
resumes may still start the command: one that reads its input ends, and one that does not runs to its end, in its slot. See the
[x21 findings](wp/x21-findings.md).

**A channel open with no reply** is dealt with too (U157, approved 2026-11-07). The open still fails after sshj's 30 s with Kotlin's error, and an error line starts `No reply
to opening`. Nothing can be closed until the worker confirms the channel, so if it answers later, that channel is closed then, where Kotlin would leave it holding a slot until the
connection ends.

## D30 — Closing an SSH session has a limit

Kotlin sends TERM, waits up to 30 s for the worker to close the channel and then fails, and a later close
can wait without any limit. It never sends KILL. Native builds send TERM, give the process up to 20 s to end,
send KILL if it did not, close the channel and give the worker up to 10 s more. After that the session is
abandoned, so no close takes more than 30 s, and the run carries on. On iOS an abandoned session retires its
simulator, so when that simulator was the last device of the pool, the pool waits for [D48](#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)'s limit, then ends, and so does the run. The close fails with the same error
as Kotlin's, and one error line names the command. A worker that confirms the close although the process ignored TERM
and KILL gives no error line: the session closes normally after 20 s, and a warning, shown with debug off, names the command
and says that the process may still be running ([U183](found-bugs.md)). A close that needed KILL and then completed gives
that warning, also when it completed with an error, which then follows its usual error path as well; an abandoned close
gives the error line instead.

**A process can survive this.** A signal over an SSH channel is a request that the server may ignore: some servers
do, such as Tailscale SSH and old OpenSSH versions, and there KILL has no effect. The limit, not the KILL, is what keeps the
run going. An abandoned process may keep running on the worker and hold one of its `MaxSessions` slots until it ends
or the connection does; the connection is shared with other sessions and stays up. If the error line appears, look at
that process on the worker. On a server that ignores signals, the channel of such a process is now closed after 20 s
instead of at once. Measured on Xcode 27.2 with iOS 18.6, 26.4 and 27.0 simulators, xcodebuild ended within 5–16 ms of TERM, and at most 3.06 s
with parallel clones, in 46 runs, so 20 s is ample. A UI test's app under test stays running for at least 90 s after that,
until the next run. This was measured on one Mac only; see the [x25 findings](wp/x25-findings.md).

No configuration change is needed, and the durations are not configurable. Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A14](known-kotlin-bugs.md) and the [x13 findings](wp/x13-findings.md).

## D31 — An rsync send that stops moving fails

Kotlin's rsync send has no timeout, so a send to a worker that has stopped answering waits for ever, and the other sends of
that worker wait behind it. Native builds pass `--timeout=300`: a send with no I/O for 300 s is stopped and fails with an
error starting `send error: no progress in 300 s`. It is a limit on progress, not on the total, so a large bundle over a slow
link is unaffected for as long as it keeps moving. No configuration change is needed, and the limit is not configurable.
[D43](#d43--an-rsync-receive-has-a-limit-on-progress) gives a receive the same limit.

**How long a stalled worker takes** (U163, approved 2026-11-07). A send that stalled does not retry. It ends in `LeavePool`, which neither the installer's
three attempts nor the device's 30 repeat, so the device leaves its pool after one send, and an iOS simulator is retired and stays out of the run. Measured through the real scheduler and device
actor, counting sends: one stalled send, where the same run with any other failure makes 90.

- One stalled send costs 300 s and 5 to 20 s to stop it, up to 20 s more where `ps` is slow.
- Sends already waiting behind it fail at once with the same error, both those to the same worker and those to the same destination, so a
  worker's queue costs one stall and not N. A send that starts afterwards tries again, because the worker may have come back, so simulators that reach
  their send one after another can each cost one stall.
- A second worker carries on. The simulator doesn't come back, even if its worker does. If it was the last device of its pool, the pool waits ([D48](#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)), then ends.
- A healthy worker that is silent for 300 s therefore loses every simulator waiting to send to it at that moment. That is the price of not waiting 7.6 hours.
- Any other failed send, such as an rsync error or a connection that failed, keeps the three attempts and the 30 retries, up to 90 sends.
- While parsing, a stalled send isn't repeated on the same device. Core's own three borrows still apply, and since
  [D54](#d54--an-apple-send-stops-when-its-call-is-cancelled-and-any-transfer-when-its-devices-connection-is-lost) all of it stays within
  `deviceInitializationTimeoutMillis`, whose expiry now ends a send in progress. Before D54 it did not: a stalled send ended
  the parse 305 s after it started, whatever the limit ([x45 findings](wp/x45-findings.md)). A remote parse that fails
  still fails the run; [D53](#d53--remote-parsing-tries-another-device-after-a-failed-attempt) gives its next attempt another device.

**A dead link costs about a minute.** With the default `keepAliveInterval` of 15 s and `keepAliveMaxCount` of 4, the device's own connection dies when its keep-alive fails, the device
is disconnected, and its prepare is cancelled. Measured with a frozen worker: 75 s and one send. What happens to that simulator next is D36's reconnect. This
replaces the earlier estimate of 1.6 hours, which assumed that sends fail while the command connection stays up. With `keepAliveInterval` at 0, nothing but the send's own limit notices:
one stalled send, then the simulator leaves. `--rsh` still sets no `ConnectTimeout`, and that is deliberate. See the [x26 findings](wp/x26-findings.md).

**What a worker sees.** On a host with rsync 3, the flag also reaches the worker in `rsync --server … --timeout=300 …`, so a forced
command or wrapper there that matches that string sees a new flag. openrsync, which macOS hosts have, does not pass it on.

**How a stall is told apart.** A failed transfer counts as stalled only if two things hold. rsync ran for at least 285 s with no progress the bridge saw: since
the start for a send, since its files last changed for a receive. That time is read from the bridge's clock, from when the process started or a look at its files returned, to when the bridge
sees rsync end or report, so a bridge that wasn't scheduled for a while loses none of it. That is the 300 s less 10 s for how late a change is seen and 5 s of tolerance. And rsync said so:
it printed its own report as a whole line, `rsync(<pid>): error: poll: timeout` from openrsync or `[sender] io timeout after 300 seconds -- exiting` from rsync 3
(any role in the brackets, or none before 3.1), or it exited with 30, unless it printed `stopping at requested limit`, since rsync 3 ends `--stop-after` and
`--stop-at` with 30 too. A failure that comes sooner keeps its retries whatever it says, even a report that a file name with newlines forged. The
285 s are counted to when the bridge sees the exit, so a bridge paused across a quick failure can still classify
timeout-looking stderr as a stall.

**What stays unbounded.** An rsync that words its report differently, and then waits on a hung ssh, keeps Kotlin's wait on a send. Its failure also counts as an ordinary one, so it gets the retries.

**What a stop leaves.** Every stall, also one decided after rsync exited by itself, ends the processes the transfer started: TERM, then KILL 5 s later to any
left. The bridge knows them from `ps`, which it runs 1 s after the start, every 10 s after that, and at the stop, so ssh is known even after rsync, or `sshpass`, has
exited and left it. Just before each signal it lists them again and signals only a process with the same pid and start time whose parent is one of them or init,
never rsync's own process once it has been reaped. A pid reused within the same second can still be hit: between that listing and the signal, and earlier too,
when a process the bridge saw exits and a new one takes its pid in the second it started, then is orphaned to init before that listing. A process
started and left behind between two looks is missed; so is an orphan that a subreaper other than init adopts, as on some Linux desktops; and where `ps`
fails or takes over 5 s only the bridge's own child is stopped. rsync isn't put in a process group of its own, so that a terminal's Ctrl-C or a CI job's cancel
still reaches it as in Kotlin.

**Under `sshpass`** the TERM goes to rsync and what it started, rsync first and each process before those it started, and not to
`sshpass`, which then exits with rsync's own status. `sshpass` 1.10 exits 0
at once on a TERM of its own that comes before ssh has prompted, without waiting for rsync, so until x50 a transfer under it to a worker
whose ssh hung, stalled or stopped, counted as a success ([U181](found-bugs.md)). Only where no listing found rsync does `sshpass` get the TERM, and then a 0 from it counts as a failure. Measured with
rsync 3.4.1, 3.5.2 and openrsync, and `sshpass` 1.06, 1.09 and 1.10 ([x50 findings](wp/x50-findings.md)).

Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A3](known-kotlin-bugs.md), the [x13 findings](wp/x13-findings.md) and the [x26 findings](wp/x26-findings.md).

**A receive's output pipes** also have the send's limit (U158, approved 2026-11-07). A process that ended, leaving a child that holds its pipes, such as a hung
ssh, holds up the receive for at most 5 s more. The process's status decides the result, and that child is not stopped.

## D32 — xcodebuild on a worker runs unbuffered

Kotlin sets `NSUnbufferedIO=YES` for xcodebuild, but over SSH it drops every environment variable, so on a worker
xcodebuild buffers its output, and the output idle timeout can see silence where a test is in fact progressing. Native builds run
`env NSUnbufferedIO=YES xcrun xcodebuild test-without-building …` on a worker. Output now arrives as it is produced, in smaller
pieces, and the idle timeout measures xcodebuild's real silence. A `testOutputTimeoutMillis` chosen to allow for
the buffering may be more generous than you now need. Other environment variables and the working directory are still
not passed over SSH ([A2](known-kotlin-bugs.md)), and a local run sets the variable as before.

The worker receives a different command line, so a script or a forced command (`ForceCommand`, `command=` in
`authorized_keys`) that matches `xcrun xcodebuild` at the start of `SSH_ORIGINAL_COMMAND` must allow for the prefix. `env` has
to be on the worker's `PATH`, as it is by default on macOS. Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the [x13 findings](wp/x13-findings.md).

## D33 — A simulator whose setup failed is set up again

In Kotlin a simulator whose setup fails stays tracked as connected. Nothing sets it up again, so it
is out for the whole run, unless a devices update removes it and adds it back. Native builds retry it on a timer,
up to three times, 30, 60 and 90 s after each failure. After the last retry it stays out, as in Kotlin, and an
error says so. A simulator that left the devices file is not retried, and one added back starts over. A profile
is different: removing a worker from the devices file doesn't remove its profiles, as in Kotlin, so a pending retry
carries on and **can still create a simulator on that removed worker**, until the retries run out or reach the
cut-off. A failed managed simulator is deleted before its replacement is created, so retries don't pile up simulators;
one that could not be deleted is reused. Retries stop at `workerConnection.lateJoinCutoff`, except for a simulator that was
in the run and is returning after a loss ([D44](#d44--a-lost-simulators-reconnect-has-a-limit)), and when the run ends. The
end of a run doesn't wait out a pause, but it does wait for an attempt in flight to stop. A retried simulator is
admitted to the run like a late one. An unreachable worker keeps its own
connection retry. A refused connection, a simulator the worker doesn't have and an unavailable profile are not
retried. There is no new setting: `attempts: 1` with `lateJoin: false` turns the retries off. `simctl` commands and
log lines increase only when a setup fails. The macOS provider is not affected. Since
[D44](#d44--a-lost-simulators-reconnect-has-a-limit), a failed setup of a simulator that another simulator's setup took along
counts too and starts a pause of its own. So the cap is four failed setups with the worker answering, the first included,
counted wherever they happen, and when several simulators on one worker fail the pauses between one simulator's setups can be
shorter than 30, 60 and 90 s. The cap has two exceptions. A setup that finds the worker unreachable isn't counted: the worker
connection retry takes the simulator and has its own limit. And removing a simulator and adding it back starts its count
over; a setup of it that was already running then counts for neither.
Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A81](known-kotlin-bugs.md) and the [x14 findings](wp/x14-findings.md).

## D34 — A failed history query is not final

Kotlin caches a history query that fails three times as empty for the whole run, so a short backend outage
at the start costs the run its history. Native builds answer with the defaults, as Kotlin does, and query
again in the background, at most 8 times per query. The delays 5, 10, 20, 40, 80, 160, 300 and 300 seconds count from the
first failure of any query, not from each query's own: a query that fails later joins the next round. An empty answer
counts as an answer and is not repeated. A queue already built keeps its order, so new history reaches
queues built later and batches composed later, so with a failing backend the order of tests can depend on when its answer arrives. A run never waits for this, and it stops when the run ends. With
a healthy backend nothing changes. With a failing one the log gains one failed timing line and one warning per
re-query, and one line when the history returns or the attempts run out. An empty answer gets that line too, and it says
that nothing changes. Closing the provider cancels a re-query at once. The HTTP clients stop there, and a query that blocks
its thread gets 3 seconds and one warning, and the backend is then closed anyway. The reports are still written, and since
[D52](#d52--a-cancel-writes-the-reports-and-cleans-up-within-a-limit) the process ends at most 3 s after the run does, whether or not that call has returned. No configuration change is needed.
Approved 2026-11-06 (project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[C41](known-kotlin-bugs.md), the [x15 findings](wp/x15-findings.md) and the [x22 findings](wp/x22-findings.md).

## D35 — A failed key exchange is not retried as a network fault

When a worker and marathon share no key exchange, cipher, MAC or host key algorithm, Kotlin calls the failure
transient and keeps retrying a worker that cannot succeed until its configuration changes. Native builds stop, as
they do for a rejected login or an unverifiable host key. The error text is the same, and network faults are still
retried. The four immediate connection attempts remain. Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A16](known-kotlin-bugs.md) and the [x16 findings](wp/x16-findings.md).

**Migration action:** none. If a worker has no algorithm in common with marathon, fix its SSH configuration; the run no longer keeps retrying it.

## D36 — A connection lost after login is a connection failure

When a worker drops the connection right after authentication, Kotlin's next command throws
`IllegalStateException: Not connected`, past the handling that commands which may fail rely on. Native builds report
`SSH connection failure: Not connected` with the reason as its cause, so such a command returns no result, an error
log says so, and marathon handles the device as it does any lost connection: on iOS it reconnects, and on macOS, which has no reconnect,
the device leaves the run. Using a live connection after marathon
closed it still reports `invalid state: Not connected`.

**The iOS reconnect has a limit** since [D44](#d44--a-lost-simulators-reconnect-has-a-limit). A worker that drops each
connection after login counts as unreachable, so it is tried at `workerConnection`'s pace and for as long as D44 says, not
every 16 ms.

Approved 2026-11-06 (project owner's decision).
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A17](known-kotlin-bugs.md) and the [x16 findings](wp/x16-findings.md).

**Migration action:** none. If a worker keeps dropping connections after login, look at the worker; an iOS run tries it as [D44](#d44--a-lost-simulators-reconnect-has-a-limit) describes.

## D37 — One unread output stream no longer kills the connection

stdout and stderr share one SSH receive window. In Kotlin, a command whose stderr is read while its stdout is left
unread (or the reverse) can overflow the 16 MiB buffer and lose the connection, failing every command on that worker.
Native builds stop giving the worker more window, so it blocks instead and the connection and its other commands
carry on. While it is blocked the other stream stops too, until that output is read, the command's timeout or idle
timeout ends it, or the session is closed. Commands whose output is read promptly behave as before. Approved 2026-11-06
(project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A32](known-kotlin-bugs.md) and the [x16 findings](wp/x16-findings.md).

**Migration action:** none.

## D38 — A failed readiness probe means not running

From runtime 16 on, Kotlin reads the output of `launchctl dumpstate` without checking its exit status, so a
failed or timed-out probe whose output already names a started SpringBoard service counts as running.
Native builds treat a failed or timed-out probe as not running on every runtime. A healthy simulator exits 0:
measured on iOS 18.6, 26.4 and 27.0, while runtimes 16, 17 and other 26.x were not measured. One failed probe costs
one more poll. A probe that fails for the whole of `bootWait` costs up to four setups of two boot waits each, about 26
to 30 minutes of booting and shutting down per simulator with the defaults, and then the simulator is out of the run.
That figure holds while another simulator keeps the pool alive, and leaves out the waits for shutdown and for the session to
close. When only the probe hangs and needs KILL, the same count gives about 33 minutes. On the last device
[D48](#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)'s wait decides instead: the run ends
`deviceInitializationTimeoutMillis` (180 s) after the simulator left the pool, or after a test result it reports later, with
`remaining tests will not run`, because a re-setup that never gets ready neither moves nor extends that wait. That is about
9 minutes after the first boot when the probe fails from the start, up to about 11 with a probe that times out or needs
KILL, and about 3 minutes after the loss when the probe starts failing at a re-setup ([x42 findings](wp/x42-findings.md)).
On runtimes 16 and 17 each probe returns about 20 MB, so a host or link too slow to deliver that within `shell` now takes
this path, where Kotlin counted the marker once about 80% of it had arrived. From runtime 18 on [D46](#d46--the-readiness-probe-is-launchctl-print-userforeground-from-runtime-18)
sends a probe of about 100 KB, which makes this risk a small one there. The 26 to 30 minute bound above does not depend on the probe's size. No probe result erases a simulator; a configured
`onPrepare: [ERASE]` erases on each re-setup, as on any setup. The commands are unchanged. Approved 2026-11-06 (project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A41](known-kotlin-bugs.md) and the [x17 findings](wp/x17-findings.md).

**Migration action:** none on a normal host. On a slow host or link running iOS 16 or 17, raise `shell` (30 s by default) so that a probe that returns about 20 MB can arrive in time.

## D39 — A test's own text does not abort its batch

Kotlin looks for its device failure texts anywhere in xcodebuild's output, so a test named after one, an
assertion message or an application's log line holding one aborts the batch. Native builds ignore these texts in
test and suite names, UI test activity lines, Swift Testing events and an application's log lines.

An assertion line, or an `Assertion Failure:` activity line, still reports when its message starts with a failure text.
That is how a failure the UI test framework records itself reads, so a simulator that cannot terminate its application
still leaves after three such batches, as in Kotlin. After an XCTest prefix such as `XCTAssertTrue failed - ` or
`failed - ` the text is the test's own: that test fails and the batch goes on. This split is reasoned from Xcode's known
formats, and no recording holds a raw framework failure.

Every other line matches as before, including `xcodebuild`, `simctl` and test runner logs. These still abort the batch
although the text is a test's own (false alarms):

- A raw `print` holding a failure text, since it cannot be told from a real failure.
- A test's own text that starts an assertion with a failure text and has no prefix, as `record(XCTIssue(…))` can.
- The later lines of a multi-line assertion message.
- A test name in xcodebuild's final `Failing tests:` summary.
- An application logging exactly one of XCTest's three sentences.
- A line that misses the form checks, for example with a trailing `\r`. A CRLF stream therefore switches the exemption off
  for every assertion and activity line; the SSH exec path has no terminal, so this is not expected.

The [x17 findings](wp/x17-findings.md) list the rest.

Some real failures are now missed:

- One appended to an unterminated assertion, activity or application log line. This is unlikely, since those lines are
  written whole.
- A framework failure behind an XCTest prefix, or in an activity line other than `Assertion Failure:`. The test fails,
  and the simulator keeps taking batches.
- One logged with a date header by a process other than `xcodebuild`, `simctl` or a test runner, apart from XCTest's own
  three sentences. The batch waits for its timeout if the runner has died.

A missed failure ends when xcodebuild's output has been silent for `testOutputTimeoutMillis` (300 s by default), once per
batch. In Kotlin the failure text itself counted as a batch failure, so since 2026-11-07 ([U165](found-bugs.md)) any such
timeout before a test has passed or failed counts as one too, with or without a missed text, because the silence is all
that's left to see. Three in a row, about 15 minutes by default, and the simulator is set up again. A batch in which a test
passed or failed resets the count, as Kotlin does for every timeout.

This also reaches a test that goes silent past the timeout with no failure text at all, which Kotlin never recycled for. A
simulator has 3 recoveries: the first starts at once, the others after pauses of 30 and 60 s. While another device keeps the
pool alive, a simulator on which every batch goes silent takes 12 batches, 3 on each of its 4 setups, about 60 minutes plus
setups and those pauses, and then leaves the run. On a pool's only simulator each recycle empties the pool, and
[D48](#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)'s wait (`deviceInitializationTimeoutMillis`, 180 s by
default) decides. A recovery that is set up and ready within it finds the pool and its tests still there, so with the
defaults and a setup shorter than about 2 minutes it is the same 12 batches, and the pool ends 180 s after the fourth loss
with its tests unrun. A recovery that misses the wait ends the pool after 3, 6 or 9 batches; before D48 it ended after 3.
Either way the run ends, where Kotlin repeats the batch until CI kills the job. `uncompletedTestRetryQuota` can end it
sooner. `crates/marathon-ios/tests/simulator_pool_recovery.rs` pins both outcomes through the real scheduler.

Approved 2026-11-06 (project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A48](known-kotlin-bugs.md) and the [x17 findings](wp/x17-findings.md).

**Migration action:** none unless a batch waits out its timeout or a sick simulator keeps taking batches. Then read the failing line in the log, and consider setting `maxBatchesWithoutProgress`. A test that is silent for longer than `testOutputTimeoutMillis` and passes needs that timeout raised, as before; three such batches in a row now also recycle the simulator.

## D40 — A stale disconnect leaves a device's replacement alone

Kotlin removes a device from its pool by serial alone, so a delayed disconnect for an old device could remove its
replacement with the same serial. Native builds compare the device instance, as readiness and termination already do.
The old device has already been told to terminate by then, and its batch comes back. No provider path found today sends
that order, so this makes the pool right on its own. Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [C27](known-kotlin-bugs.md) and the
[x17 findings](wp/x17-findings.md).

**Migration action:** none; no known path triggers it.

## D41 — A brief `offline` does not lose the device

Kotlin disconnects a device at its first `offline`, so a transport that returns within a second ends a run on its last
device. Native builds give a device with a running actor up to 10 s in `offline`, `connecting` or `authorizing`, with no
configuration change. Meanwhile no batch starts on it and it takes no new work. A test running when the transport dropped is
retried, as in any lost connection. A device that returns keeps its actor, with its logcat and reverse port forwards restored.

- A device still passing through at 10 s is disconnected, as before but later.
- A device that returned is asked for its properties (one `getprop`, bounded by `timeoutConfiguration.shell`, 20 s by
  default), and its logcat (up to 1 s) and each of its reverse port forwards (up to `timeoutConfiguration.portForward`,
  which defaults to `shell`) are restored. It takes no new batch until that is done. This runs beside the reading of the
  device list, for up to 8 devices per adb server at a time, so a device that does not answer holds up no other device's
  limit ([U170](found-bugs.md)).
- A device that goes `offline` and then leaves the list has 3 s, since a real blip passes through an empty list for up to
  a second. A killed emulator looks like this, so its retry path starts about 3.25 s after it dies, where it was immediate.
- A device leaving the list directly from `device`, or reaching a state such as `unauthorized`, is disconnected at once.
- A device disconnected after any of this that was the last of its pool has what is left of [D48](#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)'s wait to return.
- A device that returns still booting went through a reboot, and is set up again as after any disconnect. One that has
  finished a reboot by then, as from a quick snapshot, is taken for a blip and not prepared again.
- A device has at most 5 of these per adb server per run. The count is never reset, not even when the device comes back
  healthy or its serial returns as a new device. From the sixth `offline` on, the device is disconnected at once, as in
  Kotlin, with the log line `Device <serial> went offline too often; disconnecting it`, so a device that blips more often
  than that in one run is lost for good and its tests are retried elsewhere.
- During a grace `networkState` reads `DISCONNECTED` and `healthy` false in a per-test JSON written then, also if the device
  returns. A return adds one `getprop` to the commands sent, and a device loss during a batch a `host:devices` in place of
  D22's `host:version`. A healthy run sends the same commands and events.

A server outage takes over from a grace, and a device still `offline` when the outage's window closes gets no further grace.
Approved 2026-11-07 (project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N40](known-kotlin-bugs.md) and the [x19 findings](wp/x19-findings.md).

**Migration action:** none; no configuration is needed. A killed emulator reaches the retry path about 3.25 s after it dies, not at once.

## D42 — A remote parse that fails short of its list is a failed attempt

In Kotlin, a remote parsing session that ends with a failing `INSTRUMENTATION_CODE`, usually 0, keeps the tests it listed,
whatever it says, unless the text is `Process crashed`. A run can then go on with part of its tests and pass.

Native builds treat such a session as a failed attempt. It takes the existing retries, and if it fails the same way each
time, the run fails with `Unable to parse test list using <serial>: <what the session said>, tests listed: 1 of 20`. All
bundles and attempts share `deviceInitializationTimeoutMillis` (180 s by default), so a slow parse can end in
`no devices available` instead, with no such text.

A session that listed every test its `numtests` announced keeps its list, whatever its code and message: some sessions end
that way on a whole list, and a retry would only repeat it. A session with no count to reach has shown nothing and fails.
Code -1 is success as before, even with fewer tests than announced, unless the stream was cut inside that last line, which
has no line break then. `Process crashed` still means the test annotation
producer, with the same fallback. `am` refusing to start and output that never was the protocol are unchanged, and so is
the batch runner's handling of the same endings. Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [N38](known-kotlin-bugs.md) and the
[x20 findings](wp/x20-findings.md).

**Migration action:** none for a normal test APK. A custom runner that stops listing its tests early and exits with a failing code now fails the run with its text, where it used to run the tests it had listed.

## D43 — An rsync receive has a limit on progress

Kotlin gives a pull 30 s in total, so a big video or result bundle over a slow link was cut while it was still moving. With key authentication the file was lost.
Under `sshpass` the TERM reached only `sshpass`, and rsync finished in the background, until U155 stopped that too.

Native builds pass `--timeout=300` on a receive as on a send, and a pull is not cut while it moves: while its own files change at least once every 300 s, and
rsync's report counts only after 285 s without a change (see D31). Otherwise it is stopped as a stalled send is: 5 s to end by itself, then TERM, 5 s, and KILL to what the transfer started (see
[D31](#d31--an-rsync-send-that-stops-moving-fails)). It then logs an error starting `receive error: no progress in 300 s` and returns `false`, usually with
`.rsync-partial/<name>` left in the output directory. A failed pull has no retries, as before. The video listener carries on and removes the remote video, the
device log listener carries on and leaves the remote log, and the result bundle listener records `pull failed`.

**`--timeout` is not enough on its own.** Between two rsync 3 installs a pull that stops mid-file never reports: the generator's keep-alives count as I/O, so rsync waits for ever where the total
deadline used to end it. So the bridge also looks every 10 s at the pull's own files, and stops it once they have not changed for 300 s, which is 300 to 310 s after the last change. Its own
files are the destination (all of it, for a folder), rsync's temporary `.<name>.XXXXXX` beside it and `.rsync-partial/<name>`; another transfer into the same folder doesn't count. A
source ending in `/` fills the folder itself, and then the whole folder counts. openrsync reports either way, and so does rsync 3 with a worker that has openrsync,
about 1.5 times `--timeout` after the bytes stop, so after the look at files. rsync 3.5 behaves as 3.4 here ([x50 findings](wp/x50-findings.md)).

The look at files can stop a pull that is still moving: rsync 3 writes in pieces of 256 KiB, so a link slower than about 1 KiB/s can look stalled, and so can an rsync that a wrapper sends
elsewhere, such as with another `--partial-dir`.

**What a stuck worker costs.** A stuck pull costs about 305 to 330 s where it used to cost 35 to 45 s, and up to 20 s more where `ps` is slow. Nothing bounds the total over a run: a worker whose rsync hangs while its commands still work costs that much for
every pull of every batch, with the simulator held up meanwhile. On iOS result bundles are pulled in the background and are cancelled 300 s, then 60 s, after the run; on macOS they are pulled inline too. A dead link costs far less, because keep-alive ends
ssh in about 60 s, and the device is disconnected soon after.

**What a worker sees.** On a host with rsync 3, the worker's `rsync --server --sender` gets the flag. A wrapper set with `RSYNC4J_RSYNC` gets it on every transfer. openrsync doesn't pass it on.

The diagnostic `Timeout of 30 seconds reached, terminating process...` is gone. Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [A3](known-kotlin-bugs.md) and the
[x26 findings](wp/x26-findings.md).

**Migration action:** a forced command or wrapper that matches rsync's arguments must accept `--timeout=300` on receives as well. Nothing else.

## D44 — A lost simulator's reconnect has a limit

In Kotlin a registered simulator whose connection is lost is tried again every 16 ms until the run ends, and the failure of
each attempt is thrown away. Repeated failed logins can trip fail2ban or sshd's penalties. Native builds try once at once,
as Kotlin does, and then hand the simulator to whoever the failure belongs to:

| The re-provisioning… | What happens | Limit, with the defaults |
|---|---|---|
| can't reach the worker, or the connection drops right after login | The worker connection retry | One attempt per worker at 10, 30 and 90 s, then one a minute, 11 in all, until `lateJoinCutoff` after the first failure (below) |
| fails with the worker answering | [D33](#d33--a-simulator-whose-setup-failed-is-set-up-again)'s budget, which every such failure of that simulator counts against, also one in another simulator's setup that took it along | At most four such setups, the first included, with D33's two exceptions: a setup that finds the worker unreachable goes to the row above, and a re-add starts the count over. A retry waits 30, 60 and 90 s after the first, second and third failure; a failure in another simulator's setup starts a pause of its own, so the gaps can be shorter |
| is refused by the simulator's own connection (authentication, [D35](#d35--a-failed-key-exchange-is-not-retried-as-a-network-fault)) | Nothing, and an error says so, also if the planning connection drops in the same setup | No retry |

This applies to every failed re-provisioning: after a lost worker, after a simulator lost with its connection up, and after
one added back to the devices file. In Kotlin those last two were final. Each limit here holds between two changes of the
devices file for that simulator: a devices update that removes it and adds it back starts its limits over, as below.

**What a lost worker costs, with the defaults.** Take N simulators lost with one worker, and no devices update that adds one
of them back meanwhile; each such update adds the attempts of a new start. The first attempt acquires one
connection per simulator. The worker retry then makes 11 attempts in 600 s, one a minute once the first pauses are over, and
each acquires one planning connection and, if planning gets through, one more per simulator still waiting. That is at most
N + 11 × (N + 1) acquisitions: 13 for two simulators whose worker doesn't answer, and 35 for two whose worker answers while
their own connections fail. The 11-attempt schedule and the N + 11 × (N + 1) bound apply to one uninterrupted worker
retry: hand-overs through D33 can start the connection backoff again and exceed that count without a devices update.
Kotlin made up to about 3,750 attempts a minute per simulator. Each acquisition makes up to four
TCP connections to the SSH port, sshj's immediate retries, and logs in once if one of them gets through, so there is at most
one failed login per acquisition. A refused simulator connection takes that simulator out, and a refused planning connection
ends the worker retry. With `checkReachability` a host that doesn't answer gets
no TCP connection at all. With `shareWorkerConnection: true` the acquisitions on a worker share one connection, and those
waiting for the same attempt share its result, so there are fewer. The acquisitions are measured with a scripted connection
factory; what each costs on the wire is read from the code.

A simulator that was already in the run is not a late joiner, so the late-join cut-off doesn't stop its return: a worker lost
an hour into a run can still come back. It gets its own limit of `lateJoinCutoff`, counted from the first failure, and
`lateJoinCutoff: 0` means no limit, so the end of the run ends it. Other settings work as they do for any worker:
`lateJoin: false` stops after `attempts`, and `attempts: 1` with `lateJoin: false` leaves only the first attempt. After the
limit an error line says the simulator stays out of the run. The limit is looked at when a pause ends and when an attempt has
connected, not on a timer of its own, so the end comes at the first such point past it: 630 s with the defaults when every
attempt fails at once. It can be late by at most one pause (`retryInterval`) plus the time of an attempt in flight, which that
attempt's own connection and setup timeouts bound.

The old device raises one disconnect, where Kotlin raised one per attempt, and a worker that comes back gives one
connect and one actor per simulator. Termination cancels a retry that is waiting.

**What stays final.** These stay out until a devices update removes the simulator and adds it back: a refused connection, a
spent D33 budget, a worker retry at its limit or out of `attempts`, a non-transient error in that retry, and no retry at all
(`attempts: 1` with `lateJoin: false`). Another simulator's setup on the same worker doesn't take them along; only one that a
retry is still waiting for goes into such a setup. Removing and adding back is a new start, not a promise: the simulator gets
one new attempt with its D33 budget and its limit started over, and the rules above apply to that attempt as to the first.
These stay out for the run in any case: a simulator lost more than three times with its connection up, one
retired because its session didn't close or was abandoned ([D30](#d30--closing-an-ssh-session-has-a-limit)), and a late
joiner denied admission at the cut-off.

**Work that started before a re-add.** A recovery, a worker retry attempt, a D33 retry or a setup that started before the
devices file removed a simulator and added it back decides nothing for it afterwards: no parking, refusal, giving up, count
or hand-over to a retry. A simulator that was in the run and is added back gets its own attempt at once, or, if an older
recovery of it is still running, as soon as that recovery ends. If that attempt can't reach the worker, it joins the worker
retry. An attempt of that retry that began before the re-add doesn't take it: the retry's next attempt does, or, if that
older attempt ends the retry, the simulator gets a retry of its own, with its limit started over. A setup takes along only
simulators on which it holds a current claim; an unclaimed simulator that becomes eligible during it waits for the next setup. A setup
that succeeds is kept whenever it started.

**Not limited:** a connection that keeps dropping after setup succeeds. Each round is a full setup, so its pace is the time
that takes, and a successful recovery has no count, since a worker may reboot more than once in a long run.

**Remote parsing's `borrow`.** Kotlin picks at random from a map that keeps removed and disposed simulators, so a parse could
get one and use up one of its three attempts. Native builds pick only a simulator that is connected and neither removed,
lost, on its way out nor waiting to be set up again. With none, `borrow` waits, and `deviceInitializationTimeoutMillis`
bounds that as for no devices at all, as [D24](#d24--a-lost-android-device-stays-out-of-use-and-says-so) does on Android. A
healthy run sees the same choice. macOS has no change.

Approved 2026-11-07 (project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A101 and A80](known-kotlin-bugs.md), [U164](found-bugs.md) and the [x27 findings](wp/x27-findings.md).

**Migration action:** with `attempts: 1` and `lateJoin: false`, a simulator whose worker drops now stays out after one failed
reconnect; it used to return whenever the worker did. To keep waiting, leave `lateJoin` on, and set `lateJoinCutoff: 0` to
wait until the run ends.

## D45 — A hung test is a failure, however its allowance is printed

XCTest reports a test that exceeded its execution time allowance on one line, and that line is the only sign in its output:
no `failed` line follows. Kotlin reads the allowance by replacing ` seconds`, ` minutes` and ` hours` and handing the result
to `Duration.parse`. A singular unit throws, and XCTest prints `1 minute` for the 60 s that Marathon's own setup examples
recommend. The throw isn't a device failure, so the batch comes back with its tests uncompleted, and with the default
`uncompletedTestRetryQuota` a test that hangs every time repeats until CI kills the job, which D47 now refuses.

Native builds never stop on that text. What Kotlin could read reads the same. Then the English forms of XCTest's formatter
read: `1 minute`, `1 hour`, `1 minute, 30 seconds`, fractions such as `1.5 minutes`, and abbreviations such as `1 hr`. Any other
text, such as another language, is taken as 0 s with a warning; it only dates the failure, so the test still fails.

The test now fails, the batch goes on and the runner restarts as xcodebuild intends. A hung test with no failing line of its
own has the hang's line as its trace, including the plural forms, where Kotlin reported none. An earlier failing line in the
same test keeps its place. Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [A47](known-kotlin-bugs.md) and the
[x28 findings](wp/x28-findings.md).

**Migration action:** none. A test that hangs on every attempt now fails instead of repeating, so a run that used to be killed
by CI ends with a failed test.

## D46 — The readiness probe is `launchctl print user/foreground` from runtime 18

Kotlin asks whether a simulator is running with `launchctl dumpstate user/foreground/com.apple.SpringBoard` from runtime
16 on, and reads one line of the answer. The answer is 10 to 20 MB, on every poll of the boot wait. Native builds send
`launchctl print user/foreground` instead when the simulator's runtime major is 18 or later. Runtimes 16 and 17 were not
measured and keep `dumpstate`. A version that does not parse as a number keeps `print system`, as before, so the choice
is `print system` below 16 or unparsable, `dumpstate` on 16 and 17, and `print user/foreground` from 18.

Measured on iOS 18.6, 26.4 and 27.0: exit 0, 97 to 121 KB, and the same marker line, `0x… M A com.apple.springboard.services`
or `M D` while it starts, so the parser that reads the state before `com.apple.springboard.services` is unchanged. An earlier
measurement compared the two probes over 666 paired rounds, and they agreed on 658. In the 8 that did not, `print` reported
ready up to one poll earlier. `launchctl print system` and `simctl bootstatus` are not equivalent and were not used. A re-measurement for
this change is in the [x29 findings](wp/x29-findings.md). A probe that fails, or times out, is still not running (D38).

The effect is on the size, not the count: a boot poll costs about a hundredth of the bytes, so a slow link or a busy worker
no longer fails the probe for want of 20 MB of delivery, and a long boot wait holds far less in memory. Boot readiness may be
seen up to one poll interval earlier. The command a worker sees changes, and the commands of a simulator on 16 or 17 do not.

The JVM rows stay as the witnesses of what Kotlin sends: `fixtures/oracle/apple-bin/probes.jsonl` (`sim.running/16.0/*`, which
the tests expect as `print user/foreground` on 18 and later and unchanged on 16 and 17) and
`fixtures/oracle/simulator-device/events.jsonl`. The differential comparator rewrites the probe in a JVM record, or a native record pinned before D46,
only where the same record's own `SIMULATOR_RUNTIME_VERSION` reply for that simulator reads 18 or later, and refuses a record from
before D46 that already holds the new command. Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [A41](known-kotlin-bugs.md) and the
[x29 findings](wp/x29-findings.md).

**Migration action:** a forced command or filter on an iOS worker that allows `xcrun simctl spawn … launchctl dumpstate
user/foreground/com.apple.SpringBoard` must also allow `xcrun simctl spawn … launchctl print user/foreground`, or the probe
fails and the simulator is out of the run (D38).

## D47 — An unlimited `uncompletedTestRetryQuota` is refused

**This breaks existing Marathonfiles.** In Kotlin, and until now here, the quota defaults to `Integer.MAX_VALUE` and no
deadline ends a run, so a test that keeps coming back uncompleted repeats until CI kills the job.

Native builds refuse that configuration. `marathon run` and `marathon parse` exit 1 with `configuration rejected` when
the quota is unset or exactly 2147483647, on Android, iOS and macOS, and name the field, the reason, a suggested value and
the [migration section](migration.md#a-finite-uncompletedtestretryquota-d47). Any other value passes: 0, a negative value, a literal `null` or an empty value, which bind to 0 or act like it and give a test no second attempt, and 2147483646, which is your choice to make. A test's count of uncompleted returns stops at 2147483647 where Kotlin's wraps, so the largest accepted quota still ends the loop when copies of one test in a batch are charged together. A value from an environment
substitution such as `${QUOTA}` counts as the value it resolves to. No command-line option sets this field.

The rule is that the check fails only when nothing bounds the loop, and on every vendor only the quota does:

- `retryStrategy` and its quotas apply to failed tests. An uncompleted test never reaches them.
- `executionStrategy` completes a test on a pass or a failure, and a repeating uncompleted result is neither.
- The flakiness strategy only decides how many copies to start with. The timeouts bound one batch, not how many there are; on iOS a silent output timeout also counts towards recycling that simulator (D39), which retires a device, not the run.
- Android and macOS have no count of batches without progress. `maxBatchesWithoutProgress` isn't a macOS setting at all.
- iOS's `maxBatchesWithoutProgress` retires one simulator at a time, and it isn't charged for an unclassified error, or for
  a lost connection, after which the simulator comes back with a new count. So it doesn't excuse an unlimited quota,
  and it is no longer something you have to set: 0 stays off.

`marathon --help`, `version` and the hidden `config-dump`, which run no test, don't apply the check. A stub or missing
vendor keeps its own error. Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [C85](known-kotlin-bugs.md) and the
[x30 findings](wp/x30-findings.md).

**Migration action:** add `uncompletedTestRetryQuota` to every Marathonfile. See [the migration guide](migration.md#a-finite-uncompletedtestretryquota-d47).

## D48 — A pool that lost its last device waits for one to return

Kotlin ends a pool the moment its last device goes. The remaining tests never run, and a device that returns is dropped, so
a device that is away for a moment ends the run with exit 1. On iOS this defeats the worker-loss recovery of a pool on one worker.

Native builds keep a pool with tests left to run for up to `deviceInitializationTimeoutMillis` (180 s by default), with no new
setting. It is the time a run already gives a device to appear. A device that returns finds the queue as it was: same tests,
same order, same retry counts. If none returns, the pool ends as before, with `remaining tests will not run`, once the
limit is up and the devices leaving have finished cleaning up. A device that joins after the limit is dropped.

- **The bound.** The time starts when the pool's last live device leaves. A live device is one that was in the pool before
  it lacked one. A device added while it lacks one is live only once it reports ready, and stays not live until then
  whatever else happens, a test ending included. The start is the latest such departure since a test last ended, less what
  the vendor had already waited for that device, whichever order the devices are removed in. A test that ends while no
  device is live, reported by a device that has already left, starts the wait afresh at that moment: Android reports a
  batch before it pulls its files, so this is common, and the wait does not move to the end of that cleanup. With `L` for
  `deviceInitializationTimeoutMillis`: if no test ends and no device that came back is ready at `start + L`, the pool stops
  the devices still getting ready, takes no new ones, and ends when every device has reported Terminated, so that their
  outcomes count. It ends at `start + L` plus the longest cleanup still running then: a leaving device's cancelled batch or
  prepare, which the vendor's own timeouts bound, not D48. Devices that keep coming and going without getting ready don't
  move the start.
- **One wait, not several.** D41's grace and D22's outage are part of it: a killed emulator ends the run 180 s after its
  first `offline`, and a server that never returns 180 s after it went. A device on that server that was healthy until the
  outage counts from the outage, not from another device's grace. If `waitForDevicesTimeoutMillis` is that long or longer,
  there is no wait after the outage.
- **A device that came back and got ready.** If one is in the pool at `start + L`, the pool goes on with it, and ends when it
  leaves and has cleaned up, unless a test has ended meanwhile. Any test that ends, passed or failed, gives the pool a whole
  new wait: with a live device in the pool the wait is off until the next departure, and with none it starts at once.
  So a device that keeps coming back and leaving without finishing a test can't stretch the limit, but one that stays keeps
  the pool for as long as it stays, and a test that ends on it starts the wait afresh. Such a run is bounded by its
  retries and timeouts, as any run is.
- **Nothing is waited for with nothing to run.** A pool whose queue is finished ends at once, as before, and so does a run
  with no tests left, or `deviceInitializationTimeoutMillis: 0`. A healthy run's logs and commands are unchanged.
- **Each pool keeps its own time.** A device leaving another pool does not start, restart or end it. The run ends when every
  pool has ended, so a waiting pool holds up a run whose other pools are done. A pool created during a wait is joined; one
  created after the scheduler has finished joining its pools is still missed, as in Kotlin.
- **macOS does not wait**, as nothing there can bring a Mac back.
- **A device that left the pool without leaving adb's list is waited for too**, although it comes back only if the list changes:
  one whose prepare failed for good, or that [D49](#d49--a-lost-adb-request-does-not-retire-a-device-that-is-still-there) gave
  up on. That costs up to the limit on top of what it costs today.

A run that loses its last device for good now takes up to the limit longer to fail: 180 s where a killed emulator took 12.5 s.
A worker away for longer than the limit still ends the run. New log lines appear only during a wait: `no device left,
waiting up to <n> ms for one to return`, logged once the devices leaving have finished cleaning up and only if tests are left
then; `no device returned within deviceInitializationTimeoutMillis`; `device <serial> has arrived`; `device <serial> arrived
after the wait ran out, not adding it`; and, at debug level, `device <serial> is not ready when the wait ends, terminating
it`. Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [C50](known-kotlin-bugs.md) and the
[x31 findings](wp/x31-findings.md).

**Migration action:** none needed. If a run that lost all its devices must fail sooner, lower `deviceInitializationTimeoutMillis`, which also bounds the wait for the first device.

## D49 — A lost adb request does not retire a device that is still there

In Kotlin, an adb request rejected or closed before its reply retires the device for the rest of the run. adb's list doesn't
change, so nothing ever connects the device again, however healthy it is.

Native builds first look. If the server still lists the device as `device` and the device answers a `getprop` with
`sys.boot_completed`, only the batch fails, and the device takes the next one. That batch's tests are charged one
attempt, as for any lost batch. A device gets this 3 times per adb server per run, and the count is never reset,
not even when its serial returns as a new device. The fourth loss retires it, as in Kotlin, with the log line `Device <serial> lost 3 requests already; not keeping it`.

A device that doesn't answer within `shell`, has restarted, or has left the list is retired at its first loss. So a dead device
that stays listed still leaves, and a loop can't form: at most 4 batches per device go this way, each charged once. With
`uncompletedTestRetryQuota: 0` the tests of each batch fail, where Kotlin failed those of one and then lost the device. The
same request lost because of a server outage or a brief `offline` is still [D22](#d22--a-run-survives-an-adb-server-restart) and
[D41](#d41--a-brief-offline-does-not-lose-the-device). A loss adds one `getprop` to the commands sent, and a healthy run sends the same commands.
Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [N15](known-kotlin-bugs.md) and the
[x31 findings](wp/x31-findings.md).

**Migration action:** none.

## D50 — A rejected test does not use a batch slot

Kotlin's `fixed-size` batching counts every test it polls. A flaky copy of a test already in the batch, or a test of
another test APK, is put back but has still used a slot and added its expected duration. Batches end early, and each extra
batch costs another instrumentation or xcodebuild start.

Native builds count only the tests a batch accepts, against both `size` and `durationMillis`. A batch may reject at most
2 × `size` tests, so it never scans a long run of another APK; at that limit it goes out short. The first test polled is
always accepted, and one test longer than the limit still makes a batch. Rejected tests go back after the batch, as before.

- A batch with nothing rejected performs the same queue operations as in Kotlin. So does a whole run with one test APK and no duplicate copies of a test in the queue.
- A batch can now be longer. It stops at its configured `size`, or once its expected durations add up to
  `durationMillis`, which are what Kotlin meant. The duration is a stopping point, not a cap: the test that reaches it is
  still in the batch, so two 4 s tests make an 8 s batch with a 6 s limit, as in Kotlin.
- Through 0.12.0 sorted tests kept their place; ties followed heap order after rejected tests were re-added. D60 now gives
  ties FIFO order, placing rejected tests after entries that were never polled.
- In 0.12.0, two APKs in listed order without sorting took 101 batches for 1,000 tests at size 10, with or without D50.
  D60 now fills 100. The sorted D50 comparison remains 100 instead of Kotlin's 150.
- The `class-name` and `isolate` batching rules are unchanged; D60 can change their queue-derived order and grouping.

Approved 2026-11-07 (project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[C33](known-kotlin-bugs.md) and the [x32 findings](wp/x32-findings.md).

**Migration action:** none. If you set `testBatchTimeoutMillis` close to what short batches took, check it, since batches
with flaky copies or several test APKs now hold more tests.

## D51 — Metrics and cleanup failures do not stop a run

Kotlin's Graphite tracker has no catch, so a carbon that refuses the connection throws into the queue actor at the first test
result, and the queue ends. In the port, the run then never ended. Reports and closes short-circuit too: one failure skips the
ones after it.

Native builds change four things:

- **A failing analytics backend** is logged once per kind of failure, at most eight kinds, and the run carries on with the
  same results and exit code. At the end of the run one line says how many results were lost. This covers Graphite and InfluxDB 2, whose error line per test it replaces. InfluxDB's own worker already swallowed its write failures, so they never reach it or the line at the end. A backend that fails once after taking 1 s or more loses up to 5 minutes of metrics, which also thins the history for later runs. A failed backend `close` still fails the run as before.
- **Time limits on a send.** Carbon gets 10 s to look up and connect and 10 s to write, where Kotlin has no limit. No send to
  any backend holds up the queue for more than 30 s. A send that fails after 1 s or more, or hits a limit, makes the next
  5 minutes of metrics get skipped, then the backend is tried again. A carbon that drops packets costs 10 s in every 310 s. A
  refused connection costs almost nothing, so it is tried for every test. The backend's close at the end of the run, where
  InfluxDB sends what it still holds, also gets 30 s, where Kotlin waits for as long as the server keeps answering. Past that
  one warning says those metrics are lost, InfluxDB's sender is stopped, and the run ends with the exit code it had. This can
  lose points from a working server too: a write that is slow but succeeds holds the sender while later points queue behind it.
- **A Graphite series printed as `None`** has no points in the window and is skipped with a debug line, so the query keeps its
  other tests. Kotlin discarded the whole query. Any other malformed value, such as `50,None`, still fails the query.
- **Each step at the end of a run still runs after another fails:** the history close, provider termination, each reporter
  and the tracker close. Each failure is logged, and the first is returned, so the exit code is what it was. A later finish
  attempt is still a no-op, and cancellation still stops the steps after it.

Separately, when filters take a non-empty list to nothing, a run logs an ERROR line with what each filter removed, so it shows with `debug: false` too, for example
`No tests left to run after filtering: blocklist simple-class-name removed 12`. It still exits 0, because an empty shard
fragment is legitimate. `parse` writes its empty list as before, with no warning.

Metrics from a working backend are byte for byte the same, except points still queued when a close runs out of time. A queue that ended for any other reason still hung the run
([U168](found-bugs.md)) until [D68](#d68--a-failed-queue-ends-the-run-a-failed-progress-line-does-not). Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [C62, C38, C11 and C25](known-kotlin-bugs.md) and
the [x34 findings](wp/x34-findings.md).

**Migration action:** none. Metrics lost to a backend that is down or slow show up in the log, and history falls back to its
defaults for those tests. Look for `Cannot send test metrics` if history seems to stop growing.

## D52 — A cancel writes the reports and cleans up, within a limit

Kotlin runs its finish on SIGINT or SIGTERM only with `debug: true`. With the usual `debug: false`, a cancelled CI job writes no
reports and doesn't terminate the provider, so worker processes can outlive the job. With `debug: true` the finish closes the
provider first, while the tests keep being handed out, and can be killed before it reaches the reports.

Native builds do the following on the first SIGINT or SIGTERM, in both debug modes, in this order:

1. **The tests stop.** Pools, queues and devices are cancelled, which also starts closing what the batches in flight have open.
   On a worker that means D30's TERM.
2. **The reports are written:** results reach them only up to this point, so nothing can change under them.
3. **Provider termination and the history close run side by side**, once the reports are done, or after 10 s if they aren't.
   The reports carry on, since only a remote backend's close comes after the report files and it may never end.
4. **The process exits with 128 + the signal number:** 130 or 143, as before.

**At most 25 s.** That is 24 s for all of it, and 1 s for the last lines of the log if the console has stopped reading. Whatever
is still running at 24 s is abandoned, with one error line. A CI's kill after SIGTERM often comes after 10 to 30 s. The reports
come first, so a shorter grace costs cleanup and not results, and 25 s fits the longer ones and leaves room for D30's TERM,
20 s and KILL. Without a stuck worker, a cancel takes well under a second.

**A second signal exits at once**, with 128 + its own number, however soon after the first it comes. It doesn't run anything
again, and what the first hadn't finished stays unfinished. Two of the same signal that arrive before the first has been taken
count as one, since a pending signal is recorded once, and then the cleanup runs as for one. So a wrapper that sends SIGTERM to
the process group and also forwards it may deliver one or two, and the cleanup runs or is skipped accordingly; the exit code is
right either way.

**What the reports hold.** Every result that had reached the queue. The test in flight is not in them, nor are the finished tests
of a batch still in flight. Its absence is what Kotlin's captures show, and it no longer races ([A99](known-kotlin-bugs.md)).

**When there's nothing to write.** Before a run wipes its output directory, that directory holds the last run's reports and
there are no results, so a cancel writes none. That covers a cancelled `parse` too. Kotlin's hook wrote empty reports over the old ones there.

**A signal during the run's own finish.** The end of a normal run closes the provider before the reports, which can take 30 s
per session. A signal in that time now writes the reports at once and waits for what's running, up to the limit. Nothing runs
twice. In Kotlin, and in the port until now, the finish found itself already started and the process exited mid-close.

**After the run.** Once a run is over the process exits within 3 s more, even if a blocking call such as a stuck name lookup is
still on a thread (D34). A signal is ignored in those 3 s.

With U161, tests no longer bounce: a batch on a closed adb client ended at once and quietly and came back, in a loop that took no
time, and every round added an INCOMPLETE to the reports. At `uncompletedTestRetryQuota: 1000` that was 1,001 false results.

No configuration field changed. The log gains one error line naming the signal, at ERROR because this logger shows nothing lower
with `debug: false`. Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [C10 and A99](known-kotlin-bugs.md),
[U161](found-bugs.md) and the [x35 findings](wp/x35-findings.md).

**Migration action:** none for most. A cancelled job now has reports in its output directory, so don't take their presence to
mean the run finished: the exit code says so. A process that used to die at once can take up to 25 s; a harness that waits
less than that should allow for it, or send a second signal. A test in flight on Android leaves `screenrecord` running until its
limit of 180 s, and its video on the device, as an exit always has.

## D53 — Remote parsing tries another device after a failed attempt

Kotlin parses a remote test list in up to three attempts within `deviceInitializationTimeoutMillis`, and each attempt borrows
a device at random (`Marathon.kt:131-143`). Nothing remembers the device an attempt failed on, so all three can land on one
that cannot parse, such as a simulator whose worker stalls its sends or answers with errors, or an Android device whose
instrumentation keeps failing, and the run fails although another device is healthy. With two devices and one broken, that
is one run in eight.

Native builds remember, for one parse, the devices its attempts failed on, and the next attempt picks among the available
devices it hasn't tried, with the same random choice over that smaller set. Only when every available device has been tried
does it pick among all of them, as before. It never waits for an untried device. That is not a promise that a parse takes no longer or ends with the same error: a
slower untried device can use more of the time, and the run can then end with the timeout's error where three quick
failures on one device ended with the parser's. A
device counts as tried when the parse on it failed; a failed borrow, or a cancellation, marks none. A device is known by the
key its provider borrows it by, so one that comes back as a new object, after a reconnect, is still the one that failed:
on Android its adb server and serial, as [D18](#d18--android-devices-are-keyed-by-adb-server-and-serial) tells them apart; on iOS the
simulator's key in the devices file; on macOS the Mac's serial number.

**macOS (U171).** The macOS provider never drops a Mac and has nothing to reconnect one, so a Mac whose connection was lost
stayed in its list and could be handed to the parse, which then failed an attempt on it. As on iOS with
[D44](#d44--a-lost-simulators-reconnect-has-a-limit), remote parsing now takes only a Mac whose connection is up. With none,
it waits, and `deviceInitializationTimeoutMillis` bounds that as for no devices at all: a run whose only Mac lost its
connection before the parse now fails with `Timed out waiting for a temporary device for remote test parsing` after that
limit, where it used to fail its three attempts at once.

**Unchanged:** the three attempts with no pause between them, the `deviceInitializationTimeoutMillis` bound, the rules for
which error the run reports, the `Parsed … tests` timing line, the commands sent to devices and workers, and a healthy run's logs and replay
choices: the smaller set of keys is offered only after a failed attempt. A run with one device makes its three attempts on
it, as before. No log line was added. **Limit:** a worker that freezes while the parse sends to it is caught by the
borrowed device's keep-alive, at about 75 to 80 s with the default `keepAliveInterval`. [D54](#d54--an-apple-send-stops-when-its-call-is-cancelled-and-any-transfer-when-its-devices-connection-is-lost)
then ends the send and fails the attempt, and the next one borrows another device within the default 180 s. With keep-alive
off nothing notices it, a stall is decided only after 285 s or more ([D31](#d31--an-rsync-send-that-stops-moving-fails)),
and the 180 s limit ends the parse first, with the timeout's error. To give that case a second attempt, keep keep-alive on
or raise `deviceInitializationTimeoutMillis` above about 320 s. (Until D54 the limit could not end a send at all: with the
default it failed the run at about 311 s, after a second attempt that started under the expired limit. See the
[x45 findings](wp/x45-findings.md).)

Approved 2026-11-08 (project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A103](known-kotlin-bugs.md), [U171](found-bugs.md) and the [x44 findings](wp/x44-findings.md).

**Migration action:** none needed.

## D54 — An Apple send stops when its call is cancelled, and any transfer when its device's connection is lost

Kotlin's rsync bridge blocks in `monitor` inside a suspend function, so nothing ends a transfer before rsync does: not the
cancellation of its caller, and not the loss of its device's connection. A send has no limit of its own ([A3](known-kotlin-bugs.md)),
so `deviceInitializationTimeoutMillis` cannot end a remote parse whose send is to a worker that has frozen, and the parse can
hang for ever ([A104](known-kotlin-bugs.md)). The port's D31 ended such a send after 305 s, too late for the default 180 s.

Native builds, on iOS and macOS:

- **A send ends when its call is cancelled.** rsync is stopped as [D31](#d31--an-rsync-send-that-stops-moving-fails) stops
  a stalled one, with the same signals and limits, but without the first 5 s, since rsync hasn't said anything and isn't on
  its way out: TERM to rsync and what it started, KILL 5 s later to what is left, and then the same for anything it left
  behind, also when rsync exits 0 on the TERM, which keeps its success, under `sshpass` too (see D31's note on `sshpass`). That takes at most 15 s, with 5 s for output pipes
  that such a child still holds, and up to 20 s more where `ps` is slow. The send returns as cancelled and logs nothing;
  its caller reports the cancellation as before.
- **A receive is never ended by its caller's cancellation.** Every receive collects an artifact, and its callers are
  cancelled around it on purpose: a screenshot's capture at the end of every test, iOS's artifact queue 300 s after the run,
  before a 60 s grace for what is still being pulled. A pull runs to its end then, as in Kotlin.
- **Any transfer ends when its device's own connection is lost**, the same way. That is the connection the provider
  watches, so it is the loss that the provider already acts on (D36, D44): keep-alive's, or the worker closing it. A send
  fails with `SSH connection failure: Lost the connection to <worker> during a transfer`, caused by the loss, which is not a
  cancellation, so remote parsing counts the device as tried ([D53](#d53--remote-parsing-tries-another-device-after-a-failed-attempt)).
  A receive returns `false`, which its listener deals with as for [D43](#d43--an-rsync-receive-has-a-limit-on-progress).
  Each logs one ERROR line on the bridge's logger: `send error: the device's connection was lost while sending to
  <host>:<path>` or `receive error: the device's connection was lost while receiving <host>:<path>`, with the loss as its
  cause. Closing the connection ourselves, as disposal does, is not a loss.
- **Waiting sends.** Sends to one worker go one at a time, and a simulator's send of a shared bundle waits while another
  simulator sends it to the same destination. A send that is waiting gives up its wait when its own call is cancelled or
  its own device's connection is lost, and returns as above; on a loss `SharedTransfers` logs an ERROR line, `Not sending
  <path> to <worker>:<destination>: the device's connection was lost while this one waited`. The send it waited for is left
  running, and nothing is recorded against the worker, so the sends behind a cancelled or lost one go on as if it had
  ended, where D31's stall fails them.

**Remote parsing with a frozen worker.** With the default `keepAliveInterval` of 15 s and `keepAliveMaxCount` of 4, the
borrowed simulator's connection is lost about 75 s after the worker froze. The parse's send to it then fails, and D53's next
attempt parses on another device, well within 180 s. With `keepAliveInterval: PT0S` nothing notices the frozen worker,
`deviceInitializationTimeoutMillis` ends the send at its limit, and the run fails with `Timed out waiting for a temporary
device for remote test parsing`. Measured on fake workers: the parse ended at 180 s and the run exited 1 at about 211 s, after
disposing the frozen simulator waited 30 s on its connection. It used to exit 1 at about 311 s with the default keep-alive,
and 346 s without it; with the default keep-alive it now exits 0 at about 91 s.

**What else ends sooner.** A prepare cancelled when its device is lost or its pool ends (D48), or by provider termination,
and D52's cancel used to leave a send running to its end and now stop it. They report what they reported before. A send
stopped part-way leaves rsync's partial file on the worker, and the next send of it transfers it again.

**Unchanged:** the rsync and ssh command lines, with no `ConnectTimeout` or `ServerAliveInterval` added (see the
[x26 findings](wp/x26-findings.md)), the commands sent to workers, every receive in a run whose connections stay up, and
a healthy run's logs and results: nothing cancels a send there. A transfer on a local worker is a copy, which is not stopped.

Approved 2026-11-08 (project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A104 and A3](known-kotlin-bugs.md), the [x45 findings](wp/x45-findings.md) and the [x46 findings](wp/x46-findings.md).

**Migration action:** none needed.

## D55 — Text uses Unicode characters

Kotlin and 0.12.0 could keep an unmatched UTF-16 surrogate in text until writing it,
when it usually became `?`. The native runner now replaces it with U+FFFD (`�`)
at decoding boundaries in escaped YAML/JSON, plists and other text inputs. Valid
surrogate pairs at UTF-16 decoding boundaries still form one supplementary character.
DEX retains Kotlin's historical UTF-8 interpretation of MUTF-8; D57 changes its
replacement counts. Annotation or descriptor slices that split a pair also use U+FFFD.

Sorting supplementary characters against U+E000–U+FFFF now follows Unicode scalar
order, rather than UTF-16 unit order. Diagnostic abbreviations stop at whole
characters. File/path limits still count UTF-16 units (see D56), Kotlin whitespace
and line splitting stay, and fragmentation hashes and Allure history IDs are unchanged
for valid Unicode.

Approved as D55 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.2](wp/8.2.md), its
[findings](wp/8.2-findings.md) and [test changes](wp/8.2-test-changes.md).

**Migration action:** if you match malformed text or depend on this rare sort order,
update those expectations. No action for ordinary valid text.

## D56 — Path limits keep complete characters

Kotlin's UTF-16 cut could leave half a supplementary character, such as an emoji.
In 0.12.0, a `maxPath` or `maxFilename` cut there failed with `InvalidInput` and the
file or folder was not written. The native runner now cuts before that character,
one UTF-16 unit short of the limit, so the shorter valid path can be written.
If dropping the character leaves an empty component, it still fails rather than
using the parent directory. Limits, normalization and directory/diagnostic ordering
are otherwise unchanged.

Approved as D56 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.3](wp/8.3.md) and its
[baseline probe and test changes](wp/8.3-test-changes.md).

**Migration action:** if you predict truncated artifact paths, allow the shorter
complete-character name at this boundary. Otherwise none needed.

## D57 — Native decoding, socket text and Basic credentials

Kotlin and 0.12.0 used the JDK's malformed UTF-8 replacement rules. Native decoding
can produce more U+FFFD characters: encoded surrogate bytes `ED A0 80` now produce
three, and truncated `ED A0` two, where the JDK produced one. This also affects
DEX's historical UTF-8 interpretation of MUTF-8 and malformed process output.

Connection diagnostics use native socket spelling, such as `127.0.0.1:5037` and
`[::1]:5037`, without Java's leading slash or expanded IPv6 spelling; a configured
DNS name is included where needed. Android device IDs and host text in result
data keep their old spelling. HTTP Basic authentication now base64-encodes UTF-8
`user:password`, instead of ISO-8859-1 with `?` for characters outside Latin-1.
ASCII credentials and URL form/query encoding are unchanged.

Approved as D57 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.3](wp/8.3.md) and its
[test changes](wp/8.3-test-changes.md), including separate native expectations.

**Migration action:** update socket-message and malformed-text matchers. If you use
non-ASCII Basic credentials for analytics, check that the server accepts UTF-8.

## D58 — Object and annotation values replace identity strings

Kotlin printed object identities such as `Class@hash`; 0.12.0 substituted stable
class-name hashes for stateless objects. Stateless configuration objects
in Allure `environment.xml` now use their simple class name, such as
`DisabledAnalytics`. Array annotation values use `[elements]`, with each element's
ordinary rendering, and decoded null uses `null`. Raw reports and diagnostic lines
use the same values, and cancellation job text drops its identity suffix.

The `annotationData` filter now matches that text: nested array contents and decoded
null can match; old identity patterns cannot. Existing outer-array element matching,
wrapper delimiter extraction and skipping unboxed null stay. Data-class rendering
is unchanged except that embedded stateless objects now use their simple names.
Device IDs, report paths and other data are unchanged by D58. This supersedes F29
and the identity part of D12; D59 separately changes exception names.

Approved as D58 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.5](wp/8.5.md), the
[reviewed golden diff](wp/8.5-golden-diff.md) and [test changes](wp/8.5-test-changes.md).

**Migration action:** update `environment.xml` and raw-metadata expectations, and
replace identity-based `annotationData.valueRegex` patterns with value patterns.

## D59 — Generated failures use plain descriptions

Kotlin and 0.12.0 printed Java exception class names in generated failure messages,
stack traces and diagnostics. The native runner uses descriptions, for example
`unexpected end of input: …` instead of `java.io.EOFException: …`. This reaches
JUnit, Allure, per-test JSON and HTML reports as well as logs. A missing message
leaves the description alone; cause order, frame text and empty messages stay.
External instrumentation, xcodebuild, xcresult and other tool text stays verbatim,
including any Java class names in it. Exit codes are unchanged by the wording change.

The hidden developer command `config-dump` replaces `error.class` with `error.kind`,
a stable identifier such as `config.rejected`. Cause strings use descriptions and
keep their existing `: null` convention for an absent message.

Approved as D59 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.6b](wp/8.6b.md), the
[reviewed golden diff](wp/8.6b-golden-diff.md) and [test changes](wp/8.6b-test-changes.md).

**Migration action:** update failure-text classifiers and snapshots; use structured
statuses where possible. Developer `config-dump` consumers must read `error.kind`.

## D60 — Equal-priority tests run in insertion order

Kotlin and 0.12.0 used Java heap order for ties and queue listings, and computed
sorting keys during comparisons. The native queue computes each entry's key on
insertion, breaks ties first-in, first-out and lists tests in priority order.
Duration and success-rate directions, NaN ties and signed-zero ordering stay.
Backend history stays pinned when a queue is built (D34); re-added tests can use
newer run-local duration observations while surviving entries keep their stored keys.
Random sorting draws once per distinct test at its first insertion, retaining its
cache across retries and queues, so the same seed can produce another order.

Execution order, class-name batch order, retry bookkeeping and queue listings can
change. With unchanged keys, batch membership changes where ties cross a batch
boundary; changed random keys or new duration observations on re-add can also move
tests between batches. The number of batch launches can change. Worker commands
change only in queue-derived test-selector order/grouping (`adb -e class` lists
and xcodebuild `-only-testing` selectors); executables, flags, non-test operands
and command construction stay. Real-device report order follows execution order.
The reviewed native goldens reorder selectors in six views, with the same batch
members and every report file unchanged; batching limits and configuration stay.

[D83](#d83--copies-of-different-tests-take-turns-in-the-queue) refines the insertion order: the copies of a test no
longer enter the queue next to each other.

Approved as D60 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.7](wp/8.7.md), the
[reviewed golden diff](wp/8.7-golden-diff.md) and [test changes](wp/8.7-test-changes.md).

**Migration action:** remove dependencies on heap tie order or old seeded sequences;
update selector-order and batch snapshots. Keep tests independent of execution order.

## D61 — CLI help and usage use clap

Kotlin and 0.12.0 reproduced Clikt's help, usage, suggestions and grouped errors,
including a fixed 79-column native layout. These texts now come from clap.
Commands, flags, defaults, boolean meaning, repeated-option handling and exit codes
stay. Root options still do not propagate to subcommands. Help stays on stdout;
usage errors stay on stderr. Parse help now names the actual `.yml` extension;
file naming itself is unchanged.

Approved as D61 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.10](wp/8.10.md) and its
[117-case CLI matrix and test changes](wp/8.10-test-changes.md).

**Migration action:** update help/usage snapshots and scripts matching parser error
text. Use exit codes to detect usage failures.

## D62 — Apple output idle limits have no polling margin

Kotlin and 0.12.0's Apple command reader added a 20 ms backoff margin and noticed
expiry on polling ticks: a 200 ms idle limit lasted 221–240 ms. It now expires
at the configured limit on the vendor scheduler clock, independently of wall-clock
jumps. Output arriving only within the removed margin can now time out; already
readable bytes still win over expiry.

Line splitting, the timeout kind/message, cancellation and terminal draining (D19)
stay. A stream that reaches EOF while its process lives still stays open until
process end or the idle limit (A25); a timeout still discards its partial tail (A24).
This supersedes the polling and grace-period part of D10.

Approved as D62 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.12](wp/8.12.md), its
[design](wp/8.12-design.md) and [test changes](wp/8.12-test-changes.md).

**Migration action:** if a very small Apple output idle limit relied on the extra
margin, raise that limit to cover the intended silence.

## D63 — Screenshots and GIFs use native image handling

Kotlin and 0.12.0 emulated ImageIO/AWT colour conversion, resizing, transparency and
GIF metadata. Framebuffers now share a straight-alpha RGBA decoder. Display P3 is
converted once to sRGB; sRGB and unknown samples pass through. Header-defined
16-bit layouts are honoured, RGB565 is little-endian in both adapters, and 32-bit
channels use their full advertised range with nearest rounding and opaque missing
alpha. This corrects flat-black raw 10-bit images (U190), wrong BGR565 colours
(U191) and the buffered profiled-RGB565 rejection (N22).

Nearest-neighbour resize ties can move edge pixels, P3 colours change, and explicit
sRGB loses the former one-level conversion noise. In GIFs only zero alpha is
transparent; nonzero alpha becomes opaque, with the exact source transparency mask
preserved after palette quantization. The loop extension is written once, removing
19 bytes per extra frame even when the pixels match. Ordinary opaque unprofiled
8-bit PNGs and opaque single-frame GIFs with unchanged sampling/geometry have exact
pre-change byte regressions; this does not promise unchanged bytes for all images.

Paths, names, worker commands and Allure/HTML references stay. Configured dimensions,
rotations (including C64), delay truncation/validation, loop value, disposal 0 and
frame order stay. The registered native goldens have no image change; supplemental
known-pixel, byte-hash and transparency tests cover the changed cases.

Approved as D63 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.16](wp/8.16.md), its
[findings](wp/8.16-findings.md), [golden audit](wp/8.16-golden-diff.md) and
[test changes](wp/8.16-test-changes.md).

**Migration action:** review and refresh image hashes or visual baselines for the
affected cases; allow different encoded bytes for multi-frame GIFs.

## D64 — Cancellation and completion lose unused JVM machinery

The Kotlin-coroutine adapter in 0.12.0 carried ordinary cancellation tags and
removable completion callbacks. The native runtime now has one ordinary
cancellation tag and retains callbacks until their exactly-once completion.
Late registration still runs immediately after completion. Internal debug text can
say `Requested` where it said `Parent`; those tags never reliably described origin.
The workspace-only `CompletionRegistration` API is removed; the crates are not
published for external consumption.

Failure payloads, timeout identity, propagation, supervisor isolation and device
cleanup ordering stay. Configuration, results, worker commands, pinned progress
and summary output, and exit codes are unchanged.

Approved as D64 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.18](wp/8.18.md) and its
[test changes](wp/8.18-test-changes.md).

**Migration action:** none needed for CLI users.

## D65 — Pools admitted during the final join are included

Kotlin's coroutine-child traversal, carried into 0.12.0, could miss a pool admitted
while the scheduler joined its final pool. A run could fail with `Expected to run …
but no events received`, or pass without that pool's tests. The scheduler now joins
each pool admitted before its first read of an absent next pool index and includes
only that joined prefix in its outcome. Receipt of a device event or creation of
an accumulator while history prefetch is pending is not yet pool admission.

A run can end later, include previously omitted tests, commands, progress and
result counts, and select a different existing exit status for the corrected
outcome. Exit-code policy, command construction and progress/summary text formats
stay. D48's per-pool wait and D52's cancellation bound still hold.

Completing parents now seal their outcome before callbacks run and reject new
work after sealing. Child storage and completion callbacks are separate; callback
and diagnostic ordering can move, and callbacks cannot change a sibling's already
prepared outcome. Registered native goldens and the configuration corpus stay
unchanged; focused CLI tests cover the corrected late-pool reports and status.

Approved as D65 in [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12).
See [WP 8.19](wp/8.19.md) and its
[test changes](wp/8.19-test-changes.md).

**Migration action:** none needed. If a late pool adds tests or changes the outcome,
check its results rather than relying on the earlier incomplete run.

## D66 — SIGQUIT prints the run's state and execution continues

The JVM handles Unix SIGQUIT by writing a thread dump and continuing. The native
runner previously kept the OS default: SIGQUIT killed it immediately, before a
CI wrapper's later SIGTERM could write reports through D52.

Native builds attempt to install a separate SIGQUIT listener before configuration
loading, unless SIGQUIT was inherited as ignored. An inherited ignore remains
ignored, as with D52's SIGINT/SIGTERM relay. If installation fails, the CLI logs
one WARN and continues with the previous OS behavior, including for `--help`.
With a handler installed, each observed delivery during a run queues one bounded
diagnostic at WARN on
`marathon_core::diagnostics`, visible with either debug setting. It uses the normal
console writer (stdout), including compact formatting, with an explicit Unix
millisecond timestamp in its header. Execution continues; SIGQUIT never requests
shutdown or counts as D52's first or second SIGINT/SIGTERM.

The dump shows scheduler phase and job state, pool and device-state totals,
queue counts, and devices preparing, idle, running a named batch, lost or
terminating. Running batches include elapsed state time and a bounded test list.
State ages are printed only for states dated by the device state machine, not for derived
lost, cancelling or completed states. Scheduler phase becomes `finished` on error
returns too. A panic while constructing a dump produces a WARN instead; the dump
task stays alive and execution is not cancelled. It reads existing core state without sending worker commands. Queue counts are
labelled as the last queue boundary; worker-affinity retry counts cover only
retries already represented in the existing worker map. Other pending retries
remain included in `tests_left`, without a separately tracked count.

Output is capped at 16 pools, 32 device details across those pools, eight tests
per running device, 160 bytes per identifier and 64 KiB per dump. Omitted counts
are printed. This is a state snapshot, not a native stack trace or worker-command
trace. Before a run and after it finishes, no dump is required; the handler stays
installed. During D52 shutdown it remains independent and cannot accelerate or
restart cleanup. Dumps are unthrottled; ordinary Unix signal coalescing still
applies. Non-Unix builds have no SIGQUIT listener.

A wrapper that signals the process group also ends local child commands such as
xcodebuild, as with the JVM build; remote commands over SSH and adb traffic are
unaffected. Local simctl commands and short-lived `adb start-server` processes
also inherit the process group and default SIGQUIT disposition. Their loss can
fail or retry a batch through the existing paths before the later SIGTERM.
Kotlin's `ProcessBuilder` children have the same behavior. Moving children into
their own process groups is out of scope.

Approved 2026-11-11 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x54 findings](wp/x54-findings.md).

**Migration action:** none needed. Wrappers may keep sending SIGQUIT, waiting,
then SIGTERM: they now get diagnostics followed by D52's reports and exit status.

## D67 — ordinary console completion has one 60-second deadline

Ordinary CLI completion previously reused the one-second signal flush budget
for its acknowledgement wait, then started another one-second drain wait.
Loaded CI and farm hosts could print `Console flush failed` despite a successful
command, and a later drain timeout could let the process exit with queued output
unwritten (U196).

The ordinary path now starts one 60-second deadline at the final flush (or at
guard shutdown if no flush ran). The acknowledgement wait and subsequent drain
both consume its remaining time. A recovering sink can finish without a spurious
diagnostic; a stalled console can now delay normal exit by up to 60 seconds,
including both waits together. Expiry and actual sink errors retain their
diagnostics and the command's exit status. Producers remain nonblocking and
queued output retains its order.

During the run, the first-signal watchdog stays at one second, the second
signal still exits immediately, and D66 SIGQUIT stays unchanged. After the run
finishes, SIGINT/SIGTERM are not acted on while the console output completes;
there is no second-signal escape. This window already existed for about two
seconds; D67 lengthens it to up to 60 seconds on a stalled console. There is no
configuration option. Result files, worker commands, pinned progress and summary
text stay unchanged.

Approved 2026-11-11 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x60 findings](wp/x60-findings.md).

**Migration action:** a wrapper needing a hard stop during post-run console
completion must use SIGKILL. SIGINT/SIGTERM cannot shorten that wait.

## D68 — A failed queue ends the run, a failed progress line does not

Each pool's queue hands out the batches and accounts for their results. Any
error out of its handler ended the queue; the pool read the closed queue as "no
work left", nothing ended the run, and CI had to kill it ([U168](found-bugs.md)).
D51 took analytics backends out of this path; a console that refused a progress
line, NaN progress, a failed clock read for a returned batch, a batching, bundle
or replay error and a failure at the Tracker seam could still cause it.

**A console failure costs the line, not the run.** When the console fails to take
the output flushed around a per-test progress line (either flush in
`test_ended` or `test_started`), that output is lost and the test is still
accounted for: its state changes and its result is tracked, so it is in the
reports. The first such failure in a run logs one WARN on
`marathon_core::execution::progress` (visible with `debug: true`); later ones are
silent. A cancelled write still stops the queue, as cancellation does
everywhere. The run's own flushes, D67's final deadline and the signal path are
unchanged.

**Any other queue error ends the run with its cause.** Every pool is stopped,
the failed one included, and the batches in flight are cancelled as a D52
cancel does; this is the policy a failed pool already had. It holds also when
the failed queue's own pool had already closed, for example because its last
device had left and it read the failed queue as having no work. The Scheduler
then returns the first such error instead of reading the pools' progress, so
such a run never reports success. The run finishes as any failed run: the reports hold every result that reached a
queue, the provider is terminated, `Marathon run failed` is logged with the
cause, and the exit status is the existing one for a failed run: 1, or 0 with
`ignoreFailures: true`. There is no new timeout: the devices stop as at a normal
drain or a cancel. In the stub witness the run ends at the moment of the error,
with a batch running in the failed pool and another pool still preparing its
device.

Runs that completed are unchanged: result files, worker commands, the pinned
progress and summary text and exit codes. Approved 2026-11-15 (project owner's
decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and the [x61 findings](wp/x61-findings.md).

**Migration action:** none. A run that used to hang now exits 1; its cause is
in the `Marathon run failed` line. A missing progress line with a
`Cannot write progress to the console` warning means the console refused it.

## D69 — A finished test is not run again

Four Kotlin queue bugs ran tests after they had finished, or never finished
their accounting:

- Under fast ALL_SUCCESS (strict mode), a failure that is granted a retry also
  completes the test, yet the retry was queued and ran again, and its result
  was tracked as not final ([C45](known-kotlin-bugs.md)).
- When a batch held two failed copies of one test (class-name batching with a
  flaky copy) and only one was retried, the other was never ended: no progress
  line, no tracked result, and the test kept an extra total
  ([C46](known-kotlin-bugs.md)).
- A batch rejected by a device that left before starting it was queued again
  whole, including copies of a test that another device had completed
  meanwhile; they ran again ([C47](known-kotlin-bugs.md)).
- A copy that came back incomplete (its batch returned, or the test not run in
  a completed batch) was queued again even when another copy had completed the
  test meanwhile, so the finished test ran again ([C86](known-kotlin-bugs.md)).

Native builds:

- do not queue a retry whose attempt leaves the test finished, also when an
  earlier copy in the same batch finished it. That attempt leaves the test's
  totals and the total retry quota it took is given back. The per-test limit
  counts failures, not retries, and is unchanged.
- tell retried results apart by their position in the batch, so every failed
  copy is ended exactly once.
- drop a rejected batch's copies of finished tests from the queue and from the
  totals, and do not restore their retry workers. A retry dropped this way
  keeps its quota charge. The other copies are queued again as before, in
  [D60](#d60--equal-priority-tests-run-in-insertion-order) order.
- do not queue again an incomplete copy of a finished test. Within
  `uncompletedTestRetryQuota` it is still reported once, as a result that is
  not final, and it leaves the test's totals; its device is not marked as one
  to keep the test's retries away from. Once that quota is used up such a copy
  is reported as before, once, as a non-final failure, and keeps its total; it
  was not queued again there either. An incomplete copy of a test that has not
  finished is queued again as before.

A copy that is already running elsewhere when its test finishes still runs and
is reported as a non-final result, as before. The phantom total of
[C5](known-kotlin-bugs.md) is unchanged.

Reports can hold fewer executions and fewer non-final results for strict-mode
retries, flaky copies in class-name batches, rejected batches and copies that
came back incomplete after their test finished, and such runs end sooner. Progress percentages follow the exact totals. A refunded retry can
let another test of the pool be retried, which can change that test's outcome.
None of the registered native goldens change. Approved 2026-11-15 (project
owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and the [x61 findings](wp/x61-findings.md).

**Migration action:** none. Execution counts in reports and dashboards can drop
for the runs above.

## D70 — An adb outage does not charge or break tests that never started

Kotlin has no outage handling; D22 and D41 added it, and left two gaps. A batch that a device took while an adb server
outage or a brief `offline` kept it waiting came back as uncompleted when the device was given up, and each of its tests
was charged one attempt although none had run. With `uncompletedTestRetryQuota: 0` those tests failed without running
([U160](found-bugs.md)). And in a batch of several instrumentations ([D28](#d28--each-instrumentation-runs-only-its-own-tests))
only the first waited: the next one started on the dead server or the offline device and failed ([U166](found-bugs.md)).

Native builds now do the following, with no configuration change:

- **Each instrumentation waits.** Every instrumentation of a batch waits for the server to return or the grace period to
  end, as the batch's first already did, before any of its device commands and outside `testBatchTimeoutMillis`. Then it
  runs. A cancel ([D52](#d52--a-cancel-writes-the-reports-and-cleans-up-within-a-limit)) ends the wait.
- **A batch that never started goes back uncharged.** Starting means sending an instrumentation, which is recorded when it
  is sent: the lack of test results does not prove that nothing ran. A device given up while it holds a batch none of
  whose instrumentations was sent gives the batch back to the queue as one that never reached the device. Its tests run
  later, on whichever device comes, with their retry counts as they were.
- **A batch partly run keeps what it ran.** A device given up between two instrumentations of a batch ends it with the
  results of those that ran, as when the device is lost there: their finished tests count and are not run again, and the
  tests of the instrumentations that never started are uncompleted and charged one attempt. The whole batch used to come
  back, so its finished tests ran again.
- **Nothing more starts on a device given up.** A batch waiting on a device that has been given up waits until the run
  stops the device, instead of starting on it and failing.
- **It stays bounded.** Only a device that leaves gives a batch back uncharged. A device that stays and keeps failing
  before it starts its batch is charged as before, so its retries still run out. A batch given back is not a test that
  ended, so [D48](#d48--a-pool-that-lost-its-last-device-waits-for-one-to-return)'s wait goes on: a device that keeps
  coming back and leaving before it starts its batch still ends the pool at the limit.
- **A batch that has started keeps what it ran, wherever the device leaves it** (extended 2026-11-17,
  [U197](found-bugs.md)). A device given up while an instrumentation runs, or between two instrumentations away from the
  gate, also ends its batch with what ran so far: finished and failed tests count, and the test in progress and those
  not yet run are uncompleted and charged one attempt. That is what the runner reports when it sees the loss first. When
  the run heard of the loss before the runner did, the whole batch used to come back, so its finished tests ran again
  and all its tests were charged; the Kotlin marathon has the same race. Nothing more of such a batch is sent to the
  device. The test in progress ends when the batch does, as a failed run ends it. A test whose end the listeners are
  still handling at that moment counts with its status, but may lack the screen capture and profiling files attached
  as it ends.

Once an instrumentation has been sent its batch is charged as before, even if the device leaves a moment later: its
tests that had not finished are uncompleted, never given back uncharged. A healthy run sends the same commands, in the
same order, and logs the same lines.
After an outage or a grace period between two instrumentations of a batch, the next instrumentation's commands come once
the device is back, where they used to come during the outage and fail. The reports of a run with an outage hold fewer
uncompleted attempts, and a run with a small `uncompletedTestRetryQuota` can pass where it failed. New debug lines:
`Batch <id> waits for its adb server or device before its next instrumentation`, `Rejecting batch <id>: device <serial>
left before starting it` and `pool <pool>: device <serial> left before starting test batch <id>`; with the extension,
`Batch <id> has no results so far; accounted for as before`, only when those results cannot be read. Approved 2026-11-15
(project owner's decision); the extension for a batch that has started, 2026-11-17 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [U160, U166 and U197](found-bugs.md), and the
[x63](wp/x63-findings.md) and [x68](wp/x68-findings.md) findings.

**Migration action:** none.

## D71 — Unavailable test history adds no probability copies

With `flakinessStrategy: probability`, Kotlin gives a test up-front copies when its success rate is below
`minSuccessRate`, and it reads a success rate of 0 whenever there is no history to read: analytics disabled, a
backend that could not be set up, and a success-rate query that failed (C8). Every test then gets `maxCount` copies,
so an analytics outage runs the suite up to `maxCount + 1` times.

Native builds tell history that cannot be read from history that has no row for a test. The view a flakiness pass
pins ([D34](#d34--a-failed-history-query-is-not-final)) records whether its success-rate query answered. When
analytics is disabled, its backend could not be set up, or the query failed and no refresh has answered it since,
probability adds no copies for any test, and one WARN per run says why, for example:

```text
Test history unavailable (the success rate query failed); flakinessStrategy probability adds no copies
```

The other reasons read `analytics is disabled` and `the analytics backend could not be set up`. A cancelled query
cancels the run, as before. A working backend behaves as before: an empty window, partial history and tests missing from
it take `analyticsConfiguration.defaults.successRate` (0.0 unless configured), so new tests still get copies, and a
genuine 0% rate still does. A shard is still judged by one view, so a refresh that lands later reaches only views taken
after it. Sorting and batching by history, and the duration defaults, are unchanged. A Graphite backend that cannot
be set up still fails the run, as before. Retries are unchanged.

Approved 2026-11-15 (project owner's decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[C8](known-kotlin-bugs.md) and the [x62 findings](wp/x62-findings.md).

**Migration action:** a configuration that relied on disabled analytics with probability to copy every test must
switch to [`type: fixed`](#d72--flakinessstrategy-fixed-for-stabilization-jobs). Otherwise none: with a working
backend nothing changes.

## D72 — `flakinessStrategy: fixed` for stabilization jobs

Kotlin has two flakiness strategies, `ignore` and `probability`, so a job that wants every test run a set number of
times has to rely on history, and it rejects both with `ALL_SUCCESS`. Native builds add a third, which the Kotlin
marathon rejects, and allow it with `ALL_SUCCESS`:

```yaml
flakinessStrategy:
  type: fixed
  count: 4            # each test is scheduled 5 times
executionStrategy:
  mode: ALL_SUCCESS   # a test passes only if all 5 runs pass
  fast: true          # its first failure ends it and drops the copies left
retryStrategy:
  type: no-retry      # ALL_SUCCESS takes no retries
uncompletedTestRetryQuota: 5
```

Every test of the shard gets exactly `count` up-front copies, grouped per test in shard order as probability's are,
whatever analytics says, so each test is scheduled `count + 1` times; `count: 0` adds none. `count` is required and
binds as the other integer fields do; a negative value is rejected with `flakinessStrategy.count must be at least 0,
got -1`, and unknown fields are rejected. As for probability, `fixed` cannot be combined with `count` sharding,
and retry quotas apply on top of the copies where retries are allowed. `parse --include-flaky-tests`, `config-dump`
and the Allure environment (`FixedFlakinessStrategyConfiguration(count=4)`) show it.

**With `ALL_SUCCESS`** a test passes only if all its runs pass, and the run exits 1 otherwise. `ALL_SUCCESS` still
requires `retryStrategy: no-retry`, and still rejects `probability` with the message it always had. With `fast: true`
(the default) a test's first failure ends it: its copies still queued are dropped, that failure is its final result,
and the summary, result files and exit code agree. With `fast: false` every run executes; since
[D77](#d77--the-final-result-of-a-test-agrees-with-its-verdict) its first failure is still its final result, so the
outputs agree too (before D77 they followed the last run, [C87](known-kotlin-bugs.md)). The summary's `Raw:` line and
failed-tests list count every run.

**With `ANY_SUCCESS`** a test passes if any run passes. Set `fast: false` to run every copy after a pass and read the
`Raw:` line for how many runs failed; with `fast: true` the first pass drops the copies left, as it does for
probability. With `fast: false` the first pass is the final result of a test that passes (D77; before it, C87 listed
a test whose last run failed as failed although the run exits 0).

Approved 2026-11-15 (project owner's decision), with `ALL_SUCCESS` allowed by a further decision the same day. See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the [x62 findings](wp/x62-findings.md).

**Migration action:** none for existing configurations, which keep their meaning. A Marathonfile that uses `fixed`
cannot be read by the Kotlin marathon or by 0.13.0; remove it before rolling back to either.

## D74 — An Apple device's preparation and connections end with it

**Preparation (iOS).** In Kotlin a simulator's preparation, once its caller has stopped waiting for it, carries on in the
background, and disposing of the simulator neither stops it nor waits for it. A probe stuck in a stopped preparation could
therefore go on sending commands to a simulator that had been disposed of, or that a recovery was setting up again, until the
run ended ([U179](found-bugs.md)). Native builds stop it: disposing of a simulator cancels its preparation and waits for it to
end, together with a cancelled batch and within the same limit of 70 s. A preparation that has not ended by then is left
behind with one warning, and the disposal carries on. A stopped preparation sends no further command to the worker, and a
simulator that is being disposed of starts no new preparation.

**Connections (iOS and macOS).** Each Apple device holds a lease on its worker's SSH connection. Kotlin leaves some of them
open until the process exits: an iOS simulator replaced after its worker was lost ([A90](known-kotlin-bugs.md)), any iOS
simulator whose disposal failed at the end of the run ([A82](known-kotlin-bugs.md)), and on macOS a Mac whose setup failed and a
Mac that was replaced ([A89](known-kotlin-bugs.md)). Native builds release every lease once, after the device's disposal,
whether or not the disposal failed: on iOS at the end of the run, as before for the others; on macOS at once for a failed
setup, after its disposal for a replaced Mac, and at the end of the run for the rest. Each lease has one owner that
releases it: when a new iOS simulator's registration fails because its predecessor's disposal failed, the setup that
created it releases its lease and the end of the run does not (Kotlin releases it twice, and with `shareWorkerConnection`
the second release takes another simulator's use of the connection); a repeated macOS termination after a fatal disposal
releases nothing again. With `shareWorkerConnection` a
connection still closes only when its last user has released it. The errors reported stay those of the setup or disposal; a
release that fails is logged.

**What changes for a run.** A preparation stopped by its simulator's disposal no longer sends the commands it had left, and
those connections close when their device is done instead of at exit. Disposal can take up to 70 s longer when a stopped
preparation is stuck in a command that does not end; a command on a worker ends on cancellation, so in practice this is the
time a session close takes ([D30](#d30--closing-an-ssh-session-has-a-limit)). A cancel still ends within
[D52](#d52--a-cancel-writes-the-reports-and-cleans-up-within-a-limit)'s 25 s. Result files, the Marathonfile and the CLI are
unchanged, and no configuration field is added.

Approved 2026-11-15 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the [x65 findings](wp/x65-findings.md).

**Migration action:** none.

## D75 — a JUnit3 test is listed once

The test parser of Kotlin marathon (dex-test-parser 2.3.4) finds JUnit3 tests by scanning
the test APK's dex files several times, dropping the last dex each time, and merges the
results as a set. Annotation values that are arrays compare by identity there, and every
scan builds new arrays, so a JUnit3 test with an array-valued annotation is listed once per
scan that includes its dex file. Kotlin's `@Throws`, which compiles to
`dalvik.annotation.Throws`, is the usual case: in an APK with two dex files such a test in
the first dex is listed, and run, twice, and one only in the second dex once (N11).

The native parser keeps the scans and merges their results by content: the test name and
its annotations, with arrays compared element by element. The first-seen entry and order
are kept. Each test is listed and run once. JUnit4 discovery is unchanged, and so is the
case of a method found by both the JUnit3 and the JUnit4 rules.

The differential test still compares against the JVM recordings: for
`abstract-class-in-second-dex.apk` it drops exactly the 8 recorded repeats, and any other
APK must have none.

Approved 2026-11-15 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x66 findings](wp/x66-findings.md).

**Migration action:** none. A multi-dex test APK whose JUnit3 tests carry array-valued
annotations runs each of them once, so its test count, batches and run time can drop.

## D76 — a hosted xctest inside the app is not pushed twice

For a hosted XCTest bundle, Kotlin marathon pushes the app under test to the worker and
then pushes the xctest to `<app>/PlugIns/<name>` there. It meant to skip the second push
when the xctest already sits in the app's `PlugIns`, as Xcode builds it, but compares a
`File` to a `Path`, which is never equal (A9).

The native factory skips that push when the xctest's real path is the app's real path
followed by `PlugIns/<name>`, the place the app push already filled. A bundle elsewhere,
a symlinked bundle or `PlugIns` (the app push carries those as links), a macOS bundle
under `Contents/PlugIns`, or a path that cannot be resolved is pushed as before. The
files on the worker and the generated xctestrun are the same as before; the worker sees
one rsync fewer per setup.

Approved 2026-11-15 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x66 findings](wp/x66-findings.md).

**Migration action:** none. A wrapper or log check that counts worker transfers sees one
fewer for an iOS unit-test bundle built inside its app.

## D77 — The final result of a test agrees with its verdict

A test with several runs (retries, flaky copies, uncompleted re-runs) has one result tracked as final: it is the
test's line in the summary's first block (`N passed, N failed, N ignored tests` and the `Failed tests:` under it),
its per-test result file, its JUnit test case and its HTML entry. In the Kotlin marathon it is the run whose event
completes the test, with that run's own status (C87). With `executionStrategy.fast: true` (the default) the run
that decides the verdict completes the test, so the two agree. With `fast: false` the last run completes it, so:

- under `ANY_SUCCESS` a test that passed once and failed on its last run passes (exit 0) but is listed as failed,
  with a FAILURE result file and a JUnit `<failure>`;
- under `ALL_SUCCESS` (reachable with copies since [D72](#d72--flakinessstrategy-fixed-for-stabilization-jobs)) a
  test that failed once and passed on its last run fails (exit 1) but is listed as passed.

The native run tracks as final the run `fast: true` would complete the test on for the same runs. That is the
first pass under `ANY_SUCCESS` and the first failure under `ALL_SUCCESS`; when no run decides early (every run
failed under `ANY_SUCCESS`, every run passed under `ALL_SUCCESS`) it is the run that completes the test, as before.
A final INCOMPLETE (tracked as FAILURE) is the final result only when no earlier run decided the verdict, as with
`fast: true`. A test whose runs were not all reported (the end-of-run `Expected to run ... more` error) now has a
final result when an earlier run decided its verdict; before, it had none. So `fast: true` and `fast: false` write the same final
record for the same runs and differ only in how many runs happen. `fast: true` is unchanged.

```text
executionStrategy: {mode: ANY_SUCCESS, fast: false}, runs FAILURE PASSED FAILURE
Kotlin:  0 passed, 1 failed  (exit 0)     native:  1 passed, 0 failed  (exit 0)
executionStrategy: {mode: ALL_SUCCESS, fast: false}, runs PASSED FAILURE PASSED
Kotlin:  1 passed, 0 failed  (exit 1)     native:  0 passed, 1 failed  (exit 1)
```

What changes, only with `fast: false` and only for a test with several runs whose verdict an earlier run decided:
the summary's first block and its `Flakiness overhead`, the per-test result file (status, timings, batch, device and
so its directory, attachments), the JUnit test case (its status, `time`, and which runs appear as `flakyFailure` or
`rerunFailure`) and the HTML report. Under `ANY_SUCCESS` this includes a test whose runs all passed: its first run is
now the final one, not its last. A test's case can move in the JUnit and HTML order, which follows when its final
result was tracked.

What does not change: the exit code and `Marathon execution failed`, the queue, retries and copies, the `Raw:` line
and its failed and incomplete lists, `raw.json`, Allure (every run), the timeline, device bills and device files,
the analytics backends (every run is sent), the per-test progress lines, and every output of `fast: true`.

Evidence: `flakiness_scenarios::d77_any_success_without_fast_tracks_the_first_pass_as_final` and
`d72_fixed_under_all_success_without_fast_runs_every_copy_and_fails` (the two runs above, end to end on the
Stub vendor), `d77_fast_and_not_fast_track_the_same_final_record`, and
`pool_progress::d77_the_final_run_is_the_one_fast_completes_the_test_on`. The JVM progress and queue recordings
are compared with a D77 allowance that moves only the final flag of tracked runs in `fast: false` rows
(`progress_oracle::d77_moves_only_the_final_flag_of_non_fast_rows`,
`queue_oracle::d77_moves_only_final_flags_of_non_fast_recordings`).

Approved 2026-11-17 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x70 findings](wp/x70-findings.md).

**Migration action:** none. A job that sets `fast: false` and reads the summary's first block, the per-test result
files or JUnit now sees each test as the exit code does. A tool that took the final record as a test's last run
should read `raw.json`, which lists every run.

## D78 — JUnit lists only the runs of a passed test that did not pass

In the JUnit report (`tests/<pool>/marathon_junit_report.xml`) a test's case can list its other runs: a test that
passed lists them as `flakyFailure`, a test that failed as `rerunFailure`, each with the run's stack trace. In the
Surefire convention JUnit tools follow, a `flakyFailure` is a failed run of a test that finally passed, and a
`rerunFailure` a failed rerun of a test that finally failed. The Kotlin marathon filters a failed test's runs that way,
but lists every other run of a passed test, passing runs included, as an empty `flakyFailure` (incomplete runs are left
out in both cases) ([C88](known-kotlin-bugs.md)). A test that passed all three runs of
`flakinessStrategy: {type: fixed, count: 2}` ([D72](#d72--flakinessstrategy-fixed-for-stabilization-jobs)), or all
its probability copies, then has two `flakyFailure` elements, and a CI tool that counts them reports a stable test as
flaky twice.

Native builds list as `flakyFailure` only the runs of a passed test that did not pass, each with its stack trace as
before. A run is listed when its status is failure, ignored or assumption failure, the same rule the `rerunFailure`
list of a failed test has always used; a passing run is not listed.

```text
runs PASSED PASSED PASSED (fixed, count: 2)
Kotlin:  <testcase ...><flakyFailure type="rerun exception"></flakyFailure>
                       <flakyFailure type="rerun exception"></flakyFailure></testcase>
native:  <testcase ...></testcase>
runs FAILURE PASSED PASSED
Kotlin:  two flakyFailure elements, the first with the failure's trace
native:  one flakyFailure element, with the failure's trace
```

What changes: only the number of `flakyFailure` elements in the JUnit case of a passed test that had passing runs
besides its final one.

What does not change: a failed test's `<failure>` and `rerunFailure` elements, skipped and incomplete handling, the
order and attributes of test cases, the suite's `tests`, `failures`, `skipped` and `time`, `raw.json`, the per-test
result files, Allure (every run), the HTML report, the timeline and the summary, including its `Raw:` line.

Evidence: `junit_reporter::passed_test_whose_copies_all_passed_has_no_flaky_failure`,
`passed_test_with_one_failed_copy_has_exactly_that_flaky_failure` and
`failed_test_with_mixed_copies_has_rerun_failure_only_for_failed_runs`. The JVM reporter recordings are compared with
a D78 allowance that removes only the `flakyFailure` elements of passing runs, in exactly three rows
(`reporters::d78_allowance_removes_only_the_flaky_failures_of_passing_runs`,
`d78_allowance_rejects_a_recording_that_does_not_match_its_runs`).

Approved 2026-11-17 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x75 findings](wp/x75-findings.md).

**Migration action:** a CI step or dashboard that counted `flakyFailure` elements as the number of extra runs of a
passed test must read `test_result/raw.json` instead, which lists every run. A step that reads them as failed runs, as
JUnit tools do, needs no change.

## D79 — A simulator shut down during its boot wait is booted again at once

When marathon prepares a simulator it boots it and waits up to `timeoutConfiguration.bootWait` (240 s by default),
polling it every second. Two attempts are made, 10 s apart. In the Kotlin marathon the wait ends only when the
simulator is booted or `bootWait` has run out. A simulator that is shut down during that wait (`xcrun simctl shutdown`
or a crash) never boots by itself, so it was polled
for the rest of `bootWait` and only then booted again by the second attempt: 4 minutes lost with the defaults
([A108](known-kotlin-bugs.md)).

The native boot wait remembers whether it has read the simulator as booting (its device.plist state 3). When a later poll
reads it as Shutdown, the attempt ends there as a failed boot, logged as

```text
WARN … Simulator <udid> on worker <id> failed to boot: it shut down while booting after <ms> ms, attempt 1 of 2
```

and the existing retry follows: device.plist already says Shutdown, so no `simctl shutdown` is sent, and after the
10 s retry pause `simctl boot` boots it again. If the second attempt also fails the simulator leaves its pool, as
before, with the same reason.

What does not change:

- a Shutdown read before the first booting read keeps the Kotlin wait, so a device.plist that still says Shutdown just
  after `simctl boot` returns, or a simulator still being created, does not end the attempt early;
- a simulator that stays booting, including one whose readiness probe keeps failing (D38, D46), is still waited for
  until `bootWait`, so the D38 times in the changelog's known limits are unchanged;
- the number of attempts, the retry pause, the commands sent (only their timing), and D74's stop when the simulator
  is disposed of during its preparation.

Evidence: a real 0.14.0-rc.1 run on a local iOS 26.4 simulator shut down at the first poll of its boot wait logged
237 polls over 240 s, then `it has not booted within 240 s, its state is SHUTDOWN after 240010 ms, attempt 1 of 2`,
and booted on the second attempt 15 s later. The tests
`simulator_prepare::a_simulator_shut_down_during_its_boot_wait_is_booted_again_after_one_poll_d79` (the reboot
follows the shutdown within one poll interval plus the retry pause, with a 60 s `bootWait`),
`a_simulator_that_stays_booting_is_still_waited_for_until_boot_wait_d79` and
`a_shutdown_state_before_the_first_booting_read_does_not_end_the_boot_wait_d79` pin the new and the kept behaviour.

Approved 2026-11-17 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x78 findings](wp/x78-findings.md).

**Migration action:** none. A simulator that is shut down while marathon boots it now costs about 10 s, not
`bootWait`. Do not shut simulators down on purpose during a run to make marathon reboot them: each such shutdown still
uses one of the two attempts.

## D80 — A slow video transfer doesn't cost the next test its video or its time

With iOS video recording on, the Kotlin marathon handles the end of a test by stopping the recorder on the worker,
transcoding the video when transcoding is on, pulling it and removing it, all inside the listener callback. The
xcodebuild events of a batch reach the listeners one at a time, so the next test's start and end wait for all of that.
The next test's recording starts only when its start is handled, and its result is timed from when its start and end
were handled ([U201](found-bugs.md)). On a slow link the next test can run to its end before its start is handled: it
gets no video, or one cut at the start, and a duration of 0 in its result file, JUnit `time`, `raw.json` and the HTML
report. A real remote run of 0.14.0-rc.1 lost the second test's video and reported 0.0 s for a 7.7 s test: the first
test's recorder took 3.4 s to stop, its transcode 4.5 s and its pull 14.5 s. Even with a fast pull, the stop alone
shortened every following test's reported time by about 3 s on that worker.

Native builds handle a test's end without waiting for the worker:

- The recorder of the test that ended is stopped by a task of its own. The next test's recording starts after that
  stop, as it did before, so one recording runs at a time and the "Previous video recording didn't finish yet" warning
  keeps its meaning. A test that runs again in the same batch records to the same remote file, so its new recording
  also waits until its earlier video has been collected, as it did before.
- The transcode, pull and removal follow the stop in a queue per batch, one test at a time in the order the tests ended,
  beside the batch's later tests.
- Before the batch's results are built, `after_test_run` waits for the queue, so every video is attached to its test
  as before; the xcresult pull still comes after the videos, and the device takes no other batch before they are done.
- A failure of the stop or pull other than a transfer failure ("Can't pull video", unchanged), including an internal
  error of that work, no longer ends the batch's event handling at that test; the batch fails from `after_test_run`
  with it, after its other listeners have run.
- A batch that is cancelled, or whose simulator is disposed of, starts none of its queued items and no recording that
  was still waiting for the previous test's stop.
- A transfer already running is not ended by that cancellation, as an inline one was not. Its worker commands end with
  the batch, but a pull ends only when it finishes, when its device's connection is lost
  ([D54](#d54--an-apple-send-stops-when-its-call-is-cancelled-and-any-transfer-when-its-devices-connection-is-lost)),
  or after 300 s without progress ([D43](#d43--an-rsync-receive-has-a-limit-on-progress), about 305 to 330 s with its
  teardown). [D74](#d74--an-apple-devices-preparation-and-connections-end-with-it) and
  [D52](#d52--a-cancel-writes-the-reports-and-cleans-up-within-a-limit) bound how long a cancel or a disposal waits for
  the batch, not the pull: a pull still running at that limit carries on in the background, as an inline one did.

What changes for a run: on a slow worker the tests after the first keep their videos and their durations. The worker
commands are the same; the stop, transcode and pull of one test can now come after the next test's recording command.
A batch that fails at the end of a test, or a batch-failure video, is unchanged. Screenshot recording (GIF) and macOS
have no transfer at a test's end and are unchanged; Android pulls its videos over adb and keeps Kotlin's order.

Evidence: `simulator_video_pull::a_slow_video_pull_does_not_hold_back_the_next_test_d80` (the first pull held until the
second test has run 7.7 s and passed) and `a_slow_recording_stop_does_not_hold_back_the_next_test_d80` (the first stop
held the same way) fail without the change with a duration of 0 ms;
`a_recording_waiting_for_the_previous_stop_never_starts_after_disposal_d80`,
`a_repeated_test_records_again_only_after_its_first_video_is_collected_d80`,
`a_cancelled_batch_skips_queued_video_work_d80`, and `pending_artifacts` for a panicking transfer; the vendor
differential replays pass unchanged.

Approved 2026-11-17 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x76 findings](wp/x76-findings.md).

**Migration action:** none. A job that checked iOS test durations or video attachments sees them for every test of a
batch, not only for those after a fast transfer.

## D81 — Android code coverage needs no `run-as`

With `isCodeCoverageEnabled: true` the Kotlin marathon has the instrumentation write
`/data/data/<app>/coverage/coverage-<batch>.ec`, creates that folder with `run-as <app> mkdir` before each batch, and
copies it out with `run-as` afterwards (an APP_DATA file-sync entry). `run-as` only works for a debuggable app. For a
non-debuggable one the folder is never created, the test runner cannot open the file, nothing is pulled, and nothing is
logged above DEBUG: the run passes and has no coverage (C89).

The native run writes coverage to the app's own folder in shared storage, which the app can write without permission
and the shell can read:

```text
coverageFile  <external storage>/Android/data/<app>/files/coverage/coverage-<batch>.ec
pulled to     <outputDir>/device-files/<pool>/coverage/coverage-<batch>.ec
```

- **Before each instrumentation**, after `pm clear` and after file sync has prepared its pull folders (either could
  remove it), one shell command removes the folder,
  creates it and opens it to the app:
  `rm -rf <dir>; mkdir -p <dir> && { chmod o+rwx <app dir> <app dir>/files <dir> 2>/dev/null; true; }`. A folder the
  shell creates there belongs to the shell with mode 2770 and group `ext_data_rw`; `o+rwx` lets the app in and the
  set-group-ID bit gives the app's file that group, which the shell has. On API 29 and older, where shared storage
  derives its permissions, the `chmod` changes nothing and is not needed.
- **After each instrumentation that was sent**, the file is pulled over sync, the folder is removed from the device,
  and the file is attached to every test the instrumentation started, as an attachment of type `COVERAGE` named
  `coverage`. The per-test result file (`test_result/<pool>/<serial>/<test>.json`) lists it under `attachments`, next to
  its `testBatchId`, as the absolute path of the pulled file on the host, as for every attachment (never the device
  path); Allure lists it as well. A parameterised test's merged result gets it too.
- **A batch with several instrumentations** ([D28](#d28--each-instrumentation-runs-only-its-own-tests)) writes one file
  per instrumentation: `coverage-<batch>.ec`, then `coverage-<batch>-2.ec` and so on. Kotlin gave two instrumentations
  of the same app one file name.
- **Failures are errors.** A folder that cannot be created, and a file that is missing, empty or cannot be pulled, log an
  ERROR naming the batch and the path, for example
  `Code coverage of batch <id> is missing: <path> was not written or could not be pulled (<reason>)`. Those tests then
  have no `COVERAGE` attachment. The run's outcome and exit code do not change. A batch that is cancelled is not pulled.

Coverage is no longer a file-sync entry and no longer uses `run-as`, for debuggable apps too. Kotlin copied the whole
`/data/data/<app>/coverage` folder; only the coverage file is pulled now. The shell leaves
`<external storage>/Android/data/<app>` and its `files` folder in place, open to the app, when it created them.

Evidence: `android_runner::coverage_is_written_to_shared_storage_pulled_and_attached_without_run_as_d81`,
`missing_coverage_is_an_error_and_attaches_nothing_d81`, `a_coverage_directory_that_cannot_be_created_is_an_error_d81`,
`each_instrumentation_of_a_batch_writes_and_pulls_its_own_coverage_file_d81_d28`,
`coverage_is_prepared_after_a_file_sync_setup_that_contains_it_d81`,
`android_artifacts::coverage_is_attached_to_every_started_test_and_merged_parameterised_result_d81`. The recorded Kotlin
file-sync scenario is compared with a D81 allowance that drops its coverage rows. On an API 36 emulator a
non-debuggable app with JaCoCo offline instrumentation gave one readable `.ec` per batch, where 0.14.0-rc.1 gave none.

Approved 2026-11-17 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the
[x79 findings](wp/x79-findings.md).

**Migration action:** none for most jobs: the `.ec` files land where they did. A tool that reads attachment types from
the per-test result files or Allure should accept `COVERAGE`. Its path, like every attachment's, is absolute on the machine
that ran marathon; a tool reading a copy of the output folder elsewhere joins the part after the last `/device-files/`
onto its copy. A job that made its app debuggable only to get coverage
can stop doing so.

## D83 — Copies of different tests take turns in the queue

[D60](#d60--equal-priority-tests-run-in-insertion-order) breaks priority ties first-in, first-out, and the copies of a
test were inserted next to each other: `flakinessStrategy` `probability` and `fixed` list a test's copies together, and
`shardingStrategy: count` repeats each test in place. So with equal sort keys, for example `sortingStrategy: no-sorting`
or tests without history, a wave of free devices took every copy of the first tests before any copy had run. Under
`executionStrategy.fast` (the default), `fast` drops only copies that are still queued, so copies that all started
together all ran. The Kotlin marathon's heap order mixed copies of different tests by accident.

A pool's queue now takes the shard round by round: every test once, in shard order, then each test's second occurrence,
then each test's third, and so on. For three tests with four copies each and no sorting, the queue is
`t1 t2 t3 t1 t2 t3 …` where it was `t1 t2 t3 t1 t1 t1 t1 t2 t2 …`. The rule applies to every repeated entry of a pool's
tests, whatever made the repetition: flakiness copies, `count` repeats, or a test discovered twice (for example in two
test APKs), whose second entry now waits until every test has had its first.

- **What `fast` drops:** copies that are still queued when the test is decided: after its first pass under
  `ANY_SUCCESS` (flakiness copies or a test discovered twice; the configuration rejects a `count` other than 1 with
  `ANY_SUCCESS`), or after its first
  failure under `ALL_SUCCESS` (`count` repeats and `fixed` copies, for example). A copy that has started runs to the end,
  and so does a batch already handed to a device, even if its tests have not started yet. With copies in turns, more of a
  test's copies can still be queued when it is decided, so `fast` can drop more of them.
- **What stays:** which copies exist and how many (D34, D71, D72), what `marathon parse --includeFlakyTests` lists and
  in which order, the order between different sort keys (a longer test's copies still come first under
  `execution-time`), the random keys a seed gives (each test is still first inserted in shard order), batching rules
  (`fixed-size` never puts two copies of a test in one batch, `class-name` still takes every queued copy of its class),
  `fast: false`, and the rules that decide a test's result and its final record: `ALL_SUCCESS`, `ANY_SUCCESS`, `fast`,
  D69 and D77.
- **Retries and re-queued tests** are unchanged: a retry, a rejected batch's tests and `fixed-size`'s rejected copies go
  to the end of their priority, as under D60. A pool admitted late (D65) builds its queue the same way.
- **What changes:** the order of equal-priority copies, so which copies a wave of devices takes and which device runs
  which copy. Where a test's outcome depends on the device, its verdict can change with the device: with two devices,
  `count: 2`, `ALL_SUCCESS` and `fast: false`, the grouped order could send one copy of a test to the device where it
  fails, and the new order both copies to the device where it passes, so the run passes where it failed, with the same
  outcome on each device, and with `ignoreFailures: false` exits 0 where it exited 1; the exit-code rules are the same. Even with the same verdict, D77 can pick another attempt as the final record, from another
  device, with other timing and attachments. Test-selector lists in worker commands (`adb -e class`, xcodebuild
  `-only-testing`) change order and grouping, and `fixed-size` batches can be fuller or emptier and more or fewer: a
  batch that used D50's rejected polls on one test's copies and went out short is now full, but with size 2 and
  A ×4, B ×2, C ×2 the batches go from `AB|CA|AB|CA` to `AB|CA|BC|A|A`, one more launch. With `fast`, fewer copies can
  run, so reports and `raw.json` can list fewer executions. Executables and flags stay; non-test operands are built as
  before, but with code coverage on a test in another batch gets that batch's generated Android `coverageFile` path.

Evidence: on a CI farm of Android emulators, a job of 3 tests without history (each given 4 copies by `probability`,
15 runs) on 8 emulators gave 15 executions with 0.14.0-rc.2 and 10 with the JVM build, in the same wall time: rc.2's
first wave held all five runs of one test, each started before any had passed, and the copies cost about 50 minutes of
device time against 35. `flakiness_scenarios::d83_copies_of_different_tests_interleave_so_fast_drops_more_of_them` runs
3 tests with 4 copies each on 8 Stub devices under `ANY_SUCCESS`: the first wave holds every test, and 11 runs happen
where the grouped order ran 13. `flakiness_scenarios::d83_count_repeats_interleave_so_fast_all_success_drops_queued_runs`
runs `count: 5` under `ALL_SUCCESS` with one test failing: 13 runs where the grouped order ran 15. `strategy_flakiness::d83_*`
pin the order for equal keys, distinct keys, a seeded random order and a test discovered twice, and the batching rules.
Registered native goldens have no copies and are unchanged ([golden-diff note](wp/x81-golden-diff.md)).

Approved 2026-11-18 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and the [x81 findings](wp/x81-findings.md).

**Migration action:** none. Snapshots of worker commands or batches for runs with copies or repeated tests need an
update, as for D60.

## Other accepted text differences

[PLAN §3.10](../PLAN.md#310-accepted-differences-same-behaviour-different-text)
separately permits Jackson-internal parse-error wording, thread names and JVM
frames in internal traces. Custom validation messages and configuration wrappers
retain their input/validation contract under [PLAN §3.12](../PLAN.md#312-phase-8-contract);
their error wording may change, including D59. External xcodebuild, xcresult and
instrumentation traces remain unchanged. Phase 8 also replaces diagnostic logging
with `tracing` ([format and visibility](design/logging.md), [reviewed golden audit](wp/8.8-golden-diff.md))
and uses serde JSON/YAML writers whose text may differ while preserving ordered
source data ([WP 8.4](wp/8.4.md), [WP 8.9](wp/8.9.md)). One old YAML corruption is
fixed: NEL (U+0085) in a mapping key now survives instead of becoming a space
([U184](found-bugs.md)). Per-test progress and the complete end-of-run summary
block, including `Marathon run finished` and `\tRaw:`, keep their text format.
These allowances are not additional numbered deviations.

This guide records approved differences. Proposals that are still pending are not listed here.
