//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt
//! Provider-to-Scheduler witness for the last-device termination barrier and retained closed pool.
#[path = "../../marathon-core/tests/support/pool.rs"]
mod pool_support;
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_config::devices::Transport;
use marathon_core::{
    analytics::Analytics,
    ci::Ci,
    execution::scheduler::{Scheduler, SchedulerParts},
    executor::{
        CoreExecutor,
        hooks::RuntimeHooks,
        shared::{SharedDeferred, requested},
    },
    model::TestShard,
    output::OutputDriver,
    runtime::{CoreRuntime, Deferred, JobKind},
    vendor::DeviceProvider,
};
use marathon_ios::device::simulator_provider::{AppleSimulatorProvider, recovery::SimulatorRecovery};
use simulator_support::*;
use std::{
    rc::Rc,
    sync::{Arc, atomic::Ordering},
};

/// C50's Kotlin behaviour stays, after D48's limit: a pool that has ended is not brought back.
#[test]
fn last_device_loss_joins_batch_and_queue_barriers_and_a_replacement_after_the_limit_does_not_resurrect_the_pool_d48()
-> TestResult {
    last_device_loss(200, false)
}

/// D48: a worker-loss recovery on a one-worker pool. The replacement simulator finds the pool still there, with its tests.
#[test]
fn a_replacement_within_the_limit_takes_over_the_pool_d48() -> TestResult {
    last_device_loss(60_000, true)
}

fn last_device_loss(wait_millis: i64, returns_in_time: bool) -> TestResult {
    let mut p = recovery_support::RecoveryHarness::fast()?;
    std::fs::write(
        p.h().dir.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: simulator, udid: a}]\n",
    )?;
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            p.base.h.services.runtime = core.vendor();
            let provider = AppleSimulatorProvider::new(provider_support::ProviderHarness::services(
                p.h(),
                p.connections.clone(),
                SimulatorRecovery::new(),
            ))
            .map_err(|e| e.failure())?;
            let batch_gate = SharedDeferred::new();
            let recovery_gate = SharedDeferred::new();
            let script = Arc::new(BatchScript::default());
            *script.plan.lock().unwrap() = OutputPlan {
                hang: true,
                close_gate: Some(batch_gate.clone()),
                ..Default::default()
            };
            script.attach(&p.h().worker);
            let fresh = p.base.worker(Transport::Local, &["a"]);
            let g = recovery_gate.clone();
            let boot = p.h().boot.clone();
            *fresh.before.lock().unwrap() = Arc::new(move |c| {
                let gate = g.clone();
                let answer = boot.answer(&c);
                Box::pin(async move {
                    if c.last().is_some_and(|s| s == "HOME") {
                        gate.wait().await?;
                    }
                    Ok(answer)
                })
            });
            let replacement_batches = Arc::new(BatchScript::default());
            if returns_in_time {
                *replacement_batches.plan.lock().unwrap() = OutputPlan {
                    lines: [passed(1), passed(2)].concat(),
                    ..Default::default()
                };
            }
            replacement_batches.attach(&fresh);
            provider
                .initialize(p.h().services.context.clone())
                .await
                .map_err(|e| e.failure().clone())?;
            let hooks = RuntimeHooks::live();
            let parent = core.job(JobKind::Supervisor, None);
            let tracker = Arc::new(pool_support::RecordingTracker::default());
            let scheduler = Rc::new(Scheduler::new(SchedulerParts {
                core: core.clone(),
                provider: provider.clone(),
                analytics: Arc::new(Analytics::from_lookup(Arc::new(pool_support::NoMetrics))),
                configuration: {
                    let mut configuration = (*p.h().services.configuration).clone();
                    configuration.device_initialization_timeout_millis = wait_millis;
                    Arc::new(configuration)
                },
                shard: TestShard {
                    tests: tests(),
                    flaky_tests: vec![],
                },
                tracker: tracker.clone(),
                timer: p.h().services.timer.clone(),
                bundle_identifier: Some(p.h().services.identifier.clone()),
                hooks: hooks.clone(),
                barriers: hooks.barriers(),
                ci: Ci::None,
                output: OutputDriver {
                    vendor: core.vendor(),
                    sink: Arc::new(pool_support::NullSink),
                },
                context: core.context(parent.clone()),
            }));
            let running = scheduler.clone();
            let finished = SharedDeferred::new();
            let done = finished.clone();
            core.spawn(
                parent.clone(),
                Box::new(move |_| {
                    Box::pin(async move {
                        done.complete(running.execute().await);
                        Ok(())
                    })
                }),
            );
            let context = core.context(parent.clone());
            let wait = |millis| {
                let c = context.clone();
                async move { c.clock.delay(millis, c.job).await }
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while script.count.load(Ordering::SeqCst) == 0 {
                assert!(std::time::Instant::now() < deadline);
                wait(5).await?;
            }
            let original = provider.devices()[0].handle.clone();
            *p.base.connections.workers.lock().unwrap() = vec![(Transport::Local, fresh)];
            p.h().worker.connected.store(false, Ordering::SeqCst);
            script.closing.wait().await.map_err(|e| e.failure().clone())?;
            assert_eq!(scheduler.pools().len(), 1);
            assert!(!scheduler.pools()[0].1.terminated());
            assert!(finished.completed().is_none());
            batch_gate.complete(Ok(()));
            if returns_in_time {
                recovery_gate.complete(Ok(()));
                let result = finished.wait().await.map_err(|e| e.failure().clone())?;
                assert!(replacement_batches.count.load(Ordering::SeqCst) > 0);
                assert!(*result, "the tests ran on the replacement");
                provider.terminate().await.map_err(|e| e.failure().clone())?;
                parent.cancel(requested("test finished"));
                let _ = parent.join().await;
                return Ok(());
            }
            while !scheduler.pools()[0].1.terminated() {
                assert!(std::time::Instant::now() < deadline);
                wait(5).await?;
            }
            assert!(!*finished.wait().await.map_err(|e| e.failure().clone())?);
            recovery_gate.complete(Ok(()));
            while Arc::ptr_eq(&provider.devices()[0].handle, &original) {
                assert!(std::time::Instant::now() < deadline);
                wait(5).await?;
            }
            while tracker.connected.lock().unwrap().len() < 2 {
                assert!(std::time::Instant::now() < deadline);
                wait(5).await?;
            }
            assert_eq!(scheduler.pools().len(), 1);
            assert!(scheduler.pools()[0].1.terminated());
            assert_eq!(replacement_batches.count.load(Ordering::SeqCst), 0);
            provider.terminate().await.map_err(|e| e.failure().clone())?;
            parent.cancel(requested("test finished"));
            let _ = parent.join().await;
            Ok(())
        })
    })
    .map_err(|e| format!("{e:?}"))?;
    Ok(())
}

#[test]
fn cutoff_crossed_during_real_prepare_leaves_the_pool_without_requesting_a_first_batch() -> TestResult {
    let mut p = recovery_support::RecoveryHarness::fast()?;
    p.fail_initial();
    std::fs::write(
        p.h().dir.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: simulator, udid: a}]\n",
    )?;
    let time = p.time.clone();
    let boot = p.h().boot.clone();
    *p.h().worker.before.lock().unwrap() = Arc::new(move |command| {
        if command.starts_with(&["xcrun".into(), "simctl".into(), "boot".into()]) {
            time.wall_offset.store(600_001, Ordering::SeqCst);
        }
        let answer = boot.answer(&command);
        Box::pin(async move { Ok(answer) })
    });
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            p.base.h.services.runtime = core.vendor();
            let provider = AppleSimulatorProvider::new(provider_support::ProviderHarness::services(
                p.h(),
                p.connections.clone(),
                SimulatorRecovery::new(),
            ))
            .map_err(|e| e.failure())?;
            let script = Arc::new(BatchScript::default());
            script.attach(&p.h().worker);
            provider
                .initialize(p.h().services.context.clone())
                .await
                .map_err(|e| e.failure().clone())?;
            let hooks = RuntimeHooks::live();
            let parent = core.job(JobKind::Supervisor, None);
            let scheduler = Scheduler::new(SchedulerParts {
                core: core.clone(),
                provider: provider.clone(),
                analytics: Arc::new(Analytics::from_lookup(Arc::new(pool_support::NoMetrics))),
                // D48: the pool that lost its only device waits for nothing here, so not the default 180 s on a real clock.
                configuration: {
                    let mut configuration = (*p.h().services.configuration).clone();
                    configuration.device_initialization_timeout_millis = 200;
                    Arc::new(configuration)
                },
                shard: TestShard {
                    tests: tests(),
                    flaky_tests: vec![],
                },
                tracker: Arc::new(pool_support::RecordingTracker::default()),
                timer: p.h().services.timer.clone(),
                bundle_identifier: Some(p.h().services.identifier.clone()),
                hooks: hooks.clone(),
                barriers: hooks.barriers(),
                ci: Ci::None,
                output: OutputDriver {
                    vendor: core.vendor(),
                    sink: Arc::new(pool_support::NullSink),
                },
                context: core.context(parent.clone()),
            });
            assert!(!scheduler.execute().await.map_err(|e| e.failure().clone())?);
            assert_eq!(scheduler.pools().len(), 1);
            assert!(scheduler.pools()[0].1.terminated());
            assert!(provider.devices()[0].handle.retired());
            assert_eq!(script.count.load(Ordering::SeqCst), 0);
            assert_eq!(p.h().boot.boots.load(Ordering::SeqCst), 1);
            provider.terminate().await.map_err(|e| e.failure().clone())?;
            parent.cancel(requested("test finished"));
            let _ = parent.join().await;
            Ok(())
        })
    })
    .map_err(|e| format!("{e:?}"))?;
    Ok(())
}

/// D33 through the core scheduler: every device fails, whichever way its retries are going.
fn no_devices_with_failing_setups(pause: i64, finished_retries: bool) -> TestResult {
    const TIMEOUT: i64 = 300;
    let mut p = recovery_support::RecoveryHarness::settings(|_| {}, 3, pause)?;
    std::fs::write(
        p.h().dir.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: simulator, udid: a}]\n",
    )?;
    let boot = p.h().boot.clone();
    *p.h().worker.before.lock().unwrap() = Arc::new(move |command| {
        let answer = boot.answer(&command);
        Box::pin(async move {
            if command.last().is_some_and(|s| s == "HOME") {
                return Err(support::error("setup failed"));
            }
            Ok(answer)
        })
    });
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            p.base.h.services.runtime = core.vendor();
            let provider = AppleSimulatorProvider::new(provider_support::ProviderHarness::services(
                p.h(),
                p.connections.clone(),
                SimulatorRecovery::with_recovery_settings(3, pause),
            ))
            .map_err(|e| e.failure())?;
            let started = std::time::Instant::now();
            provider
                .initialize(p.h().services.context.clone())
                .await
                .map_err(|e| e.failure().clone())?;
            let mut configuration = (*p.h().services.configuration).clone();
            configuration.device_initialization_timeout_millis = TIMEOUT;
            let hooks = RuntimeHooks::live();
            let parent = core.job(JobKind::Supervisor, None);
            let scheduler = Scheduler::new(SchedulerParts {
                core: core.clone(),
                provider: provider.clone(),
                analytics: Arc::new(Analytics::from_lookup(Arc::new(pool_support::NoMetrics))),
                configuration: Arc::new(configuration),
                shard: TestShard {
                    tests: tests(),
                    flaky_tests: vec![],
                },
                tracker: Arc::new(pool_support::RecordingTracker::default()),
                timer: p.h().services.timer.clone(),
                bundle_identifier: Some(p.h().services.identifier.clone()),
                hooks: hooks.clone(),
                barriers: hooks.barriers(),
                ci: Ci::None,
                output: OutputDriver {
                    vendor: core.vendor(),
                    sink: Arc::new(pool_support::NullSink),
                },
                context: core.context(parent.clone()),
            });
            let outcome = scheduler.execute().await;
            let waited = started.elapsed();
            assert!(matches!(outcome, Err(marathon_core::error::VendorError::NoDevices(_))));
            // The failure comes at the timeout: retries neither end the wait early nor keep it open.
            assert!(waited >= std::time::Duration::from_millis(TIMEOUT as u64), "{waited:?}");
            assert!(scheduler.pools().is_empty());
            let context = core.context(parent.clone());
            if finished_retries {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while !p.log("setup failed 4 times, it stays out of the run") {
                    assert!(std::time::Instant::now() < deadline);
                    context.clock.delay(5, context.job.clone()).await?;
                }
            }
            // Termination ends a pause that is still running, here far longer than anything else in this test.
            provider.terminate().await.map_err(|e| e.failure().clone())?;
            assert!(started.elapsed() < std::time::Duration::from_millis(pause.max(60_000) as u64));
            assert_eq!(
                provider_support::home(&p.h().worker, "a"),
                if finished_retries { 4 } else { 1 }
            );
            parent.cancel(requested("test finished"));
            let _ = parent.join().await;
            Ok(())
        })
    })
    .map_err(|e| format!("{e:?}"))?;
    Ok(())
}
#[test]
fn every_device_failing_ends_in_no_devices_no_earlier_than_the_timeout() -> TestResult {
    no_devices_with_failing_setups(10, true)
}
#[test]
fn every_device_failing_ends_in_no_devices_at_the_timeout_with_a_retry_still_pending() -> TestResult {
    no_devices_with_failing_setups(120_000, false)
}
