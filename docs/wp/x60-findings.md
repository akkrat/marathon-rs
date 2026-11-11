# WP x60: ordinary console completion and diagnostic text

Base: `19ec3fbc` (`0.13.0-rc.2`). Approved 2026-11-11 by the project owner
for 0.13.0. D67 changes the ordinary completion allowance; the diagnostic
changes follow PLAN §3.12.5.

## A. U196 / D67

`TracingOutput` and `OutputGuard` own the same `Arc<CompletionDeadline>`.
Its `OnceLock<Instant>` starts on the final ordinary flush, or on guard
shutdown/drop if there was no flush. The acknowledgement and drain each
compute the remaining time immediately before waiting. An explicit shorter
guard timeout is clamped to that same deadline. Neither stage can restart
its allowance. The deadline is not measured from construction or run startup.

The ordinary allowance is 60 seconds. `EXIT_FLUSH` remains one second and
still bounds only the signal watchdog, including preparation. Second-signal
immediate exit and D66 SIGQUIT are unchanged. Queue messages, admission,
ordering, nonblocking producers and the existing expiry diagnostics are
unchanged. No configuration knob or differential stderr tolerance was added.

Tests:

- `ordinary_delayed_flush_recovers_with_status_and_ordered_output_u196`
  replaces the x59 witness: gate the sink, observe ordinary flush starting,
  keep it gated past the former one-second budget, then release it and assert
  complete ordered version output, exit 0 and no stderr diagnostic. Its
  one-second-plus window is a negative check; successful waits use 60-second
  hang guards.
- `completion_tests::stalled_flush_and_drain_share_one_deadline_u196`
  gates the sink and deliberately injects an expired ordinary deadline,
  avoiding a real minute of waiting. Flush must report expiry, and drain
  must retain that expired deadline and finish before a fresh 60-second
  allowance could expire. The sink remains gated until shutdown returns;
  the test then releases and awaits the writer. Its five-second bound
  deliberately separates immediate expiry from a restarted minute.
- `shutdown_reports_sink_failure` now checks both the flush and drain
  diagnostics for a genuine sink failure.
- Existing signal-window and cancellation subprocess tests retain their
  first/second-signal assertions.

The lifecycle design delegates writer deadline policy to logging.md and
states no ordinary numeric bound, so it needs no edit. D67 is recorded in
PLAN, deviations, migration, the changelog and U196's fixed row. The
current deviation ranges were advanced through D67 as requested.

## Verification

All Rust builds use `CARGO_BUILD_JOBS=6`, `CARGO_INCREMENTAL=0` and
`CARGO_PROFILE_DEV_DEBUG=0`. The initial disk check had 144 GiB available,
above the 20 GB stop threshold.

Part A verification before its commit:

- `cargo test -p marathon-cli --lib --test tracing_output`: passed (3 unit
  tests and 14 console integration tests).
- Counterfactual checks: restoring the one-second ordinary flush failed the
  delayed-recovery witness; restarting the guard with a fresh 60-second
  allowance failed the shared-deadline witness. Both exited 101; the source
  was restored and the witnesses passed again.
- `cargo clippy -p marathon-cli --all-targets -- -D warnings`: passed.
- Plan DAG and differential registry validators: passed.
- `tools/differential/test_evidence.py`: initially failed because the YAML
  helper had not been built; after building `snakeyaml --example yaml_tree`,
  all 107 tests passed.
- An initial full CLI run hit that same missing helper in vendor differential
  and production-parser checks. It also overlapped the temporary counterfactual
  binary, so its console witness failed as expected for the old flush policy.
  The final full crate verification is run after the log changes, with the
  helper present and no counterfactual builds overlapping it.

The configuration differential test and signal-window tests passed in that
initial run. No fixture, golden or stderr comparator was changed. Full gates,
remote checks and workspace-wide tests are reserved for the orchestrator by
the brief.

## Incidental bugs

None independently verified beyond the assigned U196 and diagnostic leftovers.
