# x129 — Stop redundant batches in a decided pool (D107)

## Cause and scope

The triggering report covered five runs of the same job on a private CI farm. The three tests passed on healthy Android emulators
early, but the job ended only after attempts on an unhealthy host reached their output timeout, about 16.5 minutes
into the run. Those attempts also counted against the raw results. The job had an available probability history
backend with no rates for the tests, so its default rate produced the maximum number of copies.

- `PoolProgressAccumulator::advance` makes Passed and Failed absorbing states. Fast ANY_SUCCESS can decide on a
  pass and fast ALL_SUCCESS on a failure. The fast shadow verdict used for final report selection when fast is
  off (D77) is not the pool's scheduling state. An exhausted incomplete quota can make that actual state terminal even
  without fast execution, so D107 explicitly does nothing under `fast: false`.
- `QueueActor::process_test_action` removes pending copies on Complete. Before D107, `on_batch_released` sent
  Terminated only when both the queue and its active batches were empty. The pool waited for that message, and
  the Scheduler joined the pool and its supervisor. Terminating a Running device normally accounts for its
  batch first. An unfinished batch returned this way can charge the uncompleted quota and become a failed
  attempt, even when its verdict is already fixed.
- D107 requests a stop once the queue has fully handled the deciding outcome (including tracker delivery), has
  no pending work, and finds every test terminal. The pool decides which of its assignments to stop on core.
  Device actors send outcomes synchronously and mark them first. An outcome sent before the cut is still
  handled even if it is in the pool or queue mailbox. Execute calls still outstanding at the cut, including
  slow vendor tail work, are stopped as whole batches. Sealing their result cells and bypassing `account_for`
  keep an outcome that arrives during cleanup from being charged or sent to metrics or reports. A pool mailbox
  marker orders the remaining outcomes before queue shutdown. Other pools continue normally.
- The pool shares D84's device stop machinery. It seals the vendors, cancels the device jobs and joins cleanup.
  Android uses D92, which joins the runner and listener cleanup and closes the device's ADB request group. D103
  already excludes cancellation and sealed devices. Apple uses the D93 work receipt, including xcodebuild
  session and recorder cleanup. A pool-local receipt does not start an early deadline for earlier artifact pulls
  or for later provider lifecycle and lease release. This distinction matters if another pool runs longer than
  the receipt's window. The intentional stop does not trigger vendor failure policy. If the vendor genuinely
  cannot confirm worker cleanup, existing disposal safeguards for unconfirmed work still apply.
- Unattached vendor files can still exist. They are not published as results. The standard reports consume
  tracked outcomes, and per-test artifact paths include the batch ID. No artifact file format, report format,
  pinned summary wording, aggregate verdict, or CLI exit mapping changes.

## Verification

The fake vendor runs the real Marathon scheduler, progress, report and analytics pipelines. Gated attempts cover
an eight-device probability run with three deciding passes, non-fast accounting, an undecided test with an
empty queue, ALL_SUCCESS with and without `ignoreFailures`, and independent model pools. The main witness also
holds vendor cleanup, tries to publish an attachment with a late result, and checks raw and final results, the
stdout summary, JUnit, Allure, HTML, timeline, INFO records and the remote analytics delegate.

On the base `9c40fba7`, copying only the new test file into a scratch worktree makes all five tests fail at
their required-stop assertions. The non-fast witness first passes its negative control and then fails on the
identical fast run. No timing guard has to expire. The main witness passed 50/50 repetitions and 1/1 with
18 competing CPU workers. The D83 characterization still asserts the same dispatch sequence, now with five
recorded instead of eleven dispatched attempts. The queue oracle permits the new
notification only at the deciding step in K4, with the captured Kotlin fixture unchanged.

Formatting and workspace Clippy passed. With the YAML helper built, the core, Apple, iOS and macOS suites
passed 2,212 tests with 27 ignored. The Android artifact, runner, outage and reconnect suites passed 148 tests.

Existing vendor witnesses on the reused cleanup paths:

- Android `android_artifacts::cancelled_and_dropped_execute_join_artifact_jobs`,
  `android_artifacts::a_failed_recorder_stop_does_not_restart_probes_at_disposal_d92` and
  `outage_batch_test::a_cancel_ends_an_instrumentation_waiting_at_the_gate_d70`.
- iOS `simulator_session::a_batch_cancelled_while_its_xcodebuild_session_is_open_ends_only_after_the_session_has_closed`
  and `simulator_dispose::one_window_from_the_runs_stop_and_lifecycle_only_after_confirmed_termination_d93`.
  Apple `ssh_supervisor::worker_commands_and_senders_stop_over_independent_real_ssh_d96` exercises the
  supervised process stop over real loopback SSH. `closing_an_executor_closes_its_supervisor_control_connection_u270`
  checks that orderly release closes both SSH transports without a `Dying because` line.

## Open questions

The farm was not rerun for this WP. The gated and vendor witnesses establish the accounting and teardown
paths, not the wall-clock savings on a particular emulator or worker.

## Incidental bugs

U291 is the wait for redundant parallel attempts after every test in a pool is decided. Fixed by D107.

## Review round 1

1. **Android tracking.** A sealed device can still be listed and ready. D41 offered it a grace and D22
   offered it restoration, and a cancelled probe could escape the per-server loop to the run's supervision.
   The provider now excludes stopping devices from grace, outage restoration and replacement. A probe already in
   flight releases its wait without publishing a server error if the device has stopped. A two-model-pool fake ADB
   run waits for one pool's stop to finish, then sends its stopped device offline and back while the other pool runs.
2. **iOS artifacts.** An earlier listener can abort the composite's finalizers, stranding a protected xcresult even
   under `pullingPolicy: Never`. The seal now identifies the exact discarded batch, unprotects its source and stops
   an admitted pull for that source. Undelivered callbacks do not report it. A scoped command pass lets earlier
   queued bundles finish their own remote cleanup. Those commands remain subject to disposal and quarantine,
   and their receipts are checked at disposal rather than waited on by the pool's earlier stop.
3. **Unconfirmed iOS work.** The original PoolStop exception incorrectly left a simulator reusable when its
   cleanup was unconfirmed. The decision now quarantines the lease and retires the simulator before releasing
   the waiters. A failure while confirming also goes through the existing cancellation path. Confirmed stops
   do not retire the simulator.
4. **iOS disposal.** A pool stop is not run end. A later disposal with no provider deadline does not run the D98
   busy-video shutdown. Configured lifecycle and any run-end busy-video shutdown use the same disposal deadline,
   apart from any tighter provider or signal deadline.
5. **Queue cutoff and records.** Added U291 and a gated witness that holds the deciding outcome's tracker call
   while a second outcome reaches the queue mailbox. That result is in raw and analytics exactly once, while a
   third, still-running copy is stopped. The reviewed implementation already preserved it. Closing the queue
   right after announcing the decision in scratch loses the second result and fails the witness.

The Android witness and all five new iOS witnesses fail on the reviewed commit `2416086b`. The Android failure
reports the stopped device's cancellation as the cause of the run ending. The iOS failures cover undelivered
bundle diagnostics, a held pull, refused remote cleanup, missing quarantine and a mid-run shutdown. The queue
ablation fails at the recorded serials, with only the deciding device present. No repository fixtures changed.
The main witness and all seven new witnesses passed 50/50 each and 1/1 each with 18 competing CPU workers.

## Review round 2

1. **iOS discarded pull.** Only the discarded batch's admitted xcresult pull now joins the pool's work
   receipt. Its cleanup shares the batch deadline. A pull that stays live past that deadline quarantines
   the lease before provider disposal, without putting earlier batches' pulls on the pool's deadline.
2. **iOS removal race.** Recording the exact discard and releasing its protected source no longer require
   winning the device seal. Earlier removal keeps its own shutdown receipt and deadline.
3. **iOS earlier artifact cleanup.** Disposal closes artifact admission, then waits for the earlier cleanup
   commands' receipts within the same disposal window, including any tighter provider or signal deadline.
   Work still unconfirmed at that point still prevents lifecycle and lease reuse. The reviewed explanation
   was too broad: unsupervised SSH waits for channel CLOSE to return a successful result, while supervised
   SSH completes the receipt from worker proof independently of CLOSE and can publish it after the result.
   A mid-run removal can also reach disposal with a cleanup command still awaiting CLOSE. The loopback
   witness sends exit status and EOF, holds CLOSE until disposal closes admission, and then checks that
   disposal runs the configured simulator shutdown without quarantining it.
4. **macOS inline pull.** Confirmed and documented below.
5. **Android D41 witness.** The offline test now requires the disconnect and rejects the grace warning
   before letting the fake device return. A short-lived grace can no longer make the old provider pass.
6. **Android U170 regression.** Provider termination seals devices while it joins the monitor. The monitor
   must still cancel and join the server tracking tasks, so a recovery cancelled by device cleanup can report
   before the per-server tracking cancellation arrives. Round 1 treated any seal as a pool stop and released
   the hold in that ordering. The provider now checks an explicit pool-stop marker for replacement, grace,
   restoration and recovery completion. A plain termination seal no longer changes U170's cancellation path.
   The unchanged U170 test reproduced the Linux failure on the first local run of the reviewed head.

The Android D41 witness fails on `2416086b` with the grace warning. On `8e35a52f`, the three iOS
witnesses fail at missing quarantine for the stalled discarded pull, the protected source still registered
after the discard, and disposal returning before peer CLOSE. The U170 witness fails on `8e35a52f`
at `in_grace(device)`, as it did on Linux.

The main D107 witness, the unchanged U170 witness and the four new or changed witnesses each passed
50/50 repetitions and 1/1 with 18 competing CPU workers.

### Known limits

- On macOS the result bundle listener runs inline and `pull_bundle` ignores the batch context's cancellation.
  If an rsync pull is already in progress at the D107 cut, it can outlive the pool's work receipt. The pool
  can time out confirming that batch and ordinary provider disposal still owns the outstanding transfer.
  No undecided verdict changes. Addressing this requires a batch-owned pull receipt and a bounded stop path,
  without changing cancellation for other receives.
