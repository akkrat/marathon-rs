# Provisioning witnesses (WP 5.4)

Run `python3 tools/oracle/provisioning/run.py [output.jsonl]` from this worktree.
Defaults: frozen `e47352f2` at `MARATHON_KT`, Android Studio JBR 21.0.10 at
`JAVA_HOME`, and the installed reference jars at `MARATHON_KOTLIN_LIB`.
Kotlin 1.9.24 and focused Mockito/JUnit/assertion jars come from the existing
read-only Gradle cache. No network, Gradle invocation, real worker or credentials.
All compilation and scratch report directories are isolated under `/tmp` and
removed after the process completes. The reference checkout is never written.

The runner compiles all five frozen provisioning sources and the three original
tracker/factory/provisioner test classes. It invokes every original method,
including all 1/2/4 timing invocations, with their assertions unchanged. The
context-only matrix row is exercised by the direct overlapping-refresh probe.
The tests' required YAML resources are read from the original test resources;
Rust uses the already copied, unchanged fixtures and the production P2 binder.

Because installed jars predate these APIs, the compilation also includes the
frozen AppleSimulatorDevice, ConnectionFailures, VendorConfiguration,
TimeoutConfiguration and WorkerConnectionConfiguration sources. They supply
matching signatures/internal visibility, not alternate provisioning logic.
Factory construction is intercepted by Mockito, as in the original tests.

Additional probes exercise nanosecond TTL boundaries, wrapping subtraction,
request-start aging, executor identity/disconnect/throwing getters,
non-shareable/partial/complete facts, separate cycles, overlapping refreshes
and later failed requests, first-transport/high-water retention, original versus
suppressed disposal failure, and the actual TimingLog/SystemTimer read order.
The cleanup trace uses a mocked factory and commands, retaining the real
provisioner/context and explicit registration/acquire/release/delete events.
Counts and registration IDs in the original-test rows come from their mocks.
These are vendor-local witnesses; no row claims concrete provider/device setup.

Only JSON objects emitted by `emit` are retained; logback output is excluded.
`PROVISIONING_RAW_LOG=/tmp/path` optionally retains the raw stdout for diagnosis.
The runner fails on assertion/compile/JVM errors rather than emitting a passed row.

`S3-approved-divergence:*` rows deliberately record Kotlin's alias/cast-failure
outcomes. Rust must reject those malformed namespaces, per the final brief.
Other cache/tracker values and bounded setup counts are compared directly by
`crates/marathon-ios/tests/provisioning_oracle.rs`.

Review round 1 adds a field-only `FrozenPlanner` shell generated under `/tmp`.
Its `plan`, `reuseExistingSimulator`, `toAppleId` and exhausted getter are sliced
verbatim from the frozen provider source; the shell supplies fields only and
starts no provider threads. Three JVM rows compare reuse order, Host/simulator
log order and start/end events around suspended serial shutdowns with Rust.
This is shared-method evidence, not full provider lifecycle integration. Both
cleanup trace probes use one permit to remove scheduling between independent
profiles from the order comparison. All supplemental oracle rows are checked
against actual Rust results, including the factory timing/suppression rows in
`tests/simulator_factory.rs` and planner/timing rows in `tests/planning.rs`.
The S3 rows additionally assert that Rust rejects the corresponding key collisions
without publishing a partial tracker mutation. The output now contains 46 rows.
