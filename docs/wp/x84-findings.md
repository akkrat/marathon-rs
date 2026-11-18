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

The FIFO holds 1,000 waiting events plus one being delivered. This matches the size of a burst already accommodated
by the InfluxDB v1 batch/channel, at one event per point. The queue adds the same fixed headroom for Carbon and
InfluxDB v2. Every new arrival to a full FIFO is dropped, and admission counts it without waiting for capacity.
Each backend close emits the count once if positive.

Report generation retains its place before backend close. The close seals admission, sets one absolute D51 deadline
and joins the delivery task. The task drains accepted events and runs the backend's close within that deadline.
A completed backend close error still reaches the existing finish-error handler. On expiry the task drops its
pending operation and emits one WARN. The last owner of the InfluxDB v1 client cancels its worker even if delivery
expired before reaching `shutdown`. This also covers losing the last pipeline owner during teardown. The D52
signal path retains its outer process deadline.

The strategy metrics provider still owns its own history client and queries, including D71's decision for
unavailable history. D86 adds no configuration or CLI fields.

## Verification

- The production scheduler and stub device start a second batch while a real Graphite tracker waits at the
  Carbon socket-adapter boundary, and while a real InfluxDB v1 client has all 1,000 channel slots occupied
  behind a held loopback HTTP response. Both finish with the two passing local results. The InfluxDB test
  observes both points at the server after release. The socket boundary makes the Carbon stall deterministic:
  its normal three-line payload fits in a loopback socket buffer even when the peer never reads.
- The overflow witness holds the oldest event in flight, submits 1,005 more, and observes 1,001 deliveries in
  FIFO order, 1,006 local results and a single WARN counting five drops. Restoring inline tracking made both
  next-batch witnesses stop at one batch and the overflow witness lose the held send to D51.
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

Formatting, changed-crate Clippy, and the core, analytics, logging and CLI test suites passed, including CLI
shutdown, configuration differential and whole-vendor differential tests. The plan DAG and differential registry
validated, and all 107 Python evidence tests passed with the built YAML helper. The orchestrator owns the
workspace-wide and remote gates. Real analytics services were outside this verification.

## Incidental bugs

The checkout of the frozen Kotlin source differs from the committed source inventory and parity matrix. The
inventory comparison shows two missing files and five changed rows. The parity checker reports missing
Apple code-coverage tests. The validators and CSVs match the base of this work package.
