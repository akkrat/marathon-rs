# WP x124 — Remote analytics admission and Carbon connections

## Evidence

- **U284.** At the base, `SocketSender::send_data` resolved, connected, wrote and shut down its socket for
  every eligible result. The Kotlin marathon at e47352f2 also opens a connection per send.
  The loopback witness sends eight payloads and checks that one accept receives all eight, byte-for-byte
  and in order. The audit's estimate of 12% CPU and 0.1 s wall time was not measured on a large run.
- **D105.** At the base, batch completion awaited the progress accumulator's tracker call. The report
  collector ran first, then the remote admission could wait while the 1,000 waiting slots were taken.
  In a scratch checkout of the base, the existing scheduler witness with a held Carbon socket adapter and
  the production 30-second limit took **32.28 s**. The next batch was blocked until the send limit released it.
  This is a measured stall, not an estimate from the queue capacity. The Carbon adapter deliberately holds
  the send because a real three-line payload fits in a loopback TCP buffer even if the peer stops reading.
- The new scheduler witness uses the unmodified InfluxDB 2 client and a loopback HTTP peer. A batch returns
  1,002 results while the first write response is gated, exceeding the FIFO even if the writer has already
  dequeued one. The next batch starts with that response still held, and all 1,003 passing results reach
  the local report. The base with the same witness has only started one batch at that checkpoint. In a second
  scratch baseline run, a drop notification around the real client's write marked the production send deadline.
  The first response was still held **29.79 s** after the harness observed the request, the next batch then
  started, and the run finished in **30.82 s** with all local results. The HTTP peer never sent a response
  for that write.

## Design and invariants

A configured Graphite tracker has one transport and one delivery worker. The worker takes the cached socket
out of a short mutex before any await and returns it only after a complete write. On a known FIN or reset it
reconnects before writing. If a write to a reused socket fails before accepting any bytes, it can reconnect
once and use the remaining write budget. A partial write, timeout or cancellation drops the connection without
replaying the payload. Fresh connection failures retain D51's classification and wording. Each connect retains
its 10-second budget, and the write attempts share 10 seconds. Close drops the cached socket after the drain,
and a cancelled delivery drops the in-flight socket. The plaintext Carbon protocol has no application ACK.
A peer that disappears concurrently with a write can still lose bytes accepted by the local kernel, as
under D51. The changes do not claim exactly-once delivery.

Admission has no asynchronous capacity wait. A full queue rejects only the incoming event and does not start
cool-off. The existing admission and backend-state locks serialize the enqueue with close and cool-off, and
nothing awaits under either lock. Accepted events remain FIFO. The first rejection warns and final release
summarizes counts by cause. Actual slow sends still trigger cool-off and shutdown still shares the 30-second
drain-and-close deadline. Local reports, strategy queries and exit status keep their existing paths. A slow
backend can receive fewer metrics, which can affect history-dependent decisions in later runs.

## Verification

In the scratch worktree, the unmodified base failed the saturation witness with one batch started instead
of two, failed the paused admission witness on its first poll, and sent only one of the eight payloads over
the first Carbon connection. Returning an error instead of reconnecting to a known reset also failed the
reset witness. The overload test gates each delivery, checks that the original 1,001 metrics and the first
post-saturation metric reach the backend in order, and checks that all 1,007 results are reported locally.
The same test on the base completed none of the five submissions while the queue was held.
Close and worker-stop rejection tests also use explicit gates. Each of the seven new or rewritten concurrency
witnesses passed 50 local runs plus one under CPU load.

The loopback Carbon transcript check now compares the concatenated payloads on a single stream against the
unchanged JVM recording. There are no golden or recording changes and no differential allowance is needed.
The tests affected by D105 are native scheduler/admission witnesses, not Kotlin parity-matrix rows.

Formatting, Clippy for core and analytics, the plan DAG and parity-matrix checks, and the differential registry
and evidence checks (117 Python tests) passed. The combined core, analytics and CLI test run passed every target
except one probe that still expected D86 backpressure. After updating that probe, its full 24-test target passed.
The CLI configuration and vendor differential suites passed. Workspace-wide and remote gates are left to the
orchestrator. No production analytics services or physical devices were used.

## Incidental bugs

None found in scope.
