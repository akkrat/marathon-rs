//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProviderWorkerRetryTest.kt
#![allow(dead_code)]
use crate::{
    provider_support::*,
    simulator_support::{Harness, TestResult},
    support::*,
};
use marathon_apple::apple_device::AppleDevice;
use marathon_apple::{
    cmd::failure,
    device::connection_factory::*,
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
};
use marathon_config::devices::{AppleTarget, Transport};
use marathon_core::{runtime::*, time::*, vendor::*};
use marathon_ios::device::{
    simulator_provider::{recovery::*, *},
    simulator_provisioner::ProvisioningConnections,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI64, Ordering},
};
pub type Answer = Arc<dyn Fn(Transport, bool, usize) -> ConnectionFuture<'static, Option<Executor>> + Send + Sync>;
pub struct ScriptConnections {
    pub inner: Arc<crate::provider_support::Connections>,
    pub calls: Mutex<Vec<(Transport, bool, i64)>>,
    pub answer: Mutex<Answer>,
    clock: Arc<Clock>,
}
impl ScriptConnections {
    pub fn attempts(&self, transport: &Transport, worker: bool) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, w, _)| t == transport && *w == worker)
            .count()
    }
    async fn acquire(
        &self,
        transport: Transport,
        worker: bool,
        caller: VendorContext,
    ) -> ConnectionResult<(Option<Executor>, Bridge)> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push((transport.clone(), worker, self.clock.now()));
            calls.iter().filter(|(t, w, _)| t == &transport && *w == worker).count()
        };
        let answer = self.answer.lock().unwrap().clone();
        let executor = answer(transport.clone(), worker, n).await?;
        if let Some(executor) = executor {
            self.inner.acquired.fetch_add(1, Ordering::SeqCst);
            return Ok((Some(executor), Arc::new(BridgeMock)));
        }
        self.inner.connect(transport, caller).await
    }
}
impl ProvisioningConnections for ScriptConnections {
    fn connect(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, (Option<Executor>, Bridge)> {
        Box::pin(self.acquire(transport, false, caller))
    }
    fn connect_worker(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, Executor> {
        Box::pin(async move {
            self.acquire(transport, true, caller)
                .await?
                .0
                .ok_or_else(|| crate::support::error("missing executor").into())
        })
    }
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()> {
        self.inner.release(executor)
    }
}
pub struct Time {
    pub clock: Arc<Clock>,
    pub wall_offset: AtomicI64,
    pub nano_offset: AtomicI64,
}
impl WallClock for Time {
    fn millis(&self, _: &'static str) -> ClockResult<i64> {
        Ok(self.clock.now().wrapping_add(self.wall_offset.load(Ordering::SeqCst)))
    }
    fn instant(&self, label: &'static str) -> ClockResult<jcompat::jtime::Instant> {
        Ok(jcompat::jtime::instant_of(self.millis(label)?.div_euclid(1000), 0).unwrap())
    }
}
impl MonotonicClock for Time {
    fn nanos(&self, _: &'static str) -> ClockResult<i64> {
        Ok(self
            .clock
            .now()
            .wrapping_mul(1_000_000)
            .wrapping_add(self.nano_offset.load(Ordering::SeqCst)))
    }
}
impl ReplayHooks for Time {
    fn draw_int(&self, l: &'static str) -> ClockResult<i32> {
        self.clock.draw_int(l)
    }
    fn choose_key(&self, l: &'static str, keys: &[String]) -> ClockResult<String> {
        self.clock.choose_key(l, keys)
    }
    fn allocate_id(&self, l: &'static str) -> ClockResult<String> {
        self.clock.allocate_id(l)
    }
    fn verify_consumed(&self) -> ClockResult<()> {
        Ok(())
    }
}
pub struct RecoveryHarness {
    pub base: ProviderHarness,
    pub connections: Arc<ScriptConnections>,
    pub recovery: Arc<SimulatorRecovery>,
    pub provider: Arc<AppleSimulatorProvider>,
    pub time: Arc<Time>,
}
impl RecoveryHarness {
    pub fn new(edit: impl FnOnce(&mut marathon_config::vendor::IosConfiguration)) -> TestResult<Self> {
        Self::settings(edit, 3, 30_000)
    }
    pub fn fast() -> TestResult<Self> {
        Self::new(|v| {
            v.worker_connection.backoff = duration(50);
            v.worker_connection.retry_interval = duration(100);
        })
    }
    pub fn settings(
        edit: impl FnOnce(&mut marathon_config::vendor::IosConfiguration),
        max: i32,
        pause: i64,
    ) -> TestResult<Self> {
        let mut base = ProviderHarness::new(edit)?;
        base.h
            .services
            .logging
            .registry
            .lock()
            .unwrap()
            .logger("AppleSimulatorProvider", Some(jcompat::logging::Level::Debug));
        let time = Arc::new(Time {
            clock: base.h.clock.clone(),
            wall_offset: 0.into(),
            nano_offset: 0.into(),
        });
        base.h.services.context.hooks = time.clone();
        base.h.services.monotonic = time.clone();
        base.h.services.wall = time.clone();
        base.h.services.timer = Arc::new(SystemTimer::new(time.clone()).unwrap());
        base.h.services.listeners = Arc::new(marathon_ios::listener::ProductionSimulatorListenerFactory);
        let connections = Arc::new(ScriptConnections {
            inner: base.connections.clone(),
            calls: Mutex::default(),
            answer: Mutex::new(Arc::new(|_, _, _| Box::pin(async { Ok(None) }))),
            clock: base.h.clock.clone(),
        });
        let recovery = SimulatorRecovery::with_recovery_settings(max, pause);
        let provider = AppleSimulatorProvider::new(ProviderHarness::services(
            &base.h,
            connections.clone(),
            recovery.clone(),
        ))?;
        base.h.services.logging.registry.lock().unwrap().logger(
            "com.malinskiy.marathon.apple.ios.device.SimulatorProvisioner",
            Some(jcompat::logging::Level::Info),
        );
        base.h
            .services
            .logging
            .registry
            .lock()
            .unwrap()
            .logger("AppleSimulatorProvider", Some(jcompat::logging::Level::Debug));
        Ok(Self {
            base,
            connections,
            recovery,
            provider,
            time,
        })
    }
    pub fn h(&self) -> &Harness {
        &self.base.h
    }
    pub fn answer(
        &self,
        answer: impl Fn(Transport, bool, usize) -> ConnectionFuture<'static, Option<Executor>> + Send + Sync + 'static,
    ) {
        *self.connections.answer.lock().unwrap() = Arc::new(answer);
    }
    pub fn fail_initial(&self) {
        self.answer(|_, w, n| Box::pin(async move { if w && n == 1 { Err(unreachable()) } else { Ok(None) } }));
    }
    pub async fn update(&self, ids: &[&str]) -> TestResult {
        self.update_workers(vec![(Transport::Local, targets(ids))]).await
    }
    pub async fn update_workers(&self, workers: Vec<(Transport, Vec<AppleTarget>)>) -> TestResult {
        self.provider
            .process_update(snapshot(workers), self.h().services.context.clone())
            .await
            .map_err(test_error)?;
        Ok(())
    }
    pub async fn start(&self, ids: &[&str]) -> TestResult {
        let mut yaml = "workers:\n- transport: {type: local}\n  devices:\n".to_owned();
        for id in ids {
            yaml.push_str(&format!("  - {{type: simulator, udid: {id}}}\n"));
        }
        std::fs::write(self.h().dir.path().join("Marathondevices"), yaml)?;
        self.provider
            .initialize(self.h().services.context.clone())
            .await
            .map_err(test_error)?;
        Ok(())
    }
    pub async fn connected(&self, n: usize) -> Vec<Simulator> {
        let mut result = Vec::new();
        for _ in 0..n {
            loop {
                let event = watchdog(self.provider.subscribe().receive()).await.unwrap().unwrap();
                if let DeviceEvent::Connected(device) = event {
                    result.push(
                        self.provider
                            .devices()
                            .into_iter()
                            .find(|d| d.device == device)
                            .unwrap()
                            .handle,
                    );
                    break;
                }
            }
        }
        result
    }
    pub async fn scan(&self) {
        self.recovery
            .reconnect(self.provider.clone(), self.h().services.context.clone())
            .await
            .unwrap();
    }
    pub async fn lose(&self, d: &Simulator) {
        self.h().boot.exit.store(1, Ordering::SeqCst);
        self.h().boot.state.store(1, Ordering::SeqCst);
        assert!(self.h().prepare(d).await.is_err());
        self.h().boot.exit.store(0, Ordering::SeqCst);
    }
    pub async fn finish(&self) -> TestResult {
        self.provider.terminate().await.map_err(test_error)?;
        // A90 is only expected on worker-loss replacement. These fake leases are counters.
        Ok(())
    }
    pub async fn balanced(&self) -> TestResult {
        self.finish().await?;
        eventually(|| {
            self.base.connections.acquired.load(Ordering::SeqCst)
                == self.base.connections.released.load(Ordering::SeqCst)
        })
        .await;
        Ok(())
    }
    pub fn log(&self, s: &str) -> bool {
        self.h().logs.lock().unwrap().iter().any(|(_, _, message, cause)| {
            message.contains(s)
                || cause
                    .as_ref()
                    .is_some_and(|e| e.to_java_string().to_owned().contains(s))
        })
    }
    pub fn current(&self, id: &str) -> Simulator {
        self.provider
            .devices()
            .into_iter()
            .find(|d| d.handle.udid() == id)
            .unwrap()
            .handle
    }
}
pub fn unreachable() -> ConnectionError {
    ConnectionError::Device(DeviceFailureException::new(
        DeviceFailureReason::UnreachableHost,
        Some("No route to host".into()),
        None,
    ))
}
pub fn auth() -> ConnectionError {
    ConnectionError::Vendor(marathon_core::error::VendorError::Other(failure(
        "net.schmizz.sshj.userauth.UserAuthException",
        "Exhausted available authentication methods",
    )))
}
pub async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}
pub async fn advance(ms: u64) {
    settle().await;
    tokio::time::advance(std::time::Duration::from_millis(ms)).await;
    settle().await;
}
