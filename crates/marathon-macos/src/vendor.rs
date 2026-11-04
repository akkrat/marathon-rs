//! Port of vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/MacosVendor.kt

use crate::{
    device::{CANCELLED_BATCH_TIMEOUT, MacosDeviceOptions},
    installer::MacosApplicationInstaller,
    provider::AppleMacosProvider,
};
use jcompat::{
    file_manager::{FileManager, JavaPath},
    writers::JavaString,
};
use marathon_apple::{
    apple_application_installer::AppleApplicationInstaller,
    apple_device_registry::AppleDeviceRegistry,
    apple_log_configurator::AppleLogConfigurator,
    apple_test_bundle_identifier::AppleTestBundleIdentifier,
    cmd::{failure, remote::ssh::sshj::config::SshLogging, shared_transfers::SharedTransfers},
    device::connection_factory::ConnectionFactory,
    model::Sdk,
    nm_test_parser::NmTestParser,
    xcodebuild_session::CLOSE_TIMEOUT,
    xctest_parser::{ParserLibraries, XCTestParser},
};
use marathon_config::{VendorConfiguration, vendor::AppleTestParserConfiguration};
use marathon_core::{
    error::{VendorError, VendorResult},
    logging::LogRecord,
    vendor::{TestParser, VendorServices},
    wiring::VendorDependencies,
};
use std::sync::Arc;

pub fn logging(dependencies: &VendorDependencies) -> SshLogging {
    let logs = dependencies.logs.clone();
    let wall = dependencies.wall.clone();
    SshLogging::new(
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
    )
}
/// Construction runs off core but does not connect, discover devices or configure logging.
pub async fn construct(dependencies: VendorDependencies) -> VendorResult<VendorServices> {
    construct_with_parser_libraries(dependencies, ParserLibraries::from_environment).await
}
/// `parser_libraries` runs only for the `xctest` parser, which fails here without its library (D20).
pub async fn construct_with_parser_libraries(
    dependencies: VendorDependencies,
    parser_libraries: impl FnOnce() -> ParserLibraries,
) -> VendorResult<VendorServices> {
    let VendorConfiguration::Macos(config) = &dependencies.configuration.vendor_configuration else {
        return Err(VendorError::Other(failure(
            "com.malinskiy.marathon.config.exceptions.ConfigurationException",
            "Expected macOS vendor configuration",
        )));
    };
    let config = Arc::new(config.clone());
    let libraries = match &config.test_parser_configuration {
        AppleTestParserConfiguration::Xctest => {
            let libraries = parser_libraries();
            libraries
                .check_at_construction(&[Sdk::Macos], &*dependencies.io)
                .await?;
            libraries
        }
        AppleTestParserConfiguration::Nm { .. } => ParserLibraries::default(),
    };
    let logging = logging(&dependencies);
    let cwd = std::env::current_dir().map_err(crate::io_error)?;
    let files = Arc::new(FileManager::new_utf16(
        dependencies.configuration.output_configuration.max_path,
        dependencies.configuration.output_configuration.max_filename,
        &dependencies.configuration.output_dir.0,
        &cwd.to_string_lossy().encode_utf16().collect::<Vec<_>>(),
    ));
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
    let registry = Arc::new(AppleDeviceRegistry::default());
    let identifier = Arc::new(AppleTestBundleIdentifier::default());
    let installer = Arc::new(MacosApplicationInstaller::default());
    let options = MacosDeviceOptions {
        dependencies: dependencies.clone(),
        vendor: config.clone(),
        logging: logging.clone(),
        files,
        shared_transfers: Arc::new(SharedTransfers::new(dependencies.io.clone(), logging.clone())),
        installer: installer.clone(),
        session_close_timeout: CLOSE_TIMEOUT,
        cancelled_batch_timeout: CANCELLED_BATCH_TIMEOUT,
    };
    let provider = AppleMacosProvider::new(
        options,
        registry.clone(),
        Arc::new(connections),
        JavaPath::from_text(&cwd.to_string_lossy()),
        std::env::vars_os()
            .map(|(k, v)| {
                (
                    JavaString::from(k.to_string_lossy().as_ref()),
                    JavaString::from(v.to_string_lossy().as_ref()),
                )
            })
            .collect(),
    );
    let configuration = Arc::new(VendorConfiguration::Macos((*config).clone()));
    let parser: Arc<dyn marathon_core::vendor::RemoteTestParser> = match &config.test_parser_configuration {
        AppleTestParserConfiguration::Xctest => Arc::new(XCTestParser::new(
            configuration.clone(),
            registry,
            identifier.clone(),
            AppleApplicationInstaller::new_owned((*configuration).clone(), dependencies.io.clone(), logging.clone())
                .with_owned_after_install(installer),
            dependencies.io.clone(),
            logging,
            libraries,
        )),
        AppleTestParserConfiguration::Nm { test_class_regexes } => Arc::new(NmTestParser::new(
            configuration,
            registry,
            identifier.clone(),
            test_class_regexes.clone(),
            dependencies.io.clone(),
            logging,
        )),
    };
    Ok(VendorServices {
        provider,
        parser: Some(TestParser::Remote(parser)),
        bundle_identifier: Some(identifier),
        log_configurator: Some(Arc::new(AppleLogConfigurator {
            compact_output: config.compact_output,
            runtime: dependencies.io,
        })),
    })
}
