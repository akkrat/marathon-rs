# x93 — Bounded Scheduler drains and interrupted iOS artifact finalization

## Design

D84's pool barrier and queue `IsEmpty` raced completion against the actor's job. A pending handler could keep both
pending indefinitely. A timer alone would have moved the hang to the final actor join because ordinary job
cancellation is cooperative. The device subscription had the same problem when a tracker callback was pending in
`device_connected`. The protected run joins that subscription even when its task fails before `settle`.

Each barrier gets 65 seconds from submission on the Scheduler clock. D67's 60 seconds exceeds D51's 30-second send
allowance. The extra five seconds come from D84's core margin. Pools retain their creation order and each pool forwards
its results before its queue gets a barrier. Each actor has its own allowance, so a stalled actor cannot exhaust another
pool's time. The worst case for the barrier phase is 130 seconds per pool. During the stop, a pool's existing `IsEmpty`
query gives way to the Scheduler's ordered drain. This prevents one stalled queue from holding up both barriers.

On expiry, the Scheduler logs one WARN naming the actor and pool and interrupts the receive future. This drops any
queue progress guard it held. The actor completes with the run's cancellation, then its mailbox is released without
dispatching more messages. The final cancellation interrupts any receive after an acknowledged barrier before joining.
In production, result and device-connection tracking run off core. Their dispatch is scoped to the waiting handler,
including the tracker pipeline's second dispatch, so a callback suspended at interruption can be dropped. Ordinary
device cancellation still lets the vendor clean up. The cancelled subscription has its own 65-second allowance.

The report generator retains events already committed. A held result that reaches it before the barrier expires is
included. A result interrupted before commit can be lost along with the timed-out actor's mailbox. A timeout never
records a run failure, so the preceding channel or actor failure and U215's cancellation precedence still hold.

The iOS artifact processor belongs to the run and can still have results from batches that ended before devices were
sealed. Starting its timer after the device window preserves a chance to retrieve them. An interrupted run has a separate
70-second allowance, matching D93's default device window. The first 30 seconds drain work and the remainder reserves
D91's 40 seconds for cooperative cancellation and joins. Both cancellation-stage joins use the same absolute deadline.
Previously the ordinary 300 seconds could be followed by two 60-second waits in code, although the second wait usually
completed immediately once workers or residual entries finished. On expiry the existing processor ledger identifies
undelivered bundles before the provider continues to disposal. D54's caller-independent receives retain their behavior.

The protected finish of a failed run uses the shorter iOS allowance. Ordinary completion, including a suite whose tests
ran and failed assertions, uses the ordinary allowance. The first termination attempt owns the provider's retained
operation. A later caller joins it, including cancellation that arrives after an ordinary termination has started.
D52 still starts reports first and bounds the signal path independently.

## Finish wait audit

The table covers the waits from stopping the Scheduler through returning the run outcome. D92 is listed for integration.
The requested x92 base contains D93 and predates the independent x91 branch.

| Wait | Governing bound or completion condition |
| --- | --- |
| Stop admission and seal devices | Synchronous on core. Signals and mailbox state only. |
| Device actor and `join_cancelled_work` | Concurrent waits against the device limit plus the 5 s core margin. D93 allows 70 s per Apple device, for 75 s in core. D92 on x91 allows 60 s by default on Android, for 65 s in core. The x92 base and a vendor returning `None` have no aggregate device limit. |
| Pool's existing `IsEmpty` query | Waits for the queue or ordinary cancellation in a running pool. Starting the drain releases the query immediately. |
| Scheduler's `Drain` and `IsEmpty` barriers | 65 s each from submission, or early actor completion. Pools and queues are traversed in creation order, pool before queue. |
| Final pool and queue job joins | Interruption drops the receive future and seals its job. Pool supervision owns device actors and return timers separately. The final joins depend on core polling and synchronous destructors, with no remaining handler future or independent timer. |
| Run task, subscription, `settle` and protected finish joins | The listener stops on cancellation or D94's 65 s watchdog. A stop job uses the device and drain bounds above. `run_under` joins the protected finish, including its downstream operations below. |
| Device channel checks in `settle` and `settle_after_termination` | Poll the current backlog and one terminal read without awaiting an empty channel. New arrivals cannot keep the synchronous drain busy. |
| Core console flushes, serial batch barriers and output pump | The CLI sink enqueues output without waiting for its writer. D67 gives the final CLI flush and writer drain one shared 60 s after core finishes. Injected sinks have no independent core limit and may also hold a batch barrier. |
| History analytics close | The refresh joins for up to 3 s (D34). Native history sources then close under their own I/O rules. Influx has 60 s connect/read/write timeouts per operation and no D51 total close deadline at this call site. Stub and disabled history close immediately. |
| iOS monitoring, updates, recovery and event joins | Monitoring and recovery are cancelled before joining. Events publish to the local channel. Worker operations have command and connection deadlines. Provider lifecycle locking and monitoring, setup and recovery joins have no provider-wide timer. Recovery's lost-device session wait is 70 s. |
| iOS artifact queue close | Interrupted runs get 30 s drain plus up to 40 s for task/receipt joins. Routine finishes get 300 s plus up to 60 s. Undelivered callbacks and ledger processing run synchronously on the vendor runtime. |
| iOS and macOS disposal waits for device work | D93's shared 70 s deadline, with a fresh window only for a device first sealed by disposal. Disposal is concurrent across active devices. |
| iOS on-dispose lifecycle | Executes after confirmed device termination under the existing per-command timeouts and D30 transport cleanup. A simulator state wait issues at most 30 queries with one-second pauses. Lifecycle has no separate aggregate timer. |
| Apple lease release, including replaced devices | Quarantine is synchronous. The final user closes the shared transport. SSH disconnect and the cancelled keepalive join have no aggregate timer in the release path. |
| Android provider termination | D92 shares the per-device deadline across disposal, socket closure and marker cleanup. Its provider monitor/setup joins and staging removal have no aggregate timer. The x91 findings record that reaping a provider-owned ADB restart can still delay termination. On the x92 base the device joins are unbounded and marker cleanup uses the shell timeout. |
| Report snapshot and reporters | The in-memory snapshot is synchronous. Reporters run in sequence. Built-ins use blocking filesystem work off core. Report generation and custom reporter futures have no aggregate timeout. |
| Remote event analytics drain and close | D51/D86 share the configured backend allowance, 30 s by default. It follows file reporters. This is distinct from history close. |
| Usage close and already-started finish steps | Production usage close is an immediate no-op (D1). A second finish waits for the same completion receipt. The receipt inherits the running step's bound or D52's outer limit when a signal is waiting. |
| Signal hook, relay and diagnostics | D52 allows 24 s for the signal's report and cleanup sequence, including up to 10 s of report head start, and 1 s for the final console. A second signal exits immediately. Outside that path, the relay and diagnostics jobs wait for cancellation and finish without vendor cleanup. |

## Witnesses

The Stub run uses the real Scheduler, tracker pipeline and `raw.json` reporter. Its channel fails when the test2 tracker
callback starts. The stalled callback never returns. A companion runs the same callback for 64 virtual seconds, and
the healthy control checks the two recorded results and the ordinary completion time.

| Witness | Result |
| --- | --- |
| `run_supervision.rs::a_stalled_queue_is_interrupted_at_the_drain_deadline_d94` | At 65 s, the callback is dropped, one WARN names the queue, and the channel failure and committed test1 are preserved. |
| `run_supervision.rs::a_slow_queue_commits_its_result_inside_the_drain_deadline_d94` | At 64 s, the run retains both results and the same channel failure, without a timeout warning. |
| `run_supervision.rs::a_healthy_run_completes_in_its_usual_order_d94` | The healthy run succeeds at 11 s with both results, in order. |
| `run_supervision.rs::the_finish_interrupts_a_stalled_subscription_d94` | The finish drops a stalled connection callback after 65 s. Both committed test results reach `raw.json`. |
| `pool_scheduler.rs::a_pool_waiting_for_its_stalled_queue_does_not_spend_a_second_drain_window_d94` | The last device disconnects and the pool queries its queue. An ensuing channel failure takes 65 s and logs one warning for the queue. |
| `simulator_provider.rs::a_stopped_run_disposes_simulators_within_one_artifact_window_d94` | A pending artifact observes cancellation at 30 s, holds open past the deadline, and is reported undelivered at 70 s. The simulator is disposed and its lease released. |

Each witness passed 50 isolated runs and one further run with 12 CPU load workers. On the scratch x92 checkout
the stalled queue and subscription exceeded the hang guard. Artifact cancellation started at 300 s. The pool query
witness failed its 65 s completion check both on the base and with D94's timers while retaining the old nested wait.
The healthy and slow controls passed on the base and the branch.

The existing `buffered_disconnect_is_processed_after_cancellation` parity witness caught a cancellation check that
skipped buffered messages outside settlement. The final guard only applies after admission closes, and the witness
passes with it.

`cargo fmt --check` and changed-package Clippy with `--all-targets -- -D warnings` passed. The broad package run covered
core, Android, Apple, iOS, macOS, analytics, CLI and test support. Its only failure was the buffered-disconnect witness
above. The final core suite passed 922 tests with one ignored, and the three focused integration suites passed 96 tests.
The final CLI rerun passed nine tests across native vendor differential, late-pool admission and signal shutdown.
Plan, source inventory, parity matrix, fixture and offline differential checks passed, including 109 comparator tests.
The source checks used the release tree named in `docs/release-source.sha`. The batch leaves `scripts/check.sh`, the
workspace-wide test run and the Linux gate to the orchestrator.

## Golden diffs and incidental findings

No golden updates. The healthy control and the existing differential tests cover reporting and command parity.
The new iOS witness drives the provider's interrupted termination entry point with a fake simulator. Real workers and
the Linux integration gate are outside this verification.

The audit found additional extension and vendor boundaries without an aggregate timer. They are listed in the table
with their actual limits. A D54 receive can still be running when the artifact processor's grace expires. The processor
records its bundle as undelivered and releases the wait. Confirmation of remote cleanup remains the transfer owner's job.
