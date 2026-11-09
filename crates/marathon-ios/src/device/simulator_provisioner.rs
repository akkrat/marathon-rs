//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/SimulatorProvisioner.kt

use super::{
    error::{ProvisioningError, ProvisioningFuture, ProvisioningResult, SetupError, clock_error},
    provisioning_plan::ProvisioningPlan,
    simulator_factory::{ConstructedSimulator, SimulatorCreating},
    timing::{profile_text, transport_text},
    worker_provisioning_context::WorkerProvisioningContext,
};
use futures_util::future::join_all;
use jcompat::logging::{Level, Logger};
use marathon_apple::{
    bin::AppleBinaryEnvironment,
    cmd::{failure, remote::ssh::sshj::config::SshLogging},
    device::{
        connection_factory::{Bridge, ConnectionFactory, ConnectionFuture, Executor},
        connection_failures::is_transient_connection_failure,
    },
};
use marathon_config::devices::{SimulatorProfile, Transport};
use marathon_core::{
    error::{Cancellation, VendorError},
    executor::shared::Signal,
    runtime::{CancellationSignal, VendorFuture},
    vendor::VendorContext,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

pub const DEFAULT_CONCURRENCY: i32 = 2;
pub trait ProvisioningConnections: Send + Sync {
    fn connect(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, (Option<Executor>, Bridge)>;
    fn connect_worker(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, Executor> {
        Box::pin(async move {
            self.connect(transport, caller).await?.0.ok_or_else(|| {
                VendorError::Other(failure("java.lang.IllegalStateException", "missing executor")).into()
            })
        })
    }
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()>;
}
impl ProvisioningConnections for Arc<ConnectionFactory> {
    fn connect(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, (Option<Executor>, Bridge)> {
        Box::pin(async move { self.create(transport, caller).await })
    }
    fn connect_worker(&self, transport: Transport, caller: VendorContext) -> ConnectionFuture<'_, Executor> {
        Box::pin(async move { ConnectionFactory::connect_worker(self, transport, caller).await })
    }
    fn release(&self, executor: Executor) -> VendorFuture<'_, ()> {
        Box::pin(async move { self.dispose(executor).await })
    }
}
/// Mirrors the source's mockable simctl service; production keeps wrapper parsing/timeouts.
pub trait SimulatorCommands: Send + Sync {
    fn create<'a>(
        &'a self,
        bin: &'a AppleBinaryEnvironment,
        name: &'a str,
        kind: &'a str,
        runtime: Option<&'a str>,
        caller: &'a VendorContext,
    ) -> ProvisioningFuture<'a, Option<String>>;
}
pub struct SimctlCommands;
impl SimulatorCommands for SimctlCommands {
    fn create<'a>(
        &'a self,
        bin: &'a AppleBinaryEnvironment,
        name: &'a str,
        kind: &'a str,
        runtime: Option<&'a str>,
        caller: &'a VendorContext,
    ) -> ProvisioningFuture<'a, Option<String>> {
        Box::pin(async move {
            bin.xcrun
                .simctl
                .simulator
                .create(name, kind, runtime, caller)
                .await
                .map_err(Into::into)
        })
    }
}
pub type Registration<H> =
    Arc<dyn Fn(String, ConstructedSimulator<H>) -> ProvisioningFuture<'static, ()> + Send + Sync>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailedSetup {
    Existing {
        udid: String,
        unreachable: bool,
        /// The simulator's own connection failed for a reason that a retry doesn't change, such as authentication.
        refused: bool,
    },
    Created {
        profile: SimulatorProfile,
        unreachable: bool,
        refused: bool,
    },
}
impl FailedSetup {
    fn with_unreachable(self, value: bool) -> Self {
        match self {
            Self::Existing { udid, refused, .. } => Self::Existing {
                udid,
                unreachable: value,
                refused,
            },
            Self::Created { profile, refused, .. } => Self::Created {
                profile,
                unreachable: value,
                refused,
            },
        }
    }
    fn refused(self) -> Self {
        match self {
            Self::Existing { udid, unreachable, .. } => Self::Existing {
                udid,
                unreachable,
                refused: true,
            },
            Self::Created {
                profile, unreachable, ..
            } => Self::Created {
                profile,
                unreachable,
                refused: true,
            },
        }
    }
}
#[derive(PartialEq, Eq)]
enum Outcome {
    SetUp,
    Unreachable,
    Failed,
    Dropped,
}
struct Combined {
    parent: Arc<dyn CancellationSignal>,
    siblings: Arc<Signal>,
}
impl CancellationSignal for Combined {
    fn reason(&self) -> Option<Cancellation> {
        self.parent.reason().or_else(|| self.siblings.reason())
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Cancellation> + Send + '_>> {
        Box::pin(async move {
            tokio::select! { biased; reason = self.parent.cancelled() => reason, reason = self.siblings.cancelled() => reason }
        })
    }
}
fn active(caller: &VendorContext) -> ProvisioningResult<()> {
    caller
        .cancellation
        .reason()
        .map_or(Ok(()), |reason| Err(VendorError::Cancelled(reason).into()))
}
fn protected(caller: &VendorContext) -> VendorContext {
    VendorContext {
        cancellation: Signal::new(),
        ..caller.clone()
    }
}
/// Vendor-local service: invoke inside call_vendor, never poll it on the core executor.
pub struct SimulatorProvisioner<H> {
    commands: Arc<dyn SimulatorCommands>,
    connections: Arc<dyn ProvisioningConnections>,
    factory: Arc<dyn SimulatorCreating<Handle = H>>,
    register: Registration<H>,
    concurrency: usize,
    permits: Mutex<HashMap<String, Arc<Semaphore>>>,
    logging: SshLogging,
    logger: Logger,
}
impl<H: Clone + Send + Sync + 'static> SimulatorProvisioner<H> {
    pub fn new(
        connections: Arc<dyn ProvisioningConnections>,
        factory: Arc<dyn SimulatorCreating<Handle = H>>,
        register: Registration<H>,
        concurrency: i32,
        logging: SshLogging,
    ) -> ProvisioningResult<Self> {
        if concurrency <= 0 {
            return Err(VendorError::Other(failure(
                "java.lang.IllegalArgumentException",
                format!("concurrency should be positive, got {concurrency}"),
            ))
            .into());
        }
        let logger = logging.logger("com.malinskiy.marathon.apple.ios.device.SimulatorProvisioner");
        Ok(Self {
            commands: Arc::new(SimctlCommands),
            connections,
            factory,
            register,
            concurrency: concurrency as usize,
            permits: Mutex::default(),
            logging,
            logger,
        })
    }
    pub fn with_simulator_commands(mut self, commands: Arc<dyn SimulatorCommands>) -> Self {
        self.commands = commands;
        self
    }
    fn log(&self, level: Level, message: impl FnOnce() -> String, error: Option<&ProvisioningError>) {
        if self.logging.enabled(&self.logger, level) {
            (self.logging.sink)(
                &self.logger,
                level,
                message(),
                error.map(ProvisioningError::output_failure),
            );
        }
    }
    pub async fn existing(
        &self,
        udids: impl IntoIterator<Item = String>,
        transport: Transport,
        caller: VendorContext,
    ) -> ProvisioningResult<Vec<FailedSetup>> {
        let cycle = Arc::new(WorkerProvisioningContext::new(transport.clone(), caller.hooks.clone()));
        self.run(
            ProvisioningPlan {
                existing_simulators: udids.into_iter().collect(),
                ..ProvisioningPlan::default()
            },
            transport,
            None,
            cycle,
            caller,
        )
        .await
    }
    pub async fn provision(
        &self,
        plan: ProvisioningPlan,
        transport: Transport,
        bin: Arc<AppleBinaryEnvironment>,
        cycle: Arc<WorkerProvisioningContext>,
        caller: VendorContext,
    ) -> ProvisioningResult<Vec<FailedSetup>> {
        self.run(plan, transport, Some(bin), cycle, caller).await
    }
    async fn run(
        &self,
        plan: ProvisioningPlan,
        transport: Transport,
        bin: Option<Arc<AppleBinaryEnvironment>>,
        cycle: Arc<WorkerProvisioningContext>,
        caller: VendorContext,
    ) -> ProvisioningResult<Vec<FailedSetup>> {
        let semaphore = self
            .permits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(transport.id())
            .or_insert_with(|| Arc::new(Semaphore::new(self.concurrency)))
            .clone();
        let siblings = Signal::new();
        let caller = VendorContext {
            cancellation: Arc::new(Combined {
                parent: caller.cancellation.clone(),
                siblings: siblings.clone(),
            }),
            ..caller
        };
        let operations = plan
            .existing_simulators
            .into_iter()
            .map(|udid| FailedSetup::Existing {
                udid,
                unreachable: false,
                refused: false,
            })
            .chain(plan.needs_provisioning.into_iter().map(|profile| FailedSetup::Created {
                profile,
                unreachable: false,
                refused: false,
            }));
        let results = join_all(operations.map(|failure| {
            let semaphore = semaphore.clone(); let caller = caller.clone(); let siblings = siblings.clone(); let cycle = cycle.clone(); let bin = bin.clone(); let transport = transport.clone();
            async move {
                let result: ProvisioningResult<Option<FailedSetup>> = async {
                    active(&caller)?;
                    let _permit = tokio::select! { biased;
                        reason = caller.cancellation.cancelled() => return Err(VendorError::Cancelled(reason).into()),
                        permit = semaphore.acquire() => permit.map_err(|e| VendorError::Other(marathon_apple::cmd::failure("java.lang.IllegalStateException", e.to_string())))?,
                    };
                    active(&caller)?;
                    match &failure {
                        FailedSetup::Existing { udid, .. } => {
                            let outcome = self.setup(udid, &transport, cycle, &caller).await?;
                            Ok(match outcome { Outcome::SetUp | Outcome::Dropped => None, Outcome::Unreachable => Some(failure.clone().with_unreachable(true)), Outcome::Failed => Some(failure.clone().refused()) })
                        },
                        FailedSetup::Created { profile, .. } => match bin {
                            Some(bin) => self.create(profile, &transport, &bin, cycle, &caller).await,
                            None => Err(VendorError::Other(marathon_apple::cmd::failure("java.lang.IllegalStateException", "profile provisioning requires binary environment")).into()),
                        },
                    }
                }.await;
                match result {
                    Err(error) if error.is_cancelled() => {
                        if let Some(reason) = error.cancellation() { siblings.cancel(reason); }
                        Err(error)
                    },
                    Err(error) => { self.log(Level::Error, || format!("Failed to set up simulator on {}", transport_text(&transport, "Local")), Some(&error)); Ok(Some(failure)) },
                    result => result,
                }
            }
        })).await;
        // Join every supervised child and its cleanup before propagating cancellation.
        let mut failed = Vec::new();
        let mut cancellation = None;
        for result in results {
            match result {
                Ok(Some(failure)) => failed.push(failure),
                Ok(None) => {}
                Err(error) => {
                    if cancellation.is_none() {
                        cancellation = Some(error);
                    }
                }
            }
        }
        cancellation.map_or(Ok(failed), Err)
    }
    async fn setup(
        &self,
        udid: &str,
        transport: &Transport,
        cycle: Arc<WorkerProvisioningContext>,
        caller: &VendorContext,
    ) -> ProvisioningResult<Outcome> {
        let (executor, bridge) = match self.connections.connect(transport.clone(), caller.clone()).await {
            Ok(connection) => connection,
            Err(error) => {
                let transient = is_transient_connection_failure(&error);
                let error: ProvisioningError = error.into();
                if error.is_cancelled() {
                    return Err(error);
                }
                self.log(
                    Level::Error,
                    || {
                        format!(
                            "Failed to connect to {} to set up simulator {udid}",
                            transport_text(transport, "Local")
                        )
                    },
                    Some(&error),
                );
                return Ok(if transient {
                    Outcome::Unreachable
                } else {
                    Outcome::Failed
                });
            }
        };
        let Some(executor) = executor else {
            return Ok(Outcome::Unreachable);
        };
        let mut simulator = None;
        let result: ProvisioningResult<Outcome> = async {
            active(caller)?;
            let created = self
                .factory
                .create(
                    transport.clone(),
                    executor.clone(),
                    bridge,
                    udid.into(),
                    cycle,
                    caller.clone(),
                )
                .await?;
            simulator = Some(created.clone());
            active(caller)?;
            let mut id: String = udid.into();
            id.push('@');
            id.push_str(&transport.id());
            (self.register)(id, created).await?;
            Ok(Outcome::SetUp)
        }
        .await;
        let mut result = match result {
            Err(error) if error.is_cancelled() => Err(error),
            Err(error) if matches!(error.original, SetupError::Dropped(_)) => {
                self.log(
                    Level::Info,
                    || error.failure().message.unwrap_or_default().to_owned(),
                    None,
                );
                Ok(Outcome::Dropped)
            }
            Err(error) if !executor.connected() => {
                self.log(
                    Level::Error,
                    || {
                        format!(
                            "Lost the connection to {} while setting up simulator {udid}",
                            transport_text(transport, "Local")
                        )
                    },
                    Some(&error),
                );
                Ok(Outcome::Unreachable)
            }
            result => result,
        };
        if !matches!(result, Ok(Outcome::SetUp)) {
            // Kotlin finally: dispose failure replaces the main result; release failure replaces either.
            if let Some(simulator) = simulator
                && let Err(error) = simulator.device.0.dispose().await
            {
                result = Err(error.into());
            }
            if let Err(error) = self.connections.release(executor).await {
                result = Err(error.into());
            }
        }
        result
    }
    async fn create(
        &self,
        profile: &SimulatorProfile,
        transport: &Transport,
        bin: &AppleBinaryEnvironment,
        cycle: Arc<WorkerProvisioningContext>,
        caller: &VendorContext,
    ) -> ProvisioningResult<Option<FailedSetup>> {
        let available = cycle.inventory(bin, caller).await?;
        let kind = profile.fully_qualified_device_type_id().to_owned();
        let runtime = profile.fully_qualified_runtime_id().map(|s| s.to_owned());
        if !available
            .devicetypes
            .iter()
            .any(|d| d.identifier.as_ref() == Some(&kind))
            || runtime
                .as_ref()
                .is_some_and(|r| !available.runtimes.iter().any(|d| d.identifier.as_ref() == Some(r)))
        {
            self.log(
                Level::Error,
                || {
                    format!(
                        "Simulator profile {} is not available at {}",
                        profile_text(profile),
                        transport_text(transport, "Local")
                    )
                },
                None,
            );
            return Ok(None);
        }
        let id = caller
            .hooks
            .allocate_id("ios.SimulatorProvisioner.create")
            .map_err(clock_error)?;
        let name = format!("{}-{}", profile.new_name_prefix.to_owned(), id.to_owned());
        let cleanup = protected(caller);
        let udid = self
            .commands
            .create(bin, &name, &kind, runtime.as_deref(), &cleanup)
            .await?;
        let mut registered = false;
        let result = async {
            active(caller)?;
            let Some(udid) = &udid else {
                self.log(
                    Level::Error,
                    || format!("Failed to create simulator for profile {}", profile_text(profile)),
                    None,
                );
                return Ok(Some(FailedSetup::Created {
                    profile: profile.clone(),
                    unreachable: false,
                    refused: false,
                }));
            };
            cycle.refresh(bin, caller).await?;
            let outcome = self.setup(udid, transport, cycle, caller).await?;
            registered = outcome == Outcome::SetUp;
            Ok(match outcome {
                Outcome::SetUp | Outcome::Dropped => None,
                Outcome::Failed | Outcome::Unreachable => Some(FailedSetup::Created {
                    profile: profile.clone(),
                    unreachable: outcome == Outcome::Unreachable,
                    refused: outcome == Outcome::Failed,
                }),
            })
        }
        .await;
        if !registered && let Some(udid) = udid {
            let deleted = bin.xcrun.simctl.simulator.delete(&udid, &cleanup).await;
            if !matches!(deleted, Ok(true)) {
                let error = deleted.err().map(ProvisioningError::from);
                self.log(
                    Level::Warn,
                    || {
                        format!(
                            "Failed to delete partially provisioned simulator {udid} on {}",
                            transport_text(transport, "Local")
                        )
                    },
                    error.as_ref(),
                );
            }
        }
        result
    }
}
