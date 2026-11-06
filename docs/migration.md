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
| A1 | Dynamic-provider EOF or errors other than connection refused stop updates permanently; this is distinct from recovery of an individual worker. | Seeded; read-side oracle evidence exists, but completed native provider parity is not claimed here. |
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
  bounds only requests without a timeout of their own. A value picked with Kotlin's unit bug in mind is now taken
  as written.
- **An adb server restart no longer ends device tracking**
  ([D22](deviations.md#d22--a-run-survives-an-adb-server-restart)). A loopback server is started again.
  A server still away after `waitForDevicesTimeoutMillis` (30 s by default) loses its devices. Remote test parsing
  waits for the server too; if it never returns at that stage, the run fails with `NoDevicesException` when
  `deviceInitializationTimeoutMillis` has passed.
- **A file sync push that never verifies fails after `pushFile`**
  ([D23](deviations.md#d23--waiting-for-a-pushed-file-has-a-limit)).
- **`networkState` can read `DISCONNECTED`** in a lost device's per-test JSON
  ([D24](deviations.md#d24--a-lost-android-device-stays-out-of-use-and-says-so)). A consumer that assumed it was
  always `CONNECTED` should be checked.
- **Failed instrumentation streams are reported** with their cause
  ([D25](deviations.md#d25--a-failed-instrumentation-stream-is-reported)). Uncompleted tests may carry that text
  instead of the generic one, `No test results` or none, and a remote parse cut off by a lost connection is retried
  instead of giving a shorter test list. A run that used to pass with tests silently missing can now run them all, or
  fail.
- **DEX 040 and 041 test APKs are accepted**
  ([D26](deviations.md#d26--dex-040-and-041-test-apks)).
- **Each instrumentation runs only its own tests**
  ([D28](deviations.md#d28--each-instrumentation-runs-only-its-own-tests)). This only matters for batches
  that mix test APKs, which takes `class-name` batching and the same class name in both.

## Other Apple behaviour changes

- **A failed readiness probe means not running**
  ([D38](deviations.md#d38--a-failed-readiness-probe-means-not-running)). Each probe returns about 20 MB of stdout. On a
  host or link too slow to deliver that within `shell` (30 s by default), every probe now times out, where Kotlin counted
  the marker once about 80% had arrived. A simulator then takes up to four setups of two `bootWait`s each, about 26 to
  30 minutes per simulator with the defaults, before it is out of the run. Raise `shell` if that happens. A configured
  `onPrepare: [ERASE]` erases on each re-setup, as on any setup.
- **A test's own text does not abort its batch**
  ([D39](deviations.md#d39--a-tests-own-text-does-not-abort-its-batch)). A failure text in a test name, an activity line,
  a Swift Testing event or an application's log line no longer reports. In an assertion it reports only when the message
  starts with it, which is how a failure the framework records reads. That rule is reasoned from Xcode's formats, not
  recorded. After a prefix such as `XCTAssertTrue failed - `, that test fails and the batch goes on. If a batch now waits
  for its timeout, or a sick simulator keeps taking batches, look at the failing line: the
  [deviations guide](deviations.md#d39--a-tests-own-text-does-not-abort-its-batch) lists the known misses.

## Other Apple worker behaviour changes

These fix bugs that Kotlin has, and need no configuration change.

- **A rejected command frees its SSH session slot**
  ([D29](deviations.md#d29--a-rejected-command-gives-its-session-slot-back)). The command still fails with
  `Request failed`; the commands after it no longer fail for want of a slot.
- **Closing a session takes at most 30 s, and can kill the process**
  ([D30](deviations.md#d30--closing-an-ssh-session-has-a-limit)). A process that ignores TERM gets KILL after 10 s. A
  server that ignores signals can leave it running; an error line starting `Abandoning session` then names the command.
- **An rsync send that stops moving fails after 300 s**
  ([D31](deviations.md#d31--an-rsync-send-that-stops-moving-fails)). If you wrap rsync with `RSYNC4J_RSYNC`, the wrapper now
  also gets `--timeout=300` on sends. A worker that is silent for more than 5 minutes during a send now fails that send.

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
