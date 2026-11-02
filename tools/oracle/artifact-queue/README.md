# Artifact queue oracle (WP 4.9)

Run from the Rust checkout, with the reference Gradle cache populated:

```sh
python3 tools/oracle/artifact-queue/run.py /tmp/mrs49-kotlin fixtures/oracle/artifact-queue
```

The first argument is a disposable clone, never the frozen source checkout. The
script checks out `e47352f2`, installs `Capture.kt` as a test-only observer, and
adds observational transition snapshots to the copied `ArtifactProcessor`.
No dependency, scheduling, timeout, admission or delivery algorithm is changed.
The four frozen suites run with Gradle 8.10.1, Kotlin 1.9.24,
kotlinx-coroutines 1.8.1 and Android Studio JBR 21.0.10, offline. The test task
always reruns; other Gradle tasks retain their ordinary incrementality.
`gradle-capture.log` and the raw event/transition streams remain in scratch.

The capture extension records level, logger, formatted message and throwable
class/message. The probe adds closed-callback, Files.move, source reinsertion,
and the distinction between injected queue time and real measureTimeMillis.
`transitions.jsonl` normalizes object identities to observation-order integers;
queue records declare the limits and scenario inherited by later events.
Submit records declare each work item's owner/batch/worker, reused by its ID.
Complete and stats records include the insertion-ordered pending-owner map.
`events.jsonl` normalizes only JUnit temp roots. Original cancellation identity
suffixes remain as witnesses, with a `rustMessage` field for the D12 projection.
The two text files are exact report tables extracted from JUnit's stdout;
`reports.jsonl` parses their numeric columns without supplying new expectations.

The Rust admission-core replay consumes every transition and compares admission
identity/order, timestamps, depths, blocked reason, worker starts, pending-owner
order and all six final statistics. Async tests compare the deterministic
admission/summary log lines and the closed callback witness. Listener tests pin
both Files.move failures and no replacement, kept sources and command argv.
The independent event simulator compares all 34 report rows exactly where its
scheduler has no coincident timers; approved P2 permits differences with timer
ties, listed in `docs/wp/4.9-findings.md`. It also enforces all Kotlin sizing
thresholds and occupancy/capacity invariants.

There are no copied resource fixtures in these Kotlin suites; their filesystem
inputs are created under temporary directories. No copied fixture is edited.
