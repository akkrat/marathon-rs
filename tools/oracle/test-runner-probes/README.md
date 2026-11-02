# Apple test runner and remote parser probes (WP 4.8)

Run `python3 tools/oracle/test-runner-probes/run.py` with the frozen Kotlin checkout,
its installed CLI jars, Gradle dependency cache and the reference JDK. The script
uses the installer probe's compiler setup and compiles the five main files unchanged.
It also compiles the unchanged dependencies whose installed jars predate e47352f2.
Compilation and synthetic bundles stay under a temporary `/tmp` directory. The
reference checkout is never changed.

The first 21 JSONL rows execute the unchanged 10 AppleDeviceTestRunnerTest,
2 XCTestParserTest, 5 SkipCopyingDerivedDataTest parsing and 4 StartupPhasesTest
symbol-scan methods. A fresh test instance and its original cleanup run each time.
The subsequent 16 rows record real runner/parser/session results, error class,
message, cause and suppressed chains, and level/logger/message/throwable text.
The unit-level seams are recording AppleDevice proxies and actual CommandSessions;
XCTest setup uses the mocked installer, as the upstream tests do.

`nm-rescan` runs the real Nm and NmTestParser against the unmodified Kotlin
FakeSimulatorWorker. It holds one scan until the second gets an OpenFailException,
then verifies the third scan runs after release. `stderr` runs the real
AppleSimulatorDevice.executeTestRequest against that worker's session override
with debug off, blank lines and a padded warning. These two scenario rows are
ready for WP 7.1's full Rust replay. No real simulator, worker or host is touched.

Normalization: scratch bundle roots become `<bundle>` in log messages; close
elapsed time becomes `<ms>`; JVM stack frames are omitted (PLAN §3.10), retaining
throwable headers, suppressed entries and causes. Throwable JSON omits null fields,
as Gson does. Independent producer-close and event-reader callbacks can interleave:
compare their enforced partial order, not incidental ordering of unrelated calls.
The runner-failed-after, runner-close-failure and runner-close-timeout probes
latch producer close until the failure and after-run callbacks are recorded.
Eight consecutive review-round-2 regenerations are byte-identical. The timeout
probe then waits for release after the runner returns. Producer cancellation
completes its close before the reader receives cancellation, so runner-cancel
must not wait for the after-run callback; its release wait expires before that
callback runs. The original tests separately assert slow-close and failure order.
The runner-cancel detail case induces cancellation inside the producer; the
original cancellation test additionally cancels the caller during a slow close.

The symbol corpus covers Unicode trim, DISABLE, a trailing `(`, four Swift parts,
three ObjC tokens, duplicate tests and the empty ObjC package. The explicit `.*`
class filter allows this raw symbol corpus to reach the result; Rust separately
checks registration before rejecting a class. Missing platform retains Kotlin's
NPE in this evidence; approved S3 gives Rust a named TestParsingException instead.

`python3 tools/oracle/test-runner-probes/mark-parity <test-commit>` regenerates
21 ported rows through the repository's parity-matrix generator.
