# WP 3.9a JVM probes

Generate new evidence with:

```
python3 tools/oracle/event-pipeline/run <reference-installDist-lib> <fresh-output-directory>
```

Use the unpatched reference distribution from `tools/oracle/build.sh reference`
(commit `e47352f25cbc50333900d5d040a5544ff1224501`). The generator compiles
`EventPipelineProbes.kt` in a disposable `/tmp` directory, using the cached Kotlin
1.9.24 compiler and Android Studio JBR 21.0.10. It reads reference jars without
modifying them. `jars.sha256` records the exact runtime classpath, including
Kotlin 1.9.24, coroutines 1.8.1, Gson 2.11.0 and logback 1.5.8.

The probes instantiate the actual reference report, generator, Track, internal
delegates, shared LogListener/LogWriter and attachment classes. Transfer probes
call the real inline extensions with a Device proxy supplying only its serial
and existing KLogger; logback's ListAppender captures debug messages. No
production class or clock expression is rewritten. Only the transfer probes
sleep (20 ms per operation). Their measured seconds and folder transfer speed
are intentionally live evidence, not replay goldens. The root/subdirectory/file
counts, sub-KB integer division, operation values, exception classes and number
of log messages are stable. Rust transfer tests supply labelled clock tapes and
check these same invariants with fixed times. Directory lengths are host values;
empty checks compare the File.exists/length predicate rather than assuming every
directory has a fixed length. Temporary file names in exception messages are
replaced only at the supplied sandbox prefix.

Inputs are explicit in the probe source. `probes.jsonl` covers stable sort and
equal timestamps, first-connection pool/device discovery, test-only pools,
conflicting finals, INCOMPLETE and assumption failure, zero/negative times,
subtraction and sum overflow, same-object/equal-distinct result identity and
structural map replacement, duplicate/throwing/snapshot fan-out, repeated and
failing close, attachment duplicates and metadata-equal Test keys, log preamble,
blank lines, tail partitions, reset-before-save-error, malformed UTF-16 bytes,
stat outcomes, and file/folder transfer success and failure. It contains JVM
outputs only; the Rust implementation does not produce expected values.

The implementation run used the existing reference distribution at
`/tmp/mrs-rev-2.3a-r2-sol/oracle/build/marathon-src/oracle-build/install/oracle/lib`.
Its source HEAD was checked against the pinned commit; `git diff HEAD --
core/src/main` was empty. All generated class files and scratch artifacts stayed
in `/tmp`. No copied Kotlin fixture is required by the five parity tests.
