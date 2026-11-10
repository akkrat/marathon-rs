# Core lifecycle and interface contract

WP 8.8's [logging design](logging.md) describes production logging: the
`tracing` subscriber, the per-configurator filter, the non-blocking console
writer and the shutdown/flush deadlines. It supersedes the registry, layout
and level mechanics in the sections below; the ownership and queue-order
rules here still apply.

Frozen by WP 0.6 against Kotlin `e47352f2`. Paths below are relative to
`core/src/main/kotlin/com/malinskiy/marathon/` unless stated otherwise.
WP 3.1 implements the runtime; 3.6–3.8 implement the actors. This document
specifies observable behaviour, not a claim that a runtime exists yet.

## Executor and ownership

One dedicated thread owns a Tokio `current_thread` runtime and `LocalSet`.
All core state transitions and mailbox receivers run there. `LocalFuture`
is deliberately not Send. The vendor/I/O runtime is multi-threaded;
`VendorFuture` is Send. The bridge starts vendor work there and resumes its
awaiting core task on the core executor. It never polls vendor work on core
merely because the future happens to implement Send.

### Explicit execution context and construction (R1-sol-1, R1-wonder-1/3)

`CoreRuntime::spawn` and `spawn_cleanup` take `TaskBody`: a closure receiving
`TaskContext` with the **new task's** job, core runtime and scheduler clock.
`ActorHandler::receive` receives `ActorContext<M>` with that same information
for the executing receiver child and its self-handle. `started` receives the
owner job's task context and runs once during construction,
before any handler dispatch, so DeviceActor can install its own-job completion
callback even when cancellation prevents the first message from running
(`DeviceActor.kt:111–117`). Registering on an already-completed job immediately
observes its completion. Retain completion registrations until disposal; do
not capture a strong actor self-handle in a callback owned by its own job.
The runtime retains actor bookkeeping through completion, then releases the
handler/context to avoid reference cycles. Actor constructors are in-crate
WP 3.1 work, with explicit parent/runtime/clock/handler arguments.

Actor-scoped children launch with `context.actor.job()` as parent, even from
inside receive; ordinary task children launch with `context.task.job`. The
separate pool supervisor is created with the source parent, not automatically
under the actor job.
`SchedulerClock::delay(millis, job)` observes the **explicitly supplied job**.
Suspending handler code passes its receiver's `context.task.job`; spawned
task code passes the child's context job; timeout code passes the timeout body's context job. Cancellation of that
job ends its delay, preserving the cancellation reason. No ambient global or
thread-local job is inferred from the executor thread. These rules make
self-close, invokeOnCompletion, retry delay and child cancellation expressible
without changing the Kotlin parent graph.

Task, cleanup and handler bodies return `OperationResult<()>`, distinguishing
`OperationError::Cancelled(Cancellation)` from `Failed(Failure)`. `?` converts
an awaited Cancellation into the former, preserving its scope/reason. The
runtime maps these to the corresponding Completion variants; cancellation
never becomes an ordinary child failure that cancels its parent/siblings
(`Retry.kt:13–15`, `DeviceActor.kt:193–205`). Cleanup has its own protected
context; caller cancellation cannot abort it, but an independently cancelled
operation it awaits can still return typed cancellation.

`DeferredFactory`, `EventChannelFactory` and `ActorFactory` are removed.
WP 3.1 supplies concrete, generic in-crate constructors for Deferred<T>,
EventChannel<T> and actors. They take explicit runtime/parent/context inputs
as applicable and do not rely on service lookup. Their generic factory traits
are not dependency handles. `TimeoutScope` is object-safe and is reachable
as `RunDependencies.timeouts`; typed output crosses its unit-body seam in a
captured local cell or Deferred<T>, just as typed vendor results cross the
erased VendorRuntime task seam. `nested_timeout_contract` demonstrates a
Vec<Test> result without a generic trait object.

Kotlin `Marathon.run` uses `runBlocking`, `supervisorScope`, then `async`
(`Marathon.kt:71–90`); Scheduler receives the current context (`:191–201`).
Actor contexts replace that context's job with their own `Job(parent)`
(`actor/Actor.kt:23–35`). These job relationships must not be inferred from
Rust lexical ownership or the thread on which a task was created:

- The run's supervised child owns the subscription launched by
  `Scheduler.subscribeOnDevices` (`execution/Scheduler.kt:74–86`).
- Scheduler's `Job()` is **detached** (`Scheduler.kt:45`). The subscription
  receives it as an argument for pool creation; it is not its child.
- Each pool actor's ordinary actor job is a child of the detached job
  (`DevicePoolActor.kt:35–39`). Its separate `SupervisorJob(parent)` is also
  a child of that detached job (`:61`), not of the pool actor's job.
- The queue and device actor jobs are children of that supervisor
  (`:63–74,308`). Each actor's receiver is a child of its own actor job.
- Device prepare/execute/readiness/termination tasks are children of the
  device actor job (`DeviceActor.kt:154–218`). The result Deferred created
  at `:59` is parentless. Cancelling an awaiter must not seal that result.


`ActorHandler::started` has a provided no-op default; override it only when the
actor needs construction-time bookkeeping. A spawned child always receives
its own child `TaskContext`, so delay and cancellation inside its body use that
context. When a state-transition side effect must launch a **sibling** under
the actor job (for example readiness launched by a transition from an execute
task, `DeviceActor.kt:154–157,188–208`), keep the owner context received by
`started`, or save `context.actor.job()` with the core/clock services in shared
actor/state-machine state. The side effect launches through that saved
owner's `core.spawn(owner_job.clone(), ...)`; the new body still receives
its own child context. Do not substitute the currently executing child's job
for the actor job: that would change cancellation and completion ownership.

No filesystem calls, process waits, blocking locks, synchronous network I/O,
thread sleeps, report writes or long CPU scans run on core. Blocking library
work goes through the vendor runtime's blocking pool. Cancellation of its
awaiter is not proof the blocking operation has stopped: the owner tracks
and joins it where Kotlin does. Async close/dispose/report methods represent
completion of that work; Rust Drop must not run observable cleanup. Ordering
is retained by awaiting off-core work at the original source boundary.

## Jobs, deferred results and cancellation

`runtime::Job` exposes distinct Active, Completing, Cancelling and Completed
states; `JobKind` distinguishes ordinary and supervisor parents. Ordinary
child failure cancels its parent and siblings. Supervisor child failure is
isolated; cancelling the supervisor still cancels its children. Cancellation
exceptions do not count as ordinary child failures. Detached jobs have no
parent. A job is completed only after its body and children finish; cancel
requests are idempotent and may precede completion by arbitrarily long I/O.

`cancel_and_join` requests cancellation and suspends until completion.
`join`, `cancel_and_join` and `join_observed_children` are protected waits:
they do not infer a caller or rethrow the joined job's failure. Ordinary
cancellable callers use `join_in(context.job.clone())`, preserving the caller's
cancellation reason through its explicit `cancellation_signal`. A cancellable
observed-child traversal, `join_observed_children_in(caller)` (an additive
provided method, WP 3.6 carried item C3), races the protected traversal with
that same explicit signal. Completion callbacks run exactly once,
also when registered after completion. Callback registration returns a
removable registration. Callbacks mutate core state on core; an I/O thread
may only enqueue a wakeup. Normal completion of an ordinary stand-alone Job
requires explicit completion; finishing its children alone does not complete
it. A body returning cancellation signals its own job and cancels its children,
without failing its parent. When a completing parent waits on a child, its
completion continuation occupies that registration position among the child's
callbacks (early-child → parent → late-child in the pinned JVM probe).
The actor's close path cancels its owner job; the receiver is a separate child.
A receive that throws cancellation closes the mailbox and completes the receiver
without completing the owner. Cancelling only the receiver also interrupts a
handler's delay through its receiver task context, preserving cleanup and leaving
actor-scoped siblings active. Mailbox closure is independent of owner completion.

Scheduler executes `for (child in job.children) child.join()`
(`Scheduler.kt:67–69`), not `job.join()` and not a wait-until-no-children loop.
The runtime offers a children-at-observation snapshot, but Scheduler must use
the lazy observed-child traversal. In the pinned kotlinx-coroutines-core-jvm
1.8.1, `JobSupport$children$1.invokeSuspend` reads state once: a single
ChildHandleNode yields one child then stops; a NodeList retains its head/current
node and obtains current.nextNode **after** each yielded child is joined. Thus
newly attached list nodes can be observed, but later children are not universally
joined. Completed/removing nodes follow that library's traversal semantics.
This was checked with the reference JDK's `javap -c -p` on the pinned jar:
state read at bytecode 48–55, single child 56–98, list yield 180–218, successor
read 262–269. WP 3.1 ports this traversal, not a fixed snapshot or fixed point.
The snapshot API is for callers requiring a snapshot; Scheduler must use the
separate observed-child traversal contract. Do not reparent the subscription,
join all future pools, or terminate the provider here to “fix” this race.

`runtime::Deferred<T>` is shared and first-writer-wins: successful value,
exceptional completion or cancellation seals it. It returns a Boolean for
whether completion won, allows repeated await and synchronous inspection of
an already-completed result. Awaiting an already-completed cell returns its
stored value, failure or cancellation before checking waiter cancellation;
a suspended successful await observes prompt waiter cancellation. For a suspended
exceptional await (including stored cancellation), the first notification wins:
exceptional completion before waiter cancellation returns the stored exception;
waiter cancellation before exceptional completion returns the waiter's reason,
even if both arrive before the waiter resumes. Dropping one handle or cancelling one await
must not cancel the cell. `Device.execute` gets the writable cell and returns
a separate future. Successful return without a value is legal; reporting a
value before returning an error is legal. Neither is collapsed into
`Result<TestBatchResults, VendorError>`.

`DeviceActor.kt:138–151` first attempts to seal the result with cancellation,
then reads whichever completion won. It synchronously `trySend`s either the
completed results or the whole returned batch. Exactly one transition leaves
Running (`:66–72`); even failure before the execute task first polls must
account for that batch. The job completion callback forces Terminate and
calls the once-only termination notifier (`:111–117,221–224`). Explicit
termination cancels/joins prepare, then execute, notifies, then closes
(`:211–218`). These are separate paths sharing the same once-only guard.

## Mailboxes, state machines and ordering

Actor mailboxes are unbounded and FIFO in enqueue order (`Actor.kt:29–35`).
There is no global order across independent senders, vendors, or pools.
`send` can fail on closure; `try_send` returns the rejected message;
`safe_send` skips a mailbox already closed. Kotlin's check followed by send
(`actor/extensions.kt:8–10`) can race with close: it is not an unconditional
swallow-all-errors primitive. The pool uses atomic try-send instead
(`DevicePoolActor.kt:231–241`).

Actor close **cancels** the job and discards unread messages (`Actor.kt:50–52`),
not a graceful channel drain. Cancellation also interrupts a suspended
receive handler. Core bookkeeping remains available for completion callbacks.
Provider channels differ: explicit close preserves buffered events until
received, whereas cancel discards them. `receive_in` checks channel cancellation,
then a buffered element, then drained closure before considering the waiter's
cancellation. If it would suspend, waiter cancellation takes precedence over
later readiness, including closure. `join_in` intentionally differs: even an
already-completed job checks the joining caller's cancellation. `subscribe()` is a shared consuming
channel, not a broadcast subscription; borrow happens before consuming it
(`device/DeviceProvider.kt:12–19`).

Required order, with source evidence:

| Order | Frozen source |
|---|---|
| Store new state → exit listeners → enter listeners → transition listeners; first matching transition wins | `actor/StateMachine.kt:46–65,73–80,128` |
| Outcome enqueue before Terminated; readiness is launched asynchronously and can be lost on cancellation | `DeviceActor.kt:131–157,211–224` |
| Reserve a device request before sending; stale Device/DeviceActor identities cannot claim assignments | `DevicePoolActor.kt:129–139,164–175,189–203,247–260` |
| Parked devices retain insertion order; other workers ask before the failed worker | `DevicePoolActor.kt:99,154–168,209–212` |
| Completed/ReturnBatch precedes ConfirmOutcome; queue's Notify precedes OutcomeProcessed | `DevicePoolActor.kt:122–126,148–160`; `queue/QueueActor.kt:88–102,193–199` |
| Queue checks emptiness and records active batch without suspension between them | `queue/QueueActor.kt:324–335` |
| Observe durations before handling finished → uncompleted → failed; remove active batch, then notify | `queue/QueueActor.kt:106–123` |
| RejectBatch uses batch **reference** identity; requeues without an attempt | `queue/QueueActor.kt:174–186` |
| Pool sends IsEmpty after earlier queue messages and waits for its response **or queue job completion**, then cancels supervisor and closes itself | `DevicePoolActor.kt:263–280` |
| Sequential composite listener calls and closes; no failure isolation | `execution/listener/CompositeLineListener.kt:3–12` |
| Attachment listeners run in registration order | `report/attachment/AttachmentProviderDelegate.kt:7–15` |

The WP 1.1 reference-heap requirement is superseded by D60: test queues use
`marathon_core::strategy::queue::OrderedQueue`, extracting keys once per insertion
and iterating in priority order with FIFO ties. Rust hash-table
iteration is not Java iteration. Preserve insertion order where Kotlin uses
linked maps/sets; do not promise a total order for ConcurrentHashMap pools.

The ordered queue takes synchronous insertion-time keys (D60). WP 3.2 P2
supersedes 0.6 D4 for queue construction: prefetch each pool's declared
history keys off core before building its queue. Key functions read memory
only, so queue operations stay on core; see model-contract.md.
RemoveDevice compares identity too: Kotlin removes by serial alone
(`DevicePoolActor.kt:283–290`, C27), and D40 fixes that stale-disconnect bug.

## Timeouts and error control flow

All configured millis are signed i64 and Java durations remain signed.
Do not silently clamp negative values into `std::time::Duration`. Scheduler
time controls delay and timeout; wall time controls Timer and report dates;
monotonic time controls explicit nanoTime deadlines/cache ages. They are
separate services. SystemTimer deliberately measures **wall** time and may
move backwards (`time/SystemTimer.kt:6–19`); arithmetic wraps like Java Long.

A timeout owns a child job and cancellation cause. `withTimeout` fails with
that scope's Timeout; `withTimeoutOrNull` translates only its own timeout,
not a nested timeout or cancellation from outside. Before returning it waits
for cancelled work's cleanup where Kotlin structured concurrency does.
NonCancellable cleanup is explicit; runtime cancellation cannot just drop a
future containing that cleanup. A runtime implementation must resume the
cancellation path or register cleanup independently and await it. Simultaneous
completion/timeout is a scheduling race in live runs and an ordered event in
replay. No new deadline is imposed on an unbounded Kotlin join.

`TimeoutScope::with_timeout` returns its own expiry as
`Err(OperationError::Cancelled(Cancellation::Timeout { scope, .. }))`; an inner
timeout propagates unchanged with its original scope.
`TimeoutScope::with_timeout_or_null` returns `Ok(None)` for only its own
invocation's expiry, `Ok(Some(()))` for successful completion, and preserves
inner/external cancellation and ordinary failure as Err. Each invocation
allocates a fresh scope identity and supplies a child TaskContext to its body;
that child's delay and nested tasks observe its cancellation. Both timeout
methods await cancelled body/child cleanup before returning. The runner is
wired once per run and shares the core scheduler clock. It is not a generic,
unusable factory service.

`VendorDependencies.clock` and `VendorContext.clock` are the same per-run
`Arc<dyn VendorClock>`. This Send + Sync service exposes Send delay,
with_timeout and with_timeout_or_null futures; the timeout closure receives
its **child** cancellation signal, which replaces the parent's in nested
VendorContexts. Delay observes the signal supplied to that call. Timeout
expiry/cancellation/cleanup ownership follows the same rules as core, with
own expiry expressed as
`Err(VendorError::Cancelled(Cancellation::Timeout { scope, .. }))`.
Vendor `with_timeout_or_null` catches only its own expiry as `Ok(None)`;
inner timeouts propagate unchanged, retaining their original scope, as do
external cancellation and ordinary failures. This covers
`StubDevice.kt:47` and Apple's `withTimeoutOrNull` at
`AppleSimulatorDevice.kt:519,921`. Typed outputs use captured cells/Deferreds.
The core and vendor scheduler clocks share one scheduler timeline. In replay,
ReplayDriver::advance_to advances **both**, and drain_current/quiescence
includes runnable vendor timer continuations and their core bridge callbacks.
No driverless virtual clock or automatic advance is permitted; WP 3.1/7.1
must test cross-executor delay/timeout advancement. Live clocks can dispatch
through Tokio, but direct tokio::time calls bypassing this seam are forbidden
for replay-controlled operations. Wall/monotonic tape clocks stay independent.

Scheduler's first-device wait is a 100 ms polling loop under the configured
initialization timeout; timeout cancels/joins the detached job and throws
NoDevices with an empty message (`Scheduler.kt:55–66`). Remote parse uses
three attempts and zero retry delay under `withTimeoutOrNull`
(`Marathon.kt:130–143`). Preparation uses 30 attempts and 10 s delay; retry
never catches cancellation (`execution/Retry.kt:7–23`, `DeviceActor.kt:165–183`).

| Vendor execute outcome | DeviceActor effect (`:189–207`) |
|---|---|
| Normal return | Complete transition; account for reported result or return batch |
| Cancellation / leave-pool / DeviceLost | Terminate; preserve an already-reported result |
| TestBatchExecution | Try exceptional result with batch-failed reason; Complete, device reusable |
| Other error (including unexpected throwable) | Try exceptional result with unknown-vendor reason; Complete, device reusable |

Apple NotBooted/NotAdmitted are cancellation subclasses, not preparation
failures to retry (`vendor/vendor-apple/ios/.../AppleSimulatorDevice.kt:1198–1204`).
`VendorError::LeavePool` preserves this distinction. Error details retain
class, nullable message and nested cause; generic I/O errors must not erase
transport causes required by the SSH spike. Transport timeout/null exit,
consumer failure, cancellation of await and explicit session close stay
separate. Await cancellation does not kill the remote process. WP 4.2 must
implement the full spike checklist, including 30 s close wait versus unbounded
join and the unresolved T07/T14 await/close ordering; this freeze does not
claim that a generic cancellation token solves those cases.

## Run completion and construction

CLI matches the parsed `VendorConfiguration` and explicitly builds vendor services with config,
runtime and shared clocks/hooks. No service locator or process-global Koin
replacement. `VendorServices` retains optional parser, bundle identifier and
log configurator (`Vendor.kt:9–16`); `RunDependencies` requires resolved
parser/log services before execution. Stub/empty are distinct config variants;
WP 2.5's caller gates Stub/Empty before CLI dispatch with exactly
`No vendor config present in <marathonfile path>` using the absolute path
(`cli/.../ApplicationView.kt:79`). Empty normally already fails stage-(a)
parse with `No vendor configuration specified`
(`ConfigurationFactory.kt:190`). Stub/Empty factory slots serve explicit
non-CLI/test construction; they do not bypass the CLI gate.
Analytics, event pipeline, FileManager and report serializers are per-run
services. Preserve construction side effects: `TrackerFactory.create` builds the report
pipeline and may create/check an Influx backend, and `MetricsProviderFactory`
may create a second client before run starts (`di/Modules.kt:24–54`). These
startup effects run off core and are awaited. History queries remain lazy
outside the pool queue prefetch approved in WP 3.2 P2:
parse without includeFlakyTests skips shard preparation (`Marathon.kt:165–169`)
but does not suppress the backend setup that constructor resolution already did.
Tracker callbacks themselves can write to Graphite (`GraphiteTracker.trackTest`
→ `BasicGraphiteClient.SocketSender`), so the frozen Tracker methods are async.
Await delegate side effects sequentially before processing the next event.

`VendorDependencies.logging` is the per-run `Arc<Mutex<LogSettings>>`
(`marathon-logging`), also reachable through RunDependencies.vendor. Device
loggers (`Component` handles) come from it. `configure_logging` sets the
debug flag **before** the vendor configurator runs, matching
`Marathon.kt:67–68`; the configurator then installs its filter through
`LogSettings::configure`, which also reloads the CLI subscriber. Registry
locks are short, never held across await, and registry
mutation/configuration runs off core so core never blocks on the vendor-held
mutex. Configure completes before the run starts.

The console is per process too (WP 2.5 R1): `CliRunner::construct` receives
the registry and the `Arc<Console>` (the shared stdout queue writer) that the
startup lines used. What `configure` installs writes through that console,
and so does the crash log after `run` fails. One writer thread owns all
stdout writes, so raw and formatted bytes never interleave mid-record.
Enqueueing never blocks: only the writer thread may block on the sink, never
core (see [logging.md](logging.md) for the queue bound and exit deadlines).
### Core log records (WP 3.6, frozen for 3.7/3.8)

Core actors log the way Kotlin's `logger.<level>(cause) { message }` calls do,
but core may neither do I/O nor take the registry lock. They therefore append a
`marathon_core::logging::LogRecord` to the executor's per-run queue
(`CoreExecutor::log_queue()`, an `Rc<LogQueue>`, core-thread only):

| field | meaning |
|---|---|
| `logger: String` | the diagnostic logger name, e.g. `DevicePool[<pool>]_DeviceActor[<serial>]`; the event target derives from it (see logging.md) |
| `at_millis: i64` | scheduler time of the call: replay order and timelines only |
| `wall_millis: i64` | wall time (epoch ms) when the record is created; the rendered stamp comes from it, however late the record drains |
| `level: marathon_logging::Level` | as called; the configurator filter decides whether it is enabled |
| `message: String` | already interpolated (core's lazy message lambdas have no side effects) |
| `cause: Option<Failure>` | the throwable, rendered as `Failure::stack_trace()` (§3.10 frames) |

Records keep call order across all core actors. Send producers outside core
(WP 3.4a's MetricsProvider) push pre-stamped records through
`LogQueue::sender()`, a `LogSender` over the same queue rather than a second
sink. Core `log`, core `raw` (WP 3.5, below) and `LogSender::push` all enqueue
into one shared `SegQueue`, so its push order is the single output order;
`records`, `take` and `take_output` (hence `OutputDriver::flush`) first drain it
into the ordered record list, so no sent record is skipped or reordered.
The run's output task (3.10) drains them through the same `OutputDriver`
(`OutputDriver::flush`) that pool progress uses, never `LogQueue::take`: the
queue also holds ordered `Raw` output records, which `take` would leave behind.
The console sink emits each record as a `tracing` event through the CLI
subscriber (`LogRecord::emit`); the subscriber's timer supplies the stamp.
Levels are not state on a logger handle: the per-configurator filter in
`marathon-logging` decides from the event target alone, with specific-target
exceptions (timings at INFO, the analytics factory/provider at DEBUG, ADB at
DEBUG, and the rest listed in [logging.md](logging.md)). Creating a
`Component` never changes any level.
The wall clock is the host's (a clock read, not I/O); `LogQueue::set_wall_clock`
replaces it, e.g. for replay. Send vendor-side owners that build records
themselves (`TestRunResultsAccumulator`) take the same source as a
`logging::WallSource` at construction.

Coroutine exception handler records follow Kotlin's Actor, whose context carries
`newCoroutineExceptionHandler(logger)`. `RuntimeActor::with_logger` tags the
actor's job with its logger, and jobs created under it inherit the tag. A
launched task that fails with no parent to take the failure runs its handler
at completion, before its completion callbacks (Kotlin's
`finalizeFinishingState`). Under a tagged scope the handler appends
`CoroutineContext closing due to unrecoverable error` (ERROR, with the cause) to
the `LogQueue` under the actor's logger, in call order with the actor's own
records. Outside any tagged scope it stays a loggerless 3.1 `CompletionLog`, at
the same point.

### Vendor-call bridge (WP 3.6)

`executor::bridge::call_vendor(vendor, cancellation, call)` is the one way core
awaits a suspending vendor call: the `Send` future runs on the vendor runtime,
`cancellation` (the calling core task's signal) is handed to the vendor as
`VendorContext.cancellation`, and the result is the vendor's **own** outcome,
joined through its cleanup even after the caller is cancelled. Kotlin's catch
blocks classify what the vendor threw, never why the caller was cancelled.

Kotlin runs the vendor inline, so its first segment (up to the first
suspension) precedes any coroutine dispatched after the caller, including the
one that cancels it. The vendor here starts later on another thread, so it sees
a gated `cancellation`: a cancellation requested after `call_vendor` began stays
hidden until the vendor's first poll returns, then becomes visible (a waiting
`cancelled()` wakes). A caller already cancelled at the call sees it at entry.

The caller must await `call_vendor` to completion. Dropping the future detaches
the vendor task: it runs on unobserved and its result is lost.

### Service error contracts (R1-wonder-7)

These services use Send `ServiceFuture<T>` with `OperationResult<T>`, rather
than the device-specific VendorError taxonomy. `Failed(Failure)` retains
class/message/cause; `Cancelled(Cancellation)` remains distinct. Backend
errors are caught only where the Kotlin implementation catches them:

- **LogWriter:** save/append failures propagate as Failed; Kotlin
  `report/logs/LogWriter.kt:11–26` does not catch writeText/appendText errors.
- **OutputFiles:** createDirectories and other thrown filesystem errors
  propagate as Failed (`io/FileManager.kt:43–80`). Preserve ignored Boolean
  results such as createTestResultFile's mkdirs; do not manufacture an error
  where Kotlin merely ignored false.
- **ReportLifecycle:** uncaught reporter errors propagate as Failed; sequential
  report generation stops at the first throw
  (`analytics/internal/sub/ExecutionReportGenerator.kt:23–33`).
- **MetricsProvider:** history-fetch errors are absorbed by runCatching with
  empty measurements, defaults and the original warnings/TimingLog
  (`analytics/external/MetricsProviderImpl.kt:55–77,110–135`). This includes
  exceptions caught inside that runCatching; do not move its catch boundary.
  Other uncaught failures, including remoteDataStore.close, propagate as
  Failed; cancellation escaping a source catch remains Cancelled. The
  observe_execution_times/report_execution_time_coverage default methods are
  no-op successes, matching MetricsProvider.kt:14,19. Overrides preserve their
  own source behaviour.

Queue key extraction remains synchronous and infallible (D60). WP 3.2 P2
(2026-10-24) supersedes 0.6 D4 for queue construction: WP 3.7 prefetches each
pool's declared history keys off core, using WP 3.4a's async MetricsProvider.
Fetch failures are absorbed with Kotlin defaults. Key functions and batching
then read the synchronous MetricsLookup view from memory; queue operations
stay on core. Per-test warn-once logs follow insertion-time lookup order (D60).
History queries and TimingLog occur earlier, and a queue that Kotlin never compares
gains a query/timing line Kotlin skips (PLAN §3.11 D13, limited to the residual cases listed there).
The prefetch goes through the run-wide memoising MetricsProvider, deduplicated by
`MetricsKey` with single-flight across concurrent pool builds: one query and
TimingLog line per key per run, as in Kotlin (whose coverage report at
`Marathon.kt:180–181` already queries the ExecutionTime keys before any pool
exists). See PLAN §3.11 D13 for the two residual cases. Outside queue
construction, retain source first-touch timing. Do not turn a failed query
into an arbitrary priority. Validate insertion-time lookup order under D60; record
residual query/log differences in the owning WP findings.

D34 (2026-11-06): a query that fails is refreshed by one task on the vendor runtime, never by a lookup, so core still
reads memory only. A strategy reads through `MetricsLookup::view`, a backend history snapshot: a queue's lasts as long as
the queue, and a batch's or a flakiness pass's as long as that read. A refresh that lands later reaches only later
snapshots. D60 stores each entry's key at insertion; run-local observed-duration fallback stays live for re-added entries,
while existing keys remain unchanged. `MetricsProvider::close` cancels and joins it before closing the source. See `docs/wp/x15-findings.md`.

Finish is guarded **before** the first close (`Marathon.kt:243–249`):
analytics.close → provider.terminate → tracker.close (report generation) →
usage.close (D1 no-op). A thrown close stops the remaining closes; the guard
prevents a retry. Preserve that defect (C25). Error-path finish is explicitly
NonCancellable (`:80–87`). A successful parse skips onFinish (`:165–176`),
including remote parse after provider initialization (C26). A failed parse still
uses exceptional cleanup. The shutdown hook only exists with debug enabled
(`ShutdownHook.kt:16`). Do not introduce RAII report generation on parse.

RunOutcome distinguishes tests-failed, no-devices, no-tests, configuration
failure and crash. It does not decide CLI exit policy: the run wrapper maps
NoDevices to false regardless of ignoreFailures; other thrown errors to
ignoreFailures (`Marathon.kt:92–101`). Exceptions before that wrapper remain
CLI failures. NoTests preserves the typo “No tests cases were found” (`:162`).

## Controlled replay seams

WP 8.12 / D62 replaces the Apple command reader's wall-clock idle arithmetic
with exact vendor scheduler deadlines, shared between stdout and stderr. Its
retired `apple.Reader.currentTimeMillis` key is ignored only in the in-memory
native tape. Other wall/monotonic hooks remain independent and strict. Local
EOF while the process lives keeps output open until end, idle expiry,
cancellation or consumer closure; terminal drains never wait for inherited
pipe write ends. `PollBackoff` remains only in the SSH exit waiter.

The exact formats and labels in `replay-tapes.md` and
`nondeterminism-sites.csv` are the contract; do not invent replacement labels.
`ReplayHooks` supplies wall millis/Instant, monotonic nanos, signed i32 draws,
stable-key selections, per-site IDs and identity suffix allocations. Timer
uses the `core.di.Clock` wall clock. TestBatch IDs use `core.TestBatch.id`;
random insertion keys use `core.RandomOrderSortingStrategy.key`. Identity
allocations take a stable object identity token, preserving aliases without
parsing literal strings (C19). No host-time/random fallback after exhaustion.

Scheduler control is separate: advance only to an explicit event timestamp,
run due timers and drain runnable work **before** delivering the external
event at that time, then drain again. Equal-time external events retain tape
array order. Due timers are released by (deadline, registration sequence),
one at a time to quiescence before the next release. This also orders vendor
timer continuations on the multi-thread worker runtime and their bridge wakes;
releasing equal-deadline timers together would race those continuations.
Live dispatch keeps its ordinary concurrent scheduling.
Wall/monotonic advance only if their individual mode requests it;
per-label reads override modes and use independent cursors. Nothing advances
automatically to the next timer. Virtual-clock use without a driver fails.

`ReplayDriver` registers all actor and provider mailboxes, tracks runnable
core work including readiness/completion callbacks, and exposes quiescence.
Runnable, running and blocking work share one counter, read in one load; every
hand-off (a vendor poll waking a core task) takes its new unit before releasing
the old one, so no read sees zero while work moves between executors.
Quiescence means no runnable work and no queued message on an open registered
channel; closed-but-buffered provider channels must still be consumed. Closed,
drained or cancelled/discarded channels are distinguished in `MailboxState`.
A stalled nonempty mailbox fails the scenario; pending future timers do not
prevent quiescence. End validates unused draws/IDs/events and accumulated hook
failures, even if application code caught the immediate error.

Live Android barriers admit `android.AdamDeviceProvider.connected`,
`core.DeviceActor.prepare`, and `core.DeviceActor.execute` by key, one at a
time. Cancellation before admission does not consume a token; release occurs
when the admitted operation completes. Duplicate/missing/exhausted barriers
are errors. Those runs use real scheduler time with frozen wall/monotonic;
they do not establish core virtual quiescence for arbitrary network work.

Core scripted replay never starts real vendor I/O. WP 7.1 compares live runs
by invariants and the documented normalization, not exact cross-device
scheduling. The fake-adb server's shutdown drains its own transcript; that
transport lifecycle is independent of the core's detached-job contract.
[`fake-worker.md`](fake-worker.md) is the frozen SSH test-worker contract.
Vendor/transport tests use its raw command evidence, shared scripted filesystem,
ready record and awaited shutdown. Dropping its handle starts cancellation;
only `shutdown().await` proves all transports/jobs/children have joined. This
is independent of Scheduler's detached core job. WP 0.4b adds Apple handlers
through that contract's extension routing; no second fake-worker protocol is
introduced here.

### Ordered raw console output (WP 3.5)

`LogQueue` also accepts `raw(String)` and retains both forms as ordered
`OutputRecord::{Log, Raw}` entries. Raw text enters the same shared queue as
core and `LogSender` records (see "Core log records"), and every read drains
it first, so sender records appear as `OutputRecord::Log` at their push
position relative to raw text. The per-run `OutputDriver::flush` takes a
batch on core and reserves its place in a completion chain before dispatching
through `call_vendor`. A later batch waits off core for the previous batch,
even if vendor threads start in reverse order. Completion releases the next
batch on success, failure or task unwind. There is no mutex acquisition or I/O
on core. All producers in a run must share the executor's `Rc<LogQueue>` (off core, a
`LogSender` from it).
`take`/`records` remain log-only compatibility operations; mixed-output consumers
must use the driver, not independently drain those operations or `take_output`.

Live output (U101). Kotlin's console appender prints each record when it is logged,
but the flush points above leave vendor log lines queued until the next one, so a
120 s test printed nothing meanwhile. In a live run (`RunInputs::live_output`, set
by the CLI when its `RuntimeHooks` carry no tape) `Marathon::run_under` also starts
an output pump: a core job that waits on the queue's `Notify` (signalled by every
`log`, `raw` and `LogSender::push`) and calls `OutputDriver::flush`, so records go
out as they arrive, and those that arrive during a write share the next batch. It
is the same take-on-core, chained write on the vendor runtime, so order and format
are unchanged and core never blocks. The serial flush points stay; while a pump is
running (`LogQueue::set_concurrent_output`) a flush that finds nothing to take still
waits for the pump's unfinished batch, so a serial owner returns only once everything
logged before it is written. A failed pump write (`OutputDriver::flush_recording`) is stored
in `Marathon::pump_error` by the write itself before its batch barrier is released, so the
serial flush that waited on that batch reports it; `run_under` flushes once more after
joining the pump to catch a batch taken after its last flush. Replay, oracle and paired-probe runs (taped hooks, the
test-support factory by default) start no pump and keep their flush points and bytes.

`ConsoleSink::write` receives an owned batch off core. The CLI's additive
`RunConsoleSink` uses the same `Arc<Console>` as startup: raw text takes the
Console text path with JVM UTF-8 replacement, and logs are emitted through
the subscriber installed for the process (or a test dispatch). The
subscriber's current filter decides per event. Stdout
write/flush errors are swallowed as PrintStream does. Actual vendor task errors
and tracker exceptions propagate. WP 3.10 constructs this adapter; it must
await final output draining.

Pool progress callbacks are serial owners. `test_ended` awaits raw output,
stores the FSM transition, flushes any diagnostic, then awaits `Tracker::test`
through the vendor bridge before returning the nullable action. Tracker failure
leaves the stored state intact. `test_started` transitions, flushes CI, then
flushes the raw started line. Retry/AddRetry and RemoveAttempts remain
synchronous and have no tracker payload; warnings wait in the shared queue.
No production caller gains a missing testStarted call (C4).

The run-results accumulator is a Send, serial vendor-side value rather than a
frozen TestRunListener implementation. Its synchronous callbacks retain merged
`LogRecord`s from a transient `LogQueue`; adapters take these with `take_logs`
after each callback, including failures, and forward them before later effects.
They must preserve wall timestamps, level, logger name and message. The record's
scheduler timestamp is zero because these callbacks have no core scheduler
context. Platform listener construction and delivery belong to 4.7/6.6a.
