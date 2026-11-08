//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/di/Modules.kt
//! and AndroidVendor.kt.
use crate::{
    device::AndroidBatchExecutor, dex_test_parser::DexTestParser, models::*, provider::AdamDeviceProvider, support::*,
};
use jcompat::{
    file_manager::JavaPath,
    logging::{Level, Logger, MarathonLogging, Vendor},
};
use marathon_config::{
    VendorConfiguration,
    vendor::{AndroidConfiguration, AndroidTestParserConfiguration, FileSyncConfiguration},
};
use marathon_core::{ServiceLog, error::*, runtime::*, vendor::*, wiring::VendorDependencies};
use std::sync::{Arc, Mutex};

pub struct AndroidRuntime {
    pub dependencies: VendorDependencies,
    pub configuration: AndroidConfiguration,
    pub file_sync: FileSyncConfiguration,
    pub bundles: Vec<Arc<AndroidTestBundle>>,
    pub identifier: Arc<AndroidTestBundleIdentifier>,
    pub executor: Arc<dyn AndroidBatchExecutor>,
    pub log: ServiceLog,
}
impl AndroidRuntime {
    pub async fn new(
        dependencies: VendorDependencies,
        executor: Arc<dyn AndroidBatchExecutor>,
    ) -> VendorResult<Arc<Self>> {
        let VendorConfiguration::Android(config) = &dependencies.configuration.vendor_configuration else {
            return Err(other(
                "java.lang.IllegalArgumentException",
                "Android configuration required",
            ));
        };
        let configuration = config.clone();
        let cwd = blocking(dependencies.io.as_ref(), || {
            let p = std::env::current_dir().map_err(io)?;
            Ok(JavaPath(p.to_string_lossy().encode_utf16().collect()))
        })
        .await?;
        let bundles = test_bundles_compat(&configuration)
            .into_iter()
            .map(|f| Arc::new(AndroidTestBundle::new(f, &cwd)))
            .collect();
        let file_sync = effective_file_sync(&configuration, dependencies.configuration.is_code_coverage_enabled);
        Ok(Arc::new(Self {
            log: dependencies.log.clone(),
            dependencies,
            configuration,
            file_sync,
            bundles,
            identifier: Arc::default(),
            executor,
        }))
    }
    pub fn logger(&self, name: &str) -> Logger {
        lock(&self.dependencies.logging).logger(name, None)
    }
    pub fn emit(&self, logger: &Logger, level: Level, message: &str, error: Option<&Failure>) {
        (self.log)(logger, level, &message, error)
    }
}
struct AndroidLogConfigurator {
    io: Arc<dyn VendorRuntime>,
}
impl LogConfigurator for AndroidLogConfigurator {
    fn configure(&self, debug: bool, logging: Arc<Mutex<MarathonLogging>>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            blocking(self.io.as_ref(), move || {
                lock(&logging).configure(debug, Vendor::Android);
                Ok(())
            })
            .await
        })
    }
}
pub async fn create_services(dependencies: VendorDependencies) -> VendorResult<VendorServices> {
    create_services_with_executor(dependencies, Arc::new(crate::runner::AndroidRunnerExecutor::default())).await
}
pub async fn create_services_with_executor(
    dependencies: VendorDependencies,
    executor: Arc<dyn AndroidBatchExecutor>,
) -> VendorResult<VendorServices> {
    let runtime = AndroidRuntime::new(dependencies, executor).await?;
    let identifier = runtime.identifier.clone();
    let cwd = blocking(runtime.dependencies.io.as_ref(), || {
        let p = std::env::current_dir().map_err(io)?;
        Ok(JavaPath(p.to_string_lossy().encode_utf16().collect()))
    })
    .await?;
    let provider = Arc::new(AdamDeviceProvider::new(runtime.clone())?);
    let parser = match runtime.configuration.test_parser_configuration {
        AndroidTestParserConfiguration::Local => TestParser::Local(Arc::new(DexTestParser::new(
            test_bundles_compat(&runtime.configuration),
            runtime.dependencies.io.clone(),
            Arc::new(move |files| Arc::new(AndroidTestBundle::new(files.clone(), &cwd))),
            Arc::new(move |test, bundle| identifier.put(test, bundle)),
        ))),
        AndroidTestParserConfiguration::Remote { .. } => {
            // Kotlin constructs the parser/logger before Marathon sets debug.
            runtime.logger("com.malinskiy.marathon.android.adam.AmInstrumentTestParser");
            TestParser::Remote(Arc::new(crate::remote_parser::AmInstrumentTestParser {
                runtime: runtime.clone(),
                provider: provider.clone(),
            }))
        }
    };
    Ok(VendorServices {
        provider,
        parser: Some(parser),
        bundle_identifier: Some(runtime.identifier.clone()),
        log_configurator: Some(Arc::new(AndroidLogConfigurator {
            io: runtime.dependencies.io.clone(),
        })),
    })
}
