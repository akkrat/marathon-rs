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
- The history provider reads the same D51 allowance used by the event backend. Its context clock bounds the
  source close after D34's refresh join. An Influx writer close already aborts its worker if the close future is dropped.
- Android seals device requests when termination starts. Device disposal runs alongside monitoring and setup
  joins. Joining tasks are interrupted on expiry, including children and the process wait of a server restart.
  The shared factory closes after device cleanup or at the provider deadline. Staging admission closes before
  removal. A removal waiting to start checks whether the provider abandoned it.
- iOS refuses new registrations and setup work once termination is published. It asks monitoring to stop and
  starts D94 artifact finalization. Once finalization returns, monitoring and setup joins, recovery, disposal
  and releases share the 70 s provider deadline. A setup that loses its waiter cannot silently release an
  unconfirmed lease. Lifecycle work uses the remaining device window and the provider deadline where applicable.
  D93's run-stop floor can use time within that provider deadline.
- Recovery and retry admission close together when the provider joins recovery. Termination cancels both groups
  together and drops retries requested after that point. Registration closed when termination was published,
  so those retries could no longer return a device to the run. Closing admission under the task lock also
  prevents tasks from escaping the final join.
- A pull's receipt gets the device shutdown deadline even if rsync already owns the transfer and its caller
  has gone. Pulls keep progressing during an ordinary run. At run stop, the last 40 s of the device window are
  reserved for D91 cleanup, leaving up to 30 s of transfer at the default 70 s. Provider shutdown reaches
  retained handles whose actors have already gone. On an interrupted finish those handles receive the 70 s
  artifact finalization deadline at entry, giving them 30 s to transfer. An earlier run-stop receipt deadline
  still stands. Routine termination assigns no pull deadline to a replaced simulator.
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
| iOS artifact finalization | D94 allows 300 s to drain and a shared 60 s for cancellation and the final receipt on routine termination, including failed assertions. Internal failure or owner cancellation uses 30 s plus 40 s. |
| iOS provider joins, disposal and release | One 70 s deadline starts after artifact finalization and covers lifecycle locking, monitoring, setup, recovery, disposal, on-dispose lifecycle and release. |
| Apple disposal and on-dispose lifecycle | D93's 70 s device window. Lifecycle requires confirmation and uses the remaining time, with D93's 5 s floor after a run stop inside any applicable provider deadline. |
| Apple artifact pulls | A pull can run while progressing during an ordinary run. Sealing at run stop grants it up to the remainder of the device window, with the last 40 s reserved for D91 cancellation. Replaced iOS simulators get the interrupted artifact window when provider termination starts. |
| Apple lease release | Up to 40 s for the final release, clipped to the earlier device or provider deadline. Expiry aborts the transport. A quarantined lease stays out of reuse. |
| Android provider termination | One D92 allowance plus the 5 s margin, 65 s by default. This covers the monitor, setup, restart reap and staging waits. The provider stops waiting for an in-flight filesystem syscall at expiry. |
| Report snapshot and reporters | Unbounded by design. Built-in reporters dispatch filesystem work off core. Warning identifies the reporter after a minute, at most once a minute. |
| Remote event analytics drain and close | D51 and D86 share 30 s by default, after file reporting. |
| Usage close and repeated finish calls | Production usage close returns immediately. A repeated call uses the same completion receipt. |
| Signal hook, relay and diagnostics | D52 remains separate. It allows 24 s for report and cleanup and 1 s for final console output. A second signal exits immediately. |

When the Scheduler initiates a stop and both analytics backends use their default allowances, the bounded CLI waits
total **403 s on iOS** (75 device + 65 barriers + 33 history + 70 artifacts + 70 provider + 30 events + 60 console)
and **318 s on Android** (65 + 65 + 33 + 65 + 30 + 60). Joining the subscription before discovering leftover
pools can add up to 65 s (468 s on iOS and 383 s on Android).

Routine iOS finish has the full **360 s artifact window** again. The artifact and provider windows total 430 s,
and the named waits after Scheduler settlement total **553 s** with both analytics backends at their default
allowances (33 history + 360 artifacts + 70 provider + 30 events + 60 console). Scheduler settlement may add
its own waits as listed above. Adding all three settlement allowances gives a conservative 758 s for routine iOS
finish (553 + 65 subscription + 75 device + 65 barriers). The vendor runtime can take up to 3 s more to shut
down. Report duration and synchronous scheduling or destruction time are outside these sums. D52 caps its own
signal path at 25 s.

### Healthy work boundary review

- **Android.** Screen recording stops and video pulls run inside the batch listener. The runner awaits
  `after_test_run_in` and listener cleanup before returning to the device actor. The provider deadline begins
  after Scheduler settlement, so it does not become an artifact or screen-recording limit on a routine run.
- **macOS.** The xcresult listener has no background artifact processor. It awaits collection and remote cleanup
  in the batch’s `after_test_run`. Routine termination applies the 40 s limit to the final lease close after
  disposal. The early pull deadline is installed by the run-stop path.
- **History and event analytics.** The CLI uses separate history and event clients. Prefetch and history queries
  use their existing limits, and the refresh joins under its D34 allowance. D95 times only the subsequent history
  source close. The event tracker still drains and closes within its existing D51/D86 budget.

## Witnesses

| Item | Witness |
| --- | --- |
| Apple lease release | `connection_factory.rs::only_the_final_lease_abandons_a_stalled_close_at_its_deadline_d95` holds close through the normal and clipped deadline, counts one WARN and abandonment, and checks sibling and replacement sharing and immediate healthy release. |
| History analytics | `provider.rs::history_close_drops_a_stalled_backend_at_the_delivery_allowance_d95` drives the replay clock at the default and overridden allowances, including rounding up to the next millisecond. It observes ownership drop, one warning and immediate healthy completion. |
| Android provider | `provider_test.rs::termination_abandons_a_stalled_monitor_join_and_keeps_its_failure_d95` holds a join after the monitor publishes its cause, verifies the 65 s limit and reads the same failure from the channel. |
| iOS provider | `simulator_provider.rs::routine_termination_delivers_a_slow_pull_before_bounding_provider_joins_d95` delivers a retained xcresult pull after 200 s, then gives a stalled setup join its own 70 s. It checks one provider WARN, lease quarantine and no on-dispose command. `a_pull_asked_for_after_the_disposals_look_at_the_pulls_is_refused_d93` checks routine termination runs the confirmed lifecycle after the full 360 s artifact window and refuses a concurrent late pull. The interrupted artifact expiry case still takes 70 s. |
| Retained pull | `simulator_provider.rs::retained_pulls_get_a_stop_deadline_only_after_the_run_stops_d95` holds a retained transfer open for 90 s before the stop, then observes the start of cancellation at 30 s of the device window. |
| Replaced simulator pull | `simulator_provider.rs::replaced_simulator_pulls_get_a_fresh_window_only_on_interruption_d95` replaces a simulator with a pull in flight and advances past its disposal deadline. Routine termination delivers the pull. Interrupted termination starts cancellation at 30 s. |
| Reports | `event_pipeline.rs::a_slow_reporter_warns_every_minute_and_still_completes_d95` observes two named warnings at 60 s and 120 s and normal reporter completion at 125 s. |
| Shared drain timer | `pool_scheduler.rs::answering_the_barriers_cancels_the_drain_timer_d95` drives the Scheduler's stop through both barriers and holds the final queue join. It observes the timer disappear before the join ends and verifies the original failure. The D94 run witnesses verify results and healthy timing. |

The initial seven D95 witnesses each passed 50 isolated runs and one run with six busy CPU workers. A scratch
worktree from the base retained the new tests while disabling the change each test exercises. All seven failed
as expected. The five stalled-work cases reached their hang guards, the report notices were absent, and the
drain timer was still active.

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

Round 1 scratch reverts reproduced the original failures. The slow pull was undelivered, the replaced pull stopped
at once on each termination path, the old provider clock skipped the lifecycle even with a full 360 s drain,
the Scheduler left one timer pending during the final queue join, and the host timer registered zero replay deadlines.
The two InfluxDB refresh witnesses that counted the new close timer as a refresh now track only refresh delays.
Ten focused concurrency witnesses and the seven analytics refresh tests each passed 50 isolated runs and one
run with six busy CPU workers in round 1.
The round 1 serial sweeps passed 932 core, 444 iOS and 69 macOS tests, with one ignored core test. The analytics
rerun passed 73 tests. All nine D94 and eight D95 witnesses passed, as did the four CLI suites (24 tests).
Formatting, affected-package Clippy, the plan graph, the differential registry and 110 offline evidence tests passed.

## Golden diffs and incidental bugs

There are no golden edits. The focused artifact regressions exercise the separate finalization and provider
deadlines and the original late-pull admission race through provider termination. D94's recorded JVM fixtures remain intact.

Injected console sinks can hold a batch barrier. A core timeout there would need a contract for partial sink
writes and serial output ownership. The CLI writer follows D67. Local blocking filesystem work already inside
a syscall cannot be preempted by the vendor clock. D95 bounds the wait and prevents an abandoned staging
removal from starting after it regains the staging lock.

A parallel iOS wire-suite run hit the watchdog in the test that holds a command on a dead transport. Two tests
install and clear the same process-wide outbound-exec hook, so one can replace the other's hold. The branch
passed the serial rerun. A separate parallel run of the three matching tests on the base also passed, so the
intermittent failure was not reproduced there.
