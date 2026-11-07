# Migrating from the JVM distribution

Keep your existing Marathonfile, Marathondevices and command lines, subject to
[the approved differences](deviations.md). Packaging checks alone do not certify
end-to-end parity: see [release readiness](releasing.md#release-readiness) for what
the evidence covers, and check the release's notes before migrating.

## Install the native entry point

Choose the archive for the **runner host**, not the device architecture:
`aarch64-apple-darwin` for Apple Silicon macOS, or `x86_64-unknown-linux-musl` for
x86_64 Linux on any distribution. Download `marathon-<version>-<target>.zip` and
`SHA256SUMS` from the same version's GitHub Release. Versions and tags have no `v`
prefix. There is no Windows, Intel Mac, Linux on ARM or universal binary. Branch builds
do not publish. GitHub latest means the last successfully promoted release, not
necessarily the highest semantic version, and pre-releases are never latest. Use a
pinned version for reproducible CI entry points.

Replace the angle-bracket placeholders before running this; each release's notes carry
the same commands with its own version and repository filled in. It verifies the
selected asset against its exact entry in `SHA256SUMS`:

```sh
set -eu
version='<version>'
repo='<owner>/<repo>'
target='aarch64-apple-darwin'  # or x86_64-unknown-linux-musl
archive="marathon-${version}-${target}.zip"
url="https://github.com/${repo}/releases/download/${version}"
curl -fLO "${url}/${archive}"
curl -fLO "${url}/SHA256SUMS"
# Fail unless the checksum file contains exactly this asset once:
awk -v asset="$archive" '$2 == asset { print; n++ } END { if (n != 1) exit 1 }' \
  SHA256SUMS > selected.sha256 || {
    echo "Expected exactly one checksum for $archive" >&2
    exit 1
  }
[ -s selected.sha256 ] || { echo "Selected checksum is empty" >&2; exit 1; }
shasum -a 256 -c selected.sha256
unzip "$archive" -d native-runner
"./native-runner/marathon-${version}/bin/marathon" version
```

On Linux, replace `shasum -a 256 -c` with `sha256sum -c`. Extract into a fresh directory.
The archive contains `marathon-<version>/bin/marathon`, with Unix mode 0755, and the text
files `LICENSE` and `THIRD-PARTY-LICENSES`, with mode 0644. There is no launcher script or
JAR directory. The final stdout line from `version` must equal the version exactly. A
startup log precedes it. To check where an archive was built, run
`gh attestation verify "$archive" --repo "$repo"`.

**macOS quarantine.** The macOS binary is ad-hoc signed and not notarised, because
that needs an Apple Developer account. A browser marks its downloads as quarantined, and
extracting such a zip with `unzip` passes the mark to the binary. Gatekeeper then blocks
it: it does not start, and in an unattended session, such as a CI step, it hangs with no
output. `curl` and `gh`, as above, set no quarantine. For a browser download, clear the
mark from the extracted binary before the first run:

```sh
xattr -d com.apple.quarantine "native-runner/marathon-${version}/bin/marathon"
```

A copy already launched while quarantined can stay blocked after that, so extract it
again and clear the new copy.

Change your PATH or CI executable path to the extracted `bin/marathon`, retaining
the previous JVM distribution separately for rollback. Check `command -v marathon`
and `marathon version` after the switch. Preserve the CI working directory and
environment used for configuration substitution. Do not carry JVM arguments into
the native argv: `JAVA_OPTS` and `MARATHON_OPTS` are ignored (D2).

### Platform and workload prerequisites

The macOS release helper sets a macOS 11.0 linker floor; observed WP 7.3 metadata
matches it, but old-macOS execution is not certified. The Linux binary is static, so
it needs no particular libc: each release runs `version` and `--help` in an Alpine
container before it is published. The real Android runs in the
[release evidence](wp/7.5-findings.md#closure-sweep-results) used a glibc build, and
musl's allocator and DNS resolver differ from glibc's, so include your Android workload
and worker hosts in the trial below. See [operator instructions](releasing.md) for
precise limitations.

The runner needs no JVM. Android still needs the configured SDK, ADB server and
devices; SDK lookup uses `androidSdk`, then `ANDROID_HOME`, then
`ANDROID_SDK_ROOT`. Apple workloads need the appropriate Xcode tools on their
workers. SSH transport, system `rsync`, and `sshpass` for password-based rsync
remain workload-dependent; recording may require tools such as `ffmpeg`.
`version` and help require no worker or test configuration. A developer JDK for
oracle generation is independent of runtime installation.

## Changes that need action

Most differences need nothing from you. These are the ones that can change what you see or what you must set. Each
links to its entry in the [deviations guide](deviations.md).

| If you… | What changed | Do |
|---|---|---|
| **have any Marathonfile that doesn't set `uncompletedTestRetryQuota`** ([D47](deviations.md#d47--an-unlimited-uncompletedtestretryquota-is-refused)) | **Breaking.** `marathon run` and `parse` now exit 1 when it is unset or 2147483647, because nothing else ends a run in which a test keeps coming back uncompleted. | Add `uncompletedTestRetryQuota: 5`, or the number of devices + 2 if that is more. [See below](#a-finite-uncompletedtestretryquota-d47). |
| rely on an empty `/sdcard` ([D27](deviations.md#d27--prepare-no-longer-wipes-external-storage)) | Prepare no longer runs `rm -r $EXTERNAL_STORAGE`. Videos from an aborted run, or cut short by a lost connection in a run that still exits 0, stay on the device. | Clear what your tests need in the tests or in your device setup. |
| set `socketIdleTimeout` ([D21](deviations.md#d21--socketidletimeout-means-what-it-says)) | It is now real milliseconds. It bounds only four requests: the adb server's version, a device's features, the property reads after boot and the removal of port forwards when a device is disposed. Kotlin read it as seconds, so `PT30S` lasted 30,000 s. | Remove it, or keep it at 30 s or more. `PT2S` now fails those requests on a slow adb server. |
| have a forced command or wrapper on an iOS or macOS worker ([D31](deviations.md#d31--an-rsync-send-that-stops-moving-fails), [D32](deviations.md#d32--xcodebuild-on-a-worker-runs-unbuffered)) | xcodebuild is sent as `env NSUnbufferedIO=YES xcrun xcodebuild …`. Sends carry `--timeout=300`. On a host with rsync 3 the worker's `rsync --server` gets it too, and so does a wrapper set with `RSYNC4J_RSYNC`. | Match `env NSUnbufferedIO=YES xcrun xcodebuild` at the start of `SSH_ORIGINAL_COMMAND`, keep `env` on the worker's `PATH`, and accept the new flag. |
| run iOS simulators on a slow host or link ([D38](deviations.md#d38--a-failed-readiness-probe-means-not-running)) | A readiness probe returns about 20 MB, and a failed or timed-out probe now means "not running". It must arrive within `shell` (30 s by default). | Raise `shell`. The time a simulator can then spend is in [D38](deviations.md#d38--a-failed-readiness-probe-means-not-running). |
| have UI tests that fail with a "Failed to terminate…" message ([D39](deviations.md#d39--a-tests-own-text-does-not-abort-its-batch)) | After a prefix such as `XCTAssertTrue failed - ` the test fails and the batch goes on. A simulator is no longer dropped after 3 such batches in a row. Three output timeouts in a row with no test passed or failed do recycle it, also for a silent test with no failure text. | If a batch waits out its timeout, or a sick simulator keeps taking batches, read the failing line in the log and consider `maxBatchesWithoutProgress`. Raise `testOutputTimeoutMillis` for a test that is silent for longer and passes. |
| depend on a simulator being set up once ([D33](deviations.md#d33--a-simulator-whose-setup-failed-is-set-up-again)) | A simulator whose setup failed is set up again 30, 60 and 90 s after each failure, even on a removed worker's profile. | `attempts: 1` with `lateJoin: false` in `workerConnection` turns it off. |
| have a worker with no algorithm in common with marathon ([D35](deviations.md#d35--a-failed-key-exchange-is-not-retried-as-a-network-fault)) | It is no longer retried. | Fix the worker's SSH configuration. |
| pull slow artifacts from password-authenticated workers ([D31](deviations.md#d31--an-rsync-send-that-stops-moving-fails), U155) | A pull that takes more than 30 s is now really cut. Kotlin's rsync finished it in the background. | Nothing yet: there is no setting for the 30 s. Expect the file to be missing and `.rsync-partial/<name>` to be left in the output directory. |
| have devices that go `offline` often ([D41](deviations.md#d41--a-brief-offline-does-not-lose-the-device)) | A device gets up to 10 s to come back, at most 5 times per adb server in a run. The count is never reset. From the sixth `offline` on the device is disconnected at once, as in Kotlin. | Fix the transport. A device that drops more than five times in one run is lost for the rest of that run. |

These deviations need no action: D1, D4, D5, D7, D13, D15–D19, D23, D26, D28–D30, D34, D36, D37, D40, D42 and D45. D2, D3,
D6, D8–D12, D14, D20, D24 and D25 have an action of their own, described in their entries and in the sections below.

### A finite `uncompletedTestRetryQuota` (D47)

Add one line at the top level of every Marathonfile, on Android, iOS and macOS alike:

```yaml
uncompletedTestRetryQuota: 5
```

Without it, or with 2147483647, `marathon run` and `marathon parse` exit 1. No other setting can stand in for it: nothing
else ends a run in which a test keeps coming back uncompleted. `retryStrategy` applies to failed tests only, the timeouts
bound one batch, and on iOS `maxBatchesWithoutProgress` isn't charged for an unclassified error or a lost connection. That
setting stays optional, with 0 meaning off, and macOS and Android don't have it.

**Which value.** Use the number of devices + 2, and at least 5. Each time a test comes back uncompleted it is charged once,
and that includes a batch lost with its device, so a test that was only unlucky can be charged about once per device
during a bad outage. Too small a value turns an emulator restart or a lost worker into failed tests. The other side of the
choice is cost: a test that hangs every time takes (quota + 1) × the time it takes to give up, usually
`testOutputTimeoutMillis`. Any value other than 2147483647 is accepted, so a very large one keeps the old behaviour, if that
is really what you want.

## Keep configuration and command placement

Use your existing valid configuration and bundles; a stub-vendor Marathonfile
is not a usable CLI quick start. These are examples for your configured test
environment, **not commands to run merely to validate installation**:

```sh
# Root invocation runs with ./Marathonfile:
marathon
# Root invocation with an explicit path:
marathon -m Marathonfile --analyticsTracking false --bugsnag false
# Equivalent explicit subcommand; its options follow it:
marathon run -m Marathonfile --analyticsTracking false --bugsnag false
# Lists discovered tests; can extract bundles and use workers:
marathon parse -m Marathonfile -o discovered-tests -f false
# Harmless installation check:
marathon version
```

The default file is `Marathonfile`. Root options are not inherited by subcommands:
`marathon -m custom-file run` does not select that file for `run`.
Boolean options need text arguments; only case-insensitive `true` is true,
so use explicit `true` or `false`, including `parse -f false`.
`--help`, `run --help` and `parse --help` show each command's own options.

Configuration keys, defaults and staged validation retain the Kotlin contract.
Environment substitution applies to Marathonfile, Marathondevices and each
dynamic update, including `${V}` and `${V:-default}`; preserve the environment
and consult [PLAN §3.2](../PLAN.md#32-configuration-three-deserialization-profiles-and-staged-validation)
for escaping, recursion and error rules. Relative Marathonfile `File` values
resolve against its directory; absolute file paths retain spelling at binding.
Apple post-processing separately canonicalizes devices, known-hosts and SSH key
paths. **Relative authentication keys in Marathondevices/dynamic updates resolve
when opened against process cwd**, not the Marathonfile directory (F4).
Keep cwd stable or supply absolute key paths.

Audit user regexes before switching (D6), including serial exclusions and test
filters. Supported string syntax such as `'^com\.example\..*Test$'` avoids
lookaround/backreferences; acceptance alone is not proof of equivalent matching.
Review malformed/exotic configuration exceptions in D9 rather than assuming all
Jackson edge cases are reproduced. The [schema](../crates/marathon-config/src/schema.rs),
[vendor schema](../crates/marathon-config/src/vendor.rs) and
[configuration evidence](wp/2.6-findings.md) are detailed references.

### Parse is discovery, with side effects

`parse` is not just configuration validation or a side-effect-free dry run.
Loading Apple bundles can delete the extraction directory and extract `.ipa` or
`.zip` contents before logical validation. Remote discovery initializes the
provider and borrows a worker; local discovery can skip provider initialization.
Requesting flaky tests can query history. Even with `-f false`, construction
can initialize configured analytics backends. Logical validation and provider-time
validation remain separate from configuration loading.

`parse -o discovered-tests` writes `<outputDir>/discovered-tests.yml`, despite
the retained help text saying `.yaml`. It does not wipe `outputDir`.
The normal run path wipes `outputDir` later, after parsing and shard preparation.
Use a disposable, isolated environment for migration trials; never point both
implementations at the same active output directory or worker state.

## Outputs, logs and exit status

Reports keep the established directory layout: device/billing JSON, JUnit,
per-test and raw results, Allure, HTML/timeline, and vendor logs/media/artifacts.
The complete contract is [PLAN §3.4](../PLAN.md#34-output-directory-complete-manifest).
Account for the omitted JSON delegate field (D3), en-US formatting (D8), text
decoding (D10/D11) and diagnostic changes (D12). Video parity compares decoded
content and metadata, not identical encoded bytes. Existing Kotlin logger names,
startup/runtime phases and progress lines remain part of the
[logging contract](../PLAN.md#35-logs-startup-and-runtime-phases).

The single maintained completion-message/stream/exit table is
[PLAN §3.1](../PLAN.md#31-cli-an-argument-and-exit-matrix). In particular,
`ignoreFailures` can give exit 0 with a failure message on stderr. Configuration
load and construction failures occur before the run catch; not every failure
prints `Marathon execution crashed`. `version` includes its startup log before
the bare version; scripts must not treat all stdout as a version string.

Crash/usage telemetry is disabled (D1), but configured Influx/Graphite remains
active (D4). **Do not rely on `readOnly` for isolation**: the Kotlin deserializers
ignore it (F3), and that bug is retained. Use isolated backends for comparisons.

## The xctest parser needs a library you supply

You need this section only if your Marathonfile selects the parser:

```yaml
vendorConfiguration:
  testParserConfiguration:
    type: xctest
```

The default is `nm`, which needs no library. If your tests are not generated at runtime,
switching to `nm` is the simple way out.

An `xctest` run injects `libxctest-parser.dylib` into a fake test run on the worker. The
native program does not contain this library
([D20](deviations.md#d20--the-xctest-parser-library-is-supplied-by-you)). Take a copy from a
Kotlin marathon distribution, under the terms of its
[EULA](https://github.com/MarathonLabs/marathon/blob/-/vendor/vendor-apple/base/src/main/resources/EULA.md),
and point the runner at it:

```sh
export MARATHON_XCTEST_PARSER_DIR=/path/to/libraries
```

The directory has the layout of the Kotlin resources (`libxctest-parser/` in its jar),
`<platform name>/libxctest-parser.dylib`. You need the platforms you test on:

| Devices | File |
|---|---|
| iPhone and iPad simulators | `iPhoneSimulator/libxctest-parser.dylib` |
| Apple TV simulators | `AppleTVSimulator/libxctest-parser.dylib` |
| Apple Watch simulators | `WatchSimulator/libxctest-parser.dylib` |
| Vision simulators | `XRSimulator/libxctest-parser.dylib` |
| macOS | `MacOSX/libxctest-parser.dylib` |

The variable is read on the machine that runs `marathon`, which pushes the file to the
worker as Kotlin did.

The library is third-party software and this project's licence does not cover it. Its EULA
says that it communicates with its server: it sends error reports, which cannot be disabled,
and anonymized usage data, which you may opt out of. Read the EULA before you use it, and
consider this if your workers have restricted network access.

Without a readable file the run stops with exit status 1 and a message naming the variable or
the file. macOS, and iOS with no simulator file at all, stop before any connection. An iOS run
with files for other platforms only stops once it has its first device, because only then is
the platform known; it does not retry or install anything.

## Retained behavior to account for

These statuses are from the [full bug ledger](known-kotlin-bugs.md) at the
reviewed SHA; the ledger links source locations and regression evidence.
Documentation adds no new reproductions.

| IDs | Consequence | Recorded status |
|---|---|---|
| A1 | Dynamic-provider EOF or errors other than connection refused stop updates permanently; this is distinct from recovery of an individual worker. | Reproduced; read-side oracle evidence exists, but completed native provider parity is not claimed here. |
| C1 / C2 | Batch filenames use `<batchId>.<batchId>`; webp/GIF screenshots carry a `jpg` suffix. Do not infer encoding solely from suffix. | Confirmed |
| A6 / N1 | The result-to-original-test lookup compares class with package and usually loses original test metadata. | Confirmed |
| F1 / F2 | An invalid execution-time `timeLimit` can report `bbb`; an invalid fixed-size batching limit silently becomes null. | Confirmed |
| F3 | Influx/Graphite `readOnly` is ignored, so it cannot protect comparison data. | Confirmed |
| F4 | Marathondevices relative keys are opened relative to process cwd. | Confirmed |
| F6 | macOS logical validation omits `MacosConfiguration.validate()`; successful validation is not proof that SSH/bundle settings are usable. | Confirmed |
| C3 | TeamCity auto-detection searches environment **values** for `TEAMCITY_VERSION`, rather than checking the key. A normally named variable alone does not ensure detection. | Confirmed |
| N4 | Kotlin's Android preparation clears the whole external-storage root, for example `/sdcard`. Native builds do not ([D27](#android-external-storage-is-no-longer-wiped)). | Fixed by D27 |
| C41 | Kotlin keeps a failed history query's empty answer for the whole run. Native builds query again in the background, a bounded number of times, and a queue already built keeps its order. With a failing backend the log has more `Cannot fetch … from database` warnings and timing lines ([D34](deviations.md#d34--a-failed-history-query-is-not-final)). | Fixed by D34 |

## Android external storage is no longer wiped

Kotlin's prepare step runs `rm -r $EXTERNAL_STORAGE` and `mkdir $EXTERNAL_STORAGE` on every device.
Native builds run neither ([D27](deviations.md#d27--prepare-no-longer-wipes-external-storage)).

- **Action for you:** if your tests rely on an empty external storage, for example because they count files
  or fail on leftovers, clear what they need yourself, in the tests or in your device setup.
- **What marathon still cleans:** each file sync pull entry is removed and recreated before every batch, and
  file sync pushes are replaced and then removed after the batch. Videos are removed after each test.
- **What can now accumulate:** videos left on the device by a run that was aborted, or by a recording that a lost
  connection cut short, in a run that still exits 0. Their names hold that run's batch id, which a later run
  cannot know, and marathon does not delete by pattern, because that could hit your data. Clear them with your
  device setup if your devices are long-lived.

## Other Android behaviour changes

These fix bugs that Kotlin has, and need no configuration change.

- **`socketIdleTimeout` is in real time** ([D21](deviations.md#d21--socketidletimeout-means-what-it-says)). It
  bounds only four requests without a timeout of their own (see the table above). A value picked with Kotlin's unit
  bug in mind is now taken as written.
- **An adb server restart no longer ends device tracking**
  ([D22](deviations.md#d22--a-run-survives-an-adb-server-restart)). A loopback server is started again.
  A server still away after `waitForDevicesTimeoutMillis` (30 s by default) loses its devices. Remote test parsing
  waits for the server too; if it never returns at that stage, the run fails with `NoDevicesException` when
  `deviceInitializationTimeoutMillis` (180 s by default) has passed. An emulator that restarted during an outage is not
  prepared again, so its batches fail on the missing APKs, until `uncompletedTestRetryQuota` runs out
  ([D47](deviations.md#d47--an-unlimited-uncompletedtestretryquota-is-refused)).
- **A brief `offline` no longer loses a device**
  ([D41](deviations.md#d41--a-brief-offline-does-not-lose-the-device)). A device gets up to 10 s, 3 s if it also leaves
  the list, so a killed emulator takes about 3.25 s to reach the retry path, where it was immediate. A device has at most
  5 of these per adb server per run, and the count is never reset.
- **A file sync push that never verifies fails after `pushFile`**
  ([D23](deviations.md#d23--waiting-for-a-pushed-file-has-a-limit)).
- **`networkState` can read `DISCONNECTED`** in a lost Android device's per-test JSON
  ([D24](deviations.md#d24--a-lost-android-device-stays-out-of-use-and-says-so)). A consumer that assumed it was
  always `CONNECTED` should be checked. iOS and macOS devices still report `CONNECTED`.
- **Failed instrumentation streams are reported** with their cause
  ([D25](deviations.md#d25--a-failed-instrumentation-stream-is-reported)). Uncompleted tests may carry that text
  instead of the generic one, `No test results` or none, and a remote parse cut off by a lost connection is retried
  instead of giving a shorter test list. A run that used to pass with tests silently missing can now run them all, or
  fail.
- **A remote parse that fails short of its list is a failed attempt**
  ([D42](deviations.md#d42--a-remote-parse-that-fails-short-of-its-list-is-a-failed-attempt)). A session whose
  `INSTRUMENTATION_CODE` isn't -1, which lists fewer tests than its `numtests`, is retried, and fails the run with its
  text if it keeps doing so. A custom runner that stops listing early and exits this way used to run the part it listed.
- **DEX 040 and 041 test APKs are accepted**
  ([D26](deviations.md#d26--dex-040-and-041-test-apks)).
- **Each instrumentation runs only its own tests**
  ([D28](deviations.md#d28--each-instrumentation-runs-only-its-own-tests)). This only matters for batches
  that mix test APKs, which takes `class-name` batching and the same class name in both.

## Batches hold more tests with flaky copies or several test APKs

`fixed-size` batching now counts only the tests a batch accepts
([D50](deviations.md#d50--a-rejected-test-does-not-use-a-batch-slot)). Batches that used to end early hold up to `size`
tests and up to `durationMillis`, so a run makes fewer instrumentation or xcodebuild starts and each batch can take longer.
Check `testBatchTimeoutMillis` if you set it close to what those short batches took. Under ties, as with `no-sorting`, tests
can run in another order after a batch that rejected a test.

## Other Apple behaviour changes

- **A failed readiness probe means not running**
  ([D38](deviations.md#d38--a-failed-readiness-probe-means-not-running)). Each probe returns about 20 MB of stdout. On a
  host or link too slow to deliver that within `shell` (30 s by default), every probe now times out, where Kotlin counted
  the marker once about 80% had arrived. A simulator then takes up to four setups of two `bootWait`s each, about 26 to
  30 minutes per simulator with the defaults, before it is out of the run. That holds while another simulator keeps the pool
  alive, and leaves out the waits for shutdown and for the session to close. On the last device the run ends after about 6
  to 13 minutes, with `remaining tests will not run`. Raise `shell` if that happens. A configured
  `onPrepare: [ERASE]` erases on each re-setup, as on any setup.
- **A test's own text does not abort its batch**
  ([D39](deviations.md#d39--a-tests-own-text-does-not-abort-its-batch)). A failure text in a test name, an activity line,
  a Swift Testing event or an application's log line no longer reports. In an assertion it reports only when the message
  starts with it, which is how a failure the framework records reads. That rule is reasoned from Xcode's formats, not
  recorded. After a prefix such as `XCTAssertTrue failed - `, that test fails and the batch goes on. If a batch now waits
  for its timeout (`testOutputTimeoutMillis`, 300 s by default, once per batch, and three in a row with no test passed or
  failed set the simulator up again), or a sick simulator keeps taking batches, look at the failing line: the
  [deviations guide](deviations.md#d39--a-tests-own-text-does-not-abort-its-batch) lists the known misses.

## Other Apple worker behaviour changes

These fix bugs that Kotlin has, and need no configuration change.

- **A rejected command frees its SSH session slot**
  ([D29](deviations.md#d29--a-rejected-command-gives-its-session-slot-back)). The command still fails with
  `Request failed`; the commands after it no longer fail for want of a slot. A command with no reply in 30 s is closed the same way, and
  an error line starting `No reply to exec` names it. A channel that a worker opens after its 30 s are up is closed then, and the line
  starts `No reply to opening`.
- **Closing a session takes at most 30 s, and can kill the process**
  ([D30](deviations.md#d30--closing-an-ssh-session-has-a-limit)). A process that ignores TERM gets KILL after 20 s. A
  server that ignores signals can leave it running; an error line starting `Abandoning session` then names the command.
  On iOS an abandoned session retires its simulator, which ends a pool whose last device it was.
- **An rsync send that stops moving fails after 300 s**
  ([D31](deviations.md#d31--an-rsync-send-that-stops-moving-fails)). If you wrap rsync with `RSYNC4J_RSYNC`, the wrapper now
  also gets `--timeout=300` on sends. On a host with rsync 3, so does the worker's `rsync --server`: check any forced
  command or wrapper there that matches that string. A worker that is silent for more than 5 minutes during a send now fails
  that send. One stalled send takes 300 s. The installer makes three attempts and the
  device retries a failed install up to 30 times, so one simulator can spend about 7.6 hours on a worker that never answers,
  and N simulators on one worker about N times that, because sends to one worker run one at a time. A second worker that
  finishes the queue ends the run. With a link that dies during a send and the default keep-alive, each send ends after
  about 60 s, which is still about 1.6 hours per simulator, and later attempts can also wait in ssh's connect for the host's
  TCP timeout, because `--rsh` sets no `ConnectTimeout`. Kotlin waits for ever.
  An rsync that reports the stall in other words than `poll: timeout` or `io timeout after`, with a hung ssh, still waits.
  A process left holding a receive's output can hold it up for at most 5 s.
- **On a password-authenticated worker, a pull that takes over 30 s is now really cut**
  ([D31](deviations.md#d31--an-rsync-send-that-stops-moving-fails)). Kotlin's TERM at the 30 s deadline reached only `sshpass`, so rsync
  carried on in the background and the file arrived in the end, although the pull had returned `false`. The video and device log listeners ignore
  that result, so a video or device log that took over 30 s to pull still turned up in the report. Now it is missing, with `.rsync-partial/<name>` left in the output
  directory. Key authentication has always behaved this way. The 30 s is not configurable. If videos or device logs go missing on such a worker, look for
  that file: the cure is a faster link or smaller files.
- **A connection lost after login is a connection failure**
  ([D36](deviations.md#d36--a-connection-lost-after-login-is-a-connection-failure)). On iOS the simulator is
  reconnected; on macOS the device leaves the run. For a registered iOS simulator whose worker stays down, the reconnect
  has no limit and goes on until the run ends, as in Kotlin.
- **One unread output stream no longer kills the connection**
  ([D37](deviations.md#d37--one-unread-output-stream-no-longer-kills-the-connection)). The command blocks until its
  output is read, times out or is closed.
- **xcodebuild's command line on a worker starts with `env NSUnbufferedIO=YES`**
  ([D32](deviations.md#d32--xcodebuild-on-a-worker-runs-unbuffered)). Check anything on the worker that matches the command, such
  as a forced command. Output arrives as it is produced, so `testOutputTimeoutMillis` measures real silence.

## Trial and rollback

Retain the previous executable and its configuration, inputs and state snapshots.
Choose separate output directories and isolated analytics backends for trials.
For Apple, disjoint simulators on the same worker are not isolation: fixed paths
under `/tmp/marathon`, shared bundles, `marathon.xctestrun` and rsync `--delete`
can conflict. Concurrent comparisons require disjoint workers. Sequential reuse
requires restoring all of `/tmp/marathon` including `shared/`, resetting simulator
state and verifying that leftover test/recording/log processes have exited.
macOS comparisons require a full host snapshot restore, including TCC databases
and test-created state, or disjoint hosts. Android comparisons use disjoint
devices and clean tracing/recording state between runs. Follow the complete
[isolation contract](../PLAN.md#67-shadow-runs-on-the-device-farm-then-cutover).

To roll back, stop the native run and finish its cleanup, restore the isolated
state/snapshots, and point PATH or the CI executable back to the retained JVM
launcher with its original runtime prerequisites. Keep native artifacts separate
for diagnosis. Selecting the old executable alone does not undo worker changes.
Shadow and canary execution belongs to the release rollout, not this documentation
check; no concurrent shared-worker safety is claimed.

### Apple code coverage

Build the app and test bundles with `xcodebuild build-for-testing
-enableCodeCoverage YES`. Set `isCodeCoverageEnabled: true` and point
`vendorConfiguration.bundle.xctestrun` to the xctestrun produced by that build,
relative to the Marathonfile. The runner copies coverage targets into each
generated xctestrun, relocates products to their installed bundles and stages
static libraries on the worker. Each batch writes coverage into its xcresult;
merging coverage across batches remains a consumer step.

With coverage disabled, `bundle.xctestrun` is ignored, including a missing file.
With coverage enabled, a supplied path must be a file and must contain coverage
buildable infos. Enabling coverage without supplying that path emits one warning
per process; the generated xctestrun has no coverage targets.
