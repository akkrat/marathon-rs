# Remaining coroutine quirks (WP 8.17, revised after review)

Design only, revised from `c956ee66` after two independent critiques and the
orchestrator's decisions. Runtime references and deletion ranges use `80edc252`
(the runtime is identical at `c956ee66`). No runtime, fixture, native golden or
parity-matrix change is committed here. `executor/` means
`crates/marathon-core/src/executor/`. See [the brief](../wp/8.17.md),
[Tokio design §7](tokio-tasks.md#7-review-outcome-orchestrator-decision),
[lifecycle contract](lifecycle.md) and [timing rules](../testing.md).

**Recommendation:** implement two WPs: **8.18 (L)** removes the unused ordinary
cancellation-tag distinction and disposable registrations; **8.19 (M)** moves
Scheduler observation to its retained pool index, replaces linked job children
with plain storage, and seals a completing parent before child callbacks can
attach more work. Keep the explicit job graph, cancellation and cleanup policy.

## 1. Corrected inventory and classifications

Production dependency means Marathon behavior or output depends on a semantic
property; an integration witness pins behavior without a contract requirement;
a synthetic witness constructs coroutine machinery directly. An internal
representation can cause production failures without being something to keep.
Classify the required semantic property separately from its current mechanism.
The first six rows below are quirks; the last is the retained domain requirement.

| Candidate | Class at the reviewed baseline | Revised decision and evidence |
|---|---|---|
| Single-node miss versus historical list mode | Synthetic coroutine witness | Retire with job-child traversal. Pools historically promote the Scheduler job to list mode before construction returns; the flag survives removal. Old S changed only three synthetic comparisons. |
| Disposable completion registrations | Synthetic coroutine witness | Retire in 8.18. DeviceActor is the only production registrant and never disposes. Old D failed only its direct disposal test and structured corpus row. |
| `Cancellation::Parent` versus `Requested` | Unused distinction, synthetic constructor surface | Retire in 8.18. Old P passed every test and native check after constructor migration. These tags do not reliably identify cancellation origin. |
| Callback markers interleaved with children | Synthetic witness **with cursor-connectivity coupling** | Retire only after Scheduler stops using job cursors. Markers affect connectivity, not just list promotion; the old claim that uniform traversal made them unobservable was wrong. |
| Parent continuation registered among child callbacks | Synthetic ordering witness with outcome consequences | Retire in 8.19, **with sealing**. Merely postponing completion lets callback-created failing work fail or indefinitely delay a parent that would already have succeeded. |
| Detached-tail cursor miss | **Production-shaped quirk / production dependency on fixing admission** | Retire. A pool published while the old traversal waits on a detached tail can be missed. Its pre-created accumulator can cause a false no-events failure; otherwise the run can return green without its tests. |
| Observing late pools during a join | **Production dependency** | Keep the semantic requirement through Scheduler's index, not through linked nodes. A single initial snapshot failed D48; this does not prove linked-node traversal is necessary. |

No candidate belongs solely to the integration-witness class. The tag distinction
has no distinguishing assertion: two tests construct Parent but require payload
and propagation, not that variant. This is a qualified synthetic classification,
not a claim of a failing tag witness.

### Why the earlier conclusions were incomplete

`RuntimeJob::join_observed_children` retains a yielded `Node`, awaits its job,
then reads `current.next`. `remove_node` unlinks that node from the live list;
removing the tail points `tail` back to its predecessor. A subsequent append
links from the predecessor, not from the retained detached cursor. Scheduler can
therefore miss it even in list mode. Do **not** add the previously proposed
removed-tail/late-new-head test that blessed this miss.

Markers sometimes preserve the missing path. In the critic's real-job probe,
P owns A and a completion marker; traversal waits on A; A completes and detaches;
B attaches. The retained path `A.next -> marker -> B` observes B. Delete the
marker and A has no successor: B is missed, including under uniform traversal.
The earlier N/R suites simply did not cover this interleaving. Their passes
cannot establish that marker removal is behavior-neutral while a job cursor is
still used. The revised marker witness must specify the new domain outcome,
not preserve a representation-dependent difference.

Production has the necessary publication window: `Scheduler::on_device_connected`
inserts its accumulator, then awaits history prefetch, then constructs and inserts
the pool. Prefetch can finish while a tail join is outstanding or after traversal
has ended. A received Connected event or an accumulator is **not** yet a published
pool. Choosing membership from the progress map reintroduces this race.

`Parent` also does not mean "inherited": `finish_body` uses it for a job's own
failure, whereas a child inherits Requested unchanged. Production readers use
both payloads identically, including Apple suppression and iOS setup-error
reconstruction. Retain their details and timeout identity, not the unused tag.

## 2. Scheduler admission and observation (decision A)

Scheduler owns the retained `IndexMap<DevicePoolId, DevicePool>`; entries are
appended once and are not removed or replaced. Its domain traversal is:

```text
i = 0; joined_progress = []
loop:
    clone (pool_id, pool) at index i; release the map borrow
    if index is absent: close this traversal's admission boundary; break
    join pool.actor.job with the caller's cancellation
    join pool.supervisor with the caller's cancellation
    retain that pool's progress; i += 1
aggregate only joined_progress, with the existing short-circuit behavior
```

**Admission contract:** every pool published in the pool index before the
traversal observes its first absent next index is joined, in insertion order.
Publication is the pool-map insertion after successful prefetch and synchronous
construction. The absent-index read is the boundary; aggregate awaits do not
reopen it. Release every RefCell borrow before either join or aggregation.
Keep caller cancellation even if a selected job has already completed.

Capture the joined pool prefix's progress references, not every accumulator in
`results`. This follows from the admission rule: the old subscription can have
inserted an accumulator and be suspended in prefetch for an unpublished pool.
That accumulator must not cause "Expected to run ... but no events received" in
the completed traversal. Missing progress for a published pool is an internal
invariant error, not permission to silently omit the pool.

This is a dynamic domain traversal, not one initial snapshot and not a job-tree
fixed point. A pool remains in the map after actor/supervisor completion, so a
late pool cannot disconnect the observer's next index. The run waits for newly
observed pools and **can end later** than the old cursor-miss run. Tests, worker
commands and result data previously omitted in that race can now be present;
previously incorrect pass/fail outcomes can change.

The contract adds no future-device grace period. A Connected event processed
and published before the boundary is included. If final completion wins and the
absent index is read first, later publication is outside this result. If history
prefetch is gated until after the boundary, its earlier accumulator is excluded.
The run-owned subscription and Scheduler's detached ownership stay as today;
publication after this result does not retroactively reopen it. Existing
Marathon/provider cleanup remains responsible for stopping that work. This WP
does not add a new admission-closed flag that changes connection tracking or
provider termination policy.

D48 still sets each pool's own wait from its specified departure/test event,
including cleanup, non-ready returns, vendor-used wait and flapping limits.
Dynamic observation joins a second pool; it does not reset the first one's
limit. D52 still cancels the caller's wait and retains its independent bound.
D34 history snapshots/shutdown, D50 batching and D54 transfer behavior stay.

### Prototype A: scope, commands and result

The scratch prototype replaces only Scheduler's observed-job loop with the
pool-index loop above and aggregates the joined prefix. It retains baseline
RuntimeJob internals and every existing assertion/fixture. This isolates the
replacement mechanism instead of changing its old synthetic tests to pass.
Scratch-only timestamp output and a virtual D52 probe add assertions; they do
not weaken an existing test.

With the prescribed Rust toolchain on PATH and `MARATHON_KT` set to the read-only
Kotlin checkout:

```sh
cargo test --workspace --no-fail-fast
cargo build -q -p snakeyaml --example yaml_tree
tools/differential/run check
cargo test -p marathon-core --test pool_scheduler d48 -- --nocapture
cargo test -p marathon-core --test run_flow_witnesses d52 -- --nocapture
```

Prototype A completed successfully:

| Run | Result |
|---|---|
| Full workspace, `--no-fail-fast` | Exit 0; no test failures |
| Critical integration targets | `pool_scheduler`: 31 passed; `run_flow_oracle`: 17 passed; `run_flow_witnesses`: 27 passed (the existing 74 plus one scratch D52 probe) |
| Native golden/owner check | Exit 0; `PASS profile=registered-offline-native (whole vendor baselines plus scoped owner contracts)` |
| Focused D48 | 19 passed; late-pool end printed `Some(201000)` |
| Focused D52 | 7 passed; both `reports_hang=false` and `true` printed `exit_at=[24000]`; SIGTERM status 143 |

No assertions, copied fixtures, native goldens or allowances were changed to
obtain this result. There were no test or golden failures to report. These
are offline registered comparisons, not new live-device captures. The exact
D48/D52 results preserve their existing limits while dynamic observation
replaces the linked-node mechanism.

The old snapshot experiment remains useful **negative** evidence: seven
workspace failures included `pool_scheduler::lazy_child_traversal_joins_a_pool_created_mid_traversal`,
`pool_scheduler::a_pool_created_during_a_wait_is_joined_and_the_run_waits_for_both_d48`
and `run_flow_oracle::two_pools_disconnect_matches_the_jvm_transcript`, plus four
synthetic traversal comparisons. Its native check failed the last contract too:
62,100 ms rather than 64,100 ms with the D48 allowance. It disproves a single
snapshot, not Scheduler-owned dynamic observation.

## 3. Job completion and storage (decision B)

Once Scheduler no longer uses a job cursor, use a plain ordered collection of
strong child references, for example `Vec<JobRef>`, with stable removal.
`children_snapshot` remains for cancellation and Marathon's child cancellation
path. It becomes a collection clone. Delete `join_observed_children` and its
caller-cancellable wrapper: Scheduler is their only production caller. Migrate
or retire the direct synthetic traversal probes explicitly; do not leave a
second production traversal API to preserve library representation witnesses.

Callbacks use separate owned FIFO storage. After 8.18 there is no disposable
handle, so `Registration`, its optional callback cell and the User/Parent callback
wrapper can go when unnecessary. Preserve immediate registration after completion,
per-job order of already-registered user callbacks, panic isolation, weak actor
capture and exception-handler-before-user-callback order on the same job.
Cross-job callback and diagnostic/log ordering can move.

### Seal at the count-zero transition

Do **not** reproduce the previous C/R design's interval in which the last child
has detached but a body-finished parent still accepts children. In the **same
mutable borrow** that removes the last child, test the completion predicate;
if the body is finished and children are empty, set `completed = true`, freeze
the outcome and take the callbacks. Apply the same predicate when the body
finishes with no children. Never publish completed and take callbacks in two
separate borrows.

A completion packet owns the job, frozen outcome and taken callbacks. Separate
membership/seal preparation from callback dispatch: detach a completed job and
seal its eligible parent before executing user callbacks, and prepare any
resulting ready ancestor chain iteratively as well. Do not leave a sealed child
counted as pending merely because its callback packet has not been dispatched;
that would let a grandchild callback extend an otherwise finished ancestor.
No inner borrow crosses a handler or callback invocation.

Then dispatch the prepared packets child before parent: run each failure handler
at its existing ownership boundary, notify waiters and dispatch its taken user
callbacks. A `Finalize` step can carry a prepared packet; finalization must not
reopen attachment or postpone membership removal until after child callbacks.
Preserve failure propagation/first-reason decisions before freezing outcomes.
There is no recursive parent chain or ChildCompletion registration. A join
observes the sealed outcome; ordinary core tasks cannot interleave a poll inside
the synchronous cascade. Reentrant callbacks use the normal public entry points
without any held inner borrow.

Construction against a sealed parent takes the existing completed-parent
rejection path. A spawned task is already cancelled before its first poll and
**never enters its body**. Thus a callback attaching failing work cannot change
parent Success to Failed or extend that parent's completion. The earlier
prototype that admitted an active child is counterevidence to the old proposal,
not behavior to preserve. Replace it with a failing-child rejection witness.

A body-finished parent with another child pending remains unsealed. An attachment
while it is Cancelling follows the existing inherited-cancellation path, never
becomes runnable body work, and must not consume another child's count. A rejected
child was never in the collection: its completion must not decrement membership.
Ordinary failure still wins over cancellation; supervisor child failure remains
isolated. Reentrant cancellation with a pending child must wait for that child's
cleanup and seal exactly once, preserving the first winning reason.

A separate focused seal prototype retained linked child storage and the old
registration API to isolate count-based completion. It removed ChildCompletion
registration, sealed/took callbacks inside the last-child-removal borrow, and
queued direct-parent callback finalization after child callbacks. This isolates
the direct-parent seal; it does not validate the full eager ancestor-preparation
phase above. It is **not** a finished 8.19 Vec/API rewrite and was not substituted
for A during its full-suite run.

All four new `runtime_review` tests passed:

- `wp817r_seal_rejects_callback_failing_work`: ordinary and supervisor parents,
  with the child callback registered before and after parent.complete; the
  parent is already Success, the attempted failing task never enters, and
  the parent callback runs once.
- `wp817r_admitted_failure_and_supervisor_isolation`: genuinely admitted work
  can still fail an ordinary parent, while a supervisor isolates that failure;
  both wait for a separate gated child.
- `wp817r_cancelling_attachment_never_enters_and_keeps_pending_child`: a
  cancelled late spawn never runs, and cannot remove the real pending child's
  membership or change the first reason.
- `wp817r_reentrant_cancel_waits_for_the_other_child`: cancellation from a
  child callback does not complete the parent before the other child's gated
  cleanup or cause a RefCell borrow panic.

The three existing `runtime_carried::deep_chain_*` cancellation, completion and
failure tests also passed. Both targeted commands exited 0; no failures.
The remaining production graph/admission witnesses below are still required
in the actual implementation; these focused tests do not establish full
combined-runtime or plain-storage parity.

### Deletions and test migration

Physical ranges below refer to baseline `executor/job.rs` and identify removed
mechanisms, not a promise that moved finalization code vanishes:

| Mechanism | Baseline lines / replacement |
|---|---|
| Disposable handles | `20–32`, handle construction `371–376`, boxed return `476–481`; delete `runtime.rs:74–76`, DeviceActor's stored handle and the contracts test's handle field in 8.18 |
| Node and iterative linked-list drop | `33–50`; delete in 8.19, not retain a child-only spine |
| Linked append and historical mode | `164–178`, `Inner` head/tail/list-mode fields and initializers; replace attachment with plain collection insertion |
| Linked removal | `254–287`; replace `remove_node`/`remove_child` scans with stable collection removal and the seal predicate |
| Parent continuation registration | `Callback::Parent`/dispatcher, `waiting_child`, first-child search/registration `289–323`; replace with count-based seal/finalization packets |
| Cursor APIs | `join_observed_children_in` `393–402`, `children_snapshot` `443–453`, observed traversal `454–475`; remove the joins and replace snapshot with collection clone |
| Callback marker nodes | allocation/append `355–358,368–370`; callbacks no longer add child-list nodes or maintain cursor connectivity |

The earlier isolated diagnostic reductions (3 lines for tags, 5 for inert
disposal, 11 for uniform traversal, 3 for no marker append, 43 for deferred
parent continuation) are historical measurements, **not** the revised net
saving. The revised design deletes the linked spine and adds sealing/domain
traversal. Measure final net lines in each implementation WP; do not carry
forward the old 100–125 estimate or count moved completion logic as deleted.

8.18 rewrites only disposal/tag expectations: keep the immutable structured
corpus's `job:callback-order = 1,3`, assert native `1,2,3` with the allocated
D-row, and retain once/late invocation checks. 8.19 replaces the runtime traversal
oracles with domain admission witnesses; retain original corpus files as history.
Split mixed coroutine corpus tests: retain every timeout, deferred, failure and
other non-retired row. Remove only the specifically named traversal/disposal
comparisons with the allocated D-row explanation, never the whole corpus.
The parent-order witness expects queued parent finalization after child callbacks,
with the parent **already sealed**, and includes failure rejection. Retain all
100,000-depth cancellation/completion/failure and many-callback tests, adding
wide child storage teardown. Keep DeviceActor accounting-before-Terminated,
already-cancelled construction, handler retention and timeout cleanup tests.

These are Rust synthetic probes, not Kotlin test methods newly assigned to this
WP. Do not mark unrelated parity rows n/a. If an implementation actually retires
a mapped test, identify the exact row, obtain review and use `8.x: <D-row>` as
required. Never edit copied fixtures. No golden update is proposed for the
registered cases; any newly covered result-data change needs a reviewed golden.

## 4. Required 8.19 witnesses (decision D)

Use explicit gates and manual polling/controlled replay. Equal timestamps are
not an ordering mechanism. A test may hold Scheduler's execute future unpolled
while tracked core/vendor work drains, then poll it deliberately; a gated test
prefetch adapter may hold publication. No new production replay tape label or
host-time deadline is needed. Use 60 s only as an external hang guard.

| Witness | Controlled ordering and intended new outcome |
|---|---|
| Marker connectivity | Reproduce the critic's P/A/marker/B sequence against the old runtime as historical evidence. For the replacement, publish domain pools A and B while the observer is held after A's completion. B is joined **with or without a user completion callback/marker**; plain job snapshots contain only actual attached children. The old negative control is scratch-only; the committed witness requires B joined in both cases. Do not preserve the old "marker removed means B missed" outcome or claim the old marker was unobservable. |
| Tail-join late pool | Hold A's supervisor cleanup (the old Scheduler-list tail); start joining it; complete/detach it while holding the traversal continuation. Publish B, then release the continuation. Scheduler reads the next retained pool index and joins B before aggregating; no no-events warning, no green completion without B's tests. |
| Connected/publication wins | Hold final A completion. Deliver Connected(B), release and acknowledge B's prefetch/publication, then finish A. B is in the admitted prefix and must finish before execute returns. |
| Final completion wins | Complete A and explicitly let traversal observe the absent index before delivering Connected(B). The captured result is unchanged; later publication is outside that traversal. No retroactive join. |
| Connected received, prefetch finishes late | With A still live, deliver Connected(B), observe its accumulator creation and hold its history prefetch. Finish A, let traversal end, and assert B's unpublished accumulator does not enter aggregation or cause a no-events failure. Then release prefetch; publication remains outside the captured result. Also cover prefetch failure/cancellation without publication. |
| Failing attached work after seal | A body-finished ordinary parent loses its last child. A child callback tries to spawn failing work under it; register that callback both before and after parent.complete. Assert Completed(Success) before/after draining, the new task is cancelled, its body entry counter is zero, callbacks run once and joins finish without that work. Repeat through a body-finished ancestor chain so a sealed-but-undispatched child is not mistaken for genuine pending work. |
| Ordinary failure before seal | Attach while a genuine child remains pending; let admitted work fail before the seal. The ordinary parent fails, cancels siblings and waits for their cleanup. This prevents over-sealing every Completing parent. |
| Supervisor isolation | Repeat admitted-child failure under a supervisor with another gated child: failure is isolated, supervisor stays Completing until the pending child ends, then succeeds. A failure attempted after seal is rejected and cannot alter success. |
| Attach while Cancelling | Hold a cancelled task's cleanup so another child remains pending. Attempt attachment; it inherits cancellation, never enters its body, and its removal cannot steal the pending child's membership. Parent remains Cancelling until real cleanup finishes, then seals once with the original reason. |
| Reentrant cancellation, other child pending | In one completed child's callback call parent.cancel while a second child is gated in cleanup. Assert no RefCell reborrow panic, no early parent completion, first reason/failure precedence, once-only callbacks and completion after releasing cleanup. |
| Already-cancelled spawn | Spawn under an already-cancelled parent before any first poll (both pending-cleanup and completed-parent shapes). A body-entry sentinel remains untouched; the child completes cancelled and cannot fail its parent. |

Keep the D48 201,000 ms late-pool assertion and D52 24,000 ms virtual-bound
assertions alongside these gates. Report exact timestamps, not tight elapsed
wall-clock limits. The seal tests must exercise both an ordinary parent and the
production actor/supervisor graph before 8.19 can merge. The no-body-entry
witness concerns spawned TaskBody work: RuntimeActor.started still retains its
documented synchronous invocation even under an already-cancelled parent.

## 5. Two implementation WPs and D-row drafts (decision C)

The old four-WP split and four D-row drafts are superseded. Draft labels below
are not allocated ledger numbers; assign free numbers at implementation time.
The workspace has `publish = false` and no supported external consumers.
Public Rust API removal still needs explicit documentation and migration of all
workspace callers; do not invent an external-compatibility support requirement.
Each WP updates its allocated PLAN §3.11 row, the lifecycle contract and narrow
test/parity explanations together with the implementation.

| WP | Effort | Focused commits and merge conditions |
|---|---|---|
| 8.18 | L | (1) Collapse the unused ordinary cancellation tags and migrate Apple/iOS payload adapters and synthetic constructors. (2) Remove disposable registrations, the public CompletionRegistration trait/handle, and stored handle fields; preserve weak DeviceActor callback retention. Include the WP's one D-row, narrow disposal expectation migration, gate and native checks. Depends on 8.17. |
| 8.19 | M | (1) Scheduler dynamic pool-index observation, admitted-prefix aggregation and gated admission regressions. (2) Plain child and separate callback storage, iterative count-based sealed completion, and deletion of cursor APIs; include failure/reentrancy/first-poll witnesses. The first commit can retain baseline job internals and must be green; the second must include the seal, never ship an attach-during-completion interval. Include the WP's one D-row, full gate and native checks. Depends on 8.18. |

> **D-CQ1 draft — WP 8.18:** The coroutine adapter carries an unused
> Parent/Requested ordinary-cancellation tag distinction (Parent is not a
> reliable origin tag) and exposes disposable completion registrations.
> Ordinary cancellation now uses Requested with the same failure payload;
> timeout scope identity, failure precedence, propagation and supervisor
> isolation are unchanged; internal enum/debug tags can now show Requested
> where they showed Parent. Registrations are retained until exactly-once
> completion, cannot be withdrawn, and still invoke immediately after completion.
> The public CompletionRegistration API and its handles are removed; workspace
> callers migrate, with no supported external consumers. DeviceActor's weak
> callback, accounting/termination order, configuration, worker commands,
> result data, progress and summary stay unchanged. CLI exit codes and their
> mapping are unchanged. Native goldens must stay unchanged.

> **D-CQ2 draft — WP 8.19:** rc.1 observes Scheduler work through JobSupport's
> historical single/list representation, mixed child/callback nodes and lazy
> successor cursor, including a detached-tail miss; parent continuation occupies
> a registration position among child callbacks. Scheduler now joins each actor
> and supervisor in its retained pool index until it reads an absent next index,
> and aggregates only that joined prefix. Accumulator creation/Connected receipt
> before prefetch completion is not admission. The run waits for late pools it
> now observes and can end later; tests, commands and result data previously
> omitted by the miss can now be produced, and spurious no-events failures or
> incomplete green outcomes are corrected. D48 per-pool limits and D52's bounded
> cancellation remain. Job children use plain storage, callback markers and
> observed-child cursor APIs go, and count-zero body-finished parents seal their
> outcome and take callbacks atomically before reentrant attachment. Work
> attempted after seal is rejected and cannot delay completion or turn Success
> into Failed. Registration order within each taken callback packet, immediate
> late invocation and panic isolation remain; parent/child callback finalization and diagnostic/log ordering move. The public
> CompletionRegistration API was removed in 8.18 and is not retained or replaced
> with a disposable API. CLI exit codes and their mapping are unchanged; an
> affected run's corrected pass/fail outcome can select a different existing
> status. Inputs and command construction are unchanged; pinned progress and
> summary text formats remain unchanged. Newly included tests can add progress
> lines and change result counts. Registered native goldens must stay unchanged,
> and any additional result-data change requires a reviewed golden update.

## 6. Recommendation, agreement and validation

Adopt decisions A–D. The critiques found real holes: passing the old synthetic
suite was insufficient to establish cursor connectivity, and postponing parent
completion changed outcomes as well as ordering. Keep the dynamic observation
requirement, not the linked-node mechanism. Do not retain the tail miss or allow
callback-created work to extend a parent after its last child is removed.

There is no disagreement with these decisions. Two consequences are made explicit:
(1) the admission rule also requires admitted-prefix result aggregation because
accumulators precede publication; (2) "exit codes unchanged" means the existing
codes and mapping policy remain, not that a previously incorrect green or failed
outcome must be preserved. The supplied production miss already permits both
wrong outcomes. Correcting them can select a different existing exit status.

Final net deletion counts, ledger allocation and full 8.19 combined-runtime
validation belong to implementation. Prototype A isolates Scheduler; it does not
claim to have validated a finished Vec/seal rewrite. Required new witnesses above
are merge requirements, not claims that the old test suite already covers them.
No new live-device or real-capture certification is claimed. WP 8.16 remains
independent.

The revision's `scripts/check.sh` ran in the foreground to completion, exit 0:

```text
==> all checks passed
```

The gate checked the full workspace and 30 vendored russh client tests, the plan
DAG, the 507-file source inventory, the 1,166-row parity matrix and 143 copied
fixtures. Prototype A's independent native check and exact timing results are
recorded above. All scratch source worktrees and build artifacts were removed.
Only this design revision is committed; runtime code and fixtures are unchanged.

## 7. WP 8.19 implementation review

D65 keeps a sealed completion packet's FIFO reachable until dispatch finishes.
A child callback registering on a sealed parent appends after its earlier
registrations; registration during that parent's own dispatch also appends.
Only registration on a fully dispatched job invokes immediately. The seal and
attachment rejection remain atomic; dispatch state does not reopen admission.
Failure handlers, like callbacks, are panic-isolated so later packets still run.

The whole synchronous transition cascade, siblings included, is prepared before
callbacks are dispatched. A callback observes, and cannot change, a sibling's
prepared outcome. For example, cancellation prepares both A and B as Cancelled
before A's callback tries B.complete; B stays Cancelled, whereas rc.1 could let
that callback complete B successfully first. This outcome change is accepted in
D65, beyond the parent/child callback and diagnostic ordering changes.

U193 records the pre-existing incomplete-tree destruction overflow. An iterative
RuntimeJob drop drains the children of uniquely owned jobs without completing,
cancelling or invoking callbacks; externally shared children keep their trees.
