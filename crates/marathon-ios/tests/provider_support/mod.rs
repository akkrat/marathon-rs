//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProviderTest.kt
#![allow(dead_code)]
use crate::{
    simulator_support::*,
    support::{BridgeMock, Runtime},
};
use jcompat::file_manager::JavaPath;
use marathon_apple::{
    apple_device_registry::AppleDeviceRegistry,
    device::connection_factory::{Bridge, ConnectionFuture, Executor},
};
use marathon_config::{
    binding::{BindingContext, Profile},
    devices::{AppleTarget, MarathonDevices, Transport, Worker},
};
use marathon_core::{
    executor::shared::SharedDeferred,
    runtime::{Deferred, VendorFuture},
    vendor::{DeviceEvent, DeviceProvider, VendorContext},
};
use marathon_ios::device::{
    error::ProvisioningError, simulator_provider::*, simulator_provisioner::ProvisioningConnections,
    timing::ProvisioningTiming,
};
use marathon_test_support::apple::FakeSimulatorWorker;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Default)]
pub struct Recovery {
    pub fail_termination: std::sync::atomic::AtomicBool,
    pub ticks: AtomicUsize,
    pub missing: Mutex<Vec<Vec<AppleTarget>>>,
    pub failed: Mutex<Vec<Vec<AppleTarget>>>,
}
impl ProviderRecovery for Recovery {
    fn connect_worker(
        &self,
        transport: Transport,
        connections: Arc<dyn ProvisioningConnections>,
        caller: VendorContext,
    ) -> ConnectionFuture<'_, Executor> {
        Box::pin(async move { connections.connect_worker(transport, caller).await })
    }
    fn reconnect(&self, _: Arc<AppleSimulatorProvider>, _: VendorContext) -> VendorFuture<'_, ()> {
        self.ticks.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn admit(&self, _: &String, _: &Simulator) -> marathon_ios::device::error::ProvisioningResult<()> {
        Ok(())
    }
    fn offer(&self, _: &Transport, _: &[AppleTarget]) -> bool {
        false
    }
    fn missing(&self, _: Transport, targets: Vec<AppleTarget>, _: i64, _: VendorContext) -> VendorFuture<'_, ()> {
        self.missing.lock().unwrap().push(targets);
        Box::pin(async { Ok(()) })
    }
    fn connect_failed(
        &self,
        _: Transport,
        _: Vec<AppleTarget>,
        _: i64,
        error: ProvisioningError,
        _: VendorContext,
    ) -> VendorFuture<'_, ()> {
        Box::pin(async move { Err(error.into_vendor()) })
    }
    fn setup_failed(&self, _: Transport, targets: Vec<AppleTarget>, _: i64) -> VendorFuture<'_, ()> {
        self.failed.lock().unwrap().push(targets);
        Box::pin(async { Ok(()) })
    }
    fn forget_setup(&self, _: &String) {}
    fn terminate(&self) -> VendorFuture<'_, ()> {
        Box::pin(async {
            if self.fail_termination.load(Ordering::SeqCst) {
                return Err(marathon_core::error::VendorError::Other(marathon_apple::cmd::failure(
                    "java.lang.IllegalStateException",
                    "recovery termination failed",
                )));
            }
            Ok(())
        })
    }
}
pub struct Connections {
    pub workers: Mutex<Vec<(Transport, Executor)>>,
    pub acquired: AtomicUsize,
    pub released: AtomicUsize,
    /// The release with this number, counting from 1, fails after being counted.
    pub fail_release: AtomicUsize,
    pub trace: Arc<Mutex<Vec<String>>>,
}
impl ProvisioningConnections for Connections {
    fn connect(&self, transport: Transport, _: VendorContext) -> ConnectionFuture<'_, (Option<Executor>, Bridge)> {
        Box::pin(async move {
            let worker = self
                .workers
                .lock()
                .unwrap()
                .iter()
                .find(|(t, _)| t == &transport)
                .map(|(_, w)| w.clone());
            self.acquired.fetch_add(1, Ordering::SeqCst);
            self.trace
                .lock()
                .unwrap()
                .push(format!("acquire:{}", transport.id().to_owned()));
            Ok((worker, Arc::new(BridgeMock) as Bridge))
        })
    }
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let number = self.released.fetch_add(1, Ordering::SeqCst) + 1;
            self.trace
                .lock()
                .unwrap()
                .push(format!("release:{}", executor.host().id));
            if number == self.fail_release.load(Ordering::SeqCst) {
                return Err(crate::support::error("release failed"));
            }
            Ok(())
        })
    }
}
pub struct ProviderHarness {
    pub h: Harness,
    pub connections: Arc<Connections>,
    pub recovery: Arc<Recovery>,
    pub provider: Arc<AppleSimulatorProvider>,
}
impl ProviderHarness {
    pub fn new(edit: impl FnOnce(&mut marathon_config::vendor::IosConfiguration)) -> TestResult<Self> {
        let h = Harness::new(edit)?;
        let connections = Arc::new(Connections {
            workers: Mutex::new(vec![(Transport::Local, h.worker.clone())]),
            acquired: 0.into(),
            released: 0.into(),
            fail_release: 0.into(),
            trace: Arc::default(),
        });
        let recovery = Arc::new(Recovery::default());
        let provider = Self::build(&h, connections.clone(), recovery.clone())?;
        Ok(Self {
            h,
            connections,
            recovery,
            provider,
        })
    }
    pub fn build(
        h: &Harness,
        connections: Arc<Connections>,
        recovery: Arc<Recovery>,
    ) -> TestResult<Arc<AppleSimulatorProvider>> {
        Ok(AppleSimulatorProvider::new(Self::services(h, connections, recovery))?)
    }
    pub fn services(
        h: &Harness,
        connections: Arc<dyn ProvisioningConnections>,
        recovery: Arc<dyn ProviderRecovery>,
    ) -> ProviderServices {
        let services = h.services.clone();
        ProviderServices {
            vendor: services.vendor.clone(),
            runtime: services.runtime.clone(),
            context: services.context.clone(),
            binding: Arc::new(BindingContext {
                profile: Profile::Marathondevices,
                marathonfile: JavaPath::from_text("/Marathondevices"),
                marathonfile_dir: JavaPath::from_text("/"),
                process_dir: JavaPath::from_text(h.dir.path().to_str().unwrap()),
                environment: vec![],
                now: jcompat::jtime::instant_of(0, 0).unwrap(),
                analytics_tracking: false,
                bugsnag_reporting: false,
            }),
            connections,
            binaries: Arc::new(move |executor| {
                Arc::new(marathon_apple::bin::AppleBinaryEnvironment::with_tools(
                    marathon_apple::bin::Tools {
                        executor,
                        timeouts: Arc::new(services.vendor.timeout_configuration.clone()),
                        runtime: services.runtime.clone(),
                        logging: services.logging.clone(),
                    },
                    5000,
                    5000,
                ))
            }),
            factory: h.factory(),
            artifacts: h.services.artifacts.clone(),
            registry: Arc::new(AppleDeviceRegistry::default()),
            timing: ProvisioningTiming::new(h.services.timer.clone(), h.services.logging.clone()),
            logging: h.services.logging.clone(),
            recovery,
        }
    }
    pub fn worker(&self, transport: Transport, ids: &[&str]) -> Arc<FakeSimulatorWorker> {
        let worker = Arc::new(
            FakeSimulatorWorker::new(Arc::new(Runtime), &mut self.h.services.logging.registry.lock().unwrap())
                .with_host_id(&transport.id().to_owned()),
        );
        worker.add(ids);
        let boot = self.h.boot.clone();
        *worker.before.lock().unwrap() = Arc::new(move |command| {
            let answer = boot.answer(&command);
            Box::pin(async move { Ok(answer) })
        });
        self.connections
            .workers
            .lock()
            .unwrap()
            .push((transport, worker.clone()));
        worker
    }
    pub async fn update(&self, ids: &[&str]) -> TestResult {
        self.provider
            .process_update(
                snapshot(vec![(Transport::Local, targets(ids))]),
                self.h.services.context.clone(),
            )
            .await
            .map_err(test_error)?;
        Ok(())
    }
    pub async fn connected(&self, count: usize) -> Vec<Simulator> {
        let mut devices = Vec::new();
        for _ in 0..count {
            let event = crate::support::watchdog(self.provider.subscribe().receive())
                .await
                .unwrap()
                .unwrap();
            let DeviceEvent::Connected(device) = event else {
                panic!("expected connect")
            };
            devices.push(
                self.provider
                    .devices()
                    .into_iter()
                    .find(|d| d.device == device)
                    .unwrap()
                    .handle,
            );
        }
        devices
    }
    pub async fn finish(&self) -> TestResult {
        self.provider.terminate().await.map_err(test_error)?;
        assert_eq!(
            self.connections.acquired.load(Ordering::SeqCst),
            self.connections.released.load(Ordering::SeqCst)
        );
        Ok(())
    }
}
pub fn targets(ids: &[&str]) -> Vec<AppleTarget> {
    ids.iter()
        .map(|s| AppleTarget::Simulator { udid: (*s).into() })
        .collect()
}
pub fn snapshot(workers: Vec<(Transport, Vec<AppleTarget>)>) -> MarathonDevices {
    MarathonDevices {
        workers: workers
            .into_iter()
            .map(|(transport, devices)| Worker { transport, devices })
            .collect(),
    }
}
pub fn count(worker: &FakeSimulatorWorker, command: &[&str]) -> usize {
    worker
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.iter().map(String::as_str).eq(command.iter().copied()))
        .count()
}
pub fn home(worker: &FakeSimulatorWorker, id: &str) -> usize {
    count(worker, &["xcrun", "simctl", "getenv", id, "HOME"])
}
pub async fn eventually(mut predicate: impl FnMut() -> bool) {
    crate::support::watchdog(async {
        while !predicate() {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await;
}
pub async fn batch(
    h: &Harness,
    device: Simulator,
    gate: Option<Arc<SharedDeferred<()>>>,
) -> (
    Arc<BatchScript>,
    tokio::task::JoinHandle<(
        marathon_core::error::VendorResult<()>,
        Option<Arc<marathon_core::model::TestBatchResults>>,
    )>,
) {
    h.prepare(&device).await.unwrap();
    let script = Arc::new(BatchScript::default());
    *script.plan.lock().unwrap() = OutputPlan {
        hang: true,
        close_gate: gate,
        ..Default::default()
    };
    script.attach(&h.worker);
    let job = spawn_batch(h, device, h.services.context.clone());
    script.opened.wait().await.unwrap();
    (script, job)
}
pub fn fixture(case: &str) -> serde_json::Value {
    include_str!("../../../../fixtures/oracle/simulator-provider/probes.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .find(|row| row["case"] == case)
        .unwrap()["value"]
        .clone()
}

pub fn test_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
