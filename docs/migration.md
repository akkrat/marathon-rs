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
| N4 | Android preparation clears the whole external-storage root, for example `/sdcard`. Use dedicated test devices/state. | Confirmed |

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
