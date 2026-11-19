//! D93: one Apple device's use of its worker's connection, and how its stopping work ended; Rust-only lifecycle support
//! for the ports of AppleSimulatorDevice.kt and MacosDevice.kt. Kotlin's disposal waits again for a batch whose cancelled
//! wait ran out and then runs the on-dispose lifecycle (or releases the Mac's connection) whatever that wait found.

use super::{CommandExecutor, CommandHost, CommandRequest, CommandSession, failure};
use marathon_core::{
    error::{Failure, VendorError},
    executor::shared::SharedDeferred,
    runtime::VendorFuture,
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use marathon_logging::Component;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

/// D93: how an Apple device's stopping work ended by its deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Termination {
    /// Its batch, preparations and sends have ended, with each send's cleanup confirmed, and its last xcodebuild session's
    /// producer has ended with its command closed.
    Confirmed,
    /// What had not ended, or not been confirmed to: the device's work may still run on the worker.
    Unconfirmed(Vec<String>),
}

/// D93: a stopping device's one window and its verdict, decided once: when everything the device ran at the seal has
/// ended, or at the window's end, whichever comes first. Sticky: every later caller (disposal, the provider's termination,
/// a recovery) reads it, and nothing that ends later turns an `Unconfirmed` into `Confirmed`.
pub struct ShutdownWindow {
    /// When the window ends, on the vendor clock.
    pub deadline: i64,
    verdict: Arc<SharedDeferred<Termination>>,
}
impl ShutdownWindow {
    pub fn new(deadline: i64) -> Arc<Self> {
        Arc::new(Self {
            deadline,
            verdict: SharedDeferred::new(),
        })
    }
    /// Latches `verdict` unless one already is; returns the one that stands.
    pub fn decide(&self, verdict: Termination) -> Termination {
        self.verdict.complete(Ok(verdict));
        self.decided()
            .unwrap_or_else(|| Termination::Unconfirmed(vec!["no verdict".into()]))
    }
    /// Latches `Unconfirmed` for an error while deciding, which [`Self::failure`] keeps for the disposal to report.
    pub fn fail(&self, error: VendorError) {
        self.verdict.complete(Err(error));
    }
    /// The error that deciding ended with, if it did.
    pub fn failure(&self) -> Option<VendorError> {
        self.verdict.completed()?.err()
    }
    pub fn decided(&self) -> Option<Termination> {
        match self.verdict.completed()? {
            Ok(verdict) => Some((*verdict).clone()),
            Err(error) => Some(Termination::Unconfirmed(vec![failure_text(error.failure())])),
        }
    }
    /// Waits for the verdict.
    pub async fn verdict(&self) -> Termination {
        match self.verdict.wait().await {
            Ok(verdict) => (*verdict).clone(),
            Err(error) => Termination::Unconfirmed(vec![failure_text(error.failure())]),
        }
    }
}
/// What a failure while confirming says, for an `Unconfirmed` verdict.
pub fn failure_text(failure: &Failure) -> String {
    format!(
        "confirming its end failed ({})",
        failure.message.as_deref().unwrap_or_else(|| failure.kind.description())
    )
}

/// D93: the part of a device's one shutdown window, at its end, given to forcing what has not ended (an xcodebuild
/// session whose producer still runs is stopped): 5 s of the default 70 s, in proportion for another window.
pub fn force_reserve_millis(window: i64) -> i64 {
    window.max(0) / 14
}

/// D93: a device's own handle on its worker's connection. Every command the device sends goes through it, so that a
/// device whose work could not be confirmed to have ended can be made to send nothing more, without touching the
/// connection, which a sibling may share (D90). The lease is the connection itself, which the provider releases or
/// quarantines.
/// What the device's provider does to the lease when the device is quarantined: its connection factory's quarantine.
pub type LeaseQuarantine = Arc<dyn Fn(&super::super::device::connection_factory::Executor) + Send + Sync>;
pub struct DeviceCommands {
    lease: super::super::device::connection_factory::Executor,
    revoked: Mutex<Option<Failure>>,
    /// The provider's quarantine for the lease, and whether it has been applied.
    quarantine: Mutex<(Option<LeaseQuarantine>, bool)>,
}
impl DeviceCommands {
    pub fn new(lease: super::super::device::connection_factory::Executor) -> Arc<Self> {
        Arc::new(Self {
            lease,
            revoked: Mutex::new(None),
            quarantine: Mutex::new((None, false)),
        })
    }
    /// D93: how the provider quarantines this device's lease. If the device was quarantined before this is given, it is
    /// applied at once.
    pub fn on_quarantine(&self, hook: LeaseQuarantine) {
        let apply = {
            let mut quarantine = self.quarantine.lock().unwrap_or_else(|e| e.into_inner());
            if quarantine.1 {
                Some(hook)
            } else {
                quarantine.0 = Some(hook);
                None
            }
        };
        if let Some(hook) = apply {
            hook(&self.lease);
        }
    }
    /// D93: the device's work was not confirmed to have ended: its commands are revoked with `why` and its lease is
    /// quarantined in its connection factory now, so that no other device is given that connection from here on. Once.
    pub fn quarantine(&self, why: Failure) {
        self.revoke(why);
        let hook = {
            let mut quarantine = self.quarantine.lock().unwrap_or_else(|e| e.into_inner());
            if quarantine.1 {
                return;
            }
            quarantine.1 = true;
            quarantine.0.take()
        };
        if let Some(hook) = hook {
            hook(&self.lease);
        }
    }
    pub fn quarantined(&self) -> bool {
        self.quarantine.lock().unwrap_or_else(|e| e.into_inner()).1
    }
    /// The connection this device holds a lease on.
    pub fn lease(&self) -> super::super::device::connection_factory::Executor {
        self.lease.clone()
    }
    /// From now on every command fails with `why`, before anything reaches the worker. The first reason stays.
    pub fn revoke(&self, why: Failure) {
        self.revoked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(why);
    }
    pub fn revoked(&self) -> bool {
        self.revoked.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}
impl CommandExecutor for DeviceCommands {
    fn host(&self) -> &CommandHost {
        self.lease.host()
    }
    fn component(&self) -> &Component {
        self.lease.component()
    }
    fn connected(&self) -> bool {
        self.lease.connected()
    }
    fn log_error(&self, message: String) {
        self.lease.log_error(message)
    }
    fn log_error_with(&self, message: String, cause: &Failure) {
        self.lease.log_error_with(message, cause)
    }
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        let revoked = self.revoked.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match revoked {
            // A stopped caller hears of its stop, as the executors tell it, rather than of the revocation.
            Some(_) if let Some(reason) = context.cancellation.reason() => {
                super::exec_outcome::record(true);
                Box::pin(async move { Err(VendorError::Cancelled(reason)) })
            }
            Some(why) => {
                super::exec_outcome::record(true);
                let mut refused = failure(
                    FailureKind::IllegalState,
                    format!(
                        "Not sending [{}]: the device's commands were revoked",
                        request.command.join(", ")
                    ),
                );
                refused.cause = Some(Box::new(why));
                Box::pin(async move { Err(VendorError::Other(refused)) })
            }
            None => self.lease.execute(request, context),
        }
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        self.lease.close()
    }
    fn shared_ssh_is_saturated(&self) -> bool {
        self.lease.shared_ssh_is_saturated()
    }
    fn hold_shared_ssh_slot(
        &self,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.lease.hold_shared_ssh_slot(slot)
    }
    fn connection_lost(&self) -> Pin<Box<dyn Future<Output = Failure> + Send>> {
        self.lease.connection_lost()
    }
}
