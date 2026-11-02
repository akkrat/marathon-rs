# WP 6.6a Android instrumentation probes

The reference is marathon `e47352f2`, adam 0.5.8, Kotlin 1.9.24,
kotlinx-coroutines 1.8.1, and Android Studio JBR 21.0.10. `run` compiles
only the harness against the existing read-only distribution. The original
classes are unchanged. Runtime HOME and TMPDIR are isolated. The corpus uses
loopback sockets or pure command construction; it never invokes adb.

```sh
python3 tools/oracle/android-runner-probes/run > fixtures/oracle/android-runner-probes.jsonl
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cargo build -p fake-adb -p marathon-cli
python3 tools/oracle/android-runner-probes/remote-scenarios
python3 tools/oracle/android-runner-probes/cli-scenarios
```

`AndroidRunnerProbe.kt` records the ordered argument/command corpus, nullable
false options, parameter lists, duplicate overrides, raw console-port wrapping,
result metadata loss, dropped composite callbacks, ignored callbacks, and
multiple-instrumentation whole-batch requests. Byte stubs also pin the actual
runner's handshake-EOF and closed-client outcomes: the detached producer fails,
but `receiveCatching().getOrNull()` lets the runner return after its after hook.
These are different from a rejection thrown synchronously by listener creation
or shell preparation. The after-hook probe uses a barrier and a socket read
timeout to show that the source leaves the producer connection open, and that
`after` overrides the earlier callback exception. Harness shutdown closes that
connection after observing it; this is not a claim of leak-free Kotlin behavior.

`RemoteProbe.kt` drives the real provider, installer, and remote parser on the
strict fake. It counts retained logcat listeners before bounded harness teardown:
0 on success, 1 after fallback, 3 after three timeouts, and 1 after suspended
receive cancellation. `remote-scenarios` copies its APK into a temporary directory.
The fallback warning includes Kotlin's original indentation. The Rust P2 cleanup
deliberately leaves zero listeners and joins instrumentation on all those exits.

`cli-scenarios` launches the real Kotlin and Rust CLIs against separate fresh
strict fakes. Each run gets isolated HOME/TMPDIR, copied APKs, a fake SDK with an
adb shim that records and rejects invocation, and an allocated non-5037 port.
Remote discovery plus execution, local discovery plus execution, output timeout,
runner crash, and remote-discovery timeout are compared. The last case asserts
nine requests, pinning three inner attempts within each of three outer attempts.
The harness records stdout/stderr and the full transcript (source build identifiers are masked through the optional `MARATHON_REDACTIONS` list); it asserts exact
instrumentation services, final messages/streams via captured files, exit codes,
and semantic raw/per-test result fields. Only clock/duration/batch/serial identity
and DEX array identity suffixes are normalized. Captured stdout/stderr,
including JVM frames and startup D1 differences, remain available for inspection.
This is focused execution evidence, not the full output-manifest gate owned by 7.1.
The corrected shared long-logcat fixture is reused; deterministic attachment
visibility and file bytes are additionally tested through the Android listener
factory, because independent CLI logcat delivery has no global callback order.

The scripts may regenerate generated observations. They never edit copied APKs
or the source-derived adam response fixture. See `fixtures/PROVENANCE.md` and
`fixtures/fake-adb/android-runner-cli/SHA256SUMS` for origins and checksums.

`PROBE_SOURCE=AndroidVersionProbe.kt python3 tools/oracle/android-runner-probes/run`
records `AndroidVersion.equals(int)` at API 29 (true) and API 28 (false), both
compared with 29. The pinned runner calls descriptor `(I)Z`; testing the Object
overload would not reproduce its behavior. Captured rows live in
`fixtures/oracle/android-version-equality.jsonl`.
