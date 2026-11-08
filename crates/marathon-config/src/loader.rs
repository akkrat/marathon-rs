//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/LogicalConfigurationValidator.kt,
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/ConfigurationValidator.kt and
//! configuration/src/main/kotlin/com/malinskiy/marathon/config/exceptions/ConfigurationException.kt.
//!
//! The only implementations of the 0.6-frozen loader/hooks/validator traits
//! (WP 2.4): stage-(a) assembly (`bind_bytes` → vendor post-processing →
//! tracking merge), the P2 `devices` entry, and stage-(b) logical
//! validation. All work is blocking (binding worker join, bundle
//! extraction, `canonicalFile`); callers dispatch it off the core executor
//! (docs/design/lifecycle.md). The trait futures run the synchronous cores
//! on first poll and hold nothing across an await.

use crate::{
    AnalyticsConfiguration, Configuration, ExecutionMode, FlakinessStrategyConfiguration, MarathonDevices,
    RetryStrategyConfiguration, ShardingStrategyConfiguration, VendorConfiguration,
    bind::bind_configuration,
    bind_vendor::bind_vendor_configuration,
    binding::{
        BindingCause, BindingContext, BindingFuture, ConfigurationError, ConfigurationLoader, ConfigurationValidator,
        Profile, ValidationStage, VendorConfigurationHooks,
    },
    pipeline::{LoadError, bind_bytes, bind_devices_bytes},
    vendor::{
        AppleThreadingConfiguration, CONFIGURATION_EXCEPTION_CLASS, ValidationError, XcresultQueueConfiguration,
        fill_android_sdk, post_process_ios_with_bundle_validation, post_process_macos_with_bundle_validation,
    },
};

/// The one `ValidationError` → `ConfigurationError` conversion (WP 2.4
/// deliverable 2): FQCN class, verbatim message, no wrapper, empty
/// path/span (P4: post-binding errors have no node context).
pub fn validation_error_to_configuration_error(error: ValidationError, stage: ValidationStage) -> ConfigurationError {
    let message: String = error.message.clone();
    ConfigurationError {
        stage,
        class: error.fqcn().into(),
        message: Some(message.clone()),
        causes: Vec::new(),
        path: Vec::new(),
        span: None,
        cause: BindingCause::Custom(message),
    }
}

/// A plain stage error with no node context (P4).
fn stage_error(stage: ValidationStage, class: &str, message: Option<String>) -> Box<ConfigurationError> {
    let message = message;
    Box::new(ConfigurationError {
        stage,
        class: class.into(),
        message: message.clone(),
        causes: Vec::new(),
        path: Vec::new(),
        span: None,
        cause: BindingCause::Custom(message.unwrap_or_default()),
    })
}
fn boxed_validation_error(error: ValidationError, stage: ValidationStage) -> Box<ConfigurationError> {
    Box::new(validation_error_to_configuration_error(error, stage))
}

/// The Apple provider that reads a Marathondevices file: the null-document
/// NPE names the dereference site, which differs between the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevicesProvider {
    /// `AppleSimulatorProvider.kt:165-170` stores the result in the local
    /// `marathonfile` before `marathonfile.workers` (bytecode LVT slot 5).
    Ios,
    /// `AppleMacosProvider.kt:86` calls `.workers` on `readValue`'s return.
    Macos,
}

/// `LoadError` at the trait edge (WP 2.4 orchestrator decision 3) for the
/// Marathonfile: substitution, null-document and worker failures keep the
/// classes below at stage `Parse`.
pub fn load_error_to_configuration_error(error: LoadError) -> Box<ConfigurationError> {
    match error {
        LoadError::Configuration(error) => error,
        LoadError::Substitution(error) => {
            stage_error(ValidationStage::Parse, error.java_class(), Some(error.message.clone()))
        }
        // Kotlin dereferences the null configuration outside the
        // `JsonProcessingException` catch (`ConfigurationFactory.kt:58`), so
        // the NPE escapes the `ConfigurationException` wrapper (F8), with
        // the JDK's helpful message (JVM witness `a-null-document`).
        LoadError::NullDocument => stage_error(
            ValidationStage::Parse,
            "java.lang.NullPointerException",
            Some(
                "Cannot invoke \"com.malinskiy.marathon.config.Configuration.getVendorConfiguration()\" because \"configuration\" is null"
                    .to_owned(),
            ),
        ),
        // No Kotlin counterpart: the binding worker could not be created,
        // so no binding took place (docs/wp/2.4-findings.md).
        LoadError::Worker(error) => stage_error(
            ValidationStage::Parse,
            "java.lang.IllegalStateException",
            Some(error.to_string()),
        ),
    }
}

/// `LoadError` at the trait edge for a Marathondevices file (P2): binding
/// failures stay at stage `Provider` (set by `bind_devices_bytes`). A null
/// document is not the P1 NPE: the provider's `readValue` returns null and
/// the NPE comes from its `.workers` dereference, outside the
/// `JsonMappingException` catch (JVM witnesses: WP 2.4 review round 1,
/// `tools/oracle/null-devices-probe`).
pub fn devices_load_error_to_configuration_error(
    error: LoadError,
    provider: DevicesProvider,
) -> Box<ConfigurationError> {
    match error {
        LoadError::NullDocument => {
            let because = match provider {
                DevicesProvider::Ios => "\"marathonfile\" is null",
                DevicesProvider::Macos => {
                    "the return value of \"com.fasterxml.jackson.databind.ObjectMapper.readValue(String, com.fasterxml.jackson.core.type.TypeReference)\" is null"
                }
            };
            stage_error(
                ValidationStage::Provider,
                "java.lang.NullPointerException",
                Some(format!(
                    "Cannot invoke \"com.malinskiy.marathon.apple.configuration.Marathondevices.getWorkers()\" because {because}"
                )),
            )
        }
        other => load_error_to_configuration_error(other),
    }
}

// ---- Vendor hooks -----------------------------------------------------------

fn post_process_sync(
    config: &mut VendorConfiguration,
    context: &BindingContext,
) -> Result<(), Box<ConfigurationError>> {
    match config {
        VendorConfiguration::Android(android) => fill_android_sdk(android, context),
        // The real `bundle.validate()` runs at step 2 of the factory branch,
        // before any `canonicalFile` (WP 2.3a "Factory within-branch order").
        VendorConfiguration::Ios(ios) => post_process_ios_with_bundle_validation(ios, context, |bundle| {
            bundle
                .validate()
                .map_err(|error| boxed_validation_error(error, ValidationStage::Parse))
        }),
        VendorConfiguration::Macos(macos) => post_process_macos_with_bundle_validation(macos, context, |bundle| {
            bundle
                .validate()
                .map_err(|error| boxed_validation_error(error, ValidationStage::Parse))
        }),
        VendorConfiguration::Stub => Ok(()),
        // `ConfigurationFactory.kt:190`, unwrapped (outside the JSON catch).
        VendorConfiguration::Empty => Err(stage_error(
            ValidationStage::Parse,
            CONFIGURATION_EXCEPTION_CLASS,
            Some("No vendor configuration specified".to_owned()),
        )),
    }
}

fn validate_vendor_sync(config: &VendorConfiguration) -> Result<(), Box<ConfigurationError>> {
    // F6: macOS `validate()` is deliberately not called at stage (b); its
    // queue/provisioning restrictions live in the logical validator.
    let result = match config {
        VendorConfiguration::Ios(ios) => ios.validate(),
        VendorConfiguration::Android(android) => android.validate(),
        VendorConfiguration::Macos(_) | VendorConfiguration::Stub | VendorConfiguration::Empty => Ok(()),
    };
    result.map_err(|error| boxed_validation_error(error, ValidationStage::Logical))
}

/// The `VendorConfigurationHooks` impl: stage-(a) post-processing and the
/// vendor half of stage (b).
#[derive(Clone, Copy, Debug, Default)]
pub struct MarathonVendorHooks;

impl MarathonVendorHooks {
    pub fn post_process_sync(
        &self,
        config: &mut VendorConfiguration,
        context: &BindingContext,
    ) -> Result<(), Box<ConfigurationError>> {
        post_process_sync(config, context)
    }
    pub fn validate_sync(&self, config: &VendorConfiguration) -> Result<(), Box<ConfigurationError>> {
        validate_vendor_sync(config)
    }
}

impl VendorConfigurationHooks for MarathonVendorHooks {
    fn post_process<'a>(
        &'a self,
        config: &'a mut VendorConfiguration,
        context: &'a BindingContext,
    ) -> BindingFuture<'a, ()> {
        Box::pin(async move { post_process_sync(config, context).map_err(|e| *e) })
    }
    fn validate<'a>(&'a self, config: &'a VendorConfiguration) -> BindingFuture<'a, ()> {
        Box::pin(async move { validate_vendor_sync(config).map_err(|e| *e) })
    }
}

// ---- Loader -----------------------------------------------------------------

/// Field-wise P2 context for `devices` (orchestrator decision 2): never
/// trust a caller's profile, because a P1 context silently re-enables
/// Marathonfile-relative `File` canonicalisation (F4).
fn devices_context(context: &BindingContext) -> BindingContext {
    BindingContext {
        profile: Profile::Marathondevices,
        marathonfile: context.marathonfile.clone(),
        analytics_tracking: context.analytics_tracking,
        bugsnag_reporting: context.bugsnag_reporting,
        marathonfile_dir: context.marathonfile_dir.clone(),
        process_dir: context.process_dir.clone(),
        environment: context.environment.clone(),
        now: context.now,
    }
}

fn parse_sync(bytes: &[u8], context: &BindingContext) -> Result<Configuration, Box<ConfigurationError>> {
    let mut configuration = bind_bytes(bytes, context, |value| {
        bind_configuration(value, bind_vendor_configuration)
    })
    .map_err(load_error_to_configuration_error)?
    .ok_or_else(|| load_error_to_configuration_error(LoadError::NullDocument))?;
    if !configuration.is_code_coverage_enabled {
        let bundle = match &mut configuration.vendor_configuration {
            VendorConfiguration::Ios(c) => c.bundle.as_mut(),
            VendorConfiguration::Macos(c) => c.bundle.as_mut(),
            _ => None,
        };
        if let Some(bundle) = bundle {
            bundle.xctestrun = None;
        }
    }
    post_process_sync(&mut configuration.vendor_configuration, context)?;
    // `ConfigurationFactory.kt:192-199`: false in either source disables.
    configuration.analytics_tracking = context.analytics_tracking && configuration.analytics_tracking;
    configuration.bugsnag_reporting = context.bugsnag_reporting && configuration.bugsnag_reporting;
    Ok(configuration)
}

fn devices_sync(
    bytes: &[u8],
    context: &BindingContext,
    provider: DevicesProvider,
) -> Result<MarathonDevices, Box<ConfigurationError>> {
    let context = devices_context(context);
    bind_devices_bytes(bytes, &context).map_err(|error| devices_load_error_to_configuration_error(error, provider))
}

/// The `ConfigurationLoader` impl: the assembled stage (a) over
/// `pipeline::bind_bytes`, plus the P2 `devices` entry. The 0.6 trait's
/// `devices` does not name the provider, so it reports a null document as
/// iOS does; the macOS provider calls `devices_sync_for`.
#[derive(Clone, Copy, Debug, Default)]
pub struct MarathonConfigurationLoader;

impl MarathonConfigurationLoader {
    pub fn parse_sync(&self, bytes: &[u8], context: &BindingContext) -> Result<Configuration, Box<ConfigurationError>> {
        parse_sync(bytes, context)
    }
    pub fn devices_sync(
        &self,
        bytes: &[u8],
        context: &BindingContext,
    ) -> Result<MarathonDevices, Box<ConfigurationError>> {
        devices_sync(bytes, context, DevicesProvider::Ios)
    }
    pub fn devices_sync_for(
        &self,
        provider: DevicesProvider,
        bytes: &[u8],
        context: &BindingContext,
    ) -> Result<MarathonDevices, Box<ConfigurationError>> {
        devices_sync(bytes, context, provider)
    }
}

impl ConfigurationLoader for MarathonConfigurationLoader {
    fn parse<'a>(&'a self, bytes: &'a [u8], context: &'a BindingContext) -> BindingFuture<'a, Configuration> {
        Box::pin(async move { parse_sync(bytes, context).map_err(|e| *e) })
    }
    fn devices<'a>(&'a self, bytes: &'a [u8], context: &'a BindingContext) -> BindingFuture<'a, MarathonDevices> {
        Box::pin(async move { devices_sync(bytes, context, DevicesProvider::Ios).map_err(|e| *e) })
    }
}

// ---- D47: the uncompleted retry loop must have a limit --------------------------

/// Where the error message sends the reader.
pub const FINITE_RETRIES_MIGRATION_URL: &str =
    "https://github.com/akkrat/marathon-rs/blob/main/docs/migration.md#a-finite-uncompletedtestretryquota-d47";

/// D47: refuse a configuration in which nothing bounds a test that keeps coming back uncompleted.
///
/// No Kotlin counterpart. Only the quota ends that loop on every path, on every vendor (the table is in
/// `docs/wp/x30-findings.md`), so unset and an explicit `Integer.MAX_VALUE` fail alike and any other value passes. A
/// run calls this after `parse_sync`; `parse_sync` itself stays Kotlin's binding, which the JVM goldens replay.
pub fn validate_uncompleted_retries_bounded(configuration: &Configuration) -> Result<(), Box<ConfigurationError>> {
    if configuration.uncompleted_test_retry_quota != i32::MAX {
        return Ok(());
    }
    // iOS's setting looks like an alternative and earlier docs recommended it, so the message answers that question.
    let not_enough = match configuration.vendor_configuration {
        VendorConfiguration::Ios(_) => {
            " maxBatchesWithoutProgress does not replace it: a lost connection and an unclassified error are not counted."
        }
        VendorConfiguration::Android(_)
        | VendorConfiguration::Macos(_)
        | VendorConfiguration::Stub
        | VendorConfiguration::Empty => "",
    };
    Err(stage_error(
        ValidationStage::Parse,
        CONFIGURATION_EXCEPTION_CLASS,
        Some(format!(
            "uncompletedTestRetryQuota is unlimited (2147483647 when unset), and no other setting ends a run in which a test keeps coming back uncompleted: it would repeat until the job is killed.{not_enough} Set a finite value in the Marathonfile, for example `uncompletedTestRetryQuota: 5` (the number of devices + 2, at least 5). See {FINITE_RETRIES_MIGRATION_URL}"
        )),
    ))
}

// ---- Logical validator (stage b) --------------------------------------------

fn logical_error(message: String) -> Box<ConfigurationError> {
    stage_error(ValidationStage::Logical, CONFIGURATION_EXCEPTION_CLASS, Some(message))
}

/// `LogicalConfigurationValidator.kt:12-89`, statement for statement: the
/// order is observable (first failure wins).
fn validate_logical_sync(configuration: &Configuration) -> Result<(), Box<ConfigurationError>> {
    let complex_flakiness = !matches!(configuration.flakiness_strategy, FlakinessStrategyConfiguration::Ignore);
    if complex_flakiness && !matches!(configuration.sharding_strategy, ShardingStrategyConfiguration::Parallel) {
        return Err(logical_error(
            "Configuration is invalid: can't use complex sharding and complex flakiness strategy at the same time. See: https://github.com/MarathonLabs/marathon/issues/197".to_owned(),
        ));
    }

    configuration
        .filtering_configuration
        .validate()
        .map_err(|error| boxed_validation_error(error, ValidationStage::Logical))?;

    let analytics_defaults = match &configuration.analytics_configuration {
        AnalyticsConfiguration::InfluxDb(config) => Some(config.defaults),
        AnalyticsConfiguration::InfluxDb2(config) => Some(config.defaults),
        AnalyticsConfiguration::Graphite(config) => Some(config.defaults),
        AnalyticsConfiguration::Disabled => None,
    };
    if let Some(defaults) = analytics_defaults
        && let Some(observed) = defaults.observed_duration_min_tests
        && observed < 1
    {
        return Err(logical_error(format!(
            "analyticsConfiguration.defaults.observedDurationMinTests must be at least 1, got {observed}"
        )));
    }

    match &configuration.vendor_configuration {
        VendorConfiguration::Ios(_) | VendorConfiguration::Android(_) => {
            validate_vendor_sync(&configuration.vendor_configuration)?;
        }
        VendorConfiguration::Macos(macos) => {
            if macos.xcresult.queue != XcresultQueueConfiguration::default_instance() {
                return Err(logical_error(
                    "xcresult.queue is supported only for iOS simulators".to_owned(),
                ));
            }
            if macos.threading_configuration.provisioning_concurrency
                != AppleThreadingConfiguration::default_instance().provisioning_concurrency
            {
                return Err(logical_error(
                    "threadingConfiguration.provisioningConcurrency is supported only for iOS simulators".to_owned(),
                ));
            }
        }
        VendorConfiguration::Stub | VendorConfiguration::Empty => {}
    }

    match configuration.execution_strategy.mode {
        ExecutionMode::AnySuccess => {
            if let ShardingStrategyConfiguration::Count { count } = configuration.sharding_strategy
                && count != 1
            {
                return Err(logical_error(
                    "Configuration is invalid: can't use complex sharding and any success execution strategy at the same time. Consult documentation for the any success execution logic".to_owned(),
                ));
            }
        }
        ExecutionMode::AllSuccess => {
            if complex_flakiness {
                return Err(logical_error(
                    "Configuration is invalid: can't use complex flakiness strategy and all success execution strategy at the same time. Consult documentation for the all success execution logic".to_owned(),
                ));
            }
            if !matches!(configuration.retry_strategy, RetryStrategyConfiguration::NoRetry) {
                return Err(logical_error(
                    "Configuration is invalid: can't use complex retry strategy and all success execution strategy at the same time. Consult documentation for the all success execution logic".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// The `ConfigurationValidator` impl (`LogicalConfigurationValidator.kt`).
/// Who calls it (inside `Marathon.runAsync`) is WP 3.10.
#[derive(Clone, Copy, Debug, Default)]
pub struct LogicalConfigurationValidator;

impl LogicalConfigurationValidator {
    pub fn validate_sync(&self, config: &Configuration) -> Result<(), Box<ConfigurationError>> {
        validate_logical_sync(config)
    }
}

impl ConfigurationValidator for LogicalConfigurationValidator {
    fn validate<'a>(&'a self, config: &'a Configuration) -> BindingFuture<'a, ()> {
        Box::pin(async move { validate_logical_sync(config).map_err(|e| *e) })
    }
}
