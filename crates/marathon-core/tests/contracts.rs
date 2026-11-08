//! Port of core/src/main/kotlin/com/malinskiy/marathon/Vendor.kt
//! and its consuming APIs. These public functions are type-checked, never run.

use jcompat::{JavaPriorityQueue, JavaRegex};
use marathon_config::{
    Configuration,
    binding::{BindingContext, ConfigurationLoader, ConfigurationValidator},
};
use marathon_core::{
    error::{OperationError, OperationResult, VendorError, VendorResult},
    listener::*,
    model::*,
    runtime::*,
    time::*,
    vendor::*,
    wiring::*,
};
use std::{rc::Rc, sync::Arc};

// WP 2.1–2.6: binding carries its profile/context and staged failures, including
// post-processing that may perform filesystem work on the vendor executor.
pub async fn configuration_family(
    loader: &dyn ConfigurationLoader,
    validator: &dyn ConfigurationValidator,
    bytes: &[u8],
    context: &BindingContext,
) {
    if let Ok(config) = loader.parse(bytes, context).await {
        // parse already ran stage-(a) extraction/flag merge; never repeat it.
        let _stage_error = validator.validate(&config).await;
    }
}
// WP 1.1/1.4, 3.2–3.4: consume the merged heap and UTF-8 full matching.
// Metrics I/O must be bridged off core before invoking a synchronous comparator.
pub fn strategies_and_filters<C: FnMut(&Test, &Test) -> std::cmp::Ordering>(
    queue: &mut JavaPriorityQueue<Test, C>,
    regex: &JavaRegex,
    input: &str,
) -> Vec<Test> {
    let copies = queue.iter().cloned().collect();
    if regex.matches(input) {
        let _next = queue.poll();
    }
    copies
}
// WP 2.1/3.3: compilation retains config-field diagnostics and pattern text.
pub fn compile_filter(pattern: &str, field: &str) -> Result<JavaRegex, jcompat::RegexCompileError> {
    JavaRegex::compile(pattern, field)
}
// WP 3.1: child ownership and receiver cancellation do not depend on Send.
pub fn runtime_contract(runtime: &dyn CoreRuntime, actor: Rc<dyn Actor<Test>>) -> JobRef {
    let detached = runtime.job(JobKind::Ordinary, None);
    let supervisor = runtime.job(JobKind::Supervisor, Some(detached.clone()));
    runtime.spawn(
        supervisor,
        Box::new(move |context| {
            Box::pin(async move {
                context.clock.delay(100, context.job.clone()).await?;
                actor.close();
                Ok(())
            })
        }),
    );
    detached
}
// WP 3.5–3.8: a shared result cell crosses executors; both batch value equality
// and batch identity are available, without equating replacement devices by serial.
pub fn scheduling_contract(
    device: DeviceRef,
    other: DeviceRef,
    batch: BatchRef,
    other_batch: BatchRef,
    result: Arc<dyn Deferred<TestBatchResults>>,
) {
    let _same_device = device == other;
    let _same_batch_object = Arc::ptr_eq(&batch, &other_batch);
    let _equal_batch_value = batch == other_batch;
    let _reported_before_return = result.completed();
}
// WP 4.7–4.10, 5.1–5.6, 6.5–6.6: this must compile as Send and 'static.
// Returning from execute and reporting the results are independent channels.
pub fn vendor_execution(
    device: DeviceRef,
    configuration: Arc<Configuration>,
    context: VendorContext,
    batch: BatchRef,
    pool: DevicePoolId,
    result: Arc<dyn Deferred<TestBatchResults>>,
) -> VendorFuture<'static, ()> {
    Box::pin(async move {
        device.0.prepare(configuration.clone(), context.clone()).await?;
        device.0.execute(configuration, pool, batch, result, context).await
    })
}
// WP 3.10, 4.8, 6.6a: remote extraction borrows before channel consumption.
pub fn parsing_contract(
    provider: Arc<dyn DeviceProvider>,
    parser: TestParser,
    context: VendorContext,
) -> VendorFuture<'static, Vec<Test>> {
    Box::pin(async move {
        match parser {
            TestParser::Local(parser) => parser.extract(context).await,
            TestParser::Remote(parser) => {
                let device = provider.borrow(context.clone()).await?;
                parser.extract(device, context).await
            }
        }
    })
}
// WP 3.9a–c, 4.7/4.9, 5.3, 6.6a–b: use one listener identity for add/remove.
pub async fn reports_and_listeners(
    producer: &dyn LogProducer,
    listener: Arc<dyn LineListener>,
    run: &dyn TestRunListener,
    attachments: &dyn AttachmentProvider,
    sink: Arc<dyn AttachmentListener>,
    test: &Test,
) -> VendorResult<()> {
    producer.add_line_listener(listener.clone())?;
    attachments.register_listener(sink)?;
    run.before_test_run().await?;
    run.test_started(test).await?;
    run.test_ended(test).await?;
    run.after_test_run().await?;
    producer.remove_line_listener(&listener)?;
    listener.close().await
}
// WP 4.1–4.3, 6.1–6.4: transport work shares cancellation observation and
// labelled clocks, with no dependency from leaf crates on actor implementations.
pub fn transport_contract(context: &VendorContext) -> ClockResult<(i64, i64)> {
    let _cancelled = context.cancellation.reason();
    Ok((
        context.hooks.millis("apple.Reader.currentTimeMillis")?,
        context.hooks.nanos("ios.AppleSimulatorDevice.nanoTime")?,
    ))
}
// WP 4.4–4.6, 6.4: vendor bundle payloads remain vendor-owned, with explicit
// reference identity instead of assuming equality by ID.
pub fn bundle_contract(identifier: &dyn TestBundleIdentifier, test: &Test) -> VendorResult<Option<String>> {
    Ok(identifier.identify(test)?.map(|bundle| bundle.0.id().clone()))
}
// WP 7.1–7.2: quiescence includes timer callbacks before an external event.
pub async fn replay_contract(driver: &dyn ReplayDriver, hooks: &dyn ReplayHooks, at: i64) -> ClockResult<String> {
    driver.advance_to(at).await?;
    driver.drain_current().await?;
    driver.assert_quiescent()?;
    hooks.allocate_id("core.TestBatch.id")
}
// WP 3.10, 7.3–7.6: core dependencies are passed explicitly. This function
// documents close order, including short-circuit failure, but is never executed.
pub async fn finish_contract(deps: &RunDependencies) -> VendorResult<()> {
    deps.analytics.close().await.map_err(service_error)?;
    deps.provider.terminate().await?;
    deps.reports.close().await.map_err(service_error)?;
    deps.usage.close().await
}

// WP 3.1/3.6: a Send future is explicitly dispatched, never polled on core.
pub fn dispatch_vendor(runtime: &dyn VendorRuntime, work: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
    runtime.spawn(work)
}

// WP 2.1/1.4/3.3: D6 regex input is UTF-8. Lossless model storage uses an
// explicit checked conversion; invalid UTF-16 is a binding/filter error.
pub fn model_filter(regex: &JavaRegex, test: &Test) -> Result<bool, std::string::FromUtf16Error> {
    Ok(regex.matches(&test.clazz))
}
// WP 1.7/3.9/5.3/6.6b: the same per-run file manager is passed to all consumers.
pub async fn output_contract(
    files: &dyn OutputFiles,
    target: &marathon_core::output::FileTarget,
) -> OperationResult<jcompat::file_manager::JavaPath> {
    files.create_file(target).await
}

// WP 1.5/3.9: model projections use the merged value trees and writers.
// MetaValue is discovery data (boxed types/identity), not another writer tree.
pub fn writer_contract(
    json: &jcompat::writers::WriterValue,
    raw: &jcompat::writers::KotlinValue,
) -> Result<[String; 3], jcompat::writers::GsonError> {
    Ok([
        jcompat::writers::GsonWriter::default().write(json)?,
        jcompat::writers::JacksonYamlWriter.write(json),
        raw.render(),
    ])
}
// WP 1.2/2.1/3.9: time and number output uses merged formatters; default
// output entry points pin en-US (D8), with no locale in the run context.
pub fn formatting_contract(instant: jcompat::jtime::Instant, duration: jcompat::jtime::Duration) -> [String; 5] {
    [
        instant.to_string(),
        duration.to_string(),
        jcompat::jnum::double_to_string(0.5),
        jcompat::formatting::format_percent(50),
        jcompat::formatting::format_date(0, jcompat::formatting::DatePattern::Html),
    ]
}
// WP 1.3/2.1: binding supplies environment lookup to the existing substitutor.
pub fn substitution_contract<F: Fn(&str) -> Option<String>>(
    lookup: F,
    input: &str,
) -> Result<String, jcompat::SubstitutionError> {
    jcompat::StringSubstitutor::new(lookup).replace(input)
}
// WP 1.7/3.2/3.9: model names bridge into the canonical path planner and hash.
// Materializing PathPlan is filesystem work for VendorRuntime, never core.
pub fn file_plan_contract(
    files: &jcompat::file_manager::FileManager,
    target: &marathon_core::output::FileTarget,
    test_name: Option<&String>,
) -> (jcompat::file_manager::PathPlan, Option<i64>) {
    (
        files.plan_file(target.request(test_name.map(String::as_str))),
        jcompat::fragment_hash::fragment_index_for_hash(
            jcompat::fragment_hash::signed_md5_utf16(&target.pool.name.encode_utf16().collect::<Vec<_>>()),
            2,
        ),
    )
}

fn service_error(error: OperationError) -> VendorError {
    match error {
        OperationError::Cancelled(reason) => VendorError::Cancelled(reason),
        OperationError::Failed(failure) => VendorError::Other(failure),
    }
}

// R2-wonder-2: handlers without construction bookkeeping omit started.
pub struct ReceiveOnlyHandler;
impl ActorHandler<Test> for ReceiveOnlyHandler {
    fn receive(&mut self, _message: Test, _context: ActorContext<Test>) -> LocalFuture<'_, OperationResult<()>> {
        Box::pin(async { Ok(()) })
    }
}

// R1-sol-1 / R1-wonder-1: handlers install own-job completion bookkeeping
// before the first message; children and delay observe their own execution job.
pub struct ContextHandler {
    completion: Option<Box<dyn CompletionRegistration>>,
}
impl ActorHandler<Test> for ContextHandler {
    fn started(&mut self, context: ActorContext<Test>) {
        let actor = Rc::downgrade(&context.actor);
        self.completion = Some(context.task.job.invoke_on_completion(Box::new(move |_| {
            if let Some(actor) = actor.upgrade() {
                actor.close();
            }
        })));
    }
    fn receive(&mut self, _message: Test, context: ActorContext<Test>) -> LocalFuture<'_, OperationResult<()>> {
        Box::pin(async move {
            let child = context.task.core.spawn(
                context.actor.job(),
                Box::new(|child| {
                    Box::pin(async move {
                        child.clock.delay(10, child.job.clone()).await?;
                        Ok(())
                    })
                }),
            );
            child.join_in(context.task.job.clone()).await?;
            context.actor.close();
            Ok(())
        })
    }
}

// R1-sol-1: cleanup also propagates cancellation, without encoding it as Failure.
pub fn cleanup_contract(runtime: &dyn CoreRuntime, awaited: JobRef) -> JobRef {
    runtime.spawn_cleanup(Box::new(move |_| {
        Box::pin(async move {
            awaited.join().await?;
            Ok(())
        })
    }))
}

// R1-sol-2 / R1-wonder-4: inner timeout escapes outer with_timeout_or_null.
// Captured cells retain typed output across the object-safe unit task seam.
pub async fn nested_timeout_contract(
    deps: &RunDependencies,
    parent: TaskContext,
) -> OperationResult<Option<Vec<Test>>> {
    let tests = Rc::new(std::cell::RefCell::new(None));
    let output = tests.clone();
    let inner = deps.timeouts.clone();
    let done = deps
        .timeouts
        .with_timeout_or_null(
            parent,
            100,
            Box::new(move |outer| {
                Box::pin(async move {
                    inner
                        .with_timeout(
                            outer,
                            10,
                            Box::new(move |context| {
                                Box::pin(async move {
                                    context.clock.delay(20, context.job.clone()).await?;
                                    *output.borrow_mut() = Some(Vec::<Test>::new());
                                    Ok(())
                                })
                            }),
                        )
                        .await?;
                    Ok(())
                })
            }),
        )
        .await?;
    Ok(done.and_then(|()| tests.borrow_mut().take()))
}

// R1-wonder-2: Send vendor delay/timeout is dispatched off core, controlled by
// the same replay timeline the core driver advances.
pub fn vendor_time_contract(deps: &VendorDependencies, context: VendorContext) -> Arc<dyn VendorTask> {
    let clock = deps.clock.clone();
    deps.io.spawn(Box::pin(async move {
        clock
            .with_timeout_or_null(
                context.cancellation,
                100,
                Box::new(move |scope| {
                    Box::pin(async move {
                        context.clock.delay(20, scope).await?;
                        Ok(())
                    })
                }),
            )
            .await?;
        Ok(())
    }))
}

// R1-wonder-3: the registry and object-safe timeout handle are reachable from
// resolved dependencies; configure sets debug before replacing appenders.
pub async fn logging_contract(deps: &RunDependencies) -> VendorResult<()> {
    deps.log_configurator
        .configure(deps.vendor.configuration.debug, deps.vendor.logging.clone())
        .await
}

// R1-spark-5: a provider implementing only the original three required methods
// inherits both Kotlin no-op hooks. Forward real queries rather than stub them.
pub struct QueryOnlyMetrics<'a>(pub &'a dyn MetricsProvider);
impl MetricsProvider for QueryOnlyMetrics<'_> {
    fn success_rate<'a>(&'a self, test: &'a Test, limit: jcompat::jtime::Instant) -> ServiceFuture<'a, f64> {
        self.0.success_rate(test, limit)
    }
    fn execution_time<'a>(
        &'a self,
        test: &'a Test,
        percentile: f64,
        limit: jcompat::jtime::Instant,
    ) -> ServiceFuture<'a, f64> {
        self.0.execution_time(test, percentile, limit)
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        self.0.close()
    }
}

// WP 3.1 additive cancellation repair: the caller is explicit for waits, and
// the worker receives its own task signal rather than an unrelated parent token.
pub async fn cancellation_aware_runtime_contract(job: JobRef, caller: JobRef, io: &dyn VendorRuntime) {
    let _ = job.join_in(caller).await;
    let task = io.spawn_cancellable(Box::new(|signal| {
        Box::pin(async move {
            let reason = signal.cancelled().await;
            Err(VendorError::Cancelled(reason))
        })
    }));
    task.cancel(marathon_core::executor::shared::requested("contract"));
    let _ = task.join().await;
}
