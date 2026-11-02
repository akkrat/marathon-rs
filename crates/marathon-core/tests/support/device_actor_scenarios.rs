//! Port of tools/oracle/module/src/main/kotlin/oracle/DeviceActorProbe.kt (its scenarios),
//! which follow core/src/test/kotlin/com/malinskiy/marathon/execution/device/DeviceActorTest.kt
//! and DeviceActorBatchAccountingTest.kt.
#![allow(dead_code)]
use super::device_actor::*;
use marathon_core::{
    error::*,
    execution::{FromDevice, device::DeviceEvent},
    model::TestStatus,
    runtime::*,
};
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// Every probe scenario, in the corpus order.
pub const SCENARIOS: &[&str] = &[
    "lost device preserves already reported passes",
    "lost device returns the batch when the reported results failed",
    "lost device returns the batch when the reported results were cancelled",
    "cancelled execution sends already reported results",
    "cancelled execution without results returns the batch",
    "termination after reported results sends them and cancels the execution",
    "termination before results returns the batch once and ignores a late report",
    "batch failure during termination does not return the batch twice",
    "results of a completed batch reach the pool even if the actor stops right after",
    "actor failing before a dispatched batch starts executing returns the batch",
    "completed execution with cancelled results returns the batch and stays ready",
    "terminate while in progress",
    "device lost while in progress returns the batch and notifies the pool",
    "batch failure after reported results sends the results only",
    "batch failure without results returns the batch only",
    "a preparation that fails with a cancellation of its own terminates the device at once, without retries",
    "terminate during preparation",
    "thirty failing preparations",
    "retry delay cancelled at its deadline",
    "execute at an unclosed terminated actor",
    "completed execution with exceptional results without a message",
    "lost device with exceptional results without a message",
    "parent cancellation while running",
    "readiness to a cancelled pool",
    "readiness to a closed pool",
    "actor job failure from a child launched in prepare",
    "actor job failure from a child launched in the actor scope",
    "entry cancellation",
];

fn failing_prepare() -> PrepareScript {
    prepare_script(|_| async { Err(VendorError::Other(ise("prepare failed"))) })
}

pub fn run_scenario(name: &'static str) -> Outcome {
    let mode = match name {
        "readiness to a cancelled pool" => PoolMode::Cancelled,
        "readiness to a closed pool" => PoolMode::Closed,
        _ => PoolMode::Open,
    };
    scenario(mode, move |run| {
        Box::pin(async move {
            match name {
                "lost device preserves already reported passes" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        e.result.complete(Ok(passed(&e.device, &e.batch)));
                        Err(device_lost(ise("transport disconnected after test passed")))
                    })))
                    .await;
                }
                "lost device returns the batch when the reported results failed" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        e.result
                            .complete(Err(VendorError::Other(ise("result bundle is corrupted"))));
                        Err(device_lost(ise("transport disconnected")))
                    })))
                    .await;
                }
                "lost device returns the batch when the reported results were cancelled" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        e.result.cancel(job_was_cancelled());
                        Err(device_lost(ise("transport disconnected")))
                    })))
                    .await;
                }
                "cancelled execution sends already reported results" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        e.result.complete(Ok(passed(&e.device, &e.batch)));
                        Err(vendor_cancellation("execution timed out while collecting artifacts"))
                    })))
                    .await;
                }
                "cancelled execution without results returns the batch" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|_| async {
                        Err(vendor_cancellation("execution timed out"))
                    })))
                    .await;
                }
                "termination after reported results sends them and cancels the execution" => {
                    let cancelled = Arc::new(AtomicBool::new(false));
                    let flag = cancelled.clone();
                    run.start_batch(ScriptedDevice::scripted(execute_script(move |e| {
                        let flag = flag.clone();
                        async move {
                            e.result.complete(Ok(passed(&e.device, &e.batch)));
                            // awaitCancellation() in a try/finally
                            let reason = e.context.cancellation.cancelled().await;
                            flag.store(true, Ordering::SeqCst);
                            Err(VendorError::Cancelled(reason))
                        }
                    })))
                    .await;
                    run.send(DeviceEvent::Terminate);
                    run.run_current().await;
                    run.extra
                        .borrow_mut()
                        .insert("executionCancelled".into(), cancelled.load(Ordering::SeqCst).into());
                }
                "termination before results returns the batch once and ignores a late report" => {
                    let late = Arc::new(Mutex::new(None));
                    let slot = late.clone();
                    run.start_batch(ScriptedDevice::scripted(execute_script(move |e| {
                        let slot = slot.clone();
                        async move {
                            let reason = e.context.cancellation.cancelled().await;
                            let accepted = e.result.complete(Ok(passed(&e.device, &e.batch)));
                            *slot.lock().unwrap() = Some(accepted);
                            Err(VendorError::Cancelled(reason))
                        }
                    })))
                    .await;
                    run.send(DeviceEvent::Terminate);
                    run.run_current().await;
                    let accepted = *late.lock().unwrap();
                    run.extra
                        .borrow_mut()
                        .insert("lateReportAccepted".into(), accepted.map_or(Value::Null, Value::from));
                }
                "batch failure during termination does not return the batch twice" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        e.context.cancellation.cancelled().await;
                        Err(batch_failure("test run failed while the device was terminated"))
                    })))
                    .await;
                    run.send(DeviceEvent::Terminate);
                    run.run_current().await;
                }
                "results of a completed batch reach the pool even if the actor stops right after" => {
                    // Kotlin queues `parent.cancel()` on the actor's single thread from inside
                    // execute, so it runs before anything the actor launches once the batch
                    // completes. Here the order is explicit: the parent is cancelled when the
                    // pool receives the outcome, after the vendor's return is bridged and
                    // before the readiness task first runs.
                    let parent = run.parent.clone();
                    *run.pool.on_message.borrow_mut() = Some(Box::new(move |message| {
                        if matches!(message, FromDevice::CompletedTestBatch(..)) {
                            parent.cancel(job_was_cancelled());
                        }
                    }));
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        e.result.complete(Ok(passed(&e.device, &e.batch)));
                        Ok(())
                    })))
                    .await;
                    run.run_current().await;
                }
                "actor failing before a dispatched batch starts executing returns the batch"
                | "actor job failure from a child launched in prepare" => {
                    // Kotlin's vendor launches a child into the preparation coroutine that
                    // fails later. A VendorContext has no job, so a test-side core child of
                    // the actor job fails it instead: the actor sees the same owner failure.
                    run.start(ScriptedDevice::stub(Stub::timed(0, 0)));
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                    if name.starts_with("actor failing") {
                        // Execute is queued ahead of the failure: the batch is Running and
                        // its execution task never starts.
                        run.send(DeviceEvent::Execute(run.batch.clone()));
                    }
                    run.fail_actor_job("log streaming crashed");
                    run.run_current().await;
                }
                "completed execution with cancelled results returns the batch and stays ready" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        e.result.cancel(job_was_cancelled());
                        Ok(())
                    })))
                    .await;
                }
                "terminate while in progress" => {
                    let mut stub = Stub::timed(1000, 10000);
                    stub.execution_results = vec![(run.batch.tests[0].clone(), vec![TestStatus::Failure])];
                    run.start(ScriptedDevice::stub(stub));
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                    run.advance_by(1000).await;
                    run.send(DeviceEvent::Execute(run.batch.clone()));
                    run.send(DeviceEvent::Terminate);
                    run.run_current().await;
                    run.advance_until_idle().await;
                }
                "device lost while in progress returns the batch and notifies the pool" => {
                    let mut stub = Stub::timed(0, 0);
                    stub.crash_with_device_lost_exception = true;
                    run.start_batch(ScriptedDevice::stub(stub)).await;
                }
                "batch failure after reported results sends the results only" => {
                    let mut stub = Stub::timed(0, 0);
                    stub.crash_with_test_batch_exception = true;
                    stub.report_results_before_crash = true;
                    run.start_batch(ScriptedDevice::stub(stub)).await;
                }
                "batch failure without results returns the batch only" => {
                    let mut stub = Stub::timed(0, 0);
                    stub.crash_with_test_batch_exception = true;
                    run.start_batch(ScriptedDevice::stub(stub)).await;
                }
                "a preparation that fails with a cancellation of its own terminates the device at once, without retries" =>
                {
                    let prepare = prepare_script(|_| async {
                        Err(VendorError::LeavePool {
                            reason: LeavePoolReason::NotBooted,
                            detail: marathon_core::executor::shared::failure(
                                "oracle.DeviceActorProbe$NotBootedProbeException",
                                "not booted",
                            ),
                        })
                    });
                    run.start(ScriptedDevice::new(Stub::timed(0, 5000), Some(prepare), None));
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                }
                "terminate during preparation" => {
                    run.start(ScriptedDevice::stub(Stub::timed(5000, 5000)));
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                    run.send(DeviceEvent::Terminate);
                    run.run_current().await;
                    run.advance_until_idle().await;
                }
                "thirty failing preparations" => {
                    run.start(ScriptedDevice::new(Stub::default(), Some(failing_prepare()), None));
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                    run.advance_until_idle().await;
                }
                "retry delay cancelled at its deadline" => {
                    let actor = run.start(ScriptedDevice::new(Stub::default(), Some(failing_prepare()), None));
                    // Registered before the retry delay, so at 10 s it runs first: the delay
                    // throws and the isActive skip is not reached.
                    let scope = run.core.job(JobKind::Ordinary, None);
                    run.core.spawn(
                        scope,
                        Box::new(move |context| {
                            Box::pin(async move {
                                context.clock.delay(10_000, context.job.clone()).await?;
                                if let Some(job) = actor.initialize_job() {
                                    job.cancel(deferred_coroutine_cancelled());
                                }
                                Ok(())
                            })
                        }),
                    );
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                    run.advance_until_idle().await;
                }
                "execute at an unclosed terminated actor" => {
                    run.start(ScriptedDevice::scripted(execute_script(|_| async { Ok(()) })));
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                    run.send(DeviceEvent::Terminate);
                    run.send(DeviceEvent::Execute(run.batch.clone()));
                    run.run_current().await;
                }
                "completed execution with exceptional results without a message" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        e.result.complete(Err(VendorError::Other(no_message_ise())));
                        Ok(())
                    })))
                    .await;
                }
                "lost device with exceptional results without a message" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        e.result.complete(Err(VendorError::Other(no_message_ise())));
                        Err(device_lost(ise("transport disconnected")))
                    })))
                    .await;
                }
                "parent cancellation while running" => {
                    run.start_batch(ScriptedDevice::scripted(execute_script(|e| async move {
                        Err(VendorError::Cancelled(e.context.cancellation.cancelled().await))
                    })))
                    .await;
                    run.parent.cancel(job_was_cancelled());
                    run.run_current().await;
                }
                "readiness to a cancelled pool" | "readiness to a closed pool" => {
                    run.start(ScriptedDevice::scripted(execute_script(|_| async { Ok(()) })));
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                }
                "actor job failure from a child launched in the actor scope" => {
                    run.start(ScriptedDevice::scripted(execute_script(|_| async { Ok(()) })));
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                    run.fail_actor_job("actor child crashed");
                    run.run_current().await;
                }
                "entry cancellation" => {
                    let entry = Arc::new(Mutex::new(Vec::new()));
                    let seen = entry.clone();
                    run.start(ScriptedDevice::scripted(execute_script(move |e| {
                        let seen = seen.clone();
                        async move {
                            // isActive, then ensureActive(), before any suspension
                            let reason = e.context.cancellation.reason();
                            seen.lock().unwrap().push(("entryActive", reason.is_none()));
                            if let Some(reason) = reason {
                                return Err(VendorError::Cancelled(reason));
                            }
                            seen.lock().unwrap().push(("commandSent", true));
                            Ok(())
                        }
                    })));
                    run.send(DeviceEvent::Initialize);
                    run.run_current().await;
                    run.send(DeviceEvent::Execute(run.batch.clone()));
                    run.send(DeviceEvent::Terminate);
                    run.run_current().await;
                    for (key, value) in entry.lock().unwrap().iter() {
                        run.extra.borrow_mut().insert((*key).into(), (*value).into());
                    }
                }
                other => panic!("unknown scenario {other}"),
            }
        })
    })
}

fn no_message_ise() -> Failure {
    Failure {
        class: "java.lang.IllegalStateException".into(),
        message: None,
        cause: None,
        stacktrace: None,
    }
}
