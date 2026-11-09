# Logging after WP 8.8

Milestone 1 is a design stop. `cargo run -p marathon-cli --example tracing_pilot
-- version` exercises the real `run_cli` with `CliOutput::traced`; `run` also
exercises missing/configuration/construction errors. The new subscriber tests
include a real ADB rejection emitted from another thread. Production startup,
existing constructors and differential adapters stay on their current path
until review. No existing expectations, copied fixtures, parity rows or goldens
change at this stop.

## Subscriber and directives

One `Registry` has a reloadable `Targets` filter and one `fmt` layer. Libraries
emit events; the CLI owns the subscriber and writer guard. The pilot explicitly
passes a `Dispatch` into CLI/ADB. Milestone 2 installs the same dispatch process
wide before construction, so core/vendor tasks cannot lose a thread-local
subscriber. The pilot reloads after configuration parsing; production reload
belongs at the existing vendor configure-logging boundary, off core, after
construction. No new flags or RUST_LOG overrides. Stable dependencies are
`tracing` 0.1.44 and `tracing-subscriber` 0.3.23, verified against crates.io.

`ordinary` below expands to `marathon_cli`, `marathon_core`, `marathon_config`,
`marathon_android`, `marathon_apple`, `marathon_ios`, `marathon_macos`,
`marathon_analytics`, `marathon_dex`, `marathon_test_support`.

| Configurator | debug=false | debug=true | Format |
|---|---|---|---|
| Startup, before config | ordinary=debug | ordinary=debug | With time |
| Android | ordinary=error | ordinary=debug | With time |
| Apple, compactOutput=false | ordinary=error | ordinary=debug | With time |
| Apple, compactOutput=true | ordinary=error | ordinary=debug | Without time |

More specific directives apply in every configuration:

| Target | Level | Existing decision |
|---|---|---|
| default / dependencies | error | Dependency noise suppressed; Netty/Vert.x are absent |
| `marathon_cli::application` | debug | Startup logger stays enabled |
| `marathon_adb` | debug | Adam inherits DEBUG ROOT; configure does not reset it |
| `marathon_core::analytics::{factory,provider,noop}` | debug | Metrics loggers constructed before configure keep DEBUG |
| `marathon_core::timing`, `marathon_ios::device::timing` | info | Timings visible with debug=false |
| `marathon_apple::xcodebuild_stderr` | warn | Nonblank xcodebuild stderr remains visible |
| `marathon_core::reporters::timeline` | debug; Apple overrides to error | Apple silences timeline |
| `marathon_apple::cmd::remote::ssh::sshj::wire` | debug | Additionally gated by existing ssh.debug |

Apple's other five suppression names are absent sshj implementation classes;
do not invent Java-name targets. The russh `log` bridge must retain its ssh.debug
forwarding gate without installing a second subscriber/dependency. HTTP debug
gates stay at their call sites. As modules migrate, audit construction-time
registrations and give deliberately retained exceptions specific targets;
do not widen a crate to make quiet-mode tests pass. The pilot migrates CLI
events and ADB's backend, leaving other call sites and queues for milestones 2/3.

## Lines and pinned bytes

Ordinary lines are `HH:mm:ss.SSS LEVEL rust_module: message key=value\n`.
Time is local wall time; the injected clock must do no blocking I/O on core.
Levels are full and unpadded. Compact Apple output omits time. Only the level
gets ANSI colour on a terminal; redirected output has no formatter-generated
escapes. Two real events, with a fixed clock:

```text
before: 12:34:56.789 [main] INFO  c.m.marathon.cli.ApplicationView - Checking Marathonfile config
after:  12:34:56.789 INFO marathon_cli::application: Checking Marathonfile config

before: W 12:34:56.789 [main] <com.malinskiy.adam.request.Request> adb server rejected command 000Chost:version
after:  12:34:56.789 WARN marathon_adb::client::log: adb server rejected command 000Chost:version
```

Both after-events run in the pilot tests. The first before-event is pinned in
`marathon-cli/tests/run_outcome.rs`; the second uses Adam's existing request
message and runtime layout. In the wider port macros move to their producing
modules; the pilot ADB adapter uses its own Rust module target, `client::log`.

`PoolProgressAccumulator::{line,started,ended}` in
`marathon-core/src/execution/progress.rs` retains the percentage formatter,
test names and status strings, then calls `LogQueue::raw`. CI uses that raw path.
`StdoutReporter::generate` in `marathon-core/src/report/reporters.rs` builds the
whole summary, including `Marathon run finished:`, pool headings, indentation,
failure lists, overhead, `\tRaw: ...`, `Total time` and final blank line, then
sends one `OutputRecord::Raw`. These generators/formatters stay unchanged.

Raw bytes bypass tracing, filters, colour and timestamps. Both raw records and
formatted events enter the same FIFO writer as complete buffers; do not detect
pinned output by matching log messages. Existing
`progress_oracle::{progress_line_contract_for_each_status,started_ci_passes_fraction_and_prints_after_ci}`
and `reporters::stdout_summary_contract` retain their expectations. The pilot
sandwiches raw progress/summary bytes between coloured events and checks order.

## Non-blocking output and exit

`QueueWriter` sends owned buffers to an unbounded std channel. One dedicated
thread writes and flushes them. Producers do no sink I/O or queue-capacity wait.
A bounded appender would drop lines or block when full, so the pilot uses this
small writer rather than tracing-appender. Its blocked-sink test enqueues
200,000 writes, releases the sink and verifies every line. A persistently blocked
sink can grow memory without limit.

On success and errors, stop/join producers, then call `OutputGuard::shutdown`
on the CLI thread before returning the requested exit code. Finish drains the
queue, flushes and joins, returning the first sink error. Drop is an early-return
fallback; never own the guard on core. The executable pilot explicitly shuts
down on every run_cli outcome, and tests cover success and configuration error.

Today ProductionRunner supplies `process::exit`: the first-signal hook flushes
OutputDriver before calling it, and the second-signal listener calls it directly.
Both skip destructors. Milestone 2 replaces this closure with
`exit_after_flush(status)`: enqueue an exit barrier without waiting on core;
the writer drains earlier buffers, flushes and exits with that status. The
subprocess pilot proves exit 130, the final raw summary, and continued processing
of exit requests after sink errors. Events after a barrier are outside its
guarantee; OS-rejected bytes cannot be recovered.

A sink that never accepts a write cannot finish draining. Production integration
still needs an off-core deadline/fallback preserving D52's second-signal
emergency exit and existing cancellation deadlines, tested with a blocked sink.
This is an explicit design-review risk; production signals are not switched here.

## Files, goldens and tests

No diagnostic file appender exists. Redirected stdout records the new diagnostic
format alongside unchanged raw text. TimingLog is console-only; timing data in
reports retains its encoders. `FileLogWriter`/`LogListener` in
`marathon-core/src/artifacts.rs` write raw log attachments: Android selects
`logcat`, iOS selects `xcodebuild-log`. XcodebuildStderrLog separately warns on
the console; preserve the original listener lines. The iOS device-log listener
runs `log stream --color none --style compact` into remote `device.log` and
pulls it verbatim. Command-session stdout/stderr, embedded logcat HTML and failure
stack traces remain external/result data, outside diagnostic formatting.
Preserve file names, bytes and newline handling.

After migration, build the YAML helper, re-bless only through
`tools/differential/run bless` following its README, then run
`tools/differential/run diff-golden --against a830d106`. Record changed
views/files in `docs/wp/8.8-golden-diff.md`; prove the ordered progress stream
and complete summary byte-identical, and result data/worker commands unchanged.
In vendor_console.py normalize only the time token on a complete new-format
prefix (`HH:mm:ss.SSS LEVEL marathon_<crate>::<module>:`). Keep level, target,
message, fields and continuation lines exact; compact events have no time.
Never apply this rule to raw output. Keep historical prefix handling. Adversarial
tests must reject changed/missing/duplicated/reordered pinned lines and unblessed
ordinary changes; only the deliberately updated view accepts a changed ordinary
line. Existing wall-seam tape keys remain copied bytes; final code stops reading
`library.logback.wall`.

The [test inventory](../wp/8.8-test-changes.md) counts 60 existing log/console-text
assertion files plus one new file with eight pilot tests. Existing assertions
stay unchanged. Later, prefer structured diagnostic assertions; preserve raw
progress/summary and result assertions, and list every changed test file there.
Deleted parity tests require `n/a`, reason `8.8: §3.12.5`, and reviewer agreement.
Run the workspace gate per milestone. Later integration tests cover actual
SIGINT/SIGTERM, construction/report shutdown, second signal with a blocked sink,
late vendor events, and one process subscriber across core/vendor runtimes.
This stop does not remove emulation or claim full WP 8.8 acceptance.
