//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/vendor/apple/AppleTestBundleConfiguration.kt
//!
//! `validate()` with its extraction side effects: branch selection,
//! `.ipa`/`.zip` extraction, `.app`/`.xctest` discovery and the resolved
//! paths. Blocking filesystem work; call it off the core executor
//! (docs/design/lifecycle.md), as WP 2.4's stage-(a) parse does.
//! Probe evidence: `fixtures/oracle/bundle-probe.jsonl`.
//!
//! Approved simplifications (docs/wp/2.3b.md): S1 rejects zip entries that
//! escape the extraction directory, lexically or through a symlink left
//! in a stale extraction tree, refuses an extraction dir that survives
//! its cleanup and never writes through an existing link; S4 sorts directory-search matches
//! (Kotlin's walk order is the filesystem's); S6 rejects an archive whose
//! stem is empty, `.` or `..` instead of deleting the parent or
//! grandparent directory; S7 makes `tempDirFor` a `validate` parameter,
//! not a stored field; S8 requires UTF-8 zip entry names. S2/S3 were
//! rejected: root-level entries land at `dst/<name>/<name>` and only
//! entries named exactly `__MACOSX` or `.DS_Store` are skipped, exactly
//! as in Kotlin. Review round 1 recorded five more differences on broken
//! or hostile inputs (docs/wp/2.3b-findings.md): failed `mkdirs()` and bad
//! CRCs are errors, duplicate entry names keep the last copy, a symlinked
//! extraction dir is unlinked rather than followed, and IO errors carry
//! one class, not a cause chain.

use crate::vendor::{AppleTestBundleConfiguration, ValidationError, absolute_path_text};
use jcompat::file_manager::JavaPath;
use std::{
    collections::HashMap,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, ValidationError>;

/// The extraction-dir factory: Kotlin's `tempDirFor` constructor parameter
/// (S7). It receives the archive path and returns the directory to extract
/// into, creating it as the Kotlin default does.
pub type TempDirFor<'a> = dyn FnMut(&JavaPath) -> Result<JavaPath> + 'a;

const ARCHIVE_EXTENSIONS: [&str; 2] = ["ipa", "zip"];
// Resource-fork junk, matched against the whole entry name (Kotlin IGNORED).
const IGNORED: [&str; 2] = ["__MACOSX", ".DS_Store"];

fn configuration(message: &str) -> ValidationError {
    ValidationError::configuration(message)
}

fn text(path: &JavaPath) -> String {
    String::from_utf16_lossy(&path.0)
}

fn io_error(error: io::Error) -> ValidationError {
    ValidationError::io(error.to_string())
}

fn path_buf(path: &JavaPath) -> Result<PathBuf> {
    path.to_path_buf().map_err(|e| ValidationError::io(e.to_string()))
}

fn java_path(path: &Path) -> JavaPath {
    JavaPath::from_text(&path.to_string_lossy())
}

/// `java.io.File.isFile` / `isDirectory` on the stored spelling: false on
/// any error, following symlinks.
fn is_file(path: &JavaPath) -> bool {
    path.to_path_buf().is_ok_and(|p| p.is_file())
}

fn is_directory(path: &JavaPath) -> bool {
    path.to_path_buf().is_ok_and(|p| p.is_dir())
}

/// Kotlin `File.extension`: after the last `.` of the name, `""` if none.
fn extension(path: &JavaPath) -> String {
    let name = name(path);
    name.rsplit_once('.').map_or("", |(_, ext)| ext).to_owned()
}

/// Kotlin `File.name`: after the last `/`.
fn name(path: &JavaPath) -> String {
    let spelling = text(path);
    spelling
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_owned()
}

/// Kotlin `File.nameWithoutExtension`: before the last `.`, the whole name
/// if there is no dot (so `.zip` gives `""`).
fn name_without_extension(file_name: &str) -> &str {
    match file_name.rfind('.') {
        Some(index) => &file_name[..index],
        None => file_name,
    }
}

/// The default `tempDirFor`: a sibling directory named after the archive
/// stem, deleted recursively then recreated. Kotlin ignores the results of
/// `deleteRecursively()` and `mkdirs()`; the port ignores the second only.
pub fn default_temp_dir_for(file: &JavaPath) -> Result<JavaPath> {
    let buf = path_buf(file)?;
    let file_name = name(file);
    let stem = name_without_extension(&file_name);
    if matches!(stem, "" | "." | "..") {
        // S6: Kotlin would delete, then extract into, the parent (stems ""
        // and ".") or the grandparent ("..") directory.
        return Err(configuration(&format!(
            "archive file name has no usable stem to derive an extraction directory from: {}",
            text(file)
        )));
    }
    let dst = match buf.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(stem),
        _ => PathBuf::from(stem),
    };
    let removed = match fs::symlink_metadata(&dst) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => fs::remove_dir_all(&dst),
        Ok(_) => fs::remove_file(&dst),
        Err(_) => Ok(()),
    };
    // S1: a destination that survives its cleanup (an r-x parent, a
    // locked subtree) may be a link out of the tree or hold one, so it is
    // an error here, where Kotlin extracts into whatever is left.
    if fs::symlink_metadata(&dst).is_ok() {
        let reason = removed
            .err()
            .map_or_else(|| "it still exists".to_owned(), |e| e.to_string());
        return Err(ValidationError::io(format!(
            "cannot clean the extraction directory {}: {reason}",
            dst.to_string_lossy()
        )));
    }
    let _ = fs::create_dir_all(&dst);
    Ok(java_path(&dst))
}

impl AppleTestBundleConfiguration {
    /// `validate()` with the Kotlin default extraction directory.
    pub fn validate(&mut self) -> Result<()> {
        self.validate_with_temp_dir_for(&mut default_temp_dir_for)
    }

    /// `validate()` (AppleTestBundleConfiguration.kt:33-83). On success
    /// `xctest` is always set; `app`/`test_app` follow the branch taken.
    /// Extraction side effects of an earlier step persist when a later
    /// step fails, exactly as in Kotlin.
    pub fn validate_with_temp_dir_for(&mut self, temp_dir_for: &mut TempDirFor<'_>) -> Result<()> {
        if let Some(xctestrun) = &self.xctestrun
            && !is_file(xctestrun)
        {
            return Err(configuration(&format!("xctestrun {} is not a file", text(xctestrun))));
        }
        if let (Some(application), Some(test_application)) = (self.application.clone(), self.test_application.clone()) {
            self.app = Some(resolve_application(&application, temp_dir_for)?);
            self.resolve_test_application(&test_application, temp_dir_for)?;
            Ok(())
        } else if let Some(derived) = self.derived_data_dir.clone() {
            self.xctest = Some(
                find_directory(&derived, "xctest", true)?
                    .ok_or_else(|| configuration("Unable to find xctest bundle"))?,
            );
            self.app = find_directory(&derived, "app", true)?;
            Ok(())
        } else {
            Err(configuration(
                "please specify your application and test application either with files or provide derived data folder",
            ))
        }
    }

    fn resolve_test_application(
        &mut self,
        test_application: &JavaPath,
        temp_dir_for: &mut TempDirFor<'_>,
    ) -> Result<()> {
        let ext = extension(test_application);
        if is_file(test_application) && ARCHIVE_EXTENSIONS.contains(&ext.as_str()) {
            let extracted = extract(test_application, temp_dir_for)?;
            // A found .app scopes the xctest search to itself; the rest of
            // the extraction is not searched (probe: bundle-probe
            // test-archive-app-scopes-xctest).
            let scope = match find_directory(&extracted, "app", false)? {
                Some(found) => {
                    self.test_app = Some(found.clone());
                    found
                }
                None => extracted,
            };
            self.xctest = Some(
                find_directory(&scope, "xctest", true)?.ok_or_else(|| configuration("Unable to find xctest bundle"))?,
            );
            Ok(())
        } else if is_directory(test_application) && ext == "app" {
            self.test_app = Some(test_application.clone());
            self.xctest = Some(
                find_directory(test_application, "xctest", true)?
                    .ok_or_else(|| configuration("Unable to find xctest bundle"))?,
            );
            Ok(())
        } else if is_directory(test_application) && ext == "xctest" {
            self.xctest = Some(test_application.clone());
            Ok(())
        } else {
            Err(configuration(
                "test application should be .ipa/.zip archive or a .app/.xctest folder",
            ))
        }
    }
}

fn resolve_application(application: &JavaPath, temp_dir_for: &mut TempDirFor<'_>) -> Result<JavaPath> {
    let ext = extension(application);
    if is_file(application) && ARCHIVE_EXTENSIONS.contains(&ext.as_str()) {
        let extracted = extract(application, temp_dir_for)?;
        // validate=true already threw when nothing was found, so the None
        // arm only mirrors Kotlin's nullable return into the `app` var.
        find_directory(&extracted, "app", true)?
            .ok_or_else(|| configuration("Unable to find an app directory in the extracted archive"))
    } else if is_directory(application) && ext == "app" {
        Ok(application.clone())
    } else {
        Err(configuration(
            "application should be .ipa/.zip archive or a .app folder",
        ))
    }
}

/// `findDirectoryInDirectory` (AppleTestBundleConfiguration.kt:86-114):
/// every directory (root included) whose extension matches. S4 sorts the
/// matches; Kotlin's `walkTopDown` order is the filesystem's.
fn find_directory(directory: &JavaPath, extension_name: &str, validate: bool) -> Result<Option<JavaPath>> {
    let mut found: Vec<JavaPath> = Vec::new();
    if let Ok(root) = directory.to_path_buf() {
        walk(&root, extension_name, &mut found);
    }
    found.sort_by_key(text);
    match found.len() {
        0 if validate => Err(configuration(&format!(
            "Unable to find an {extension_name} directory in {}",
            absolute_path_text(directory)
        ))),
        2 => {
            let (a, b) = (found[0].clone(), found[1].clone());
            // A watch app sits under X.app/Watch/Y.app; only X counts.
            if first_component_below(&b, &a).as_deref() == Some("Watch") {
                Ok(Some(b))
            } else if first_component_below(&a, &b).as_deref() == Some("Watch") {
                Ok(Some(a))
            } else {
                // Kotlin bug F19: both matches were removeFirst()-ed before
                // the message joins the list, so it always prints [].
                Err(ambiguous(extension_name, &[]))
            }
        }
        n if n > 2 => Err(ambiguous(extension_name, &found)),
        _ => Ok(found.into_iter().next()),
    }
}

fn ambiguous(extension_name: &str, found: &[JavaPath]) -> ValidationError {
    let list: Vec<String> = found.iter().map(absolute_path_text).collect();
    configuration(&format!(
        "Ambiguous {extension_name} configuration in derived data folder [{}]. Please specify parameters explicitly",
        list.join(", ")
    ))
}

/// `walkTopDown` without an `onFail` handler: a directory that cannot be
/// listed is still matched but not descended into, and unreadable entries
/// are skipped (probe `unreadable-dir-in-derived`).
fn walk(dir: &Path, extension_name: &str, found: &mut Vec<JavaPath>) {
    if !dir.is_dir() {
        return;
    }
    let as_java = java_path(dir);
    if extension(&as_java) == extension_name {
        found.push(as_java);
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, extension_name, found);
        }
    }
}

/// The first component of `to` relative to `from`, lexically (the walk
/// shares one root spelling, so no filesystem resolution is needed):
/// `Some("Watch")` when `to` is under `from/Watch/...`.
fn first_component_below(from: &JavaPath, to: &JavaPath) -> Option<String> {
    let from_parts: Vec<String> = text(from)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let to_parts: Vec<String> = text(to)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if to_parts.len() > from_parts.len() && to_parts[..from_parts.len()] == from_parts[..] {
        Some(to_parts[from_parts.len()].clone())
    } else {
        None
    }
}

/// `extract` (AppleTestBundleConfiguration.kt:121-149) over
/// `java.util.zip.ZipFile` semantics: central-directory order and names,
/// exact-name ignores, paths rebuilt from `/`-split segments with blank
/// segments dropped, regular files only (no modes, no symlinks).
fn extract(file: &JavaPath, temp_dir_for: &mut TempDirFor<'_>) -> Result<JavaPath> {
    let dst = temp_dir_for(file)?;
    let dst_buf = path_buf(&dst)?;
    let archive_path = path_buf(file)?;
    let open = || fs::File::open(&archive_path).map_err(|e| open_error(file, e));
    let mut zip = zip::ZipArchive::new(Patched::new(open()?, Vec::new())).map_err(zip_error)?;
    let mut headers = io::BufReader::new(open()?);
    let (entries, unicode_ids) = central_directory(&mut headers, zip.central_directory_start())?;
    if !unicode_ids.is_empty() {
        // The zip crate indexes entries by their 0x7075 name, so distinct
        // stored names sharing one collapse into one entry; ZipFile ignores
        // the field. Re-read the archive with those fields hidden.
        zip = zip::ZipArchive::new(Patched::new(open()?, unicode_ids)).map_err(zip_error)?;
    }
    // S1 containment: every path written is checked against the real
    // extraction dir, so a link left in a stale tree cannot redirect a
    // write outside it.
    let root = fs::canonicalize(&dst_buf).map_err(io_error)?;
    // The zip crate's index of each distinct stored name: an exact
    // duplicate shares the first copy's index and holds the last copy's
    // data (R1-sol-3, last wins).
    let mut indices: HashMap<&[u8], usize> = HashMap::new();
    for central in &entries {
        let next = indices.len();
        let index = *indices.entry(central.name.as_slice()).or_insert(next);
        let mut entry = zip.by_index(index).map_err(zip_error)?;
        if entry.central_header_start() != central.start && entry.name_raw() != central.name.as_slice() {
            return Err(ValidationError::zip("zip central directory entry cannot be located"));
        }
        // S8: entry names must be UTF-8.
        let entry_name = std::str::from_utf8(&central.name)
            .map_err(|_| ValidationError::zip("zip entry name is not valid UTF-8"))?;
        if IGNORED.contains(&entry_name) {
            continue;
        }
        // ZipEntry.isDirectory(): the name ends with '/'.
        if entry_name.ends_with('/') {
            let parts = contained_parts(&segments(entry_name), entry_name)?;
            create_dirs(&dst_buf, &root, &parts, entry_name)?;
        } else {
            // substringAfterLast/substringBeforeLast on "/": for a
            // root-level entry both give the whole name, so the file lands
            // at dst/<name>/<name> (probe root-entry; S2 rejected).
            let file_name = entry_name.rsplit('/').next().unwrap_or(entry_name);
            let parent_part = match entry_name.rfind('/') {
                Some(at) => &entry_name[..at],
                None => entry_name,
            };
            let mut parts = segments(parent_part);
            parts.push(file_name.to_owned());
            let parts = contained_parts(&parts, entry_name)?;
            let target = match parts.split_last() {
                Some((name, parents)) => create_dirs(&dst_buf, &root, parents, entry_name)?.join(name),
                None => dst_buf.clone(),
            };
            let mut out = create_file(&target)?;
            io::copy(&mut entry, &mut out).map_err(io_error)?;
        }
    }
    Ok(dst)
}

/// A central-directory record as `ZipFile` reads it: where it starts and
/// the stored name bytes.
struct CentralEntry {
    start: u64,
    name: Vec<u8>,
}

/// Walks the central directory from `start`, in order. Names are the
/// stored bytes, which `ZipFile` decodes as UTF-8, flagged or not; the
/// zip crate's own names fall back to CP437 without the flag and take the
/// Info-ZIP Unicode Path field (0x7075) instead, which Java ignores
/// (probes `utf8-unflagged-name`, `unicode-path-extra`). Also returns the
/// file position of every 0x7075 field's id, for [`Patched`]. The zip
/// crate has already validated the directory, so the walk just stops at
/// the first record that is not a central header.
fn central_directory(headers: &mut io::BufReader<fs::File>, start: u64) -> Result<(Vec<CentralEntry>, Vec<u64>)> {
    let (mut entries, mut unicode_ids) = (Vec::new(), Vec::new());
    let mut position = start;
    headers.seek(SeekFrom::Start(start)).map_err(io_error)?;
    loop {
        let mut fixed = [0u8; 46];
        match headers.read_exact(&mut fixed) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            other => other.map_err(io_error)?,
        }
        if fixed[..4] != [0x50, 0x4b, 0x01, 0x02] {
            break;
        }
        let le16 = |at: usize| usize::from(u16::from_le_bytes([fixed[at], fixed[at + 1]]));
        let (name_len, extra_len, comment_len) = (le16(28), le16(30), le16(32));
        let mut name = vec![0; name_len];
        headers.read_exact(&mut name).map_err(io_error)?;
        let mut extra = vec![0; extra_len];
        headers.read_exact(&mut extra).map_err(io_error)?;
        let extra_start = position + 46 + name_len as u64;
        let mut at = 0;
        while at + 4 <= extra.len() {
            if extra[at..at + 2] == [0x75, 0x70] {
                unicode_ids.push(extra_start + at as u64);
            }
            at += 4 + usize::from(u16::from_le_bytes([extra[at + 2], extra[at + 3]]));
        }
        headers.seek_relative(comment_len as i64).map_err(io_error)?;
        entries.push(CentralEntry { start: position, name });
        position = extra_start + (extra_len + comment_len) as u64;
    }
    Ok((entries, unicode_ids))
}

/// The archive as the zip crate is shown it: each 0x7075 extra-field id at
/// the given positions reads as 0xffff, an id it skips like `ZipFile`.
struct Patched {
    file: fs::File,
    position: u64,
    ids: Vec<u64>,
}

impl Patched {
    fn new(file: fs::File, ids: Vec<u64>) -> Self {
        Self { file, position: 0, ids }
    }
}

impl Read for Patched {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read(buf)?;
        let end = self.position + read as u64;
        let first = self.ids.partition_point(|&id| id + 2 <= self.position);
        for &id in self.ids[first..].iter().take_while(|&&id| id < end) {
            for byte in id..id + 2 {
                if (self.position..end).contains(&byte) {
                    buf[(byte - self.position) as usize] = 0xff;
                }
            }
        }
        self.position = end;
        Ok(read)
    }
}

impl Seek for Patched {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.position = self.file.seek(pos)?;
        Ok(self.position)
    }
}

/// `FileOutputStream(target)`, except that nothing is written through what
/// is already there: an existing file or link is unlinked and the file
/// created anew (O_EXCL), so a stale hard or symbolic link cannot carry
/// the write outside (S1). A failed unlink is an error.
fn create_file(target: &Path) -> Result<fs::File> {
    let not_found = |e: io::Error| ValidationError::file_not_found(format!("{} ({e})", target.to_string_lossy()));
    match fs::symlink_metadata(target) {
        // FileOutputStream throws FileNotFoundException on a directory;
        // a write-only open gives the same error without creating anything.
        Ok(metadata) if metadata.is_dir() => {
            return Err(match fs::OpenOptions::new().write(true).open(target) {
                Err(e) => not_found(e),
                Ok(_) => not_found(io::Error::other("Is a directory")),
            });
        }
        Ok(_) => fs::remove_file(target)
            .map_err(|e| ValidationError::io(format!("cannot replace {}: {e}", target.to_string_lossy())))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e)),
    }
    // FileOutputStream throws FileNotFoundException when it cannot open
    // the destination (an unwritable path).
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(not_found)
}

/// Creates `parts` under `dst` one directory at a time (Kotlin `mkdirs()`)
/// and returns the deepest. A component that already exists as a link is
/// followed only when it resolves inside `root`; one that exists as a
/// file is an error, where Kotlin ignores the failed `mkdirs()` (probe
/// `mkdirs-ignored`).
fn create_dirs(dst: &Path, root: &Path, parts: &[String], entry_name: &str) -> Result<PathBuf> {
    let mut current = dst.to_path_buf();
    for part in parts {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                let is_dir = if metadata.file_type().is_symlink() {
                    ensure_inside(&current, root, entry_name)?.is_dir()
                } else {
                    metadata.is_dir()
                };
                if !is_dir {
                    return Err(ValidationError::io(format!(
                        "cannot create directory {}: a file is in the way",
                        current.to_string_lossy()
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(io_error)?;
            }
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(current)
}

/// The real path of the link `path`, which must lie inside `root`; a
/// dangling link counts as escaping, since a write would create its target.
fn ensure_inside(path: &Path, root: &Path, entry_name: &str) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(real) if real.starts_with(root) => Ok(real),
        _ => Err(escapes(entry_name)),
    }
}

fn escapes(entry_name: &str) -> ValidationError {
    configuration(&format!("zip entry escapes the extraction directory: {entry_name}"))
}

/// `ZipFile` on a missing archive throws `NoSuchFileException` with the
/// path as its message (probe `hidden-archive-stem`).
fn open_error(file: &JavaPath, error: io::Error) -> ValidationError {
    if error.kind() == io::ErrorKind::NotFound {
        ValidationError::no_such_file(text(file))
    } else {
        io_error(error)
    }
}

fn zip_error(error: zip::result::ZipError) -> ValidationError {
    match error {
        zip::result::ZipError::Io(inner) => io_error(inner),
        other => ValidationError::zip(other.to_string()),
    }
}

/// Kotlin's `split("/").filter { it.trim().isNotEmpty() }`: original
/// (untrimmed) segments, blank ones dropped.
fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// Resolves `.`/`..` in `parts` lexically, relative to the extraction dir.
/// S1 (security fix, not reproduced from Kotlin): a path that would climb
/// out of it is rejected instead of written (known bug F21; probe
/// `zip-slip`).
fn contained_parts(parts: &[String], entry_name: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for part in parts {
        match part.as_str() {
            "." => {}
            ".." => {
                if out.pop().is_none() {
                    return Err(escapes(entry_name));
                }
            }
            other => out.push(other.to_owned()),
        }
    }
    Ok(out)
}
