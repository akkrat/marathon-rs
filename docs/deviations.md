# Approved differences from Kotlin marathon

[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) is
canonical. This guide covers D1–D34; implementation availability
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

Kotlin defects remain in v1, including metadata lookup, dynamic-provider EOF
and CI detection quirks. Migration is not a bug-fix release. Consult the
[ledger and its per-row status](known-kotlin-bugs.md) and the
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

## Other accepted text differences and synchronization

[PLAN §3.10](../PLAN.md#310-accepted-differences-same-behaviour-different-text)
separately permits Jackson-internal parse-error wording, thread names and JVM
frames in internal traces. Custom validation messages and configuration wrappers
retain their contract; external xcodebuild, xcresult and instrumentation traces
remain unchanged. These allowances are not additional numbered deviations.

An integration that approves or amends a deviation must update this guide in the
same change or leave an explicit WP 7.4 handoff. Compare both ID sets **and full
entry meanings** at review and again in WP 7.5 on the release SHA. Later iOS or
differential work may add approved IDs; do not promote pending proposals here.
The [documentation findings](wp/7.4-findings.md) record this comparison.

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
cannot fire before their own timeout. `socketIdleTimeout` now bounds the few requests with no
timeout of their own, such as asking the adb server's version. No configuration
change is needed. A value you chose assuming seconds is now taken as written.
Approved 2026-11-06 (user decision). See
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
it is not prepared again, so its batches fail on the missing APKs until their retries run out or it
really disconnects. A plain adb server restart does not restart emulators. A server
that does not return within `waitForDevicesTimeoutMillis` has its devices disconnected, so the
run ends as it does when devices are lost, and other servers are not affected. If adb restarts
take longer on your hosts, raise that value.
Approved 2026-11-06 (user decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N16](known-kotlin-bugs.md) and the [x12 findings](wp/x12-findings.md).

## D23 — Waiting for a pushed file has a limit

With verification on, Kotlin polls a pushed file's md5 with no limit of its own, so a
file sync push whose checksum never matches waits for the whole batch timeout. Native builds
stop after `timeoutConfiguration.pushFile` with an error that names the file. The legacy install
keeps its `Timeout transferring` message. Approved 2026-11-06 (user decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and
[N5](known-kotlin-bugs.md).

## D24 — A lost Android device stays out of use and says so

Kotlin keeps disconnected devices in its map, so remote test parsing can borrow a disposed one,
and reports `networkState: CONNECTED` for ever. Native builds drop the device from the map and
report `DISCONNECTED` once it is lost, also after its serial returns as a new device. The change
reaches output only for a device lost by then: `networkState` in its per-test
`test_result` JSON. Approved 2026-11-06 (user decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N27 and N29](known-kotlin-bugs.md) and the [x12 findings](wp/x12-findings.md).

## D25 — A failed instrumentation stream is reported

In Kotlin, an instrumentation stream that fails (a rejected handshake, a closed connection, output
the parser refuses) looks like a normal end: tests go missing with a generic text and
nothing says why. Native builds report a test-run failure with the cause and log it, so those
tests are retried and their reports carry the real reason. A device is retired only
for a failure that retires it in any other adb request. A remote parsing attempt now fails, and is retried, instead of
returning only the tests found so far. Test crashes and non-zero exits are unchanged. Approved
2026-11-06 (user decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N38](known-kotlin-bugs.md) and the [x12 findings](wp/x12-findings.md).

## D26 — DEX 040 and 041 test APKs

Kotlin's parser rejects test APKs newer than DEX 039 with `Invalid dexMagic`. Native builds accept
040 and 041, including 041 containers of several images, and find every class in them. Anything newer is
still rejected with the same message. Approved 2026-11-06 (user decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) and
[N10](known-kotlin-bugs.md).

## D27 — Prepare no longer wipes external storage

Kotlin runs `rm -r $EXTERNAL_STORAGE` and `mkdir` on every device prepare, deleting everything
under, for example, `/sdcard`. Native builds leave it alone. **This changes behaviour for existing
users: a test suite that relies on an empty external storage must clear it itself.** See the
[migration guide](migration.md#android-external-storage-is-no-longer-wiped). Approved 2026-11-06
(user decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N4](known-kotlin-bugs.md) and the [x12 findings](wp/x12-findings.md).

## D28 — Each instrumentation runs only its own tests

With test APKs of different instrumentations in one batch, Kotlin gives each instrumentation the whole
batch, repeats the ignored-test callbacks and the end-of-run step, and keeps only the first result.
Native builds run each instrumentation on its own tests, in the order first seen, and report one result
for the batch. If one instrumentation fails or times out the others still run; a lost device ends the
batch, and its unrun tests are retried. `testBatchTimeoutMillis`, `testOutputTimeoutMillis` and clearing
package data apply to each instrumentation. Batches of one instrumentation are unchanged.
Approved 2026-11-06 (user decision). See
[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[N35](known-kotlin-bugs.md) and the [x12 findings](wp/x12-findings.md).

## D34 — A failed history query is not final

Kotlin caches a history query that fails three times as empty for the whole run, so a short backend outage
at the start costs the run its history. Native builds answer with the defaults, as Kotlin does, and query
again in the background: after 5, 10, 20, 40, 80, 160, 300 and 300 seconds, at most 8 times per query. An empty answer
counts as an answer and is not repeated. A queue already built keeps its order, so new history reaches
queues built later and batches composed later. A run never waits for this, and it stops when the run ends. With
a healthy backend nothing changes. With a failing one the log gains one failed timing line and one warning per
re-query, and one line when the history returns or the attempts run out. No configuration change is needed.
Approved 2026-11-06 (user decision). See [PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12),
[C41](known-kotlin-bugs.md) and the [x15 findings](wp/x15-findings.md).
