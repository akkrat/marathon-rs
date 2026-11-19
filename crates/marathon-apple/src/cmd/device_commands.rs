//! D93: one Apple device's use of its worker's connection, and how its stopping work ended; Rust-only lifecycle support
//! for the ports of AppleSimulatorDevice.kt and MacosDevice.kt. Kotlin's disposal waits again for a batch whose cancelled
//! wait ran out and then runs the on-dispose lifecycle (or releases the Mac's connection) whatever that wait found.

use super::{CommandExecutor, CommandHost, CommandRequest, CommandSession, failure};
use marathon_core::{
    error::{Cancellation, Failure, VendorError},
    executor::shared::SharedDeferred,
    runtime::{CancellationSignal, VendorFuture},
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use marathon_logging::Component;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
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
/// D93 (round 7): why a device was sealed, which alone decides what a refusal after it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealCause {
    /// The run is stopping (the Scheduler's stop, a failure, the owner's cancel, a signal): a refusal is a cancellation,
    /// orderly, with no retry and no change to the run's outcome.
    RunStop,
    /// The device is disposed of outside a run's stop (removed from the devices file, recovered, replaced): a refusal is a
    /// device loss, so discovery takes another device (D53) and a batch is retried as for a lost device.
    Disposal,
    /// D93 (round 8): the provider terminates at the run's end: a refusal is a cancellation, orderly, as at a run's stop;
    /// unlike a run's stop it does not bound the disposal (owner's decision: disposal outside a run's stop keeps its limits).
    Termination,
}
impl SealCause {
    /// The error a refusal of this cause is, with `why` as its failure.
    /// The one place a refusal is built (round 8): every admission, gate, session, send and pull refusal uses it.
    pub fn refusal(self, why: Failure) -> VendorError {
        match self {
            Self::RunStop | Self::Termination => VendorError::Cancelled(Cancellation::Requested(why)),
            // As a batch on a disposed device is lost: a device loss whose cause is why.
            Self::Disposal => {
                let mut lost = failure(FailureKind::DeviceLost, why.to_string());
                lost.cause = Some(Box::new(why));
                VendorError::DeviceLost(lost)
            }
        }
    }
    /// D93 (round 8): whether this cause closes the device's pulls too. A run's stop and the provider's termination leave
    /// them open for the orderly artifact drain; a disposal outside them closes them.
    pub fn closes_pulls(self) -> bool {
        self == Self::Disposal
    }
    /// D93 (round 9): whether a disposal under this cause stops a pull still running. A run's stop and the termination
    /// dispose of an iOS simulator only after the provider's artifact drain (300 s, 60 s grace), so a pull left then is
    /// past it; a disposal outside them (a removal mid-run) leaves the artifact work alive, as Kotlin does. Either way the
    /// device is quarantined (`settle_pulls`).
    pub fn stops_pulls(self) -> bool {
        self != Self::Disposal
    }
}

tokio::task_local! {
    /// D93: work that passes a sealed device's gate: its own on-dispose lifecycle, and the batch it ran when it was
    /// sealed (its cleanup and result collection, which the seal's receipt waits for).
    static GATE_PASS: ();
    /// D93 (round 10): the admission of the command the current task is executing through its device, read by the
    /// executor in the caller's task and carried to where the command is handed to its transport.
    static ADMISSION: Arc<CommandAdmission>;
}
/// D93 (round 10): the admission of the command being executed through a device by the current task, if any.
pub(crate) fn admission() -> Option<Arc<CommandAdmission>> {
    ADMISSION.try_with(Arc::clone).ok()
}
/// D93 (round 10): a command's admission to its device's transport, taken by the executor at the moment it hands the
/// command over (the SSH channel's writer, as it writes the exec request; the local executor, just before the process
/// starts). The gate's contract: nothing is handed to the transport after the seal (unless the task holds a pass), and
/// everything handed before it is registered with the device in the same step, under the gate's lock, so that the
/// device's verdict waits for it to end.
pub struct CommandAdmission {
    gate: Weak<DeviceCommands>,
    /// The caller held a pass (the lifecycle, the registered batch) when it asked.
    pass: bool,
}
impl CommandAdmission {
    /// Admits the command, registering it with the device (the receipt the executor ends once the command has ended), or
    /// refuses it with the gate's cancellation.
    pub(crate) fn hand_over(&self) -> Result<Option<Arc<super::TransferReceipt>>, Cancellation> {
        let Some(gate) = self.gate.upgrade() else {
            return Ok(None);
        };
        let sealed = gate.sealed.lock().unwrap_or_else(|e| e.into_inner());
        let cause = sealed.as_ref().map_or(SealCause::Disposal, |(cause, _)| *cause);
        if let Some(why) = gate.revoked.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Err(gate_cancellation(cause.refusal(why).failure().clone()));
        }
        if let Some((cause, why)) = sealed.as_ref()
            && !self.pass
        {
            return Err(gate_cancellation(cause.refusal(why.clone()).failure().clone()));
        }
        let receipt = super::TransferReceipt::new();
        let mut handed = gate.handed.lock().unwrap_or_else(|e| e.into_inner());
        handed.retain(|r| r.ended() != Some(super::TransferCleanup::Confirmed));
        handed.push(receipt.clone());
        Ok(Some(receipt))
    }
}
/// D93: runs `body` as the device's on-dispose lifecycle, whose commands pass the sealed gate (not a revoked one).
pub async fn as_lifecycle<T>(body: impl Future<Output = T>) -> T {
    GATE_PASS.scope((), body).await
}
/// D93: runs `body` as a batch registered on the device, whose own commands pass the gate once the device is sealed, so
/// that its cleanup and results are not cut off. Work it spawns (a session's producer, a listener's task) runs in tasks
/// of its own and does not pass: a session sealed before it opens never opens.
pub async fn as_registered_batch<T>(body: impl Future<Output = T>) -> T {
    GATE_PASS.scope((), body).await
}

/// What the device's provider does to the lease when the device is quarantined: its connection factory's quarantine.
pub type LeaseQuarantine = Arc<dyn Fn(&super::super::device::connection_factory::Executor) + Send + Sync>;
pub struct DeviceCommands {
    me: Weak<Self>,
    lease: super::super::device::connection_factory::Executor,
    revoked: Mutex<Option<Failure>>,
    /// Told when the gate closes (a seal, a revocation), for the signals that carry it into executors and bridges.
    changed: tokio::sync::Notify,
    /// D93 (round 7): the gate, closed by the device's seal with its cause; checked as each command executes.
    sealed: Mutex<Option<(SealCause, Failure)>>,
    /// The provider's quarantine for the lease, and whether it has been applied.
    quarantine: Mutex<(Option<LeaseQuarantine>, bool)>,
    /// D93 (round 10): the commands handed to the transport and not confirmed to have ended (`CommandAdmission`).
    handed: Mutex<Vec<Arc<super::TransferReceipt>>>,
    /// D93 (round 10): closed by the disposal as it takes its look at the device's pulls, whatever the seal's cause.
    pulls_closed: Mutex<Option<Failure>>,
}
impl DeviceCommands {
    pub fn new(lease: super::super::device::connection_factory::Executor) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            lease,
            revoked: Mutex::new(None),
            changed: tokio::sync::Notify::new(),
            sealed: Mutex::new(None),
            quarantine: Mutex::new((None, false)),
            handed: Mutex::new(Vec::new()),
            pulls_closed: Mutex::new(None),
        })
    }
    /// D93 (round 7): closes the gate, once: from now on a command of this device is refused as `cause` says, with `why`,
    /// before it reaches the worker, except the device's own on-dispose lifecycle (`as_lifecycle`). The device calls it
    /// under the lock its seal is recorded under.
    pub fn seal(&self, cause: SealCause, why: Failure) {
        self.sealed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert((cause, why));
        self.changed.notify_waiters();
    }
    /// D93 (round 8): why a command of this device is refused now, if it is: revoked (a quarantine) or sealed, unless the
    /// current task holds a pass (the lifecycle, the registered batch) and the commands are not revoked. Built by the
    /// seal's cause.
    fn command_refusal(&self) -> Option<VendorError> {
        let sealed = self.sealed();
        let cause = sealed.as_ref().map_or(SealCause::Disposal, |(cause, _)| *cause);
        if let Some(why) = self.revoked.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Some(cause.refusal(why));
        }
        match sealed {
            Some((cause, why)) if GATE_PASS.try_with(|_| ()).is_err() => Some(cause.refusal(why)),
            _ => None,
        }
    }
    /// D93 (round 8): why a pull from this device is refused now, if it is: a quarantine, a seal whose cause closes
    /// pulls (a disposal outside a run's stop), or (round 10) the disposal's look at its pulls.
    fn pull_refusal(&self) -> Option<Failure> {
        if let Some(why) = self.revoked.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Some(why);
        }
        if let Some(why) = self.pulls_closed.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Some(why);
        }
        self.sealed()
            .filter(|(cause, _)| cause.closes_pulls())
            .map(|(_, why)| why)
    }
    /// D93 (round 8): `parent`, which is also cancelled once this device's gate closes for commands: the context an
    /// executor gets, so that it can look at the gate again at the last moment before it sends a command (after its
    /// channel is accepted, inside its spawn). The cancellation it reports for the gate is recognised by
    /// [`Self::gate_cancellation`].
    fn command_signal(&self, parent: Arc<dyn CancellationSignal>) -> Arc<dyn CancellationSignal> {
        Arc::new(GateSignal {
            gate: self.me.clone(),
            parent,
            pulls: false,
        })
    }
    /// D93 (round 8): a signal cancelled once this device's pulls are refused, for a receive's stop.
    pub fn pull_signal(&self) -> Arc<dyn CancellationSignal> {
        Arc::new(GateSignal {
            gate: self.me.clone(),
            parent: Arc::new(super::SessionScope::default()),
            pulls: true,
        })
    }
    /// D93 (round 8): whether `reason` is this device's gate refusing, rather than its caller's own cancellation.
    pub fn is_gate_cancellation(&self, reason: &Cancellation) -> bool {
        reason.failure().kind == FailureKind::IllegalState
            && reason
                .failure()
                .message
                .as_deref()
                .is_some_and(|message| message.starts_with(GATE_PREFIX))
    }
    /// D93 (round 10): the commands this device handed to its transport that are not confirmed to have ended: still
    /// running, or ended without that being confirmed. The device's verdict waits for them.
    pub fn handed_commands(&self) -> Vec<Arc<super::TransferReceipt>> {
        self.handed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|r| r.ended() != Some(super::TransferCleanup::Confirmed))
            .cloned()
            .collect()
    }
    /// D93 (round 10): closes the device's pulls with `why`, whatever its seal's cause: the disposal calls it under the
    /// lock its pulls register under, in the same step as it takes its look at them, so that a pull is either in that
    /// look or refused as it starts. Once; the first reason stays.
    pub fn close_pulls(&self, why: Failure) {
        self.pulls_closed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(why);
        self.changed.notify_waiters();
    }
    /// D93 (round 10): whether every command this device handed to its transport has ended (confirmed or not).
    pub fn handed_ended(&self) -> bool {
        self.handed_commands().iter().all(|c| c.ended().is_some())
    }
    /// D93 (round 10): waits for the commands this device handed to its transport to end, looking again once they have (a
    /// registered batch's command passes the sealed gate and counts too); `signal` ends the wait, not the commands.
    pub async fn wait_handed(&self, signal: Arc<dyn CancellationSignal>) -> marathon_core::error::VendorResult<()> {
        loop {
            let live: Vec<_> = self
                .handed_commands()
                .into_iter()
                .filter(|c| c.ended().is_none())
                .collect();
            if live.is_empty() {
                return Ok(());
            }
            for command in live {
                if let Err(VendorError::Cancelled(reason)) = command.wait_in(signal.clone()).await
                    && command.ended().is_none()
                {
                    return Err(VendorError::Cancelled(reason));
                }
            }
        }
    }
    /// D93 (round 10): what keeps the device's handed commands from being confirmed ended, for its verdict.
    pub fn handed_left(&self) -> Vec<String> {
        self.handed_commands()
            .iter()
            .filter_map(|c| match c.ended() {
                None => Some("a command it handed to the worker has not ended".to_owned()),
                Some(super::TransferCleanup::Unconfirmed(why)) => Some(format!(
                    "a command's end was not confirmed ({})",
                    why.message.as_deref().unwrap_or_default()
                )),
                Some(super::TransferCleanup::Confirmed) => None,
            })
            .collect()
    }
    /// The seal's cause and reason, once sealed.
    pub fn sealed(&self) -> Option<(SealCause, Failure)> {
        self.sealed.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
        self.changed.notify_waiters();
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
        // D93 (round 8): refused at once if the gate is closed; otherwise the executor gets a context that carries the gate,
        // and looks at it again at the last moment before it sends the command.
        if let Some(refusal) = self.command_refusal() {
            super::exec_outcome::record(true);
            return Box::pin(async move { Err(refusal) });
        }
        let gated = if GATE_PASS.try_with(|_| ()).is_ok() {
            context
        } else {
            VendorContext {
                cancellation: self.command_signal(context.cancellation.clone()),
                ..context
            }
        };
        let admission = Arc::new(CommandAdmission {
            gate: self.me.clone(),
            pass: GATE_PASS.try_with(|_| ()).is_ok(),
        });
        let executing = self.lease.execute(request, gated);
        Box::pin(async move {
            match ADMISSION.scope(admission, executing).await {
                Err(VendorError::Cancelled(reason)) if self.is_gate_cancellation(&reason) => {
                    // Refused before the command was sent: it started nothing.
                    super::exec_outcome::record(true);
                    Err(self.command_refusal().unwrap_or(VendorError::Cancelled(reason)))
                }
                other => other,
            }
        })
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

/// The prefix of the cancellation a closed gate reports through [`GateSignal`].
const GATE_PREFIX: &str = "The device's command gate is closed";
/// D93 (round 8): a caller's cancellation, also cancelled once a device's gate closes (for commands, or for pulls).
struct GateSignal {
    gate: Weak<DeviceCommands>,
    parent: Arc<dyn CancellationSignal>,
    pulls: bool,
}
impl GateSignal {
    fn closed(&self) -> Option<Cancellation> {
        let gate = self.gate.upgrade()?;
        let why = if self.pulls {
            gate.pull_refusal()?
        } else {
            gate.command_refusal()?.failure().clone()
        };
        Some(gate_cancellation(why))
    }
}
/// The cancellation a closed gate reports, with `why` as its cause ([`DeviceCommands::is_gate_cancellation`]).
fn gate_cancellation(why: Failure) -> Cancellation {
    let mut reason = failure(
        FailureKind::IllegalState,
        format!("{GATE_PREFIX}: {}", why.message.as_deref().unwrap_or_default()),
    );
    reason.cause = Some(Box::new(why));
    Cancellation::Requested(reason)
}
impl CancellationSignal for GateSignal {
    fn reason(&self) -> Option<Cancellation> {
        self.closed().or_else(|| self.parent.reason())
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = Cancellation> + Send + '_>> {
        Box::pin(async move {
            let Some(gate) = self.gate.upgrade() else {
                return self.parent.cancelled().await;
            };
            loop {
                let changed = gate.changed.notified();
                if let Some(reason) = self.closed() {
                    return reason;
                }
                tokio::select! { biased;
                    reason = self.parent.cancelled() => return self.closed().unwrap_or(reason),
                    _ = changed => {}
                }
            }
        })
    }
}

/// D93 (round 9): a disposal's look at the pulls registered with its device, before its on-dispose lifecycle or the
/// release of its lease. Returns why the device's end is not confirmed: a pull still running, or one whose cleanup after
/// a stop was not confirmed (now or earlier in the run). Empty: the lifecycle and the release may run. When `stop`, a pull
/// still running is stopped and so ends on D91's path for a stopped transfer, within its 40 s bound (killed and reaped, or
/// unconfirmed); it is not waited for, since the device is quarantined either way.
pub fn settle_pulls(pulls: &[Arc<super::TransferReceipt>], serial: &str, stop: bool) -> Vec<String> {
    pulls
        .iter()
        .filter_map(|pull| match pull.ended() {
            None => {
                if stop {
                    pull.stop(
                        Cancellation::Requested(failure(
                            FailureKind::IllegalState,
                            format!("Device {serial} is being disposed while a pull from it runs"),
                        )),
                        None,
                    );
                }
                Some("a pull from it was still running".to_owned())
            }
            Some(super::TransferCleanup::Unconfirmed(why)) => Some(format!(
                "a pull's cleanup was not confirmed ({})",
                why.message.as_deref().unwrap_or_default()
            )),
            Some(super::TransferCleanup::Confirmed) => None,
        })
        .collect()
}
