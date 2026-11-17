# WP x71: a reconfigured console keeps callsites an earlier console registered (U199)

Base: `dbe540ed`. Approved 2026-11-17 by the project owner ("investigate U199"). No deviation: logs are not contract, and
the released binary was not affected.

## Cause

`run_outcome::run_events_and_the_crash_log_share_the_console` once lost its final `Marathon execution crashed` ERROR line
on the macOS CI runner. The console queue was not at fault. `TracingOutput::configure` reloads the console's target
filter; `tracing_subscriber::reload` then calls `tracing_core::callsite::rebuild_interest_cache`. While only one dispatcher
is registered, tracing-core rebuilds through `dispatcher::get_default` on the calling thread instead of the registered
list. `run_cli` configures the console outside its dispatcher's scope, and test binaries set no global default, so the
rebuild asked `Dispatch::none`, which answers `never`. Every callsite already registered, including the crash line's
`error!` in `CliOutput::log` (registered by an earlier test's crash), was cached as never, and the line was dropped.
`LogRecord::emit` (the run's INFO line) uses its own callsite that ignores the cache, which is why only the ERROR line
went missing.

The condition is that this test's console is the only live dispatcher when it is created, and some earlier test in the
same binary already hit the callsite: rare in the parallel suite, and certain when two crash runs go one after the other
in one process (reproduced: the second run's stdout lacks the line).

The released binary is not affected: `main.rs` sets the console as the global default before anything configures it, so
`get_default` returns the console.

## Fix

`TracingOutput::configure` performs the reload inside `tracing::dispatcher::with_default(&self.dispatch, ..)`, so the
single-dispatcher rebuild asks this console. With several dispatchers registered tracing-core iterates the registered list
as before, so nothing else changes.

## Verified

- `console_interest::a_later_console_keeps_a_callsite_an_earlier_one_registered_u199`, in its own test binary because
  other tests' live dispatchers in a shared binary hide the condition: two consoles in turn, each configured from the
  test thread and then emitting the same `error!` callsite in scope. Without the fix the second console's output lacks
  the line (fails at "second console lost the line"); with it both have it.
- `cargo test -p marathon-cli`, fmt, clippy; the gates at merge.

## Incidental bugs

None.
