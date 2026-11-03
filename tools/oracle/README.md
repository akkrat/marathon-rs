# Kotlin oracle

The reference build of the Kotlin marathon (commit `e47352f2`) and the JVM tools that generate the oracle tables and goldens
(PLAN.md §6.3, WP 0.2a). None of this runs in `scripts/check.sh` or CI: it needs the Kotlin repository and a JDK.

## Layout

| Path | What |
|---|---|
| `build.sh` | clones, patches and builds the Kotlin marathon and the oracle tools |
| `patches/*.patch` | oracle-only patches applied to the clone with `git apply`. `0001` registers the oracle tools; `0002` adds inactive-by-default replay hooks to the separate patched clone |
| `module/` | the sources of the `:oracle` Gradle subproject. `0001-oracle-module.patch` includes it from the clone's `settings.gradle.kts` as `project(":oracle").projectDir = file("../../module")`, so the sources stay reviewable here instead of inside a patch |
| `oracle` | runs a command of the built tools; `config-dump`, `jcompat-tables`, `yaml-corpus` and `regex-corpus` are shorthands for it |
| `config-dump-goldens.sh` | regenerates `fixtures/oracle/config-dump/` |
| `cli-matrix/generate.py` | WP 2.5: records the reference CLI per argument list into `fixtures/oracle/cli-matrix.jsonl` (see below) |
| `instant-parse-probe/` | WP 2.5 review round 1: `Instant.parse` results and exception chains into `fixtures/oracle/jcompat/instant_parse_errors.jsonl` (see below) |
| `plist-probes/` | WP 4.5a JVM probes against dd-plist 1.28 and the Kotlin plist models (`PlistProbes.java`, recorded `expected.txt`; see its `README.md`) |
| `strategy-probes/` | WP 3.2 JVM probes of the e47352f2 strategies against the reference `installDist` jars (`StrategyProbes.kt`; `run [OUT_DIR]` writes `fixtures/oracle/strategies/*.jsonl`) |
| `queue-probes/` | WP 3.7 JVM probe driving the e47352f2 `QueueActor` with the real `PoolProgressAccumulator` (`QueueProbes.kt`; `run [OUT_DIR]` writes `fixtures/oracle/queue/queue.jsonl`) |
| `reader-race-probe/` | WP x9 JVM probe of the e47352f2 `produceLinesManually` with stdout and EOF arriving between `available()` and `canRead()` (`ReaderRaceProbe.java`, recorded `expected.txt`; evidence for D19, see its `README.md`) |
| `run-isolated.sh` | runs a marathon binary in a fresh sandbox and writes a manifest |
| `test-run-isolated.sh` | tests the isolation checks of `run-isolated.sh` |
| `build/` (gitignored) | the clone, `build/marathon-src`, and all build outputs |
| `runs/` (gitignored) | run sandboxes |

## Reference JDK

Android Studio's JBR, `JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"`. `java -version`:

```
openjdk version "21.0.10" 2026-01-20
OpenJDK Runtime Environment (build 21.0.10+-117844308-b1163.108)
OpenJDK 64-Bit Server VM (build 21.0.10+-117844308-b1163.108, mixed mode)
```

Every script defaults `JAVA_HOME` to it; set `JAVA_HOME` to use another JDK (then the tables record that JDK in their headers).

## Building

```sh
tools/oracle/build.sh
```

- Clones `MARATHON_KT` (default `~/src/marathon`) with `git clone --no-hardlinks` into
  `build/marathon-src` and `build/marathon-patched`, checks out `e47352f2`, and applies only `0001` to the reference and all
  patches to the patched clone. `build.sh reference` or `build.sh patched` selects one variant. The source repository is only read.
- Runs `./gradlew :cli:installDist :oracle:installDist` in the clone. Extra Gradle flags go in `ORACLE_GRADLE_FLAGS`, for
  example `ORACLE_GRADLE_FLAGS=--offline` when the Gradle cache is warm and the network is not available.
- Smoke-tests the distribution with `marathon version`.
- Before every build it verifies the clone: HEAD must be the pinned commit, and the working tree must equal the pinned tree
  with the patches applied. Every file counts, whether `.gitignore` excludes it or not (Gradle compiles an ignored file under a
  source directory too), except the build outputs listed in `BUILD_OUTPUTS` in `build.sh` (`.gradle/`, the projects'
  `build/` directories and `oracle-build/`). Any other change, such as an edited or added Kotlin source, stops the build.
- Running it again reuses the clone, skips applied patches and lets Gradle's up-to-date checks do the rest. Delete `build/` to
  start from scratch.

Outputs:
- the reference distribution: `build/marathon-src/cli/build/install/marathon/bin/marathon`;
- the oracle tools: `build/marathon-src/oracle-build/install/oracle/bin/oracle` (the module's build directory is redirected
  into the clone, so nothing is built inside `tools/oracle/module`).

## Tools

### config-dump

```sh
tools/oracle/config-dump <Marathonfile> [--env-file F] [--now <ISO instant>]
```

Parses the Marathonfile as the CLI does, runs the logical validation of the run path and prints one JSON document: the stage
that rejected it, the error, the canonical configuration and the filesystem changes under the Marathonfile's directory. The
format is specified in `docs/design/config-dump-format.md`. `--env-file` makes the environment exactly the file's `KEY=VALUE`
lines (the tool re-executes itself); `--now` fixes the reference time of relative `timeLimit`s.

Parsing an Apple configuration extracts `.ipa`/`.zip` bundles next to them, so run it on a copy of the inputs.
`config-dump-goldens.sh` does that for the fixture goldens.

### jcompat-tables, yaml-corpus, regex-corpus

```sh
tools/oracle/jcompat-tables fixtures/oracle/jcompat
tools/oracle/oracle string-substitutor-edges fixtures/oracle/jcompat
tools/oracle/yaml-corpus fixtures/oracle/yaml-corpus.jsonl
tools/oracle/yaml-review-corpus fixtures/oracle/yaml-review-corpus.jsonl
tools/oracle/yaml-round3-corpus fixtures/oracle/yaml-round3-corpus.jsonl
tools/oracle/regex-corpus fixtures/oracle/regex-corpus.jsonl
tools/oracle/config-dump-goldens.sh
tools/oracle/validation-corpus
```

Each writes JSONL tables with a schema header line; `fixtures/oracle/README.md` lists them. The generators use fixed seeds
and a fixed "now", so their output is byte-stable on a given JDK.

- `jcompat-tables`: the PLAN.md §4.3 tables: `PriorityQueue`, `Double.toString`, `String.format`, `Instant`/`Duration`,
  date formats, `DurationFormatUtils`, `StringSubstitutor`, the fragmentation hash, Gson and the `parse -o` YAML emitter.
- `yaml-corpus`: docs/research/yaml.md §5 rows, the strategy `timeLimit` rows, and the PLAN.md §6.3 mutation families over a
  base Android Marathonfile, each as a Jackson token stream and as a binding result.
- `yaml-round3-corpus`: WP 0.3b round-3 regressions; fixed jnum panic and prototype binding rows marked `binding_expected_fail` for WP 2.1. Expected results still come from the JVM mapper.
- `yaml-review-corpus`: WP 0.3b round-1 review regressions, byte decoding and formatter probes. Uses the same mapper and simple targets; reads context/problem/location marks directly from exceptions.
- `regex-corpus`: docs/research/regex.md §4, families 1–11. Adversarial rows run under a watchdog that counts the matcher's
  character reads (limit 20,000,000) instead of timing it, so an aborted match is recorded deterministically.

### device-actor-probe

```sh
tools/oracle/oracle device-actor-probe /tmp/device-actor.jsonl
cmp /tmp/device-actor.jsonl fixtures/oracle/device-actor/device-actor.jsonl
```

WP 3.6: the real DeviceActor of `:core` on a `StandardTestDispatcher`, scenario by scenario (`DeviceActorProbe.kt`); see
`fixtures/oracle/README.md`.

### validation-corpus

```sh
tools/oracle/validation-corpus [output dir]
```

WP 2.4: whole Marathonfiles (inline cases, then the copied fixture configs) through `ConfigurationFactory.parse` and
`LogicalConfigurationValidator`, recording the rejecting stage and error, `toMap()` and, for marked rows, `serialize()`
(`ValidationCorpus.kt`). Writes `config-validation-corpus.jsonl` into the output dir (default `fixtures/oracle`). Every row
runs on a temp copy, so nothing is extracted into `fixtures/`.

### cli-matrix

```sh
tools/oracle/build.sh reference   # once; or point MARATHON_CLI at another :cli:installDist start script
python3 -I tools/oracle/cli-matrix/generate.py
```

WP 2.5: runs the reference `marathon` once per case of `generate.py` (help, usage errors, value forms, the Marathonfile
check, configuration errors up to the vendor gate), each in a fresh sandbox under `runs/` with stdin closed and a minimal
environment, and writes `fixtures/oracle/cli-matrix.jsonl` with normalised stdout, stderr and exit status. Rows whose
`program` is `config-dump` run `tools/oracle/config-dump` instead (`CONFIG_DUMP` overrides it), and `marathon config-dump`
on the Rust side. The header lists the normalisation; rows changed by D1, D12 or a recorded simplification carry a `rust`
expectation. `run-isolated.sh` is not used: it refuses `--`
and subcommands that are not first, and none of the cases writes outside its sandbox. The Rust side is
`crates/marathon-cli/tests/cli_matrix.rs`; `marathon config-dump` is checked against the config-dump goldens by
`crates/marathon-cli/tests/config_dump_goldens.rs`.

### instant-parse-probe

```sh
java tools/oracle/instant-parse-probe/InstantParseProbe.java > fixtures/oracle/jcompat/instant_parse_errors.jsonl
```

WP 2.5 review round 1: `Instant.parse` over hand-picked edges and systematic edits of three valid instants, recording the
`toString` of the result or each level of the exception chain. `jcompat::jtime::parse_instant_detailed` is checked against
it by `crates/jcompat/tests/formatting_oracle.rs`. Single-file source launch on the reference JDK.

### canonical-file-probe

```sh
java tools/oracle/canonical-file-probe/CanonProbe.java
```

WP 2.4 review round 1: `File.canonicalFile` through symlink loops (`loop` -> `loop`, `a` <-> `b`) at the leaf and
mid-path. Single-file source launch on any JDK 11+; run it on both macOS and Linux, since the JDKs differ (see
`crates/marathon-config/src/paths.rs`).

### run-isolated.sh

```sh
tools/oracle/run-isolated.sh [--id ID] [--timeout SECONDS] [--env NAME=VALUE]... [--pass NAME]... [--] BINARY INPUT_DIR [ARG...]
```

Copies `INPUT_DIR` to `runs/<id>/in`, runs `BINARY ARG...` there with `HOME=runs/<id>/home`, `TMPDIR=runs/<id>/tmp`,
`PATH=/usr/bin:/bin:/usr/sbin:/sbin` and only the variables given with `--env`/`--pass`, kills its process group after the
timeout (default 600 s), and writes `stdout`, `stderr` and `manifest.json` (run metadata, exit code, the input tree, and the
tree of `in/`, `home/` and `tmp/` afterwards with sizes and sha256). `@IN@` in an argument stands for the sandbox's `in`
directory. The id defaults to a timestamp plus a random suffix; an existing run directory is never reused.

Every writable location must lie in the run directory, so the sandbox is rejected before the run (exit 3, run directory
removed) when the input holds a symlink that resolves outside it, when `HOME` or `TMPDIR` is overridden to a path outside
the run directory, when an absolute argument lies outside it, when the Marathonfile's `outputDir`, `application` or
`testApplication` resolves outside it, or when `parse -o NAME` would write `<outputDir>/NAME.yml` outside it. The
Marathonfile values are decoded by `oracle marathonfile-writes`, which runs marathon's substitution (with the environment
the binary gets) and marathon's YAML mapper, so quoted keys, flow mappings, block scalars and duplicate keys decode as
marathon decodes them; the oracle must therefore be built. The checks fail closed on what they do not model: a
subcommand must be the first argument (Clikt does not pass root options down, so `-m a.yml parse` would make `parse` read
`./Marathonfile`), `--` is refused, and so are environment names or values with line breaks, which the helper's
`KEY=VALUE` transport would change. `tools/oracle/test-run-isolated.sh` has a regression case for
each of these, with `/bin/echo` as the binary.

The Kotlin binary needs `JAVA_HOME`:

```sh
K=tools/oracle/build/marathon-src/cli/build/install/marathon/bin/marathon
J="JAVA_HOME=/Applications/Android Studio.app/Contents/jbr/Contents/Home"
tools/oracle/run-isolated.sh --env "$J" "$PWD/$K" fixtures/configuration/fixture/config/ios version
tools/oracle/run-isolated.sh --env "$J" "$PWD/$K" fixtures/configuration/fixture/config/ios parse -m @IN@/sample_6.yaml -o tests
```

## Unpatched reference limits

- `--now` works through `ConfigurationFactory`'s `mapper` argument, which only the oracle can pass. The CLI distribution
  itself always uses `InstantTimeProviderImpl` (the real clock), so a Kotlin run of a Marathonfile with a relative
  `timeLimit` cannot be pinned to a "now" without a behaviour patch (the `OracleHooks` clock of PLAN.md §6.3).

## Controlled replay (WP 0.2b)

The reference paths above remain unpatched. The hooked CLI and oracle are under `build/marathon-patched` at the same relative
paths. With no tape, hooks delegate the original expressions. With `-Doracle.hooks.tape=...`, the scenario driver supplies the
virtual core dispatcher, per-site draw/ID tapes, and quiescence checks. See [replay-tapes](../../docs/design/replay-tapes.md)
for formats and commands and [nondeterminism](../../docs/design/nondeterminism.md) for inventory and validation boundaries.

`tools/oracle/validate-patches.sh` builds both variants, runs the full non-Docker Kotlin unit suite, compares ten repeated
runs of each source core scenario and a controlled Android CLI run, then compares fresh reference/taped output pairs. It
writes evidence under `build/validation/`, not under fixtures. `--runs 1` reduces repetitions explicitly; `--parts a,b,c` and
`--skip-build` are iteration options. `docs/wp/0.2b-findings.md` records the actual acceptance run and timings.

### Regex spike round-1 regressions

`tools/oracle/module/src/main/resources/regex-round1.jsonl` contains review inputs only; it has no hand-written expected
answers. `RegexCorpus` adds them to the original families and runs every case through Kotlin's real `Regex`, including
`split`/`replace` continuation state. After rebuilding `:oracle:installDist`, generate the extended table with:

```sh
tools/oracle/regex-corpus fixtures/oracle/regex-round1-corpus.jsonl
```

JDK 21 Unicode tables for the Rust translator have a separate generator; see
[`regex-unicode/README.md`](regex-unicode/README.md).
WP 1.7 adds `jcompat/file_manager_edges.jsonl` via `FileManagerEdges.kt`.
The regular `jcompat-tables` command generates it alongside the original tables.
Generate into a fresh directory and copy only this new file when extending the
coverage; existing fixtures remain unchanged. It records UTF-16 units to retain
surrogate-splitting truncation. Absolute File cases normalize the runtime working
directory to `/oracle-cwd`; scratch paths are fixed at `/tmp/oracle-wp17`.
### Writer probes (WP 1.5)

```sh
tools/oracle/writer-probes/run > fixtures/oracle/jcompat/writer_edges.jsonl
python3 tools/oracle/writer-probes/prepare_cases.py
tools/oracle/writer-probes/kotlin-inputs \
  tools/oracle/build/marathon-src/oracle-build/install/oracle/lib fixtures/oracle/jcompat
```

The first command uses the pinned cached Maven jars and the reference JDK, without
building marathon. The third compiles the updated Kotlin oracle sources against
an existing reference distribution's read-only jars; it writes only the two new
Kotlin writer tables. Normal `jcompat-tables` also writes `kotlin_writer_models`.
The lightweight probes compile in temporary directories and do not edit reference
sources or existing tables. `writer_models` preserves existing JVM expectations
as UTF-16 arrays because serde_json cannot decode lone surrogates in JSON strings.
Its inputs are explicit model projections; C60c checks emission of a bound model,
while WP 2.1 owns configuration binding, defaults and relative-path resolution.

WP 1.5 round-1 line-break regressions use the same JVM writer probes:

```sh
tools/oracle/writer-probes/run --round1 > fixtures/oracle/jcompat/writer_round1.jsonl
```

This writes 18 new rows, without changing the original writer table.
### Logging corpus (WP 1.6)

After `tools/oracle/build.sh reference`, run
`python3 tools/oracle/logging-corpus.py`. This calls `oracle logging-probe` in
fresh JVMs and captures actual logback/Jansi output using a PTY, pipe and file.
The fixed event clock follows `hooks/Logging.kt`; it does not change messages,
levels or the vendor stream. See `fixtures/oracle/logging/README.md` for the
256 logging rows and pinned environment. The existing jcompat logback generator
also records complete throwable-cause inputs without changing expected bytes.

### Kotlin regex operations (WP 1.4, rescoped D6)

`tools/oracle/regex-operations/run > fixtures/oracle/jcompat/regex_operations.jsonl`
compiles a small Kotlin 1.9.24 probe from cached jars and runs it on the reference
JDK. The 77 rows cover shared-syntax findAll/split/replace behavior; no Java
translator or regex-flavour oracle is needed. Compilation stays in a temporary
directory. Under D6 the Rust wrapper uses Unicode scalar boundaries, so empty
matches never fall inside a surrogate pair.

## WP 2.1 binding witnesses

`tools/oracle/oracle binding-corpus fixtures/oracle/yaml-binding-corpus.jsonl` generates required Kotlin creator, regex cause-chain and tree materialization cases. Run `tools/oracle/build.sh reference` first. After regenerating `yaml-corpus`, `yaml-review-corpus` or `yaml-round3-corpus`, run `python3 tools/oracle/annotate-yaml-d9.py`; it adds target-specific approved policy assertions without replacing JVM results.

### filter-probes (WP 3.3)

`tools/oracle/filter-probes/run [OUT_DIR]` compiles and runs the real filters
against the reference installDist. Its five JSONL tables cover selection,
ordered composition, metadata rendering, values-file cache/error timing and
Allure 2.29.0 environment/JSON/supplier behavior. See
`filter-probes/README.md` for the isolated-child environment contract and pins.

### Connection/transfer probes (WP 4.3)

`tools/oracle/connection-probes/run` compiles the actual frozen factory,
SharedTransfers, WriteFile and rsync wrapper sources with Kotlin 1.9.24 against
read-only reference installDist jars (`MARATHON_KOTLIN_LIB` overrides their path).
Modes `argv <scratch>`, `semantics <scratch>`, `gates <scratch>`,
`deadlines <scratch>`, `timeout` and `reachability` record the live methods.
For gates/deadlines set `MRS43_GATE_ROOT=<scratch>` and
`RSYNC4J_RSYNC=$PWD/tools/oracle/connection-probes/gated-rsync`.
Each scratch must be fresh. `Reachability.java [timeout-ms]` is the standalone
JDK-only equivalent used on the Linux build box. The captured tables, raw streams,
normalization and the incomplete U4 password evidence are documented under
`fixtures/oracle/connections/README.md`; no remote Mac is used.

### config-differential (WP 2.6)

`tools/oracle/config-differential live --run-dir /tmp/mrs26-live-unique` builds the pinned reference and a fresh Rust
binary, records one isolated JVM `config-dump` per case and compares fresh Rust runs. Offline replay is
`cargo test -p marathon-cli --test config_differential`. See
[`fixtures/oracle/config-differential/README.md`](../../fixtures/oracle/config-differential/README.md) for the
4,299-case key/operator ledger, raw evidence, explicit D6/D9 alternatives, Linux replacements and generation commands.

### Test root factory (WP 4.5b)

`python3 tools/oracle/test-root-probes/run.py` runs the unchanged factory against
recording commands/transfers and real or synthetic bundles: 179 generation cases
plus three writer/surrogate cases and one temp-prefix rejection. See
[test-root-probes/README.md](test-root-probes/README.md) for the complete vehicle,
input descriptions and typed host-path normalization.

## Release source c95b2128 (WP 7.5)

The baseline remains the default. Set
`ORACLE_SOURCE_SHA=c95b212882c7ba7b1ad91b7981916ed175f1530e` and
`MARATHON_KT` to a disposable release checkout for `build.sh`, `oracle` (and its
shell shorthands), or `validate-patches.sh`. Only these two full immutable SHAs
are accepted. Release reference/patched builds have their own
`build-<full SHA>/marathon-{src,patched}` directories and scratch indexes. Each
build still verifies the exact pinned tree plus its declared patches. No
baseline build, fixture or runner default is repointed.

`coverage-probes/run.py` executes all 17 original release coverage-target tests
against that distribution. It recompiles only the two pinned coverage sources
and the unchanged test file, first unmodified, then with a return/error recorder.
Both execute every original assertion; file and Mach-O snapshots must match,
and the recorded variant must repeat byte-for-byte. The metadata hashes the
source files and the observation overlay. `coverage-probes/installer.py` reuses
the existing recording-device installer probe against the release distribution,
with nine enabled/disabled/hosted/missing/empty cases and an exact repeat. It
compiles no replacement production implementation. These probes require only
cached dependencies and the reference JDK; they never invoke Xcode or devices.

Release observations live in `fixtures/oracle/coverage-c95b2128/`. Existing
baseline observations retain their original source attribution. Other owner
probe scripts with their own build paths or metadata remain baseline tools;
setting the release variable does not relabel their evidence. Fresh complete
release certification is still an explicit WP 7.5 closure requirement.
