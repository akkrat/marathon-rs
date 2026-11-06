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
See the [README](../README.md) for the current status and
[release readiness](releasing.md#release-readiness) for what the evidence covers.

Most Kotlin bugs are deliberately retained; the approved exceptions fix the ones
they name. The [bug ledger](known-kotlin-bugs.md) distinguishes confirmed ports
from seeded source evidence and marks the fixed rows; the other entries are a
future fix backlog, not a promise that migration fixes them. [Deviations](deviations.md) explains each approved
PLAN §3.11 exception. PLAN §3.10 separately permits library parse-error wording,
thread names and internal stack-trace differences. WP-local approved
simplifications remain in their owning findings until integrated into the plan.

The native distribution replaces the JVM launcher and JAR payload with one
executable. [Archive and release operations](releasing.md) cover checksums,
targets and tag-only publishing. A successful extraction, help or version check
proves packaging only. Whole-run differential checks, real Android/iOS/macOS
runs, port-queue reconciliation and isolated shadow/canary cutover are separate
[release gates](../PLAN.md#10-risks-and-release-gates).

This project remains derived from marathon. Its source is distributed under the
[GNU GPL version 2 only](../LICENSE) (`GPL-2.0-only`). Preserve source attribution,
copyright notices and applicable dependency notices when redistributing; each
archive's `THIRD-PARTY-LICENSES` lists the crates and other files compiled in.
`libxctest-parser` is excluded: this repository and its archives do not contain it,
and this repository's licence does not cover it. It is third-party software under its
own EULA, and an `xctest` run uses a copy that you supply ([D20](deviations.md#d20--the-xctest-parser-library-is-supplied-by-you)). No new project branding
or replacement upstream documentation is implied. Use the
[migration guide](migration.md) for the executable switch and rollback procedure.
