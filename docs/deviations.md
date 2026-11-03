# Approved differences from Kotlin marathon

[PLAN §3.11](../PLAN.md#311-deliberate-deviations-approved-2026-10-12) is
canonical. This guide covers D1–D19; implementation availability
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
that disconnects stays in the provider's map as in Kotlin. When several servers
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
