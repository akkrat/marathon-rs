# Concrete reporter oracle (WP 3.9b)

Run in the assigned worktree:

```sh
python3 tools/oracle/reporter-probes/run \
  /tmp/mrs-rev-2.3a-r2-sol/oracle/build/marathon-src/oracle-build/install/oracle/lib \
  /tmp/mrs-reporters-fresh
```

The output directory must be fresh. Compilation and execution use a disposable
`/tmp` directory; the reference jars and source checkout are read-only. The
reference source HEAD is `e47352f25cbc50333900d5d040a5544ff1224501` and its
`git diff HEAD -- core/src/main` is empty. Runtime jar hashes are recorded.
Versions: Android Studio JBR 21.0.10, Kotlin compiler 1.9.24, Gson 2.11.0,
Jackson 2.17.2, SnakeYAML 2.2, dex-test-parser 2.3.4, logback 1.5.8.

The probe invokes the actual six Kotlin reporters and MarathonTestParseCommand.
`INVISIBLE_MEMBER` suppression permits test access to internal reporters; no
production method is replaced or recompiled. A no-op UsageTracker implements
D1. Its unused generate-time clock calculation still runs in the unpatched JVM,
but has no effect on any captured file/warning/stdout. Rust intentionally does
not consume `core.BillingReporter.generate` (the catalogue entry is retained).

A FixedTimer returns the declared elapsed value. Reflection sets only the
already-constructed BillingReporter.defaultStart to Instant.ofEpochMilli(1000).
This is the existing `core.BillingReporter.defaultStart` seam; all billing
collection, selection, duration arithmetic, serialization and I/O remain the
original code. No new oracle source patch needs disabled-hook validation.
Explicit preparation-finish scenarios are independent of this fallback field.
The Rust tape reads defaultStart once, and its SystemTimer reads
`core.di.Clock` at construction/elapsed time; empty/failed reports omit the
elapsed read. Those adaptations are explicit in reporter_support::generate.

The committed evidence is newly generated, never copied or hand-edited:

- `probes.jsonl`: 20 six-reporter scenarios with source-produced input
  descriptors, original complete file bytes and directory manifests, stdout
  bytes, billing warning messages and the fail-json exception. Two additional
  rows hold five actual parse-command YAML files and nonfinite raw JSON.
- `binary.jsonl`: 20 mapper/Gson outputs for byte arrays of lengths
  0/1/2/3/55/56/57/58/114/120 in maps/lists. This covers tagged binary empty
  scalars, base64 wrapping and the final-newline/chomping distinction.
- `junit-errors.jsonl`: createNewFile IOException versus FileOutputStream
  FileNotFoundException, including ignored mkdirs failure.
- `identities.json`: actual identityHashCode suffixes for aliased/equal-distinct
  DEX arrays, the DecodedNull singleton, byte[] and char[]. Rust supplies those
  values through `core.RawJsonReporter.identities`, keyed by allocation
  identity. Literal `[I@123abc` remains literal. Nothing rewrites identity-like
  strings in output.
- `context.json`: typed attachment-path sandbox cwd and fixed defaultStart.
- `errors.json`: Gson's actual nonfinite exception; `jars.sha256`: dependencies.

FileManager and billing logging use non-additive logback loggers so raw
`System.out` captures only StdoutReporter. Billing ListAppender messages are
recorded independently. FileManager's unchanged diagnostics/path rules remain
covered by the merged 3.9a/1.7 evidence. No stdout is trimmed. Original D3 lazy
fields remain in JVM evidence; Rust comparison removes only the exact
`safeSerialNumber$delegate` object field, without parsing/reprinting JSON.
Attachment paths retain their original typed sandbox spelling through cwd
injection. All other file bytes are compared directly, including XML CR/LF/tab,
non-ASCII/supplementary Unicode, split CDATA, empty trace CDATA, and final blank
stdout lines. JSON/XML files retain their absence of a terminal LF.

The six assigned parity ports additionally compare the unchanged historical XML
fixtures structurally and validate against the unchanged junit-10.xsd. Offline
validation uses `xmllint --nonet --noout --schema`; structural comparison uses
`--nonet --noblanks --c14n`. Test hosts need libxml2's `xmllint` (macOS includes
it; Linux package `libxml2-utils`). No network or JDK is needed for Rust tests.

Review round 1 adds three scenarios (`rv-retried`, `rv-unicode`, `rv-no-tests`).
The runner includes them in its fresh `probes.jsonl`; their newly generated rows
are committed separately in `fixtures/oracle/reporters/review-round1.jsonl` to
preserve the original evidence. Filter fresh rows by these exact IDs to
regenerate that file. Inputs originated in review; expected values were generated
afresh with this runner and unchanged reference jar hashes. No identity or cwd
normalization applies to these three scenarios.
