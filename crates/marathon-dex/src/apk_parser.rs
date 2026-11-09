//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/ApkParser.kt
//! and vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/InstrumentationInfo.kt

use crate::{
    axml::{AxmlParser, END_DOCUMENT, START_TAG},
    error::{DexError, DexResult},
};
use marathon_error::FailureKind;

use std::{fs::File, io::Read, path::Path};

/// InstrumentationInfo.kt: (applicationPackage, instrumentationPackage,
/// testRunnerClass) — applicationPackage is the instrumentation's
/// targetPackage, instrumentationPackage the manifest's own package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstrumentationInfo {
    pub application_package: String,
    pub instrumentation_package: String,
    pub test_runner_class: String,
}

/// ZipFile(apk).getEntry("AndroidManifest.xml") + getInputStream(entry):
/// a missing entry is the platform NPE from getInputStream(null), which
/// ApkParser's IOException catch does not wrap (probed: class
/// java.lang.NullPointerException, message "entry").
fn read_manifest(apk: &Path) -> DexResult<Vec<u8>> {
    let file = File::open(apk).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            // ZipFile opens through java.nio: NoSuchFileException directly.
            DexError::with_class(FailureKind::NoSuchFile, Some(apk.to_string_lossy().as_ref().into()))
        } else {
            DexError::io(err.to_string().as_str())
        }
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|err| match err {
        // ZipFile's own wording for a file with no END record; it rides
        // inside the RuntimeException cause chain marathon logs.
        zip::result::ZipError::InvalidArchive(_) => {
            DexError::with_class(FailureKind::Zip, Some("zip END header not found".into()))
        }
        other => DexError::from_zip(other),
    })?;
    let mut entry = match archive.by_name("AndroidManifest.xml") {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Err(DexError::null_pointer("entry")),
        Err(err) => return Err(DexError::from_zip(err)),
    };
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(|err| DexError::io(err.to_string().as_str()))?;
    Ok(bytes)
}

/// Only IOException is caught and rethrown as RuntimeException; the
/// checkNotNull IllegalStateExceptions and the missing-entry NPE pass
/// through unwrapped.
fn wrap_io<T>(result: DexResult<T>) -> DexResult<T> {
    result.map_err(|err| {
        if err.is_io() {
            DexError::unable_to_parse_manifest(err)
        } else {
            err
        }
    })
}

pub fn parse_instrumentation_info(apk: &Path) -> DexResult<InstrumentationInfo> {
    wrap_io(parse_instrumentation_info_inner(apk))
}

fn parse_instrumentation_info_inner(apk: &Path) -> DexResult<InstrumentationInfo> {
    let manifest = read_manifest(apk)?;
    let mut parser = AxmlParser::new(&manifest)?;
    let mut event_type = parser.event_type();

    // Kotlin's locals are swapped relative to their names (testPackage
    // holds the manifest package, appPackage the targetPackage) and the
    // constructor call un-swaps them; the RESULT is what's ported here.
    let mut app_package: Option<String> = None;
    let mut test_package: Option<String> = None;
    let mut test_runner_class: Option<String> = None;
    while event_type != END_DOCUMENT {
        if event_type == START_TAG {
            let parser_name = parser.name()?;
            let is_manifest = parser_name.as_deref() == Some("manifest");
            let is_instrumentation = parser_name.as_deref() == Some("instrumentation");
            if is_manifest || is_instrumentation {
                for i in 0..parser.attribute_count() {
                    let attribute_name = parser.attribute_name(i)?;
                    if is_manifest && attribute_name.as_deref() == Some("package") {
                        test_package = parser.attribute_value_string(i)?;
                    } else if is_instrumentation && attribute_name.as_deref() == Some("targetPackage") {
                        app_package = parser.attribute_value_string(i)?;
                    } else if is_instrumentation && attribute_name.as_deref() == Some("name") {
                        test_runner_class = parser.attribute_value_string(i)?;
                    }
                }
            }
        }
        event_type = parser.next()?;
    }

    let Some(mut runner) = test_runner_class else {
        return Err(DexError::illegal_state("Could not find test runner class."));
    };
    let Some(test_package) = test_package else {
        return Err(DexError::illegal_state("Could not find test application package."));
    };
    let Some(app_package) = app_package else {
        return Err(DexError::illegal_state("Could not find application package."));
    };

    // Support relative declaration of instrumentation test runner.
    if runner.starts_with('.') {
        let mut combined = test_package.clone();
        combined.push_str(&runner);
        runner = combined;
    } else if !runner.contains('.') {
        let mut combined = test_package.clone();
        combined.push('.');
        combined.push_str(&runner);
        runner = combined;
    }

    Ok(InstrumentationInfo {
        application_package: app_package,
        instrumentation_package: test_package,
        test_runner_class: runner,
    })
}

pub fn parse_app_package_name(apk: &Path) -> DexResult<String> {
    wrap_io(parse_app_package_name_inner(apk))
}

fn parse_app_package_name_inner(apk: &Path) -> DexResult<String> {
    let manifest = read_manifest(apk)?;
    let mut parser = AxmlParser::new(&manifest)?;
    let mut event_type = parser.event_type();

    let mut app_package: Option<String> = None;
    while event_type != END_DOCUMENT {
        if event_type == START_TAG {
            let parser_name = parser.name()?;
            if parser_name.as_deref() == Some("manifest") {
                for i in 0..parser.attribute_count() {
                    if parser.attribute_name(i)?.as_deref() == Some("package") {
                        app_package = parser.attribute_value_string(i)?;
                        break; // first package attribute wins; the walk continues
                    }
                }
            }
        }
        event_type = parser.next()?;
    }

    app_package.ok_or_else(|| DexError::illegal_state("Could not find application package."))
}
