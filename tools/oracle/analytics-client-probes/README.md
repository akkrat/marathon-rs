# Analytics client probes

Run `tools/oracle/build.sh reference`, then `tools/oracle/analytics-client-probes/run`.
`MARATHON_ORACLE_LIB` can point to an existing read-only reference installDist
`lib` directory; all compiler output stays in a temporary directory. The source
checkout remains untouched. The runner uses Android Studio JBR and the cached
Kotlin 1.9.24 compiler, and records actual pinned client requests against local
HTTP and TCP peers.

The committed recordings and their SHA-256 provenance are documented in
`fixtures/PROVENANCE.md`. See `docs/wp/3.4b-findings.md` for the lifecycle defaults,
coverage, approved P1–P4 boundaries and remaining release qualification.

`qualify-services` is the explicit Phase 7 command. It requires a **local**
Docker/colima runtime, starts disposable pinned Influx v1/v2 and Graphite
containers with dummy credentials, runs the transcript comparator, then runs the
same nine shared history cases against each real service and both recreation
cases. It records image IDs, polls ingestion readiness, and removes only its own
containers on exit. This command has not been run for WP 3.4b. Its real-service
mode shifts the fixed-anchor event spacing to the current second so normal
retention policies accept the data; default loopback replay stays fixed at
2026-01-01T00:00:00Z. It compares real aggregation results to the original Kotlin
assertion bounds; byte-level wire comparison uses the committed JVM exchanges.
