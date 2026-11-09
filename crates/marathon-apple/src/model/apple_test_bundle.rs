//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/model/AppleTestBundle.kt
//!
//! The Mach-O check is WP 4.5a's [`crate::mach_o`] (A30: no fat-64 magic). Construction reads both
//! Info.plists, as Kotlin's `appId`/`testBundleId` initializers force their lazies (decision 4); the binary
//! getters scan on first use, keep a successful value and retry after a failure, like Kotlin's `lazy`.

use super::Sdk;
use crate::{
    cmd::{blocking, failure, remote::ssh::sshj::config::SshLogging},
    mach_o::is_apple_binary_reader,
    plist::{PropertyList, bundle::BundleInfo, plist_failure},
};
use jcompat::{
    logging::{Level, Logger},
    regex_patterns::BUNDLE_NAME,
};
use marathon_config::CONFIGURATION_EXCEPTION_CLASS;
use marathon_core::{
    error::{VendorError, VendorResult},
    runtime::VendorRuntime,
    vendor::TestBundle,
};
use marathon_error::FailureKind;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

fn configuration(message: impl AsRef<str>) -> VendorError {
    VendorError::Other(failure(CONFIGURATION_EXCEPTION_CLASS, message))
}

/// `java.io.File` construction: repeated and trailing slashes collapse; nothing else is normalized.
fn java_file(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '/' && out.ends_with('/') {
            continue;
        }
        out.push(c);
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}
/// `File.getAbsolutePath`: a relative path is resolved against the working directory, without canonicalizing.
fn java_absolute(path: &Path) -> io::Result<String> {
    let file = java_file(path);
    if file.starts_with('/') {
        return Ok(file);
    }
    let cwd = java_file(&std::env::current_dir()?);
    Ok(if file.is_empty() { cwd } else { format!("{cwd}/{file}") })
}
fn file_name(file: &str) -> &str {
    file.rsplit('/').next().unwrap_or(file)
}
/// `File.nameWithoutExtension`.
fn name_without_extension(file: &str) -> &str {
    let name = file_name(file);
    name.rfind('.').map_or(name, |dot| &name[..dot])
}
/// `File.extension == ""`: no dot at all, or a trailing one (`.hidden` has the extension `hidden`).
fn has_no_extension(name: &str) -> bool {
    name.rfind('.').is_none_or(|dot| dot + 1 == name.len())
}

fn io_failure(path: &Path, error: &io::Error) -> VendorError {
    let reason = match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory".to_owned(),
        io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        _ => error.to_string(),
    };
    VendorError::Other(failure(
        FailureKind::FileNotFound,
        format!("{} ({reason})", path.display()),
    ))
}
/// The directory scanned for a bundle's binaries; macOS goes through the absolute path, as Kotlin's
/// `Paths.get(absolutePath, ...)` does.
fn binary_dir(sdk: Sdk, relative: &[&str], bundle: &Path) -> VendorResult<PathBuf> {
    if sdk != Sdk::Macos {
        return Ok(bundle.to_path_buf());
    }
    let absolute = java_absolute(bundle).map_err(|e| io_failure(bundle, &e))?;
    Ok(relative.iter().fold(PathBuf::from(absolute), |p, c| p.join(c)))
}
/// `listFiles()?.filter { isFile && extension == "" && isAppleBinary }`: None when the directory cannot be
/// listed. A candidate that cannot be opened fails, as Kotlin's `inputStream()` does.
fn binaries(dir: &Path) -> VendorResult<Option<Vec<PathBuf>>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(None);
    };
    let mut found = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return Ok(None);
        };
        let path = entry.path();
        if !std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            continue;
        }
        if !has_no_extension(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let file = std::fs::File::open(&path).map_err(|e| io_failure(&path, &e))?;
        if is_apple_binary_reader(file).map_err(|e| io_failure(&path, &e))? {
            found.push(path);
        }
    }
    Ok(Some(found))
}

pub struct AppleTestBundle {
    pub application: Option<PathBuf>,
    pub test_application: Option<PathBuf>,
    pub xctest_bundle: PathBuf,
    pub sdk: Sdk,
    id: String,
    application_bundle_info: Option<BundleInfo>,
    app_id: String,
    test_bundle_info: BundleInfo,
    test_bundle_id: String,
    runtime: Arc<dyn VendorRuntime>,
    logging: SshLogging,
    logger: Logger,
    test_binary: Mutex<Option<PathBuf>>,
    test_runner_binary: Mutex<Option<PathBuf>>,
    application_binary: Mutex<Option<Option<PathBuf>>>,
}
impl AppleTestBundle {
    /// Reads the application's Info.plist, then the test bundle's, off the core executor.
    pub async fn new(
        application: Option<PathBuf>,
        test_application: Option<PathBuf>,
        xctest_bundle: PathBuf,
        sdk: Sdk,
        runtime: Arc<dyn VendorRuntime>,
        logging: SshLogging,
    ) -> VendorResult<Self> {
        // Kotlin holds java.io.File values, already normalized; listings and messages use that spelling.
        let file = |path: PathBuf| PathBuf::from(java_file(&path));
        let (application, test_application, xctest_bundle) =
            (application.map(file), test_application.map(file), file(xctest_bundle));
        let logger = logging.logger("com.malinskiy.marathon.apple.model.AppleTestBundle");
        let root = Self::root_path(sdk);
        let (app, xctest) = (application.clone(), xctest_bundle.clone());
        let (id, application_bundle_info, app_id, test_bundle_info, test_bundle_id) = blocking(&*runtime, move || {
            let id = java_absolute(&xctest).map_err(|e| io_failure(&xctest, &e))?;
            let read = |bundle: &Path| {
                let file = bundle.join(root).join("Info.plist");
                BundleInfo::from_path(&file).map_err(|e| plist_failure(&file, e))
            };
            let application_bundle_info = app.as_deref().map(read).transpose()?;
            let app_id = match &application_bundle_info {
                // A27: a missing CFBundleIdentifier fails in the delegate, before this fallback.
                Some(info) => info
                    .identification()
                    .bundle_identifier()
                    .map_err(|e| plist_failure(Path::new(""), e))?,
                None => {
                    let shown = app.as_deref().map_or_else(|| "null".into(), java_file);
                    return Err(configuration(format!("No bundle identifier specified in {shown}")));
                }
            };
            let test_bundle_info = read(&xctest)?;
            // A31: the nameWithoutExtension fallback is unreachable, a missing CFBundleName fails first.
            let bundle_name = test_bundle_info
                .naming()
                .bundle_name()
                .map_err(|e| plist_failure(Path::new(""), e))?;
            let test_bundle_id = BUNDLE_NAME
                .replace(&bundle_name, "_")
                .map_err(|e| VendorError::Other(failure(FailureKind::IllegalArgument, e.to_string())))?;
            Ok((
                String::from(id.as_str()),
                application_bundle_info,
                app_id,
                test_bundle_info,
                test_bundle_id,
            ))
        })
        .await?;
        Ok(Self {
            application,
            test_application,
            xctest_bundle,
            sdk,
            id,
            application_bundle_info,
            app_id,
            test_bundle_info,
            test_bundle_id,
            runtime,
            logging,
            logger,
            test_binary: Mutex::new(None),
            test_runner_binary: Mutex::new(None),
            application_binary: Mutex::new(None),
        })
    }
    fn root_path(sdk: Sdk) -> &'static str {
        if sdk == Sdk::Macos { "Contents" } else { "" }
    }
    pub fn application_bundle_info(&self) -> Option<&BundleInfo> {
        self.application_bundle_info.as_ref()
    }
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    pub fn test_bundle_info(&self) -> &BundleInfo {
        &self.test_bundle_info
    }
    pub fn test_bundle_id(&self) -> &str {
        &self.test_bundle_id
    }
    /// Path of the app and test binaries relative to the bundle folder.
    pub fn relative_binary_path(&self) -> Vec<&'static str> {
        let mut path = self.relative_root_path();
        if self.sdk == Sdk::Macos {
            path.push("MacOS");
        }
        path
    }
    pub fn relative_root_path(&self) -> Vec<&'static str> {
        if self.sdk == Sdk::Macos {
            vec!["Contents"]
        } else {
            vec![]
        }
    }
    pub fn relative_frameworks_path(&self) -> Vec<&'static str> {
        let mut path = self.relative_root_path();
        path.push("Frameworks");
        path
    }
    async fn scan(&self, bundle: &Path) -> VendorResult<Option<Vec<PathBuf>>> {
        let (sdk, relative) = (self.sdk, self.relative_binary_path());
        let bundle = bundle.to_path_buf();
        // Resolving a relative macOS path reads the working directory, so it runs on the blocking pool too.
        blocking(&*self.runtime, move || binaries(&binary_dir(sdk, &relative, &bundle)?)).await
    }
    fn warn(&self, message: String) {
        if self.logging.enabled(&self.logger, Level::Warn) {
            (self.logging.sink)(&self.logger, Level::Warn, message, None);
        }
    }
    /// Several candidates: warn, prefer the one named like the bundle, else the first listed.
    fn choose(
        &self,
        mut candidates: Vec<PathBuf>,
        bundle: &Path,
        folder: &str,
        missing: &str,
    ) -> VendorResult<PathBuf> {
        match candidates.len() {
            0 => Err(configuration(missing)),
            1 => Ok(candidates.remove(0)),
            _ => {
                let names: Vec<String> = candidates
                    .iter()
                    .map(|c| {
                        c.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    })
                    .collect();
                self.warn(format!(
                    "Multiple test binaries present [{}] in {folder} folder",
                    names.join(",")
                ));
                let stem = java_file(bundle);
                let stem = name_without_extension(&stem);
                let index = names.iter().position(|name| name == stem).unwrap_or(0);
                Ok(candidates.swap_remove(index))
            }
        }
    }
    pub async fn test_binary(&self) -> VendorResult<PathBuf> {
        let mut cached = self.test_binary.lock().await;
        if let Some(path) = &*cached {
            return Ok(path.clone());
        }
        let missing = format!(
            "missing test binaries in xctest folder at {}",
            java_file(&self.xctest_bundle)
        );
        let candidates = self
            .scan(&self.xctest_bundle)
            .await?
            .ok_or_else(|| configuration(&missing))?;
        let path = self.choose(candidates, &self.xctest_bundle, "xctest", &missing)?;
        *cached = Some(path.clone());
        Ok(path)
    }
    pub async fn test_runner_binary(&self) -> VendorResult<PathBuf> {
        let mut cached = self.test_runner_binary.lock().await;
        if let Some(path) = &*cached {
            return Ok(path.clone());
        }
        let Some(test_application) = &self.test_application else {
            return Err(configuration("no test application provided"));
        };
        let missing = format!(
            "missing test binaries in test runner folder at {}",
            java_file(test_application)
        );
        let candidates = self
            .scan(test_application)
            .await?
            .ok_or_else(|| configuration(&missing))?;
        let path = self.choose(candidates, test_application, "test runner", &missing)?;
        *cached = Some(path.clone());
        Ok(path)
    }
    /// No bundle-name preference here, unlike the test binaries.
    pub async fn application_binary(&self) -> VendorResult<Option<PathBuf>> {
        let mut cached = self.application_binary.lock().await;
        if let Some(path) = &*cached {
            return Ok(path.clone());
        }
        let path = match &self.application {
            None => None,
            Some(application) => match self.scan(application).await? {
                None => None,
                Some(mut candidates) => match candidates.len() {
                    0 => None,
                    1 => Some(candidates.remove(0)),
                    _ => {
                        self.warn("Multiple application binaries present in app folder".into());
                        Some(candidates.remove(0))
                    }
                },
            },
        };
        *cached = Some(path.clone());
        Ok(path)
    }
}
impl TestBundle for AppleTestBundle {
    fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod plist_boundary_test {
    #[test]
    fn bundle_plist_error_uses_canonical_class_and_message() {
        let error = super::plist_failure(
            std::path::Path::new("Info.plist"),
            crate::plist::PlistError::Io {
                is_not_found: false,
                message: "read denied".into(),
            },
        );
        assert_eq!(error.failure().kind, marathon_error::FailureKind::Io);
        assert_eq!(error.failure().message.as_ref().unwrap().to_owned(), "read denied");
    }
}
