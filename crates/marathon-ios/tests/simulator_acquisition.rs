//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/Detached.kt
//! Barrier witnesses for the retained acquisition adapter and WP 4.3 acknowledgment seam.
use marathon_error::{FailureKind, SshKind};
mod provider_support;
mod recovery_support;
mod simulator_support;
mod support;
use marathon_apple::{
    cmd::{CommandExecutor, CommandHost, CommandRequest, CommandSession},
    device::connection_factory::*,
};
use marathon_config::devices::Transport;
use marathon_core::{
    executor::shared::{SharedDeferred, Signal, requested},
    runtime::VendorFuture,
    vendor::VendorContext,
};
use marathon_ios::device::{detached::DetachedConnections, simulator_provisioner::ProvisioningConnections};
use recovery_support::settle;
use simulator_support::{Harness, TestResult};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};
struct Counting {
    inner: Executor,
    closed: AtomicUsize,
}
impl CommandExecutor for Counting {
    fn host(&self) -> &CommandHost {
        self.inner.host()
    }
    fn component(&self) -> &marathon_logging::Component {
        self.inner.component()
    }
    fn log_error(&self, s: String) {
        self.inner.log_error(s)
    }
    fn connected(&self) -> bool {
        self.inner.connected()
    }
    fn execute(&self, r: CommandRequest, c: VendorContext) -> VendorFuture<'_, CommandSession> {
        self.inner.execute(r, c)
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async {
            self.closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
struct ConnectorGate {
    gate: Arc<SharedDeferred<()>>,
    entered: Arc<SharedDeferred<()>>,
    executor: Executor,
}
impl Connector for ConnectorGate {
    fn prepare(&self, _: Transport) -> ConnectionFuture<'_, PreparedConnection> {
        let gate = self.gate.clone();
        let entered = self.entered.clone();
        let executor = self.executor.clone();
        Box::pin(async move {
            Ok(Box::new(move || {
                Box::pin(async move {
                    entered.complete(Ok(()));
                    gate.wait().await?;
                    Ok(executor)
                }) as ConnectionFuture<'static, Executor>
            }) as PreparedConnection)
        })
    }
}
struct Probe;
impl AddressProbe for Probe {
    fn check(&self, _: Transport, _: jcompat::jtime::Duration) -> ConnectionFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Setup {
    h: Harness,
    factory: Arc<ConnectionFactory>,
    count: Arc<Counting>,
    gate: Arc<SharedDeferred<()>>,
    entered: Arc<SharedDeferred<()>>,
}
impl Setup {
    fn new(shared: bool) -> TestResult<Self> {
        let h = Harness::new(|_| {})?;
        let count = Arc::new(Counting {
            inner: h.worker.clone(),
            closed: 0.into(),
        });
        let gate = SharedDeferred::new();
        let entered = SharedDeferred::new();
        let mut ssh = h.services.vendor.ssh.clone();
        ssh.share_worker_connection = shared;
        let factory = ConnectionFactory::with_seams(
            false,
            ssh,
            h.services.vendor.rsync.clone(),
            support::duration(100),
            8,
            h.services.runtime.clone(),
            h.services.context.clock.clone(),
            h.services.logging.clone(),
            Arc::new(ConnectorGate {
                gate: gate.clone(),
                entered: entered.clone(),
                executor: count.clone(),
            }),
            Arc::new(Probe),
        )
        .map_err(|e| format!("{e:?}"))?;
        Ok(Self {
            h,
            factory,
            count,
            gate,
            entered,
        })
    }
    fn context(&self, signal: Arc<Signal>) -> VendorContext {
        VendorContext {
            cancellation: signal,
            ..self.h.services.context.clone()
        }
    }
    fn detached(&self) -> Arc<DetachedConnections> {
        DetachedConnections::new(Arc::new(self.factory.clone()), self.h.services.runtime.clone())
    }
}
fn oracle(case: &str) -> serde_json::Value {
    include_str!("../../../fixtures/oracle/simulator-recovery/probes.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["case"] == case)
        .unwrap()["value"]
        .clone()
}
fn poll<T>(future: Pin<&mut impl Future<Output = T>>) -> Poll<T> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}
#[tokio::test(start_paused = true)]
async fn ready_result_and_cancellation_release_exactly_once_for_shared_and_unshared_connections() -> TestResult {
    for shared in [false, true] {
        let s = Setup::new(shared)?;
        let adapter = s.detached();
        let signal = Signal::new();
        let mut acquire = Box::pin(adapter.connect_worker(support::worker("127.0.0.1"), s.context(signal.clone())));
        assert!(poll(acquire.as_mut()).is_pending());
        s.entered.wait().await.map_err(provider_support::test_error)?;
        s.gate.complete(Ok(()));
        settle().await;
        signal.cancel(requested("ready/cancel barrier"));
        let cancelled = matches!(
            acquire.await,
            Err(ConnectionError::Vendor(marathon_core::error::VendorError::Cancelled(_)))
        );
        assert_eq!(
            cancelled,
            oracle("detached-ready-cancel")["cancelled"].as_bool().unwrap()
        );
        provider_support::eventually(|| s.count.closed.load(Ordering::SeqCst) == 1).await;
        s.factory.join_pending().await.map_err(provider_support::test_error)?;
        assert_eq!(
            s.count.closed.load(Ordering::SeqCst) as u64,
            oracle("detached-ready-cancel")["released"].as_u64().unwrap()
        );
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn dropped_awaiter_returns_its_late_success_once_and_keeps_shared_peer_lease() -> TestResult {
    for shared in [false, true] {
        let s = Setup::new(shared)?;
        let adapter = s.detached();
        let peer = if shared {
            s.gate.complete(Ok(()));
            Some(
                s.factory
                    .connect_worker(support::worker("127.0.0.1"), s.h.services.context.clone())
                    .await
                    .map_err(|e| format!("{e:?}"))?,
            )
        } else {
            None
        };
        let mut acquire = Box::pin(adapter.connect_worker(support::worker("127.0.0.1"), s.h.services.context.clone()));
        assert!(poll(acquire.as_mut()).is_pending());
        drop(acquire);
        s.gate.complete(Ok(()));
        settle().await;
        s.factory.join_pending().await.map_err(provider_support::test_error)?;
        assert_eq!(s.count.closed.load(Ordering::SeqCst), usize::from(!shared));
        if let Some(peer) = peer {
            s.factory.dispose(peer).await.map_err(provider_support::test_error)?;
        }
        assert_eq!(s.count.closed.load(Ordering::SeqCst), 1);
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn u84_merged_factory_acknowledges_a_ready_result_before_observing_cancellation() -> TestResult {
    let s = Setup::new(false)?;
    let signal = Signal::new();
    let mut acquire = Box::pin(
        s.factory
            .connect_worker(support::worker("127.0.0.1"), s.context(signal.clone())),
    );
    assert!(poll(acquire.as_mut()).is_pending());
    s.entered.wait().await.map_err(provider_support::test_error)?;
    s.gate.complete(Ok(()));
    settle().await;
    signal.cancel(requested("ready/cancel barrier"));
    let result = acquire.await;
    assert!(result.is_ok());
    if let Ok(executor) = result {
        s.factory
            .dispose(executor)
            .await
            .map_err(provider_support::test_error)?;
    }
    s.factory.join_pending().await.map_err(provider_support::test_error)?;
    assert_eq!(s.count.closed.load(Ordering::SeqCst), 1);
    Ok(())
}
struct CurrentConnector(std::sync::Mutex<Executor>);
impl Connector for CurrentConnector {
    fn prepare(&self, _: Transport) -> ConnectionFuture<'_, PreparedConnection> {
        let executor = self.0.lock().unwrap().clone();
        Box::pin(async move {
            Ok(
                Box::new(move || Box::pin(async move { Ok(executor) }) as ConnectionFuture<'static, Executor>)
                    as PreparedConnection,
            )
        })
    }
}
#[tokio::test(start_paused = true)]
async fn a90_worker_loss_leaks_predecessor_lease_while_connected_loss_retains_and_releases_it() -> TestResult {
    use marathon_core::vendor::DeviceProvider;
    use marathon_ios::device::simulator_provider::{
        AppleSimulatorProvider, ProviderRecovery, recovery::SimulatorRecovery,
    };
    for connected_loss in [false, true] {
        let p = recovery_support::RecoveryHarness::fast()?;
        let transport = support::worker("127.0.0.1");
        let first = p.base.worker(transport.clone(), &["a"]);
        let next = p.base.worker(transport.clone(), &["a"]);
        let old = Arc::new(Counting {
            inner: first.clone(),
            closed: 0.into(),
        });
        let fresh = Arc::new(Counting {
            inner: next,
            closed: 0.into(),
        });
        let connector = Arc::new(CurrentConnector(std::sync::Mutex::new(old.clone())));
        let mut ssh = p.h().services.vendor.ssh.clone();
        ssh.share_worker_connection = true;
        let factory = ConnectionFactory::with_seams(
            false,
            ssh,
            p.h().services.vendor.rsync.clone(),
            support::duration(100),
            8,
            p.h().services.runtime.clone(),
            p.h().services.context.clock.clone(),
            p.h().services.logging.clone(),
            connector.clone(),
            Arc::new(Probe),
        )
        .map_err(|e| format!("{e:?}"))?;
        let recovery = SimulatorRecovery::new();
        let provider = AppleSimulatorProvider::new(provider_support::ProviderHarness::services(
            p.h(),
            Arc::new(factory.clone()),
            recovery.clone(),
        ))?;
        provider
            .process_update(
                provider_support::snapshot(vec![(transport.clone(), provider_support::targets(&["a"]))]),
                p.h().services.context.clone(),
            )
            .await
            .map_err(provider_support::test_error)?;
        provider
            .subscribe()
            .receive()
            .await
            .map_err(provider_support::test_error)?;
        let original = provider.devices()[0].handle.clone();
        if connected_loss {
            p.h().boot.exit.store(1, Ordering::SeqCst);
            p.h().boot.state.store(1, Ordering::SeqCst);
            assert!(p.h().prepare(&original).await.is_err());
            p.h().boot.exit.store(0, Ordering::SeqCst);
        } else {
            *connector.0.lock().unwrap() = fresh.clone();
            first.connected.store(false, Ordering::SeqCst);
        }
        recovery
            .reconnect(provider.clone(), p.h().services.context.clone())
            .await
            .map_err(provider_support::test_error)?;
        provider_support::eventually(|| !Arc::ptr_eq(&provider.devices()[0].handle, &original)).await;
        provider.terminate().await.map_err(provider_support::test_error)?;
        factory.join_pending().await.map_err(provider_support::test_error)?;
        let case = if connected_loss {
            "connected-loss-leases"
        } else {
            "A90-worker-loss-leases"
        };
        let expected: serde_json::Value = include_str!("../../../fixtures/oracle/simulator-recovery/probes.jsonl")
            .lines()
            .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
            .find(|v| v["case"] == case)
            .unwrap()["value"]
            .clone();
        assert_eq!(
            old.closed.load(Ordering::SeqCst) == 0,
            expected["oldRemaining"].as_bool().unwrap()
        );
        assert_eq!(fresh.closed.load(Ordering::SeqCst), usize::from(!connected_loss));
        if !connected_loss {
            factory
                .dispose(old.clone())
                .await
                .map_err(provider_support::test_error)?;
        }
        assert_eq!(old.closed.load(Ordering::SeqCst), 1);
    }
    Ok(())
}
struct AuthOnSimulator {
    calls: AtomicUsize,
    executor: Executor,
}
impl Connector for AuthOnSimulator {
    fn prepare(&self, _: Transport) -> ConnectionFuture<'_, PreparedConnection> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let executor = self.executor.clone();
        Box::pin(async move {
            Ok(Box::new(move || {
                Box::pin(async move {
                    if n == 1 {
                        return Err(ConnectionError::Device(
                            marathon_apple::logparser::parser::DeviceFailureException::new(
                                marathon_apple::logparser::parser::DeviceFailureReason::UnreachableHost,
                                None,
                                Some(marathon_apple::cmd::remote::ssh::sshj::SshFailure::new(
                                    FailureKind::Ssh(SshKind::Authentication),
                                    "Exhausted available authentication methods",
                                )),
                            ),
                        ));
                    }
                    Ok(executor)
                }) as ConnectionFuture<'static, Executor>
            }) as PreparedConnection)
        })
    }
}
#[tokio::test(start_paused = true)]
async fn production_provider_connections_keep_simulator_authentication_typed_and_do_not_retry_it() -> TestResult {
    use marathon_core::vendor::DeviceProvider;
    use marathon_ios::device::{
        detached::ProviderConnections,
        simulator_provider::{AppleSimulatorProvider, recovery::SimulatorRecovery},
    };
    let p = recovery_support::RecoveryHarness::fast()?;
    let transport = support::worker("127.0.0.1");
    let worker = p.base.worker(transport.clone(), &["a"]);
    let connector = Arc::new(AuthOnSimulator {
        calls: 0.into(),
        executor: worker,
    });
    let mut ssh = p.h().services.vendor.ssh.clone();
    ssh.share_worker_connection = false;
    let factory = ConnectionFactory::with_seams(
        false,
        ssh,
        p.h().services.vendor.rsync.clone(),
        support::duration(100),
        8,
        p.h().services.runtime.clone(),
        p.h().services.context.clock.clone(),
        p.h().services.logging.clone(),
        connector.clone(),
        Arc::new(Probe),
    )
    .map_err(|e| format!("{e:?}"))?;
    let provider = AppleSimulatorProvider::new(provider_support::ProviderHarness::services(
        p.h(),
        Arc::new(ProviderConnections(factory.clone())),
        SimulatorRecovery::new(),
    ))?;
    provider
        .process_update(
            provider_support::snapshot(vec![(transport, provider_support::targets(&["a"]))]),
            p.h().services.context.clone(),
        )
        .await
        .map_err(provider_support::test_error)?;
    recovery_support::advance(10_000).await;
    assert!(provider.devices().is_empty());
    assert_eq!(connector.calls.load(Ordering::SeqCst), 2);
    provider.terminate().await.map_err(provider_support::test_error)?;
    factory.join_pending().await.map_err(provider_support::test_error)?;
    Ok(())
}
