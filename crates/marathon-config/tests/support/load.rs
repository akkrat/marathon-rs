//! Whole-Marathonfile helpers for the WP 2.4 tests: temp-dir copies of the
//! fixture configs (bundle extraction never writes into `fixtures/`), the
//! P1 context Kotlin's tests build, and the production loader/validator.
#![allow(dead_code)]

use jcompat::jtime::{Instant, instant_of};
use marathon_config::{
    Configuration,
    binding::{
        BindingContext, BindingFuture, ConfigurationError, ConfigurationLoader, ConfigurationValidator, Profile,
    },
    loader::{LogicalConfigurationValidator, MarathonConfigurationLoader},
};
use marathon_error::FailureKind;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn fixture_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/configuration/fixture/config")
}

pub fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// A temp dir and its canonical path (the CLI passes the Marathonfile's
/// canonical parent; macOS temp dirs sit behind the `/var` symlink).
pub fn temp() -> io::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let canonical = dir.path().canonicalize()?;
    Ok((dir, canonical))
}

/// A temp copy of `fixtures/configuration/fixture/config`.
pub fn fixture_copy() -> io::Result<(tempfile::TempDir, PathBuf)> {
    let (dir, root) = temp()?;
    copy_dir(&fixture_config(), &root)?;
    Ok((dir, root))
}

pub fn java_path(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().into_owned())
}

/// Kotlin tests' reference instant, `Instant.ofEpochSecond(1000000)`.
pub fn reference_now() -> Instant {
    instant_of(1_000_000, 0).expect("in range")
}

/// The P1 context of a `ConfigurationFactory(marathonfileDir = dir, …)`
/// whose environment reader returns `sdk`, with both CLI tracking flags
/// left at "not disabled".
pub fn context(marathonfile: &Path, marathonfile_dir: &Path, sdk: Option<&str>) -> BindingContext {
    BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: java_path(marathonfile),
        analytics_tracking: true,
        bugsnag_reporting: true,
        marathonfile_dir: java_path(marathonfile_dir),
        process_dir: java_path(marathonfile_dir),
        environment: sdk
            .map(|sdk| vec![(String::from("ANDROID_HOME"), String::from(sdk))])
            .unwrap_or_default(),
        now: reference_now(),
    }
}

/// Drives a frozen-trait future: the impls do their blocking work on the
/// first poll and never return `Pending`.
pub fn ready<T>(future: BindingFuture<'_, T>) -> Result<T, Box<ConfigurationError>> {
    let mut future = future;
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(result) => result.map_err(Box::new),
        std::task::Poll::Pending => panic!("the configuration futures complete on their first poll"),
    }
}

/// `ConfigurationFactory.parse(file)`: stage (a) through the production
/// loader, called through the frozen `ConfigurationLoader` trait.
pub fn parse(file: &Path, context: &BindingContext) -> Result<Configuration, Box<ConfigurationError>> {
    let bytes = fs::read(file).map_err(|e| {
        Box::new(ConfigurationError {
            stage: marathon_config::binding::ValidationStage::Parse,
            kind: FailureKind::Io,
            message: Some(e.to_string()),
            causes: Vec::new(),
            path: Vec::new(),
            span: None,
            cause: marathon_config::binding::BindingCause::Custom(e.to_string()),
        })
    })?;
    ready(ConfigurationLoader::parse(
        &MarathonConfigurationLoader,
        &bytes,
        context,
    ))
}

/// `LogicalConfigurationValidator().validate(configuration)`.
pub fn validate(configuration: &Configuration) -> Result<(), Box<ConfigurationError>> {
    ready(ConfigurationValidator::validate(
        &LogicalConfigurationValidator,
        configuration,
    ))
}

pub fn message(error: &ConfigurationError) -> String {
    error.message.to_owned().unwrap_or_default()
}
pub fn class(error: &ConfigurationError) -> String {
    error.kind.legacy_class().to_owned()
}
