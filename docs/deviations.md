# Approved differences from Kotlin marathon

[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) is
canonical. This guide covers D1–D42, D45, D47, D50 and D51; implementation availability
is qualified by the [README](../README.md). Each entry describes the Kotlin
behavior, native behavior and migration action. Pending proposals are not
shipped differences.

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
CI detection quirks. D14–D19 and D21–D42 fix the ones described in this guide, and
the [ledger](known-kotlin-bugs.md) marks the rows they fix as fixed. Consult the
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
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[ADB findings](wp/6.1-findings.md) and [logcat findings](wp/6.5-findings.md).

## D12 — Native CLI and process diagnostics

Kotlin emits an SLF4J provider line, JVM stack frames and run-specific identity
hashes. Native output omits that line and internal frames, retaining exception
class, message and `Caused by:` lines; external tool traces pass through.
Identity suffixes are stable, and `System Information:` lists OS facts instead
of JVM properties. Native signal handling retains captured JVM exit statuses.

Kotlin expands `@argfile`, provides `_MARATHON_COMPLETE` completion and wraps
help to the terminal. Native help uses 79 columns, with no argfile expansion or
completion script. Pass arguments directly, replace completion integration and
update diagnostic parsers.
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
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and [provider findings](wp/6.5-findings.md).

## D15 — Android instrumentation cleanup

Kotlin can leave log collection and instrumentation running after remote-parsing
failure or a failing execution after hook. Native execution cleans them up on
every exit. No configuration change is needed; do not rely on leaked
instrumentation continuing after failure.
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12)
and [runner findings](wp/6.6a-findings.md).

## D16 — Android artifact cleanup

Kotlin can race recording operations and leave profiling running after run
failure. Native execution coordinates recording and waits for artifact tasks
to stop during cleanup. On failure, perfetto stops but its trace stays on the
worker without being downloaded. Video/GIF cancellation adds no remote cleanup
commands; profiling setup failures still fail the batch. No configuration change
is needed; account for retained remote traces.
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
forwards restored; one not listed within `waitForDevicesTimeoutMillis` is disconnected. An emulator
that itself restarted during the outage looks the same when it is listed again under the same serial:
it is not prepared again, so its batches fail on the missing APKs. `uncompletedTestRetryQuota`, which must be finite
([D47](#d47--an-unlimited-uncompletedtestretryquota-is-refused)), ends them when it runs out, and a small one fails the tests. This is a known limit. A plain adb server restart does not restart emulators. A server
that does not return within `waitForDevicesTimeoutMillis` has its devices disconnected, so the
run ends as it does when devices are lost, and other servers are not affected. If adb restarts
take longer on your hosts, raise that value.
Remote test parsing waits in the same way: an outage does not use up its attempts. If the server never
returns at that stage, the run fails with `NoDevicesException` when `deviceInitializationTimeoutMillis`
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
java.io.EOFException: the instrumentation stream ended before a result`. That includes the test in progress. A test
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
come back as uncompleted, as when any other device command fails, instead of pushing the folder inside the leftover. A video can stay behind on
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
simulator, so when that simulator was the last device of the pool, the pool ends and so does the run. The close fails with the same error
as Kotlin's, and one error line names the command.

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
link is unaffected for as long as it keeps moving. Receives keep Kotlin's 30 s total deadline. No configuration change is
needed, and the limit is not configurable.

**How long a dead worker takes.** One stalled send takes 300 s. The installer makes three attempts, and when it fails the
device's own retry runs it again, up to 30 times, 10 s apart. One simulator can therefore spend 90 sends, about 7.6 hours,
before it is given up. Sends to one worker and destination run one at a time, so N simulators on one worker take about N
times that. A second worker that finishes the queue ends the run. Kotlin waits for ever here.

This applies to a process wedged behind an sshd that still answers, to a peer that accepts the connection and says nothing,
and to any stall with `keepAliveInterval` set to 0. With the default `keepAliveInterval` of 15 s and `keepAliveMaxCount` of 4,
a dead link or a frozen peer ends ssh, and so that send, after about 60 s, with ssh's `Timeout, server … not responding` in
the error. That still costs about 1.6 hours per simulator. Attempts after the first can also wait in ssh's connect for the
host's TCP timeout, because `--rsh` sets no `ConnectTimeout`, which makes it longer.

**What a worker sees.** On a host with rsync 3, the flag also reaches the worker in `rsync --server … --timeout=300 …`, so a forced
command or wrapper there that matches that string sees a new flag. openrsync, which macOS hosts have, does not pass it on.

**What stays unbounded.** The bridge recognises a stall from what rsync prints: `poll: timeout` from openrsync, `io timeout after`
from rsync. An rsync that words it differently, and then waits on a hung ssh, keeps Kotlin's wait.

Approved 2026-11-06 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A3](known-kotlin-bugs.md) and the [x13 findings](wp/x13-findings.md).

**A receive at its deadline** is stopped the same way (U155, approved 2026-11-07): TERM, 5 s, KILL, and under `sshpass` rsync gets them too.
Kotlin's wrapper sends TERM to `sshpass` alone, which leaves rsync and ssh running, and then waits without a limit for a process that ignores it. The 30 s
deadline itself and the receive's command line are unchanged. See the [x21 findings](wp/x21-findings.md).

**This loses files on a password-authenticated worker.** In Kotlin, rsync went on after `sshpass` had gone, and finished the pull in the background, so a pull over 30 s returned
`false` and its file still arrived. The video and device log listeners ignore the result, so those files still turned up in the report. Now the pull is really cut: the file is
missing, and `.rsync-partial/<name>` is left in the output directory. That is what key authentication has always done. Measured with real `sshpass`, openrsync and sshd, 24 MiB at
1 MiB/s, TERM at 5 s: TERM to `sshpass` alone, and the file was complete 27.4 s later; TERM to rsync too, or key authentication, and it never was.

**A receive's output pipes** also have the send's limit (U158, approved 2026-11-07). A process that ended before the deadline, leaving a child that holds its pipes, such as a hung
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
one that could not be deleted is reused. Retries stop at `workerConnection.lateJoinCutoff` and when the run ends. The
end of a run doesn't wait out a pause, but it does wait for an attempt in flight to stop. A retried simulator is
admitted to the run like a late one. An unreachable worker keeps its own
connection retry. A refused connection, a simulator the worker doesn't have and an unavailable profile are not
retried. There is no new setting: `attempts: 1` with `lateJoin: false` turns the retries off. `simctl` commands and
log lines increase only when a setup fails. The macOS provider is not affected.
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
a `ConnectionException` with the same text and the reason as its cause, so such a command returns no result, an error
log says so, and marathon handles the device as it does any lost connection: on iOS it reconnects, and on macOS, which has no reconnect,
the device leaves the run. Using a live connection after marathon
closed it is still an `IllegalStateException`.

**Known limit: the iOS reconnect has no end.** Every 16 ms the provider looks for lost simulators and starts a recovery for
each one that is not already being recovered. After a lost connection that recovery has no attempt limit, so a registered
simulator whose worker stays down is tried again for as long as the run lasts, each attempt as long as its own connect takes.
The configured retry limits apply only to a simulator lost while its connection stays up. Kotlin has the same shape
([A101](known-kotlin-bugs.md), [U164](found-bugs.md)).

Approved 2026-11-06 (project owner's decision).
See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[A17](known-kotlin-bugs.md) and the [x16 findings](wp/x16-findings.md).

**Migration action:** none. If a worker keeps dropping connections after login, look at the worker; an iOS run keeps reconnecting to it until the run ends.

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
close. On the last device the run ends sooner, after about 6 to 13 minutes, with `remaining tests will not run`. When only the
probe hangs and needs KILL it takes about 33 minutes, and with every command hung about 50.
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
simulator has 3 recoveries, so if every batch goes silent it takes at most 12 batches, about 60 minutes plus setups and
pauses of 30 and 60 s, and it then leaves the run. On the last device the run then ends, where Kotlin repeats the batch until
CI kills the job. `uncompletedTestRetryQuota` can end it sooner.

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
- The limits are checked between rounds of reading the device list, and a round also asks each device that returned for
  its properties (one `getprop`, bounded by `timeoutConfiguration.shell`, 30 s by default). While a returned device's
  `getprop` goes unanswered, no other device's limit is checked, so a device can be held up to that much past its 10 s or
  3 s, and the killed emulator below reaches its retry path that much later ([U170](found-bugs.md)).
- A device that goes `offline` and then leaves the list has 3 s, since a real blip passes through an empty list for up to
  a second. A killed emulator looks like this, so its retry path starts about 3.25 s after it dies, where it was immediate.
- A device leaving the list directly from `device`, or reaching a state such as `unauthorized`, is disconnected at once.
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
`NoDevicesException` instead, with no such text.

A session that listed every test its `numtests` announced keeps its list, whatever its code and message: some sessions end
that way on a whole list, and a retry would only repeat it. A session with no count to reach has shown nothing and fails.
Code -1 is success as before, even with fewer tests than announced, unless the stream was cut inside that last line, which
has no line break then. `Process crashed` still means the test annotation
producer, with the same fallback. `am` refusing to start and output that never was the protocol are unchanged, and so is
the batch runner's handling of the same endings. Approved 2026-11-07 (project owner's decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12), [N38](known-kotlin-bugs.md) and the
[x20 findings](wp/x20-findings.md).

**Migration action:** none for a normal test APK. A custom runner that stops listing its tests early and exits with a failing code now fails the run with its text, where it used to run the tests it had listed.

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

Native builds refuse that configuration. `marathon run` and `marathon parse` exit 1 with a `ConfigurationException` when
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
- With sorted tests every test keeps its place. Under ties, as with `no-sorting`, the order is the heap's: rejected tests go
  to its end, so after such a batch tests can run in another order than in Kotlin.
- Tests of two APKs in the order they were listed, without sorting, gain almost nothing: 1,000 tests at size 10 take 101
  batches either way. Sorted, those take 100 instead of 150.
- `class-name` and `isolate` batching are unchanged.

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

Metrics from a working backend are byte for byte the same, except points still queued when a close runs out of time. A queue that ends for any other reason still hangs the run
([U168](found-bugs.md)). Approved 2026-11-07 (project owner's decision). See
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

## Other accepted text differences

[PLAN §3.10](../PLAN.md#310-accepted-differences-same-behaviour-different-text)
separately permits Jackson-internal parse-error wording, thread names and JVM
frames in internal traces. Custom validation messages and configuration wrappers
retain their contract; external xcodebuild, xcresult and instrumentation traces
remain unchanged. These allowances are not additional numbered deviations.

This guide is updated in the same change that approves or amends a deviation.
Proposals that are still pending are not listed here.
