# Simulator provider witnesses (WP 5.5a)

Run `python3 tools/oracle/simulator-provider/run.py [output.jsonl]`.
Defaults are frozen `e47352f2` at `MARATHON_KT`, installed reference dependency
jars at `MARATHON_KOTLIN_LIB`, and Android Studio JBR 21.0.10 at `JAVA_HOME`.
Kotlin 1.9.24, coroutines 1.8.1, Mockito 5.12.0 / Mockito Kotlin 5.4.0 and
JUnit/assertion jars come from the existing read-only Gradle cache.

The runner clones the frozen source into a disposable `/tmp` checkout and
compiles the actual provider, device, provisioning/context/tracker, connection
factory, artifact processor and matching configuration sources there. The five
original test classes are compiled unchanged; the entrypoint invokes all 20
executable invocations assigned to 5.5a, running BeforeEach/AfterEach and supplying
isolated TempDir fields. The commented legacy test is covered by Rust's
`should_close_the_channel`; there is no executable Kotlin method to invoke.
No source checkout, simulator, real worker or credential is modified or used.

The supplemental entrypoint drives the actual provider with Kotlin fake workers,
Mockito device faults and loopback TCP. It records A80–A82, replaced-reference
acquire/release order, whole-worker removal/repeat/profile retention, static file
precedence and binding stages, substitution cycles, split/coalesced frames,
negative/oversized/zero frames, every EOF stage, malformed mapping, source
rendezvous backpressure, permanent producer stop with monitoring alive, refused
connect then success, live-plan exclusions and registration/removal races.
Borrow's 200 ms boundary uses the Kotlin test scheduler. Socket tests have
bounded watchdogs; partial EOF probes flush and allow 100 ms for the receiver to
consume the partial segment before closing, separating EOF from buffered-read
scheduling. Refused-connect evidence uses Kotlin's actual five-second delay.
Rust additionally verifies 5000/16/200 ms with injected/paused clocks and socket
closure/cancellation, live registry rereads, and a shared WorldExecutor scenario.

Original update rows include per-serial events and allocation identities,
reference method counts and command argv/counts. Sorting groups independent
simulators; event order within each serial is retained. Command counts do not
claim a total order across supervised setups. The dedicated replaced-leases
probe records the source's reference order separately. Raw logs are available
with `PROVIDER_RAW_LOG=/tmp/provider.log`; timestamps, default object identity
strings and unrelated logback output are excluded from committed evidence.
Failure class/message/cause records are retained, including binder diagnostic
differences documented as U83 in the WP findings.

These are component/provider witnesses, not WP 7.1 whole-run manifests or WP 7.2
real-simulator results. All original assertions run unchanged; supplemental
cases and their Rust consumers are listed in `docs/wp/5.5a-findings.md`.
