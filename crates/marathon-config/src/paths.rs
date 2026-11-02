//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/FileDeserializer.kt
//! canonicalFile resolves existing symlink prefixes even when the leaf does not exist.
use std::{
    io,
    path::{Component, Path, PathBuf},
};
pub(crate) fn canonical_file(path: &Path) -> io::Result<PathBuf> {
    let mut out = PathBuf::new();
    let last = path.components().count().saturating_sub(1);
    for (i, part) in path.components().enumerate() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            part => {
                out.push(part.as_os_str());
                match std::fs::canonicalize(&out) {
                    Ok(resolved) => out = resolved,
                    Err(e)
                        if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory)
                            || name_too_long(&e)
                            || (symlink_loop(&e) && (i == last || cfg!(target_os = "macos"))) => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }
    Ok(out)
}

// ELOOP differs by JDK (WP 2.4 R1, `tools/oracle/canonical-file-probe`).
// macOS (JBR 21.0.10) keeps the unresolved suffix wherever the loop is:
// `loop`, `loop/Marathondevices` and `a/x/y` (with `a`<->`b`) all stay as
// written. Linux (OpenJDK 21.0.12) accepts any errno from realpath of the
// whole path but only ENOENT/ENOTDIR/EACCES from shorter prefixes, so a loop
// at the leaf stays as written and a loop mid-path throws
// `IOException: Too many levels of symbolic links`.
fn symlink_loop(e: &io::Error) -> bool {
    #[cfg(target_os = "macos")]
    {
        e.raw_os_error() == Some(62)
    }
    #[cfg(not(target_os = "macos"))]
    {
        e.raw_os_error() == Some(40)
    }
}

// The JDK Unix canonicalizer retains an unresolved suffix on ENAMETOOLONG.
fn name_too_long(e: &io::Error) -> bool {
    #[cfg(target_os = "macos")]
    {
        e.raw_os_error() == Some(63)
    }
    #[cfg(not(target_os = "macos"))]
    {
        e.raw_os_error() == Some(36)
    }
}

#[cfg(test)]
mod tests {
    use super::canonical_file;
    use std::os::unix::fs::symlink;

    fn loops() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        symlink("loop", dir.join("loop")).unwrap();
        symlink("b", dir.join("a")).unwrap();
        symlink("a", dir.join("b")).unwrap();
        (tmp, dir)
    }

    // Witness: tools/oracle/canonical-file-probe on Ubuntu OpenJDK 21.0.12.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_keeps_a_symlink_loop_at_the_leaf() {
        let (_tmp, dir) = loops();
        assert_eq!(canonical_file(&dir.join("loop")).unwrap(), dir.join("loop"));
        assert_eq!(canonical_file(&dir.join("a")).unwrap(), dir.join("a"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_rejects_a_symlink_loop_mid_path() {
        let (_tmp, dir) = loops();
        for rel in ["loop/Marathondevices", "a/x/y", "loop/.."] {
            let e = canonical_file(&dir.join(rel)).unwrap_err();
            assert_eq!(e.raw_os_error(), Some(40), "{rel}");
        }
    }

    // Witness: the same probe on JBR 21.0.10.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keeps_every_symlink_loop() {
        let (_tmp, dir) = loops();
        for rel in ["loop", "loop/Marathondevices", "a/x/y", "a"] {
            assert_eq!(canonical_file(&dir.join(rel)).unwrap(), dir.join(rel), "{rel}");
        }
        assert_eq!(canonical_file(&dir.join("loop/..")).unwrap(), dir);
    }
}
