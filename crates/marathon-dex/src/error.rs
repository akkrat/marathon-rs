//! Port of com/linkedin/dex/spec/DexException.kt (dex-test-parser 2.3.4,
//! BSD-2-Clause, Copyright (c) LinkedIn Corporation) and the exception
//! shapes marathon's ApkParser surfaces (vendor/vendor-android/src/main/
//! kotlin/com/malinskiy/marathon/android/ApkParser.kt).
//!
//! One error shape (WP 6.4 decision P3): the JVM exception class and
//! message are kept where marathon's own code forges them (DexException
//! texts, checkNotNull messages, the RuntimeException wrap) and where they
//! reach logs (FileNotFoundException); malformed-input crash taxonomy
//! (BufferUnderflowException, ArrayIndexOutOfBoundsException,
//! StackOverflowError, …) collapses to DexException with our wording.

use jcompat::writers::JavaString;
use std::{fmt, io, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DexError {
    pub jvm_class: JavaString,
    pub message: Option<JavaString>,
    pub cause: Option<Box<DexError>>,
}

impl DexError {
    pub fn dex_exception(message: impl Into<JavaString>) -> Self {
        Self {
            jvm_class: "com.linkedin.dex.spec.DexException".into(),
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn illegal_state(message: impl Into<JavaString>) -> Self {
        Self {
            jvm_class: "java.lang.IllegalStateException".into(),
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn null_pointer(message: impl Into<JavaString>) -> Self {
        Self {
            jvm_class: "java.lang.NullPointerException".into(),
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn with_class(class: &str, message: Option<JavaString>) -> Self {
        Self {
            jvm_class: class.into(),
            message,
            cause: None,
        }
    }

    pub fn eof() -> Self {
        Self::with_class("java.io.EOFException", None)
    }

    /// ApkParser's `catch (e: IOException)` wrap; the wording (including
    /// "test app" in parseAppPackageName) is marathon's and reaches logs.
    pub fn unable_to_parse_manifest(cause: DexError) -> Self {
        Self {
            jvm_class: "java.lang.RuntimeException".into(),
            message: Some("Unable to parse test app AndroidManifest.xml.".into()),
            cause: Some(Box::new(cause)),
        }
    }

    /// IOException-shaped failure (the only kind ApkParser catches/wraps).
    pub fn io(message: impl Into<JavaString>) -> Self {
        Self {
            jvm_class: "java.io.IOException".into(),
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn is_io(&self) -> bool {
        let class: JavaString = "java.io.IOException".into();
        let zip: JavaString = "java.util.zip.ZipException".into();
        let eof: JavaString = "java.io.EOFException".into();
        let fnf: JavaString = "java.io.FileNotFoundException".into();
        let nsf: JavaString = "java.nio.file.NoSuchFileException".into();
        self.jvm_class == class
            || self.jvm_class == zip
            || self.jvm_class == eof
            || self.jvm_class == fnf
            || self.jvm_class == nsf
    }

    /// Opening a file with java.io semantics: a missing file is
    /// FileNotFoundException("<path> (No such file or directory)") with a
    /// NoSuchFileException cause on the reference JDK (probes.jsonl).
    pub fn open_file(path: &Path, err: &io::Error) -> Self {
        let shown = path.to_string_lossy();
        if err.kind() == io::ErrorKind::NotFound {
            return Self {
                jvm_class: "java.io.FileNotFoundException".into(),
                message: Some(format!("{shown} (No such file or directory)").as_str().into()),
                cause: Some(Box::new(Self {
                    jvm_class: "java.nio.file.NoSuchFileException".into(),
                    message: Some(shown.as_ref().into()),
                    cause: None,
                })),
            };
        }
        Self::io(format!("{shown} ({err})").as_str())
    }

    pub fn zip_exception(message: impl Into<JavaString>) -> Self {
        Self {
            jvm_class: "java.util.zip.ZipException".into(),
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn from_zip(err: zip::result::ZipError) -> Self {
        match err {
            zip::result::ZipError::Io(io_err) => Self::io(io_err.to_string().as_str()),
            other => Self::zip_exception(other.to_string().as_str()),
        }
    }
}

impl fmt::Display for DexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.jvm_class.to_string_lossy())?;
        if let Some(message) = &self.message {
            write!(f, ": {}", message.to_string_lossy())?;
        }
        if let Some(cause) = &self.cause {
            write!(f, " (cause: {cause})")?;
        }
        Ok(())
    }
}

impl std::error::Error for DexError {}

pub type DexResult<T> = Result<T, DexError>;
