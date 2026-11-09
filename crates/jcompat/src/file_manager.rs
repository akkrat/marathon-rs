//! Port of core/src/main/kotlin/com/malinskiy/marathon/io/FileManager.kt,
//! io/FileType.kt, io/FolderType.kt, extension/StringExtensions.kt and json/FileSerializer.kt.

use std::{
    io,
    path::{Path, PathBuf},
};

/// Java's negated ASCII character class consumes a supplementary code point once.
pub fn escape(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '#' {
                c
            } else {
                '-'
            }
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
    pub path: PathBuf,
    pub directory: PathBuf,
    pub diagnostics: Vec<String>,
    best_effort_mkdirs: bool,
    folder: bool,
    invalid_empty_cut: bool,
    preflight: Vec<PathBuf>,
}
impl PathPlan {
    /// Blocking: call on an I/O executor. This creates directories, never the file.
    pub fn create_directories(&self) -> io::Result<()> {
        self.materialize(|_| {}).map(|_| ())
    }

    /// Folder diagnostics precede mkdir; file diagnostics follow directory creation.
    pub fn materialize(&self, mut log_error: impl FnMut(&str)) -> io::Result<PathBuf> {
        // A trim or absolute pool can hide invalid input from the eventual mkdir.
        for path in &self.preflight {
            if path.to_string_lossy().contains('\0') {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "Nul character not allowed"));
            }
        }
        let emit = |log_error: &mut dyn FnMut(&str)| {
            for message in &self.diagnostics {
                log_error(message);
            }
        };
        let invalid_cut = || io::Error::new(io::ErrorKind::InvalidInput, "invalid utf-16: lone surrogate found");
        if self.folder {
            emit(&mut log_error);
            if self.invalid_empty_cut {
                return Err(invalid_cut());
            }
        }
        if self.best_effort_mkdirs {
            // File.mkdirs returns false on failure; createTestResultFile ignores it.
            let _ = std::fs::create_dir_all(&self.directory);
        } else {
            std::fs::create_dir_all(&self.directory)?;
        }
        if !self.folder {
            emit(&mut log_error);
        }
        if self.invalid_empty_cut {
            return Err(invalid_cut());
        }
        Ok(self.path.clone())
    }
}

#[derive(Debug)]
pub struct FileManager {
    pub max_path: i32,
    pub max_filename: i32,
    output: PathBuf,
}

// Unix File/Path normalizes repeated slashes and trailing slashes, but retains dot components.
fn normalize(path: &str) -> PathBuf {
    let mut result = String::new();
    for c in path.chars() {
        if c != '/' || !result.ends_with('/') {
            result.push(c);
        }
    }
    while result.len() > 1 && result.ends_with('/') {
        result.pop();
    }
    PathBuf::from(result)
}
fn append(base: &Path, child: &str, resolve: bool) -> PathBuf {
    if resolve && child.starts_with('/') {
        return normalize(child);
    }
    let mut joined = base.to_string_lossy().into_owned();
    if !child.is_empty() {
        joined.push('/');
        joined.push_str(child);
    }
    normalize(&joined)
}

/// `new File(String).getPath()`: slash normalization without path validation or resolution.
pub fn file_path(path: &str) -> PathBuf {
    normalize(path)
}

/// FileSerializer writes this absolute path, without canonicalizing `.` or `..`.
pub fn absolute_file_path(path: &str, cwd: &str) -> PathBuf {
    if path.starts_with('/') {
        normalize(path)
    } else {
        append(Path::new(cwd), path, false)
    }
}

// Java's limits count UTF-16 units. D56 omits a character if its pair would be split.
fn cut(text: &str, limit: usize) -> &str {
    let mut units = 0;
    for (offset, c) in text.char_indices() {
        units += c.len_utf16();
        if units > limit {
            return &text[..offset];
        }
    }
    text
}

impl FileManager {
    /// `cwd` must be the process working directory.
    pub fn new(max_path: i32, max_filename: i32, output: &str, cwd: &str) -> Self {
        Self {
            max_path,
            max_filename,
            output: absolute_file_path(output, cwd),
        }
    }
    pub fn plan_file(&self, r: FileRequest<'_>) -> PathPlan {
        let mut directory = append(&append(&self.output, r.file_type.directory(), false), r.pool, false);
        if r.test_name.is_some() || r.batch_id.is_some() {
            directory = append(&directory, &escape(r.serial), true);
        }
        let mut filename = r
            .test_name
            .map(escape)
            .unwrap_or_else(|| r.batch_id.map(str::to_owned).unwrap_or_else(|| escape(r.serial)));
        let mut component = |value: Option<&str>| {
            if let Some(value) = value {
                filename.push('-');
                filename.push_str(value);
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
        filename.push('.');
        filename.push_str(if r.test_name.is_none() {
            r.batch_id.unwrap_or(r.file_type.suffix())
        } else {
            r.file_type.suffix()
        });
        if r.test_name.is_none()
            && r.batch_id.is_some()
            && let Some(chunk) = r.chunk
        {
            filename.push('-');
            filename.push_str(chunk);
        }
        self.finish_file(directory, filename)
    }
    pub fn plan_test_result_file(&self, filename: &str) -> PathPlan {
        let mut plan = self.finish_file(append(&self.output, "test_result", false), filename.to_owned());
        plan.best_effort_mkdirs = true;
        plan
    }
    fn finish_file(&self, directory: PathBuf, mut filename: String) -> PathPlan {
        let mut diagnostics = Vec::new();
        let mut invalid_empty_cut = false;
        if self.max_filename > 0 && filename.encode_utf16().count() > self.max_filename as usize {
            filename = cut(&filename, self.max_filename as usize).to_owned();
            // A later path cut can remove the invalid filename altogether, as in rc.1.
            invalid_empty_cut = filename.is_empty()
                && (self.max_path <= 0
                    || self.max_path as usize > directory.to_string_lossy().encode_utf16().count() + 1);
            diagnostics.push(format!("File name length cannot exceed {} characters and has been trimmed to {} and can create a conflict.This usually happens because the test name is too long.", self.max_filename, filename));
        }
        let (path, empty_path_component) =
            self.trim_path(append(&directory, &filename, false), false, &mut diagnostics);
        invalid_empty_cut |= empty_path_component;
        PathPlan {
            path,
            preflight: vec![directory.clone()],
            directory,
            diagnostics,
            best_effort_mkdirs: false,
            folder: false,
            invalid_empty_cut,
        }
    }
    pub fn plan_folder(&self, kind: FolderType, pool: Option<&str>, serial: Option<&str>) -> PathPlan {
        let mut path = append(&self.output, kind.directory(), false);
        let mut preflight = vec![path.clone()];
        if let Some(pool) = pool {
            path = append(&path, pool, true);
        }
        if let Some(serial) = serial {
            path = append(&path, &escape(serial), true);
        }
        let mut diagnostics = Vec::new();
        preflight.push(path.clone());
        let (path, invalid_empty_cut) = self.trim_path(path, true, &mut diagnostics);
        PathPlan {
            directory: path.clone(),
            path,
            diagnostics,
            best_effort_mkdirs: false,
            folder: true,
            invalid_empty_cut,
            preflight,
        }
    }
    fn trim_path(&self, path: PathBuf, folder: bool, diagnostics: &mut Vec<String>) -> (PathBuf, bool) {
        let text = path.to_string_lossy();
        if self.max_path <= 0 || text.encode_utf16().count() <= self.max_path as usize {
            return (path, false);
        }
        let trimmed = cut(&text, self.max_path as usize);
        let split_pair = trimmed.encode_utf16().count() < self.max_path as usize;
        let invalid_empty_cut = split_pair && (trimmed.is_empty() || trimmed.ends_with('/'));
        diagnostics.push(format!("{} path length cannot exceed {} characters and has been trimmed from {} to {}{}", if folder { "Directory" } else { "File" }, self.max_path, text, trimmed, if folder { " and can create a conflict. This happened because the combination of file path, pool name and device serial is too long." } else { " and can create a conflict. This happened because the combination of file path, test class name, and test name is too long." }));
        (normalize(trimmed), invalid_empty_cut)
    }
}

/// Unix `File(parent, child)` retains the parent even for a leading slash.
pub fn child_file(parent: &Path, child: &str) -> PathBuf {
    append(parent, child, false)
}

/// Kotlin File.relativePathTo: absolute lexical paths, normalized dot segments.
pub fn relative_file(path: &Path, base: &Path, cwd: &Path) -> String {
    fn segments(path: &Path, cwd: &Path) -> Vec<String> {
        let absolute = absolute_file_path(&path.to_string_lossy(), &cwd.to_string_lossy());
        let mut parts = Vec::new();
        for part in absolute.to_string_lossy().split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                _ => parts.push(part.to_owned()),
            }
        }
        parts
    }
    let target = segments(path, cwd);
    let base = segments(base, cwd);
    let common = target.iter().zip(&base).take_while(|(a, b)| a == b).count();
    std::iter::repeat_n("..".to_owned(), base.len() - common)
        .chain(target.into_iter().skip(common))
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    #[test]
    fn truncated_path_diagnostics_keep_only_complete_characters() {
        let plan = FileManager::new(0, 2, "/out", "/").plan_test_result_file("a😀z");
        assert_eq!(plan.path.as_os_str(), Path::new("/out/test_result/a").as_os_str());
        assert_eq!(
            plan.diagnostics,
            [
                "File name length cannot exceed 2 characters and has been trimmed to a and can create a conflict.This usually happens because the test name is too long."
            ]
        );
    }
}
