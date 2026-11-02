# Fork notes

marathon-rs is a native Rust rewrite of the Kotlin marathon fork, pinned to
`e47352f2` (`e47352f25cbc50333900d5d040a5544ff1224501`). The compatibility
reference is that snapshot, not today's upstream marathon. Later fork commits
belong to the port queue, which must be drained and compared again on the
release source SHA before release.

The compatibility goal in [PLAN §3](../PLAN.md#3-compatibility-contract) covers
Marathonfile configuration, Marathondevices files and dynamic updates, command
line options and exit codes, artifact paths and contents, logs, scheduling,
and the commands and timeouts sent to workers. Native implementation alone
does not establish that these contracts have passed their release gates.
See the [README's dated status](../README.md) for currently wired vendors.

Known Kotlin bugs are deliberately retained unless an approved exception says
otherwise. The [bug ledger](known-kotlin-bugs.md) distinguishes confirmed ports
from seeded source evidence; its entries are a future fix backlog, not a promise
that migration fixes them. [Deviations](deviations.md) explains each approved
PLAN §3.11 exception. PLAN §3.10 separately permits library parse-error wording,
thread names and internal stack-trace differences. WP-local approved
simplifications remain in their owning findings until integrated into the plan.

The native distribution replaces the JVM launcher and JAR payload with one
executable. [Archive and release operations](releasing.md) cover checksums,
targets and tag-only publishing. A successful extraction, help or version check
proves packaging only. Whole-run differential checks, real Android/iOS/macOS
runs, port-queue reconciliation and isolated shadow/canary cutover are separate
[release gates](../PLAN.md#10-risks-and-release-gates).

This project remains derived from marathon under the existing
[GNU GPL version 2](../LICENSE). Preserve source attribution, copyright notices
and applicable dependency notices when redistributing. No new project branding
or replacement upstream documentation is implied. Use the
[migration guide](migration.md) for the executable switch and rollback procedure.
