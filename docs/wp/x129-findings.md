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
