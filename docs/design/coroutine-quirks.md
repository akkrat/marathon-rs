# Remaining coroutine quirks (WP 8.17)

Design only, against `80edc252`, after WP 8.15. No runtime, fixture, golden or
parity-matrix change is made here. This implements the investigation requested
by [the brief](../wp/8.17.md) and [Tokio design §7](tokio-tasks.md#7-review-outcome-orchestrator-decision).
The implementation proposals below require separate review. WP 8.16 is independent.
Paths and line numbers refer to this revision; `executor/` abbreviates
`crates/marathon-core/src/executor/`.

**Recommendation:** retire the distinction between ordinary cancellation tags,
disposable registrations, the single-node traversal exception, callback nodes
in the child list, and registration-position parent completion. Keep lazy
successor traversal and the explicit job graph. The saving is concentrated in
`job.rs`; this does not justify a Job-to-JoinSet rewrite.

## 1. Inventory and classification

A production dependency means removing the behavior changes Marathon's run,
results or worker effects. An integration witness means only an end-to-end test
requires it, without a corresponding contract requirement. A synthetic coroutine
witness constructs jobs/callbacks directly to reproduce coroutine-library
internals, even if its expected values came from a JVM oracle. An oracle test's
name does not make it a Marathon integration witness.

| Candidate | Class | Production reason and removal experiment |
|---|---|---|
| Single-node miss versus historical list mode | Synthetic coroutine witness | Each pool historically promotes the Scheduler job to list mode before observation. S removes `list_mode` and always reads the successor after joining. Evidence below. |
| Disposable completion registration | Synthetic coroutine witness | The only production registration is DeviceActor's `started`; it stores the handle but never calls `dispose`. D makes disposal inert with all test assertions unchanged. |
| `Cancellation::Parent` versus `Requested` | Synthetic coroutine witness (constructor surface only) | Production propagation and adapters preserve the payload; no policy branches on these two tags. P erases the variant and migrates constructors/patterns, including test construction, mechanically. |
| Callback markers interleaved with child nodes, including callback-triggered list promotion | Synthetic coroutine witness | Only `RuntimeJob::register` creates `Node { job: None }`; these nodes affect historical traversal state, not child ownership. N stops appending them. |
| Parent completion continuation's position among child callbacks | Synthetic coroutine witness | Only DeviceActor registers a production user callback, and it ignores the outcome. C moves automatic parent completion after the child's callbacks. |
| Lazy successor reads after each joined child | Production dependency | A new pool can arrive while an earlier pool is waiting under D48. L replaces traversal with a snapshot; its failures below establish why this behavior stays. |

P is strictly unwitnessed as a *distinction*: its two synthetic test
constructors exercise payload handling, not a requirement for a Parent tag.
Within the brief's three-way classification it belongs to the synthetic
category with this explicit qualifier; no failing witness is claimed for it.

No candidate falls solely in the integration-witness class at this revision.
Absence of a failure is evidence for this checkout and its test coverage, not
proof about every possible downstream user of the public Rust types.

### Production call graph

- `execution/scheduler.rs::execute` is the only production caller of
  `join_observed_children_in`; its protected counterpart is used internally by
  that wrapper. The subscription belongs to the run task, while the Scheduler
  job is detached. Pool creation is synchronous after history prefetch:
  `DevicePoolActor::spawn_with` creates the actor under the Scheduler job, and
  `started` creates its **sibling** supervisor before construction returns.
  Scheduler cannot observe a half-constructed pool on its single core thread.
  This promotes its detached job to list mode before observation, and that
  historical flag never resets when a child is removed; even a later
  one-child remainder therefore already takes the list path.
  If no child is left when observation begins, both the old and proposed
  traversal return immediately. Neither waits for future admission.
  Completion of `Scheduler::execute` does not itself cancel the detached job
  or its run-owned subscription. An event for a new pool after that result can
  therefore still create work while the subscription remains alive; it is not
  retroactively joined or included in the captured result. D48 closes an
  expired existing pool to returns, not every possible future pool ID. The
  prototype explicitly checks this boundary below. Marathon/provider cleanup
  remains responsible for stopping the subscription and device source.
- `execution/device/actor.rs::started` is the only production call of
  `invoke_on_completion`. `Shared.registration` retains the returned handle;
  no production `CompletionRegistration::dispose` call exists. The callback
  captures `Weak<Shared>`, forces Terminate, accounts for a Running batch and
  sends Terminated once. The runtime retains the handler through completion.
  Removing the handle must retain that callback and its weak capture.
- `executor/job.rs::{finish_body,child_failed_step}` and
  `marathon.rs::run_under` create `Parent` cancellations. `error.rs::failure`
  and completion logging unwrap both tags identically. Apple
  `apple_device_test_runner.rs::RunError::into_vendor` adds suppression text
  through one shared payload arm. iOS `device/error.rs::SetupFailure::into_vendor`
  reconstructs the original tag with a replacement payload, without choosing
  different policy. Preserve suppression and timeout identity through both.
- `execution/pool.rs` awaits queue acknowledgement before cancelling its
  supervisor and closing its actor. DeviceActor synchronously enqueues batch
  outcomes before Terminated. These domain orders survive C: a callback is
  still run on core exactly once before owner bookkeeping is released.

### What is outside the retirement set

All eleven files in `executor/` were inspected. The remaining mechanisms have
separate responsibilities already identified in `tokio-tasks.md`: dedicated
core `LocalSet` versus Send vendor execution (`mod.rs`), FIFO/discard and
owner/receiver lifetime (`actor.rs`), first-poll cancellation visibility and
protected payload join (`bridge.rs`), shared scheduler deadlines and scoped
cleanup (`clock.rs`), retry policy (`retry.rs`), reasoned signals, repeatable
result cells and consuming provider channels (`shared.rs`), quiescence and
replay tapes (`tracking.rs`, `hooks.rs`, `test_support.rs`), and failure
logging (`completion_logging.rs`). They are retained, not newly classified as
removable quirks by this design. There is no `timeout.rs` after WP 8.14.

In particular, retain first-reason-wins, first failure precedence, timeout
scope identity, ordinary/supervisor failure ownership, protected cleanup,
caller-cancellable joins, callback panic isolation, and the iterative cascade.
D34's history snapshots/refresh shutdown, D50's batching rule and D54's
transfer cancellation/connection-loss distinction are not retirement targets.

## 2. Prototype method and observed failures

Each experiment starts from the same revision in a detached scratch git
worktree. Concurrent experiments have separate sources and build directories;
restore tracked files before reusing a worktree. No copied fixture, golden,
allowance or timing value is changed, and no existing assertion is weakened,
omitted or assigned a new expected value. The only test-source
migration is P's replacement of the removed enum constructor. D deliberately
keeps the old registration API as an inert test shim so disposal witnesses can
fail at runtime rather than fail to compile. N and C similarly isolate one
semantic change before deleting the resulting dead implementation surface.

With the prescribed Rust toolchain on PATH and `MARATHON_KT` pointing to the
read-only Kotlin checkout, run for the baseline and **each** experiment:

```sh
cargo test --workspace --no-fail-fast
cargo build -q -p snakeyaml --example yaml_tree
tools/differential/run check
```

The YAML helper is built initially and rebuilt if the comparator reports it
stale. `--no-fail-fast` is essential: a deliberately failing runtime test must
not prevent the rest of the workspace, including D48 and D52, from running.
`tools/differential/run check` exercises the registered offline native golden
comparisons and owner contracts; it is not a fresh JVM capture, a live-device
run or a claim about unregistered profiles. Historical JVM evidence remains
unchanged. Scratch sources are removed after the investigation. The original assertions
remain active; scratch-only probes add D48 timestamp output and D52 deadline
assertions, without replacing any existing test.

### Mutations (independently reproducible)

| ID | Mutation against the baseline |
|---|---|
| S | Delete `Inner.list_mode`, its initializer and the promotion in `append`. In `join_observed_children`, start from `head` and delete the single-node `break`; preserve post-join `current.next`. |
| D | Replace `RegistrationHandle::dispose` with an empty method. Keep its return type and all tests so ignored disposal is observable. |
| P | Delete `Cancellation::Parent(Failure)`; replace its three production constructors and test constructors with `Requested`; remove duplicate match arms in core, Apple and iOS. No payload or timeout change. |
| N | Delete only `register`'s `if completion.is_none() { self.append(node.clone()); }`. Keep child nodes, disposal and parent continuations otherwise unchanged. |
| C | Delete `Callback::Parent`, its dispatcher arm, `waiting_child` and the first-child search/registration in `try_finish_step` (lines 289–323). After `parent.remove_child(self)`, push `Step::TryFinish(parent)` **before** pushing user callbacks onto the LIFO stack. This runs the child's callbacks before automatic parent completion. Keep the body/child-count completion guard. |
| L | Replace the body of the observed traversal with `for job in self.children_snapshot() { job.join().await?; }`. Leave cancellation of the caller in the existing wrapper. |
| R | Combine S, D, P, N and C, excluding L. Old assertions remain. Uniform traversal intentionally removes N's callback-promotion difference; verify the composed witness set rather than assuming a union. |

All final mutations compiled and ran the entire workspace. Exit 101 below
means the listed old witnesses failed; every other workspace test passed.
Only ordinary repository-ignored tests were skipped. No expected-value edits
were made to make a prototype pass. Paths in this table are under
`crates/marathon-core/tests/`.

| Experiment | Workspace exit / failures | `tools/differential/run check` |
|---|---|---|
| Control | 0; none | 0; complete pass |
| S | 101; `runtime_carried.rs::cancellable_traversal_matches_jvm_traversal_rows`, `runtime_oracle.rs::pinned_coroutines_corpus_matches_runtime`, `runtime_races.rs::single_child_traversal_misses_later_attachment` | 0; complete pass |
| D | 101; `runtime_behavior.rs::completion_callbacks_are_once_late_and_disposable`, `runtime_oracle.rs::structured_coroutines_corpus_matches_runtime` | 0; complete pass |
| P | 0; none | 0; complete pass |
| N | 101; `runtime_carried.rs::cancellable_traversal_matches_jvm_traversal_rows`, `runtime_oracle.rs::historical_list_and_callback_promotion_match_jvm` | 0; complete pass |
| C | 101; `runtime_review.rs::parent_completion_is_ordered_between_child_callbacks` | 0; complete pass |
| L | 101; `pool_scheduler.rs::a_pool_created_during_a_wait_is_joined_and_the_run_waits_for_both_d48`, `pool_scheduler.rs::lazy_child_traversal_joins_a_pool_created_mid_traversal`, `run_flow_oracle.rs::two_pools_disconnect_matches_the_jvm_transcript`, `runtime_carried.rs::cancellable_traversal_matches_jvm_traversal_rows`, `runtime_oracle.rs::historical_list_and_callback_promotion_match_jvm`, `runtime_oracle.rs::pinned_coroutines_corpus_matches_runtime`, `runtime_races.rs::list_traversal_reads_successor_after_each_join` | 1; `run_flow_oracle::two_pools_disconnect_matches_the_jvm_transcript` |
| R | 101; `runtime_behavior.rs::completion_callbacks_are_once_late_and_disposable`, `runtime_carried.rs::cancellable_traversal_matches_jvm_traversal_rows`, `runtime_oracle.rs::structured_coroutines_corpus_matches_runtime`, `runtime_oracle.rs::pinned_coroutines_corpus_matches_runtime`, `runtime_races.rs::single_child_traversal_misses_later_attachment`, `runtime_review.rs::parent_completion_is_ordered_between_child_callbacks` | 0; complete pass |

S changes only `single:attached-observed` from false to true. D changes only
the structured corpus's `job:callback-order` from `1,3` to `1,2,3` and its
direct counterpart. N makes `list:callback-promotes` false; historical
second-child promotion still works. C changes `job:parent-child-callbacks`
from `early-child,parent,late-child` to `early-child,late-child,parent`.
P has no distinguishing failure at all after constructor migration.

R fails **six** old tests, not the union of the isolated failures: with S's
uniform traversal, N no longer makes historical callback-promotion rows
false. This is why traversal precedes storage cleanup in the proposed split.
R has no new production integration failure. A supplementary R-only test,
`runtime_review::wp817_late_child_callback_can_attach_to_a_completing_parent`,
passes: a callback registered after the parent starts completing attaches a
live child, and the parent completes only after that child is completed.
This probe was also run explicitly as a targeted test; it changes no existing
expectation.

L is a rejected replacement. `late-pool.result` becomes false instead of true;
the D48 pool-B assertion fails while its work is still running. The native
check also rejects the two-pool transcript: it ends at **62,100 ms**, rather
than the required **64,100 ms** (the historical 4,100 ms plus D48's 60,000 ms).
The native check stops at this registered owner-contract failure; later check
targets are not reached in L, while the separate workspace `--no-fail-fast`
run still executes them all. This is a failure of the existing D48 allowance,
not a request to widen it or bless new goldens. The five isolated retirements
and R complete every registered native check target successfully.

Measured physical source churn in the diagnostic prototypes (excluding tests,
including comments/blanks; removed/added/net removed):

| Experiment | Removed | Added | Net removed |
|---|---:|---:|---:|
| S | 12 | 1 | 11 |
| D (inert disposal shim) | 6 | 1 | 5 |
| P | 8 | 5 | 3 |
| N (no marker append) | 3 | 0 | 3 |
| C | 44 | 1 | 43 |
| L (rejected snapshot) | 13 | 2 | 11 |
| R (combined diagnostic removals) | 73 | 8 | 65 |

The real API/storage deletions below remove the diagnostic shims as well;
65 is the observed prototype reduction, not the final implementation estimate.

### D48 and D52 timing

Use the existing virtual-clock assertions rather than a host elapsed-time
threshold. S, D, P, N and C pass the focused `pool_scheduler d48` and
`run_flow_witnesses d52` runs; R passes both too. The same tests run as part
of each full workspace suite. Results are exact scheduler timestamps:

| Boundary / witness | Control and proposed retirements | L (snapshot) |
|---|---|---|
| No return after departure at 1,500; `a_run_ends_when_no_device_returns_d48` | Still pending at 61,499; ends at 61,500 | Same |
| Same-pool device after departure at 500 plus the limit; `a_device_after_the_limit_is_dropped_and_still_tracked_d48` | Ends at 60,500; late device tracked, no batch | Same |
| Pool B arrives at 1,000 while joining pool A; `a_pool_created_during_a_wait_is_joined_and_the_run_waits_for_both_d48` | Pending at 60,500; ends at 201,000 after B's two batches | Already ended at 60,500; focused rerun prints `Some(60500)` |
| Caller cancels during return wait at 1,000; `a_cancelled_run_does_not_wait_out_the_limit_d48` | Ends cancelled at 1,000 | Same |
| New pool after `execute` returned; scratch `wp817_d48_new_pool_after_end_is_not_retroactively_joined` | Captured end remains 60,500; new pool can still run two batches under the live detached graph | Same |
| Hung provider, with either finishing or hung reports; scratch `wp817_d52_virtual_bound` | Exit callback at 24,000; SIGTERM status 143 | Same |

The additional after-end-pool probe runs on the control, N, C, L and R; the
initial S/D/P runs use the existing D48 cases. The more detailed clock
instrumentation on control/N/C/L/R observes provider termination beginning at
0 when reports finish, or at exactly 10,000 when reports hang. Exit is at
24,000 in both cases. Initial S/D/P probes also assert the 24,000 bound.

D52's separate 1,000 ms final-output allowance remains unchanged, making the
contract bound 25 s. The virtual probe measures the 24 s cleanup path with an
unblocked final flush; it does not assert a tight wall-clock 25 s bound on a
loaded host. All full suites also run the existing CLI blocked-provider,
blocked-console, second-signal and shutdown-report tests successfully.
D48 tests for flapping, cleanup beyond expiry, stale timers, vendor-used wait
and returning devices that never become ready likewise keep their assertions.

Thus D48 does depend on lazy successor timing, but not single-node/list
representation; D52 does not require any retired representation or tag.

## 3. Proposed replacements, deletions and contracts

D-row identifiers below are **draft labels**, not allocated ledger numbers.
Allocate the next free numbers when commissioning each WP; the concurrent
image WP may allocate a row first. This design does not approve a contract
change or edit §3.11 in advance.

### A. One ordinary cancellation tag

Use `Requested(Failure)` for explicit and inherited non-timeout cancellation;
retain `Timeout { scope, detail }`. Keep `OperationError::Cancelled` distinct
from `Failed`. An inherited ordinary failure still sets the parent's
`Completion::Failed`, fails ordinary ancestors and cancels siblings; relabeling
the cancellation signal must not turn that failure into successful completion.

Delete `error.rs`'s `Parent` enum member and duplicate payload arm, two job
constructor uses, Marathon's constructor use, completion logging's duplicate
arm, Apple's duplicate suppression arm, and iOS's duplicate reconstruction
arm. This is a small sum-type simplification, not removal of reasoned
cancellation: three physical lines disappear after mechanical
constructor replacements; no runtime algorithm is deleted.

Behavior change: Rust callers inspecting enum/debug tags see `Requested`
where they previously saw `Parent`. Failure details, nullable messages, causes,
suppression, cancellation precedence and timeouts remain. Migrate the direct
constructors in `runtime_races.rs` and `queue_prepare.rs`; preserve
their payload assertions. Add a direct propagation test proving ordinary
failure still wins completion while the sibling signal contains the identical
payload under Requested. Keep the nested-timeout and own-timeout tests.

> **D-CQ1 draft:** rc.1 distinguishes explicit and inherited ordinary
> cancellation with Requested/Parent tags in the Rust coroutine adapter.
> Ordinary cancellation now uses Requested with the same failure payload;
> inherited failure propagation and supervisor isolation are unchanged.
> Timeout scope identity and cleanup/error precedence are unchanged. No
> configuration, worker-command, result-data, progress or summary change is
> expected; native goldens must remain unchanged.

### B. Non-disposable completion callbacks

Make `invoke_on_completion(Box<dyn FnOnce(Completion)>) -> ()`. The job owns the
callback until exactly-once dispatch; an already-completed job invokes it
synchronously. Keep weak actor capture and catch callback panics separately.
Delete `runtime.rs:74–76` (`CompletionRegistration`), `job.rs:20–32`
(`RegistrationHandle` and disposal), handle construction at `job.rs:371–376`
and the boxed return at `476–481`. Adapt private `register` to return the
registration/completion pair needed by the current dispatcher. Delete
DeviceActor's stored registration field, initializer and assignment; register
directly in `started`. The `contracts.rs` test handler also loses its stored
handle. Keep callback nodes temporarily until D so this WP does not silently
alter list promotion. Approximately 25–35 lines net disappear, depending on
how the private registration return is expressed; this excludes later savings.

Behavior change: a callback cannot be withdrawn. Dropping an ignored old
handle already did not dispose it, so production retention stays the same.
Rewrite `runtime_behavior::completion_callbacks_are_once_late_and_disposable`
to assert once-only FIFO callbacks and immediate late registration without
`dispose`. In `runtime_oracle::structured_coroutines_corpus_matches_runtime`,
retain historical `job:callback-order = 1,3` as evidence; assert native `1,2,3`
with a named D-CQ2 expectation for exactly that row. Do not remove the rest of
the structured corpus test. Keep DeviceActor batch-accounting, cancellation
before first dispatch and once-only Terminated regressions.

> **D-CQ2 draft:** rc.1's completion registrations expose disposable handles.
> Registrations now last until job completion and cannot be withdrawn; callbacks
> remain exactly once, ordered within a job, panic-isolated, and immediate
> when registered after completion. DeviceActor's weak completion callback and
> outcome-before-Terminated order stay unchanged. Only synthetic disposal
> witnesses change; native result goldens must remain unchanged.

### C. One lazy child traversal

Always walk the linked child cursor and read its successor after each join.
Remove `list_mode` at `job.rs:94,141`, its promotion in `append`, the
tuple/single flag at `459–462`, and the `467–469` break. The S prototype gives an exact small
physical-line saving; see the measured churn above. Rename the traversal only
if it improves the documentation; preserve the cancellable wrapper.

Behavior change: a job with just one child can observe an attachment made
while that child is joined. Registering a callback and adding/removing an
unrelated second child cease to choose the traversal algorithm. It remains a
cursor traversal, **not** a fixed point: an empty start returns;
a new head attached after the cursor detached need not be observed. Preserve
the retained cursor's successor when removing a node. Keep iterative list
teardown: a long list of real child nodes can still overflow a recursive drop.

Rewrite `runtime_races::single_child_traversal_misses_later_attachment` to hold
the late child on a gate, assert traversal is still active, release it and
join. In `runtime_oracle::pinned_coroutines_corpus_matches_runtime` and
`runtime_carried::cancellable_traversal_matches_jvm_traversal_rows`, change
only the native `single:attached-observed` expectation to true under D-CQ3.
Retain historical JVM rows on disk. Keep list successor, snapshot, caller
cancellation, empty traversal and detached-Scheduler coverage; add a
removed-tail/late-new-head witness to prevent an accidental fixed-point join.

> **D-CQ3 draft:** rc.1's observed-child traversal selects one-child or list
> behavior from kotlinx JobSupport's historical node representation. All jobs
> now use lazy successor traversal: attachments linked behind the current
> cursor during a join can be observed even when traversal began with one
> child. Empty traversal still returns and no future-pool admission window is
> added. Scheduler's detached ownership, lazy late-pool join, D48 wait limits
> and D52 cancellation bound remain. Native goldens must remain unchanged.

### D. Children and callbacks as separate storage

Keep the linked **child-only** spine; do not replace it with a snapshot,
JoinSet, task tracker, Vec rescan or a wait-until-empty loop. Keep a FIFO Vec
of owned user callbacks. Complete a job when its body has finished and its
child count is zero. On child completion remove it from its parent, enqueue
that parent's `TryFinish`, then enqueue the child's callback steps in reverse
registration order. The LIFO cascade runs those callbacks before automatic
parent continuation, without recursion per tree level. Take callback storage
and release RefCell borrows before invoking user code, including immediate
registration after completion. Explicit reentrant callback actions can still
start their own cascade; no global ordering across jobs is promised.

Delete `Callback::Parent` (`job.rs:13–14`), its dispatch (`78–82`),
`waiting_child` and initializer, and the search/ChildCompletion registration
at `289–323`. The isolated C prototype removes 44 lines and adds one parent
`TryFinish` push (43 net). After B and C, delete callback marker
allocation/append (`355–358,368–370`), the Callback wrapper/Registration
indirection if no longer needed, and `Node.job: Option` plus its callback-only
branches. `Node.job` becomes a concrete child reference; `remove_node` and
iterative Drop still serve child removal/cursor retention. Budget a further
15–30 lines net, subject to the final callback dispatcher shape. Do not count
the same handle or marker lines twice across WPs.

Behavior change: automatic parent completion no longer occupies a chosen
position in a child's user-callback list. With early and late child callbacks,
`early-child,parent,late-child` becomes `early-child,late-child,parent`.
A child callback can consequently see its parent Completing for longer and
attach work that the old, already-completed parent would have rejected. This
is a consequence of the proposed order, not an observed production dependency:
DeviceActor's callback launches termination under its own completed owner, not
under that owner's parent. Add a gated callback-attachment test in the
implementation WP to pin this consequence. Explicit reentrant calls can still
finish or cancel a parent before subsequent callbacks. Parent completion
diagnostics can move relative to child callback diagnostics; the exception
handler still precedes user callbacks on its own job.
Callback registration no longer creates or promotes child nodes; C has already
made that representation unobservable to traversal. A callback-only job no
longer builds a linked node chain.

Rewrite `runtime_review::parent_completion_is_ordered_between_child_callbacks`
for the new automatic continuation order, with a named D-CQ4 native
expectation for historical `job:parent-child-callbacks`. Retain
`runtime_oracle::historical_list_and_callback_promotion_match_jvm` as historical
evidence with native behavior checked independently: traversal observes the
late child with or without a callback. Avoid claiming that callbacks still
"promote" anything. Keep the 100,000-callback release and deep completion,
cancellation and failure chain tests in `runtime_carried`; add a wide real
child-list teardown case. Preserve exception-handler-before-callback,
callback-panic and reentrant DeviceActor tests. Do not broaden a JVM allowance
to unrelated callback/deferred rows.

> **D-CQ4 draft:** rc.1 models JobSupport's parent ChildCompletion registration
> among user callbacks and callback nodes among child nodes. Child ownership
> and user callbacks now use separate storage; automatic parent continuation
> is queued after the completed child's user callbacks rather than at its old
> registration position; until then a still-completing parent can accept a
> callback's new child. Reentrant callback actions can start their own cascades. Per-job user registration order, immediate late invocation, panic
> isolation, failure ownership, and completion after body and children remain.
> Callback registration cannot affect child traversal. DeviceActor accounting,
> D48 and D52 behavior and native result goldens must remain unchanged.

### Test and fixture policy for every implementation WP

The affected runtime probes are Rust synthetic witnesses, not Kotlin Marathon
test methods listed for this WP in `docs/parity-matrix.csv`. Do not mark
unrelated Kotlin tests n/a because a Rust runtime corpus comparison changed.
For any actually mapped test retired during implementation, identify its exact
row, obtain review, and use `n/a` with `8.x: <allocated D-row>` as required by
the repository rules. Prefer preserving tests with narrow native expectations
and unchanged historical corpus files. Never edit copied fixtures. No native
golden update is proposed; any observed result-data movement blocks the
implementation pending a specific contract decision and reviewed new golden.

## 4. Implementation work packages

Provisional IDs; allocate after this design's independent critiques. Effort
uses PLAN's L/M/H scale. Every row includes a focused implementation commit,
its allocated D-row, lifecycle/design updates, narrow test migrations, full
`scripts/check.sh`, and `tools/differential/run check` before merge.

| WP | Scope | Effort | Dependency / merge criterion |
|---|---|---|---|
| 8.18 | A: collapse Parent into Requested; preserve timeout and payload adapters | L | 8.17; no changed golden, propagation and timeout assertions pass |
| 8.19 | B: make completion callbacks non-disposable; delete handles and actor retention field | L | 8.18 recommended sequence; only disposal expectations change |
| 8.20 | C: uniform lazy child traversal; delete historical list mode | L | 8.19 recommended sequence; only single-child expectations change, exact D48/D52 timing preserved |
| 8.21 | D: direct iterative parent continuation and separate child/callback storage | M | 8.20; only the named parent-order witness changes; deep/wide teardown and DeviceActor accounting remain green |

These are deliberately independently mergeable. B keeps marker nodes and
parent registrations, C keeps the spine, and D deletes the newly unnecessary
plumbing. No dependency on the image implementation, no new runtime crate,
and no transfer of vendor work onto core is needed. Total implementation
effort is three L plus one M; net savings are modest, approximately 100–125
production lines, with less allocation and less representation-driven policy.
This is an estimate of final implementation, not the diagnostic shim diffs.

## 5. Recommendation and limits

Commission A–D in that order after design review; keep lazy traversal and all
explicit lifecycle ownership. Reject L. A uniform traversal is useful chiefly
because it makes callback nodes and historical promotion unnecessary, not
because a single conditional is expensive. The substantive simplification is
D's direct child-count completion and removal of parent callback registration.

Open limits: this evidence covers the pinned workspace and registered native
suite, not arbitrary external Rust API consumers or every live-device
interleaving. The callback API and enum changes are source incompatible for
such consumers; release notes should name them. A policy that admits pools
for a fixed future window, or universally joins every future pool, remains
undecided and is outside this proposal. Exact final line savings depend on
callback storage design and must be measured by each implementation WP.
Production-output changes uncovered during implementation are not authorized
by this recommendation. Final ledger numbers and WP IDs remain for the
orchestrator to allocate, and the two independent critiques required by the
brief have not been performed by this design author.

## 6. Validation of the design branch

`scripts/check.sh` ran in the foreground to completion with the prescribed
toolchain and Kotlin source environment, exit 0:

```text
==> all checks passed
```

This includes the full workspace, 30 vendored russh client tests, plan DAG,
507-file source inventory, 1,166-row parity matrix and 143 copied fixtures.
The unchanged control also passes `tools/differential/run check`; prototype
exit codes and failures are recorded above. The YAML helper was rebuilt when
requested by the comparator. All scratch worktrees and prototype sources were
removed. Only this design and the required WP 8.17 PLAN row/pointer change are
committed; runtime code, copied fixtures and native goldens remain unchanged.
