# Logging (WP 8.8)

Diagnostics are `tracing` events. §3.12.5 pins only the per-test progress
lines and the end-of-run summary block; every other line uses the native
format below and may change again under the same clause. There is no
emulation left: `jcompat::logging` is deleted, no code abbreviates logger
names, parses ANSI, fakes threads, or keeps per-logger mutable levels.

## Owners

| Piece | Owner | Role |
|---|---|---|
| `Level`, `Component`, `LogSettings`, `Vendor`, `filter()`, `emit()` | `marathon-logging` | The filter and the bridge from owned records to `tracing` events. No I/O. |
| `LogQueue`, `LogRecord`, `LateOutput` | `marathon-core::logging` | Core's non-blocking record queue: call order, scheduler/wall stamps for replay, raw+log single order. |
| `TracingOutput`, `QueueWriter` | `marathon-cli::tracing_output` | The only subscriber and the only stdout writer. One fmt layer with a compact flag, over one queue writer. |
| `Console`, `CliOutput` | `marathon-cli::application` | Shared console for startup, run and crash output; wires `LogSettings::configure` to the subscriber reload. |

Libraries never install a subscriber, never format, and never write to a
terminal. Core actors append owned `LogRecord`s to the per-run queue (core
may do no I/O and takes no registry lock); vendor code logs through
`Component` handles and service-log closures into the same queue. Targets
are Rust module paths: `LogSettings::component` derives the target from the
caller's file (`#[track_caller]`), so a component created in
`crates/marathon-ios/src/device/simulator_provider.rs` is
`marathon_ios::device::simulator_provider`. At the console sink each record
becomes one `tracing` event via `marathon_logging::emit`, which caches one
static callsite per (target, level) — dynamic core targets therefore have
stable `Metadata`, working reloads, and zero per-event registry lookup.
Call sites that already run off core (the ADB client adapter, the CLI) may
use `tracing` macros directly against the same filter.

## Format

Regular (local `HH:mm:ss.SSS`; a queued record is stamped from its own `wall_millis`, however late it drains, and a direct event from the subscriber's clock):

```text
13:24:21.123 INFO marathon_cli::application: Starting marathon v0.10.1, CliConfiguration: ...
13:24:22.456 ERROR marathon_core::execution::queue: Message: ...
```

Compact (`apple.configuration.compactOutput`): no timestamp.

```text
INFO marathon_core::timing: Device provider initialized in 0 ms (+0 ms since start)
```

`tracing-subscriber`'s fmt layer owns color: ANSI on a terminal, plain when
redirected. Marathon code contains no ANSI parser and no stream-state
machine. Continuation lines of a message or stack trace keep their exact
bytes and whitespace; a stack trace gets the same `\tat frame` prefix rule
the Kotlin layout had (external frames in crash output keep theirs).

Raw output (progress, summary, test lists, YAML, exception text, worker
output forwarding) goes through the same `QueueWriter` (`Console::write_text`), so a
formatted record and the raw bytes around it never interleave. Lossy UTF-8
replacement for worker output and post-progress summary flushing are
unchanged.

## Filter

`marathon_logging::filter(vendor, debug)` is the single decision, used both
by `LogSettings::enabled` (laziness fences) and by the subscriber's reload
layer (rendering). The vendor configurator installs it through
`LogSettings::configure(debug, vendor)` after `configure_logging` sets the
debug flag. Decisions, pinned by one `marathon-logging` unit test per row
plus `configurators_preserve_levels_and_compact_output` in
`marathon-cli/tests/tracing_output.rs`:

| Configurator | debug=false | debug=true | Why |
|---|---|---|---|
| Startup (before config) | `marathon_*=debug`, other=`error`, timeline=`debug` | same | Before the Marathonfile is read only the startup logger exists and the root level is DEBUG; unrelated dependencies stay at `error`. |
| Android | `marathon_*=error`, other=`error`, timeline=`debug` | `marathon_*=debug`, other=`error`, timeline=`debug` | The ordinary level is chosen at `debug` and absolute; nothing sets it later. |
| Apple, regular output | `marathon_*=error`, other=`error`, timeline=`error` | `marathon_*=debug`, other=`error`, timeline=`error` | Only the `Timeline` suppression of the six Kotlin suppressions has a Rust target; the five sshj class names do not. The Apple configurator suppresses the timeline in both debug settings. |
| Apple, compact output | `marathon_*=error`, other=`error`, timeline=`error` | `marathon_*=debug`, other=`error`, timeline=`error` | Same suppression; compact only drops the timestamp from the format. |

Specific-target exceptions in every row: `marathon_cli::application=debug`
(the startup logger, never widened to the crate), `marathon_adb=debug` (it
inherits the root level, which no configurator resets),
`marathon_core::analytics::factory=debug`,
`marathon_core::analytics::provider=debug`,
`marathon_core::analytics::noop=debug` (the analytics loggers, whose WARN
diagnostics therefore print with debug off, as before),
`marathon_core::diagnostics=warn` (D66 SIGQUIT dumps, visible with debug off),
`marathon_core::timing=info`, `marathon_ios::device::timing=info`,
`marathon_apple::xcodebuild_stderr=warn`,
`marathon_core::reporters::timeline` per the table,
`marathon_apple::cmd::remote::ssh::sshj::known_hosts=warn` with debug off (`debug` with debug on — a floor, not a cap; host-key
warnings are security-relevant and stay visible; its matcher
component uses the same target),
`marathon_apple::batch_outcome=warn` with debug off (`debug` with debug on;
an all-incomplete Apple batch must be diagnosable without debug), and
`marathon_apple::cmd::remote::ssh::sshj::wire=debug` gated additionally by
`ssh.debug` on the `Component` (with ssh debug off, only ERROR shows). iOS provisioning timing records are emitted on
`marathon_core::timing` today; the `marathon_ios::device::timing` directive
is kept for that module's own records.

Two registry details matter:

- `LogSettings::enabled_target` with no vendor configured yet
  (`Vendor::Startup`) honours the `debug` flag for the ordinary level:
  `configure_logging` sets `debug` before the vendor configurator runs, and
  a configurator may be inert (the test mock), so the flag alone decides
  until `configure` installs a vendor. The subscriber's Startup filter is
  only ever installed with `debug=true`, before the Marathonfile is read.
- `LogSettings::set_level` / `component_with_level` pin one target ahead of
  the filter. Production code never calls them; test harnesses use them to
  capture one component's records while the rest of a run stays at its
  configured level (the old explicit per-logger registrations in tests).

## Cancellation diagnostics (WP x56)

Expected batch and discovery cancellation is DEBUG, as in task-completion logging.
An actual timeout still logs ERROR with the timeout advice. The signal notice
remains ERROR so D52 reaches a CI log with debug off. Device-loss and failure
levels retain their existing meanings.

`Failure::for_log` projects only the known Marathon-generated job-cancellation
messages, generated SSH timeout units and the exact generated device-loss
wrappers, recursively through typed causes. Known generated suppressed/cause
headers inside copied stacktrace text use that same mapping. Generated
`idle timeout PT30S reached` messages similarly render as
`idle timeout 30 s reached` (including fractional durations); only diagnostics
change, while stored failures and result text retain the ISO duration. Generated
session-close and simulator boot-timeout diagnostics use the same projection.
Provider startup, cancelled-batch and failed-boot shutdown warnings render
configured durations as seconds, preserving fractional values. Startup telemetry
values are labelled `cliFlags` because the Marathonfile can override them.
Returned-batch
reasons are projected only at their log sites, within the actor-generated failure
envelopes; the original reason data stays unchanged. `LogRecord::emit` uses that
projection; ordinary `Display`, stored failures and opaque external frame/tool
text keep their bytes. Apple listeners have a
cancellation callback whose default forwards the original report message to
`test_run_failed`; the composite preserves callback order and the debug listener
uses the separate diagnostic. Thus a cancellation cannot echo the report's
historical timeout wording back into the log.

## Queue bound

Finding (old code, `a830d106`): the core queue was **unbounded and
non-blocking**. `LogQueue` pushed into a crossbeam `SegQueue` and an ordered
`Vec`; `LateOutput` used an unbounded `std::sync::mpsc` channel to its
forwarder. The output task drained in batches and never back-pressured a
producer; a stalled sink grew memory, and only the exit deadlines bounded
shutdown.

The new writer keeps exactly that property: `QueueWriter` sends each
formatted chunk over an unbounded `std::sync::mpsc` channel to one writer
thread. `write` never waits for the sink, so core and vendor producers can
never block on a full pipe. The trade-off is the same as before: memory is
unbounded while a sink stalls. `tracing-appender` is therefore not used —
its bounded worker (lossy or blocking) would change this property, and one
writer must serve raw and formatted output anyway. New direct dependencies
are only `tracing` and `tracing-subscriber`.

## Shutdown, signals, late events

The production signal path (`ProductionRunner` in
`marathon-cli/src/runner.rs`) keeps D52's behaviour during the run:

- **SIGQUIT (D66):** a separate process-lifetime Unix listener is installed before
  config loading unless the signal was inherited as ignored. Setup failure warns
  once and lets every command continue with previous OS behavior. During a run
  the listener notifies a core snapshot task, which queues one
  bounded WARN event on `marathon_core::diagnostics` through the same writer. The
  explicit header timestamp remains in compact output. It never enters the
  shutdown relay; before/after a run it may be silent. Dump construction panics
  become WARN events and leave the task running. Dumps remain unthrottled.
- **First SIGINT/SIGTERM during the run:** cancel and write what can be written (reports, then the
  summary), bounded by the existing cancel limit. The exit path runs off
  core: `exit_after_preparing` does the final queue forwarding on the
  `marathon-exit-output` thread and a `marathon-exit` watchdog thread exits
  the process after one second if the flush has not drained — a blocked
  stdout (full pipe, stopped reader) cannot hang signal exit. After the
  deadline the process exits anyway. The signal hook never
  completes after queueing that exit, so `Marathon::run` cannot return a
  failure ahead of it (the one `process::exit` comes from the writer or the
  watchdog, or from a second signal); `exit_after_flush` after the writer
  has already closed exits at once through the watchdog, because the `Exit`
  acknowledgement disconnects. Every exit in the CLI goes through
  `tracing_output::exit_once`: the writer, the watchdog and a second signal can
  reach it at nearly the same time, libc `exit` must not run on two threads at
  once, so the first caller exits and a later one parks.
- **Second signal during the run:** `SignalHooks.immediate_exit` is `exit_once` with the
  second signal's status (130/143). It does not wait for cleanup,
  reports, the writer thread, or the flush deadline.
- **Ordinary completion after the run (D67):** `CliOutput::flush` and the later
  `OutputGuard` drain share one 60-second deadline, starting at the flush (or
  the drain if no flush ran). Both stages use only its remaining time. The
  signal relay has stopped, while Tokio's handlers remain installed:
  SIGINT/SIGTERM are not acted on and there is no second-signal escape. This
  window already existed for about two seconds; a stalled console can now
  hold exit for up to 60 seconds. A wrapper needing a hard stop then must use
  SIGKILL. D66 SIGQUIT behavior is unchanged.
- **Late vendor events:** vendor tasks that outlive the run task push
  through `LogSender`/`LateOutput`. `LateOutput::close` is a fence:
  everything pushed before it is forwarded to the console sink in order
  before `close` returns, and records pushed after the fence are forwarded
  by the same forwarder task while it lives. Once the console writer itself
  has closed, a write fails explicitly with `BrokenPipe` and is counted in
  `OutputStats.rejected` — never a panic, and never a silent success.
  Nothing in any of these paths blocks the core thread.

Tests: `shutdown_reports.rs::a_cancel_mid_test_writes_the_reports_and_does_not_bounce_the_test_d52_u161`
(a real SIGTERM/SIGINT through the built binary; the summary block is in
stdout), `signal_windows.rs::a_second_signal_exits_at_once_during_a_blocked_sink`
and `a_stuck_console_does_not_hold_a_cancel_d52` (blocked sink, first vs
second signal), `a_slow_exit_preparation_still_exits_with_the_signal_status`
(exit preparation that never finishes cannot let the run return first),
`log_queue_output.rs::late_output_forwards_records_pushed_before_and_after_the_close_fence`,
`tracing_output.rs::writes_after_close_are_rejected_not_lost_silently`,
`a_blocked_sink_never_blocks_or_drops_producers` and
`cancellation_flushes_even_when_process_exit_skips_guards` (including
`exit_after_flush` after the writer has closed).

## Construction-time level audit

The old registry froze a level on each logger at its first lookup
(DEBUG while `debug` was still its default `true`); a later
`configure(false)` never revisited it (C18). Every logger created during
construction therefore kept DEBUG in a `debug: false` run. The audit of the
old construction sites, per crate, and the new treatment:

| Crate | Construction-time loggers (old kept level DEBUG) | Now |
|---|---|---|
| `marathon-cli` | the `ApplicationView` startup logger, created before config | Retained: `marathon_cli::application=debug` (specific target, not the crate). |
| `marathon-core` | `FileManager`, `BillingReporter`, `TrackerFactory` (`registry_logger` in `construction.rs`), reporter constructors | Ordinary level. |
| `marathon-core` (analytics) | `MetricsProviderFactory`, `MetricsProviderImpl`, `NoOpMetricsProvider` (registered in `analytics/`) | Retained: factory/provider/noop targets at `debug`. |
| `marathon-core` | `Timings` (explicit INFO on first use, not construction) | Retained: `marathon_core::timing=info`. |
| `marathon-adb` | adam's loggers (never registered; inherited ROOT) | Retained: `marathon_adb=debug`. |
| `marathon-android` | `AdamDeviceProvider` (provider `new` in services construction), `AmInstrumentTestParser` (Kotlin constructs it before debug is set) | Ordinary level. |
| `marathon-apple` | loggers created during services construction; sshj loggers are created per lookup at run time | Ordinary level; the wire keeps its `ssh.debug` gate; `xcodebuild` stderr keeps its explicit WARN as `marathon_apple::xcodebuild_stderr=warn`; of the six Apple ERROR suppressions only `Timeline` maps (`marathon_core::reporters::timeline`), the sshj class names have no Rust target and are dropped. The sshj `known_hosts` logger and its matchers (one target) retain a WARN floor in every configurator; `marathon_apple::batch_outcome` also has a WARN floor (no JVM counterpart). |
| `marathon-ios` | `AppleSimulatorProvider`/`provisioner` (constructed in `ios_vendor::construct`) | Ordinary level (test harnesses pin these targets with `set_level` to capture their records). |
| `marathon-macos` | `AppleMacosProvider` (constructed in `vendor::construct`) | Ordinary level. |
| `marathon-analytics` | no loggers of its own at construction (records flow through core analytics) | — |
| `marathon-test-support` | `StubDevice` (inherited ROOT) | Ordinary level; test-only. |

"Ordinary level" is the per-configurator `marathon_*` level from the table
above — the level a logger created after `configure` always had. The level
a run gets no longer depends on whether a handle happened to be created
before configuration, and recreating a `Component` can never change it.

### Debug-off visibility (C18 superseded)

Dropping C18 is deliberate: one consistent rule (ordinary is ERROR with
`debug: false`) replaces creation-order luck. The cost is that, with debug
off, WARN/INFO lines from the components Kotlin happened to construct
before `configureLogging` no longer show. They are: `AdamDeviceProvider`
(`marathon_android::provider`); the construction-time loggers in
`marathon-core` (`FileManager`, `BillingReporter`, `TrackerFactory`, today
all on `marathon_core::construction`); the iOS simulator provider
(`marathon_ios::device::simulator_provider`); and the macOS provider
(`marathon_macos::provider`). Their ERROR lines still show, and everything
shows with `debug: true`, as for every other ordinary logger.

Two exceptions have a WARN floor in every configurator.
`marathon_apple::cmd::remote::ssh::sshj::known_hosts` keeps unknown- or
changed-host-key warnings visible because they are security-relevant and must
not hide behind a debug flag. It is
pinned by `marathon-logging`'s
`known_hosts_warns_in_every_configurator_with_debug_off` and by the
directive inventories.

`marathon_apple::batch_outcome` explains an Apple batch with no finished, failed
or ignored test result, because diagnosing an all-incomplete batch must not
require a debug rerun. Once result listeners and session stop have finished, the
shared Apple runner emits one WARN with the device serial (udid and worker
host:port for SSH), batch size, session duration and the known exit status,
signal, missing status, timeout or worker-connection failure. A command-open
failure says `xcodebuild could not be started` with the known reason. Elapsed
time renders as seconds (one decimal below 10 s, whole seconds thereafter).
Session duration uses the vendor monotonic clock from launch through output completion or forced
stop, before command cleanup; artifact collection can overlap a running session.
Requested cancellation, including D52, and batches with any completed result are
excluded. The target is a floor like
`known_hosts` (`debug` with debug on), pinned by
`batch_outcome_warns_in_every_configurator_with_debug_off`, the directive
inventories and the CLI subscriber test. See [WP x57](../wp/x57-findings.md).

## What is gone

- `jcompat::logging` (layout, `%logger{N}` abbreviation, Jansi handling of
  event text, mutable per-logger levels and laziness side effects) and its
  oracle tests. C18, C20 and C24 in `docs/known-kotlin-bugs.md` are
  superseded.
- The `library.logback.wall` scenario-tape read: record stamps come from
  each record's `wall_millis`; no Rust code reads that key. (The key still
  exists in the frozen JVM oracle hooks under `tools/oracle`, which replay
  historical captures.)
- The thread column: records keep only scheduler/wall stamps internally
  for replay; no fake thread names are rendered.

The observed formats are pinned by `marathon-cli/tests/tracing_output.rs`
and `cli_matrix.rs`; the contract bytes by the differential goldens
(`docs/wp/8.8-golden-diff.md`).
