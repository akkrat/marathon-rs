# Real-host end-to-end preparation and runbook

WP 7.2 prepares **16 required scenario/profile pairs**, with **28 pair executions**
under the approved repetition policy (56 CLI runs, plus independent discovery and any extra Kotlin volatility witnesses).
The [ledger](../../tools/real-e2e/scenarios.json) names every test-method intent,
parser, transport and independent artifact witness. The
[coverage file](../../fixtures/real-e2e/coverage.json) currently marks every pair
**blocked** pending published coverage. Preparation used no live workers or
devices; the decision-9 follow-up inspected only retained captures and xcresults. `scripts/check.sh` only validates offline inputs and guard tests.

The driver builds, stages, preflights and collects bounded production CLI
captures. Decision 7 makes `compare --release --policy …` an **outcome-level**
pair gate. Decision 9 defines its exact semantic projection: exit, keyed test
results/counts, console verdict/counts, JUnit and Allure semantics, failure
locations, xcresult counts/issues and artifact kinds per test, with P3 media
sanity. It reuses 7.1's lossless evidence envelope and validation. Raw evidence
is immutable; `qualify` and release comparison use the same extraction function.
Command/transfer/ADB causal parity stays in 7.1b. A real transport recorder is
not required. Passing one pair does not certify the 16-pair/28-execution matrix.
`compare` without `--release` remains the strict raw diagnostic comparator.

## Operations and private configuration

All commands below run from this checkout, with `python3` (3.10 or newer,
including `tarfile`'s data extraction filter) and the documented Rust PATH.
`run validate` and `stage-inputs` are offline. `qualify` and `compare` read
retained captures; media and xcresult projection also runs local read-only artifact
inspectors (ffprobe/ffmpeg/xcresulttool), never a worker or device. `build`, `preflight`,
`capture-pair` and recovery `cleanup` require `--approved`; that flag records
orchestrator authorization, it does not obtain approval from the user.

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
python3 -I tools/real-e2e/run validate
python3 -I tools/real-e2e/test_real_e2e.py
umask 077
cp tools/real-e2e/templates/host.example.json tools/real-e2e/local.json
chmod 600 tools/real-e2e/local.json
"$EDITOR" tools/real-e2e/local.json
```

The example contains deliberately invalid placeholders, never usable default
endpoints. Fill the stable private `worker_id`, actual worker account HOME,
JBR 21.0.10, frozen Kotlin source, production JVM/native binaries and the
applicable SDK/runtime/device type. Paths must be absolute. For iOS video set
`ffmpeg` to the worker's actual executable. Decision 8 approves `no-sorting`: the frozen CLI has **no lexical strategy**.
No additional `sorting_review` setting is required. No fake lexical option or production scheduling change is
introduced. `environment` allows only DEVELOPER_DIR/ANDROID_HOME/ANDROID_SDK_ROOT/MARATHON_XCTEST_PARSER_DIR. A native capture of an `xctest` scenario needs the last one: the directory holding the real library from the host's Kotlin distribution (D20), so both sides run the same file.
The CLI gets a fresh HOME/TMPDIR and an explicit environment allowlist; replay
tapes, telemetry, analytics backends and inherited JAVA_OPTS are absent.

For `linux-kvm`, also set `exclusive_android_worker: true`, `reservation_review`,
non-default `adb_port`, an even `emulator_port` in the supported console range,
and an **installed** `system-images;android-<API>;...;x86_64` image, API ≥34.
This implementation supports a reserved, otherwise empty KVM worker. It rejects
attached USB devices, existing adb/emulator processes, occupied selected ports,
and an occupied default ADB port. A network/device namespace is not provisioned
by this driver. No SDK/image install, license acceptance, Docker or system
service change is automated. Private AVD/HOME state is created per invocation.

For `ssh-26.6`, configure only the approved loopback forward, public-key account,
absolute key/known-hosts paths, and remote `worker_python`/`worker_checkout`.
Strict host-key verification and a real `exit 37` probe are mandatory. No
password or outer access-service exit status is accepted. The worker must have
this exact harness revision; its source hashes are checked before provisioning.
The remote build config may omit SSH fields because building on the worker
requires no onward SSH connection. Keep it mode 0600 too.

## Source selection (WP 7.5)

The frozen baseline (`e47352f2`) is the default of every command. The release source (the SHA in
`docs/release-source.sha`) is chosen only by `build --source release`; that command verifies the commit in the Kotlin
checkout, records its full SHA, `source_kind` and the sample tree hashes (selected and frozen, with `matches_frozen`) in
`build.json`, and refuses a build whose sample tree differs from the frozen one. `stage-inputs` copies `build.json` into
the hashed input tree, so every capture of those inputs is bound to the source. `--source` on `stage-inputs` and
`capture-pair` only asserts what the record says. The JVM distribution proves its own source through a record written by
`run reference --source … --oracle-clone … --jvm …` from the receipt that `tools/oracle/build.sh` writes after a verified,
successful build (source, tree hash with patches, installed jar hashes); the record embeds the receipt and is named by the
host config's `jvm_reference`. Every new capture, frozen or release, requires it and re-hashes the distribution against it.
Every new capture writes `source`, `source_kind` and `jvm_reference_source` into `capture.json`; a release capture also
writes `source` into the contract and `source_sha` into the evidence provenance (7.1's envelope then records it as the
evidence source). The reader checks the retained `in/build.json` against the hashed input inventory and rejects any
disagreement; a capture without the WP 7.5 fields is read only if its evidence digest is on the audited list
`tools/real-e2e/legacy-captures.json` of retained 7.2 captures. `qualify` and `compare --release` reject
captures, contracts or witnesses that name different sources, so a policy qualified on one source cannot release a pair of
the other. The sweep's steps live in `tools/real-e2e/sweep/` (see the WP 7.5 findings).

## Ordered preflight checklist

1. Obtain the host-specific approvals described below. Reserve the **entire**
   Apple worker, including `/tmp/marathon/shared`, or the empty Linux KVM worker.
   The reservation is advisory between cooperating operators; no other Marathon
   run may start during it. Do not use a shared farm worker.
2. Build the reference with `tools/oracle/build.sh reference` on the relevant
   CLI host (JBR 21.0.10), and the native binary with
   `cargo build --release -p marathon-cli --bin marathon`. These are the public
   production CLIs, not 7.1's hook/fake probe. Record the actual source SHA; run
   the ordinary gate first. The oracle builder verifies its frozen source and
   selected patch before Gradle. Do not edit the read-only source.
3. Review the private config and source hashes. Check installed tools/runtime,
   signing and an unlocked GUI login session for macOS UI execution. No signing
   override is supplied: build/launch failure remains a blocker. This phase has
   not demonstrated that fresh client HOME works with each host's real local
   CoreSimulator/XCTest environment; retain any failure rather than silently
   inheriting desktop state.
4. Run the scenario's `preflight` command. It is read-only apart from its private
   evidence directory. Apple checks exact Xcode profile, SDKs, rsync and simulator
   inventory. macOS additionally requires **both** system/user TCC `access`
   row-count queries through `sqlite3 -readonly`, and actual `csrutil status`.
   If the terminal lacks permission, stop; do not grant Full Disk Access, change
   SIP/TCC, reset permissions or substitute a zero count. Counts are the approved
   evidence, not proof that every row's content is identical.
5. `/tmp/marathon` must be absent. An existing directory, even empty, or a symlink
   blocks. The driver does not clean a pre-existing baseline. A retained
   `/tmp/marathon-real-e2e.lock` blocks a new run. Resolve it through its owner,
   never by broad removal. This is the intentionally narrow empty-baseline
   profile; arbitrary baseline backup/restore is not implemented.
6. Build samples and stage immutable product copies, preserving links/modes.
   Record build commands, tool versions, source/config/product hashes and DEX
   magic. DEX 040 stays visible as N10, not silently upgraded parser semantics.
7. Run Kotlin discovery, review its actual names/counts, and freeze selection.
   A7 Objective-C identities come from Kotlin. Android omitted ignored tests need
   an explicit parser-specific explanation in `excluded_methods`; no other test
   omission is implied. Rust must use the same frozen selection and product bytes.

## First host: local iOS on this Mac, Xcode 27

Start with `ios-unit-nm` locally. It needs **no remote approval**. The
orchestrator runs the following already-authorized local steps; `--approved`
acknowledges that ownership, not a new permission request. The implementer does
not run these commands. Keep `/tmp/marathon` exclusively reserved and absent,
and use fresh directories. iOS does not depend on readable desktop TCC databases.
Do not put the macOS TCC preflight ahead of this first witness.

After the common private-config setup above, run these commands **in order**.
Set `local.json` to `local-27`, this worker HOME, its installed runtime/device type,
the JBR and frozen source, and these absolute JVM/native binary paths. The native
build uses the default target directory here; adjust its config path if your
build environment overrides `CARGO_TARGET_DIR`.

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
python3 -I tools/real-e2e/run validate
scripts/check.sh
tools/oracle/build.sh reference
cargo build --release -p marathon-cli --bin marathon
# jvm in local.json: $PWD/tools/oracle/build/marathon-src/cli/build/install/marathon/bin/marathon
# native in local.json: $PWD/target/release/marathon
export MRS_HOST_FILE="$PWD/tools/real-e2e/local.json"
export MRS_RUN="$PWD/tools/real-e2e/runs/local-27-first"
export MRS_SCENARIO=ios-unit-nm
export MRS_INPUTS="$MRS_RUN/ios-unit-inputs"
export MRS_DISCOVERY="$MRS_RUN/ios-unit-discovery"
export MRS_PAIR="$MRS_RUN/ios-unit-pair-1"
export MRS_RERUN="$MRS_RUN/ios-unit-pair-2"
python3 -I tools/real-e2e/run preflight --host "$MRS_HOST_FILE" --scenario ios-unit-nm --output "$MRS_RUN/preflight-ios" --approved
python3 -I tools/real-e2e/run build --host "$MRS_HOST_FILE" --vendor iOS --output "$MRS_RUN/ios-build" --approved
python3 -I tools/real-e2e/run stage-inputs --build "$MRS_RUN/ios-build" --application Debug-iphonesimulator/sample-app.app --tests Debug-iphonesimulator/sample-appUnitTests.xctest --output "$MRS_INPUTS"
python3 -I tools/real-e2e/run capture-pair --host "$MRS_HOST_FILE" --scenario "$MRS_SCENARIO" --side jvm --discover --inputs "$MRS_INPUTS" --output "$MRS_DISCOVERY" --approved
cp "$MRS_DISCOVERY/discovery/in/report/discovery.yml" "$MRS_INPUTS/discovery.yml"
shasum -a 256 "$MRS_INPUTS/discovery.yml"
cp tools/real-e2e/templates/selection.example.json "$MRS_INPUTS/selection.json"
"$EDITOR" "$MRS_INPUTS/selection.json"
python3 -I tools/real-e2e/run capture-pair --host "$MRS_HOST_FILE" --scenario "$MRS_SCENARIO" --side jvm --inputs "$MRS_INPUTS" --output "$MRS_PAIR" --approved
python3 -I tools/real-e2e/run capture-pair --host "$MRS_HOST_FILE" --scenario "$MRS_SCENARIO" --side jvm --inputs "$MRS_INPUTS" --output "$MRS_RERUN" --approved
cp tools/real-e2e/templates/outcomes.example.json "$MRS_RUN/outcomes-draft.json"
"$EDITOR" "$MRS_RUN/outcomes-draft.json"
python3 -I tools/real-e2e/run qualify --reference "$MRS_PAIR" --rerun "$MRS_RERUN" --draft "$MRS_RUN/outcomes-draft.json" --output "$MRS_RUN/outcomes.json" --mismatch "$MRS_RUN/jvm-mismatch-1"
# Complete the hash-bound artifact witness reviews described below before release comparison.
"$EDITOR" "$MRS_PAIR/jvm/witnesses.json" "$MRS_RERUN/jvm/witnesses.json"
python3 -I tools/real-e2e/run capture-pair --host "$MRS_HOST_FILE" --scenario "$MRS_SCENARIO" --side native --inputs "$MRS_INPUTS" --output "$MRS_PAIR" --approved
"$EDITOR" "$MRS_PAIR/native/witnesses.json"
python3 -I tools/real-e2e/run compare "$MRS_PAIR" --release --policy "$MRS_RUN/outcomes.json" --mismatch "$MRS_RUN/native-mismatch-1"
python3 -I tools/real-e2e/run capture-pair --host "$MRS_HOST_FILE" --scenario "$MRS_SCENARIO" --side native --inputs "$MRS_INPUTS" --output "$MRS_RERUN" --approved
"$EDITOR" "$MRS_RERUN/native/witnesses.json"
python3 -I tools/real-e2e/run compare "$MRS_RERUN" --release --policy "$MRS_RUN/outcomes.json" --mismatch "$MRS_RUN/native-mismatch-2"
```

The stage command uses the frozen sample's Debug simulator product names; verify
`ios-build/build.json` before it. If this Xcode emits another directory, select
its actual recorded paths, never a stale product. The selection editor step
records exact discovered names/hash/review. The policy editor step inspects the
**two JVM captures only** and adds a review reference and any P3 media bounds
they justify. Required report/artifact kinds belong in `scenarios.json`. A failed qualification retains its
mismatch directory: revise the draft after inspecting that difference and use a
new mismatch/output path. Do not run Rust until JVM/JVM qualification succeeds.
The two Kotlin runs above are the two P2 references, not extra replacements.

After the local iOS witness, use the common sequence for local UI scenarios,
then macOS. macOS runs directly under decision 6, with read-only TCC counts and
SIP before/after, no security changes or prompt approvals:

```sh
python3 -I tools/real-e2e/run preflight --host "$MRS_HOST_FILE" --scenario macos-unit-nm --output "$MRS_RUN/preflight-macos" --approved
python3 -I tools/real-e2e/run build --host "$MRS_HOST_FILE" --vendor macOS --output "$MRS_RUN/macos-build" --approved
```

The iOS build archives only `sample/ios-app` from `e47352f2` into scratch,
queries workspace schemes, builds `UITesting` for a generic simulator, then
builds `sample-appUnitTests` **only if present**. Missing hosted or UI products
block. The macOS build copies the maintained project and adds only
`testBoundedSlow` to the disposable hosted target; it emits a marker and sleeps
for at most 120 seconds. The existing sample and copied fixtures stay unchanged.
Both builds retain signing defaults and use the actual host's toolchain.

Stage each scenario's matching application/test bundle. The following variables
are relative to the recorded `build.json.products_path`, selected after checking
the actual build (hosted `.xctest` for xctest; UI runner `.app` for xcuitest):

```sh
python3 -I tools/real-e2e/run stage-inputs --build "$MRS_BUILD" --application "$MRS_APPLICATION_PRODUCT" --tests "$MRS_TEST_PRODUCT" --output "$MRS_INPUTS"
```

Do this for `ios-unit-nm`, `ios-ui-video`, `ios-ui-screenshot`, `macos-unit-nm`,
`macos-ui-xctest`, `macos-cancel`, `macos-permissions`. Reuse identical build
products on that host; create separate input directories for unit/UI selection.
Then follow the common pair sequence below for every scenario.

## Remote Mac: Xcode 26.6 over the approved forward

**Approval:** explicit user approval is required before any remote contact,
forward establishment, checkout transfer, read-only preflight, sample build or
execution. The orchestrator supplies all private endpoint/account/key values.
No Remote Login is enabled on either Mac. Build on the remote Xcode 26.6 host,
then run the paired CLIs on the controller with **real SSH transport to that
same worker**, using the same products; do not compare local-27 to ssh-26.6.

If the approved forward is not already running, establish it in an owned
terminal (variables are runtime-only values supplied by the orchestrator):

```sh
ssh -N -L "127.0.0.1:${MRS_FORWARD_PORT}:127.0.0.1:${MRS_REMOTE_SSH_PORT}" "$MRS_FORWARD_ACCESS"
```

In the controller terminal verify exit propagation before any further work:

```sh
ssh -T -p "$MRS_FORWARD_PORT" -i "$MRS_KEY" -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$MRS_KNOWN_HOSTS" "$MRS_ACCOUNT@127.0.0.1" 'exit 37'
printf 'ssh probe rc=%s (must be 37)\n' "$?"
```

Use that same approved route to install this reviewed checkout/harness on the
worker and open the worker's terminal. Do not transfer private keys. On the
**remote worker**, set `MRS_HOST_FILE` to its private build-only config and
`MRS_RUN` to its ignored private staging directory, then execute, in order:

```sh
python3 -I tools/real-e2e/run build --host "$MRS_HOST_FILE" --vendor iOS --output "$MRS_RUN/ios-build" --approved
python3 -I tools/real-e2e/run build --host "$MRS_HOST_FILE" --vendor macOS --output "$MRS_RUN/macos-build" --approved
python3 -I tools/real-e2e/run stage-inputs --build "$MRS_BUILD" --application "$MRS_APPLICATION_PRODUCT" --tests "$MRS_TEST_PRODUCT" --output "$MRS_INPUTS"
tar -C "$MRS_INPUTS" -cf "$MRS_INPUT_ARCHIVE" .
```

Repeat staging for the required hosted/UI products. Copy each **private input
archive** over the same approved forward (not `scp -r`, which follows links),
then extract into a new controller input directory:

```sh
scp -P "$MRS_FORWARD_PORT" -i "$MRS_KEY" -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$MRS_KNOWN_HOSTS" "$MRS_ACCOUNT@127.0.0.1:$MRS_INPUT_ARCHIVE" "$MRS_LOCAL_ARCHIVE"
mkdir "$MRS_INPUTS"
tar -C "$MRS_INPUTS" -xf "$MRS_LOCAL_ARCHIVE"
python3 -I tools/real-e2e/run preflight --host "$MRS_HOST_FILE" --scenario macos-unit-nm --output "$MRS_RUN/preflight-macos" --approved
python3 -I tools/real-e2e/run preflight --host "$MRS_HOST_FILE" --scenario ios-unit-nm --output "$MRS_RUN/preflight-ios" --approved
```

Here the controller's `MRS_HOST_FILE` is the `ssh-26.6` config with the approved
forward credentials and remote harness paths. Its `worker_id` must match the
remote build-only config. All subsequent worker operations use that forward;
the lease remains alive over one SSH session and cleans on EOF. The driver
repeats the exit-37 probe and harness hash check on every session. Raw worker
files remain on the worker under its ignored `runs/worker/<token>` directory.

## Linux: reserved KVM worker, no Docker

**Approval:** the orchestrator arranges Linux access and reserves an otherwise
empty worker with usable `/dev/kvm`, an installed SDK, JBR 21.0.10 and an
installed x86_64 image, API ≥34. Existing phones, emulators or ADB servers block;
there is no fallback to the default ADB port or this Mac. Preflight must be run
on the Linux worker, not through an arbitrary Android endpoint.

On the approved Linux worker, with its mode-0600 config and fresh `MRS_RUN`:

```sh
python3 -I tools/real-e2e/run preflight --host "$MRS_HOST_FILE" --scenario android-local-video-sync --output "$MRS_RUN/preflight" --approved
python3 -I tools/real-e2e/run build --host "$MRS_HOST_FILE" --vendor Android --output "$MRS_RUN/android-build" --approved
python3 -I tools/real-e2e/run stage-inputs --build "$MRS_RUN/android-build" --application debug/app-debug.apk --tests androidTest/debug/app-debug-androidTest.apk --output "$MRS_INPUTS"
```

The build archives the frozen sample, uses its wrapper/dependency declarations
and runs `:app:assembleDebug :app:assembleDebugAndroidTest`. No analytics settings
from the sample Marathonfile are reused. Retain `dex-magic.json` and actual
resolved dependency/build logs; cached dependencies and signing remain host
preconditions, not simulated successes.

For every discovery/side, the worker creates a new private AVD directory and
HOME, starts its own foreground `adb -L tcp:127.0.0.1:<reserved> nodaemon server`
as a tracked child, and starts its own emulator with the explicit console port.
Both receive the private server socket/port environment. The driver reads the
already-running server's `host:devices` directly (no auto-start), requires exactly
the owned serial in `device` state, and rechecks throughout the CLI invocation. The client receives a private SDK
whose adb executable is a rejecting tripwire: if the provider attempts its
`start-server` fallback, the capture fails without spawning an untracked daemon.
The worker uses the actual SDK adb executable for its already-owned foreground
server and boot probes; no successful adb response is simulated.
Every shell boot probe explicitly selects host, port and serial. Recreating the
AVD for Rust removes prior external storage/recordings, not just app data.
Validate emulator registration on the first host attempt: it is not certified
by these offline doubles. Any unexpected inventory aborts and is retained.

Run the common sequence for `android-local-video-sync` and
`android-remote-screenshots`, twice each. The templates use LOCAL_TMP push/pull,
TEST_RUN folder aggregation, coverage, ON_ANY media, and the exact remote
annotation listener. GIFs and adam screenshots are separate witnesses.

## Common discovery and pair sequence

Run this for each scenario/profile in the ledger. Variables below name new
private directories and the scenario ID, never an existing successful attempt.
Discovery itself owns and cleans a fresh resource invocation:

```sh
python3 -I tools/real-e2e/run capture-pair --host "$MRS_HOST_FILE" --scenario "$MRS_SCENARIO" --side jvm --discover --inputs "$MRS_INPUTS" --output "$MRS_DISCOVERY" --approved
cp "$MRS_DISCOVERY/discovery/in/report/discovery.yml" "$MRS_INPUTS/discovery.yml"
shasum -a 256 "$MRS_INPUTS/discovery.yml"
cp tools/real-e2e/templates/selection.example.json "$MRS_INPUTS/selection.json"
"$EDITOR" "$MRS_INPUTS/selection.json"
```

Set the shown discovery SHA, exact qualified names from Kotlin, review reference
and any explicitly reviewed Android parser omission. This review occurs **before
Rust**. Config templates use the frozen selection; they never infer identities
from Rust results. Once frozen, do not edit the input directory between sides.
Then capture Kotlin into `$MRS_PAIR` and `$MRS_RERUN` with the same frozen inputs,
qualify the semantic projection, and only then capture Rust. Use the exact first-host
sequence above with the current scenario/profile variables. Every scenario/profile
needs its own two JVM witnesses; rules do not transfer unexamined between hosts.
For cancel/permissions P2 requires only one Rust comparison; a second Kotlin run
is still needed to qualify the projection. Keep that additional witness recorded
separately from the required 28 pairs.

Kotlin must complete before Rust can start. Each side separately records worker
versions, ownership, CLI exit and watchdog, runner state before harness cleanup,
cleanup result and input hashes. Native capture rejects changed product/selection
bytes or a different worker, OS, Xcode, SDK, image hashes, ADB or emulator version. There
are no automatic retries. A runner exit 1 is valid for designated failing-test
workloads; a watchdog/cleanup failure is a harness failure, never an expected
test timeout. Unknown expected exits (Android setup/assumption behavior,
permissions, cancellation) come from the actual Kotlin capture. Ordinary runs
must produce raw test results. Permission captures must show the actual A18
refusal. Cancellation sends SIGINT only after the sample's emitted start marker,
then gives the runner 90 seconds (greater than its 70-second close budget).

The empty worker baseline is restored by retaining `/tmp/marathon` as the
owned worker evidence tree, then deleting only the recorded simulator or joining
only the spawned server/emulator children. No broad kill, simulator erase-all,
delete-unavailable, server kill command, SIP change, TCC write or global service
restart is used. An unexpected Apple test/tool process also retains the worker files in place;
the harness does not kill an unattributed process. A failed cleanup retains the
reservation and blocks reuse.
After a hard interruption, use the recorded token only after inspecting the
worker journal and ensuring the old supervisor/recorded children are gone:

```sh
python3 -I tools/real-e2e/run cleanup --host "$MRS_HOST_FILE" --run-id "$MRS_OWNED_TOKEN" --output "$MRS_CLEANUP_ATTEMPT" --approved
```

Recovery refuses a live or reused PID, a foreign token, or a simulator create
whose returned UDID was not journalled. It never guesses another resource to
remove. If any such check fails, the orchestrator must inspect and explicitly
resolve the owned resource; a stale lock is not permission to remove host state.

## Decision-9 projection and qualification

The private policy uses `format: "marathon-real-e2e/outcomes@2"`, a real `review`
reference, `rules` (only reviewed P3 media rules) and `deviations` (explicit
D-number annotations). Version 1 scalar/path patch policies are obsolete and
are rejected. Copy `templates/outcomes.example.json` and inspect two independent
JVM captures. The fixed projection itself implements decision 9; no per-field
clock/UUID/log patch list is needed. All scenario-specific expectations live in
`scenarios.json` under `projection`: required global kinds, required per-test
kinds, and whether complete selected-test/summary coverage is required.
Cancellation and permission-refusal scenarios permit partial results.

`outcomes.project` calls the same `semantic_projection` for every capture:

* CLI exit and summary counts by stream and pool, including raw attempt counts
  when present, plus execution verdict lines. Truncated summaries fail. Timing
  lines and diagnostic logger output remain only in raw evidence.
* Tests keyed by fully qualified name: final reported status, attempt count,
  ordered attempt status/ignored/success flags, and ordered per-test failure
  class/message/source locations. No attempt deduplication occurs. Failure
  classes are null when the report contains none. Source locations retain the
  filename suffix and starting/ending lines; checkout prefixes are discarded.
  Java-style source frames also retain filename and line. Failure messages are
  retained, including multiline messages.
* JUnit testcase multiset, suite counts, failure/skip/error elements, attributes,
  messages and source locations. Testcase order, time and timestamp are ignored;
  duplicate cases and element order are retained.
* Allure result multiset keyed by test identity, preserving name/fullName,
  status and statusDetails message, and attachment counts by MIME type,
  including nested steps. File names and UUIDs do not identify results.
* HTML per-test status/failure data and report presence. Failure/report payloads
  are read as data, never executed as JavaScript.
* xcresult root and action test/failure/skip counts and every issue's test,
  message and location. Reconstruct each retained bundle in a private temporary
  directory and use `xcrun xcresulttool get --legacy --format json`; read each
  action's `testsRef` too and compare test identifiers/statuses. Bundle UUIDs,
  reference IDs, destination identities and clock fields do not enter the
  projection. Empty, linked or unreadable bundles fail. Test-batch IDs associate
  each bundle with its per-test artifact roles without comparing the IDs.
* Global and per-test report/artifact kind sets. Per-test JSON and Allure
  attachments must resolve to real captured files. Empty report directories
  do not satisfy required kinds. Screenshots and deterministic pulled file-sync
  bytes remain exact; video/GIF uses the reviewed P3 policy below.

No projected field is excluded by the current policy. `deviations` entries
have exactly `number` and `reason`; the number must be D1–D16 with an existing
PLAN §3.11 row. D3's serializer delegate and D12's environment identity hashes
are annotated as raw-only differences for the first witness. An annotation
cannot remove or transform a projected field. A future projected-field exception
needs an explicit, source-reviewed implementation naming its approved D-number;
arbitrary exclusion paths and scalar replacement rules are rejected.

Qualification requires matching input/selection/host/toolchain, distinct run IDs,
identical JVM jars/JDK/patches and **equal semantic projections**. It writes both
JVM evidence hashes and a fingerprint of the extraction code, scenario and
policy. Release revalidates the original evidence, fingerprint, JVM/JVM equality
and media evidence tags before comparing JVM/native. Changing a scenario option,
projection or media bounds requires qualification again. Raw captures are never
rewritten. Passing one pair does not close the full live matrix.

`--mismatch DIR` now writes `comparison.json` on success as well as failure:
`expected` and `actual` contain the projections, `differences` contains exact
field/value differences, and `raw_differences_informational` lists raw manifest
path/hash/size/mode/link differences. Raw differences **do not gate**. Extraction
failures retain their error and raw diagnostic section. Choose a new directory
for every attempt; previous reports and release receipts remain immutable.
`compare` without `--release` still uses 7.1's strict raw comparator.

P3 rules specify `id`, `kind: "media"`, a raw path/glob, `reason`, exact
`expected: {"codec_name":…, "width":…, "height":…}`, `duration_seconds: [min,max]`
and `frame_count: [min,max]`. Each media file must match exactly one rule.
For GIF also supply `gif_loop`, `gif_extensions`, `gif_disposals` and
`gif_delay_centiseconds`. Full ffmpeg decode and ffprobe frame counting check
nonempty playable media; bounds never widen automatically. Qualification binds
observed digest/duration/frame-count tuples from both JVM captures to each rule.
The projection compares codec/dimensions, rule role and associated tests. The
witness review below compares ordered visual states. Missing tools or failed
inspection blocks comparison. Inspectors have a 180-second per-command timeout
and operate only on retained artifacts, never workers/devices.

The retained `local-27-ios26` first witness now qualifies pair 1 versus pair 5,
and both release comparisons pass under decision 9 (all three commands rc=0).
Their projections have zero differences; raw differences remain informational.
See [the findings](../wp/7.2-findings.md#first-real-witness--retained-pairs-1-and-5-2026-10-31)
for policy hashes, exact command forms, scope and classifications.

For **each side**, create `jvm/witnesses.json` or `native/witnesses.json` after
reviewing the retained reports and artifact references. The review is an explicit
human observation, not an inferred success from the CLI exit:

```json
{
  "review": "actual-review-reference",
  "evidence_sha256": "SHA256-of-this-side/evidence/evidence.json",
  "witnesses": {
    "pass-fail-skip": {"observation":"actual identities and counts checked", "paths":["reports/test_result/raw.json"]},
    "failure-enrichment": {"observation":"actual enriched failure and attachment checked", "paths":["actual/report/path"]},
    "xcresult-readable": {"observation":"actual extracted failure/attachment data checked", "paths":["actual/xcresult/path"]}
  }
}
```

Compute the hash with `shasum -a 256 "$MRS_PAIR/jvm/evidence/evidence.json"`
(and the corresponding native/rerun path). Include exactly the scenario's
`witnesses` keys from `scenarios.json`, meaningful observations and existing raw
manifest paths. For every matched raw media file add
`media_landmarks: {"actual/raw/media/path": ["first observed app state", "next observed app state"]}`.
The two implementations must have the same ordered states for the same owning
test, attempt and recording role (decision 16: a recording that no test attempt
owns is rejected, and two recordings cannot trade places); the reviewer inspects
actual decoded frames, and the digest binds that review to their bytes. This
human part of P3 is not an automated pixel-similarity claim. The release command
checks review completeness/hash/path existence and mechanical media sanity.
It also requires the two retained Kotlin reruns, comparison success and complete
cleanup. Successful output is an immutable `pair/release.json` with policy and
both evidence hashes. Missing/changed reports, files or outcomes fail; no
transport recorder condition remains.

## Witness review, evidence and release handoff

Retain raw reports/media/xcresults, process streams, input manifests, JVM jar and
patch hashes, native binary/source SHAs, worker journals and before/after SIP/TCC
counts on the run host. The client adapter stores original report bytes in 7.1's
lossless gzip content-addressed envelope. Failed attempts and mismatch directories
are immutable; choose new paths. Worker trees can be large and stay external.
Never commit these private raw directories or host configs.

For each pulled xcresult and media artifact, use the host's corresponding tools
with paths selected from the actual report references, saving diagnostics:

```sh
xcrun xcresulttool get --legacy --format json --path "$MRS_XCRESULT" > "$MRS_XCRESULT_JSON"
ffprobe -v error -show_streams -show_format -of json "$MRS_MEDIA" > "$MRS_MEDIA_METADATA"
mkdir "$MRS_DECODED_FRAMES"
ffmpeg -i "$MRS_MEDIA" "$MRS_DECODED_FRAMES/%06d.png"
```

Verify failure enrichment, actual test execution/counts/statuses, all attachment
references, file-sync bytes against the original two inputs, coverage output,
video dimensions/codecs/ordered app states, GIF frame timing/loop/disposal and
readable xcresults. Set concrete live-media bounds **from Kotlin**, then obtain
review; do not invent tolerances from native results. Keep A19 leftovers, A20
complete log variants and any A47/A38/A39 failure visible. No blanket absent-log,
pixel-tolerance or xcresult-ignore rule is authorized.

Publication is manual after review: commit compact redacted derivatives under
`fixtures/real-e2e/<profile>/<scenario>/`, document every typed private-field
mapping and original/external artifact hash, retention location, source/toolchain
revision, normalization contract and reviewer. Do not redact messages/statuses
or strip evidence. Absent/expired external artifacts cannot
satisfy release acceptance. Update provenance and extend the offline validator
with the reviewed real-capture schema before changing coverage rows to passed.
`compare --release` now records a passed pair in `release.json` only after the
qualified outcome comparison and scenario witness reviews pass. The committed
coverage ledger remains blocked until the orchestrator publishes real evidence
and all required repetitions; no offline test certifies a host.
