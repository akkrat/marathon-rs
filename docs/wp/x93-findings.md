# x93 — Bounded Scheduler drains and interrupted iOS artifact finalization

## Design

D84's pool barrier and queue `IsEmpty` raced completion against the actor's job. A pending handler could keep both
pending indefinitely. A timer alone would have moved the hang to the final actor join because ordinary job
cancellation is cooperative. The device subscription had the same problem when a tracker callback was pending in
`device_connected`. The protected run joins that subscription even when its task fails before `settle`.

After device cleanup, all pools begin draining together, with one 65-second deadline on the Scheduler clock. D67's
60 seconds exceeds D51's 30-second send allowance. The extra five seconds come from D84's core margin. Each pool forwards
its results before its own queue gets a barrier. The mailbox is sealed when its barrier is sent, so late messages
cannot start a new handler behind it. The entire barrier phase has 65 seconds regardless of pool count. A pool's
existing `IsEmpty` query gives way to the Scheduler's ordered drain.

On expiry, the Scheduler logs one WARN naming the actor and pool and interrupts the receive future. This drops any
queue progress guard it held. The actor completes with the run's cancellation, then its mailbox is released without
dispatching more messages. Actors that completed their barriers are cancelled normally. If a pool misses the deadline,
its queue is interrupted only if it still has work to drain.
In production, result and device-connection tracking run off core. Their dispatch is scoped to the waiting handler,
including the tracker pipeline's second dispatch, so a callback suspended at interruption can be dropped. Ordinary
device cancellation still lets the vendor clean up. The cancelled subscription has its own 65-second allowance.

The report generator retains events already committed. A held result that reaches it before the barrier expires is
included. A result interrupted before commit can be lost along with the timed-out actor's mailbox. A timeout never
records a run failure, so the preceding channel or actor failure and U215's cancellation precedence still hold.

The iOS artifact processor belongs to the run and can still have results from batches that ended before devices were
sealed. Starting its timer after the device window preserves a chance to retrieve them. An interrupted run has a separate
70-second allowance, matching D93's default device window. The first 30 seconds drain work and the remainder reserves
D91's 40 seconds for cooperative cancellation and joins. On both paths, worker joins and the final receipt share the
same cancellation deadline. Routine artifact finalization now has at most 360 seconds of waits (300 + 60), down from
420 seconds (300 + 60 + 60). On expiry the existing processor ledger identifies undelivered bundles before the provider
continues to disposal. D54's caller-independent receives retain their behavior.

Only an internal run failure or owner cancellation selects the shorter iOS allowance. A suite that completes with
failed assertions uses routine termination, as does D52's signal path under its separate deadline. The first
termination attempt owns the provider's retained operation. A later caller joins it, including cancellation that
arrives after an ordinary termination has started.

## Finish wait audit

The Scheduler seals every device and starts any D92 vendor cleanup before cancelling the actors. It joins devices
concurrently, using the D92 cleanup job when the device returned one and the D93 receipt for Apple devices. The shared
D94 barrier deadline starts after those joins. D52 also seals devices before cancelling the Scheduler and run jobs.

D93's per-device seal closes command admission. Its first cause determines whether a refusal is an orderly
cancellation (run stop or provider termination) or a device loss (disposal). A command handed to the transport
registers a receipt under the same gate. Declaring an SSH connection dead can wake the caller while that receipt
stays live. The peer's channel CLOSE, an exec refusal, or the end of the SSH session task can settle it. iOS recovery
uses the device's sticky disposal verdict for lost devices within the original window.

The provider drains artifacts before disposal, and the command seal leaves artifact pull admission open. Disposal
closes pull admission as it snapshots the pull receipts. A live pull at the snapshot or unconfirmed pull cleanup
quarantines the device and skips the iOS lifecycle and lease release. On run stop or termination, disposal requests
cancellation of live pulls. The transfer owners continue the D91 cleanup without adding a wait to disposal.

For iOS simulators sealed at run stop, the default bounded phases can total **215 seconds** (75 for the Scheduler's
device joins, 65 for all pool and queue barriers, 70 for interrupted artifact finalization, and 5 for the lifecycle
floor after the device window has expired). The release wait uses the original window. This total covers the bounded
phases and excludes other finish steps shown below. A simulator first sealed for disposal or provider termination
gets a fresh device window and uses the ordinary lifecycle and lease-release waits.

| Wait | Governing bound or completion condition |
| --- | --- |
| Stop admission and seal devices | Synchronous on core. All devices are sealed and any D92 cleanup tasks are started before cancelling actors. D52 seals devices before it cancels the Scheduler and run jobs. |
| Device actor and vendor cleanup or `join_cancelled_work` | Concurrent waits against each device's bound from the start of the stop, plus the 5 s core margin. D93 allows 70 s per Apple device, for 75 s in core. D92 allows 60 s by default on Android, for 65 s in core. A vendor returning `None` has no aggregate device limit. |
| Pool's existing `IsEmpty` query | Waits for the queue or ordinary cancellation in a running pool. Starting the drain releases the query immediately. |
| Scheduler's `Drain` and `IsEmpty` barriers | One shared 65 s deadline after device cleanup. All pool barriers are sent together. Each pool's queue barrier follows its pool barrier. |
| Final pool and queue job joins | Sending a barrier seals its mailbox. Only actors that still have work at the deadline are interrupted. Pool supervision owns device actors and return timers separately. The final joins depend on core polling and synchronous destructors, with no remaining handler future or independent timer. |
| Run task, subscription, `settle` and protected finish joins | The listener stops on cancellation or D94's 65 s watchdog. That allowance runs alongside the device joins once the Scheduler's stop starts. If `settle` starts a stop for leftover pools, it first joins the listener. `run_under` joins the protected finish, including the operations below. |
| Device channel checks in `settle` and `settle_after_termination` | Poll the current backlog and one terminal read without awaiting an empty channel. New arrivals cannot keep the synchronous drain busy. |
| Core console flushes, serial batch barriers and output pump | The CLI sink enqueues output without waiting for its writer. D67 gives the final CLI flush and writer drain one shared 60 s after core finishes. Injected sinks have no independent core limit and may also hold a batch barrier. |
| History analytics close | The refresh joins for up to 3 s (D34). Native history sources then close under their own I/O rules. Influx has 60 s connect/read/write timeouts per operation and no D51 total close deadline at this call site. Stub and disabled history close immediately. |
| iOS monitoring, updates, recovery and event joins | Monitoring and recovery are cancelled before joining. Events publish to the local channel. Worker operations have command and connection deadlines. Provider lifecycle locking and monitoring, setup and recovery joins have no provider-wide timer. For a lost device, recovery uses disposal's sticky verdict, which already covers its sessions. |
| iOS artifact queue close | Internal failure or owner cancellation gets 30 s drain plus a shared 40 s for worker joins and the final receipt. Routine termination gets 300 s plus one shared 60 s cancellation allowance for a total of 360 s, down from 420 s. Undelivered callbacks and ledger processing run synchronously on the vendor runtime. |
| iOS and macOS disposal waits for device work | Disposal uses D93's existing 70 s device deadline, including receipts for all handed-off commands. Declaring an SSH connection dead leaves its command receipts live until the channel or session ends. A device first sealed for disposal or provider termination gets a fresh window. Active devices are disposed of concurrently. |
| Apple disposal checks for artifact pulls | The disposal snapshot closes pull admission. A live or unconfirmed pull prevents the lifecycle and lease release. On run stop or termination a live pull is asked to stop, with its own 40 s D91 cleanup budget and no additional disposal wait. |
| iOS on-dispose lifecycle | Requires confirmed device termination. After a run stop it gets the remaining D93 window or the 5 s floor, whichever is greater. At expiry the device is quarantined and the retained command is cancelled so its executor can finish cleanup. Outside a run stop the ordinary per-command limits apply. A simulator state wait issues at most 30 queries with one-second pauses. |
| Apple lease release, including replaced devices | D93 quarantines synchronously at the decision and skips release of a quarantined lease. For a device sealed at run stop, any release wait ends at its original deadline. The transport close can continue in the factory's task. Outside a run stop the release path has no aggregate timer. |
| Android provider termination | D92 shares the per-device deadline across disposal, socket closure and marker cleanup. Devices first sealed at termination get their own window. Provider monitor/setup joins run alongside disposal and have no aggregate timer. Staging removal and the provider-owned ADB restart reap also have no aggregate timer. |
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
| `pool_scheduler.rs::several_stalled_queues_share_one_drain_deadline_d94` | Three pools stay stalled and a fourth commits at 64 s. The run returns with the first cause at 65 s and there is a warning for each of the three stalled queues. |
| Stub provider entry-point counts in `run_supervision.rs` | A suite returning `Ok(false)` from failed assertions and a signal each use `terminate` once. A Scheduler failure and owner cancellation before termination each use `terminate_after_stop` once. |

The original six witnesses each passed 50 isolated runs and one further run with 12 CPU load workers. On the scratch
x92 checkout the stalled queue and subscription exceeded the hang guard. Artifact cancellation started at 300 s. The pool query
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

In review, the multi-pool witness was still pending at 65 s on `f65be777`. The shared-deadline witness and the four
termination-routing witnesses each passed 50 isolated runs and one further run with 12 CPU load workers.
Formatting and changed-package Clippy passed. Core, test-support and CLI suites passed 1,046 tests with eight ignored,
including the native vendor differential. The iOS provider and disposal suites passed 30 tests. The plan graph and
offline differential evidence checks passed.

On the rebased x92 head `3c36e19c`, the 26 D93 and nine D94 witnesses passed in their respective filtered runs.
The D94 witnesses, two routing checks under D84, and four D93 seal, join and lifecycle witnesses each passed
50 isolated runs and one more with 12 CPU load workers. The plan, source inventory, parity matrix, fixture and
offline differential evidence checks passed, including 110 comparator tests.
After building the native `yaml_tree` helper, formatting, Clippy for all seven affected crates, and their test suites
passed. The integrated run covered core, Apple, iOS, macOS, Android, CLI and test support, with 2,475 passed,
zero failed and 34 ignored.

After rebasing onto `b45d2352`, the focused reruns passed 49 D93 witnesses, nine D94 witnesses and both D84
termination routing checks. The nine D94 witnesses and the two routing checks each passed 50 isolated runs
and one more with 12 CPU load workers. The plan, source inventory, parity matrix, fixture and offline differential
evidence checks passed, including 110 comparator tests.
After building the native `yaml_tree` helper, `cargo fmt --check` and Clippy for all seven affected crates passed.
Their suites passed 2,497 tests with zero failures and 34 ignored, including the CLI configuration and native
vendor differential suites.

## Golden diffs and incidental findings

No golden updates. The healthy control and the existing differential tests cover reporting and command parity.
The new iOS witness drives the provider's interrupted termination entry point with a fake simulator. Real workers and
the Linux integration gate are outside this verification.

The audit found additional extension and vendor boundaries without an aggregate timer. They are listed in the table
with their actual limits. A D54 receive can still be running when the artifact processor's grace expires. The processor
records its bundle as undelivered and releases the wait. D93's disposal snapshot still accounts for the live pull receipt
and quarantines the device if cleanup is outstanding. The transfer owner confirms remote cleanup.
