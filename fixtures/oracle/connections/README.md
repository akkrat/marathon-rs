# WP 4.3 JVM and process evidence

Generated from the actual Kotlin methods at `e47352f2` by
`tools/oracle/connection-probes/run`; the run compiles the seven live WP sources
against read-only installed reference jars with Kotlin 1.9.24. No wrapper method
is replaced. Android Studio JBR 21.0.10 on macOS; Linux reachability uses OpenJDK
21.0.12.1. `Reachability.java` is a standalone JDK-only probe for the Linux box.

- `rsync-argv.jsonl`: sixteen send/receive combinations of key/password and both
  debug switches, plus the four debug bridges' constructor version probes
  (`--version /tmp /tmp`; first stdout line is logged), in invocation order.
  `normalize-argv <log> <scratch> <argv-rsync path>` rewrites only scratch root
  → `<root>` and recorder → `<shim>`, keeping argv-rsync's serialization, so a
  fresh `run argv` regenerates the file byte for byte. The repeated receive
  `links(true)` setter emits one flag.
- `default-programs.jsonl`: actual rsync4j defaults are `/usr/bin/rsync` and
  `/usr/bin/sshpass`; missing optional sshpass emits a one-time warning.
- `processoutput-timeout.jsonl`: processoutput4j 0.0.12 with a one-second monitor
  timeout, actual exit 143, stdout retained, `hasTimedOut=true`.
- `gates*.jsonl`: actual bridge, two sends and two receives. Both receives enter
  while the first send holds the send mutex; the second send enters after its exit.
- `deadlines*.jsonl`: actual bridge's receive returns false after its 30-second
  process monitor kills the child; send is still pending and succeeds after release.
- `semantics.jsonl`, `bridge-auth-argv.jsonl`: actual factory/SharedTransfers/
  WriteFile methods. Distinct transport auth shares the bridge and the second
  send uses the first key/user. Same source path with changed bytes is reused;
  null exists reuses, false exists re-transfers. All-byte WriteFile is mode 0600.
- `reachability-*.jsonl`: `java Reachability.java` with a 150 ms budget: JDK
  loopback true; documentation networks, broadcast 255.255.255.255 and
  multicast 224.0.0.1 false. Broadcast/multicast connects fail immediately
  (EAFNOSUPPORT on macOS, ENETUNREACH on Linux), which `tcp_ping4` reports as
  unreachable instead of throwing. Raw Linux JDK version in the accompanying text.
- Text artifacts retain the corresponding raw JVM stdout/stderr. Timestamps,
  scratch paths and the reference logger abbreviations have not been rewritten.

`kotlin-worker-standard-U4/` is the **failed**, single full standard harness
run of `tools/fake-worker/validation/run.py --only standard`, including its raw
argv, streams, results, versions and awaited-cleanup transcript (gzip, mtime 0).
It completed the actual Kotlin wrapper's key and askpass-assisted password
roundtrips, WriteFile, generated-file fallback, bytes/modes/tree and delete checks.
Six of twenty independent direct sshpass attempts failed with status 255 (U4).
The manifest deliberately remains `terminal_outcome=failed`: it is incomplete
password evidence, never an all-pass or a comparator golden. The result-tree
archive is JSON with byte payloads as base64; it includes no disposable keys.
The existing shim adds explicit known_hosts/agent isolation, recorded separately
from the caller argv; production commands have no such extra options.

Rust checks: `connection_factory.rs`, `shared_transfers.rs`, `write_file.rs`,
`rsync_file_bridge.rs`, and `production_runtime.rs` under marathon-apple tests.
The real rsync Rust test uses system openrsync on macOS; the Linux gate uses
its documented Samba profile. It does not rerun flaky direct sshpass or claim
that askpass removes U4.

The focused process-error probes preserve nonzero false results, send-only error
prefixing, normalized stderr, and process-start IOException. The bounded
nonexecutable case records failed JVM cleanup: processoutput4j starts its reader
threads before ProcessBuilder.start, and those threads keep the JVM alive after
both methods have thrown. The watchdog kills that oracle JVM after five seconds
and returns 124; it is a bug witness, not a clean completion. Missing binary
discovery fails before readers start. Rust preserves the method error but owns
readers only after a successful spawn, under the frozen lifecycle's no-orphan rule.

`process-nonzero.*` uses `RSYNC4J_RSYNC=$PWD/tools/oracle/connection-probes/failing-rsync`
with `run process-errors <scratch>`; `process-nonexec.*` is the earlier run with a
nonexecutable shim, stopped externally after both method results. The fresh
`process-nonexec-bounded.*` run uses a temporary mode-0644 shell file as the
override and the same mode; it retains both errors and the watchdog's terminal
row, with exit 124. These streams are unnormalized. `run programs <scratch>`
without overrides produces `default-programs.*`. The Linux privilege witness
records uid 1000 without CAP_NET_RAW, matching the selected TCP-echo profile.

Review round 1 corrects the zero-budget interpretation: the existing
`reachability-zero-{macos,linux}.jsonl` files are historical single-run
observations, **not deterministic golden data**. They are retained unchanged.
The macOS review counts (934/1000 and 984/1000 true) and fresh R1 counts both
observe true and false for loopback. `ZeroReachability.java` reproduces the
1000-call measurement per address with JBR 21.0.10; counts are observations,
never expected probabilities. The Rust zero-budget test accepts completion or
an actual timeout on both platforms, alongside a positive-budget control.

`rsync-empty-options-argv.jsonl` comes from `run empty-options <scratch>`, with
both RSYNC4J_RSYNC and RSYNC4J_SSHPASS set to `argv-rsync`, and MRS43_ARGV set
to a fresh log. It records send/receive for remote path `/usr/bin/rsync`, then
empty, each with password `fake-password`, then empty. `normalize-argv` turns
the log into the file byte for byte (recorder → `<shim>`, scratch files root →
`<root>`); raw method stdout and stderr are retained. Empty passwords keep sshpass but omit `-p` and its
operand; empty remote paths omit `--rsync-path` entirely.

`process-malformed-utf8.*` comes from `run process-errors <scratch>` with
RSYNC4J_RSYNC pointing to `malformed-rsync`. That executable emits ED A0 80,
CR, `line`, then exits 23. The actual Kotlin wrapper logs one U+FFFD and
normalizes lines; Rust covers the same bytes through real subprocess stderr.
