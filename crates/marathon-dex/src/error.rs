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

use marathon_error::{DexKind, FailureKind};
use std::{fmt, io, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DexError {
    pub kind: FailureKind,
    pub message: Option<String>,
    pub cause: Option<Box<DexError>>,
}

impl DexError {
    pub fn dex_exception(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Dex(DexKind::Decode),
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn illegal_state(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::IllegalState,
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn null_pointer(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::NullReference,
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn with_class(class: FailureKind, message: Option<String>) -> Self {
        Self {
            kind: class,
            message,
            cause: None,
        }
    }

    pub fn eof() -> Self {
        Self::with_class(FailureKind::Eof, None)
    }

    /// ApkParser's `catch (e: IOException)` wrap; the wording (including
    /// "test app" in parseAppPackageName) is marathon's and reaches logs.
    pub fn unable_to_parse_manifest(cause: DexError) -> Self {
        Self {
            kind: FailureKind::Runtime,
            message: Some("Unable to parse test app AndroidManifest.xml.".into()),
            cause: Some(Box::new(cause)),
        }
    }

    /// IOException-shaped failure (the only kind ApkParser catches/wraps).
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Io,
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn is_io(&self) -> bool {
        matches!(
            self.kind,
            FailureKind::Io | FailureKind::Zip | FailureKind::Eof | FailureKind::FileNotFound | FailureKind::NoSuchFile
        )
    }

    /// Opening a file with java.io semantics: a missing file is
    /// FileNotFoundException("<path> (No such file or directory)") with a
    /// NoSuchFileException cause on the reference JDK (probes.jsonl).
    pub fn open_file(path: &Path, err: &io::Error) -> Self {
        let shown = path.to_string_lossy();
        if err.kind() == io::ErrorKind::NotFound {
            return Self {
                kind: FailureKind::FileNotFound,
                message: Some(format!("{shown} (No such file or directory)").as_str().into()),
                cause: Some(Box::new(Self {
                    kind: FailureKind::NoSuchFile,
                    message: Some(shown.as_ref().into()),
                    cause: None,
                })),
            };
        }
        Self::io(format!("{shown} ({err})").as_str())
    }

    pub fn zip_exception(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Zip,
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
        write!(f, "{}", self.kind.to_owned())?;
        if let Some(message) = &self.message {
            write!(f, ": {}", message.to_owned())?;
        }
        if let Some(cause) = &self.cause {
            write!(f, " (cause: {cause})")?;
        }
        Ok(())
    }
}

impl std::error::Error for DexError {}

pub type DexResult<T> = Result<T, DexError>;
