//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! Pre-processing/binding only; vendor post-processing belongs to ConfigurationLoader.
//!
//! Profile split (WP 2.3b decision 5): P1 (`bind_bytes`) wraps binding
//! failures in `ConfigurationException("Error parsing config file …")` at
//! stage `Parse`, as `ConfigurationFactory` does. P2
//! (`bind_devices_bytes`) has no wrapper: the original Jackson class and
//! its stream-vs-coercion cause survive at stage `Provider`, because the
//! Apple providers catch only `JsonMappingException` and wrap it as
//! `NoDevicesException` themselves (WP 5.5a/5.6 own that wrapping and the
//! empty-workers check). Substitution failures escape unwrapped in both
//! profiles, as in Kotlin.
use crate::{
    MarathonDevices,
    bind_devices::bind_marathon_devices,
    binder::{Binder, Result, Value},
    binding::{BindingContext, ConfigurationCause, ConfigurationError, ValidationStage},
};
use jcompat::{StringSubstitutor, SubstitutionError};

#[derive(Debug)]
pub enum LoadError {
    Substitution(SubstitutionError),
    Configuration(Box<ConfigurationError>),
    /// The Kotlin caller dereferences a null configuration outside its parse wrapper.
    NullDocument,
    /// The OS could not create the binding worker; no config binding took place.
    Worker(std::io::Error),
}
// Debug composition binding measured 7,568 bytes per recursive level on aarch64.
// Budget that cost for all 1,000 parser levels (even though composition needs
// two containers per level), multiply by four, and round up to 32 MiB.
// The maximum-depth subprocess regression checks the margin on each build.
pub const BIND_STACK_SIZE: usize = 32 * 1024 * 1024;

/// Decode, substitute and bind the first document, without any profile's
/// error wrapping. `None` is a leading null document.
fn bind_first<T: Send>(
    bytes: &[u8],
    context: &BindingContext,
    bind: impl FnOnce(Value<'_>) -> Result<T> + Send,
) -> std::result::Result<Option<T>, LoadError> {
    let decoded = jcompat::utf8::decode_utf8(bytes);
    // A repeated name resolves to its FIRST occurrence, as JVM
    // `System.getenv` (the substitutor's environment lookup) does.
    let substitutor = StringSubstitutor::new(|key| {
        context
            .environment
            .iter()
            .find(|(k, _)| k == &key.to_owned())
            .map(|(_, v)| v.to_owned())
    });
    let text = substitutor.replace(&decoded).map_err(LoadError::Substitution)?;
    let stream = snakeyaml::node::load(&text);
    let bound = std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("config-binding".into())
            .stack_size(BIND_STACK_SIZE)
            .spawn_scoped(scope, || {
                let binder = Binder::new(context, &stream);
                binder.first(&stream, bind)
            })
            .map_err(LoadError::Worker)?;
        match worker.join() {
            Ok(result) => Ok(result),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })?;
    bound.map_err(LoadError::Configuration)
}

/// Blocking (worker join and File canonicalization): call from an off-core task.
/// Binding runs on a dedicated stack so accepted parser depths are also bindable.
pub fn bind_bytes<T: Send>(
    bytes: &[u8],
    context: &BindingContext,
    bind: impl FnOnce(Value<'_>) -> Result<T> + Send,
) -> std::result::Result<T, LoadError> {
    bind_first(bytes, context, bind)
        .and_then(|bound| bound.ok_or(LoadError::NullDocument))
        .map_err(|error| match error {
            LoadError::Configuration(mut e) if e.class.to_owned().starts_with("com.fasterxml.jackson.") => {
                e.causes.insert(
                    0,
                    ConfigurationCause {
                        class: e.class.clone(),
                        message: e.message.take(),
                    },
                );
                e.class = "com.malinskiy.marathon.config.exceptions.ConfigurationException".into();
                let path = jcompat::file_manager::absolute_file_path(
                    &context.marathonfile.to_string_lossy(),
                    &context.process_dir.to_string_lossy(),
                );
                e.message = Some(format!(
                    "Error parsing config file {}",
                    path.to_string_lossy().into_owned()
                ));
                LoadError::Configuration(e)
            }
            other => other,
        })
}

/// The P2 entry point: the body `ConfigurationLoader::devices` will call
/// (WP 2.4 assembles the loader). The caller passes a `BindingContext`
/// with `Profile::Marathondevices`, which is what selects the standard
/// `File` deserializer and rejects `Instant`/`Duration` in the binder.
/// Blocking like `bind_bytes`: it binds on a 32 MiB-stack thread and
/// joins it, so call it off the core executor (docs/design/lifecycle.md).
pub fn bind_devices_bytes(bytes: &[u8], context: &BindingContext) -> std::result::Result<MarathonDevices, LoadError> {
    bind_first(bytes, context, bind_marathon_devices)
        .and_then(|bound| bound.flatten().ok_or(LoadError::NullDocument))
        .map_err(|error| match error {
            LoadError::Configuration(mut e) => {
                e.stage = ValidationStage::Provider;
                LoadError::Configuration(e)
            }
            other => other,
        })
}
