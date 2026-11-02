//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/IosVendor.kt

use crate::{
    apple_simulator_device::{AppleSimulatorConstructor, SimulatorServices},
    device::{
        error::ProvisioningResult,
        simulator_factory::{BinaryFactory, SimulatorFactory},
        simulator_provider::{AppleSimulatorProvider, ProviderRecovery, ProviderServices},
        simulator_provisioner::ProvisioningConnections,
        timing::ProvisioningTiming,
    },
};
use marathon_apple::{
    apple_application_installer::AppleApplicationInstaller,
    apple_device_registry::AppleDeviceRegistry,
    apple_log_configurator::AppleLogConfigurator,
    bin::{AppleBinaryEnvironment, Tools},
    cmd::shared_transfers::SharedTransfers,
    nm_test_parser::NmTestParser,
    xctest_parser::XCTestParser,
};
use marathon_config::{VendorConfiguration, binding::BindingContext, vendor::AppleTestParserConfiguration};
use marathon_core::vendor::{TestParser, VendorServices};
use std::sync::Arc;

/// All run-owned services are explicit. In particular the device constructor and both
/// parser branches share the identifier supplied in SimulatorServices.
pub struct IosVendor {
    pub simulator: SimulatorServices,
    pub binding: Arc<BindingContext>,
    pub connections: Arc<dyn ProvisioningConnections>,
    pub recovery: Arc<dyn ProviderRecovery>,
    pub shared_transfers: Arc<SharedTransfers>,
    pub registry: Arc<AppleDeviceRegistry>,
}
impl IosVendor {
    pub fn construct(self) -> ProvisioningResult<VendorServices> {
        let s = self.simulator;
        let services = s.clone();
        let binaries: BinaryFactory = Arc::new(move |executor| {
            Arc::new(AppleBinaryEnvironment::with_tools(
                Tools {
                    executor,
                    timeouts: Arc::new(services.vendor.timeout_configuration.clone()),
                    runtime: services.runtime.clone(),
                    logging: services.logging.clone(),
                },
                services.configuration.test_batch_timeout_millis,
                services.configuration.test_output_timeout_millis,
            ))
        });
        let timing = ProvisioningTiming::new(s.timer.clone(), s.logging.clone());
        let factory = Arc::new(SimulatorFactory::new(
            binaries.clone(),
            Arc::new(AppleSimulatorConstructor(s.clone())),
            self.shared_transfers,
            s.track.clone(),
            timing.clone(),
        ));
        let provider = AppleSimulatorProvider::new(ProviderServices {
            vendor: s.vendor.clone(),
            runtime: s.runtime.clone(),
            context: s.context.clone(),
            binding: self.binding,
            connections: self.connections,
            binaries,
            factory,
            artifacts: s.artifacts.clone(),
            registry: self.registry.clone(),
            timing,
            logging: s.logging.clone(),
            recovery: self.recovery,
        })?;
        let configuration = Arc::new(VendorConfiguration::Ios((*s.vendor).clone()));
        let parser = match &s.vendor.test_parser_configuration {
            AppleTestParserConfiguration::Xctest => TestParser::Remote(Arc::new(XCTestParser::new(
                configuration.clone(),
                self.registry,
                s.identifier.clone(),
                AppleApplicationInstaller::new_owned((*configuration).clone(), s.runtime.clone(), s.logging.clone()),
                s.runtime.clone(),
                s.logging.clone(),
            ))),
            AppleTestParserConfiguration::Nm { test_class_regexes } => TestParser::Remote(Arc::new(NmTestParser::new(
                configuration,
                self.registry,
                s.identifier.clone(),
                test_class_regexes.clone(),
                s.runtime.clone(),
                s.logging.clone(),
            ))),
        };
        Ok(VendorServices {
            provider,
            parser: Some(parser),
            bundle_identifier: Some(s.identifier),
            log_configurator: Some(Arc::new(AppleLogConfigurator {
                compact_output: s.vendor.compact_output,
                runtime: s.runtime,
            })),
        })
    }
}

/// Production construction performs no discovery or connection acquisition.
pub async fn construct(
    dependencies: marathon_core::wiring::VendorDependencies,
) -> marathon_core::error::VendorResult<VendorServices> {
    use jcompat::{
        file_manager::{FileManager, JavaPath},
        writers::JavaString,
    };
    use marathon_apple::{
        apple_test_bundle_identifier::AppleTestBundleIdentifier,
        artifact_processor::ArtifactProcessor,
        cmd::{failure, remote::ssh::sshj::config::SshLogging},
        device::connection_factory::ConnectionFactory,
    };
    use marathon_core::{
        analytics::Track, error::VendorError, executor::shared::Signal, logging::LogRecord, vendor::VendorContext,
    };
    let VendorConfiguration::Ios(config) = &dependencies.configuration.vendor_configuration else {
        return Err(VendorError::Other(failure(
            "com.malinskiy.marathon.config.exceptions.ConfigurationException",
            "Expected iOS vendor configuration",
        )));
    };
    let config = Arc::new(config.clone());
    let logs = dependencies.logs.clone();
    let wall = dependencies.wall.clone();
    let logging = SshLogging::new(
        dependencies.logging.clone(),
        Arc::new(move |logger, level, message, cause| {
            logs.push(LogRecord {
                logger: logger.name().into(),
                at_millis: 0,
                wall_millis: wall(),
                level,
                message: message.into(),
                cause,
            });
        }),
    );
    let cwd = std::env::current_dir().map_err(|e| VendorError::Other(failure("java.io.IOException", e.to_string())))?;
    let cwd = JavaPath::from_text(&cwd.to_string_lossy());
    let context = VendorContext {
        cancellation: Signal::new(),
        clock: dependencies.clock.clone(),
        hooks: dependencies.hooks.clone(),
    };
    let connections = ConnectionFactory::new(
        dependencies.configuration.debug,
        config.ssh.clone(),
        config.rsync.clone(),
        config.timeout_configuration.reachability,
        dependencies.io.clone(),
        dependencies.clock.clone(),
        logging.clone(),
    )
    .map_err(|e| e.into_vendor())?;
    let artifacts = ArtifactProcessor::from_configuration(
        &config.xcresult.queue,
        dependencies.io.clone(),
        dependencies.hooks.clone(),
        logging.clone(),
    )
    .map_err(|e| VendorError::Other(failure("java.lang.IllegalArgumentException", e.0.to_string_lossy())))?;
    let track = Track::new(dependencies.io.clone(), dependencies.hooks.clone());
    track.add(dependencies.tracker.clone());
    let files = Arc::new(FileManager::new_utf16(
        dependencies.configuration.output_configuration.max_path,
        dependencies.configuration.output_configuration.max_filename,
        &dependencies.configuration.output_dir.0,
        &cwd.0,
    ));
    IosVendor {
        simulator: SimulatorServices {
            configuration: dependencies.configuration,
            vendor: config,
            runtime: dependencies.io.clone(),
            context,
            monotonic: dependencies.hooks.clone(),
            wall: dependencies.hooks.clone(),
            timer: dependencies.timer,
            files,
            output: dependencies.files,
            logs: dependencies.logs,
            result_wall: dependencies.wall,
            listeners: Arc::new(crate::listener::ProductionSimulatorListenerFactory),
            identifier: Arc::new(AppleTestBundleIdentifier::default()),
            artifacts,
            track,
            logging: logging.clone(),
            timeouts: Default::default(),
        },
        binding: Arc::new(BindingContext {
            profile: marathon_config::binding::Profile::Marathondevices,
            marathonfile: cwd.clone(),
            marathonfile_dir: cwd.clone(),
            process_dir: cwd,
            environment: std::env::vars_os()
                .map(|(k, v)| {
                    (
                        JavaString::from(k.to_string_lossy().as_ref()),
                        JavaString::from(v.to_string_lossy().as_ref()),
                    )
                })
                .collect(),
            now: dependencies
                .hooks
                .instant("config.Configuration.now")
                .map_err(crate::device::error::clock_error)?,
            analytics_tracking: false,
            bugsnag_reporting: false,
        }),
        connections: Arc::new(crate::device::detached::ProviderConnections(connections)),
        recovery: crate::device::simulator_provider::recovery::SimulatorRecovery::new(),
        shared_transfers: Arc::new(SharedTransfers::new(dependencies.io, logging)),
        registry: Arc::new(AppleDeviceRegistry::default()),
    }
    .construct()
    .map_err(crate::device::error::ProvisioningError::into_vendor)
}
