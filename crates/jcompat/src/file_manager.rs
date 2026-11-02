//! Port of core/src/main/kotlin/com/malinskiy/marathon/io/FileManager.kt,
//! io/FileType.kt, io/FolderType.kt, extension/StringExtensions.kt and json/FileSerializer.kt.

use std::{io, path::PathBuf};

/// Java paths retain UTF-16 units even when a length limit splits a surrogate pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaPath(pub Vec<u16>);

impl JavaPath {
    pub fn from_text(text: &str) -> Self {
        Self(text.encode_utf16().collect())
    }

    /// Legacy File operations also reject malformed UTF-16 on the reference JDK.
    pub fn to_file_path_buf(&self) -> io::Result<PathBuf> {
        self.to_path_buf()
    }

    /// File.toPath uses the NIO encoder, which rejects malformed UTF-16 and NUL.
    pub fn to_path_buf(&self) -> io::Result<PathBuf> {
        let text = String::from_utf16(&self.0).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        if text.contains('\0') {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Nul character not allowed"));
        }
        Ok(PathBuf::from(text))
    }
}

/// Java's negated ASCII character class consumes a supplementary code point once.
pub fn escape(input: &str) -> String {
    // All output units are ASCII.
    escape_utf16(&input.encode_utf16().collect::<Vec<_>>())
        .into_iter()
        .map(|u| char::from(u as u8))
        .collect()
}

pub fn escape_utf16(input: &[u16]) -> Vec<u16> {
    char::decode_utf16(input.iter().copied())
        .map(|c| match c {
            Ok(c) if c.is_ascii_alphanumeric() || c == '.' || c == '#' => c as u16,
            _ => u16::from(b'-'),
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    Test,
    TestResult,
    Log,
    DeviceLog,
    DeviceInfo,
    Video,
    Screenshot,
    ScreenshotPng,
    ScreenshotJpg,
    ScreenshotWebp,
    ScreenshotGif,
    Xctestrun,
    Bill,
    Tracing,
}

impl FileType {
    pub fn directory(self) -> &'static str {
        match self {
            Self::Test => "tests",
            Self::TestResult => "test_result",
            Self::Log => "logs",
            Self::DeviceLog => "device-logs",
            Self::DeviceInfo => "devices",
            Self::Video => "video",
            Self::Screenshot
            | Self::ScreenshotPng
            | Self::ScreenshotJpg
            | Self::ScreenshotWebp
            | Self::ScreenshotGif => "screenshot",
            Self::Xctestrun => "xctestrun",
            Self::Bill => "bill",
            Self::Tracing => "profiling",
        }
    }
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Test => "xml",
            Self::TestResult | Self::DeviceInfo | Self::Bill => "json",
            Self::Log | Self::DeviceLog => "log",
            Self::Video => "mp4",
            Self::Screenshot => "gif",
            Self::ScreenshotPng => "png",
            Self::ScreenshotJpg | Self::ScreenshotWebp | Self::ScreenshotGif => "jpg",
            Self::Xctestrun => "xctestrun",
            Self::Tracing => "profile",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FolderType {
    DeviceFiles,
    Html,
}
impl FolderType {
    pub fn directory(self) -> &'static str {
        match self {
            Self::DeviceFiles => "device-files",
            Self::Html => "html",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FileRequest<'a> {
    pub file_type: FileType,
    pub pool: &'a str,
    pub serial: &'a str,
    /// Already formatted Test.toTestName(), before escaping.
    pub test_name: Option<&'a str>,
    pub batch_id: Option<&'a str>,
    pub id: Option<&'a str>,
    pub chunk: Option<&'a str>,
}

#[derive(Debug)]
pub struct PathPlan {
    pub path: JavaPath,
    pub directory: JavaPath,
    /// ERROR messages, in the order FileManager emits them. Keep UTF-16 for split surrogates.
    pub diagnostics: Vec<JavaPath>,
    best_effort_mkdirs: bool,
    folder: bool,
    preflight: Vec<JavaPath>,
}
impl PathPlan {
    /// Blocking: call on an I/O executor. This creates directories, never the file.
    /// Use materialize when the original ERROR log ordering matters.
    pub fn create_directories(&self) -> io::Result<()> {
        self.materialize(|_| {}).map(|_| ())
    }

    /// Performs Kotlin's directory side effects and emits the exact ERROR message units.
    /// Folder diagnostics precede mkdir; file diagnostics follow successful directory creation.
    pub fn materialize(&self, mut log_error: impl FnMut(&[u16])) -> io::Result<JavaPath> {
        for path in &self.preflight {
            path.to_path_buf()?;
        }
        let emit = |log_error: &mut dyn FnMut(&[u16])| {
            for message in &self.diagnostics {
                log_error(&message.0);
            }
        };
        if self.folder {
            emit(&mut log_error);
        }
        if self.best_effort_mkdirs {
            // File.mkdirs returns false on failure; createTestResultFile ignores it.
            if let Ok(path) = self.directory.to_file_path_buf() {
                let _ = std::fs::create_dir_all(path);
            }
        } else {
            std::fs::create_dir_all(self.directory.to_path_buf()?)?;
        }
        if !self.folder {
            emit(&mut log_error);
        }
        Ok(self.path.clone())
    }
}

#[derive(Debug)]
pub struct FileManager {
    pub max_path: i32,
    pub max_filename: i32,
    output: JavaPath,
}

// Unix File/Path normalizes repeated slashes and trailing slashes, but retains dot components.
fn normalize(path: &[u16]) -> JavaPath {
    let mut result = Vec::new();
    for &unit in path {
        if unit != 47 || result.last() != Some(&47) {
            result.push(unit);
        }
    }
    while result.len() > 1 && result.last() == Some(&47) {
        result.pop();
    }
    JavaPath(result)
}
fn append_units(base: &JavaPath, child: &[u16], resolve: bool) -> JavaPath {
    if resolve && child.first() == Some(&47) {
        return normalize(child);
    }
    let mut units = base.0.clone();
    if !child.is_empty() {
        units.push(47);
        units.extend(child);
    }
    normalize(&units)
}
fn append(base: &JavaPath, child: &str, resolve: bool) -> JavaPath {
    append_units(base, &child.encode_utf16().collect::<Vec<_>>(), resolve)
}

/// `new File(String).getPath()`: slash normalization without path validation or resolution.
pub fn file_path(path: &str) -> JavaPath {
    normalize(&path.encode_utf16().collect::<Vec<_>>())
}

/// FileSerializer writes this absolute path, without canonicalizing `.` or `..`.
pub fn absolute_file_path(path: &str, cwd: &str) -> JavaPath {
    absolute_file_path_utf16(
        &path.encode_utf16().collect::<Vec<_>>(),
        &cwd.encode_utf16().collect::<Vec<_>>(),
    )
}
pub fn absolute_file_path_utf16(path: &[u16], cwd: &[u16]) -> JavaPath {
    if path.first() == Some(&47) {
        normalize(path)
    } else {
        append_units(&JavaPath(cwd.to_vec()), path, false)
    }
}

/// Raw Java strings for callers that retain malformed UTF-16 from configuration.
#[derive(Clone, Copy, Debug)]
pub struct Utf16FileRequest<'a> {
    pub file_type: FileType,
    pub pool: &'a [u16],
    pub serial: &'a [u16],
    pub test_name: Option<&'a [u16]>,
    pub batch_id: Option<&'a [u16]>,
    pub id: Option<&'a [u16]>,
    pub chunk: Option<&'a [u16]>,
}

impl FileManager {
    /// `cwd` must be the process working directory. Kotlin resolves `output.absolutePath` against the
    /// live `user.dir` on every call; this resolves it once, which is identical only for that `cwd`.
    pub fn new(max_path: i32, max_filename: i32, output: &str, cwd: &str) -> Self {
        Self {
            max_path,
            max_filename,
            output: absolute_file_path(output, cwd),
        }
    }

    pub fn new_utf16(max_path: i32, max_filename: i32, output: &[u16], cwd: &[u16]) -> Self {
        Self {
            max_path,
            max_filename,
            output: absolute_file_path_utf16(output, cwd),
        }
    }

    pub fn plan_file(&self, r: FileRequest<'_>) -> PathPlan {
        let pool = JavaPath::from_text(r.pool);
        let serial = JavaPath::from_text(r.serial);
        let test = r.test_name.map(JavaPath::from_text);
        let batch = r.batch_id.map(JavaPath::from_text);
        let id = r.id.map(JavaPath::from_text);
        let chunk = r.chunk.map(JavaPath::from_text);
        self.plan_file_utf16(Utf16FileRequest {
            file_type: r.file_type,
            pool: &pool.0,
            serial: &serial.0,
            test_name: test.as_ref().map(|s| s.0.as_slice()),
            batch_id: batch.as_ref().map(|s| s.0.as_slice()),
            id: id.as_ref().map(|s| s.0.as_slice()),
            chunk: chunk.as_ref().map(|s| s.0.as_slice()),
        })
    }

    pub fn plan_file_utf16(&self, r: Utf16FileRequest<'_>) -> PathPlan {
        let mut directory = append_units(&append(&self.output, r.file_type.directory(), false), r.pool, false);
        if r.test_name.is_some() || r.batch_id.is_some() {
            directory = append_units(&directory, &escape_utf16(r.serial), true);
        }
        let mut filename = r.test_name.map(escape_utf16).unwrap_or_else(|| {
            r.batch_id
                .map(<[u16]>::to_vec)
                .unwrap_or_else(|| escape_utf16(r.serial))
        });
        let mut component = |value: Option<&[u16]>| {
            if let Some(value) = value {
                filename.push(45);
                filename.extend(value);
            }
        };
        if r.test_name.is_some() {
            component(r.batch_id);
        }
        component(r.id);
        if r.test_name.is_some() {
            component(r.chunk);
        }
        // C1: batch ID, not the file type's suffix.
        filename.push(46);
        let suffix = JavaPath::from_text(r.file_type.suffix());
        filename.extend(if r.test_name.is_none() {
            r.batch_id.unwrap_or(&suffix.0)
        } else {
            &suffix.0
        });
        if r.test_name.is_none()
            && r.batch_id.is_some()
            && let Some(chunk) = r.chunk
        {
            filename.push(45);
            filename.extend(chunk);
        }
        self.finish_file(directory, JavaPath(filename))
    }

    pub fn plan_test_result_file(&self, filename: &str) -> PathPlan {
        self.plan_test_result_file_utf16(&filename.encode_utf16().collect::<Vec<_>>())
    }

    pub fn plan_test_result_file_utf16(&self, filename: &[u16]) -> PathPlan {
        let mut plan = self.finish_file(append(&self.output, "test_result", false), JavaPath(filename.to_vec()));
        plan.best_effort_mkdirs = true;
        plan
    }

    fn finish_file(&self, directory: JavaPath, mut filename: JavaPath) -> PathPlan {
        let mut diagnostics = Vec::new();
        if self.max_filename > 0 && filename.0.len() > self.max_filename as usize {
            filename.0.truncate(self.max_filename as usize);
            let mut message = JavaPath::from_text(&format!(
                "File name length cannot exceed {} characters and has been trimmed to ",
                self.max_filename
            ));
            message.0.extend(&filename.0);
            message.0.extend(
                " and can create a conflict.This usually happens because the test name is too long.".encode_utf16(),
            );
            diagnostics.push(message);
        }
        let mut joined = directory.0.clone();
        if !filename.0.is_empty() {
            joined.push(47);
            joined.extend(filename.0);
        }
        let path = self.trim_path(normalize(&joined), false, &mut diagnostics);
        PathPlan {
            path,
            preflight: vec![directory.clone()],
            directory,
            diagnostics,
            best_effort_mkdirs: false,
            folder: false,
        }
    }

    pub fn plan_folder(&self, kind: FolderType, pool: Option<&str>, serial: Option<&str>) -> PathPlan {
        let pool = pool.map(JavaPath::from_text);
        let serial = serial.map(JavaPath::from_text);
        self.plan_folder_utf16(
            kind,
            pool.as_ref().map(|s| s.0.as_slice()),
            serial.as_ref().map(|s| s.0.as_slice()),
        )
    }

    pub fn plan_folder_utf16(&self, kind: FolderType, pool: Option<&[u16]>, serial: Option<&[u16]>) -> PathPlan {
        let mut path = append(&self.output, kind.directory(), false);
        let mut preflight = vec![path.clone()];
        if let Some(pool) = pool {
            path = append_units(&path, pool, true);
        }
        if let Some(serial) = serial {
            path = append_units(&path, &escape_utf16(serial), true);
        }
        let mut diagnostics = Vec::new();
        preflight.push(path.clone());
        let path = self.trim_path(path, true, &mut diagnostics);
        PathPlan {
            directory: path.clone(),
            path,
            diagnostics,
            best_effort_mkdirs: false,
            folder: true,
            preflight,
        }
    }

    fn trim_path(&self, path: JavaPath, folder: bool, diagnostics: &mut Vec<JavaPath>) -> JavaPath {
        if self.max_path <= 0 || path.0.len() <= self.max_path as usize {
            return path;
        }
        let trimmed = JavaPath(path.0[..self.max_path as usize].to_vec());
        let mut message = JavaPath::from_text(&format!(
            "{} path length cannot exceed {} characters and has been trimmed from ",
            if folder { "Directory" } else { "File" },
            self.max_path
        ));
        message.0.extend(&path.0);
        message.0.extend(" to ".encode_utf16());
        message.0.extend(&trimmed.0);
        message.0.extend(if folder {
            " and can create a conflict. This happened because the combination of file path, pool name and device serial is too long."
        } else {
            " and can create a conflict. This happened because the combination of file path, test class name, and test name is too long."
        }.encode_utf16());
        diagnostics.push(message);
        normalize(&trimmed.0)
    }
}

/// Unix `File(parent, child)` retains the parent even for a leading slash.
pub fn child_file(parent: &JavaPath, child: &[u16]) -> JavaPath {
    append_units(parent, child, false)
}

/// Kotlin File.relativePathTo: absolute lexical paths, normalized dot segments.
pub fn relative_file(path: &JavaPath, base: &JavaPath, cwd: &JavaPath) -> crate::writers::JavaString {
    fn segments(path: &JavaPath, cwd: &JavaPath) -> Vec<Vec<u16>> {
        let absolute = absolute_file_path_utf16(&path.0, &cwd.0);
        let mut parts = Vec::new();
        for part in absolute.0.split(|u| *u == 47) {
            match part {
                [] | [46] => {}
                [46, 46] => {
                    parts.pop();
                }
                _ => parts.push(part.to_vec()),
            }
        }
        parts
    }
    let target = segments(path, cwd);
    let base = segments(base, cwd);
    let common = target.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let parts = std::iter::repeat_n(vec![46, 46], base.len() - common).chain(target.into_iter().skip(common));
    crate::writers::JavaString(parts.collect::<Vec<_>>().join(&47))
}
