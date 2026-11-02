# Configuration differential corpus (WP 2.6)

4,299 cases: the 30 existing config-dump goldens, 87 validation witnesses, 18 fully populated schema seeds,
4,070 deterministic mutations (407 key/variant rows × 10 operators), and 94 focused interactions. There are no
production Marathonfiles. The three existing Linux replacements and one new Linux replacement are expectations
for the same cases, not additional cases. No device, SDK installation, Xcode or vendor provider is used.

## Replay and live comparison

From the repository root, with the Homebrew rustup path set:

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cargo test -p marathon-cli --test config_differential -- --nocapture
```

Cargo selects the production executable through `CARGO_BIN_EXE_marathon`; there is no search for an installed or
previously built executable. The Rust test invokes Python 3's standard library driver with a fresh run directory.
All committed JVM outcomes, including the other host's variants, are validated before replay. The test runs the
production `marathon config-dump` for every case. `scripts/check.sh` includes it through `cargo test --workspace`.
The second Rust test compares public `config_dump::document` with `canonical_configuration` on the same production
loader result. The existing 87-row `toMap()`/serialization suite and bundle safety profiles remain separate gates.

For a fresh JVM/Rust comparison:

```sh
tools/oracle/config-differential live --run-dir /tmp/mrs26-live-unique
```

`live` rebuilds the reference with `tools/oracle/build.sh reference`, builds Rust with
`cargo build --locked -p marathon-cli --bin marathon --message-format=json`, and uses the executable returned by
Cargo's artifact message. An optional `--binary` must name that exact executable. Build logs and the executable's
SHA-256 remain in the run directory. Builds have a five-minute limit. Each config invocation has a 30-second JVM
or 20-second Rust limit; process groups are killed and joined on timeout. Six JVMs or four Rust processes run at
once. JVM recording and Rust replay use fresh sibling sandboxes from the same setup recipe, with distinct empty
cwd/HOME/TMPDIR directories and an explicit env file (including empty environments). Every invocation uses
`--now 1970-01-12T13:46:40Z`.

`live` updates committed oracle evidence. It retains every raw output and `tree.json` final manifest in the run directory;
offline replay also writes each selected JVM document to `oracle.json` beside the Rust sandbox. Failures
also retain `failures.txt` with case IDs and the first differing JSON path/value (cause index or filesystem entry
included). Exit status, stderr, JSON schema, complete case inventory and checksums are checked. Configuration
rejection is a valid dump outcome with command exit 0. Unexpected command stderr always fails.

Other modes:

```sh
tools/oracle/config-differential record --run-dir /tmp/mrs26-record-unique
tools/oracle/config-differential replay --binary /absolute/fresh/marathon --run-dir /tmp/mrs26-replay-unique
tools/oracle/config-differential self-test
```

`record` rebuilds only the reference and records JVM outcomes; repeated unchanged inputs must produce equivalent
normalized outcomes and final trees. `--select TEXT` selects case IDs containing TEXT, and fails when none match. Direct `replay`
requires an explicitly selected freshly built binary; use the Cargo test or `live` for enforced build selection.
Run directories must be absent or empty. Only disposable copies of archives are opened; fixture resources are unchanged.
The archive generator rejects absolute or escaping entry names and symlink targets. Existing adversarial bundle
profiles remain in `bundle_probe_matches_jvm`, with the F21/F24–F28 alternatives approved in WP 2.3b.

## Files and generation

- `seeds.json`: generation-only conversion of the 16 serialized JVM validation rows to JSON maps, by
  `ConfigDifferentialSeeds.kt`. The generator supplements optional keys and two variant seeds. Emitted built-in nm
  lookahead defaults are omitted from ordinary seeds; rebinding uses the approved built-in default. User lookahead
  rejection has its own D6 witness.
- `cases.jsonl.gz`: immutable input/setup recipes, environment, base, field/operator and vendor for every stable ID.
  Gzip is deterministic (`mtime=0`); its decompressed bytes have the manifest's `inputsSha256`.
- `manifest.json`: every key/operator witness, source binding-key name audit, case inventory, measured counts and
  references to the unchanged neighbouring corpora. Map-value names are representative dynamic keys, not an attempt
  to enumerate user-supplied strings. All ten operators have a concrete witness for every enumerated row.
- `outcomes.jsonl.gz`: 4,303 outcome records (4,299 primary plus four Linux variants). Each retains **raw JVM JSON**,
  normalized JSON with original number spellings, SHA-256, input identity, final filesystem manifest, host, env, cwd,
  now and a build-identity reference. Gzip keeps this evidence compact; it is not an expected-failure filter.
- `alternatives.json`: 12 explicit D6/D9 alternatives plus the approved P2 `[null, {}]` type-id document. Raw JVM outcomes remain untouched. Filter-list alternatives
  assert the JVM value before replacing that one field; reject/accept changes carry full explicit Rust documents.
- `build-identity*.json`: reference source SHA, JDK version, every distribution JAR hash and oracle source hashes.

To regenerate inputs after an intentional generator change:

```sh
tools/oracle/build.sh reference
tools/oracle/oracle config-differential-seeds "$PWD" "$PWD/fixtures/oracle/config-differential/seeds.json"
python3 -I tools/oracle/config-differential-lib/generate.py
tools/oracle/config-differential live --run-dir /tmp/mrs26-new-inputs-unique
```

The input generator has no third-party Python dependencies and does not parse production YAML. Its small YAML
emitter creates inputs only; production parsing/rendering remain in the loader and ConfigDump.kt. No source or
parity matrix ownership changes are needed (WP 2.6 owns 0 source files and 0 Kotlin tests).

## Comparison rules

Object key order, number spellings, null/absent distinctions, ordered arrays and every effect field compare exactly.
Only Android fileSync pull/push, Apple permission sets and iOS lifecycle sets compare by membership and count.
Sandbox spellings and the macOS data-volume alias normalize only typed path fields and error diagnostics; opaque
user maps and names are preserved. Sandbox aliases (`/tmp` ↔ `/private/tmp`, `/var` ↔ `/private/var`,
and `/System/Volumes/Data`) are expanded lexically so archived evidence validates independently of the replay host's
filesystem or whether the recording directory still exists. Initial files containing the declared `@ROOT@` placeholder use the template's
normalized bytes for final-tree size/hash, only if their bytes still equal the exact expanded input. Output/extracted
file hashes are never normalized. Absolute paths retain their noncanonical components.

PLAN §3.10 permits generated Jackson/SnakeYAML diagnostic wording. The comparator retains class, cause count and
order and every custom message. It does not discard an error or a document. F29 normalization is restricted to typed
strategy `toMap()` fields; it never touches canonical configuration or user strings. The carried `toMap()` corpus
remains authoritative for this separate view.

The self-test corrupts stage, custom message, cause count/class, canonical fields, ordered list order, set membership,
number spelling, effect hash, case evidence and raw evidence. Each must fail. Permitted set reorderings and typed
F29 suffix changes pass. Missing/truncated gzip, missing outcomes, duplicate JSON keys, malformed documents,
nonzero exits, timeouts and unexpected stderr fail the replay.

## Linux evidence

`validation-a-devices-symlink-loop`, `validation-a-known-hosts-symlink-loop` and
`validation-a-bundle-before-devices` retain the existing three Linux replacements. `effects-parse` adds the fourth:
Linux rejects the relative devices path during binding, before extraction; macOS resolves the loop spelling and
extracts. Both raw outcomes and final trees are retained. There is no host skip.

The orchestrator's Linux gate replays the same 4,299 cases, swapping these four expectations. To refresh JVM variants,
copy this corpus, the helper scripts, unchanged setup fixtures and the exact reference oracle distribution into a fresh
Linux scratch directory. Set `JAVA_HOME` to the Linux JDK, then run `record --prepared-oracle --select ...` for those
IDs. `--prepared-oracle` verifies every copied JAR against `build-identity.json`; it does not rebuild the source on the
box. Copy the four `.linux` records and `build-identity-linux.json` back, repack, and remove the scratch directory.
The recorded Linux JDK is OpenJDK 21.0.12.1; primary evidence uses Android Studio JBR 21.0.10.

Review round 1 regenerated all four Linux variants using `COPYFILE_DISABLE=1 tar -czf` and verified that the
extracted fixtures contain no `._*` entries. Both JVM recordings were stable (4/4). Seven new `r1-wrapper-*`
JVM witnesses were also recorded twice (7/7 stable); select them with `--select=r1-wrapper`.
`approved-p2.json` separately records the approved `[null, {}]` type-id simplification, including both raw
JVM/Rust documents. It does not change the 12 D6/D9 alternatives or the comparator.

Review round 2 adds 36 focused witnesses, recorded twice (36/36 stable): `r2-regex-*` (regex scalar kinds),
`r2-reader-control*` and `r2-code-point-*` (JacksonYAMLParseException keeps the ReaderException or YAMLException
cause; both SnakeYAML classes join the generated-wording list, their class and position stay compared),
`r2-creator-*` (unknown properties buffered by Jackson's property-based creator) and `r1-p2-null-type-id`, which replays
`approved-p2.json`'s Rust document as the 13th explicit alternative. Select them with `--select=r2-` and
`--select=r1-p2`.
