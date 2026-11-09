//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/WorkerProvisioningContextTest.kt
mod simulator_support;
mod support;
use marathon_apple::{
    cmd::CommandResult,
    device::connection_factory::{Bridge, ConnectionFuture, Executor},
};
use marathon_config::{devices::Transport, vendor::SshAuthentication};
use marathon_core::{
    runtime::{Deferred, VendorFuture},
    vendor::VendorContext,
};
use marathon_ios::{
    apple_simulator_device::AppleSimulatorDevice,
    device::{
        provisioning_plan::ProvisioningPlan,
        simulator_provisioner::{ProvisioningConnections, SimulatorProvisioner},
        worker_provisioning_context::WorkerProvisioningContext,
    },
};
use marathon_test_support::apple::{
    FakeSimulatorWorker,
    fake_simulator_worker::{ENV, LIST, SYSTEM_PROFILER, UNAME, connection_exception},
};
use simulator_support::*;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::{BridgeMock, Runtime, profile};
struct Connections(Mutex<VecDeque<Executor>>);
impl ProvisioningConnections for Connections {
    fn connect(&self, _: Transport, _: VendorContext) -> ConnectionFuture<'_, (Option<Executor>, Bridge)> {
        Box::pin(async move {
            let mut q = self.0.lock().unwrap();
            let e = if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q[0].clone()
            };
            Ok((Some(e), Arc::new(BridgeMock) as Bridge))
        })
    }
    fn release(&self, _: Executor) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
type Devices = Arc<Mutex<Vec<Arc<AppleSimulatorDevice>>>>;
fn provisioner(
    h: &Harness,
    workers: Vec<Executor>,
    concurrency: i32,
) -> (SimulatorProvisioner<Arc<AppleSimulatorDevice>>, Devices) {
    let devices: Devices = Arc::default();
    let reg = devices.clone();
    let p = SimulatorProvisioner::new(
        Arc::new(Connections(Mutex::new(workers.into()))),
        h.factory(),
        Arc::new(move |_, d| {
            reg.lock().unwrap().push(d.handle);
            Box::pin(async { Ok(()) })
        }),
        concurrency,
        h.services.logging.clone(),
        h.services.context.hooks.as_ref(),
    )
    .unwrap();
    (p, devices)
}
fn new_worker(h: &Harness, shared: bool) -> Arc<FakeSimulatorWorker> {
    let w = FakeSimulatorWorker::new(Arc::new(Runtime), &mut h.services.logging.registry.lock().unwrap());
    let w = Arc::new(if shared {
        w.with_simulators(h.worker.simulators.clone())
    } else {
        w
    });
    let boot = h.boot.clone();
    *w.before.lock().unwrap() = Arc::new(move |c| {
        let answer = boot.answer(&c);
        Box::pin(async move { Ok(answer) })
    });
    w
}
#[tokio::test(start_paused = true)]
async fn a_failed_simulator_command_looks_the_simulator_up_in_a_new_inventory() -> TestResult {
    let h = Harness::new(|_| {})?;
    h.worker.add(["c"]);
    let sims = h.worker.simulators.clone();
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        if c == ENV {
            sims.lock().unwrap()["b"].available.store(false, Ordering::SeqCst);
        }
        let answer = if c.get(2).is_some_and(|s| s == "getenv")
            && c.get(3).is_some_and(|s| s != "a")
            && c.last().is_some_and(|s| s == "HOME")
        {
            Some(failing())
        } else {
            boot.answer(&c)
        };
        Box::pin(async move { Ok(answer) })
    });
    let (p, devices) = provisioner(&h, vec![h.worker.clone()], 1);
    p.existing(
        ["a", "b", "c"].map(str::to_owned),
        Transport::Local,
        h.services.context.clone(),
    )
    .await?;
    assert_eq!(
        devices
            .lock()
            .unwrap()
            .iter()
            .map(|d| d.serial().to_owned())
            .collect::<Vec<_>>(),
        ["a@local", "c@local"]
    );
    assert_eq!(h.worker.lists(), 3);
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_is_validated_against_an_inventory_no_older_than_the_bound() -> TestResult {
    let h = Harness::new(|_| {})?;
    let sims = h.worker.simulators.clone();
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        let sims = sims.clone();
        let answer = boot.answer(&c);
        Box::pin(async move {
            if c == ["xcrun", "simctl", "getenv", "a", "HOME"] {
                sims.lock().unwrap()["b"].available.store(false, Ordering::SeqCst);
                tokio::time::advance(std::time::Duration::from_secs(11)).await;
            }
            Ok(answer)
        })
    });
    let (p, devices) = provisioner(&h, vec![h.worker.clone()], 1);
    p.existing(["a".into(), "b".into()], Transport::Local, h.services.context.clone())
        .await?;
    assert_eq!(devices.lock().unwrap().len(), 1);
    assert_eq!(h.worker.lists(), 2);
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_lost_connection_invalidates_what_was_loaded_through_it_within_one_cycle() -> TestResult {
    let h = Harness::new(|_| {})?;
    h.worker.add(["c"]);
    let replacement = new_worker(&h, true);
    *replacement.arch.lock().unwrap() = "x86_64".into();
    let another = new_worker(&h, true);
    let devices: Devices = Arc::default();
    let reg = devices.clone();
    let first = h.worker.clone();
    let p = SimulatorProvisioner::new(
        Arc::new(Connections(Mutex::new(
            vec![h.worker.clone() as Executor, replacement.clone(), another.clone()].into(),
        ))),
        h.factory(),
        Arc::new(move |_, d| {
            if d.handle.serial().starts_with("a@") {
                first.connected.store(false, Ordering::SeqCst);
            }
            reg.lock().unwrap().push(d.handle);
            Box::pin(async { Ok(()) })
        }),
        1,
        h.services.logging.clone(),
        h.services.context.hooks.as_ref(),
    )?;
    p.existing(
        ["a", "b", "c"].map(str::to_owned),
        Transport::Local,
        h.services.context.clone(),
    )
    .await?;
    assert_eq!(
        devices
            .lock()
            .unwrap()
            .iter()
            .map(|d| marathon_core::vendor::Device::abi(d.as_ref()).to_owned())
            .collect::<Vec<_>>(),
        ["arm64", "x86_64", "x86_64"]
    );
    assert_eq!(replacement.lists(), 1);
    assert_eq!(replacement.worker_fact_commands(), 4);
    assert_eq!(another.worker_fact_commands(), 0);
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_timed_out_probe_is_not_shared() -> TestResult {
    let h = Harness::new(|_| {})?;
    let n = Arc::new(AtomicUsize::new(0));
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        let answer = if c.first().is_some_and(|s| s == SYSTEM_PROFILER) && n.fetch_add(1, Ordering::SeqCst) == 0 {
            Some(CommandResult {
                stdout: vec![],
                stderr: vec![],
                exit_code: None,
            })
        } else {
            boot.answer(&c)
        };
        Box::pin(async move { Ok(answer) })
    });
    let (p, devices) = provisioner(&h, vec![h.worker.clone()], 1);
    p.existing(["a".into(), "b".into()], Transport::Local, h.services.context.clone())
        .await?;
    assert_eq!(
        devices
            .lock()
            .unwrap()
            .iter()
            .map(|d| marathon_core::vendor::Device::info(d.as_ref())
                .device_features
                .contains(&marathon_core::model::DeviceFeature::Video))
            .collect::<Vec<_>>(),
        [false, true]
    );
    assert_eq!(h.worker.worker_fact_commands(), 8);
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_reconnect_takes_a_new_inventory_and_new_worker_facts() -> TestResult {
    let h = Harness::new(|_| {})?;
    h.worker.add(["c", "d"]);
    let replacement = new_worker(&h, true);
    *replacement.arch.lock().unwrap() = "x86_64".into();
    let (p, devices) = provisioner(
        &h,
        vec![
            h.worker.clone(),
            h.worker.clone(),
            h.worker.clone(),
            h.worker.clone(),
            replacement.clone(),
        ],
        1,
    );
    p.existing(
        ["a", "b", "c", "d"].map(str::to_owned),
        Transport::Local,
        h.services.context.clone(),
    )
    .await?;
    h.worker.connected.store(false, Ordering::SeqCst);
    h.worker.simulators.lock().unwrap().remove("c");
    let old = h.worker.commands.lock().unwrap().len();
    p.existing(
        ["a", "b", "c", "d"].map(str::to_owned),
        Transport::Local,
        h.services.context.clone(),
    )
    .await?;
    assert_eq!(devices.lock().unwrap().len(), 7);
    assert_eq!(replacement.lists(), 1);
    assert_eq!(replacement.worker_fact_commands(), 4);
    assert_eq!(h.worker.commands.lock().unwrap().len(), old);
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_failed_inventory_is_not_kept_and_the_next_simulator_takes_it_again() -> TestResult {
    let h = Harness::new(|_| {})?;
    let n = Arc::new(AtomicUsize::new(0));
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        let answer = if c == LIST && n.fetch_add(1, Ordering::SeqCst) == 0 {
            Some(failing())
        } else {
            boot.answer(&c)
        };
        Box::pin(async move { Ok(answer) })
    });
    let (p, devices) = provisioner(&h, vec![h.worker.clone()], 1);
    p.existing(["a".into(), "b".into()], Transport::Local, h.services.context.clone())
        .await?;
    assert_eq!(devices.lock().unwrap().len(), 1);
    assert_eq!(h.worker.lists(), 2);
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_created_simulator_that_cannot_be_looked_up_is_deleted() -> TestResult {
    let h = Harness::new(|_| {})?;
    h.worker.simulators.lock().unwrap().clear();
    let n = Arc::new(AtomicUsize::new(0));
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        let answer = if c == LIST && n.fetch_add(1, Ordering::SeqCst) == 1 {
            Some(failing())
        } else {
            boot.answer(&c)
        };
        Box::pin(async move { Ok(answer) })
    });
    let (p, devices) = provisioner(&h, vec![h.worker.clone()], 1);
    let cycle = Arc::new(WorkerProvisioningContext::new(Transport::Local, h.clock.clone()));
    p.provision(
        ProvisioningPlan {
            existing_simulators: Default::default(),
            needs_provisioning: vec![profile(), profile()],
            ..Default::default()
        },
        Transport::Local,
        h.binary(h.worker.clone()),
        cycle,
        h.services.context.clone(),
    )
    .await?;
    assert_eq!(devices.lock().unwrap().len(), 1);
    assert_eq!(h.worker.simulators.lock().unwrap().len(), 1);
    assert_eq!(h.worker.lists(), 3);
    assert_eq!(
        h.worker
            .commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.get(2).is_some_and(|s| s == "delete"))
            .count(),
        1
    );
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn worker_facts_are_kept_only_after_they_were_loaded_completely() -> TestResult {
    let h = Harness::new(|_| {})?;
    h.worker.add(["c"]);
    let env = Arc::new(AtomicUsize::new(0));
    let arch = Arc::new(AtomicUsize::new(0));
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        let answer = if c == ENV && env.fetch_add(1, Ordering::SeqCst) == 0 {
            Some(failing())
        } else if c == UNAME && arch.fetch_add(1, Ordering::SeqCst) == 0 {
            return Box::pin(async {
                Err(marathon_core::error::VendorError::Other(connection_exception(
                    "channel could not be opened",
                )))
            });
        } else {
            boot.answer(&c)
        };
        Box::pin(async move { Ok(answer) })
    });
    let (p, devices) = provisioner(&h, vec![h.worker.clone()], 1);
    p.existing(
        ["a", "b", "c"].map(str::to_owned),
        Transport::Local,
        h.services.context.clone(),
    )
    .await?;
    assert_eq!(
        devices
            .lock()
            .unwrap()
            .iter()
            .map(|d| marathon_core::vendor::Device::abi(d.as_ref()).to_owned())
            .collect::<Vec<_>>(),
        ["Unknown", "arm64"]
    );
    assert_eq!(h.worker.count(ENV), 3);
    assert_eq!(h.worker.count(UNAME), 2);
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn workers_with_different_credentials_on_one_host_share_neither_inventory_nor_facts() -> TestResult {
    let h = Harness::new(|_| {})?;
    let bob = new_worker(&h, false);
    bob.add(["b1", "b2"]);
    *bob.arch.lock().unwrap() = "x86_64".into();
    let (p, devices) = provisioner(&h, vec![h.worker.clone(), h.worker.clone(), bob.clone()], 1);
    let transport = |user: &str| Transport::Ssh {
        addr: "mac-1".into(),
        port: 22,
        authentication: Some(SshAuthentication::PublicKey {
            username: user.into(),
            key: std::path::PathBuf::from(user),
        }),
        check_reachability: true,
    };
    p.existing(["a".into(), "b".into()], transport("alice"), h.services.context.clone())
        .await?;
    p.existing(["b1".into(), "b2".into()], transport("bob"), h.services.context.clone())
        .await?;
    assert_eq!(devices.lock().unwrap().len(), 4);
    assert_eq!(h.worker.worker_fact_commands(), 4);
    assert_eq!(bob.worker_fact_commands(), 4);
    assert_eq!(bob.lists(), 1);
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_lookup_after_a_simulator_was_created_does_not_hold_up_other_simulators_of_the_worker() -> TestResult {
    let h = Harness::new(|_| {})?;
    let loading = marathon_core::executor::shared::SharedDeferred::new();
    let release = marathon_core::executor::shared::SharedDeferred::new();
    let l = loading.clone();
    let r = release.clone();
    let n = Arc::new(AtomicUsize::new(0));
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        let pause = c == LIST && n.fetch_add(1, Ordering::SeqCst) == 1;
        let l = l.clone();
        let r = r.clone();
        let answer = boot.answer(&c);
        Box::pin(async move {
            if pause {
                l.complete(Ok(()));
                r.wait().await?;
            }
            Ok(answer)
        })
    });
    let (p, devices) = provisioner(&h, vec![h.worker.clone()], 2);
    let cycle = Arc::new(WorkerProvisioningContext::new(Transport::Local, h.clock.clone()));
    let bin = h.binary(h.worker.clone());
    cycle.inventory(&bin, &h.services.context).await?;
    let op = p.provision(
        ProvisioningPlan {
            existing_simulators: ["a".into()].into(),
            needs_provisioning: vec![profile()],
            ..Default::default()
        },
        Transport::Local,
        bin,
        cycle,
        h.services.context.clone(),
    );
    tokio::pin!(op);
    tokio::select! {v=&mut op=>panic!("unexpected {v:?}"),_=loading.wait()=>{}}
    tokio::select! {
        value=&mut op=>panic!("completed over blocked refresh: {value:?}"),
        _=async { while devices.lock().unwrap().is_empty() {tokio::task::yield_now().await;} }=>{}
    }
    assert_eq!(devices.lock().unwrap().len(), 1);
    release.complete(Ok(()));
    op.await?;
    assert_eq!(devices.lock().unwrap().len(), 2);
    assert_eq!(h.worker.lists(), 2);
    let finished = devices.lock().unwrap().clone();
    h.finish(&finished).await;
    Ok(())
}
