# Apple installer probes (WP 4.10b)

`python3 tools/oracle/installer-probes/run.py` compiles the unchanged
`e47352f2` installer, DistributedDerivedData, factory, remote manager,
SharedTransfers, binary wrappers and models against the reference installDist
jars, using Kotlin 1.9.24 and Android Studio JBR 21.0.10. No source checkout is
modified. Compilation and synthetic bundles live in automatically removed
`/tmp` scratch directories. `MARATHON_KT`, `MARATHON_KOTLIN_LIB`, and `JAVA_HOME`
can override the local defaults. No Xcode commands, SSH, or credentials are used.

The recording AppleDevice uses real SharedTransfers and the real factory. Each
JSONL row records the input, ordered worker argv with shell/idle timeouts,
shared sends (relative local source and verbatim remote destination), generated
file lifetime and base64 bytes, combined command/send/hook/install order,
installer logger/level/message, exception class/message/reason, cleanup, and
elapsed coroutine scheduler milliseconds. kotlinx-coroutines-test 1.8.1 runs the
actual Retry.kt delays on its virtual scheduler. Only the typed local bundle
root in exception/log messages becomes `<bundle>`; commands and XML stay exact.
Generated temporary source names are represented by their role, suffix and
existence during transfer, following 4.5b's approved staging policy.

Synthetic plist and eight-byte binary contents reproduce
SkipCopyingDerivedDataTest.bundle exactly; the probe uses the per-case temporary
root as the Products directory. macOS adds its required minimum-version key.
`macos-real` extracts the unchanged `fixtures/bundles/macos.zip` into scratch.
The DerivedData cases call the real configuration validation. Other truth-table
cases explicitly assign the already-resolved fields so raw-application presence
can vary independently; their inputs document that distinction.

`no-result` uses JVM reflection to null the nonnullable xcresult field and
records the otherwise unreachable second guard. Rust retains that guard but
cannot construct this invalid state; the corpus test skips only that row.
Missing vendor/env, absent bundle and absent app run through both implementations.
The no-op hook has its own case; other cases record the hook boundary before
extra app installation. Simulator default permissions have an empty grant loop.

`python3 tools/oracle/installer-probes/run-harness.py` additionally compiles the
**unmodified** SkipCopyingDerivedDataTest and FakeSimulatorWorker sources and
runs all thirteen installation tests through the actual AppleSimulatorDevice
and AppleSimulatorApplicationInstaller from the reference distribution. Each
case calls the original dispose method (including processor closeAndJoin), and
the driver has a 90-second watchdog. `kotlin-tests.jsonl` is that pass record.
This checks the approved narrow Rust vehicle against the actual subclass.

Rust replays the corpus with the extracted 4.5b device, FakeSimulatorWorker,
real SharedTransfers and the production generated-file helper. Local transport
uses the counting bridge. Tests compare complete recorded fields, use bounded
watchdogs, dispose the worker before asserting, and verify staging cleanup.
Retry delay requests are recorded through VendorClock under paused Tokio time
and checked against JVM virtual elapsed time. Five parser parity rows belong
to 4.8; direct TestParsing constructor cases here claim no parser rows.

`python3 tools/oracle/installer-probes/mark-parity <test-commit>` verifies each
Rust test name and regenerates the CSV: 13 ported rows, two commented deleted-
class rows marked n/a per decision 4 (both reviewers agreed). The generator
rules move the five parsing rows to 4.8. No CSV is edited by hand.
