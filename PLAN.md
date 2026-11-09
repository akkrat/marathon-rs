# marathon-rs: plan for rewriting a marathon fork in Rust

**Revision 4.1, accepted as the implementation baseline by Sol in round 5.** It addresses the Sol reviews (rounds 1–3) and the Muse audits (rounds 1 and 2). All of them are summarised in §12.

**Source of truth:** the Kotlin checkout (`$MARATHON_KT`), the fork's branch. The port is frozen at `e47352f2`, which is 165 fork commits on top of upstream `develop`. Fork-specific behaviour is described in the fork's change notes. Later the fork's branch commits go into a port queue, and the queue must be drained before release (§10).

## 1. Goal and constraints

- A single native `marathon` binary that replaces the JVM distribution.
- **External interface unchanged, except for the deviations of §3.11 (D47 requires a finite `uncompletedTestRetryQuota`):** Marathonfile, Marathondevices and the dynamic-provider protocol, the CLI, exit codes, the output-directory layout and file formats, and the commands sent to workers.
- **Scope:** all three vendors in v1 (iOS, macOS and Android). All three are release-gated on real end-to-end runs (§6.6).
- **Phases 0–7 used bug-for-bug parity by default**; from Phase 8, §3.12 supersedes conflicting baseline rules in §3.1–3.11. Intentional deviations are listed in §3.11; smaller approved simplifications are recorded in each WP's findings.

## 2. What is being ported

| Kotlin module | Main LOC | Test LOC | Rust crate |
|---|---|---|---|
| configuration | 2.9k | 1.6k | `marathon-config` |
| core | 8.6k | 8.3k (+ integration tests) | `marathon-core` |
| report/html-report, execution-timeline | 0.15k + prebuilt JS/HTML | – | `marathon-core::report` (assets embedded as committed) |
| vendor-apple/base | 8.3k | 5.4k | `marathon-apple` |
| vendor-apple/ios | 3.6k | 5.8k | `marathon-ios` |
| vendor-apple/macos | 0.85k | 0.5k | `marathon-macos` |
| vendor-android | 4.2k | 2.7k | `marathon-android` |
| adam 0.5.8 (ADB client library) | external | external | `marathon-adb` (new) |
| dex-test-parser 2.3.4, axmlparser 1.0 | external | external | `marathon-dex` (new) |
| vendor-test | 0.35k | – | `marathon-test-support` |
| cli | 0.26k | – | `marathon-cli` |
| analytics/usage, bugsnag | 0.2k | 0.02k | no-op (D1) |

In total that is about 29k LOC of Kotlin main code and about 30k LOC of tests. The expected Rust size is 40–50k LOC of main code plus a comparable amount of tests.

Every Kotlin main source file is assigned to a work package in `docs/source-inventory.csv`, which `scripts/source_inventory.py` generates.
- It already exists and covers all 505 main source files, including `buildSrc` and `sample`. Every file has an owner.
- The files marked `n/a` each carry a reason: telemetry (D1), the dead `ScpFileBridge`, a JDK 8 `ByteBuffer` shim, Gradle build logic, and the sample apps.
- The xcresult codegen in `buildSrc` belongs to 4.6.
- The script fails if any file has no owner, and it runs in the gate.

## 3. Compatibility contract

**Exception:** D47 refuses a Marathonfile whose `uncompletedTestRetryQuota` is unlimited, which Kotlin accepted (§3.11). It is the one deliberate exception to the rule that the Marathonfile format stays as it is.

Each subsection becomes an executable test suite. "Identical" means byte-identical after the normalisation defined in §6.4.

### 3.1 CLI: an argument and exit matrix

The surface:
- root `marathon [-m|--marathonfile F] [--analyticsTracking B] [--bugsnag B]`, which runs when no subcommand is given;
- `run [same options]`;
- `parse [-m|--marathonfile F] [-o|--output NAME] [-f|--include-flaky-tests B]`;
- `version`.

Parsing rules:
- Root options are **not** passed down to subcommands.
- Booleans use Kotlin `String.toBoolean()`, so only `true` (any case) is true.
- The parser is hand-written to reproduce Clikt 5 behaviour and texts where the matrix shows it (decided in WP 2.5: every visible text is custom, so clap adds nothing).

The matrix records stdout, stderr and the exit status separately for each case:
- help and `-h`;
- unknown, missing and repeated options; option placement;
- missing Marathonfile and config errors, which happen **outside** the catch that prints "crashed" in Kotlin;
- no devices, no tests, and parse mode with and without `-o`;
- `ignoreFailures`;
- a crash inside `run`;
- `version`, which logs a startup line in the startup log format before printing the bare version.

The final messages, with the stream they go to:

| Situation | Message | Stream | Exit |
|---|---|---|---|
| Success | `Marathon execution finished` | stdout | 0 |
| Failures with `ignoreFailures` | `Marathon execution finished with failures (Failures suppressed because ignoreFailures is \`true\`)` | stderr | 0 |
| Failures | `Marathon execution failed` | stderr | 1 |
| Crash | `Marathon execution crashed` | stderr | 1 |

### 3.2 Configuration: three deserialization profiles and staged validation

**P1: Marathonfile** (Jackson YAML, Kotlin module, `SerializeModule`, JavaTimeModule). The contract is specified as Jackson's token and binding behaviour over SnakeYAML events, not as generic "YAML 1.1":
- **Scalars:** SnakeYAML 2.2's YAML 1.1 resolution (`yes/no/on/off`, `500_000`, `0x`/`0b`/leading-`0` octal, `~`), as tokenised by Jackson. A plain empty value (`key:`) is a null token; only a quoted empty value is `""`. Other cases measured on the real jars: `0o17`, `08` and `1:20` stay strings; `.inf` and `.nan` are tokenised as floats but fail to bind; an empty first document makes `readValue` return null (a crash path, kept as is). See `docs/research/yaml.md`.
- **Coercion:** a quoted number is accepted for a number field, a scalar for a string field, a float for an int; an integer is accepted as an enum ordinal.
- **Aliases:** they arrive as strings. `<<` is an ordinary key, and is rejected or ignored depending on the deserializer.
- **Other syntax:** duplicate keys, tags, multiple documents and mapping-key spelling follow what SnakeYAML and Jackson do. Spike 0.3b measures this.
- **Unknown keys and null:**
  - unknown keys are rejected, except in the six hand-written deserializers: `influxdb` (and its retention policy), `graphite`, `execution-time` sorting, `probability` flakiness and `fixed-size` batching;
  - the last three look keys up with a recursive `findValue`;
  - `readOnly` is silently dropped for `influxdb` and `graphite`.
- **Explicit `null` depends on the field type** (`NullIsSameAsDefault=false`, `FAIL_ON_NULL_FOR_PRIMITIVES` off):
  - a Kotlin primitive field (`Int`, `Long`, `Double`, `Boolean`) binds `null` to its zero value, even when it has a default;
  - a non-null reference field rejects `null`;
  - a nullable field takes `null`;
  - collections and maps follow the oracle.

  The oracle settles each type with corpus cases for boolean, int, long, double, reference, collection and map fields.
- **`timeLimit` is specific to each field:**
  - the three hand-written deserializers catch only `InvalidFormatException`, then retry the value as a Duration relative to an injectable "now";
  - when that also fails, `fixed-size` yields `null`, `execution-time` throws `"bbb"` and `probability` throws its own message;
  - `success-rate` has no custom deserializer, so its value must be an absolute Instant.
- **Durations and Instants:** ISO-8601 Durations, a bare number means seconds; Instants as ISO strings or epoch numbers.
- **Paths:** `File` paths resolve against the Marathonfile's directory. The Apple re-resolution rules apply: `canonicalFile` for `devices`, `knownHostsPath` and the SSH key. A missing `devices` defaults to `<dir>/Marathondevices`.
- **Derived defaults:** for example `xcresulttool = max(shell, 120 s)` and `bootWait = max(boot, 180 s)`. They are computed from the values actually supplied.
- **Enums:** case-sensitive, except `serialStrategy` and the two `preferableRecorderType` enums. `watchOS`, `tvOS` and `visionOS` are aliases of `iOS`.
- **Android SDK:** found from `androidSdk`, then `ANDROID_HOME`, then `ANDROID_SDK_ROOT`.

**P2: Marathondevices and dynamic updates.** A separate mapper: Kotlin module only, with **no** `FileDeserializer` and no JavaTimeModule. A relative `key` path in transport authentication therefore resolves against the process working directory. Short ids are expanded: `deviceType` becomes `com.apple.CoreSimulator.SimDeviceType.*` and `runtime` becomes `com.apple.CoreSimulator.SimRuntime.*`.

**P3: output YAML** (`parse -o`). Jackson YAML generator: a `---` document start, double-quoted strings, NON_NULL. The file is `<outputDir>/<name>.yml`, even though the help text says `.yaml`.

**Env substitution truth table** (commons-text `StringSubstitutor`), produced by the oracle with a controlled environment:
- `${V}`, `${V:-d}`, `$${V}`;
- a variable that is undefined, empty, or recursive;
- a cycle, which raises an `IllegalStateException` *outside* the `ConfigurationException` wrapper;
- `${A_${B}}`.

The same substitution runs over P1 text, P2 files and every dynamic update. Its failures fall outside each profile's error wrapper.

**Staged validation.** Each stage is kept, with its timing and side effects:

| Stage | When | What it does |
|---|---|---|
| (a) Parse time | `ConfigurationFactory.parse` | Apple `bundle.validate()`, which **extracts `.ipa`/`.zip` files and deletes the extraction directory first**; the Android SDK lookup; vendor post-processing (`xcresult.pull` → `pullingPolicy`) |
| (b) Logical validation | Inside `Marathon.run` | `LogicalConfigurationValidator` and the vendor `validate()` calls. The macOS branch does **not** call `MacosConfiguration.validate()` |
| (c) Provider time | Device provider start | Marathondevices errors (`NoDevicesException`) |

The oracle records which stage rejected a config, the exact message (for custom messages), the exit behaviour, and the filesystem changes.

### 3.3 Dynamic-provider protocol (iOS)

- **Transport:** a TCP client to `deviceProvider.host:port`.
- **Frame:** `[4 bytes ignored][int32 big-endian, signed length][payload]`. The payload is Marathondevices YAML after substitution, parsed with profile P2.
- **Buffer:** starts at 4096 bytes and grows by powers of two up to 2^20. A larger or negative length fails through the buffer-limit handling.
- **Snapshots:** each frame is a full snapshot. Workers and devices missing from it are removed.
- **Errors:**
  - connection refused is retried every 5 s;
  - any other error, including EOF and invalid YAML, stops the producer for good (the Kotlin bug is kept, D5).
- **Static versus dynamic:** if a devices file exists, the static provider is used even when the config says `dynamic`.

Tests cover split and coalesced frames, zero, negative and oversized lengths, partial EOF, invalid YAML and the static-file precedence.

### 3.4 Output directory: complete manifest

Every artifact family is compared, not only a sample.
- **Directory rules:**
  - `outputDir` is wiped only on the `run` path, after parsing; `parse` never wipes it;
  - the directory manifest itself is compared, including the empty `screenshot/` and `logs/` folders the HTML reporter creates.
- **Reports:**
  - `devices/<pool>/<safeSerial>.json` and `bill/<pool>/<safeSerial>.json`;
  - `tests/<pool>/marathon_junit_report.xml` (StAX, no indentation, Java `Double.toString` times, CDATA split around `]]>`, the flaky/rerun failure rules);
  - `test_result/raw.json` and `test_result/<pool>/<safeSerial>/<escaped>.json`;
  - `allure-results/<uuid>-result.json` and `environment.xml`. These are written by the allure-java-commons lifecycle with its own Jackson model, not by Gson. `environment.xml` comes from `Configuration.toMap()`, whose values are Kotlin `toString()` output with secrets masked;
  - `html/**`: the same committed `app.min.js`/`.css` and `index.html` template, with placeholders filled exactly (`${relative_path}`, `${data_json}`, `${log}`, `${date}` in UTC), and the JSON models with their quirks (`isTable`, `total_flaky = 0`);
  - `html/timeline/**`: copied assets plus `${dataset}`.
- **Per-test files** from vendors: `logs/`, `video/`, `screenshot/`, `device-logs/`, `xctestrun/`, `profiling/` and `device-files/`.
  - FileManager escaping and `maxPath`/`maxFilename` truncation;
  - the `<batchId>.<batchId>` naming bug;
  - the `jpg` suffix for webp and gif screenshots.
- **JSON rules:**
  - Gson rules: HTML escaping, nulls omitted, declaration order, `File` written as an absolute path;
  - leaked fields: `isTimeInfoAvailable` stays; `safeSerialNumber$delegate` is handled by D3.
- **Media:**
  - the Android screenshot GIF uses the configured `width`, `height` and `delayMs`, with the frame timing, loop and disposal metadata of `GifSequenceWriter`;
  - videos are compared by decoded content and metadata, not bytes.

### 3.5 Logs: startup and runtime phases

- **Startup phase,** before vendor configuration (from `cli/logback.xml`):
  - pattern `%d{HH:mm:ss.SSS} [%thread] %-5level %logger{36} - %msg%n` to stdout;
  - root level DEBUG; `io.netty` and `io.vertx` at ERROR.
- **Runtime phase,** after `configureLogging()`. The vendor configurator replaces the appenders:
  - Android, and Apple without compact output: `%highlight(%.-1level %d{HH:mm:ss.SSS} [%thread] <%logger{40}> %msg%n)`;
  - Apple with `compactOutput`: `%highlight(%.-1level [%thread] <%logger{48}> %msg%n)`;
  - Apple also forces some sshj loggers and `TimelineSummaryProvider` to ERROR;
  - `%highlight` goes through Jansi's stream. In Jansi's default mode it writes ANSI codes on a terminal and strips them when output is redirected to a pipe or a file. The tests cover terminal, pipe and file output under each Jansi mode.
- **Level semantics:**
  - **Every** `MarathonLogging.logger()` call, including a lookup of an existing named logger, sets that logback logger's level to `level ?: (debug ? DEBUG : ERROR)`, so the last call wins;
  - a logger last requested before `configureLogging()` keeps DEBUG until it is requested again;
  - the tests repeat the same lookup before and after configuration;
  - `Timings` is fixed at INFO, with `(+N ms since start)`;
  - `Xcodebuild` is fixed at WARN.
- **Logger names** stay the Kotlin names (e.g. `com.malinskiy.marathon.Marathon`, abbreviated as logback does), so users' greps keep working.
- **`println` output:** the progress lines `%02d%% | [pool]-[serial] <test> passed|failed|ignored|incomplete`, the stdout summary, and TeamCity service messages.
- **Debug-only behaviour:** the shutdown hook that runs `onFinish` on Ctrl-C is installed only with `debug: true`.

### 3.6 Commands sent to workers

**Same commands.** Every argv, shell string and timeout is the same, checked by comparing command transcripts (§6.4). This covers:
- Apple tools: `simctl`, `xcodebuild` (with the Xcode ≥ 14 `-collect-test-diagnostics never` and Xcode ≥ 16 `xcresulttool --legacy` branches), `xcresulttool`, `nm` pipelines, `lipo`, `/usr/libexec/PlistBuddy`, `sqlite3`, `/usr/sbin/system_profiler`, `ioreg`, `plutil`, `ffmpeg`;
- file transfer: `rsync` (exact flags and the `--rsh` string with `ServerAlive*`) and the base64 `WriteFile` command;
- Android: `adb` shell commands.

**Same SSH quirks.**
- `env` and `workdir` are ignored over SSH.
- The `shellCommand` quoting is bug-compatible.
- Each command opens its own exec channel.
- Signal TERM gives exit 143; a timeout gives null.
- Keep-alive: `KEEP_ALIVE` semantics, with sshj's whole-second interval truncation.

### 3.7 Scheduling semantics

- **`Test` equality and hashing use only pkg/clazz/method; metadata is ignored.** This is frozen separately from identity (`===` on `Device`, `DeviceActor`, `TestBatch`) and from the serialized form.
- **Java `PriorityQueue`.** We port the heap of the pinned reference JDK, including `removeAll`/bulk removal, iteration order and `toList`.
- **Comparator call order is observable.** The lazy random keys in `random-order` and the memoised or observed-duration keys in `execution-time` change results depending on which comparisons run first.
- **Ported exactly:** the actor state machines, the retry, flakiness and quota accounting (including the phantom `AddRetry` quirk), and the fork's "retries prefer another worker".

### 3.8 Analytics backends

Parity covers the whole backend lifecycle, not just the line protocol:
- **Influx v1:** database and retention-policy check and creation, batching, flush on close.
- **Influx v2:** organisation lookup and bucket creation.
- **Graphite:** plaintext writes and render-API queries.
- **All three:** the queries, the 60 s network timeouts, retries (`withRetry(3, 50 ms)`), caching per `(percentile, limit)`, and the fallback to `NoOpMetricsProvider(backendUnavailable)` when setup throws.

Request transcripts are compared against real Influx v1/v2 and Graphite services (§6.3).

### 3.9 Android contract with code on the device

The instrumentation arguments that marathon passes come from adam's `android-testrunner-contract`. They are read by test-side libraries, so the names and values are an external contract:
- the `ArgumentsFactory` output;
- the test-access arguments: adb reverse port, emulator console port and token, gRPC host and port (console port + 3000);
- coverage and `class` arguments;
- `-e log true` and the `TestAnnotationProducer` listener argument for remote parsing.

Feature negotiation must match adam: shell v2, sync v2, `cmd` and `abb_exec` choose the code path. So must request generation and the translation of exceptions into device states.

### 3.10 Accepted differences (same behaviour, different text)

- **Parse errors:** the wording of errors raised inside Jackson. The Rust text names the file, the key path and the cause. The `ConfigurationException` wrapper and every custom validation message stay identical.
- **Thread names** in `[%thread]`.
- **Stack traces:** JVM frames in marathon-internal stack traces. Traces that come from xcodebuild, xcresult or instrumentation output are kept as they are.

### 3.11 Deliberate deviations (approved 2026-10-12)

| # | Kotlin behaviour | Proposal |
|---|---|---|
| D1 | Bugsnag reporting and Grafana Cloud usage metrics (hard-coded token), on by default | Accept the flags and config keys; send nothing. Kotlin logs `Init BugSnag` or `Noop exceptions reporter started` at INFO in the startup log phase. In the same place Rust logs one INFO line saying that crash reporting and usage analytics are not supported by this build and that `--bugsnag`, `--analyticsTracking`, `bugsnagReporting` and `analyticsTracking` are ignored |
| D2 | `MARATHON_OPTS` / `JAVA_OPTS`; the Windows start script that the Gradle `application` plugin generates | Ignored. No Windows build |
| D3 | Gson leaks `safeSerialNumber$delegate`, with a non-deterministic value, into `devices/*.json`, `bill/*.json` **and every per-test `test_result/**.json`** (through `TestResult.device`) | Omit it everywhere. Keep `isTimeInfoAvailable` |
| D4 | Influx/Graphite access through Java client libraries | Our own HTTP and TCP clients with the same lifecycle, requests and timeouts (§3.8) |
| D5 | Known Kotlin bugs (e.g. `clazz == pkg` lookup, no dynamic-provider reconnect after EOF, TeamCity auto-detect checking env values) | **Most are kept.** Every one is listed in `docs/known-kotlin-bugs.md` (Kotlin location, behaviour, WP, status). Implementers and reviewers add to the list as they find more. D14–D19 and D21–D55 fix the ones they name, and the ledger marks each of those rows as fixed; the others remain a backlog |
| D6 | Kotlin compiles user-supplied regexes (test filters, `excludeSerialRegexes` and the other regex config fields) with `java.util.regex` | They are compiled with the Rust `regex` crate instead: its syntax, Unicode classes and linear-time matching. `matches` stays a full match, and `find`, `split` and `replace` keep Kotlin's semantics around the match. A pattern the crate rejects (lookaround, backreferences, possessive quantifiers and other Java-only syntax) fails at config load, with an error naming the pattern and field. Decided by the user 2026-10-23, after spike 0.3c and WP 1.4 showed that exact Java parity costs more than the few users need. Supersedes the earlier "reject only what the backend can't express" D6 and its runtime-budget extension |
| D7 | When a worker command dies from a signal other than TERM, sshj sends a stale TERM request on the already-closed channel; OpenSSH treats it as a protocol error and drops the whole connection (every simulator on that worker loses its connection, batches are rescheduled, the worker is re-provisioned) | Not reproduced: the command still reports no exit code, but the other channels on the worker survive (approved 2026-10-12; `docs/research/ssh-spike-results.md`, R2) |
| D8 | Kotlin formats the HTML summary footer date (month names) and the progress and file-transfer percentages and decimals (`String.format`) with the JVM default locale, taken from the user's machine (e.g. `janv.` and `2,68` on fr) | Always formatted as en-US, whatever the machine locale; on an en-US machine the output is identical to Kotlin's. Pinned-locale outputs (JUnit, Graphite) are unaffected (approved 2026-10-23; WP 1.2) |
| D9 | Jackson binding edge cases in Marathonfile and Marathondevices that only malformed or exotic input reaches: creator-time precedence for a duplicated key carrying `null`; out-of-range epoch or duration numbers wrapping (to the epoch, `PT0S` or a flipped sign); the exact exception class (`InvalidFormatException` vs `MismatchedInputException`) for blank or scalar-root documents; non-ASCII digits in quoted numbers and ordinals; lone surrogates in keys, paths and regexes; a regex written in the object form `{pattern, options}`; `null` elements inside collections, which Kotlin accepts and then fails on downstream (F10) | Duplicated keys: last wins, then nullability is checked. Out-of-range numbers fail at load with a clear error; in-range nanosecond precision stays exact. Error classes are only distinguished where the binder gets them for free; WP 2.6 compares the stage and accept/reject, not the class. Coercions take ASCII digits only (the token layer keeps Java's digit rules). Keys, `File` paths and regexes must be valid UTF-8; string values that reach output stay lossless. The regex object form `{pattern, options}` is rejected with a clear error naming the field; only string patterns are supported. A `null` collection element is rejected at bind (WP 2.2, S4), except in a filter `values` list (`SingleValueTestFilter`/`AnnotationFilter`), where it is dropped instead: Kotlin only calls `contains`/`intersect` on those lists, so a null element never matches and matching is identical; One exception (WP 2.2 review round 2, recorded in WP 2.4): `TestPackageFilter` with `subpackages: true` tests `startsWith("$it.")`, so a `null` element becomes `"null."` and would match packages starting with `null.` — effectively unreachable, since `null` is a reserved word in Java and Kotlin; the dropped-null behaviour stands. Kotlin's `toMap()`/config-dump would still show the dropped nulls (WP 2.2 review round 1, noted for WP 2.4). Approved 2026-10-24 for WP 2.1 under the user's pragmatic-parity direction (spike 0.3b rounds 2–3) |
| D10 | Apple command output accepts arbitrary JVM charsets and decodes each read chunk independently; the reader uses InputStream.available() | UTF-8 with replacement only (charset API slot retained), decoded incrementally across chunks. Async pipe polling retains the shared labelled clock, +20 ms idle grace, bounded line channels and observable PollBackoff. Approved 2026-10-24 in WP 4.1 decisions P1/P4/P6. Unused `produceLines` is omitted (P3); total-timeout process survival and await's cancellation control flow are retained |
| D11 | adam decodes streamed adb text (shell, logcat) chunk by chunk, so a UTF-8 character split across reads becomes replacement characters, and its vert.x transport uses pooled buffers and busy-spinning | Streamed text is decoded as UTF-8 across reads; shell v1 bytes pass through raw. No vert.x buffer pools or busy-spin; the per-read chunk caps stay. Approved 2026-10-24 in the WP 6.1 decisions (P2, P3). Extended 2026-10-25 by WP 6.5 P4: logcat keeps partial lines across chunks instead of Kotlin’s per-chunk `lines()` |
| D12 | The JVM runtime shows through in CLI output: the `SLF4J(I): Connected with provider…` line on stderr in every run, `\tat` stack frames in uncaught-exception traces, run-specific identity hashes (e.g. in the `version` startup line), `@argfile` expansion and the `_MARATHON_COMPLETE` completion script from Clikt, help wrapped to the terminal width on a TTY | Not reproduced: no SLF4J line; errors print class, message and `Caused by:` lines without stack frames (external tool traces are still printed as received); identity hashes use the stable class-name rule approved for F29; no `@argfile` or completion script; help always uses the 79-column layout. Approved 2026-10-24 in the WP 2.5 decisions (P1–P5). Extended 2026-10-25 (WP 3.10 P1/P3): the `System Information:` block lists a fixed set of OS facts instead of JVM system properties, and SIGINT/SIGTERM go through a native signal relay (no JVM shutdown-hook scheduling or registration failures), with exit statuses matching the JVM captures |
| D13 | Strategy history queries occur at the first comparison or batching lookup | Prefetch each pool's declared history keys off core when its queue is built, through the run-wide memoising MetricsProvider (deduplicated by `MetricsKey`, single-flight across concurrent pool builds), so there is still one query and TimingLog line per key per run as in Kotlin, whose `Marathon.kt:180–181` coverage report already queries the ExecutionTime keys before any pool exists. Comparators and batching then read memory synchronously on core. An extra query/timing line appears only for SuccessRate sorting of a pool with fewer than 2 tests whose limit differs from the flakiness limit (or flakiness `Ignore`), and when the filtered shard is empty. The flakiness key keeps Kotlin's timing (3.4a/3.10 at `Marathon.kt:167/180`, ProbabilityBased only, non-empty shard). Per-test warnings retain lazy lookup order. Approved 2026-10-24 in WP 3.2 decision 3 (P2), superseding 0.6 D4 for queue construction; async MetricsProvider stays frozen |
| D14 | Android sizes boot/install/per-device/logcat thread pools, mutates the shared file-sync set during setup, and propagates reverse-removal failures from dispose | Tokio vendor tasks replace those pools; `bootWaitingThreads` and `installThreads` remain accepted but do not size pools or reject zero. A provider-owned effective file-sync set contains user entries, Allure and coverage in insertion order with set deduplication; downstream listeners read that set. Reverse-removal failures are logged and disposal remains joinable/idempotent. Disconnected devices stay under D5; adb-serial map collisions were fixed by D18. Approved 2026-10-25 by WP 6.5 P1/P3/P5a |
| D15 | Failed Android remote-parsing attempts leave log accumulators registered and instrumentation producers running; an execution after hook that throws skips channel cancellation | Always unregister the remote accumulator and cancel/join owned instrumentation producers on every exit, including a throwing after hook. Preserve exception precedence, successful argv, callback order, and first-writer result completion. Approved 2026-10-25 by WP 6.6a P2, with JVM leak witnesses (N36/N37) |
| D16 | Android artifact listeners block on 300 ms process-kill sleeps, race recorder state, and leave profiling running after run failure | WP 6.6b P1/P3/P5: VendorClock delay with the same attempts/commands, direct screenshot deletion after joined completion, and synchronized recorder state. P4: stop/join perfetto on run failure without pulling/removing its trace. Batch-owned final cleanup cancels/joins artifact jobs, including a dropped execute waiter; no remote cleanup commands are added for video/GIF cancellation. Profiling setup failures still fail the batch (P2 rejected). Approved 2026-10-26; JVM evidence in `fixtures/oracle/android-artifact-probes.jsonl` |
| D17 | The iOS screen-recording stop (`kill -s INT $(cat pidfile) && lsof -p … +r 1`) reports failure when the recorder has already exited, which is the usual case on current Xcode (it exits 6–31 ms after SIGINT, before lsof's first pass). The pidfile then stays and the next test's recording waits 10 s and is lost (A96) | After a failed stop, one extra worker probe `sh -c "ps -p $(cat <pidfile>) -o pid="` runs. Exit 1 with empty output (pid no longer exists) counts the stop as successful and removes the pidfile; a live pid, a probe error or a reused pid keeps the existing warn-and-return-false path. The stop command itself is unchanged and the stopped/attachment logic stays as ported. Approved 2026-10-31; real-host probe in `docs/wp/5.3-findings.md` (U100) |
| D18 | The Android provider keeps its `devices` map keyed by adb serial alone (`AdamDeviceProvider.kt:160,165-171,185-192`). Large farms run several adb servers (hosts) that all expose `emulator-5554`/`emulator-5556`, so one host's DISCONNECTED can dispose the healthy same-serial device of another host (its batch is returned and it never works again), while the dropped device's own actor stays in the pool and its reconnect is ignored as "already present" (N27) | The map is keyed by (adb server endpoint, serial): a state change on server A affects only A's device. The dropped device is disposed, its reconnect gets a fresh actor that receives work, and B's healthy device keeps working without a returned batch. The `borrow` replay key stays the bare serial unless several servers expose it, then `serial@server`. The per-server state tracker, the pool/actor identity (`host:port:serial`), log text and, until D24 reversed it on 2026-11-06, the retention half of N27 (disconnected entries stay in the map; borrow can return a disposed device of the same server) are unchanged. Approved 2026-11-02 (user decision); evidence: x1 Kotlin/Rust CLI probes in `docs/wp/x1-findings.md` and `fixtures/fake-adb/android-reconnect/expected/cross-host-shadowed-kotlin.json` (the witness of the old behaviour), implementation in `docs/wp/x2-findings.md` |
| D19 | The Apple command reader (`extensions/Reader.kt:56,92,98`) checks `available()` and, when it is 0, then `canRead()`: `cmd.isOpen && !cmd.isEOF` over SSH (`SshjCommandExecutor.kt:51-52`), `process.isAlive && !exitCode.isCompleted` locally (`KotlinProcessCommandExecutor.kt:70-71`). Output that arrives together with EOF or the exit between those two calls is never read: the command completes with its exit status and without (some of) its stdout or stderr, e.g. a successful `env` without `HOME` fails simulator setup (U145, A100) | Once `can_read` turns false, the reader reads whatever is still buffered, until a read finds nothing, before draining its line buffer. That last read neither updates the shared last-output time nor checks the idle timeout. Every other part of the ported reader is unchanged: polling and backoff, idle expiry and its discarded tail (A24), detached drain collectors (A26), polling past pipe EOF while `can_read` is true (A25), and error and cancellation ordering. Approved 2026-11-03 (user decision); evidence: `tools/oracle/reader-race-probe` (Kotlin's own reader loses both lines), regression test `crates/marathon-apple/tests/reader.rs::output_arriving_with_eof_between_an_empty_read_and_can_read_is_still_delivered`, `docs/wp/x9-findings.md` |
| D20 | `XCTestParser.kt:61,64` copies `/libxctest-parser/$platform/libxctest-parser.dylib` from the program's own resources to a temp file and pushes it (`:68`). Those five libraries are third-party software under their own EULA (`vendor/vendor-apple/base/src/main/resources/EULA.md`), not part of the GPL-licensed code | The program neither contains nor ships the library. An `xctest` run reads `<platform name>/libxctest-parser.dylib` from the directory named by `MARATHON_XCTEST_PARSER_DIR`, a copy the user obtains under the EULA from a Kotlin distribution. The vendor checks it during construction: macOS for `MacOSX`, iOS for any simulator platform, because a device's platform is only known later. `extract` then checks the device's platform once before its three attempts. A construction failure is a `ConfigurationException` on the uncaught path (exit 1, as `ApplicationView.kt:79`); a failure at `extract` is a `TestParsingException` on the path of any parsing failure (`Marathon.kt:92`). Everything after the read is the port. `nm` runs never read the variable. `testParserConfiguration` keeps its format. Approved 2026-11-04 (user decision); evidence: `crates/marathon-cli/tests/xctest_parser_supply.rs`, `crates/marathon-apple/tests/xctest_parser.rs`, `tools/differential/test_evidence.py` (`test_d20_*`), `docs/wp/x11-findings.md` |
| D21 | adam passes `socketIdleTimeout` in milliseconds to vert.x `setIdleTimeout(int)`, which counts seconds (`VertxSocketFactory.kt:39-40`, N12): PT30S lasts 30,000 s, and nothing ever idles out | The idle timeout counts the milliseconds it is given; 0, negative values and `Long.MAX_VALUE` mean no limit. Requests bounded by a configured timeout or a watchdog (every shell command under `shell`, including `pm clear`, `rm` and md5; screenrecord; install, push, pull and list; screen capture; port forward; getprop during boot; the device list during `waitForDevicesTimeoutMillis`), and streams silent by nature (track-devices, logcat, `am instrument`, perfetto) run with no idle limit, so that the limit can never fire before their timeout. `socketIdleTimeout` then bounds the requests with no bound of their own: `host:version`, device features, later getprop, and removing a reverse forward at dispose. No configuration field added. Approved 2026-11-06 (user decision); evidence: `crates/marathon-adb/tests/client_fake_adb.rs::idle_timeout_counts_milliseconds_d21`, `crates/marathon-android/tests/device_test.rs::silent_requests_under_a_timeout_outlast_the_idle_limit_d21`, `docs/wp/x12-findings.md` |
| D22 | When `host:track-devices` fails because the adb server went away, the merged stream cancels `providerJob`, and its surrounding `while (isActive)` loop never restarts (`AdamDeviceProvider.kt:123-182`, N16): the run no longer sees any device change | Each server is tracked by its own task, with one server's failure leaving the others alone. It reconnects with a capped backoff (1 s to 8 s) on the vendor clock, through the path initial setup uses, so a loopback server that refuses the connection is started again. During an outage the devices keep their actors and new batches wait. After the reconnect a device from before the outage counts as present until it is listed or `waitForDevicesTimeoutMillis` passes; one still not listed is then disconnected normally, one listed keeps its actor with no event (a pool ends with its last device, so removing and adding it would end the run) and has its logcat stream and reverse forwards restored (an emulator that itself restarted during the outage cannot be told apart and is not prepared again), and a device new to the list, or whose setup the outage interrupted, goes through the normal connect path. A server not back within `waitForDevicesTimeoutMillis` has its devices disconnected, which is what ends a run whose server never returns; retries continue until provider termination cancels them. A device loss reported by a batch while the server is away, or until its list has settled after it returned, fails only that batch. Remote parsing waits at the same gate: an attempt that failed because the server is away (`host:version` fails, or the gate is closed) does not count as one of its three, which have no delay, and it runs again once the server is back and its device listed. A device given up uses up its attempts at once, so at this stage a run whose server never returns ends with `NoDevicesException` when `deviceInitializationTimeoutMillis` (180 s by default) passes, unless another server's device can parse. An emulator that restarted during an outage is not prepared again, so its batches fail on the missing APKs and, with the default unlimited `uncompletedTestRetryQuota`, repeat until something else ends the run (a known limit). An attempt whose output went silent sends no probe. Approved 2026-11-06 (user decision); evidence: `crates/marathon-android/tests/provider_test.rs` (`*_d22`), `crates/marathon-android/tests/reconnect_test.rs::a_run_survives_an_adb_server_restart_d22`, `::a_run_ends_when_its_adb_server_never_returns_d22`, `docs/wp/x12-findings.md`, `docs/wp/x18-findings.md` |
| D23 | `waitForRemoteFileSync` (`BaseAndroidDevice.kt:199-214`, N5) polls md5 with no bound of its own; a file sync push has no other bound than the batch timeout | The wait is bounded by `timeoutConfiguration.pushFile` for every caller and fails with a `TransferException` naming the file. Legacy install's enclosing timeout has the same length and started earlier, so it fires first and its `Timeout transferring` message stays. No configuration field added. Approved 2026-11-06 (user decision); evidence: `crates/marathon-android/tests/installer_test.rs::md5_poll_is_bounded_by_the_push_timeout_d23`, `::legacy_install_keeps_its_message_when_the_md5_never_matches_d23` |
| D24 | The provider's map retains disconnected devices, so `borrow` can return a disposed device (`AdamDeviceProvider.kt:160,185-192`, the retention half of N27), and `networkState` is frozen as CONNECTED at construction (`AdamAndroidDevice.kt:453-459`, N29) | A disconnected device leaves the map, so `borrow` waits for a live one under its caller's timeout. `networkState` and `healthy` follow the live state, and a lost device object stays lost after its serial reconnects as a new object. D18's identity and reconnect behaviour is unchanged. Output changes only for a device lost by then: `networkState` in per-test `test_result` JSON reads `DISCONNECTED` and `healthy` stays false; `devices/*.json` and `bill/*.json`, from the connect event, are unchanged. iOS and macOS devices still report CONNECTED (A72). N27's other half (Disconnected is emitted even if setup never emitted Connected) is kept. Approved 2026-11-06 (user decision); evidence: `crates/marathon-android/tests/provider_test.rs::multi_server_disconnect_keeps_identity_and_borrow_skips_the_lost_device_d24`, `docs/wp/x12-findings.md` |
| D25 | Instrumentation producers run detached, and `receiveCatching().getOrNull()` discards the channel's close cause (`AndroidDeviceTestRunner.kt:81-83`, `AmInstrumentTestParser.kt:143-146`, N38): handshake EOF, a closed connection or a parser exception look like a normal end | The failure reaches the listener as a test-run failure, `Instrumentation failed: <class>: <message>`, and is logged, so the batch's unfinished tests take the retry path with that text. It loses the device only for the classes that do so in a synchronous request (`execution_error`), so a parser exception cannot retire a healthy device. A client closed by provider shutdown and cancellation stay quiet (N19); a test crash or a non-zero instrumentation exit is unchanged. A remote parsing attempt fails with the cause instead of returning the tests found so far. A stream that ends without `INSTRUMENTATION_CODE`, which `am instrument -r` prints last for every instrumentation that ran, is such a failure too, with the text `Instrumentation failed: java.io.EOFException: the instrumentation stream ended before a result` and the exit code if the shell reported one. A dropped connection ends the shell stream normally, so without this, a parse returned a partial list and a run could exit 0 with tests missing. Two ends without the code are not failures and keep their answer: `am` refusing to start an instrumentation (`INSTRUMENTATION_FAILED`), and output that never was the protocol (`No test results`). The text also reaches the test in progress, which has no result of its own, and only that instrumentation's. A test that finished before the cut stays finished. Approved 2026-11-06 (user decision); evidence: `crates/marathon-android/tests/android_runner.rs` (`producer_*_d25`, `*stream_cut_off*`, `ordinary_ends_*`), `crates/marathon-android/tests/remote_parser.rs` (`producer_failure_fails_the_parsing_attempt_d25`, `*cut_off*`, `ordinary_ends_are_not_retried_d25`), `crates/marathon-android/tests/reconnect_test.rs::a_run_does_not_take_a_cut_off_parse_for_the_list_d25`, `crates/marathon-adb/src/instrumentation.rs` (`*_d25`), `docs/wp/x12-findings.md`, `docs/wp/x18-findings.md` |
| D26 | dex-test-parser 2.3.4's magic check takes versions 035 to 039 only (`spec/DexMagic.kt`, N10), so test APKs in DEX 040 or newer are rejected | DEX 040 and 041 are accepted. 040 changes nothing the parser reads. 041 is parsed as a container: every image's header and id tables are read, with all offsets relative to the file, so every class is found, and a malformed container is a `DexException`. Anything newer keeps the existing `Invalid dexMagic` message. Approved 2026-11-06 (user decision); evidence: `crates/marathon-dex/tests/dex_versions.rs` over real d8 output (`tools/oracle/dex/build-dex-versions.sh`, `fixtures/PROVENANCE.md`), compared with the same sources in 039; the JVM row `probe-dex040.apk` in `fixtures/oracle/dex/probes.jsonl` stays as the witness |
| D27 | `prepare()` runs `rm -r $EXTERNAL_STORAGE` and `mkdir $EXTERNAL_STORAGE` (`AdamAndroidDevice.kt`, N4), wiping the whole external-storage root, for example `/sdcard`, on every prepare | Neither command runs. Test suites that rely on an empty external storage must clear it themselves. File sync pull entries are still cleared per batch and pushed files replaced; a pushed folder's destination is now removed first, since a leftover from a run cut short would otherwise stay what the tests read, with the push nested inside it. Videos left by an aborted earlier run now stay, because their names hold that run's batch id, which a later run cannot know; only a broad pattern over user data could find them. The same goes for a video interrupted by a lost connection, in a run that still exits 0. The comparators leave out exactly these two commands of JVM records and of native records pinned from before D27. Approved 2026-11-06 (user decision); evidence: `crates/marathon-android/tests/installer_test.rs::prepare_leaves_external_storage_alone_d27`, `crates/marathon-android/tests/scenarios_test.rs::the_d27_allowance_covers_only_the_root_wipe`, `tools/differential/test_evidence.py` (`test_d27_*`), Kotlin transcripts as the witness |
| D28 | Distinct instrumentation infos each receive the whole non-ignored batch; ignored callbacks and afterTestRun repeat per info, and the first completed result wins (`AndroidDeviceTestRunner.kt:62-98,171-205`, N35) | The non-ignored batch is partitioned by instrumentation info in first-seen order, and each instrumentation receives only its tests. Ignored callbacks, beforeTestRun and afterTestRun fire once per batch; a later instrumentation is announced by a new internal listener callback, which prepares its app data for file sync. Results aggregate over the batch. After a run failure, stuck output or a batch timeout the later instrumentations still run; a lost device, an error or a cancellation ends the batch, with the rest uncompleted. `testBatchTimeout`, `testOutputTimeout` and clearing data apply per instrumentation, as they do in Kotlin's loop. A batch of one instrumentation behaves as before. Few configurations reach this: `fixed-size` batching locks a batch to one bundle, so a mixed batch takes `class-name` batching and the same class name in both APKs. Approved 2026-11-06 (user decision); evidence: `crates/marathon-android/tests/android_runner.rs` (`*_d28`), `crates/marathon-android/tests/android_artifacts.rs::file_sync_with_two_instrumentations_prepares_and_pulls_once_d28`; JVM rows `runner:multiple-services` and `runner:multiple-events` stay as the witness |
| D29 | When the worker rejects an `exec` request, `exec` throws `ConnectionException("Request failed")` and never closes the session channel it opened (`SshjCommandExecutor.kt:47-48` with sshj `Session.exec`, A33), so on a shared connection every rejected command holds a `MaxSessions` slot until the connection ends | The rejected channel is closed at once, and the caller gets the same `Request failed` after the peer's reply or 5 s, whichever comes first; a peer that never answers costs that wait and its slot, and no error of the close replaces the rejection. A later command can open a channel where Kotlin's failed with `OpenFailException`. Approved 2026-11-06 (user decision); evidence: `crates/marathon-apple/tests/ssh_session.rs` (`exec_rejection_*_d29`), the Kotlin fake-worker transcript `round2-macos-faults/reject` as the witness of the old behaviour. WP x21 (U154, approved 2026-11-07): an `exec` with no reply in its 30 s is closed the same way, keeps Kotlin's error and logs one ERROR line naming the command; evidence `ssh_session.rs` and `ssh_real_sshd.rs` (`*_u154`). WP x21 (U157, approved 2026-11-07): the same for a channel open with no reply in sshj's 30 s, whose bound and error do not change: as nothing can be closed before the confirmation, a late one is closed when it comes, a late refusal's text is discarded, and one ERROR line is logged; evidence `*_u157` |
| D30 | `SshjCommandSession.close` sends TERM if open, then `command.close()`, which waits up to 30 s for the peer's CLOSE and throws, then `command.join()`, which has no limit; a later close after a failed one joins until the peer's CLOSE or the end of the connection; there is no KILL (`SshjCommandSession.kt:31-38`, A14) | Closing a session has a bound of 30 s and an escalation: TERM; up to 20 s for the process to end (the peer's exit report or CLOSE ends the wait); KILL if it did not; CLOSE; up to 10 s for the peer's reply; then the session is abandoned locally and the close fails with the same `Timeout expired: 30000 MILLISECONDS`, with one ERROR line naming the command. KILL precedes CLOSE because russh forgets a channel when it sends CLOSE, so CLOSE reaches the peer up to 20 s later than Kotlin's for a process TERM does not end. An abandoned session retires its iOS simulator (`apple_simulator_device/mod.rs:264`), which ends a pool whose last device it was. Measured on one Mac with Xcode 27.2 (iOS 18.6, 26.4, 27.0), xcodebuild ended within 5–16 ms of TERM, at most 3.06 s with parallel clones, in 46 runs, so 20 s is ample; a UI test's app under test stays running for at least 90 s after that (`docs/wp/x25-findings.md`). Signals are advisory: a server that ignores them may leave the process running, which holds its session slot until it ends or the connection does, and the shared connection is left alone. An abandoned session's scope is cancelled, so its awaiters are released at once. Later closes never wait longer than the bound, and the one after a failed close still cancels and joins the scope (approved P5). Exit 143 by `autodetect_exit_code`, `safe_execute` returning null on a failed close and the timeout branch's null are unchanged. No configuration field added. Approved 2026-11-06 (user decision); evidence: `crates/marathon-apple/tests/ssh_session.rs` (`*_d30`, `timeout_close_failure_fails_the_exit_code_and_safe_execute_returns_null`), `ssh_real_sshd.rs::t30_kill_ends_a_process_that_ignores_term`, Kotlin evidence `t15`/`t15h` as the witness of the old behaviour |
| D31 | rsync *send* has no timeout (`RsyncFileBridge.kt`, A3), so a stuck send blocks the bridge's other sends for ever; a receive has a 30 s total deadline | A send passes `--timeout=300` to rsync: 300 s with no I/O ends it, while any large transfer that keeps moving is unaffected. A receive has the same limit since D43. The readers' 5 s bound after the process ended applies to a receive too (U158, `*_u158`). An rsync 3 host also passes the flag on to the worker's `rsync --server`; openrsync does not. rsync 3 then exits; openrsync on macOS closes ssh's input and waits for ssh, for ever if ssh is hung, so the bridge reads rsync's report on stderr, gives it 5 s, sends TERM to it and every process the transfer started that `ps` has seen (at 1 s, every 10 s and at the stop, so an ssh orphaned by rsync 3 or `sshpass` is still known), gives them 5 s more and sends KILL to those still alive. Each signal goes only to a process listed just before with the same pid and start time and a parent that is one of them or init, never to the reaped child. Since x26's review a stall needs 285 s with no progress the bridge saw (since the start for a send, the last change of its files for a receive) and either rsync's report as a whole line (`rsync(<pid>): error: poll: timeout`, `[<role>] io timeout after <n> seconds -- exiting`) or exit 30 without `stopping at requested limit`; every stall, also one decided after rsync exited, ends what it started. An rsync that words its report differently, with a hung ssh, stays unbounded. It reaps the process before returning, so a retry cannot overlap it and a send queued behind it gets its turn. It logs `send error: no progress in 300 s sending to <destination>, rsync stopped: <rsync's text>`. Output pipes held open by a surviving ssh wait at most 5 s after the process ended. Since WP x26 (U163, approved 2026-11-07) the send returns `LeavePool` with `TransferStalled`, which neither the installer's three attempts nor the device's `with_retry(30, 10_000)` repeat, instead of `false`, so one send and about 305 to 320 s, up to 20 s more where `ps` is slow, end that simulator's prepare, where it was up to 90 sends and about 7.6 h. Sends already waiting behind it, to that worker or that destination, fail at once with the same error; one that starts later tries. An iOS simulator is retired. Any other failed send keeps both retries. A dead link with the default keep-alive disconnects the device in about 75 s. `--rsh` sets no `ConnectTimeout`, deliberately (`docs/wp/x26-findings.md`). Kotlin waits for ever. No configuration field added. The comparators give the JVM's and pinned native records' sends exactly this flag. Approved 2026-11-06 (user decision); evidence: `crates/marathon-apple/tests/rsync_file_bridge.rs` (`*_d31`), `crates/marathon-cli/tests/ios_stalled_send.rs` (`*_u163`, the stalls `#[ignore]`d since they take 295 s each on the real clock), `tools/differential/test_evidence.py` (`test_d31_*`), `fixtures/oracle/connections/deadlines.jsonl` and the Kotlin argv recordings as the witness of the old behaviour |
| D32 | `env` and `workdir` are ignored over SSH, so `NSUnbufferedIO=YES`, the one variable that matters (`Xcodebuild.kt`), never reaches xcodebuild on a worker (`SshjCommandExecutor.kt`, the `NSUnbufferedIO` part of A2) | Over SSH the exec string of a request whose env has `NSUnbufferedIO=YES` starts with `env NSUnbufferedIO=YES `, which only `xcrun xcodebuild test-without-building` does. `env` rather than a `VAR=x` prefix, because that is a syntax error in a login shell such as csh. Every other variable and the workdir stay ignored, so the rest of A2 is kept (D5). A local execution sets the variable in the process's environment, as before, once. xcodebuild's output now arrives in smaller writes as it is produced, so the output idle timeout measures xcodebuild's real silence; lines stay whole. The comparators give the JVM's and pinned native records' xcodebuild test command exactly this prefix, and the fake world routes it as the command it precedes. Approved 2026-11-06 (user decision); evidence: `crates/marathon-apple/tests/ssh_real_sshd.rs::t32_ns_unbuffered_io_reaches_the_process_and_nothing_else_does`, `ssh_session.rs::only_ns_unbuffered_io_reaches_the_worker_in_the_exec_string_d32`, `apple_bin_wire.rs::xcodebuild_runs_on_the_worker_with_the_unbuffered_prefix_d32`, `reader.rs::small_writes_of_unbuffered_output_hold_off_the_idle_limit_and_keep_lines_whole_d32`, `tools/differential/test_evidence.py` (`test_d32_*`), Kotlin T04 as the witness of the old behaviour |
| D33 | A simulator whose setup failed stays tracked as connected, so an unchanged snapshot never sets it up again, and a static devices file has no later snapshot at all (`AppleSimulatorProvider.kt:372,429-440`, `DeviceTracker.kt:76-90`, A81) | The provider retries it on a timer: up to 3 retries, 30, 60 and 90 s after each failure, the same count and base pause as a lost simulator, and no new setting. Failures while the worker answers count: factory, registration, `simctl create` and planning. An unreachable worker stays with its connection retry, and a refused connection, a simulator missing from the worker and an unavailable profile are not retried. All due targets of a worker share one attempt, and one planned setup cycle (first, late or retried) runs per worker at a time, other workers unaffected; a lost simulator's re-provisioning (`provisioner.existing`) is not gated, as it has no plan and sets up registered simulators, which no plan picks. A removal voids a waiting retry, and a registration after it is rejected. Device events are sent in the order raised, and a removal during a predecessor's disposal raises its disconnect after that replacement's connect. A profile's retries survive the removal of its worker, since tracking keeps profiles (A52), so they can still create a simulator there. A failed managed simulator is deleted, as in Kotlin, and its retry creates one replacement; one that could not be deleted is reused. Retries stop at the late-join cut-off and at termination, a retried simulator is admitted like a late one, and `attempts: 1` with `lateJoin: false` turns them off. After the last retry the simulator stays out of the run, as in Kotlin. The macOS provider has no tracker and is not affected. Approved 2026-11-06 (user decision); evidence: `crates/marathon-ios/tests/simulator_setup_retry.rs`; JVM row `A81-failed-setup-remains-tracked` stays as the witness |
| D34 | A history query that exhausts its three attempts is cached as an empty measurement for the whole run, and later lookups never recover (`MetricsProviderImpl.kt:54-68,105-122`, C41) | The defaults answer until a refresh gets an answer. One task on the vendor runtime re-queries failed keys, one attempt per key per round, after 5, 10, 20, 40, 80, 160, 300 and 300 s by the vendor clock, at most 8 times per key (a key that fails later starts at the round then due); an empty answer is a success and is not retried. Nothing on core blocks, and a lookup or prefetch never queries a failed key. New history takes effect at explicit points: a sorting comparator keeps the history of its queue's build, so a live `JavaPriorityQueue` never sees a key change; a batch reads one snapshot, taken as it is composed; a flakiness pass reads one snapshot. So with a failing backend, whether a queue or batch gets the history can depend on when the answer lands against when it is built, as with device arrival; within one queue or batch it cannot vary. `close` cancels and joins the refresh before closing the source, so a run never waits for it; the join has a bound of 3 s by the vendor clock, after which a source that blocks inside its future gets one warning and is closed anyway, though the process can end only when that blocking call returns. A healthy backend sees the same queries, order, TimingLog lines and output files. A failing one gets one more failed Timings line and WARN per re-query, with one INFO on recovery or one WARN at the end. D13's one query per key per run holds while queries succeed |
| D35 | sshj reports a failed key exchange (no common algorithm) as `TransportException[UNKNOWN]`, and `UNKNOWN` is in `TRANSIENT_DISCONNECTS`, so the worker is retried although the KDoc says a failed key exchange is not transient (`device/ConnectionFailures.kt:12-19,27-38`, A16) | A negotiation that finds no common algorithm is not transient. `UNKNOWN` stays transient for everything else, and only russh's `NoCommonAlgo` sets the marker, so a real network fault or text sent by a server cannot pass for it. Class, reason and message are unchanged, so logs and the report keep the JVM text. The connector still makes its four immediate attempts, as for an unverifiable host key; the device is then treated like the other non-transient failures: a worker's retries stop and `FailedSetup` is not `unreachable`. Approved 2026-11-06 (user decision); evidence: `crates/marathon-apple/tests/connection_factory.rs::failed_kex_is_not_transient_but_other_unknown_faults_are_d35`, `crates/marathon-apple/tests/ssh_connect.rs::no_common_kex_is_transport_unknown_after_four_attempts_and_not_transient_d35`, `crates/marathon-ios/tests/simulator_worker_retry.rs::a_failed_key_exchange_is_not_retried_but_a_dropped_connection_is_d35`, `crates/marathon-apple/tests/ssh_real_sshd.rs::d35_no_common_kex_on_a_real_sshd_is_not_transient`; JVM evidence `kex-failure.stdout` stays as the witness |
| D36 | A connection the server dropped right after authentication makes the next `execute()` throw `IllegalStateException("Not connected")`, which `safeExecute` does not catch and which is not transient (`sshj/PatchedSSHClient.kt:14`, `cmd/CommandExecutor.kt:76-85`, A17) | A transport that died gives a `ConnectionException` with the same text and the death as its cause, so `safeExecute` returns null, and callers that already treat a `ConnectionException` as a lost connection do so. This also holds once disposal has closed a connection the worker dropped. Only closing a live connection and the `Not authenticated` check stay `IllegalStateException`. `safeExecute` logs the new failure on `Unable to start a remote shell session`. Recovery does not go through `isTransientConnectionFailure`, which only sees connect failures. On iOS the provider's `!executor.connected()` checks make the worker's targets missing, and they are re-provisioned (a registered simulator whose worker stays down is retried every 16 ms with no limit until the run ends, a known limit that Kotlin shares: `source.rs:135`, `recovery.rs`); macOS has no reconnect, so the device leaves the run, as for any lost connection. Nothing reconnects because of a null. iOS and macOS setup and iOS descriptor reads refuse a lost connection, so no device registers with default values and none is reported as not booted; what other callers do with a null is listed in `docs/wp/x16-findings.md`. Approved 2026-11-06 (user decision); evidence: `crates/marathon-apple/tests/ssh_connect.rs` (`disconnect_after_auth_makes_safe_execute_return_null_d36`, `closing_a_connection_the_server_already_dropped_keeps_it_a_lost_connection_d36`, `a_server_that_always_drops_after_auth_costs_one_connection_per_acquire_d36`), `crates/marathon-ios/tests/simulator_setup.rs::setup_on_a_lost_connection_fails_instead_of_describing_the_simulator_with_defaults_d36`, `crates/marathon-ios/tests/simulator_prepare.rs::a_connection_lost_during_prepare_is_not_a_failed_boot_d36`, `crates/marathon-macos/tests/provider.rs::setup_on_a_connection_lost_before_the_last_probe_fails_instead_of_using_the_default_abi_d36`, `crates/marathon-apple/tests/ssh_real_sshd.rs::d36_a_killed_server_session_makes_safe_execute_return_null`; JVM evidence `after_auth-*.stdout` stays as the witness |
| D37 | stdout and stderr share one receive window, but a read refills it against the read stream's own buffer only. With one stream unread and the other read, the unread 16 MiB `CircularBuffer` overflows, the reader thread dies with `Attempted overflow` and takes the connection, so every command on that worker fails (`cmd/remote/ssh/sshj/SshjCommandExecutor.kt:51-52` with sshj `ChannelInputStream`, A32) | A read refills the window only as far as the fuller stream can still take, so a peer that honours the window cannot overflow: a flood with stderr at least every 512 KiB stops at 16,777,215 buffered bytes with the connection alive, where the old credit loses it at 16,760,832. The worker then blocks and the other stream stops too, which is what SSH flow control allows on one channel. Progress comes from a read of that stream, the command's timeout or an idle timeout, or the session's close: TERM and CLOSE need no window. On OpenSSH 10.5p1 at the cap, the command timeout gives a null exit code, the idle timeout a `TimeoutException`, close returns at once, and another command on the connection runs while the session stays blocked. A process that ignores TERM gets D30's KILL after 20 s with the window still closed, and the close succeeds. The window is per channel, and the pump never waits for a consumer, so the client's transport task does not stall. While no stream holds more than 14 MiB unread, every grant is what sshj's would be. Each stream stays below 16 MiB, and a peer that ignores the window still ends in `Attempted overflow`. Approved 2026-11-06 (user decision); evidence: `crates/marathon-apple/tests/ssh_session.rs` (`reading_only_stderr_blocks_at_the_cap_without_losing_the_connection_d37`, `a_stuck_consumer_at_the_cap_is_ended_by_the_command_timeout_d37`, `a_stuck_consumer_at_the_cap_is_ended_by_the_idle_timeout_d37`, `a_session_blocked_at_the_cap_can_be_closed_d37`, each asserting the cap through `wait_for_the_cap` where a session is used; the fake leaves the reply to CLOSE and other channels' traffic out of what it can show, U153), `crates/marathon-apple/src/cmd/remote/ssh/sshj/channel.rs` unit tests, `crates/marathon-apple/tests/ssh_real_sshd.rs` (`d37_*`); JVM oracle `fixtures/oracle/ssh/flood.jsonl` stays as the witness |
| D38 | From runtime 16 on, `isRunning` uses the `launchctl dumpstate` output without checking its exit status (`SimulatorService.kt:111-151`, A41), so a failed or timed-out probe whose output names a started SpringBoard service counts as running | A failed or timed-out probe is not running on every runtime. A healthy simulator exits 0: measured on throwaway iOS 26.4 and 27.0 simulators and an already booted iOS 18.6 one; runtimes 16, 17 and other 26.x not measured. A false negative is one more poll. A probe that fails for the whole of `bootWait` costs up to four setups of two boot waits each, about 26 to 30 minutes per simulator with the defaults, and then the simulator is out of the run. That holds while another simulator keeps the pool alive and leaves out the waits for shutdown and session close: with only the probe hung and needing KILL it takes about 33 minutes. On the last device the run ends `deviceInitializationTimeoutMillis` (180 s, D48) after the simulator left the pool, or after a test result it reports later, with `remaining tests will not run`, as re-setups that never get ready don't move that wait: about 9 to 11 minutes after the first boot, or about 3 minutes after a loss if the probe starts failing at a re-setup (`docs/wp/x42-findings.md`). No probe result erases a simulator; a configured `onPrepare: [ERASE]` erases on each re-setup, as on any setup. Each probe returns about 20 MB, so a host or link too slow to deliver that within `shell` now takes this path | Commands unchanged. The JVM rows `sim.running/{16,16.0,+16.1}/{fail,nul}` stay as the witness, with a named allowance for exactly those. See `docs/wp/x17-findings.md`. |
| D39 | Device failure literals match anywhere in stdout (`DeviceFailureParser.kt`, `XctestEventProducer.kt:18-20`, A48), so a test name, an assertion message or an application's log line holding one aborts the batch | Test and suite names, UI test activity, Swift Testing events and the log lines of an application no longer report a failure; a hosted application's log still reports XCTest's own three sentences. An assertion line, and an activity line's `Assertion Failure:`, report only when the message starts with a literal: XCTest puts a prefix before a test's own text, while a failure the framework records appears raw. That split is reasoned from Xcode's known formats, and no recording holds a raw one. Everything else matches as before, because no recording here establishes the literals' real forms. A missed failure waits for the output idle timeout (`testOutputTimeoutMillis`, 300 s by default) once per batch. Extended 2026-11-07 (x28, U165): any such timeout before a test has passed or failed counts as a batch failure, so three in a row recycle the simulator; one with progress resets the count as in Kotlin. Known misses are listed in `docs/deviations.md` | Commands unchanged. The JVM row stays as the witness, with a named allowance for that one line. See `docs/wp/x17-findings.md`. |
| D40 | `RemoveDevice` removes by serial alone, unlike readiness and termination (`DevicePoolActor.kt:283-290`, C27), so a delayed disconnect for an old device can remove its same-serial replacement | Removal compares the device instance (`DeviceRef`), since a replacement can have the same id string. The old device was already told to terminate before a replacement could enter the pool, and its batch comes back. Every sender passes the reference it announced. The last device still ends the pool in every order tested: new first, old first and duplicates. A device rejected as a duplicate no longer removes the one the pool holds | No vendor path found today sends the stale order, so transcripts are unchanged. The JVM row C27 stays as the witness. See `docs/wp/x17-findings.md`. |
| D41 | Any change from `device` to another state disconnects the device at once (`DeviceStateTracker.kt`, N40), so a transport back within a second ends a run on its last device | A device with an actor gets 10 s in `offline`, `connecting` or `authorizing`, 3 s of that if missing from the list, at most 5 times per serial per server per run, with no configuration field. Batches wait, and one ending in that time returns only when it is over, so no next batch is charged. A device loss during it fails the batch only. A device returning without `sys.boot_completed` goes through disconnect and connect. An outage takes over from a grace and adds none to its own bound. The commands of a healthy run are unchanged, and no recording shows an `offline`. Approved 2026-11-07 (user decision); `docs/wp/x19-findings.md` |
| D42 | A remote parsing session that ends with an `INSTRUMENTATION_CODE` other than -1, and a message other than `Process crashed`, keeps the tests it listed (`AmInstrumentTestParser.kt:177-187`, the rest of N38), so a run can go on with part of its tests | Such a session fails the attempt with a `TestParsingException` naming the session's text and how far it got, and takes the existing retries (three per bundle on the device, three around `borrow`), unless it listed every test its `numtests` announced: a whole list stands, as a retry would only repeat it. Code -1 with a whole last line, `Process crashed` (the producer fallback), `am` refusing to start and the batch runner are unchanged. One failing bundle with nothing announced now fails a parse the other bundles would have carried. All attempts share `deviceInitializationTimeoutMillis`, so a slow parse can end in `NoDevicesException` | No recording has such a session in a remote parse, so transcripts are unchanged and no comparator changed. adam's `log_6` is the witness of a whole list with code 0. See `docs/wp/x20-findings.md`. |
| D43 | A receive has a 30 s total deadline (`RsyncFileBridge.kt`, `output.timeOut = 30`), which cuts a pull that is still moving | A receive passes `--timeout=300`, as a send does since D31, and the bridge stops it the same way on rsync's report. Between two rsync 3 installs a pull that stops mid-file never reports, because the generator's keep-alives count as I/O, so the bridge also looks every 10 s at the pull's own files (the destination, rsync's `.<name>.XXXXXX` beside it and `.rsync-partial/<name>`, or a whole folder for a source ending in `/`) and stops a receive when none has changed for 300 s. It logs `receive error: no progress in 300 s receiving <source>, rsync stopped: <rsync's text>` and returns `false`, with `.rsync-partial/<name>` left. No listener retries. A stuck pull costs about 305 to 330 s where it cost 35 to 45 s, and nothing bounds the total over a run. An rsync 3 host passes the flag to the worker's `rsync --server --sender`. No configuration field added. The comparators give the JVM's and pinned native records' receives exactly this flag. Approved 2026-11-07 (project owner's decision); evidence: `crates/marathon-apple/tests/rsync_file_bridge.rs` (`*_d43`), `tools/differential/test_evidence.py` (`test_d31_d43_*`), `tools/fake-worker/tests/contracts.rs`, `docs/wp/x26-findings.md`; `fixtures/oracle/connections/deadlines.jsonl` stays the witness of the old behaviour |
| D44 | A registered simulator whose connection is lost is tried again every 16 ms until the run ends, and `existing` drops its failures (`AppleSimulatorProvider.kt:215-218,267-272,333-368`, A101). Any other failed re-provisioning is final. `borrow` picks at random from a map that keeps removed and disposed simulators (`:445-450`, A80) | A failed re-provisioning goes to the worker connection retry if the worker is unreachable and to D33's budget if it answered. A re-provisioning refused by the simulator's own connection is not retried. The old handle is no longer scanned, and a replacement raises one connect. A simulator that was in the run is exempt from the late-join cut-off and waits for its worker for `lateJoinCutoff`, counted from the first failure. `borrow` takes only a simulator that can work, and waits otherwise, like D24 |
| D45 | A hung test's allowance goes through `Duration.parse` after replacing plural units only (`TestRunProgressParser.kt:187-194`, A47), so `1 minute`, which XCTest prints for the 60 s that Marathon's docs recommend, throws; the batch comes back uncompleted and repeats | Every allowance reads: what Kotlin could read reads the same, then the English forms of XCTest's formatter, and any other text counts as 0 s with a warning. The test fails, and a hang with no failing line of its own has the hang's line as its trace, on the plural forms too. | Commands unchanged. The JVM `duration/*` and `real/xcode27-timeout` rows stay as the witness, behind a named allowance with a test that it covers exactly those throws. A real stream from an iOS 18.6 simulator under Xcode 27.2 is a new fixture. See `docs/wp/x28-findings.md` |
| D46 | The readiness probe is `launchctl dumpstate user/foreground/com.apple.SpringBoard` from runtime 16 on (`SimulatorService.kt:111-151`, A41), which returns 10 to 20 MB per poll, once per poll for up to `bootWait`, and over a slow link or a worker with a command filter it is the costliest command a simulator sends | From runtime 18 on the probe is `launchctl print user/foreground`: on iOS 18.6, 26.4 and 27.0 it exits 0 with 97 to 121 KB and the same `M A`/`M D` line before `com.apple.springboard.services`, so `is_running`'s parser is unchanged. Runtimes 16 and 17 were not measured and keep `dumpstate`, and a version that does not parse as a number keeps `print system`. An earlier measurement of 666 paired rounds agreed on 658, and the 8 that did not all reported ready up to one poll earlier; re-measured on a throwaway simulator per runtime for this change (`docs/wp/x29-findings.md`). `print system` and `bootstatus` are not equivalent. The command a worker sees changes, so a worker command filter that matches `launchctl dumpstate` must also allow `launchctl print user/foreground`. D38's 20 MB slow-link risk now holds on runtimes 16 and 17 only; its 26 to 30 minute bound is unchanged. Approved 2026-11-07 (project owner's decision); evidence: `crates/marathon-apple/tests/apple_bin_oracle.rs` (`d46_*`), `crates/marathon-ios/tests/simulator_prepare.rs` (`the_readiness_probe_of_a_runtime_18_simulator_is_launchctl_print_user_foreground_d46`), `crates/marathon-test-support/tests/world_executor.rs`, `tools/differential/test_evidence.py` (`test_d46_*`) and the native vendor differential; JVM oracle `fixtures/oracle/apple-bin/probes.jsonl` and `fixtures/oracle/simulator-device/events.jsonl` stay as the witnesses behind a named allowance for exactly this command on runtime 18 and later |
| D47 | `uncompletedTestRetryQuota` defaults to `Integer.MAX_VALUE` and no deadline ends a run, so a test that keeps coming back uncompleted repeats until CI kills the job (`Configuration.kt`, C85). This is the one deliberate exception to §3's rule that the Marathonfile format stays as it is: a Marathonfile that Kotlin accepted can now be refused | A run or `parse` is refused when `uncompletedTestRetryQuota` is unlimited: unset, or exactly 2147483647, on any vendor; the process exits 1 with a `ConfigurationException` naming the field, why, a suggested value and the migration section. Any other value passes, including 0, a negative value and 2147483646. The check is a function of the parsed configuration and vendor, run after the vendor gate; the binding and its default are unchanged and `config-dump` doesn't apply it, so JVM goldens replay unedited. No other setting counts, because none bounds the loop on every path: `retryStrategy` and the flakiness and execution strategies don't see an uncompleted result, the timeouts are per batch, Android and macOS have no count of batches without progress, and iOS's `maxBatchesWithoutProgress` isn't charged for an unclassified error or a lost connection. No configuration field added. Approved 2026-11-07 (project owner's decision), narrowed by the owner to "fail only when nothing bounds the loop"; evidence: `crates/marathon-cli/tests/run_outcome.rs` (the D47 section), `simulator_batch::an_unclassified_error_is_not_counted_as_a_batch_without_progress`, `simulator_recovery::a_worker_that_keeps_dropping_its_connection_is_not_charged_to_the_limit_on_losses`, `docs/wp/x30-findings.md` |
| D48 | A pool ends with its last device (`DevicePoolActor.kt`), and a device that returns is dropped by `safeSend` while still tracked (`Scheduler.kt:119-127`, C50), so a brief loss on a one-device pool ends the run | A pool whose queue still has work waits for a device, up to `deviceInitializationTimeoutMillis` from the latest departure of a live device (one there before, or one that came back and got ready, and only from its IsReady), or from a test that ended with no live device, through the cleanup of the devices leaving and through returns that never get ready. At the limit with no live device it stops those still getting ready, takes no new ones, and ends once every device has reported Terminated. The queue is untouched. The limit runs until a test ends, time with a returned device included, so flapping cannot stretch it; `Device::return_wait_used_millis` takes off what the vendor already waited (D22's outage, D41's grace), and on macOS, which has no return path, all of it. A queue with nothing left, or a limit of 0, ends the pool at once. After the limit C50 stands. The JVM rows `broadcast`, `all-lost` and `two-pools-disconnect` stay as witnesses behind an allowance for exactly the wait. Approved 2026-11-07 (project owner's decision); evidence: `crates/marathon-core/tests/pool_scheduler.rs` (`*_d48`), `crates/marathon-android/tests/reconnect_test.rs` (`*_d48`), `crates/marathon-ios/tests/simulator_pool_recovery.rs` (`*_d48`), real emulators in `docs/wp/x31-findings.md` |
| D49 | A request rejected or closed before its reply is `RequestRejectedException`, which `execution_error` turns into a lost device (`AdamAndroidDevice.kt:480-481`, N15), and the tracker sees no change, so a healthy device is out for the run | The device loss of a batch asks what the server lists, as D22 and D41 do. A device listed as `device` that also answers `getprop` with `sys.boot_completed` under `shell` is kept and only the batch fails, 3 times per serial per server per run with no reset. The fourth, or a device that does not answer or is not listed, retires it as before. `means_device_loss` and the D22 and D41 paths are unchanged. One more `getprop` per loss. Approved 2026-11-07 (project owner's decision); evidence: `crates/marathon-android/tests/offline_grace_test.rs` (`*_d49`), `crates/marathon-android/tests/reconnect_test.rs` (`*_d49`), real emulators through a proxy in `docs/wp/x31-findings.md` |
| D50 | A poll that FixedSize rejects, a flaky copy already in the batch or a test of another bundle, still uses a slot and adds its duration (`FixedSizeBatchingStrategy.kt`, C33), so batches end early and each extra one costs another instrumentation or xcodebuild start | Only accepted tests count against `size` and `durationMillis`. Rejected polls have their own limit, 2 × `size` per batch, so a long run of another bundle isn't scanned; at that limit the batch goes out short. Rejected tests are re-added after the batch as before. A batch with nothing rejected performs the same operations as Kotlin. Under a strict order every test keeps its place; under ties, rejected tests go to the array's end and the heap's tie order differs from Kotlin's after such a batch. No configuration field. The JVM rows stay as witnesses behind a narrow allowance (`strategy_batching::D50_ROWS`, `queue_oracle::D50_ROW`). See `docs/wp/x32-findings.md`. Approved 2026-11-07 (project owner's decision) |
| D51 | A tracker's exception ends the queue actor, and in the port nothing then ended the run (`GraphiteTracker.kt`, `BasicGraphiteClient.kt`, C62); carbon has no timeout; one `None` discards a whole history query (`GraphiteDataSource.kt`, C38); a failing reporter or close skips the later ones (`ExecutionReportGenerator.kt`, `Marathon.kt:243-249`, C11, C25); a filter that removes every test gives a silent green run | A remote backend's failure is logged once per kind and the run carries on. Carbon gets 10 s to connect and 10 s to write, any send at most 30 s, the backend's close at the end of the run at most 30 s (after which it is logged and dropped), and a slow failure skips metrics for 5 minutes. `None` lines are skipped with a debug line, and other malformed lines fail as before. Every close runs, each failure is logged, and the first is returned, so the exit code stays. An empty filtered run logs an ERROR and stays exit 0. Metrics from a working backend are unchanged, except points still queued when a close runs out of time. A queue that ends for another reason still hangs the run (U168). No configuration, CLI or exit code changes |
| D52 | The shutdown hook is installed only with `debug: true` (`ShutdownHook.kt:16`, C10), so a cancel with `debug: false` skips provider termination and reports, and worker processes can outlive the job. With `debug: true` the hook closes the provider while the actors keep running, and they bounce tests off the closed client (U161), and its `onFinish` closes the provider before the reports | Any SIGINT or SIGTERM during a run stops the actors, writes the reports, then terminates the provider and closes the history side by side, and exits with 128 + the signal number, as before. It takes at most 25 s: 24 s for all that and 1 s for the last output, however long a step takes. The reports get 10 s before the rest starts alongside them. A second signal exits at once, however soon after the first; two of the same signal that arrive together count as one. A signal during the run's own finish does what is left in this order. Nothing is written over the last run's reports before the output directory is wiped. Once the run is over, a blocking call gets 3 s and no longer holds the process (D34). The JVM's `signal-*` rows stay as the witness behind a narrow allowance |
| D53 | Remote parsing makes three attempts, each borrowing a device at random, and nothing remembers the device an attempt failed on (`Marathon.kt:131-143`, A103), so all three can land on one that cannot parse while another is healthy. The macOS provider hands out a Mac whose connection is lost (U171) | Within one parse, an attempt picks among the available devices that no earlier attempt failed on, with the same random choice, and among all once every one has been tried; it never waits for an untried device. A device is known by its provider's borrow key (Android: adb server and serial; iOS: the simulator's key; macOS: serial), so it stays known across a reconnect. macOS parsing takes only a connected Mac and otherwise waits within `deviceInitializationTimeoutMillis`, as iOS does under D44. Three attempts, the time limit, error precedence, logs, commands and a healthy run's replay choices are unchanged. With the default 180 s limit and keep-alive, a frozen worker's attempt ends when keep-alive loses it, at about 75 to 80 s (D54); with keep-alive off the limit ends the parse |
| D54 | A transfer blocks in `monitor` inside a suspend function (`RsyncFileBridge.kt:40-66`), so neither its caller's cancellation nor the loss of its device's connection ends it, and a send has no limit (A3): `deviceInitializationTimeoutMillis` cannot end a remote parse sending to a frozen worker, which can hang for ever (A104) | An iOS or macOS send takes its call's cancellation, and every transfer, send or receive, the loss of its device's own SSH connection; a receive never takes a cancellation, since every receive collects an artifact that its callers are cancelled around on purpose. A stop ends rsync with D31's signals and limits, without the first 5 s: TERM, then KILL 5 s later, to rsync and what it started, whatever rsync's status. A cancellation returns as one and logs nothing. A loss fails a send with `ConnectionException: Lost the connection to <worker> during a transfer` and leaves a receive `false`, with one ERROR line. A send waiting for the worker's or the destination's earlier send gives up its wait on its own stop; the earlier send and those behind go on. With the default keep-alive a frozen worker's parse attempt fails at about 75 to 80 s and D53 moves on; with keep-alive off the 180 s limit ends the parse at its limit. rsync and ssh argv, worker commands, receives and healthy runs are unchanged |
| D55 | Java strings retain UTF-16 units, including lone surrogates, until output encoding, and compare by UTF-16 unit order | WP 8.2 stores text as Rust `String` / `&str`. DEX MUTF-8, escaped YAML/JSON (including xcresult), plist and process text inputs replace lone surrogates with U+FFFD at decoding; valid pairs at UTF-16 decoding boundaries remain their supplementary character. The DEX parser keeps Kotlin's pre-existing UTF-8 decoding of MUTF-8 bytes. Output therefore carries U+FFFD rather than the JVM UTF-8 encoder's `?` for malformed input. Rust scalar-value sort order is accepted for supplementary characters versus U+E000–U+FFFF. File/path length limits and truncation still count UTF-16 units, Kotlin whitespace/lines remain, and fragmentation/Allure hash inputs are unchanged for valid Unicode. Length-framed annotation slices and DEX descriptor slices that split a surrogate pair produce U+FFFD. Diagnostic and log abbreviations cut at whole characters without inserting a replacement character. Output filename/path limits retain UTF-16 counting; D56 defines truncation through a surrogate pair |
| D56 | At a `maxPath` or `maxFilename` cut inside a supplementary character, rc.1 retains a lone high surrogate. File plans create their untrimmed directory and return the invalid path; conversion at the caller's file open fails with `InvalidInput`, so no file is written (`FileLogWriter` propagates it). Folder plans emit their diagnostic and fail with `InvalidInput` in `PathPlan::materialize` while creating the trimmed directory; the result-bundle listener reports it as undelivered and returns false | WP 8.3 stores paths as `PathBuf`, counts both limits in UTF-16 units, and cuts before the whole supplementary character, one unit shorter than the limit. The shorter valid file or folder can be written. If dropping the pair leaves an empty component, materialization retains rc.1's `InvalidInput` failure instead of returning the parent directory as the file or folder path. Normalization, diagnostic wording and directory/log ordering remain. Confirmed by compiling the unchanged `file_manager.rs` at `f18ebdd8` in an isolated probe, exercising file and folder materialization and file-path conversion, and tracing `FileLogWriter` and `ResultBundleRunListener`; the new write regression is `jcompat::hash_paths::split_pair_limits_write_complete_paths_d56` |
| D57 | JDK UTF-8 replacement decoding combines a complete or truncated encoded surrogate into one U+FFFD; socket error text uses Java's leading slash and expanded IPv6 spelling; OkHttp Basic credentials encode ISO-8859-1 and replace characters outside Latin-1 with `?` | WP 8.3: (1) `String::from_utf8_lossy` uses Unicode maximal-subpart replacement granularity, so `ED A0 80` becomes three U+FFFD and `ED A0` two, including DEX's historical UTF-8 decoding of MUTF-8 bytes. (2) Connection diagnostics use `SocketAddr` Display, for example `127.0.0.1:5037` and `[::1]:5037`, with a configured DNS name in front where needed. Android host text used in device IDs and result data retains its old spelling. No result golden contains the changed socket diagnostics. (3) Basic credentials base64-encode the UTF-8 bytes of `user:password`; ASCII credentials are unchanged. Separate expectations in `fixtures/phase8/` retain the original oracle witnesses; URL form and query bytes are unchanged |



### 3.12 Phase 8 contract

`0.12.0-rc.1` is the JVM-parity baseline described by §3.1–3.11. From Phase 8, §3.12 supersedes those sections wherever they conflict. Phase 8 preserves:

1. All Marathonfile and Marathondevices inputs accepted today, with the same meaning, including YAML 1.1 scalars, environment substitutions (`${VAR}` and `${VAR:-default}`), ISO-8601 durations and instants, and binder coercions. Rejected inputs remain rejected; error wording may change.
2. CLI subcommands, flags, defaults and exit codes. Help, usage and error wording may change.
3. Result paths and names and their data. Compare JSON, XML and YAML as parsed data, preserving key and element order and attribute sets; equivalent numeric and escaped forms compare equal. Text, images and binaries compare as today. Fragmentation hashes and Allure history IDs remain exact.
4. Commands sent to workers (adb, ssh, rsync, xcrun and xcodebuild).
5. Per-test progress lines and the end-of-run summary block, including `Marathon run finished` and the `\tRaw: ...` lines, retain their text format; other logs may change.

Changes require a D-row in §3.11 and, for result files, a reviewed golden update. JVM artefacts such as identity strings, array identity values and Java exception class names may be replaced with real values. Exotic Kotlin bugs, including lone-surrogate and supplementary-plane sorting quirks, may be removed when preserving them requires emulation.

## 4. Target architecture

### 4.1 Cargo workspace

```
crates/
  jcompat/              Java/Kotlin semantics we must reproduce (§4.3)
  marathon-config/      YAML Node layer, Jackson-compatible binding, profiles P1–P3, validation stages
  marathon-core/        model, vendor traits, job/actor runtime, strategies, filters, analytics,
                        event pipeline, reporters, Marathon run flow
  marathon-apple/       command layer (local + SSH), rsync, plist/xctestrun, xcresult, log parser,
                        runner/session, nm/XCTest parsers, ArtifactProcessor, installer
  marathon-ios/         simulator provider, device, provisioning, recovery, late join, listeners
  marathon-macos/       macOS host provider and device
  marathon-adb/         async ADB client + scripted ADB server (used for tests and the oracle)
  marathon-dex/         DEX reader, JUnit3/4 discovery (LinkedIn rules), AXML, ApkParser
  marathon-android/     Android provider, device, installer, instrumentation runner, listeners
  marathon-test-support/ StubVendor/StubDevice, FakeSimulatorWorker world, fake executors, golden helpers
  marathon-cli/         `marathon` binary
tools/
  fake-worker/          SSH server acting as a scripted Apple worker (§6.4); used by both Kotlin and Rust
  oracle/               Kotlin oracle build, oracle-only patches, config-dump, jcompat table generators
fixtures/               test resources copied from the Kotlin repo (with provenance), corpora
docs/                   source inventory, parity matrix, design notes (lifecycle), deviations, WP briefs
```

Koin dependency injection becomes explicit construction in `marathon-cli`: a `VendorKind` match builds the vendor, which supplies its provider, parser, bundle identifier and log configurator.

### 4.2 Library choices

Each choice is confirmed or replaced by the Phase 0 spikes.

| Need | Choice | Fallback / notes |
|---|---|---|
| Async runtime | `tokio`: multi-thread for I/O plus a dedicated single-thread core executor (§5) | – |
| CLI | Hand-written parser (WP 2.5) | Matrix-tested against Clikt (§3.1) |
| YAML | A function-by-function Rust port of SnakeYAML 2.2's reader, scanner and parser (Apache-2.0, chosen by spike 0.3b over `saphyr-parser`, whose scanner diverged), feeding our own `Node` tree. The tree keeps raw spelling as Rust strings (with UTF-16 counting retained where required by D55), scalar style, tags, anchors, duplicate entries and positions | Our own binding layer reproduces Jackson. Alias expansion has a size cap. Input is decoded from bytes as Kotlin `readText()` does. See `docs/research/yaml-spike-results.md` and the binding cases carried to WP 2.1 in `docs/wp/0.3b-findings.md` |
| Regex | The Rust `regex` crate (D6), wrapped in `jcompat` to give Kotlin's `matches`/`find`/`split`/`replace` semantics and errors that name the config field | Spike 0.3c (fancy-regex behind a Java translator) is kept only as research |
| SSH | `russh` 0.64.x (exec channel per command, `signal`, keep-alive, password and public-key auth, our own known_hosts verifier and accept-any verifier), pinned | A vendored, patched russh. `ssh2`/libssh2 is ruled out: it has no channel-signal API, its keep-alive never waits for replies, and it only blocks (`docs/research/ssh.md`). Open question for spike 0.3a: sshj loads PuTTY `.ppk` keys and russh does not, which may need a deviation |
| rsync | System `rsync`, plus `sshpass` for password auth, with rsync4j's exact argv | – |
| Plist | `plist` 1.x for reading (its `Dictionary` is backed by `indexmap` and keeps insertion order); our own XML writer for byte-exact xctestrun | Inputs are XML or binary. The fixtures are checked for OpenStep plists |
| xcresult | Types generated from `xcresulttool-schema.json` (checked-in codegen), with legacy `_value`/`_values` unwrapping | – |
| HTTP | `reqwest` + rustls | – |
| JSON | `serde_json` with a Gson-compatible formatter; Allure has its own serializer matching allure-java-commons' Jackson output | – |
| XML | `quick-xml` | – |
| Images | `image` + `gif` | Animated GIF metadata as in `GifSequenceWriter` |
| Zip | `zip` | – |
| Logging | `tracing` with a logback-compatible formatter, logger-name-based level filter, and a two-phase setup | – |

### 4.3 `jcompat`

The pieces of Java and Kotlin behaviour we reproduce. All tables are generated by running the reference JDK (§6.3).

- **`JavaPriorityQueue`.** The heap of the reference JDK, including bulk removal, iteration order and comparator call order.
- **Number formatting.** `Double.toString` changed in JDK 19, so the JDK must be the one the fork's CI runs. Also `String.format` for the patterns that are used.
- **Time.** `Instant` and `Duration` parsing and formatting, `SimpleDateFormat` (UTC), and `DurationFormatUtils`.
- **Substitution.** The commons-text `StringSubstitutor`.
- **Regex.** `JavaRegex`.
- **Hashing and names.** The signed-`BigInteger` md5 fragment hash, `escape()` and FileManager truncation.
- **Serialization.** The Gson writer, and the Jackson YAML emitter used for P3.
- **Logging.** logback's pattern layout, `%logger{N}` abbreviation and `%highlight`.
- **Kotlin `toString`.** The data-class format, used by `Configuration.toMap()` and by the `.expected` fixtures.

## 5. Runtime and lifecycle design

**Documented first.** WP 0.6 starts by writing `docs/design/lifecycle.md` from the Kotlin code, and the Rust runtime is designed against that document.
- **`Scheduler.job` is a detached `Job()`** (`Scheduler.kt:45`):
  - the device subscription is launched in the *caller's* context, not under `job`;
  - pools and their `poolJob`s live under the detached job;
  - `execute()` joins only the children that exist while it iterates;
  - provider termination happens later, in `onFinish`.
- **Shutdown order:** `analytics.close` → `deviceProvider.terminate` → `tracker.close` (reports) → `usageTracker.close`.
- **The parse path skips `onFinish` entirely.**
- **Actor close drops the mailbox:** `Actor.close()` cancels the actor at once, and messages not yet processed are lost.
- **Termination fires once:** `invokeOnCompletion` forces Terminate, and `Terminated` is sent exactly once.

**Building blocks:**
- **`Job`.**
  - A parent/child tree with SupervisorJob isolation.
  - Cancel-requested and completed are separate states, so cancelling is not the same as having finished.
  - `cancel_and_join`, `invoke_on_completion`, and joining the children that exist at that moment.
  - A detached job is a first-class case.
  - A bare `CancellationToken` is not enough.
- **`Deferred<T>`.** First writer wins, can be sealed by a cancellation reason, awaitable. It carries the DeviceActor ↔ vendor result protocol.
- **`Actor<M>`.** Unbounded mailbox. `close()` cancels and drops unprocessed messages; `safe_send` ignores a closed actor.
- **Identity.** `Arc::ptr_eq` or generation ids wherever Kotlin uses `===`.
- **Errors.** Exceptions used for control flow become explicit enums:
  - `VendorError` drives the DeviceActor table;
  - `RunOutcome` covers no devices, no tests and similar ends;
  - the Apple `NotBooted`/`NotAdmitted` become an explicit "leave pool" outcome.

**Execution model.** The core actors are serialised on one dedicated thread, a tokio `current_thread` runtime with a `LocalSet`, as Kotlin's `runBlocking` does. Vendor work runs on the multi-thread runtime. Blocking calls never run on the core thread.

**Determinism.**
- Live runs are not deterministic, in Kotlin or in Rust: vendor completions and device events arrive concurrently, and random sorting and random device borrowing add more variation.
- Exact batch assignments are therefore compared only under **controlled replay**. Both implementations run with an injected clock, UUID sequence and random seed, and with scripted vendor completion barriers.
- Live runs are compared through invariants and outcome distributions instead.

## 6. Verification strategy

### 6.1 Inventories

**`docs/source-inventory.csv`** lists every main Kotlin file and its owning WP. Files with no tests count too.

**`docs/parity-matrix.csv`** lists every test across all source sets: `test`, `integrationTest`, and disabled or commented-out tests (recorded separately). It covers:
- each `@Test`;
- each `@ParameterizedTest` invocation, expanded from its argument sources;
- the tests of the replaced libraries: adam's instrumentation transformer and stub-server tests, and dex-test-parser's tests.

Each row records its status (`ported` / `n/a` / `pending`) and the commit that ported it. An `n/a` row needs the reason and the agreement of a reviewer.

### 6.2 Golden fixtures

All Kotlin test resources are copied into `fixtures/` unchanged.
- **Apple:** xcodebuild logs with their `.expected` files, xcresult JSON, xctestrun files, simctl JSON.
- **Config and devices:** config samples, Marathondevices files.
- **Outputs:** JUnit XML and the XSD, raw JSON.
- **Android:** APKs and the framebuffer dump.

Where an `.expected` file holds Kotlin `toString()` output, the Rust tests render the same format.

### 6.3 Kotlin oracle

- **Reference build.** JDK pinned to the fork's CI JDK (determined in WP 0.2). `e47352f2` built with `installDist`.
- **Hook inventory** (`docs/design/nondeterminism.md`, WP 0.2). It lists every source of nondeterminism in the Kotlin code, each with its call site:
  - the injected `Clock`, `Instant.now()`, `System.currentTimeMillis()`, `Date()` and `SimpleDateFormat` use, `System.nanoTime()`/monotonic time, and coroutine `delay`/`withTimeout`;
  - Kotlin `Random` and `.random()`, for example `RandomOrderSortingStrategy` and `devices.values.random()` in the providers;
  - `UUID.randomUUID()`, in `TestBatch`, Allure and simulator names;
  - every point where concurrency decides an order: asynchronously launched readiness in `DeviceActor`, vendor completions, provider events.
- **Oracle-only patches** (`tools/oracle/patches/`, never shipped). Each hook site goes through a small `OracleHooks` service. The Rust code has the same named seams in debug builds.
  - **Clocks** are three separate virtual clocks: wall, monotonic and scheduler. They advance only as an explicit tape says.
  - **Random draws** come from a recorded *draw tape* keyed by call-site label, not from a shared seed. That way the generator algorithm and the draw order don't need to match.
  - **IDs** come from a per-site allocation tape.
- **Patch validation.**
  - Without a tape, the patched Kotlin build must pass the full Kotlin test suite.
  - With a tape, it must reproduce its own outputs over N repeated runs.
  - Its outputs must match the unpatched build except for the injected values.
  - Until that is shown, replay goldens are not authoritative.
- **`config-dump`** (JVM) and a hidden `marathon config-dump` (Rust) print:
  - canonical JSON of the parsed configuration, including derived values hidden by `@JsonIgnore`, such as the resolved bundle paths;
  - which validation stage rejected the config, and the message;
  - the filesystem changes.
- **Isolated runs.** Every comparison runs each implementation in its own copy of the filesystem, environment and backend snapshot, so one run can't change the other's inputs (bundle extraction, outputDir wipes).
- **Corpora:**
  - every fixture and production Marathonfiles;
  - a mutation corpus generated by Muse: for each key, drop it, mistype it, add an unknown sibling, quote it, use YAML 1.1 literals, add substitutions, duplicate it, tag it, alias it, and set `null`;
  - the substitution truth table;
  - a Java-regex corpus.
- **jcompat tables** come from the same JDK.
- **Analytics.** Request transcripts from both implementations against real Influx v1, Influx v2 and Graphite in containers (Docker or colima). This ports `core/src/integrationTest`.

### 6.4 Differential end-to-end on fakes (no hardware)

**Fake Apple worker** (`tools/fake-worker`). A russh-based SSH server. Kotlin (sshj) and Rust (russh) both connect to it as an SSH worker in Marathondevices.

**What it intercepts.** Every *executor* command goes through an exec channel, so the fake sees them all, including absolute paths such as `/usr/libexec/PlistBuddy` and `/usr/sbin/system_profiler`.
- **Scripted world.** Commands are handled by a scripted world built from the Kotlin `FakeSimulatorWorker`, replaying the xcodebuild and xcresulttool fixtures. It covers:
  - simctl (`io recordVideo` and `screenshot` create media files), xcodebuild, xcresulttool, nm, lipo, PlistBuddy, sqlite3, system_profiler, ioreg, plutil;
  - `ffmpeg` (`-version` and transcoding), chmod, codesign, csrutil, getconf, sw_vers, xcode-select;
  - `env`, `uname` and the file commands;
  - a macOS host world.
- **One virtual filesystem** is rooted at `/tmp/marathon` for each run. The same path mapping applies to exec argv, `WriteFile` payloads, `rsync --server` path arguments, and the paths inside the xctestrun that the fake xcodebuild reads.
- **OpenSSH and rsync.** The server must work with a stock OpenSSH client (rsync's `--rsh`), with both key and password auth (through sshpass), and with rsync streams in both directions. It runs `rsync --server` for real against the virtual root.
- **Session limits.** A configurable limit on concurrent sessions, like sshd's `MaxSessions`.
- **Push paths.** Both are exercised: `WriteFile` over exec, and the rsync fallback, which `ssh.debug` forces.

**What runs on the host, even with SSH transport.** The fake server can't see these, so each is covered separately:

| Host-side work | How it is covered |
|---|---|
| The rsync client, and the ssh/sshpass it spawns | Logging shims record each argv (including the `--rsh` string) and then exec the pinned real binaries. The rsync path comes from rsync4j's `RSYNC4J_RSYNC` override, which the Rust code honours too. The debug-mode `rsync --version` call is normalised |
| Bundle extraction and validation at parse time, `Info.plist` reads, Mach-O magic scans | Real host fixtures: `sample/ios-app` and a macOS sample, built once with this Mac's Xcode, stored under `fixtures/bundles/`, and copied into each isolated run (§6.3) |
| The local temp files staged before a push (xctestrun, `libxctest-parser.dylib`), and the local rename after an xcresult pull | Compared through the pushed content (in the virtual root) and the output manifest, not through transcripts |

**Scenarios:** pass, fail, hang/timeout, runner crash, CoreSimulator failure, boot failure, lost connection, MaxSessions exhaustion, late worker, and dynamic updates.

**Validation first:** Kotlin marathon is run against the fake before any Rust vendor code exists.

**Local transport** gets its own comparison suite, because its file bridge copies files directly instead of using rsync.
- **Hermetic contract tests**, runnable in CI: the local executor (quote stripping, env, workdir, timeouts, idle timeout) and the local file bridge, on fake binaries in a controlled `PATH`.
- **Whole-run comparison:** runs on this Mac with the real Xcode tools, as part of 7.2. Several tools are called by absolute path, so a fake toolchain can't stand in for them on CI.

**Scripted ADB server** (`marathon-adb`, server side). Kotlin, through adam, and Rust both use it through `adbServers`. It plays scripted devices: features, getprop, install, instrumentation output streams, sync and framebuffer.

**Comparison:**
- the full output-directory manifest, and every artifact family in §3.4;
- command transcripts, stdout, stderr and the exit status;
- the cleanup state left on the worker.

Normalisation is defined per field (UUIDs, timestamps, durations, temporary paths) and keeps referential integrity: the same UUID maps to the same placeholder everywhere. Text formats are compared byte for byte after normalisation; media are compared by decoded content.

### 6.5 Controlled scheduling replay

Core scenario tests use StubDevice, the hook tapes of §6.3 (virtual clocks, draw tapes, ID tapes) and an **event script**.

**The event script.** It lists the vendor and provider events in order: device connected or disconnected, prepare done, batch results, failures. The replay harness delivers one event, then waits for **actor quiescence**:
- every core actor mailbox is empty;
- no core task is runnable;
- every asynchronously launched readiness message has been delivered.

Only then does it deliver the next event.

**Equivalent control on the Kotlin side** (patched oracle):
- a test dispatcher drives the `runBlocking` loop;
- the same quiescence check is built from the actors' channel states and the dispatcher's task queue.

**Comparisons:**
- Exact batch assignments and `raw.json` are compared against the Kotlin scenario fixtures and the oracle.
- This happens only once the oracle's replay is shown to be stable (§6.3 patch validation).
- Property tests check invariants under random interleavings: no duplicate assignment, quotas never exceeded, and every result reported.

### 6.6 Real end-to-end (release blockers for all three vendors)

WP 7.2 uses this Mac's Xcode 27 profile, an approved loopback SSH forward to
one Xcode 26.6 Mac, and a reserved Linux KVM worker (no Docker). Host coordinates
and credentials are runtime-only private configuration. The approved matrix is
16 scenario/profile pairs; ordinary pairs repeat twice, cancellation and
permission pairs once, for 28 pair executions. See `docs/design/real-e2e.md`.

| Vendor | What runs |
|---|---|
| iOS | `sample/ios-app`: unit/nm, UI/video and UI/screenshots on both Macs, with a newly created owned simulator for every invocation |
| macOS | The maintained macOS sample: unit/nm, UI/xctest, cancellation and matched A18 permission refusal on both Macs |
| Android | `sample/android-app` on the Linux KVM worker only: local/video/sync and remote/screenshots, one fresh AVD per invocation, a private non-default ADB server, no other visible devices |

Kotlin runs before Rust on the same worker/toolchain/image with identical build
products. Each invocation reserves the entire Apple `/tmp/marathon`; runner
leftovers are recorded before harness cleanup. The user's simulators, devices
and ADB server are never used. Missing tools, capture seams or approvals block
coverage; an offline preparation gate is not real-host certification.

**WP 7.2 macOS exception (user decision, 2026-10-28):** run directly on the
hosts, without VMs, without changing SIP or TCC by any route. Record actual
`csrutil status` and both TCC access-table row counts before and after every
invocation; unavailable reads or changed state block the profile. A real matched
A18 refusal plus the existing exact TCC command contracts satisfies the permission
case. This exception is scoped to the maintained WP 7.2 sample and does not
waive the independent shadow/farm isolation requirements in §6.7.

### 6.7 Shadow runs on the device farm, then cutover

**Isolation is required.** Both implementations use `/tmp/marathon`, including `/tmp/marathon/shared/` with fixed bundle names, the fixed name `marathon.xctestrun`, and rsync `--delete`. Their locks and transfer caches are local to each process. Disjoint simulators on the same worker are therefore **not** isolated. Either:
- concurrent runs use **disjoint workers** (a split farm), or
- the runs are sequential on the same workers, and between runs:
  - the whole `/tmp/marathon` is restored, including `shared/`;
  - simulator state is reset: erase or recreate, then boot;
  - every leftover xcodebuild, recordVideo or log-stream process is checked to have exited.

**macOS hosts.** A run also writes the system and user TCC databases (`MacosDevice.kt:514`), and its tests can change host state of their own.
- Sequential reuse of a macOS host **always** requires restoring a full host snapshot between runs: a VM snapshot (e.g. a tart or UTM macOS VM) or an equivalent restore of both TCC databases, the test-created state and `/tmp/marathon`.
- Leaving `permissions` out of the config doesn't remove that requirement.
- If no snapshot is possible, the macOS comparison runs use disjoint hosts.
- Permission handling is also compared through the fake-worker transcripts (§6.4).

The WP 7.2 sample runs in §6.6 have the explicit direct-host exception above;
this section continues to govern shadow/farm runs.

On Android they use disjoint devices, and the remote tracing and recording files are cleaned between runs.

**Comparison** over at least two weeks:
- outcomes, failure classification, retries, recoveries, timings and artifacts;
- then a canary that publishes Rust as `latest`, with the JVM build kept for rollback.

## 7. Work packages

Each WP is one branch and worktree with a brief in `docs/wp/<id>.md`. The brief lists:
- the Kotlin files (from the source inventory) and tests (from the parity matrix) it ports;
- the fixtures it uses;
- the Rust API it exposes;
- the quirks to preserve;
- its acceptance gate.

Implementer effort is **L**, **M** or **H**; H is used only for concurrency-heavy or foundational work.

### Phase 0: Foundation, spikes and harness

**Phase 0 exit barrier.** No WP in Phases 2–6 starts until all of 0.1–0.6 are merged, which includes 0.4b and 0.5 validated against Kotlin marathon and the oracle patch validation (§6.3).
- The source inventory has no unowned files.
- Phase 1 (`jcompat`) may start once 0.2 is done.

| WP | Content | Opus | Depends on |
|---|---|---|---|
| 0.1 | `git init`, workspace, toolchain pin, lints, `scripts/check.sh`, `AGENTS.md` / `CLAUDE.md`, fixtures copy, **source inventory** and **parity matrix** generators (all source sets, parameterized tests, disabled tests, replaced-library tests) | L | – |
| 0.2a | Kotlin reference build in a disposable clone of the source at `e47352f2` (the source checkout itself is never built in or modified), using the reference JDK; `config-dump` with staged outcomes; jcompat table generators; corpus generators for the YAML and regex spikes; isolated-run runner | M | 0.1 |
| 0.2b | **Hook inventory** (`docs/design/nondeterminism.md`), oracle patches (`OracleHooks`: three virtual clocks, draw tapes, ID tapes, quiescence probe), **patch-validation suite** (§6.3) | H | 0.2a |
| 0.3a | **SSH spike**, russh against sshj on a real sshd: exec, TERM → 143, timeout → null, keep-alive (whole-second truncation, max count, transport loss), exhausted MaxSessions, output around EOF and close, both streams at once, a TERM that is ignored, host-key and key formats, auth failures, connect retries with fresh clients. Also sets up the containerised sshd that the `SshjCommandExecutorTest` equivalents need. Decides russh or the fallback | M | 0.1 |
| 0.3b | **YAML spike**: saphyr-parser against SnakeYAML/Jackson on an acceptance and rejection corpus (tags, duplicates, multiple documents, anchors and aliases, `<<`, 1.1 scalars, limits). Fixes the `Node` design | M | 0.2a |
| 0.3c | **Regex spike**: JVM-generated corpus of patterns × inputs (Unicode, line terminators, flags, quoting, possessive and atomic constructs, backtracking-heavy lines). Compares the translator over fancy-regex with PCRE2. Output: supported subset and D6 list | M | 0.2a |
| 0.4a | `tools/fake-worker` SSH server: exec routing, transcripts, a virtual `/tmp/marathon` root with one path mapping for argv, `WriteFile` payloads, rsync paths and xctestrun content, real `rsync --server`, OpenSSH client support with key and password auth, a session limit; host-side argv shims for rsync, ssh and sshpass | H | 0.3a |
| 0.4b | Scripted Apple world (iOS simulators and macOS host) from `FakeSimulatorWorker` and the fixtures, covering every command in §6.4 (including ffmpeg and the minor tools) and creating media files; real host bundle fixtures built from the samples. **Validated by running Kotlin marathon against it** | M | 0.4a, 0.2a |
| 0.5 | Scripted ADB server (replaces adam `server-stub`, and drives Kotlin marathon too) and its device-script DSL | M | 0.1, 0.2a |
| 0.6 | **Interface freeze**: `docs/design/lifecycle.md`; model types (Test equality vs identity vs serialization); vendor traits; `VendorError`; `Timer`; listener, LogProducer and attachment traits; config skeleton; the explicit-construction replacement for DI | H | 0.3a–c |

### Phase 1: `jcompat` (runs alongside Phase 2)

| WP | Content | Opus | Depends on |
|---|---|---|---|
| 1.1 | `JavaPriorityQueue`, including comparator call traces | M | 0.2a |
| 1.2 | Double, Instant, Duration and date formatting and parsing; `DurationFormatUtils`; `String.format` subset | M | 0.2a |
| 1.3 | `StringSubstitutor` and its truth table | L | 0.2a |
| 1.4 | The regex wrapper over the Rust `regex` crate (D6): Kotlin `matches`/`find`/`split`/`replace` semantics, config-load errors naming the field, and the hard-coded marathon patterns ported to `regex` syntax | L | 0.3c |
| 1.5 | Gson writer, Jackson YAML emitter, Kotlin data-class `toString` | M | 0.2a |
| 1.6 | Logging: two-phase setup, logback pattern formatter, abbreviation, `%highlight`, level set on every `logger()` lookup (§3.5), Jansi terminal and redirect modes, `Timings` and `Xcodebuild` | M | 0.1 |
| 1.7 | md5 fragment hash, `escape()`, FileManager path rules | L | 0.2a |

### Phase 2: Configuration and CLI

| WP | Content | Opus | Depends on |
|---|---|---|---|
| 2.1 | `Node` layer and Jackson-compatible binding (coercion, unknown-key policy, null rules, Duration, Instant, File, Regex deserializers) | H | 0.3b, 1.3, 1.4, 0.6 |
| 2.2 | Top-level schema, strategies (per-field `timeLimit`), filters, analytics configs (lenient deserializers, `findValue`, `readOnly` drop) | M | 2.1 |
| 2.3a | Android, iOS and macOS vendor configs; derived defaults; path rules; SDK lookup | M | 2.1 |
| 2.3b | Apple bundle validation with extraction side effects; P2 Marathondevices profile; dynamic frame decoder | M | 2.1 |
| 2.4 | Staged validation: parse time, logical, vendor; exact messages; `toMap()` | M | 2.2, 2.3a, 2.3b, 1.5 |
| 2.5 | CLI argument and exit matrix; startup-phase logging; `version`; hidden `config-dump` | M | 2.4, 1.6 |
| 2.6 | Config differential suite on the oracle: corpus, staged outcomes, filesystem effects | M | 2.5, 0.2a |

### Phase 3: Core

| WP | Content | Opus | Depends on |
|---|---|---|---|
| 3.1 | `Job`, `Deferred`, `Actor`, `StateMachine`, core executor (per `lifecycle.md`); controlled-replay hooks | H | 0.6 |
| 3.2 | Strategies: pooling, sharding, sorting (fork-specific: `execution-time` keeps the position a test got when it was queued), batching, flakiness, retry (`RetryWatchdog`, `PoolTestFailureAccumulator`) | M | 0.6, 1.1, 2.2 |
| 3.3 | Filters, including the allure test plan and `FilterValuesFileReader` | M | 0.6, 1.4, 1.7, 2.2 |
| 3.4a | MetricsProvider: caching, retries, no-op fallback, observed durations, coverage and `Timings` logging | M | 3.2 |
| 3.4b | Influx v1, Influx v2 and Graphite clients with their whole lifecycle; container integration tests | M | 3.4a, 3.9a |
| 3.5 | `PoolProgressAccumulator` FSM, `TestRunResultsAccumulator`, progress lines, TeamCity | M | 3.1 |
| 3.6 | `DeviceActor` | H | 3.1 |
| 3.7 | `QueueActor`, including the fork's retry placement and its analytics use (`observeExecutionTimes`, sorting keys) | H | 3.2, 3.4a, 3.5, 3.6 |
| 3.8 | `DevicePoolActor` and `Scheduler` | H | 3.6, 3.7 |
| 3.9a | Event pipeline: `Track`, `MappingTracker`, `DelegatingTrackerInternal`, `ExecutionReportGenerator`, `ExecutionReport`/`PoolSummary`/`Summary`; shared `LogListener`/`LogWriter`/`LogProducer`, attachments, FileManager | M | 0.6, 1.7 |
| 3.9b | Reporters: DeviceInfo, Bill, JUnit, raw, test JSON, stdout; P3 parse YAML | M | 3.9a, 1.5 |
| 3.9c | Reporters: HTML (committed bundle), timeline, Allure (allure-java-commons model, `environment.xml`); `ScreenCapturer` and `GifSequenceWriter` | M | 3.9a |
| 3.10 | `Marathon` run flow (validation stage b, outputDir wipe timing, the parse path, `onFinish` order, shutdown hook, exit mapping); the core `Timings` lines (provider init, test parsing and device wait, shard preparation, scheduling start); the replacement for `di/Modules.kt`; StubVendor; scenario and replay tests (§6.5) | M | 3.8, 3.9b, 3.9c, 2.5, 3.3, 3.4b |

### Phase 4: Apple base

| WP | Content | Opus | Depends on |
|---|---|---|---|
| 4.1 | `CommandExecutor`, `LineBuffer`, `PollBackoff`, idle-timeout reader, `consume_output`, local executor (quote stripping), local file bridge | M | 0.6 |
| 4.2 | SSH executor: `shellCommand` quoting, TERM, exit codes, the `PatchedExecChannel` behaviours, `PerformanceDefaultConfig`, keep-alive, auth, known_hosts, connect attempts, transient-failure taxonomy | H | 4.1, 0.3a |
| 4.3 | `ConnectionFactory` (leases, 8 handshakes at a time, sharing, reachability); rsync bridge; `WriteFile`; `SharedTransfers` (fork-specific: push once per worker, reuse after an existence check, no `--delete` re-push from each device) | M | 4.2 |
| 4.4 | `AppleBinaryEnvironment` and tool wrappers (simctl services, xcodebuild argument builder with the Xcode ≥ 14 branch, xcresulttool with the `--legacy` ≥ 16 branch and the start-up log line with the effective timeout, nm, lipo, PlistBuddy, sqlite3, chmod, codesign, csrutil, getconf, ioreg, sw_vers, xcode-select, xcrun); models (`XcodeVersion`, `Sdk`, `OS`, `Platform`, `Arch`, `AppleTestBundle` with the local `Info.plist` and Mach-O reads) | M | 4.1, 4.5a, 4.6 |
| 4.5a | Plist reading (pinned `plist` version); xctestrun v0/v1/v2 models; byte-exact writer; Info.plist models; Mach-O detection | M | 0.1 |
| 4.5b | `TestRootFactory`: xctestrun generation for each test type, with the local temp file and the push | M | 4.4, 4.5a, 4.10a, 5.1 |
| 4.6 | xcresult schema codegen and legacy JSON | L | 0.1 |
| 4.7 | Log parser (`XctestEventProducer`, `DeviceFailureParser`, `TestRunProgressParser`); result listeners with xcresult enrichment | M | 4.6, 0.6 |
| 4.8 | `AppleDeviceTestRunner`, `XcodebuildSession` (one-minute close), `Xcodebuild` stderr logger; nm parser (parallel scans, failure on a scan that can't start); XCTest parser (`libxctest-parser` dylibs embedded and pushed); the `skipCopyingDerivedData` checks inside both parsers; `AppleTestBundleIdentifier` | M | 4.4, 4.7, 4.10a, 4.10b |
| 4.9 | `ArtifactProcessor` (limits, fairness, protect and release, `closeAndJoin`, admission stats); `ResultBundleRunListener` | H | 4.3, 4.7, 4.10a |
| 4.10a | Base `AppleDevice` and `RemoteFileManager` (worker paths and commands), plus the `LogConfigurator` impl; Marathondevices targets/transports were done in 2.3b and the log layout in 1.6 | L | 4.3, 4.4, 2.3b |
| 4.10b | `AppleApplicationInstaller` (witnesses the fork-specific PlugIns rule that 4.5b implements: iOS does not copy the xctest into `PlugIns`, macOS still does); `DistributedDerivedData` and the `skipCopyingDerivedData` checks for setup | M | 4.3, 4.5b, 4.10a |

### Phase 5: iOS and macOS

| WP | Content | Opus | Depends on |
|---|---|---|---|
| 5.1 | In-process `FakeSimulatorWorker` and fake executor, sharing the scripted world of 0.4b | M | 4.4, 0.4b |
| 5.2 | `AppleSimulatorDevice`: setup, prepare (parallel installation and simulator steps), boot (`bootWait`, two attempts, shutdown in between), lifecycle, permissions, media, keyboard; execute with failure classification, `BatchFailurePolicy` (3 in a row) and the fork's `maxBatchesWithoutProgress`, session gating, `joinCancelled`; dispose; `Timings` lines for simulator setup and prepare; `AppleSimulatorApplicationInstaller`, `AppleSimulatorMediaImporter` | H | 3.10, 4.8, 4.9, 4.10b, 5.1, 5.4 |
| 5.3 | Listeners: screen recording (pidfile wait at 100 ms up to 10 s), screenshots, device log, data container clear; `RecorderTypeSelector` | M | 5.2 |
| 5.4 | `WorkerProvisioningContext` (10 s inventory cache, worker facts and their invalidation), `SimulatorProvisioner` (`provisioningConcurrency`), `SimulatorFactory`, `ProvisioningPlan`, `DeviceTracker`; `Timings` lines for connect, plan, provision and shutdown of unused simulators | M | 4.3, 5.1 |
| 5.5a | `AppleSimulatorProvider`: static and dynamic providers (§3.3), update processing (only the added simulators are set up; removal semantics) | H | 5.2, 5.4 |
| 5.5b | `AppleSimulatorProvider`: reconnect loop, lost-worker teardown in parallel and outside the loop, recovery (three times, 0/30/60 s), retire rules, update leaving devices to recovery; a replaced simulator's connection is closed only at the end of the run; a pool whose last device is lost stops; worker retry and late join (`Detached`, `PendingWorker`) with the cutoff, re-checked after setup and before the first batch | H | 5.5a, 5.3 |
| 5.6 | macOS: provider (connects inside `initialize`), device, installer (`PlugIns` copy), TCC permissions with the SIP check, cancelled-batch wait of 1 min + 10 s with no lost-device report | M | 3.10, 4.8, 4.9, 4.10b |

### Phase 6: Android

| WP | Content | Opus | Depends on |
|---|---|---|---|
| 6.1 | `marathon-adb` client core: framing, host services, track-devices, features, shell v1 with the exit trick, shell v2, `exec:`, timeouts and cancellation | H | 0.6, 0.5 |
| 6.2 | Sync v1/v2, installs (streaming, legacy with md5, split session), uninstall, reverse forward, framebuffer | M | 6.1 |
| 6.3 | Instrumentation parser (port of adam's transformer and its tests), `TestAnnotationParser` v2–v4, logcat `-v long` parser | M | 0.1 |
| 6.4 | `marathon-dex`: DEX reader, JUnit3/4 discovery (dex-test-parser rules and tests), AXML, `ApkParser`, `InstrumentationInfo`. Compared with the JVM parser on fixture APKs and production APKs | H | 0.2a |
| 6.5 | Provider: multi-server `DeviceStateTracker`, `adb start-server`, serial strategies; `AndroidDevice` setup and prepare; `AndroidAppInstaller` | M | 6.2, 3.10 |
| 6.6a | Runner: `ArgumentsFactory` and the test-runner contract (§3.9), feature negotiation, requests, timeouts, exception translation; results and log listeners; remote test parser | H | 6.5, 6.3, 6.4 |
| 6.6b | Listeners: file sync, video chunks, screenshot GIF, adam screen capture, profiling | M | 6.6a, 3.9c |

### Phase 7: Integration and release

| WP | Content | Opus | Depends on |
|---|---|---|---|
| 7.1a | Differential harness: evidence format, strict comparator, JVM and native capture of the local and parse scenarios, CI workflow (see docs/wp/7.1.md, 7.1-findings.md) | M | 0.4b, 0.5, 3.10, 5.1 |
| 7.1b | Differential end-to-end suites per vendor on the fake worker (SSH and local) and the scripted ADB server; full-manifest comparison (§6.4); the 7.1a review residuals | M | 7.1a, 5.5b, 5.6, 6.6a |
| 7.2 | Real end-to-end for iOS, macOS and Android (§6.6) | M | 5.x, 6.x, 7.1b |
| 7.3 | Packaging: `marathon-<ver>.zip` → `marathon-<ver>/bin/marathon`; version from `GIT_TAG_NAME` at build time; per-target archives; GitHub Actions (test, build, tag-only release publishing; see docs/wp/7.3.md) | M | 2.5 |
| 7.4 | Docs: README, fork notes, deviations, migration | L | – |
| 7.5 | Drain the port queue: port each fork commit after `e47352f2`, then re-run every differential gate on the release source SHA | M | every WP except 7.6 |
| 7.6 | Isolated shadow runs, then canary cutover (§6.7) | – | 7.5 |

### Phase 8: Native cleanup

| WP | Content | Opus | Depends on |
|---|---|---|---|
| 8.1 | Native contract and goldens | M | – |
| 8.2 | Replace `JavaString` with `String` (docs/wp/8.2.md) | M | – |
| 8.3 | Replace JavaPath with PathBuf; drop jcompat utf8 and net (docs/wp/8.3.md) | M | 8.2 |

Later candidates:

- `jnum` and `jtime` (numbers and time; their parsers are the input syntax and stay)
- Typed errors in `Failure` instead of Java class strings.
- Serde writers instead of Gson/Jackson emulation.
- `tracing` instead of logback emulation.
- Plain async reads instead of Apple polling readers.
- `BTreeMap` with a sequence tie-breaker instead of `JavaPriorityQueue`.
- `image`/`gif` instead of ImageIO/AWT and the JDK sRGB profile.
- `Display`/serde instead of Kotlin `toString` rendering.
- Tokio `JoinSet`/`CancellationToken` instead of coroutine emulation.
- `clap` instead of Clikt emulation.
- A YAML library with a small YAML 1.1 scalar layer instead of the SnakeYAML port, last because input risk is highest.

**Critical path**, effort-weighted and computed by `scripts/check_plan_dag.py` (which also fails on unknown dependencies and cycles):

0.1 → 0.3a → 0.4a → 0.4b → (Phase 0 exit) → 2.1 → 2.2 → 3.2 → 3.4a → 3.7 → 3.8 → 3.10 → 5.2 → 5.5a → 5.5b → 7.2 → 7.5 → 7.6.

The SSH spike and the fake worker lead Phase 0. Configuration is on the critical path after it, because the strategies need its types. The oracle branch, 0.2a → 0.2b, and the other spikes, 0.2a → 0.3b/0.3c → 0.6, run alongside.

**Running alongside it:**
- `jcompat` (Phase 1), from 0.2 on;
- after Phase 0 exit: Apple base (Phase 4) and the Android leaves (6.1–6.4);
- the differential suite 7.1, from 3.10 on.

At most about 5 implementers run at once.

## 8. Agent workflow

### 8.1 Roles

| Role | Model | Effort |
|---|---|---|
| Orchestrator (this thread) | Opus | Writes WP briefs, keeps the inventories, merges, runs the gates |
| Implementer | Opus, one git worktree per WP | L, M or H as in §7 |
| Primary reviewer | Sol (`gpt-6.1-sol`, via `delegate_task`) | Medium; high for H WPs |
| Secondary reviewer and auditor | Muse (`muse-spark-1.3-contributor`, unlimited) | High; xhigh for H WPs |
| Research, corpora, docs drafts | Muse (`muse-spark-1.3-contributor`) | High or xhigh |

What Sol checks:
- semantic parity with the cited Kotlin source;
- concurrency, cancellation and lifecycle;
- the error taxonomy;
- that the tests assert the Kotlin behaviour.

What Muse checks:
- test-port completeness against the parity rows;
- a line-by-line diff of the Kotlin and Rust logic;
- `jcompat` misuse, for example `BinaryHeap` instead of `JavaPriorityQueue`, or a raw regex instead of `JavaRegex`.

### 8.2 Loop per WP

1. The orchestrator writes the brief.
2. The Opus implementer ports the code and tests and runs the gate.
3. Sol and Muse review **the same commit SHA** in parallel. Findings go into `docs/wp/<id>-findings.md`, each with its severity (blocking, major or minor), reviewer, SHA and status.
4. The implementer fixes the findings or answers any it disputes, then pushes a new SHA.
5. The next review round is a fresh `delegate_task` containing the original brief, the earlier findings, the answers and anything still open. Muse re-audits the parity rows the fixes touched.
6. **Closure:** every blocking and major finding from *both* reviewers is resolved or explicitly waived.
   - The orchestrator records the reason for each waiver.
   - A waiver that changes observable behaviour needs your approval.
   - Three rounds without closure escalate the WP; they never close it.
7. Merge to `main`; the full gate runs again on the merged SHA, and the parity matrix and source inventory are updated.

### 8.3 Gate (for each WP and for `main`)

- **Checks:** `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`.
- **Linux:** the same gate passes on Linux x86_64 through `scripts/remote-check.sh` (the Linux box over SSH). GitHub Actions does not run for this repository.
- **Coverage:** the WP's parity rows are complete and its inventory files are assigned.
- **Oracle:** the oracle and differential suites the WP touches are clean.
- **Code rules:** no blocking calls on the core thread; no `unwrap()` on I/O paths.

## 9. Prerequisites and infrastructure

- **This machine:**
  - `rustup`;
  - the reference JDK: Android Studio's bundled OpenJDK 21.0.10. The fork's CI uses Temurin 21.0.x; both are OpenJDK 21, and WP 0.2 confirms the CI version from a CI job log. Logs can differ between the two JDK builds, so the oracle normalises JVM-specific log text;
  - `sshpass`, from a tap, only when 0.4a tests password auth;
  - Docker or colima for the Influx, Graphite and sshd containers;
  - a macOS VM with Xcode and snapshot restore (e.g. tart), for the isolated macOS comparison runs (§6.7).
- **Repository:** `marathon-rs` is not a git repository yet. WP 0.1 runs `git init`; the remote is your decision (§11).
- **CI:** the runner image needs Rust. macOS binaries need a macOS runner or a cross-compile (`cargo-zigbuild` with the SDK), decided in 7.3.
- **Real SSH workers:** two Macs reachable over SSH.
  - `ssh remote-mac` (an SSH config alias) reaches the first one (macOS 26.7.1, arm64, Xcode 26.6, 30 simulators, openrsync as `/usr/bin/rsync`).
  - The second is this Mac (macOS 27.0.1, Xcode 27.0, 22 simulators). It runs over local transport, and over SSH as a worker of the remote Mac's run if that is ever needed.
  - They provide the real sshd for spike 0.3a and the SSH-transport end-to-end runs (§6.6).
  - Concurrent comparison runs use one Mac per implementation, which satisfies the disjoint-worker rule of §6.7. The two Macs have different Xcode versions (26.6 and 27.0), so concurrent comparisons swap Macs between rounds and compare per-Mac outcomes; exact output diffs come from sequential runs on the same Mac.
- **Linux box** (Ubuntu 26.04, x86_64, 16 cores, 91 GB, KVM, Java, rsync; reachable over SSH):
  - the Linux x86_64 build and tests;
  - Android emulators under KVM, for the WP 7.2 Android end-to-end runs;
  - the sshd, Influx and Graphite containers, once a container runtime is installed there (it has no Docker yet). Until then, colima on this Mac.
- **No Remote Login on this Mac:** the remote Mac and a containerised sshd replace it.
- **The remote rsync is openrsync.** Real macOS workers run Apple's openrsync, not Samba rsync. The fake worker (0.4a) uses the same remote rsync flavour as the real workers.
- **Android:** a fresh owned emulator on the reserved Linux KVM worker for WP 7.2 end-to-end runs. Shadow runs on Android devices need a device pool, still to be named.

## 10. Risks and release gates

| Risk | Mitigation |
|---|---|
| Jackson/SnakeYAML binding edge cases | `Node` layer that keeps raw tokens; spike 0.3b; oracle mutation corpus with staged outcomes |
| Java regex semantics | Spike 0.3c; translator plus backend; D6 for constructs we can't support |
| Scheduling and lifecycle divergence | `lifecycle.md`; `JavaPriorityQueue` with comparator traces; controlled replay against the oracle; property tests |
| sshj vs russh | Spike 0.3a with a fallback backend; real-sshd contract tests; the fake worker exercises both clients |
| Simulator provider state machine | Fake worker built and validated against Kotlin first; WPs 5.5a and 5.5b in order; H effort for implementer and reviewer |
| New ADB client and DEX parser | Scripted ADB server validated against Kotlin/adam; the replaced libraries' own tests ported; APK comparison with the JVM |
| The fork keeps moving | Port queue; 7.5 drain plus differential re-run on the recorded release SHA |
| Upstream marathon releases | After cutover, upstream changes are ported by hand |

**Release gates:**
- the source inventory has no unowned files, and the parity matrix has no `pending` rows;
- every finding is closed;
- the oracle and differential suites are clean on the release SHA;
- real end-to-end runs pass for iOS, macOS and Android;
- shadow runs show no unexplained divergence;
- the port queue is drained.

## 11. Decisions (2026-10-12)

1. **Deviations:**
   - D1, with an INFO line saying the telemetry options are ignored;
   - D2, D3, D4 and D6 as proposed;
   - D5, with the maintained list `docs/known-kotlin-bugs.md`.
   - D6 rewritten (2026-10-23): user-supplied regexes use the Rust `regex` crate, not Java regex semantics. This supersedes the original D6 and its runtime-budget extension, which was decided earlier the same day.
   - D7, added after the SSH spike: the stale TERM after a non-TERM signal is not reproduced.
   - D8 (2026-10-23), after WP 1.2: locale-dependent output is always formatted as en-US.
   - D9 (2026-10-24), from the WP 2.1 brief: simplified Jackson binding edge cases (proposals P1, P2, P4, P5 and P6). P3 (canonicalising absolute paths) was rejected because keeping Kotlin's verbatim spelling is simpler.
   - D10 (2026-10-24), from the WP 4.1 decisions: Apple command output is UTF-8 only and decoded incrementally, and the reader polls pipes asynchronously while keeping the idle clock, the 20 ms grace and `PollBackoff`.
   - D11 (2026-10-24), from the WP 6.1 decisions: adb streamed text is decoded as UTF-8 across reads, with no vert.x buffer-pool or busy-spin emulation.
   - D12 (2026-10-24), from the WP 2.5 decisions: JVM runtime artefacts in CLI output (the SLF4J line, stack frames, identity hashes, argfiles and completion, TTY help width) are not reproduced.
   - D12 extended (2026-10-25), from the WP 3.10 decisions: native `System Information:` lines instead of JVM properties; a native signal relay instead of JVM shutdown hooks, with the JVM exit statuses kept.
   - D13 (2026-10-24), from the WP 3.2 decisions: each pool's metrics history is prefetched when its queue is built (superseding 0.6 decision D4); a pool that never compares tests makes one extra history query and timing line.
   - D14 (2026-10-25), from WP 6.5 (P1, P3 and P5a): Tokio tasks replace the Android thread pools, the provider owns the effective file sync set, and a failed reverse port-forward removal at dispose is logged. See `docs/wp/6.5-findings.md`.
   - D15 (2026-10-25), from WP 6.6a (P2): a failed Android remote-parsing attempt always unregisters its log accumulator and cancels its instrumentation producers (N36, N37). See `docs/wp/6.6a-findings.md`.
   - D16 (2026-10-26), from WP 6.6b (P1, P3, P4 and P5; P2 rejected): Android artifact cleanup without blocking sleeps or recorder races, and profiling stopped on run failure. See `docs/wp/6.6b-findings.md`.
   - D17 (2026-10-31), from WP 5.3: an iOS screen-recording stop whose recorder has already exited counts as successful (U100). See `docs/wp/5.3-findings.md`.
   - D18 (2026-11-02), by the user's decision: Android devices are keyed by adb server and serial, the same-serial half of N27. See `docs/wp/x1-findings.md` and `docs/wp/x2-findings.md`.
   - D19 (2026-11-03), by the user's decision: the Apple command reader reads output that arrives with EOF (U145). See `docs/wp/x9-findings.md`.
   - D20 (2026-11-04), by the user's decision: the `xctest` parser library is supplied by the user, not shipped. See `docs/wp/x11-findings.md`.
   - D21–D28 (2026-11-06), from WP x12, by the project owner's decision: eight Android bugs kept for parity are fixed (N12, N16, N5, N27's retention with N29, N38, N10, N4, N35). See `docs/wp/x12-findings.md`. D22 and D25 were extended the same day by WP x18, after the real-emulator checks: `docs/wp/x18-findings.md`.
   - D29–D32 (2026-11-06), from WP x13, by the project owner's decision: four SSH and rsync bugs kept for parity are fixed (A33, A14, A3, and of A2 only `NSUnbufferedIO`). See `docs/wp/x13-findings.md`.
   - D33 (2026-11-06), from WP x14, by the project owner's decision: a simulator whose setup failed is set up again, a bounded number of times (A81). See `docs/wp/x14-findings.md`.
   - D34 (2026-11-06), from WP x15, by the project owner's decision: a failed history query is refreshed off core, bounded, and takes effect at the next queue build or batch (C41). See `docs/wp/x15-findings.md`. Hardened by WP x22 (2026-11-07): the bound on `close`, a cleanup guard, the wording for an empty answer, and tests through the real clients; see `docs/wp/x22-findings.md`.
   - D35–D37 (2026-11-06), from WP x16, by the project owner's decision: three SSH connection bugs kept for parity are fixed (A16, A17, A32). See `docs/wp/x16-findings.md`.
   - D38–D40 (2026-11-06), from WP x17, by the project owner's decision: a failed readiness probe, device failure text in a test's own output, and a stale device removal (A41, A48, C27). See `docs/wp/x17-findings.md`.
   - D41 (2026-11-07), from WP x19, by the project owner's decision: a brief `offline` does not lose the device (N40). See `docs/wp/x19-findings.md`.
   - D42 (2026-11-07), from WP x20, by the project owner's decision: a remote parsing session that fails short of its list fails the attempt (the rest of N38). See `docs/wp/x20-findings.md`.
   - D43 (2026-11-07), from WP x26, by the project owner's decision: an rsync receive has a limit on progress, not a 30 s total. See `docs/wp/x26-findings.md`.
   - D44 (2026-11-07), from WP x27, by the project owner's decision: a lost simulator's reconnect has a limit, a failed re-provisioning is retried, and `borrow` skips simulators that cannot work (A101, A80). See `docs/wp/x27-findings.md`.
   - D45 (2026-11-07), from WP x28, by the project owner's decision: a hung test is a failure, whatever form its allowance is printed in (A47), and D39 extended so that an output timeout with no progress counts as a batch failure (U165). See `docs/wp/x28-findings.md`.
   - D46 (2026-11-07), from WP x29, by the project owner's decision: the readiness probe is `launchctl print user/foreground` from runtime 18, with `dumpstate` kept for 16 and 17 (A41's cost). See `docs/wp/x29-findings.md`.
   - D47 (2026-11-07), from WP x30, by the project owner's decision: a run with an unlimited `uncompletedTestRetryQuota` is refused at config parsing, since nothing else bounds the uncompleted loop (C85). See `docs/wp/x30-findings.md`.
   - D48 and D49 (2026-11-07), from WP x31, by the project owner's decision: an emptied pool waits for a device to return, and a lost adb request does not retire a device still there (C50, N15). See `docs/wp/x31-findings.md`.
   - D50 (2026-11-07), from WP x32, by the project owner's decision: a test that `fixed-size` batching rejects does not use a batch slot (C33). See `docs/wp/x32-findings.md`.
   - D51 (2026-11-07), from WP x34, by the project owner's decision: analytics and cleanup failures do not stop a run, a Graphite `None` is skipped, and an empty filtered run logs an error (C62, C38, C11, C25). See `docs/wp/x34-findings.md`.
   - D52 (2026-11-07), from WP x35, by the project owner's decision: a cancel always writes what it can, within a limit, whatever `debug` says (C10, U161). See `docs/wp/x35-findings.md`.
   - D53 (2026-11-08), from WP x44, by the project owner's decision: remote parsing tries another device after a failed attempt, on Android, iOS and macOS, and macOS parsing takes only a connected Mac (A103, U171). See `docs/wp/x44-findings.md`.
   - D54 (2026-11-08), from WP x46, by the project owner's decision: an iOS or macOS send stops when its call is cancelled, and any transfer when its device's connection is lost, so a remote parse no longer hangs on a frozen worker (A104). See `docs/wp/x46-findings.md`.
2. **Repository:** a new private GitHub repository. The code is derived from marathon, which is GPL-2.0, so the repository carries the same `LICENSE`.
3. **JDK:** The fork's CI uses Temurin 21.0.x. The oracle uses Android Studio's OpenJDK 21.0.10. The CI version is confirmed from a CI job log in WP 0.2a, and JVM-specific log text is normalised.
4. **Farm:**
   - the remote Mac over SSH, and this Mac;
   - the Linux box, for Linux builds, Android emulators (KVM) and containers;
   - Android devices for shadow runs: to be named.
5. **Binary targets:** `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu`.
   - Note, 2026-11-04: the release targets are now `aarch64-apple-darwin` and `x86_64-unknown-linux-musl` (`docs/releasing.md`); the gnu build remains a test target of the gate. This decision stays as it was taken.
6. **Installs:**
   - approved: `rustup`, colima (+ docker CLI), `sshpass` (only when needed) and tart (only for the macOS VM runs);
   - no JDK install (Android Studio's is used);
   - no Remote Login.

## 12. Review log

Full texts: `docs/reviews/plan-r1-sol.md`, `plan-r1-muse.md`, `plan-r2-muse.md`, `plan-r2-sol.md`, `plan-r3-sol.md`.

**Round 1, on revision 1.**
- *Sol:* 25 findings, all adopted in revision 2.
- *Muse:* coverage gaps and owners for fork behaviour adopted. Two claims rejected: plist ordering (plist 1.x is `indexmap`-backed) and the Android log pattern (`AndroidLogConfigurator` has its own). Sol agreed with both rejections in round 2.

**Round 2, on revision 2.**

*Sol:* 18 findings resolved, 7 partly resolved, and 3 new. All of them are adopted in revision 3:

| Finding | Where it is addressed |
|---|---|
| Shadow runs: disjoint simulators don't isolate `/tmp/marathon/shared` (blocking) | §6.7: disjoint workers, or sequential runs that restore the whole `/tmp/marathon` |
| Host-side operations aren't seen by the fake SSH worker | §6.4: host-side table, argv shims, real bundle fixtures, OpenSSH and rsync streams in both directions, local-transport suites |
| Oracle injection was only a promise | §6.3: hook inventory, `OracleHooks` with three virtual clocks, draw and ID tapes, patch validation; §6.5: event script and quiescence |
| Explicit-`null` rule was wrong | §3.2: primitive fields take the zero value; reference fields reject; nullable fields accept |
| Logging rules were wrong | §3.5: the level is set on every `logger()` call; Jansi strips ANSI when output is redirected |
| Dependency graph | §7: the 7.5/7.6 cycle removed; 4.5 split into 4.5a/4.5b and 4.10 into 4.10a/4.10b; 3.7 now depends on 3.4a; Phase 0 exit barrier added |

*Muse:*
- **Source inventory:** generated now (490 files, none unowned, 11 `n/a`).
- **Remaining fork-specific details:** placed in 3.2, 3.10, 4.4, 4.8, 5.5b and 0.4b.
- **Host-side execution list:** merged into §6.4.
- **D2 wording:** fixed.
- **`ScpFileBridge`:** recorded as dead code.

**Round 3, on revision 3.**

*Sol:* no blocking issues. The remaining items are fixed in revision 4:
- **Logging wording:** WP 1.6 now says the level is set on every `logger()` lookup.
- **Apple dependencies:** 4.4 → 4.6, 4.7 → 4.10a, 4.8 → 4.10b, 4.9 → 4.10a.
- **Inventory:** it covers all 505 main files, `buildSrc` and `sample` included. The codegen goes to 4.6, the `Xcresulttool` wrapper to 4.4, and the build logic and samples are `n/a` with reasons.
- **macOS isolation:** sequential reuse always restores a full host snapshot (in practice a macOS VM); if that is impossible, disjoint hosts are used (§6.7). *Round 4* found the earlier no-permissions alternative insufficient, and it was removed. Round 4 also moved `ProfilingRunListener` to 6.6b.
- **Critical path:** recomputed by `scripts/check_plan_dag.py`.

**Rounds 4 and 5 (confirmation passes).** Round 4 left two items open, both fixed in revision 4.1:
- `ProfilingRunListener` moved to 6.6b;
- sequential macOS runs always restore a full host snapshot.

Round 5 confirmed both and accepted revision 4.1 as the implementation baseline (`docs/reviews/plan-r4-sol.md`).

