# Simulator recovery witnesses (WP 5.5b)

Run `python3 tools/oracle/simulator-recovery/run.py [output.jsonl]`.
The runner verifies frozen `e47352f2`, clones it without hard links into a
throwaway `/tmp` checkout, and compiles the actual provider, simulator device,
provisioner, planner, connection factory, artifact processor, configuration and
three original test classes. It never writes into the reference checkout.

The reference JDK is Android Studio JBR 21.0.10. Kotlin 1.9.24, coroutines 1.8.1,
Mockito 5.12.0 / Mockito Kotlin 5.4.0 and the JUnit/Kluent/assertion jars are read
from the existing reference installation and Gradle cache. No remote workers,
real credentials, Xcode commands or real simulators are used.

`probes.jsonl` has 43 independently generated JVM rows:

- All 37 original methods execute unchanged, including both StartupPhases methods
  with real setup/prepare. Every original assertion runs; BeforeEach/AfterEach
  methods and TempDir fields are honored.
- A90 and its connected-loss control run the actual provider **and actual
  ConnectionFactory**, with injected fake executors and a mock SimulatorFactory.
  Reflection invokes the actual reconnect scan under a structured coroutine
  scope and inspects real lease/user counts after provider termination. Worker
  loss leaves one old lease/user; connected loss leaves zero. The witness then
  releases its intentionally leaked old reference.
- A YAML-bound iOS configuration omitting workerConnection records defaults and
  the source `pauseAfter` offsets 0/10/30/90/150 seconds.
- Actual Detached uses explicit dispatcher/acquisition barriers to record ready/cancel
  and blocked-cancel release exactly once.
- Actual PendingWorker records duplicate retention, draining and closing without
  erasing already stored targets.

Original-method evidence includes per-serial allocation-labelled events,
connection acquire/release method counts, available command argv/counts, and
provider logs/errors. Events are grouped by serial, preserving each serial's
order; no total ordering across workers is claimed. Logs omit live replacement
object identities, dynamic-source endpoints and the connection-success elapsed
line. The original tests run live shortened retries; incidental retry counts and
remaining live log timing are observations, not deterministic whole-run goldens.
Rust tests compare the relevant policy/state/release witnesses and retain the
source assertions; this corpus is not WP 7.1 certification.

The generator also re-executes the existing 3.8 pool/Scheduler probes in separate
scratch staging, using their unchanged observational hooks and test-dispatcher
adaptations. All 31 original test rows and 19 Scheduler scenarios must compare
byte-identically with `fixtures/oracle/pool/`; `pool.json` records their paths,
row counts and SHA-256 digests instead of duplicating those large fixtures.
`--pool-only` reruns only this comparison.

`rust-cli-smoke.json` is **native observation**, never a JVM expected answer.
Regenerate it with:

```sh
MRS55B_SMOKE_EVIDENCE="$PWD/fixtures/oracle/simulator-recovery/rust-cli-smoke.json" \
  cargo test -p marathon-cli --test ios_production
```

The built `marathon` binary runs against the real loopback SSH fake world with
fresh disposable Ed25519 keys, real rsync and synthetic valid plist/Mach-O bundle
shapes. It checks root/run and parse argv/exit/output, default nm, production
DeviceLogListener commands, shutdown, zero unmatched routes, and iOS aliases.
Successful parse performs discovery, writes YAML and skips provider shutdown
(C26); it intentionally creates no batch session. Tests do not regenerate any
committed evidence unless the explicit environment variable above is supplied.
