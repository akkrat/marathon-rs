# X39: follow-ups to D51, D47 and D50 from a post-merge review

A second-model review of the merged x34 (D51), x30 (D47) and x32 (D50) raised one major finding, one minor and one nit.
All three are fixed here. No configuration field, CLI option or exit code changed, and no new deviation was needed: each
fix makes an existing one do what its text says.

## S-x34-1 (major): InfluxDB v1's last flush could hold the end of a run

**Finding, checked against the code.** `BackendGuard::close` (`crates/marathon-core/src/analytics/event_pipeline.rs`)
awaited the backend's close with no limit. For InfluxDB v1 that close is `InfluxClient::shutdown`
(`crates/marathon-analytics/src/influx.rs`): it sends `Close` to the worker, which flushes what it holds and makes one last
attempt per buffered batch, then joins the worker. Each request is bounded only by the connector's 60 s connect, read and
write idle timeouts, and a server that sends a byte now and then never trips them. The reviewer saw a run still waiting
after 65 s and 261 chunks. Kotlin's close waits the same way, so this was a gap in D51, not a regression.

**Fix.**

- `BackendGuard::close` bounds the whole close, worker join included, with `BackendLimits::send`, the 30 s that D51 already
  gives a send. On expiry it logs one WARN, `The analytics backend did not close in 30000 ms; test metrics it still held are
  lost`, and returns `Ok`, so the exit code is what it would have been. A close that fails or is cancelled still passes
  through as before.
- Dropping the close is how the guard stops waiting, and that alone would leave InfluxDB's worker running its request.
  `InfluxClient` now keeps the worker's own cancellation signal, and `shutdown` cancels it if its future is dropped before
  the join completes. The worker's request then ends at once and the socket is closed. A later `shutdown` that finds the
  worker stopped this way returns `Ok`, since the caller that gave up has already reported it.

**Why 30 s.** It is the budget D51 sets for one send, and a close is one send per batch the client still holds. A working
server answers a batch of 1000 points far inside it. A separate value would be a second number to explain for no gain.

**Graphite and InfluxDB 2, checked.** `GraphiteTracker` has the adapter's default close, which returns at once; carbon keeps
no connection between sends. `Influx2Client::shutdown` only takes its operations lock and sets `closed`. The lock is held by
a write in progress, and the guard has already dropped any write that ran past its limit, which releases it. History's
clients are separate objects closed by the history provider, and the v1 one has no points to send at close. All three
tracker closes now fall under the same 30 s anyway.

**Tests.**

| Test | What it shows |
|---|---|
| `marathon-analytics/tests/clients.rs::a_last_flush_to_a_trickling_server_gives_way_at_the_close_limit` | The real `InfluxClient`, `InfluxTracker` and guard against a loopback server that answers writes with a body sent a byte every 20 ms. The test waits for the worker's timed flush to reach the server, closes with a 500 ms limit, and then waits for the server to see the client hang up. Exactly one guard line is logged. Runs in about 1.7 s |
| `marathon-core/tests/event_pipeline.rs::a_close_that_never_finishes_gives_way_and_is_logged_once` | A scripted backend whose close never returns: the guard's close returns `Ok` with the one WARN line |

Revert proofs, run on this machine: with the guard's limit taken out, the first test hangs until the 60 s watchdog
(`analytics test stalled`); with the limit in place but `shutdown`'s cancel taken out, it hangs there too, because the
worker goes on trickling after the guard gives up.

**Docs.** `docs/migration.md`'s analytics section said a slow backend could not hold up a run, which was not true of the
close; it now names the close's 30 s. The same is added to D51 in `docs/deviations.md`, its PLAN §3.11 row and its
CHANGELOG bullet. `docs/wp/x34-findings.md` gets a pointer here where it says closes are not bounded.

**Limits.**

- A slow but working server that needs more than 30 s for everything the client still holds loses the rest. The retry
  buffer only grows while sends fail, so this needs a server that was failing and then answers slowly.
- If the close is dropped for another reason, such as cancellation of the end of the run, the worker now stops too, where
  before it went on alone until the runtime ended. Nobody observed its result in either case.
- Not verified against a real InfluxDB server, or through a whole CLI run with a trickling server.

## S-x30-1 (minor): the uncompleted count could wrap at the largest accepted quota

**Finding, checked against the code.** `handle_uncompleted_tests` (`crates/marathon-core/src/execution/queue.rs`) charges
one attempt per uncompleted copy, with `wrapping_add` for Kotlin's `Int`. Copies of one test in the same batch are charged
together, so the count steps by more than one. With four copies and a quota of 2147483646 the count goes 0, 4, …,
2147483644, which is under the quota, then wraps to -2147483648 and never reaches it. D47 says every accepted finite quota
ends the loop, and that was false at the top of the range. Kotlin wraps the same way; it was unreachable there too, since
the default was unlimited.

**Fix.** The charge moves into `charge_uncompleted`, which saturates at 2147483647. It still decides which copies are over
the quota from the counts before any copy of the batch is charged, so nothing changes below the boundary. D47's text in
`docs/deviations.md` says so in one sentence, and ledger row C85 notes it.

**Tests.** `execution::queue::tests::four_copies_at_the_largest_accepted_quota_still_reach_it` seeds the count at
2147483644 and charges four copies twice: the first leaves the count at 2147483647, the second ends all four. With
`wrapping_add` restored it fails with the count at -2147483648. `copies_in_one_batch_all_see_the_old_count` pins the
partition below the boundary. `queue_oracle`, `queue_behaviour` and `run_flow_oracle` pass unchanged.

**Other counters on the path, checked.** None can wrap this way. Progress's `running` goes up when a copy starts and down
on its result, so a repeating uncompleted copy leaves it where it was; `total` and `done` don't move on a result that isn't
final. `retry_workers` gains an entry per return and loses it when the copy is taken again; it holds entries, not a count.

**Limits.** With a quota of 2147483647, which D47 refuses, a saturating count would now end the loop at that count where a
wrapping one would not. Neither is reachable in practice: 2^31 returns of one test.

## S-x32-1 (nit): `durationMillis` read as a cap

**Finding.** `docs/migration.md` said batches hold "up to `durationMillis`", and D50 in `docs/deviations.md` "up to its
configured `size` and `durationMillis`". `fixed_size` adds a test and then checks the batch's expected duration, as Kotlin's
`FixedSizeBatchingStrategy` does, so the test that reaches the limit is kept: two 4 s tests make an 8 s batch with a 6 s
limit.

**Fix.** Both places now call it the point where a batch stops, with that example. The algorithm is unchanged. CHANGELOG and
PLAN already say "counts against", which is accurate.

## What was run

- `cargo fmt --check`; `cargo clippy -p marathon-core -p marathon-analytics -p marathon-cli --all-targets -- -D warnings`.
- `cargo test -p marathon-core -p marathon-analytics`: 69 test binaries, 764 passed, 0 failed.
- `cargo test -p marathon-cli`, the one crate that depends on `marathon-analytics`: 48 passed, 0 failed.
- `scripts/check_plan_dag.py`.
- Not run, as the brief says: `scripts/check.sh`, `scripts/remote-check.sh`, the workspace suite, and the tests of the
  vendor crates, which depend on `marathon-core` but not on the guard's close or the count's behaviour below 2^31.

## Incidental bugs

- PLAN §3.11's D51 row still says an empty filtered run "logs a WARN"; since x36 it is an ERROR line, as D51's text in
  `docs/deviations.md` says. Not changed here.
- S-x34-1 and S-x30-1 are flaws in our own code, but they have no `docs/found-bugs.md` rows: other branches in flight add
  rows there, and a U number picked here could collide. The orchestrator can number them at merge.
