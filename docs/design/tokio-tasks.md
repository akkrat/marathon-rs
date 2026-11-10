# Tokio tasks and cancellation (WP 8.13)

Design only, surveyed at `1e264101`, after WP 8.11. This document proposes no
contract change, D-row, golden update or parity-matrix exemption. Paths are relative
to the repository; names under `executor/` mean
`crates/marathon-core/src/executor/`. WP 8.12's Apple reader work is independent;
its eventual reader implementation is not assumed here.

**Recommendation: reduce the adapters selectively; do not replace the lifecycle
with a tree of CancellationTokens and JoinSets.** Tokio already schedules the
work. Most remaining code determines which result wins, who owns cleanup and when
reports can run. Keep that policy explicit. Implement A and B below first; assess
C and D separately. E and F are conditional follow-ups, not prerequisites or a
promise to remove `RuntimeJob`.

## 1. Inventory and the semantics actually used

> Sections 1–6 are the WP 8.13 snapshot. WP 8.15 has since replaced the single-implementation
> traits `Job`, `CoreRuntime`, `TimeoutScope` and `Deferred` with concrete types; see §7.

### Size and counting method

The executor contains **12 files, 2,776 physical lines**, including comments,
blanks, the 166-line replay driver and the 155-line inline tracking tests. It is
not 2,800 lines of removable coroutine infrastructure:

| File | Lines | Responsibility |
|---|---:|---|
| `mod.rs` | 354 | Dedicated core thread/current-thread Tokio runtime + LocalSet; two-worker vendor runtime; task launch, panic conversion, vendor joins and shutdown |
| `job.rs` | 476 | Ordinary/supervisor ownership, completion/cancellation/failure cascades, removable callbacks, observed-child traversal |
| `actor.rs` | 232 | FIFO mailbox, owner and receiver jobs, handler/context retention |
| `clock.rs` | 323 | Shared scheduler timeline, core/vendor delay, scoped timeouts |
| `timeout.rs` | 55 | Four signed Java Duration-to-millis overloads |
| `retry.rs` | 68 | Core and Send retry loops |
| `shared.rs` | 303 | Reasoned Signal, SharedDeferred, SharedChannel, left-first race helper |
| `bridge.rs` | 114 | Off-core dispatch, protected join, first-poll cancellation gate |
| `tracking.rs` | 273 | Runnable/running/blocking quiescence accounting, including tests |
| `hooks.rs` | 378 | Wall/monotonic clocks, replay tapes, random/ID draws and barriers |
| `test_support.rs` | 166 | ControlledReplay and explicit timer release/drain |
| `completion_logging.rs` | 34 | Completion-to-diagnostic mapping |

The following counts are **source files referencing a construct**, not dynamic
instances or raw occurrences. They include imports and implementation definitions.
Scan `crates/marathon-{core,android,apple,ios,macos,analytics,cli}/src/**/*.rs`;
exclude `tests.rs`, `tests/`, `test_support.rs`, `transport_faults.rs` and inline
`#[cfg(test)] mod ... { ... }` tails. Ignore strings and comments (notably the
string `Job was cancelled`). Core includes the implementation itself. Wildcard
imports and indirect use through `VendorDependencies` do not count as a named type
reference. The clock row additionally counts `.delay(...)` and
`.with_timeout[_or_null](...)`, so implicit vendor-clock uses are included.
This is a reproducible static footprint, not a claim about crate coupling depth.

| Construct/search family | core | android | apple | ios | macos | analytics | cli |
|---|---:|---:|---:|---:|---:|---:|---:|
| Jobs/scopes/context: `Job`, `JobRef`, `JobKind`, `JobState`, `TaskContext`, `TaskBody`, `CoreRuntime`, `CompletionRegistration` | 14 | 0 | 0 | 0 | 0 | 0 | 0 |
| Actors: `RuntimeActor`, `Actor`, `ActorHandler`, `ActorContext` | 6 | 0 | 0 | 0 | 0 | 0 | 0 |
| Signals: `Signal`, `CancellationSignal` | 19 | 6 | 11 | 10 | 2 | 4 | 0 |
| `SharedDeferred` | 6 | 0 | 2 | 5 | 1 | 0 | 1 |
| Provider channels: `SharedChannel`, `EventChannel` | 5 | 1 | 0 | 1 | 1 | 0 | 0 |
| Clock/timeout seam: preceding calls plus `SchedulerClock`, `VendorClock`, `RuntimeClock`, `RuntimeTimeouts`, `TimeoutScope`, `VendorTimeoutBody` | 20 | 7 | 11 | 8 | 2 | 1 | 0 |
| Retry helper calls: `with_retry`, `with_retry_send` | 3 | 0 | 1 | 0 | 0 | 0 | 0 |
| Vendor task types: `VendorRuntime`, `VendorTask`, `VendorTaskBody` | 22 | 9 | 23 | 8 | 0 | 2 | 0 |
| `call_vendor(...)` calls | 5 | 3 | 0 | 0 | 0 | 0 | 0 |
| `CoreExecutor` | 9 | 0 | 0 | 0 | 0 | 0 | 1 |
| `.invoke_on_completion(...)` | 1 | 0 | 0 | 0 | 0 | 0 | 0 |
| `.join_observed_children[_in](...)` | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| `.spawn_cleanup(...)` | 1 | 0 | 0 | 0 | 0 | 0 | 0 |
| Replay seam: `RuntimeHooks`, `ReplayHooks`, `ReplayDriver`, `ControlledReplay`, `ReplayBarriers`, `Tape`, `ClockMode` | 12 | 0 | 0 | 0 | 0 | 0 | 1 |
| Completion diagnostics: `CompletionLog`, `completion_log`, `completion_logging` | 2 | 0 | 0 | 0 | 0 | 0 | 0 |

Static call-site counts, using the same exclusions (including internal forwarding
calls, never multiplied by devices, connections, batches or attempts):

| Call | core | android | apple | ios | macos | analytics | cli |
|---|---:|---:|---:|---:|---:|---:|---:|
| `Signal::new()` | 13 | 5 | 1 | 16 | 3 | 2 | 0 |
| `SharedDeferred[::<T>]::new()` | 5 | 0 | 4 | 7 | 3 | 0 | 1 |
| `SharedChannel::new()` | 1 | 1 | 0 | 1 | 1 | 0 | 0 |
| `RuntimeActor::{new,with_logger}(...)` | 3 | 0 | 0 | 0 | 0 | 0 | 0 |
| `.with_timeout[_or_null](...)` | 6 | 2 | 6 | 4 | 1 | 0 | 0 |
| `with_retry[_send](...)` | 3 | 0 | 1 | 0 | 0 | 0 | 0 |
| Runtime/RetainedTasks `.spawn(...)` (manually excluding process/thread creation) | 9 | 1 | 24 | 14 | 4 | 1 | 0 |
| `.spawn_cancellable(...)` | 3 | 8 | 2 | 2 | 0 | 0 | 0 |
| `.spawn_blocking(...)` | 3 | 2 | 3 | 3 | 0 | 0 | 0 |
| `.spawn_cleanup(...)` | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| Explicit `tokio::[task::]spawn[_local,_blocking](...)` | 2 | 0 | 0 | 0 | 0 | 0 | 0 |

The last row excludes `handle.spawn`, counted in the runtime-spawn row; the two core sites are
`launch_retained` and actor bookkeeping. A generic `.spawn(` grep also finds
process/thread creation and wrapper calls, so it must not be presented as a task
count. For example, the CLI deliberately starts console, exit/watchdog and run
threads; macOS launches through Apple's `RetainedTasks` without naming
`VendorTask`. The analytics transport also already uses native Tokio timeouts;
zero scoped-timeout calls there does not mean zero deadlines.

### Ownership and call-site inventory

| Construct | Concrete consumers and relied-on semantics |
|---|---|
| Ordinary Job, SupervisorJob, detached scopes | `marathon-core/src/marathon.rs::run_under` builds root → supervisor → run child. `execution/scheduler.rs::new` makes a detached ordinary job; `subscribe_on_devices` is under the run context, not that job. `execution/pool.rs::spawn_with`/`started` build the pool owner and a **sibling** supervisor under the scheduler job; queue/device owners are under the supervisor. Ordinary child failure cancels parent/siblings; supervisor failure is isolated, its cancellation cascades. Completion waits for the body and children, and a stand-alone job requires explicit `complete`. |
| TaskContext and protected cleanup | `execution/device/actor.rs::launch` launches prepare, execute, readiness and terminate under the actor owner. Readiness is a sibling of execute. `marathon.rs::spawn_step` and the error path use `spawn_cleanup` without the cancelled parent. Cancelling is a request, not proof that a body, artifact pull or blocking task has ended. |
| Job joins and completion registrations | `execution/device/actor.rs::started` installs a weak own-job callback before dispatch, forces termination and accounts for an unpolled batch. Protected `join` ignores the joined failure; `join_in` checks the caller even if the target already completed. `Scheduler::execute` uses lazy observed-child traversal, not a snapshot or a fixed-point join. The single-node/list distinction and early-child → parent → late-child callbacks are still pinned. |
| Actor + mailbox | Exactly three production constructor sites: `execution/device/actor.rs`, `execution/pool.rs`, `execution/queue.rs`. Unbounded FIFO in enqueue order, one receiver, no global ordering across senders. Close rejects sends, discards unread messages and cancels the owner; receiver cancellation alone leaves owner/siblings active. `started` runs even for an already-cancelled parent; handler storage lives through callbacks. `safe_send` keeps its check-then-send race. |
| Signal and linked cancellation | `Signal` is first-reason-wins and Send + Sync; cancellation carries Requested/Parent/Timeout with scope identity. Android `artifact_listeners/mod.rs::ChildSignal`, Apple `artifact_processor/mod.rs::WorkerSignal`, iOS `device/simulator_provider.rs::LinkedSignal`, `apple_simulator_device/mod.rs::LinkedSignal` and `device/simulator_provisioner.rs::Combined` combine sources with left/parent priority. `bridge.rs::Gated` delays newly requested cancellation until the vendor's first poll returns. These are not interchangeable token trees. |
| SharedDeferred | DeviceActor's parentless batch-result cell can seal before `Device::execute` returns a different outcome, or remain unsealed after successful return. It is first-writer-wins, repeat-awaitable, synchronously inspectable and independent of waiter cancellation. Also used by `marathon.rs` finish steps/signals, CLI `runner.rs` command delivery, Apple `artifact_processor/{core,mod}.rs` admission/drain barriers, iOS simulator prepare/execute/dispose/provider gates and macOS device disposal/outcomes. |
| EventChannel/SharedChannel | Android `provider.rs`, iOS `device/simulator_provider.rs`, macOS `provider.rs` each construct one device-event channel; core's `event_channel` registers a replay probe. Shared **consuming** receiver, not broadcast. Close drains admitted buffered events, cancel discards and overrides close. Ready buffered/terminal results precede waiter cancellation; a suspended wait has prompt cancellation. |
| TimeoutScope and VendorClock timeouts | Core scheduler's first-device poll and `marathon.rs::extract_remotely`; Android `support.rs::timed` and `device.rs`; Apple local/SSH executors, `xcodebuild_session.rs`, artifact processor; iOS boot/execution/video; macOS execution. A timeout gets a fresh identity, cancels its own child, then awaits cleanup; `or_null` catches only its own expiry. Ordinary cleanup failure outranks cancellation. Durations are signed; nonpositive timeout skips the body, nonpositive delay is a no-op, `i64::MAX` delay is unbounded; Duration conversion overflow fails before starting a scope. |
| Retry helpers | `marathon.rs::extract_remotely`: three attempts, no delay; `execution/device/actor.rs::initialize`: thirty attempts, 10 s; `analytics/provider.rs::fetch`: three initial attempts with 50 ms delays via Send helper; Apple `apple_application_installer.rs`: three, 1 s. Cancellation is not retried. The Send helper checks and races its signal and **drops an in-flight attempt** on cancellation; the core helper awaits the attempt's own result. Do not accidentally unify those policies. |
| Other deadline helpers | Apple `cmd/remote/ssh/sshj/client.rs::within` races a generic future against VendorClock.delay, body first, without a timeout-child scope. It is used for SSH replies and rsync stop waits. Analytics Graphite uses native `tokio::time::timeout`; HTTP has per-operation socket-idle limits. Keep these distinct from cleanup-bearing TimeoutScope operations. |
| Other retry loops | Android provider reconnect/backoff and iOS recovery/setup retries are domain policy. Apple `cmd/remote/ssh/sshj/factory.rs::with_retry_sync` creates fresh clients, closes failed ones and has its own error precedence. Analytics `influx.rs::BatchWriter` and `http.rs` have buffer/status/Retry-After policy. A general retry crate must not subsume these. |
| VendorTask and task collections | `IoRuntime` runs Send work on the vendor runtime, exposes cooperative cancel and repeatable protected join. Android `provider.rs`, `device.rs`, `runner.rs`, artifact listeners own handles. Apple's `cmd/base_command.rs::SessionScope` tracks command/SSH lifetimes; `cmd/retained_tasks.rs::RetainedTasks` prunes successes but retains errors until joined. ConnectionFactory, rsync, iOS provider/recovery/prepare/execute and macOS device/provider use those retained collections. Analytics `influx.rs` owns its writer; core `analytics/provider.rs` owns D34 refresh. |
| Bridge and blocking work | `call_vendor` callers include core Marathon, scheduler, DeviceActor, progress and output, plus Android provider/device/remote parser. Its protected join returns the **vendor outcome**, not the caller's cancellation. Parsing, file operations, worker-source reads and connection setup use vendor `spawn_blocking`. No future being Send is permission to poll it on core. |
| Clock, tracking, hooks and diagnostics | `Timeline` is shared by RuntimeClock and SendClock. `Tracked` counts cross-thread hand-offs before releasing the prior unit. `ControlledReplay` releases timers one by one in (deadline, sequence) order. `RuntimeHooks` preserves independent wall/monotonic time and tape/barrier consumption. Completion logging is separate from lifecycle ownership and is already routed through 8.8's output machinery. |

## 2. Mapping and boundaries

Cargo.lock pins Tokio **1.53.2** and tokio-util **0.7.20**; core directly depends
on Tokio, not tokio-util. Introducing CancellationToken/TaskTracker would require
an explicit pinned dependency and the appropriate `rt` feature. No upgrade is
part of this proposal.

| Current construct | Target | Deliberately custom remainder |
|---|---|---|
| Dedicated CoreExecutor + non-Send TaskContext | Keep `current_thread` runtime + `LocalSet` + `spawn_local`, already in use | Explicit core/vendor split, Rc ownership and per-run services. No Arc/Mutex conversion of core state. |
| RuntimeJob, Ordinary/Supervisor and scoped jobs | Keep now. A future task registry may use `JoinSet::spawn_local` for **physical task retention only** | Parent graph, failure propagation, child completion, explicit complete, callbacks, traversal and reasoned cancellation. A JoinSet is not a Job. |
| Signal | Keep the current `OnceLock<Cancellation> + Notify` initially. Optional `OnceLock<Cancellation> + CancellationToken` notification backend | Set reason before wake; only the winning setter cancels the token. Keep token private and unparented. Native `child_token()` cannot publish the correct child reason, distinguish parent failure or run ordered callbacks. Linked sources retain their priority; Gated stays. |
| Protected cleanup | Explicit detached cleanup tasks, tracked and joined by the responsible owner | Never cancel cleanup by dropping a scope or an abort guard. No automatic observable cleanup in Drop. D52 is a deliberate bounded-wait exception. |
| Scoped timeout / timeout-or-null | Clock-seam timer + biased `tokio::select!` over **borrowed pinned work**, followed by cancel + protected await | Identity, signed limits, child context, children/cleanup join and failure precedence. Raw `tokio::time::timeout(body)` is unsuitable. Keep SchedulerClock/VendorClock; native sleep stays behind Timeline. |
| Retry | Keep the small explicit loops; use the same clock/signal interfaces | Counts, cancellation exclusion, last-error precedence, different core/Send cancellation safety. No backoff library or new jitter. |
| SharedDeferred / repeatable VendorTask join | Keep custom first-writer cell and waiter arbitration | oneshot is single-consumer; watch does not reproduce notification-time exception precedence; a Shared future does not give synchronous external seal/inspection. Use oneshot only for private single-consumer transport such as the bridge payload. |
| Actor mailbox | Candidate: `tokio::sync::mpsc::unbounded_channel` on core, one local receiver task | RuntimeActor owner/receiver split, close-as-discard, started, retention and MailboxProbe; keep existing queue if those adapters erase the saving. Never use broadcast or a bounded mailbox. |
| SharedChannel provider events | Keep SegQueue/counters/Notify implementation | Cloned consuming receivers, close versus cancel and nonblocking admission/close on core. mpsc's one mutable receiver would require a new dispatcher or an async lock; broadcast duplicates events. Neither is a simplification. |
| IoRuntime/VendorTask/RetainedTasks/SessionScope | Keep facade; candidate owner-side Tokio task registry, with explicit joins | Multiple observers, stored result/error, snapshot/admission semantics, owner-specific cleanup and bounded abandonment. Keep insertion order where errors are joined in order. |
| call_vendor | VendorRuntime spawn + private `oneshot<T>` payload + protected task join | Gated first poll, panic/error classification, explicit vendor-runtime dispatch and Tracked completion. Never race/drop the whole vendor call on caller cancellation. |
| Timeline + ControlledReplay + Tracked | Keep; live timers already use `tokio::time::sleep` | Explicit advancement, cross-executor quiescence, equal-deadline release order, mailbox probes and watchdog. Tokio paused time on one runtime cannot drive this two-runtime contract. |
| RuntimeHooks/completion diagnostics | Keep outside the replacement scope | Replay labels/cursors/barriers and ordered log/raw output. Cosmetic diagnostic changes do not justify disturbing the 8.8 exit path. |

Tokio's [JoinSet documentation](https://docs.rs/tokio/1.53.2/tokio/task/struct.JoinSet.html)
specifies completion-order results and abort-on-drop. Its `spawn_local` supports
non-Send tasks, but supplies none of the parent policy above. Use explicit
`join_next` error handling rather than `join_all`/`shutdown` shortcuts.
[CancellationToken](https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html)
signals a request and supports one-way parent-to-child notification; it does not
carry a cause or acknowledge task completion. These are building blocks, not the
replacement architecture.

### Before/after sketches

These sketches describe boundaries, not compiling APIs to add in this WP.

**Scoped timeout:** retain the work after the deadline so async cleanup executes.
A live Tokio timeout also polls the body before checking expiry; wrapping work
with it would change the pinned equal-deadline rule. See
[Tokio timeout](https://docs.rs/tokio/latest/tokio/time/fn.timeout.html).

```rust,ignore
// Before: clock.rs has separate core and Send nested race trees.
match race(race(cancelled(), timeline.sleep(ms)), work.as_mut()).await { /* ... */ }

// After: same scope creation and signed checks; centralize only arbitration.
let mut work = pin!(body_and_structured_join(child_context));
let event = tokio::select! {
    biased;
    reason = child_signal.cancelled() => Stop::Cancelled(reason),
    elapsed = timeline.sleep(ms) => stop_from_timer(elapsed, own_timeout),
    result = work.as_mut() => Stop::Finished(result),
};
match event {
    Stop::Finished(result) => result,
    Stop::Cancelled(reason) => {
        child.cancel(reason.clone());
        preserve_failure(work.await, reason) // never drop work on expiry
    }
}
// or_null translates only Cancellation::Timeout with THIS invocation's id.
```

Core work includes `finish_body` and the structured child join, so the deadline
remains armed after body completion while children run. Send timeout work has no
RuntimeJob children, but must still finish its body cleanup. Unify the terminal
arbitration without pretending those ownership models are identical. Pending
borrowed futures need not become `'static` spawned tasks.

**Bridge payload:** one consumer permits a real simplification.

```rust,ignore
// Before: call_vendor captures Arc<Mutex<Option<T>>>; task joins, then takes it.
let cell = Arc::new(Mutex::new(None));
// vendor stores Some(value); core joins and locks cell

// After: task completion/error remains authoritative; payload has one reader.
let (send, receive) = tokio::sync::oneshot::channel();
let task = vendor.spawn(arm_after_first_poll(gate, async move {
    let value = call(gated_signal).await?;
    let _ = send.send(value);
    Ok(())
}));
task.join().await?; // protected; a panic/failure wins over payload disconnect
receive.await.map_err(missing_vendor_result)
```

The sender is not a batch-result Deferred and cannot replace one. Keep the first
poll gate around the same portion of vendor work. Awaiting payload alone would
lose task failures and would not prove cleanup completed.

**Actor transport:** native channel under the existing owner semantics.

```rust,ignore
// Before: RuntimeActor holds RefCell<VecDeque<M>> and Notify.
queue.borrow_mut().push_back(message);
changed.notify_waiters();

// Candidate after: synchronous admission on core; receiver owned by its task.
if actor.is_closed_for_send() { return Err(rejected(message)); }
sender.send(message).map_err(rejected_send)?;
// receiver loop checks receiver-job cancellation before taking a ready message.
// close marks closed, cancels OWNER and wakes receiver; receiver closes/drains
// unread items without dispatch, runs protected handler cleanup, then completes.
```

No RefCell borrow or mutex guard may span `recv().await`. Keep a logical closed
flag: mpsc close alone drains buffered messages and is not Actor.close. Probe
counts must be updated on successful admission, consumption and discard, with
no core suspension between admission and increment. Check message-drop timing
and owner-completion callbacks before accepting this candidate; if immediate
queue release requires a second custom queue, retain VecDeque/Notify.

**Cancellation notification:** possible, but not the recommended first change.

```rust,ignore
// Before: reason.set(reason); changed.notify_waiters(); cancelled() loops on Notify.
// After: private token, no exposed cancel/child_token/drop_guard path.
fn cancel(&self, reason: Cancellation) -> bool {
    if self.reason.set(reason).is_err() { return false; }
    self.token.cancel();
    true
}
async fn cancelled(&self) -> Cancellation {
    self.token.cancelled().await;
    self.reason.get().cloned().expect("cancel publishes a reason first")
}
```

The assertion concerns an internal publication invariant, not I/O. The reason
is published before the token wakes; parent propagation still explicitly calls each child's `cancel(reason)`.
This replaces a small, working notification loop with a dependency, not the
476-line Job implementation. It should earn its place through reduced maintenance
or measured benefit before an implementation WP is commissioned.

## 3. What must not change and the pinning tests

This design keeps the existing semantics, including internal witnesses. Phase 8
could later remove a Kotlin-only quirk, but this is not permission to weaken a
test until a replacement passes. A changed result, worker command, progress line
or exit policy requires the §3.12/D-row process. Older lifecycle.md text saying
the shutdown hook is debug-only or finish stops after its first thrown close is
superseded by D51/D52 and logging.md's current finish/cancellation paths.

Test paths below are `crates/<crate>/tests/<file>.rs` unless stated otherwise.
Run each whole named suite as well as the cited tests; table entries identify the
relevant regression, not a complete replacement for the gate.

| Invariant | Existing pins |
|---|---|
| Inputs, CLI flags/defaults/status mapping; result files and worker commands | `marathon-cli/cli_matrix::cli_matrix_matches_the_reference_build`; `marathon-core/run_flow_oracle`, `run_flow_witnesses`, `parse_command`, `marathon_scenarios`; native cases and captured command/report views validated by `tools/differential/run validate` and exercised by workspace tests. Keep fixtures and allowances unchanged. |
| Exact per-test progress and summary; ordered output despite vendor scheduling | `marathon-core/progress_oracle`, `pool_progress`, `run_results`; `log_queue_output::late_output_forwards_records_pushed_before_and_after_the_close_fence`; `live_output` (pump and concurrent flush). |
| Ordinary failure, supervisor isolation, child cancellation, explicit completion and protected cleanup | `marathon-core/runtime_behavior::{ordinary_child_failure_cancels_parent_and_sibling,supervisor_isolates_failure_but_cascades_cancellation,standalone_job_needs_complete_and_waits_for_children,cancellation_waits_for_body_cleanup_and_children}`; `runtime_review::cancellation_return_cancels_children`. |
| Protected/cancellable joins and callbacks; no unbounded recursion | `runtime_behavior::{join_ignores_joined_failure_and_join_in_observes_caller,completion_callbacks_are_once_late_and_disposable}`; `runtime_review::parent_completion_is_ordered_between_child_callbacks`; `runtime_carried::{deep_chain_cancellation_cascades_iteratively,deep_chain_completion_cascades_upward_iteratively,deep_chain_failure_propagates_iteratively,many_completion_callbacks_on_one_job_are_released_iteratively}`. These and the following runtime suites belong to marathon-core. |
| Detached scheduler graph, 100 ms first-device polling and lazy traversal | `runtime_races::{single_child_traversal_misses_later_attachment,list_traversal_reads_successor_after_each_join}`; `runtime_carried::cancellable_traversal_matches_jvm_traversal_rows`; `pool_scheduler::{no_devices_timeouts_keep_subscription_alive,first_device_is_observed_on_the_hundred_millisecond_poll,lazy_child_traversal_joins_a_pool_created_mid_traversal}`; `runtime_oracle::historical_list_and_callback_promotion_match_jvm`. |
| Actor close/cancel/started/handler lifetime | `runtime_behavior::{actor_fifo_started_and_send_after_close,actor_close_interrupts_handler_and_discards_queue,actor_safe_send_keeps_check_then_send_race,actor_panic_retains_handler_until_completion_callbacks}`; `runtime_review::cancelled_receiver_keeps_actor_owner_active`; `runtime_review2::receiver_only_cancellation_interrupts_handler_delay_and_awaits_cleanup`. |
| Batch first seal, outcome-before-Terminated, once-only notification and queue acknowledgement | `device_actor_batch_accounting::{termination_before_results_returns_the_batch_once_and_ignores_a_late_report,actor_failing_before_a_dispatched_batch_starts_executing_returns_the_batch}`; `pool_batch_accounting::pool_shuts_down_only_after_the_queue_processed_results_forwarded_before_the_last_terminated`; `device_actor::{readiness_is_a_sibling_under_the_owner_not_the_execute_task,terminated_is_sent_at_most_once}`. |
| Deferred fast path versus suspended success/exception arbitration; awaiter cannot seal cell | `runtime_behavior::deferred_first_seal_repeat_wait_and_cancelled_waiter_isolation`; `runtime_review::cancelled_waiter_observes_already_failed_deferred`; all six `runtime_review2::suspended_deferred_*` cases; `runtime_carried::{completion_in_the_first_poll_window_precedes_pending_cancellation,vendor_thread_completion_races_pending_cancellation}`. |
| Provider graceful close, cancel-over-close, buffered-before-caller-cancel and suspended prompt cancel | `runtime_behavior::{provider_channel_close_drains_cancel_discards_and_cancel_overrides_close,provider_receive_cancellation_preserves_cell_and_other_waiters}`; `runtime_review2::cancelled_channel_waiter_*` and `suspended_channel_*`; `pool_scheduler::buffered_disconnect_is_processed_after_cancellation`. |
| Timeout identity, failure precedence, signed edges, children and cleanup | `runtime_races::{timeout_owns_child_context_and_awaits_protected_cleanup,nested_core_timeout_is_not_caught_by_outer_or_null,external_core_cancel_preserves_reason_through_timeout,core_timeout_stays_armed_while_completed_body_waits_for_children,timeout_panics_surface_as_failures_without_leaving_children_live}`; vendor twins `vendor_timeout_awaits_cleanup_on_shared_timeline`, `nested_vendor_timeout_keeps_original_scope_and_external_cancel`; `runtime_oracle::{pinned_coroutines_corpus_matches_runtime,vendor_signed_edges_match_same_jvm_corpus,structured_coroutines_corpus_matches_runtime}`. |
| Retry count/delay, cancellation and vendor error classification | `runtime_behavior::{retry_counts_attempts_and_rethrows_last_failure,retry_cancellation_short_circuits_and_delay_observes_job}`; `device_actor::{thirty_failing_preparations_retry_every_ten_seconds_and_log_each_attempt,a_preparation_that_fails_with_a_cancellation_of_its_own_terminates_the_device_at_once_without_retries,every_vendor_outcome_maps_to_its_log_and_transition}`; `marathon-apple/skip_copying_derived_data::installer_oracle_transcripts` and `parser_error_catches`. |
| Core never blocks; bridge result and first-poll boundary | `runtime_behavior::{vendor_work_and_blocking_work_run_off_core_and_remain_joinable,vendor_task_cancel_resumes_explicit_signal_and_awaits_cleanup}`; `device_actor::the_vendors_own_outcome_wins_over_the_core_cancellation`; `runtime_carried::{drain_waits_for_blocking_work,watchdog_fails_a_scenario_blocked_inside_poll}`. |
| Replay timeline, equal-time timers/events and cross-executor continuation order | `runtime_races::{timers_win_before_equal_timestamp_external_completion,core_same_deadline_delay_loses_to_own_timeout,cross_executor_vendor_delay_continuation_and_bridge_are_drained,same_deadline_vendor_continuations_keep_registration_order,equal_timestamp_events_keep_delivery_order_and_future_timers_do_not_advance,pending_callback_work_and_stalled_closed_buffers_fail_quiescence}`; `replay_hooks::{wall_instant_and_scheduler_time_remain_separate,virtual_clock_and_scheduler_hooks_require_a_driver,barriers_admit_in_order_and_cancelled_wait_does_not_consume_token}`; `executor/tracking.rs` unit tests `a_hand_off_between_threads_never_reads_as_idle`, `a_delayed_duplicate_wake_does_not_queue_uncounted_work`. |
| D34 refresh schedule, eight rounds per key, empty success, snapshots and bounded close | `analytics_refresh::{a_backend_that_stays_down_gets_a_capped_schedule_and_a_bounded_total,an_empty_answer_is_a_success_so_it_is_cached_and_never_refreshed,lookups_and_prefetches_of_a_failed_key_never_query_the_backend,close_drops_a_refresh_query_in_flight_before_the_source_closes,close_stops_waiting_for_a_source_that_blocks_inside_its_future,a_batch_a_shard_and_a_queue_each_read_one_view_of_the_history}`; `backend_failure_run`. Keep 5/10/20/40/80/160/300/300 s vendor-clock rounds and 3 s close bound. |
| D48 replacement-device wait and cleanup | `pool_scheduler::{a_cancelled_run_does_not_wait_out_the_limit_d48,the_wait_runs_while_the_leaving_device_cleans_up_d48,a_cleanup_still_running_at_the_limit_is_waited_for_and_nothing_new_is_taken_d48,a_pool_created_during_a_wait_is_joined_and_the_run_waits_for_both_d48}`. Keep the deadline based on departure/test progress rather than cleanup completion; no fixed-point join of all future pools. |
| D50/D60 queue/batch behavior under changed scheduling | `strategy_batching::{fixed_size_counts_only_accepted_tests_against_the_size_and_re_adds_the_rest,fixed_size_adds_only_accepted_durations_and_breaks_at_the_limit,fixed_size_stops_after_two_rejected_polls_per_slot,d50_fills_the_slot_a_flaky_copy_took_in_the_jvm_row}`; `queue_oracle::D50_ROW` witness; `strategy_sorting`, `analytics_refresh::reinsertion_uses_new_observations_with_pinned_history_and_immutable_survivor_keys`; keep insertion-time keys, queue history view, accepted-only size/duration and 2 × size rejected-poll cap. Runtime refactoring must not introduce a yield inside queue check/assignment. |
| D52 and 8.8 first/second signal, reports, flush and process end | `marathon-cli/shutdown_reports::{a_cancel_mid_test_writes_the_reports_and_does_not_bounce_the_test_d52_u161,a_cancel_writes_reports_first_and_stops_a_slow_worker_process_d52}`; `signal_windows::{a_signal_terminates_the_provider_whatever_debug_says_d52,a_stuck_provider_holds_a_cancel_only_up_to_the_limit_d52,a_second_signal_right_after_the_first_is_not_lost_d52,a_blocked_vendor_thread_does_not_hold_the_exit_after_the_run_d52,a_second_signal_exits_at_once_during_a_blocked_sink,a_slow_exit_preparation_still_exits_with_the_signal_status}`; `tracing_output::{writes_after_close_are_rejected_not_lost_silently,cancellation_flushes_even_when_process_exit_skips_guards}`. |
| D54 transfer-specific cancellation/loss, serialized send admission and cleanup | `marathon-apple/rsync_file_bridge::{a_send_cancelled_mid_transfer_stops_rsync_at_once_and_the_send_behind_it_goes_on_d54,a_cancelled_send_whose_rsync_ignores_term_is_killed_d54,a_cancelled_send_whose_rsync_exits_0_on_term_still_ends_what_it_started_d54,a_send_whose_connection_is_lost_fails_stops_rsync_and_the_send_behind_it_goes_on_d54,a_receive_whose_connection_is_lost_returns_false_and_stops_rsync_d54}`; run the entire suite for pre-start/waiter and sshpass cases, plus `ssh_task_lifetime`, `ssh_session`, `local_executor`. |

In particular, D52 is **24 s for cancellation cleanup plus 1 s final output**,
with reports getting a 10 s head start before the remaining cleanup starts
alongside them. Stop actors first; do not overwrite previous reports before
output-directory preparation. A second signal exits at once; coalesced same-signal
delivery counts once. Exit status is 128 + signal. Keep the off-core writer and
exit watchdog, the nonreturning post-exit relay wait and `tracing_output::exit_once`.
After an ordinary run, CoreExecutor's vendor shutdown waits at most 3 s; that
must not become an unbounded JoinSet drain.

D54 sends observe caller cancellation and their own connection loss, including
while waiting for an earlier send; receives ignore caller cancellation and observe
connection loss. A stop sends TERM immediately, then KILL after 5 s to rsync and
its descendants, even if rsync exits zero. Do not give artifact receives the
execute task's token or introduce a pre-TERM 5 s wait. Preserve no-log cancellation,
send loss error versus receive `false`, and the continued admission of other sends.

## 4. Risks and mitigations

| Risk | Handling / rejection criterion |
|---|---|
| Drop cancellation skips async finally work | Never select against an owned prepare/execute/timeout/bridge future and drop it as a generic policy. Pin it outside select; signal its child and await it, or run explicit owner-retained cleanup. Preserve intentional drop-safe exceptions: Send retry attempts, cancellable channel waits, D34 in-flight refresh query and bounded D52 waits. Classify each call, not each Rust future type. |
| JoinSet abort-on-drop and completion-order errors | Owner must outlive its waiters; cancelled join futures cannot own the set. Drain individual results explicitly; never replace insertion-order joins with completion-order error reporting. On the D52 deadline, do not drop a set containing report/transfer cleanup. If adoption needs `detach_all` plus another registry of repeatable results, count that code against the saving and prefer the current handles. |
| Panics differ from OperationError/Cancelled | Preserve catch_unwind at task/body boundaries, panic → Runtime Failure, ordinary/supervisor propagation, cleanup-child joins and once-only diagnostic ownership. Handle JoinError explicitly; Tokio abort is not a reasoned Cancellation. A callback panic remains isolated as today. |
| LocalSet versus Send futures | Keep all core state on the dedicated thread. Use spawn_local for core and explicit vendor handle for Send work; never call tokio::spawn merely because a core closure can be made Send. LocalSet does not prevent blocking: preserve code review and watchdog checks for blocking work. |
| Paused-clock timer resolution/order | Do not replace shared virtual Timeline with Tokio paused time or auto-advance. Core and vendor runtimes have independent Tokio drivers. Keep signed boundary checks before Duration conversion, explicit timer sequence and one-at-a-time drain. Live Tokio millisecond resolution does not define replay equality ordering. |
| Native timeout or select fairness changes winner | Raw timeout polls its body first; unbiased select randomizes ready branches. Keep cancellation → timer → work priority in the scoped-timeout runners, or retain race itself. Apple SSH `client.rs::within` intentionally polls work before its timer; preserve that different helper policy. Equal-time external events remain after due timers, in tape order. No new yield between result admission and actor state transition. |
| CancellationToken cannot carry reasons or completion | Keep reasoned Signal facade. Do not use a bare child_token graph, `run_until_cancelled` or drop_guard around cleanup-bearing operations. Publish reason before notification; retain priority in linked signals and the bridge first-poll gate. |
| watch/oneshot changes deferred arbitration | SharedDeferred stays for external first seal, repeated waits and synchronous inspection. New private oneshot transport has one consumer and joins task completion before reading payload. Do not rewrite notification-time exception wins as a next-poll select bias. |
| mpsc actor conversion changes close/drop timing | Separate logical closure from native channel drain; reject sends synchronously, suppress dispatch and retain handlers through callbacks. Preserve immediate discarded probe state. Validate queued-message destruction and self-close/reentrant completion; abandon conversion if an extra mailbox/dispatcher is necessary. |
| Lost replay work or retained task leaks | Wrap every new physical task and any registry-drain task in Tracked. Acquire its busy unit before spawning; publish completion/wake before releasing the prior unit. Pending task count / JoinSet emptiness is not runnable quiescence. New abort/drop paths must release tracking tickets; current Tracked assumes ordinary completion, so adding abort needs an explicit tested Drop policy. |
| Blocking work cannot be killed by Tokio cancellation | Keep off-core dispatch, join ownership and watchdogs. Tokio documents that started [spawn_blocking tasks cannot be aborted](https://docs.rs/tokio/1.53.2/tokio/task/fn.spawn_blocking.html); shutdown_timeout stops waiting, not the call. D34/D52 bounds must remain process-level policy. Do not claim a token solves a thread blocked inside poll. |
| WP 8.12 concurrent Apple reader changes | Inventory is a baseline, not a rewrite prescription for Reader. Implement A/B in core first. Re-survey SessionScope, command task lifetimes and reader tests after 8.12 merges before commissioning E. Do not change reader backpressure, EOF/idle precedence or cancellation while moving task ownership. |

No live-device timing claims were established by this design. New implementation
tests follow testing.md: virtual time for semantic deadlines, gates for ordering,
60 s hang guards for real work and awaited shutdown before counting transcripts.
Never make a replay test pass by adding an arbitrary number of yields or widening
a behavior-defining timeout.

## 5. Independently mergeable implementation WPs

These are proposed brief identifiers **A–F**, not assigned §7 WP numbers. Estimate
scale is PLAN's **L/M/H** (low/medium/high); elapsed estimates are focused engineer
days including tests and review fixes, not elapsed CI time. Each candidate ships
a complete old-facade-compatible change with `scripts/check.sh` green, no fixture,
allowance or golden edits, and no parity-matrix deletions. A mixed implementation
is supported between every merge. If new tests reveal an observable change, fix
it within that WP or stop; do not make later WPs repair an interim regression.

| Candidate / order | Scope and dependencies | Effort | What it deletes; acceptance and stopping rule |
|---|---|---|---|
| **A. Remove unused timeout overloads** | After design. Audit `executor/timeout.rs` and remove the module export in `executor/mod.rs`. No active timer implementation changes. | **L, 0.5–1 day** | Delete the four unused Duration overloads and their 55-line module after confirming the workspace API audit (no call sites were found). Keep signed conversions in active vendor call paths; Java Duration parsing stays. Run runtime_oracle/races and the full gate. No compatibility shim is needed for an unconsumed internal workspace API. |
| **B. Single-consumer result transport** | Independent of A. `executor/bridge.rs::call_vendor` only; keep VendorRuntime/VendorTask facade. | **L, 1–2 days** | Delete `Arc<Mutex<Option<T>>>`, lock/store/take branches; use private oneshot plus protected task join. Keep Gated/ArmAfterFirstPoll. Add gated panic, cancellation-before-first-poll and missing-payload coverage if not exercised by existing tests; run device_actor, runtime_races and output suites. This replaces no batch-result Deferred. |
| **C. Timeout policy consolidation** | After A/B for review sequencing; technically depends only on the design. `executor/clock.rs` core/Send timeout arbitration. Keep runtime traits and Timeline intact. | **M, 2–3 days** | Delete duplicated terminal-event/failure-precedence code by sharing policy. Preserve the core child job/join and the Send borrowed body. Run runtime_oracle/races and all vendor timeout suites. Native timeout is not the implementation target; abandon factoring if a generic driver becomes harder to audit than the two small bodies. |
| **D. Actor mailbox backend** | After C for review sequencing; technically independent, uses existing jobs. `executor/actor.rs` and tests; three actor constructors stay API-compatible. | **M, 3–5 days** | Delete VecDeque/Notify mailbox wake loop and its lost-wakeup setup, replace with unbounded mpsc. Retain owner/receiver and closure/probe adapter. Run runtime_behavior/review2, device_actor accounting, queue and pool suites. First verify queued-message destruction semantics. Stop if preserving immediate discard needs a second queue or if bookkeeping grows rather than shrinks. |
| **E. Vendor task retention** | After 8.12 and fresh survey/review; separate from core jobs. Start at `IoRuntime` completion/handle ownership, then migrate one bounded owner class through an unchanged VendorTask facade. SessionScope/RetainedTasks migration must be a separate focused commit or WP if it cannot fit. | **H, 5–8 days for the first owner class** | Candidate deletion: duplicated successful-task pruning and owner-specific spawn bookkeeping in RetainedTasks consumers. Evaluate a runtime-owned reaper using JoinSet, or tokio-util TaskTracker plus stored outcomes; neither replaces replay Tracker. Keep joinable result cells and ordered error ledgers. A reaper must be explicitly owned, tracked and stopped; no hidden always-running task. If it just adds a second registry, delete the prototype and retain existing collections. Gate with ssh_task_lifetime, rsync_file_bridge, Android/iOS/macOS provider/dispose and D34/D52 suites. No broad vendor migration bundled with this estimate. |
| **F. Core job/scope reduction** | Last, conditional on demonstrated net reduction in A–E. Separate approval of an implementation brief, without changing this design's contracts. `runtime.rs`, `executor/{job,mod,actor,clock}.rs`, `execution/{scheduler,pool,device/actor}.rs`, `marathon.rs`. | **H, 8–12 days for one compatible vertical slice; full replacement unestimated** | No deletion is established for a full replacement: spawned Tokio handles are already discarded, and a new registry would add ownership code. Only schedule a vertical slice if its brief identifies an existing retention path it can delete. It **cannot presently delete** the RuntimeJob state/callback list/cascade/traversal or explicit TaskContext. Keep the Job facade until a slice passes all runtime and scheduler witnesses. If these structures merely move into a JoinSet adapter, stop: that is no reduction. Removing lazy traversal or callback ordering needs a separate behavior proposal and is outside F's unchanged-golden scope. |

**No retry-only WP:** `retry.rs` is 68 lines with four production helper call
sites. A library would add configuration/error adapters and threaten cancellation
classification for no useful deletion. Keep it custom. Audit Send attempt drop
safety when its callers change; do not alter it during C.

**No general SharedDeferred replacement WP:** B is the bounded single-consumer
case. The 303-line shared module also supplies signals and provider channels;
removing it is not a legitimate saving estimate. Keep SharedDeferred and its
DeferredResume notification arbitration. Do not convert dispose/admission/finish
cells merely because their payload is `()`; several have multiple waiters or
repeatable inspection. Likewise there is no provider-channel rewrite in this split.

**No Signal-only WP yet:** cancellation tokens would delete a small Notify loop
but add a direct dependency and preserve the reason cell and parent graph. If E
needs the token for a concrete owner, make the backend substitution a separate
L (about one day) compatible change, retaining all reason and linked-priority
pins. It is not a dependency of A–D.

## 6. Recommendation and unsettled questions

Commission **A and B**. Proceed to C only if sharing the timeout policy makes it
shorter and easier to audit; decide D from its mailbox/discard spike and diff.
These changes can reduce duplicate mechanics without changing Marathon's
lifecycle. Keep retry, SharedDeferred, SharedChannel, Timeline/replay and the reasoned Signal API custom.

Do **not** commission a full Job-to-JoinSet rewrite on the present evidence.
LocalSet and Tokio tasks are already in place; preserving the current job graph,
callbacks and Scheduler's observed traversal would recreate most of RuntimeJob
around the new primitives. Standardize the task-retention parts only when a
specific owner becomes simpler. A claim that this will delete all 2,776 lines is
unsupported; this design offers no performance or line-reduction estimate for F.

Questions requiring implementation evidence or a later contract decision:

1. Does shared timeout arbitration reduce code once the core structured join and
   Send borrowed body are expressed without extra boxing or generic scaffolding?
   C must answer with its diff and all signed/nested/tie tests, not a new timeout
   policy.
2. Can D preserve queued-message destruction and reentrant close/callback lifetime
   without keeping the old queue? Existing tests pin discard and handler lifetime,
   but do not establish every message destructor's timing. Add a gated drop probe
   before adopting mpsc; retain the old transport if the answer is no.
3. Which post-8.12 vendor owner has an actual net saving from JoinSet/TaskTracker?
   Repeatable VendorTask results, bounded abandonment, insertion-order error
   precedence and cross-thread calls remain requirements. E must select and measure
   one owner before promising a rollout.
4. Should Phase 8 deliberately retire Scheduler's single-node/list observation
   quirk or callback-order witnesses? They can affect late pools, result timing
   and shutdown. This design preserves them; only a separately reviewed proposal
   with evidence and any required D-row/golden updates can relax them.

No prototype or runtime modification is retained by WP 8.13. The only plan change
is the design row; the later-candidates bullet stays until concrete implementation
briefs and their numbered rows exist.

## 7. Review outcome (orchestrator decision)

Two independent critiques reviewed this design. Both agreed with its central
conclusion: no Job-to-JoinSet rewrite, no CancellationToken tree, and keep the
core `LocalSet`, the reasoned cancellation signals, `SharedDeferred`,
`SharedChannel`, the retry loops and the replay `Timeline`. They differed from it
on scope, and those points are adopted:

1. **Merge single-implementation layers (implemented by WP 8.15).**
   `Job`, `CoreRuntime` and `TimeoutScope` each had exactly one implementation,
   including tests. `JobRef = Rc<RuntimeJob>`, `Rc<CoreExecutor>` in
   `TaskContext`/run wiring, and `Rc<RuntimeTimeouts>` replace those traits.
   The pointer-keyed weak job registry, its amortized sweep and panicking
   `resolve()` are deleted without changing scheduling. Private construction
   and the dedicated thread's Send factory/result boundary prevent foreign
   jobs from reaching another runtime; two `JobRef` compile-fail doctests pin
   that ownership guarantee. A future constructor on an existing thread must
   add owner validation. `Deferred<T>` is also a concrete alias for the sole
   implementation, `SharedDeferred<T>`, with inherent completion/wait methods;
   vendor signatures keep the name without trait objects. `VendorRuntime`,
   `VendorClock`, `CancellationSignal`, `EventChannel` and `Actor` stay traits
   (they have multiple implementations). This replaces C and D as the
   substantive simplification.
2. **B uses `try_recv` after the protected join.** The producer has already sent
   the payload when the join completes; awaiting a oneshot adds a suspension point
   (Tokio's cooperative budget can return `Pending` with a value present) where
   none exists today.
3. **Delete dead surface:** the unused timeout overloads (A), `Actor::safe_send`
   and `Job::cancel_and_join` (no production callers), and the production path of
   `completion_logging`, whose loggerless failures accumulate in a vector nothing
   drains: route them to the log queue instead.
4. **Fix defects the critiques found** (separately from the refactors, each with a
   witness test):
   - a vendor future that panics on its first poll leaves its cancellation gate
     unarmed, so a retained child holding the gate can wait for ever
     (`bridge.rs`);
   - `Tracked` releases its replay ticket only in `poll`, with no `Drop`, so any
     abort leaks the busy count;
   - iOS `apple_simulator_device::LinkedSignal::cancelled()` uses an unbiased
     `select!` while `reason()` prefers the parent, so the two can disagree when
     both sources are ready;
   - `timeout_panics_surface_as_failures_without_leaving_children_live` spawns no
     child; add a witness that a panicking timeout body cancels and cleans up a
     live descendant.
   The precedence where a cancelled vendor task that panics reports cancellation
   rather than the panic (`IoRuntime`) is kept and documented, not changed.
5. **Not commissioned:** D (actor mailbox to mpsc: tokio's receiver drains rather
   than discards on close and adds cooperative-budget yields, so it is at best
   line-neutral with scheduling risk), E (the retention code it targets is about
   120 lines and already uses repeatable protected joins), F, a token backend.
   C only as a spike whose first commit shows a net-negative diff with the sleep
   future pinned outside any loop.
6. **Kotlin quirks are a separate decision.** Retiring the single-child traversal
   distinction, disposable completion registrations and the `Parent`/`Requested`
   tag needs its own design-only brief that classifies each as production
   dependency, integration witness or synthetic coroutine witness, and prices a
   D-row and any golden movement (including the D48 interaction).

Resulting work packages, in order: **8.14** runtime cleanup (A, B with
`try_recv`, dead surface, `completion_logging` routing, and the four defect fixes
above; S–M); **8.15** concrete core handles (M); then, only on request, a
design-only brief for retiring the coroutine quirks.
