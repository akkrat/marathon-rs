# X34: Graphite failures, the C38 `None`, and the end of a run (D51)

The project owner approved this on 2026-11-07. D51 is a new deviation in PLAN §3.11. No configuration field, CLI option or
exit code changed, and metrics from a working backend are unchanged.

## What was wrong, checked against the code

- **The hang.** A tracker error reaches `progress.rs:357-364`, ends the queue actor, and the pool logs "queue is closed …
  dropped". Nothing then ends the run. Reproduced: with the guard taken out, `backend_failure_run` fails at once with
  `success.json: the run did not end`.
- **Kotlin.** `BasicGraphiteClient.sender` turns an `IOException` from `Socket(host, port)` into `GraphiteException`, and
  `GraphiteTracker` has no catch, so carbon being down throws there too. Only a reset during a write is Rust-only, because
  `PrintWriter` swallows those. U167's and C62's text read as if Kotlin carries on, which holds only for a write error, not for
  the realistic fault of carbon being down. Both are corrected.
- **A stalled send.** Track's `test` is awaited by the queue actor, so a send's time is the queue's time. A send is three short
  lines, which fit a socket's buffer, so what really stalls is the connect.

## The fix

### Where the failure stops

`BackendGuard` wraps the remote backend in `TrackerFactory`. That is the narrowest place that sees every backend through
production wiring:

- The report generator never fails on `track`, so it needs no guard.
- `Tracker` and the queue are unchanged, so "a handler failure ends the queue" stays as it is, for x31 and x32 to work in.
  `handler_failure_ends_the_queue` passes unchanged and is recorded as U168's pin.
- `SocketSender` and `GraphiteTracker` still return their errors, so `carbon_reset_errors_propagate_…`,
  `refused_carbon_connection_raises_graphite_exception` and `invalid_carbon_port_fails_at_the_write_boundary` still compare with
  their JVM rows.

Cancellation passes through the guard. A failed `close` passes through too, for `on_finish` to report.

### The limits, and why these values

| Limit | Value | Why |
|---|---|---|
| Carbon lookup and connect | 10 s | Outlasts one dead DNS server (5 s by default) and three lost SYNs (1, 3 and 7 s) |
| Carbon write and shutdown | 10 s | Three lines fit the socket's buffer, so a working carbon never comes near it |
| Any send, any backend | 30 s | Above carbon's 20 s in all, and below the InfluxDB clients' 60 s per step |
| A failure that counts as slow | 1 s | Separates a refusal or reset, which cost nothing, from a timeout or an unreachable host |
| Skipped after a slow failure | 300 s | Keeps the worst send under a tenth of the queue's time; only history for that window is lost |

Worst cases for the queue actor:

| Fault | Cost |
|---|---|
| Carbon refuses | About a millisecond per test, tried every time, so metrics resume as soon as it returns |
| Carbon's packets are dropped | 10 s in every 310 s, about 3%, or about 4 minutes over a 2 hour run, spread out |
| Carbon stalls after connecting | At most 20 s in every 320 s |
| InfluxDB 2 stalls | At most 30 s in every 330 s |
| A backend that answers just inside the limit | Up to 30 s per test, with no cool-off, because it did succeed. Not expected of carbon, since a kernel completes the connect and buffers the write |

### InfluxDB, checked and aligned

- **InfluxDB v1** writes to a channel served by a worker, as Kotlin's batch processor does. Its worker swallows send failures.
  `write` fails only once the client is closed, and the guard covers that. It blocks only if the channel of 1000 points fills,
  which the 30 s limit now bounds.
- **InfluxDB 2** differs from Kotlin: each test's write is a synchronous POST with 60 s per step, where Kotlin batches off the
  caller. A stalled server could hold the queue for minutes per test, and its catch logged one line per test. The tracker now
  returns the failure to the guard, which is the only thing that can see how long it took. The text
  `Failed to write data point to InfluxDb2` is gone, replaced by the guard's line.
- **Closes** are not bounded here. InfluxDB v1's close makes its one last attempt per buffered batch, at 60 s per step, as
  Kotlin's does. It runs once, at the end, and nothing waits on it but the end of the run.

### Logging once per kind

A kind is the failure's class and message, at most eight per run, because a server's text can name a test. The close adds one
line with the totals. Those are "up to" for skipped results, since the guard can't see whether a skipped result would have been
sent at all.

### C38

`has_no_datapoint` matches a line whose whole value is exactly `None`. `parse_line` is unchanged, so its JVM rows still compare.
The allowance in `query_and_graphite_parser_jvm_rows` checks that the data source skips exactly the rows ending `|None`, and those
rows must carry the JVM's error. `50,None`, `none` and `None ` still fail the query. The debug line uses `MetricsProviderImpl`'s
logger, which is already registered, so the logger registry doesn't change.

### The end of a run

`on_finish` runs each step, logs each failure and returns the first. A console flush counts as a step, because x33's lead was that
a broken stdout could skip cleanup. Inside the reports' close, `ExecutionReportGenerator` and `DelegatingTrackerInternal` do the
same, and log only failures after the first, since `on_finish` logs that one.

- The guard stays set before the closes (C25), so a later attempt is a no-op, as `exceptional-finish-fail` still shows.
- A repeated explicit close of the generator still regenerates (C11).
- Cancellation stops the later steps as it did, since they would only be cancelled too.
- Reporters run in their old order. A later one that needs an earlier one's output is unaffected unless that one failed, and then
  it can fail too and be logged.

`run_flow_witnesses` keeps every JVM `finish-fail-*` row. `d51_trace` adds only the slots after the one that failed, and
`d51_extra_records` removes only named records, which must be present. `the_d51_allowances_hide_nothing_else` holds the negative
cases.

### The empty-run warning

The WARN is logged where the run decides not to execute, naming each filter's removal count. `parse` doesn't warn:
`filtered_empty_parse_writes_an_empty_test_list` and `test_parse_command_should_create_a_file_with_zero_tests` are unchanged. An
all-removed run still exits 0, since ending it with an error is a policy decision for the owner.

## Verified, and how

| Check | Kind | Result |
|---|---|---|
| A failing backend, whole run through the real scheduler: `success`, `filtering-2`, `disconnect`, `uncompleted-1` | Stub vendor on the virtual executor | Completes, with one warning, and the same `raw.json`, value and finish time as a control |
| The same with the guard removed (temporary) | Revert proof | Fails: `success.json: the run did not end` |
| Real `SocketSender` to two addresses that drop packets | One-off real run on this Mac, not kept as a test because it depends on the network | Both returned `connect timed out after 10000 ms` in 10.003 s |
| Stalled write, 64 MiB to a peer that never reads | Real sockets, in `clients.rs` | Times out at its limit with the expected text |
| Refused carbon through the real `GraphiteTracker` and guard, 5 tests | Real sockets, in `clients.rs` | Two log lines: the failure and the totals |
| Carbon history against the JVM exchange rows (`history.rs`) | Real sockets | Unchanged, so a working backend's bytes are the same |
| Once per kind, the 8 kind cap, cool-off, retry after it, no answer at all, cancellation, a healthy backend | `event_pipeline.rs` | Pass |
| Every close running, in order, with the first failure returned | `event_pipeline.rs`, `reporters.rs`, `allure_reporter.rs`, `run_flow_witnesses.rs` | Pass |

## Not verified

- A real carbon or InfluxDB server. These are real sockets and the JVM's recorded exchanges.
- A whole JVM run with carbon down. That Kotlin's queue actor ends comes from reading the source and the JVM queue pins; that the
  whole run then hangs was seen only in the port.
- `a_carbon_that_never_accepts_times_out_on_connect` doesn't reach the connect limit on macOS, where a full accept queue refuses
  at once. It should on Linux, where the SYN is dropped, but that wasn't run here. The one-off above is the evidence for the
  connect limit on this Mac.
- A whole run with a real stalled carbon. The guard's skip is tested with a scripted backend and limits set per test.

## Incidental bugs

- **U168.** Any other queue handler failure still ends the queue and hangs the run, including a console flush failure in
  `test_started` or `test_ended` (`progress.rs:330,347`). Recorded, not fixed, as the brief asks.
- InfluxDB 2's write is synchronous here, where Kotlin's is batched off the caller. D51 bounds what that costs, but each test still
  makes one POST. Left as it is.
