# WP x84 — Remote telemetry off the batch-completion path

## Evidence

At 3e53e6a4, `QueueActor::on_batch_completed` awaited the progress callbacks before releasing the batch.
`PoolProgressAccumulator::test_ended` awaited `Tracker::test`, and `TrackerFactory::create` put the remote
subscriber inline after the local report collector. The same call through progress and queue completion is present
in the Kotlin marathon at e47352f2.

- `GraphiteTracker` sends three lines per eligible result through a fresh Carbon TCP connection.
  `SocketSender` allows 10 s each for connect and write. A hypothetical 5 ms per send adds 0.5 s to a batch of 100.
- `InfluxClient::write` awaits a send to the 1,000-point Tokio channel while holding its close gate.
  With the batch worker stalled in HTTP and the channel full, the next enqueue waits for D51's 30-second send limit.
  InfluxDB v2's blocking-write API also awaits the HTTP response for each result.
- `BackendGuard` put a slow failure into a five-minute cool-off. U174 already bounded a final close, including an
  InfluxDB flush, with one 30-second allowance and cancellation of the stopped flush.

## Design and invariants

The report collector receives the original event inline. The same wall-clock draw, progress transition and local
execution-time observation precede batch release. The configured writer's single vendor task owns delivery and
serializes admitted test events in FIFO order. The backends keep their own eligibility rules and formatting.

The FIFO holds 1,000 waiting events plus one being delivered. A backend delivering about 34 events per
second can drain the waiting backlog within D51's 30-second close allowance. The final flush also uses that
allowance. A larger queue delays backpressure and can move the loss to shutdown. The bound applies equally to
Graphite and both InfluxDB versions. Graphite previously allowed sends from different pools to overlap.
The single worker serializes them, which can bring backpressure forward on a slow link. At a hypothetical
5 ms per send, a single worker handles about 200 results per second.

There is no capacity wait while an event fits. When the FIFO fills, callers wait for a permit from the FIFO's
semaphore. A waiter keeps its place across notifications and rechecks admission and cool-off before sending
with a reserved permit. Each wait has a single 30-second limit measured from the first full check. A slow send
failure or the first admission expiry starts the same five-minute cool-off and wakes waiting callers. New
admissions skip immediately during that window. Healthy sustained overload slows producers as capacity runs
out and delivers every accepted event while the backlog and close finish within their deadlines.

Admission records four rejection causes: wait expiry, active cool-off, admission closed, and worker stopped.
The first rejection logs a WARN during the run. One WARN on release of the last tracker owner gives the total
and the per-cause counts. Its lifetime includes events submitted during close and after close returned. The CLI
collects those final records through its late output queue. D52 can exit the process with owners still alive,
so the first warning is available even when that exit cuts the final total short.

Report generation retains its place before backend close. Close seals admission, sets one absolute D51
deadline, wakes blocked callers and joins the delivery task. A completed backend close error still reaches
the existing finish-error handler. On deadline expiry the task drops its pending operation and emits one WARN.
Already queued events can also be lost if a send fails or is cancelled, the worker skips a queued send during
cool-off, or the worker panics. The last owner of the InfluxDB v1 client cancels its worker even if delivery
expired before reaching `shutdown`. Its existing retry-buffer limits and D52's outer process deadline still apply.

The strategy metrics provider still owns its own history client and queries, including D71's decision for
unavailable history. Lost observations can affect later runs' flakiness copies and duration-based sorting or
batching, including when a test falls back to a default. D86 adds no configuration or CLI fields.

## Verification

- The production scheduler and stub device start a second batch while a real Graphite tracker waits at the
  Carbon socket-adapter boundary, and while a real InfluxDB v1 client has all 1,000 channel slots occupied
  behind a held loopback HTTP response. Both finish with the two passing local results. The InfluxDB test
  observes both points at the server after release. The socket boundary makes the Carbon stall deterministic:
  its normal three-line payload fits in a loopback socket buffer even when the peer never reads.
- A slow, healthy worker is permitted one send at a time while the FIFO stays saturated. Each permit
  frees one blocked admission. All 1,006 events reach the backend in order and all 1,006 results reach the
  local report. The implementation at `2c0187a6` advanced the producer before a permit was released.
- The production scheduler also runs two 1,002-result batches and a final test with Carbon held. The full
  metrics queue holds the first batch until the only attempted send reaches its D51 limit. Both later batches
  proceed during cool-off, and all 2,005 results are recorded locally. The implementation at `2c0187a6` started
  the second batch before the held send reached its deadline.
- Paused-time admission tests cover expiry starting cool-off and waking a caller with a later deadline,
  close waking a blocked caller and counting late submissions after it returns, and an actual vendor-task
  panic waking callers and counting later submissions. Each pins the first WARN and the single total with
  the expected cause counts. Reinstating the original try-only path with the paused-clock test seam
  failed all three witnesses. Suppressing just the late and panicked-worker rejection counts failed the
  two corresponding witnesses.
- Paused-clock coverage checks expiry both during drain and during close after seven of ten seconds spent draining.
  Extending the deadline by another allowance made the witness observe 20 s in place of 10 s. Loopback socket
  tests also cover the U174 shutdown timeout and dropping the last client before shutdown starts. Removing the
  drop cancellation made the latter witness reach its 60-second hang guard with the socket still open.
- The Graphite and both InfluxDB client comparisons run the fixture events through the queued pipeline and
  still compare ordered traffic against the immutable JVM recordings. Whole-vendor differential runs use disabled
  analytics. The analytics fixtures compare traffic within each backend, so a change in its order relative to
  reports has no comparator effect. The factory tests retain the reporter sequence and remote-close position,
  with the remote send permitted on either side of report generation.
- The diff contains no golden or recording changes.

The initial implementation passed formatting, changed-crate Clippy, and the core, analytics, logging and
CLI test suites, including CLI shutdown, configuration differential and whole-vendor differential tests.
All 107 Python evidence tests passed with the built YAML helper.

Round one passed formatting, changed-crate Clippy, the plan DAG and differential registry checks, and 165
targeted tests covering the admission and delivery pipeline, backend clients and JVM wire fixtures, history
factory and provider, backend failure runs, and CLI shutdown. The orchestrator owns the workspace-wide and
remote gates. Real analytics services were outside this verification.

## Incidental bugs

The checkout of the frozen Kotlin source differs from the committed source inventory and parity matrix. The
inventory comparison shows two missing files and five changed rows. The parity checker reports missing
Apple code-coverage tests. The validators and CSVs match the base of this work package.
