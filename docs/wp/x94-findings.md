# x94 — Finish deadlines

D95 continues the finish-wait audit in [x93](x93-findings.md). The work uses the existing vendor and backend
clocks and adds no Marathonfile setting. A timeout returns through the ordinary cleanup path and does not become
the run's failure. The Scheduler still owns D84's first cause.

## Design and invariants

- The connection factory removes the last lease from its generation before it starts closing the transport. A
  release while another user holds that generation just decrements its count. The final close owns disconnect,
  keep-alive and transport termination. Expiry drops the close and aborts the SSH session. The generation slot
  is released on transport end. Quarantine continues to retire the generation for future acquisitions while
  leaving sibling leases alone.
- The history provider reads the same D51 allowance used by the event backend. The deadline wraps the source
  future after D34's refresh join. An Influx writer close already aborts its worker if the close future is dropped.
- Android seals device requests when termination starts. Device disposal runs alongside monitoring and setup
  joins. Joining tasks are interrupted on expiry, including children and the process wait of a server restart.
  The shared factory closes after device cleanup or at the provider deadline. Staging admission closes before
  removal. A removal waiting to start checks whether the provider abandoned it.
- iOS refuses new registrations and setup work once termination is published. Monitoring, setup, recovery,
  the artifact drain, disposal and releases share the provider deadline. A setup that loses its waiter cannot
  silently release an unconfirmed lease. Lifecycle work uses the remaining device window and the provider
  deadline where applicable. D93's run-stop floor can use time within that provider deadline.
- A pull's receipt gets the device shutdown deadline even if rsync already owns the transfer and its caller
  has gone. Pulls keep progressing during an ordinary run. At run stop, the last 40 s of the device window are
  reserved for D91 cleanup, leaving up to 30 s of transfer at the default 70 s. Provider shutdown reaches
  retained handles whose actors have already gone.
- Report generation owns its progress timer in the same future as the reporter. The timer identifies the
  active reporter and continues to allow its work to finish. The D94 barrier timer was a shared future held
  through the final actor joins. It is now cancelled and dropped as soon as the barriers answer.

## Finish wait audit (after x94)

| Wait | Governing bound or completion condition |
| --- | --- |
| Admission and sealing | Synchronous on core. Sealing precedes actor cancellation, including D52's signal path. |
| Device actor and vendor cleanup or `join_cancelled_work` | Concurrent. Android has 60 s by default plus the 5 s core margin. iOS and macOS have 70 s plus the margin. A third-party vendor that supplies no allowance remains responsible for its own bound. |
| Pool queue query | Waits for its answer or cancellation. Starting the drain releases it. |
| Pool and queue barriers | One 65 s deadline after device cleanup. The barrier timer ends once every pool and queue answers. |
| Final pool and queue joins | The barriers seal the mailboxes. Remaining handlers are interrupted. The joins depend on core scheduling and synchronous destructors. |
| Run and subscription joins | D94 gives a cancelled subscription 65 s. That window overlaps device and barrier waits when the Scheduler initiates the stop. In a finish where settlement discovers leftover pools, the subscription can be joined before device stop starts. |
| Device channel settlement | Reads the current backlog and terminal state without waiting for further arrivals. |
| Console and output pump | Built-in console enqueues writes; D67 bounds final CLI flush and writer drain to 60 s. An injected sink may hold the batch barrier. |
| History analytics close | D34 refresh join up to 3 s, followed by the D51 allowance of 30 s by default for source close. |
| iOS provider termination | 70 s in total. Lifecycle lock, monitor, setup, recovery, artifacts, disposal and release use the same deadline. A stopped run reserves up to 40 s of remaining time for artifact cancellation; routine termination reserves up to 60 s. |
| Apple disposal and on-dispose lifecycle | D93's 70 s device window. Lifecycle requires confirmation and uses the remaining time, with D93's 5 s floor after a run stop inside any applicable provider deadline. |
| Apple artifact pulls | A pull can run while progressing during an ordinary run. Sealing at run stop grants it up to the remainder of the device window, with the last 40 s reserved for D91 cancellation. This also covers retained transfers. |
| Apple lease release | Up to 40 s for the final release, clipped to the earlier device or provider deadline. Expiry aborts the transport. A quarantined lease stays out of reuse. |
| Android provider termination | One D92 allowance plus the 5 s margin, 65 s by default. This covers the monitor, setup, restart reap and staging waits. The provider stops waiting for an in-flight filesystem syscall at expiry. |
| Report snapshot and reporters | Unbounded by design. Built-in reporters dispatch filesystem work off core. Warning identifies the reporter after a minute, at most once a minute. |
| Remote event analytics drain and close | D51 and D86 share 30 s by default, after file reporting. |
| Usage close and repeated finish calls | Production usage close returns immediately. A repeated call uses the same completion receipt. |
| Signal hook, relay and diagnostics | D52 remains separate. It allows 24 s for report and cleanup and 1 s for final console output. A second signal exits immediately. |

When the Scheduler initiates a stop and both analytics backends use their default allowances, the bounded CLI waits
total **333 s on iOS** (75 device + 65 barriers + 33 history + 70 provider + 30 events + 60 console) and **318 s on
Android** (65 + 65 + 33 + 65 + 30 + 60). A finish that joins the subscription before discovering leftover pools can spend up to another 65 s
(398 s on iOS and 383 s on Android). The vendor runtime can take up to 3 s more to shut down. These are sums of the
named windows. Report duration and
synchronous scheduling or destruction time are outside those sums, and D52 caps its own signal path at 25 s.

## Witnesses

| Item | Witness |
| --- | --- |
| Apple lease release | `connection_factory.rs::only_the_final_lease_abandons_a_stalled_close_at_its_deadline_d95` holds close through the normal and clipped deadline, counts one WARN and abandonment, and checks sibling and replacement sharing and immediate healthy release. |
| History analytics | `provider.rs::history_close_drops_a_stalled_backend_at_the_delivery_allowance_d95` observes ownership drop at 30 s and 8 s, one warning and immediate healthy completion. |
| Android provider | `provider_test.rs::termination_abandons_a_stalled_monitor_join_and_keeps_its_failure_d95` holds a join after the monitor publishes its cause, verifies the 65 s limit and reads the same failure from the channel. |
| iOS provider | `simulator_provider.rs::termination_abandons_setup_at_the_provider_deadline_and_quarantines_d95` stalls a second worker setup, checks one provider warning, and confirms the existing device skips its lifecycle. The surrounding provider and disposal suites cover healthy and already-quarantined dispositions. |
| Retained pull | `simulator_provider.rs::retained_pulls_get_a_stop_deadline_only_after_the_run_stops_d95` holds a retained transfer open for 90 s before the stop, then observes the start of cancellation at 30 s of the device window. |
| Reports | `event_pipeline.rs::a_slow_reporter_warns_every_minute_and_still_completes_d95` observes two named warnings at 60 s and 120 s and normal reporter completion at 125 s. |
| Shared drain timer | `scheduler.rs::answering_the_barriers_cancels_the_drain_timer_d95` checks the outstanding timer is cancelled and no longer queued at the barrier completion event. The D94 run witnesses verify the first cause, results and healthy timing through the full Scheduler. |

Each D95 witness passed 50 isolated runs and one run with six busy CPU workers. A scratch worktree from the base
retained the new tests while disabling the change each test exercises. All seven failed as expected. The five
stalled-work cases reached their hang guards, the report notices were absent, and the drain timer was still active.

The broad run caught an iOS disposal panic that the timed lifecycle task initially treated as an ordinary command
error. The result now carries the distinction across the task boundary. The existing D84 witness verifies both
disposal callers receive the panic and the disposal logs it as an error. The D87 recovery test uses the production
device window so it can still hold disposal across the preparation deadline. Interruptible tasks preserve the
runtime's distinction between a plain spawn and a cancellation-aware spawn. The event-ordering witness passes
with its original scheduling seam.

The vendored SSH source reconstructed byte for byte from the archive and patches. Its 30 client tests passed.
The plan graph, source inventory, parity matrix, fixture comparison, differential registry and offline differential
evidence checks passed. The offline comparator suite ran 110 tests. Source checks used the tree pinned by the
repository's release source marker.

Formatting and affected-package Clippy passed. The affected-package sweep had 2,574 passes, 34 ignored tests and
three failures in the iOS panic, preparation-deadline and event-ordering witnesses described above. All 81 tests
in the five focused iOS rerun suites passed after the corrections, followed by 443 passes in a full serial iOS
run. The final CLI reruns passed 24 tests across the native vendor differential, iOS production smoke,
shutdown-report and signal-window suites. The fake-worker suite passed 31 tests.
The workspace and Linux gates are reserved for the orchestrator. Local evidence uses test workers and fake devices.

## Golden diffs and incidental bugs

There are no golden edits. The two focused artifact regressions now exercise the common provider deadline and the
original late-pull admission race. The latter drives disposal directly so there is time for a lifecycle command
to hit the gate. D94's recorded JVM fixtures remain intact.

Injected console sinks can hold a batch barrier. A core timeout there would need a contract for partial sink
writes and serial output ownership. The CLI writer follows D67. Local blocking filesystem work already inside
a syscall cannot be preempted by the vendor clock. D95 bounds the wait and prevents an abandoned staging
removal from starting after it regains the staging lock.

A parallel iOS wire-suite run hit the watchdog in the test that holds a command on a dead transport. Two tests
install and clear the same process-wide outbound-exec hook, so one can replace the other's hold. The branch
passed the serial rerun. A separate parallel run of the three matching tests on the base also passed, so the
intermittent failure was not reproduced there.
