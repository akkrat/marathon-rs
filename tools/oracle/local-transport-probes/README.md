# Local transport edge probes (WP 4.1)

Run `tools/oracle/local-transport-probes/run.sh` with the reference JDK and
Kotlin 1.9.24 stdlib in the Gradle cache. Override `JAVA_HOME` or
`KOTLIN_STDLIB` when needed. The scratch directory is under this checkout's
`target/` and is removed after the probe.

`run.sh --kotlin` additionally runs the actual Kotlin local executor from the
read-only source checkout's already-built CLI distribution. `MARATHON_KT`
overrides its path. This does not build or modify the source checkout.

`LocalTransportProbe.java` calls Kotlin `removeSurrounding`, `trimMargin` and
`copyRecursively` (including its default error handler), plus JDK duration,
UTF-8 decoding and process signals. `KotlinCommandProbe.java` checks the
retained total-timeout leak and idle-timeout cancellation wrapper. Outcomes
are recorded in `docs/wp/4.1-findings.md`; these tools are not CI fixtures.
