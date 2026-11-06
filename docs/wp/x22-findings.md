# X22: follow-ups to the history refresh (D34 / C41)

The review of [x15](x15-findings.md) left four items, and the project owner approved them on 2026-11-07. They harden D34
and are not a new deviation. No configuration field, CLI option or exit code changed, and no recording or
comparator did.

| Item | Result |
|---|---|
| A real transport from failing to answering | `crates/marathon-analytics/tests/refresh.rs`, for InfluxDB, InfluxDB 2 and Graphite |
| The stale `refresh.task` after the run's cancellation | A drop guard, `RefreshEnd` |
| `close` held up by a source that blocks | The join has a bound of 3 s by the vendor clock, then one WARN |
| "returned 0 tests after failing earlier" | The line stays at INFO, and no longer promises new history |

## The transports

`refresh.rs` runs each real client through `MetricsProviderFactory`, on real sockets and the vendor runtime of
`tests/support/mod.rs`. A test clock passes a first query's retries at once and holds each refresh delay until
the test advances it.

The peer serves the JVM recording `history-<backend>-p50`, and panics on any request the recording lacks. Only the
query fails while the backend is down, either with an HTTP 500 or with a connection that ends with no response, which
needed a small `Reply::dropped` in the test support.

The sequence is the same for each of the 6 combinations:

| Step | Queries so far | The answer |
|---|---|---|
| First query, three attempts | 3 | The default, 300000 |
| The 5 s re-query, still down | 4 | The default |
| The 10 s re-query, back up | 5 | The JVM's recorded 5000 |
| `close` | 5, with only the delays of 5 s and 10 s ever started | |

The log has 2 failed Timings lines, 2 `Cannot fetch` warnings, and one line each for `returned 1 tests in` and
`returned 1 tests after failing earlier`.

## The stale handle

`race(run.cancelled(), run_refresh(..))` dropped `run_refresh` when the run's signal won, and its cleanup never ran.
`refresh.task` stayed set and `failed` kept its keys, so a later failure started no refresh, and any later round would
have queried the old keys too.

In production the effect is small: once the run's signal fires, a prefetch fails before it reaches `schedule_refresh`,
apart from a window of a few instructions on another thread. The stale state is still wrong, and each of the three
exits had its own cleanup.

`RefreshEnd` is created before the race, because the signal can win before `run_refresh` is first polled. It clears
both fields on any early end. A refresh that runs out of keys disarms it under the lock that a new failure registers
under. Otherwise the guard could clear a new task started between that unlock and the drop, and break single flight.

Only one task can hold an armed guard. A new task starts only when `task` is `None`, which needs the old guard to have
cleared or been disarmed, or `close` or the provider's drop to have taken the handle. After either of those no new
task can start.

A task the runtime drops before its first poll has no guard, but that only happens as the vendor runtime goes away,
when nothing can use the state. The guard is deliberately not made in `schedule_refresh`: tokio can drop a future
inline during a spawn on a runtime that has shut down, and `schedule_refresh` holds the lock the guard takes.

A real signal never un-fires, so the tests use one that does (`Refiring`). This makes the stale state visible with no
test-only API.

## The bound on `close`

**The bound is 3 s of vendor clock.** An async query ends at its next await, which takes microseconds, and a two-worker
vendor runtime on a loaded host may add some scheduling delay. So only a source that blocks inside its future
takes 3 s.

No such source exists today, as the HTTP clients are async. A wait that is cut short costs little: one WARN, and
a close under a query that has already been cancelled. That query can no longer log or publish when it ends, since
`fetch` checks the signal after the query returns. So the bound leans short.

The line is `MetricsProviderImpl` WARN `The history refresh did not stop within 3000 ms of being cancelled. Closing the
analytics backend with its query still running`.

- The delay uses a signal of its own, since the run's cancellation must not end the wait early and let the source
  close under a query that is about to stop.
- If the clock itself fails, which only a replay's does, `close` waits for the join as it did before.
- A healthy run has no task at `close`, so it never asks the clock for this delay. With a failing backend the timer is
  dropped once the join finishes, and the virtual clock ignores dropped timers, so replay time doesn't move:
  `startup_timings::a_run_against_a_backend_that_stays_down_ends_without_waiting_for_its_refresh` still ends at the
  same moment as a healthy run.

## The empty answer

The line was misleading. For an answer of 0 tests, `Queues built and batches composed from now on use it` promises
something that cannot happen, as there is nothing to use and tests keep the defaults.

The line itself is still useful: it is why the key's warnings stop, and the Timings line alone doesn't say the refresh
has ended. So the level stays INFO and the text now ends `There is no history to use, so nothing changes`. The number of
lines is unchanged, so D34's accounting holds. The accounting in `x15-findings.md` now also lists the new `close` warning.

## Evidence

No recording changed. `analytics_oracle::provider_sequences_match_jvm_bits_calls_and_ordered_logs` still replays the JVM
`cache-fail` row unchanged, and `history.rs` and `lifecycle.rs` still replay theirs.

Each change was reverted temporarily, and these failed:

| Reverted | Failed |
|---|---|
| The guard never armed | `a_refresh_the_run_cancels_leaves_no_stale_handle_for_the_next_failure`, `a_refresh_the_run_cancels_before_its_first_poll_leaves_no_stale_handle_either` |
| The guard armed only at `run_refresh`'s first poll | `a_refresh_the_run_cancels_before_its_first_poll_leaves_no_stale_handle_either` |
| `close` joins with no bound | `close_stops_waiting_for_a_source_that_blocks_inside_its_future`, `refresh::close_gets_past_a_blocked_vendor_worker_on_the_real_runtime_and_clock` |
| `schedule_refresh` not called | `refresh::influxdb_comes_back_through_the_refresh_after_http_errors`; the other five transport tests were not run with this revert, and share its code |
| The old wording for an empty answer | `an_empty_answer_is_a_success_so_it_is_cached_and_never_refreshed` |

`close_gets_past_a_blocked_vendor_worker_on_the_real_runtime_and_clock` uses the real vendor runtime and clock, so it
takes the full 3 s of real time. It sets no upper limit on the time. Its lower limit is safe, since a real timer cannot
fire early.

Run: `cargo fmt --check`, `cargo clippy -p marathon-core -p marathon-analytics --all-targets -- -D warnings`, and
`cargo test -p marathon-core -p marathon-analytics`, which passed. `scripts/check.sh`, `scripts/remote-check.sh` and
`cargo test --workspace` were not run, by the rule for this batch; the orchestrator runs them.

## Not verified

- No real InfluxDB or Graphite was used. The peers are fake, and their answers are the JVM's recordings.
- `source.close()` itself has no bound, and that was outside this item. A source blocked on something its `close` also
  needs could still hold up the end of a run.
- `close` needs a free vendor worker to run on. A source that blocks both workers at once, in a refresh and in a pool's
  first query, would stop `close` from running at all. A first query has no bound of its own.
- The disarm under the lock has no failing test. The window it closes has no await in it, so a test cannot hold it open.

## Incidental bugs

None recorded in `docs/found-bugs.md`. One observation, not a bug: `clients::real_transport_query_failures_retry_three_times_and_remain_cached`
still says "remain cached". It is true of what the test sees, since it closes before the first refresh is due, and the
name was left alone as out of scope.
