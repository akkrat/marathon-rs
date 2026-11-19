//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/Detached.kt
//! Retained acquisition only (approved P1); undelivered successes keep their release capability.

use super::simulator_provisioner::ProvisioningConnections;
use marathon_apple::{
    cmd::{failure, retained_tasks::RetainedTasks},
    device::connection_factory::*,
};
use marathon_config::devices::Transport;
use marathon_core::{
    error::VendorError,
    executor::shared::Signal,
    runtime::{VendorFuture, VendorRuntime},
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

pub struct DetachedConnections {
    inner: Arc<dyn ProvisioningConnections>,
    runtime: Arc<dyn VendorRuntime>,
    tasks: RetainedTasks,
    termination: Arc<Mutex<Option<(i64, VendorContext)>>>,
}
impl DetachedConnections {
    pub fn new(inner: Arc<dyn ProvisioningConnections>, runtime: Arc<dyn VendorRuntime>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            runtime,
            tasks: RetainedTasks::default(),
            termination: Arc::default(),
        })
    }
    async fn acquire<T: Send + 'static>(
        &self,
        caller: VendorContext,
        work: impl Future<Output = ConnectionResult<T>> + Send + 'static,
        executor: impl FnOnce(&T) -> Option<Executor> + Send + 'static,
    ) -> ConnectionResult<T> {
        if let Some(reason) = caller.cancellation.reason() {
            return Err(VendorError::Cancelled(reason).into());
        }
        let inner = self.inner.clone();
        let termination = self.termination.clone();
        let (tx, rx) = oneshot::channel();
        self.tasks.spawn(self.runtime.as_ref(), async move {
            let result = work.await;
            let lease = result.as_ref().ok().and_then(executor);
            let (ack, received) = oneshot::channel();
            let delivered = tx.send((result, ack)).is_ok() && received.await.is_ok();
            if !delivered && let Some(lease) = lease {
                release(inner.as_ref(), &termination, lease).await?;
            }
            Ok(())
        });
        tokio::select! { biased;
            reason = caller.cancellation.cancelled() => Err(VendorError::Cancelled(reason).into()),
            result = rx => {
                let (result, ack) = result.map_err(|e| VendorError::Other(failure(FailureKind::IllegalState, e.to_string())))?;
                // A result can become ready in the same turn as cancellation. Do not acknowledge it then.
                if let Some(reason) = caller.cancellation.reason() { return Err(VendorError::Cancelled(reason).into()); }
                let _ = ack.send(());
                result
            }
        }
    }
}
async fn release(
    inner: &dyn ProvisioningConnections,
    termination: &Mutex<Option<(i64, VendorContext)>>,
    executor: Executor,
) -> marathon_core::error::VendorResult<()> {
    let limit = termination.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match limit {
        Some((deadline, context)) => inner.release_before(executor, Some(deadline), context).await,
        None => inner.release(executor).await,
    }
}
impl ProvisioningConnections for DetachedConnections {
    fn seal_for_signal(&self, finish: marathon_core::signal_finish::SignalFinish) {
        self.inner.seal_for_signal(finish);
    }
    fn seal_for_termination(&self, deadline: i64, context: VendorContext) {
        self.termination
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert((deadline, context));
    }
    fn transport_log_text(&self, transport: &Transport, local: &str) -> String {
        self.inner.transport_log_text(transport, local)
    }
    fn connect(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, (Option<Executor>, Bridge)> {
        let inner = self.inner.clone();
        let protected = VendorContext {
            cancellation: Signal::new(),
            ..caller.clone()
        };
        Box::pin(self.acquire(
            caller,
            async move { inner.connect(transport, protected).await },
            |pair| pair.0.clone(),
        ))
    }
    fn connect_worker(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, Executor> {
        let inner = self.inner.clone();
        let protected = VendorContext {
            cancellation: Signal::new(),
            ..caller.clone()
        };
        Box::pin(self.acquire(
            caller,
            async move { inner.connect_worker(transport, protected).await },
            |executor| Some(executor.clone()),
        ))
    }
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()> {
        Box::pin(release(self.inner.as_ref(), &self.termination, executor))
    }
    fn release_before(
        &self,
        executor: Executor,
        deadline: Option<i64>,
        context: VendorContext,
    ) -> VendorFuture<'_, ()> {
        self.inner.release_before(executor, deadline, context)
    }
    fn quarantine(&self, executor: &Executor) {
        self.inner.quarantine(executor)
    }
}

/// The provider overrides SimulatorProvisioner's legacy nullable `create` call with
/// `createOrThrow`, so authentication is not flattened into a missing executor.
pub struct ProviderConnections(pub Arc<ConnectionFactory>);
impl ProvisioningConnections for ProviderConnections {
    fn seal_for_signal(&self, finish: marathon_core::signal_finish::SignalFinish) {
        self.0.seal_for_signal(finish);
    }
    fn transport_log_text(&self, transport: &Transport, local: &str) -> String {
        self.0.transport_log_text(transport, local)
    }
    fn connect(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, (Option<Executor>, Bridge)> {
        Box::pin(async move {
            self.0
                .create_or_throw(transport, caller)
                .await
                .map(|(executor, bridge)| (Some(executor), bridge))
        })
    }
    fn connect_worker(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, Executor> {
        Box::pin(async move { self.0.connect_worker(transport, caller).await })
    }
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()> {
        Box::pin(async move { self.0.dispose(executor).await })
    }
    fn release_before(&self, executor: Executor, deadline: Option<i64>, _: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move { self.0.dispose_before(executor, deadline).await })
    }
    fn quarantine(&self, executor: &Executor) {
        self.0.quarantine(executor)
    }
}
