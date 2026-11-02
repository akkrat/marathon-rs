//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/ValidationResponse.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use std::path::Path;

use super::consts::MAX_REMOTE_PATH_LENGTH;
use super::feature::Feature;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationResponse {
    pub success: bool,
    pub message: Option<String>,
}

impl ValidationResponse {
    pub const SUCCESS: ValidationResponse = ValidationResponse {
        success: true,
        message: None,
    };

    pub fn new(success: bool, message: Option<String>) -> Self {
        Self { success, message }
    }

    pub fn missing_feature(feature: Feature) -> String {
        format!("{} is not supported by device", feature.name())
    }

    pub fn missing_either_feature(features: &[Feature]) -> String {
        format!("Supported features must include either of {}", join(features))
    }

    pub fn one_of_files_should_be(extension: &str) -> String {
        // uppercase(Locale.ENGLISH) has no locale-specific mappings, so full Unicode uppercasing matches.
        format!(
            "At least one of the files has to be an {} file",
            extension.to_uppercase()
        )
    }

    pub fn package_should_exist(file: &Path) -> String {
        format!("Package {} doesn't exist", absolute_path(file))
    }

    pub fn package_should_be_regular_file(file: &Path) -> String {
        format!("Package {} is not a regular file", absolute_path(file))
    }

    /// The doubled closing brace and "on of" are adam's.
    pub fn package_should_be_supported_extension(file: &Path, supported: &[&str]) -> String {
        format!(
            "Unsupported package extension {}. Should be on of {}}}",
            extension(file),
            supported.join(", ")
        )
    }

    pub fn path_should_not_be_long() -> String {
        format!("Remote path should be less that {MAX_REMOTE_PATH_LENGTH} bytes")
    }
}

fn join(features: &[Feature]) -> String {
    features.iter().map(|f| f.name()).collect::<Vec<_>>().join(", ")
}

/// `File.absolutePath`: relative paths are resolved against the working directory, nothing else changes.
fn absolute_path(file: &Path) -> String {
    if file.is_absolute() {
        return file.display().to_string();
    }
    match std::env::current_dir() {
        Ok(dir) => dir.join(file).display().to_string(),
        Err(_) => file.display().to_string(),
    }
}

/// Kotlin `File.extension`: the name's part after the last dot, or empty.
pub fn extension(file: &Path) -> String {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        assert_eq!(
            ValidationResponse::missing_feature(Feature::AbbExec),
            "ABB_EXEC is not supported by device"
        );
        assert_eq!(
            ValidationResponse::missing_either_feature(&[Feature::AbbExec, Feature::Cmd]),
            "Supported features must include either of ABB_EXEC, CMD"
        );
        assert_eq!(
            ValidationResponse::package_should_be_supported_extension(Path::new("/a/b.zip"), &["apk", "apex"]),
            "Unsupported package extension zip. Should be on of apk, apex}"
        );
        assert_eq!(
            ValidationResponse::one_of_files_should_be("apk"),
            "At least one of the files has to be an APK file"
        );
        assert_eq!(
            ValidationResponse::package_should_exist(Path::new("/x.apk")),
            "Package /x.apk doesn't exist"
        );
        assert_eq!(extension(Path::new("/a/.hidden")), "hidden");
        assert_eq!(extension(Path::new("/a/plain")), "");
    }
}
