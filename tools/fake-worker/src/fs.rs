//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/RemoteFileManager.kt
//! Harness filesystem semantics; all disk work is isolated from the async executor.
use crate::transcript::hash;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

pub const ROOT: &str = "/tmp/marathon";
#[derive(Clone)]
pub struct VirtualFs {
    root: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
    changed: Arc<tokio::sync::Notify>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: String,
    pub mode: u32,
    pub size: u64,
    pub sha256: Option<String>,
    pub link: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Snapshot(pub Vec<(Entry, Vec<u8>)>);
/// Held throughout a real rsync child's lifetime. Prevents filesystem handlers
/// from swapping a checked ancestor for a symlink while the child is using it.
pub struct Lease {
    pub root: PathBuf,
    _guard: OwnedMutexGuard<()>,
}
impl Lease {
    pub fn resolve(&self, remote: &str) -> Result<PathBuf> {
        resolve(&self.root, remote, false)
    }
}
impl VirtualFs {
    /// `root` must not already exist: reusing another run's state is an error.
    pub async fn create(root: PathBuf) -> Result<Self> {
        let root = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            fs::create_dir(&root)?;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
            Ok(root.canonicalize()?)
        })
        .await??;
        Ok(Self {
            root: Arc::new(root),
            lock: Arc::new(Mutex::new(())),
            changed: Arc::default(),
        })
    }
    pub async fn lease(&self) -> Lease {
        Lease {
            root: (*self.root).clone(),
            _guard: self.lock.clone().lock_owned().await,
        }
    }
    async fn op<T: Send + 'static>(&self, f: impl FnOnce(&Path) -> Result<T> + Send + 'static) -> Result<T> {
        let guard = self.lock.clone().lock_owned().await;
        let root = self.root.clone();
        let changed = self.changed.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            struct Wake(Arc<tokio::sync::Notify>);
            impl Drop for Wake {
                fn drop(&mut self) {
                    self.0.notify_waiters();
                }
            }
            let _wake = Wake(changed);
            f(&root)
        })
        .await?
    }
    /// Register before checking, so a write between the check and wait is not lost.
    pub async fn wait_for_file(&self, remote: &str) -> Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let guard = self.lock.clone().lock_owned().await;
            let root = self.root.clone();
            let path = remote.to_owned();
            let exists = tokio::task::spawn_blocking(move || {
                let _guard = guard;
                Ok::<_, anyhow::Error>(resolve(&root, &path, true)?.is_file())
            })
            .await??;
            if exists {
                return Ok(());
            }
            changed.await;
        }
    }
    pub async fn make_executable(&self, remote: &str) -> Result<()> {
        let path = remote.to_owned();
        self.op(move |root| {
            let path = resolve(root, &path, false)?;
            let mode = fs::metadata(&path)?.permissions().mode();
            fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o111))?;
            Ok(())
        })
        .await
    }
    pub async fn mkdir(&self, remote: &str) -> Result<()> {
        let p = remote.to_owned();
        self.op(move |r| {
            fs::create_dir_all(resolve(r, &p, false)?)?;
            Ok(())
        })
        .await
    }
    pub async fn read(&self, remote: &str) -> Result<Vec<u8>> {
        let p = remote.to_owned();
        self.op(move |r| Ok(fs::read(resolve(r, &p, true)?)?)).await
    }
    pub async fn write(&self, remote: &str, data: Vec<u8>, mode: u32) -> Result<()> {
        let p = remote.to_owned();
        self.op(move |r| {
            let path = resolve(r, &p, false)?;
            fs::write(&path, data)?;
            fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
            Ok(())
        })
        .await
    }
    /// Emulate the exact temporary-file/size/rename sequence without a shell.
    pub async fn write_file(&self, remote: &str, data: Vec<u8>, expected: usize) -> Result<()> {
        let p = remote.to_owned();
        self.op(move |r| {
            let dst = resolve_leaf(r, &p)?;
            let tmp = resolve(r, &format!("{p}.marathon-tmp"), false)?;
            let result = (|| -> Result<()> {
                fs::write(&tmp, &data)?;
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
                ensure!(data.len() == expected, "decoded size differs");
                fs::rename(&tmp, dst)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(tmp);
            }
            result
        })
        .await
    }
    pub async fn stat(&self, remote: &str) -> Result<Entry> {
        let p = remote.to_owned();
        self.op(move |r| entry(r, &resolve(r, &p, true)?.canonicalize()?, &p))
            .await
    }
    pub async fn list(&self, remote: &str) -> Result<Vec<Entry>> {
        let p = remote.trim_end_matches('/').to_owned();
        self.op(move |r| {
            let dir = resolve(r, &p, true)?;
            let mut entries = vec![];
            for item in fs::read_dir(dir)? {
                let item = item?;
                entries.push(entry(
                    r,
                    &item.path(),
                    &format!("{p}/{}", item.file_name().to_string_lossy()),
                )?);
            }
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(entries)
        })
        .await
    }
    pub async fn remove(&self, remote: &str) -> Result<()> {
        let p = remote.to_owned();
        self.op(move |r| {
            let path = resolve_leaf(r, &p)?;
            ensure!(path != r, "cannot remove virtual root; use reset");
            remove(&path)
        })
        .await
    }
    pub async fn rename(&self, src: &str, dst: &str) -> Result<()> {
        let (s, d) = (src.to_owned(), dst.to_owned());
        self.op(move |r| {
            let src = resolve_leaf(r, &s)?;
            let mut dst = resolve(r, &d, false)?;
            if dst.is_dir() {
                dst.push(src.file_name().ok_or_else(|| anyhow::anyhow!("root operand"))?);
            }
            ensure!(src != r && dst != r, "root operand");
            check(r, &dst, false)?;
            fs::rename(src, dst)?;
            Ok(())
        })
        .await
    }
    pub async fn copy(&self, src: &str, dst: &str) -> Result<()> {
        let (s, d) = (src.to_owned(), dst.to_owned());
        self.op(move |r| {
            let src = resolve_leaf(r, &s)?;
            let mut dst = resolve(r, &d, false)?;
            if dst.is_dir() {
                dst.push(src.file_name().ok_or_else(|| anyhow::anyhow!("root operand"))?);
            }
            ensure!(!dst.starts_with(&src), "recursive copy into source");
            copy(r, &src, &dst)
        })
        .await
    }
    /// Test-only bridge copies use the same resolver/lease as command-side copies.
    pub async fn import(&self, src: PathBuf, dst: &str) -> Result<()> {
        let dst = dst.to_owned();
        self.op(move |root| {
            let dst = resolve(root, &dst, false)?;
            copy(root, &src, &dst)
        })
        .await
    }
    pub async fn export(&self, src: &str, dst: PathBuf) -> Result<()> {
        let src = src.to_owned();
        self.op(move |root| {
            let src = resolve(root, &src, false)?;
            // Local output is explicitly supplied by the test, outside the virtual tree.
            copy_local(&src, &dst)
        })
        .await
    }
    pub async fn symlink(&self, src: &str, dst: &str) -> Result<()> {
        let (s, d) = (src.to_owned(), dst.to_owned());
        self.op(move |r| {
            let dst = resolve(r, &d, false)?;
            let parent = dst.parent().ok_or_else(|| anyhow::anyhow!("missing parent"))?;
            let target = if s.starts_with('/') {
                lexical(r, &s)?
            } else {
                normalize(parent.join(&s))?
            };
            ensure!(target.starts_with(r), "symlink escapes root");
            // Store the virtual target verbatim so rsync --links preserves it.
            // Reads resolve it ourselves; real children may never follow links.
            std::os::unix::fs::symlink(s, dst)?;
            Ok(())
        })
        .await
    }
    pub async fn snapshot(&self) -> Result<Snapshot> {
        self.op(move |r| {
            let mut entries = vec![];
            snapshot(r, r, &mut entries)?;
            Ok(Snapshot(entries))
        })
        .await
    }
    pub async fn reset(&self, snapshot: Snapshot) -> Result<()> {
        self.op(move |r| {
            for item in fs::read_dir(r)? {
                remove(&item?.path())?;
            }
            for (e, data) in snapshot.0 {
                let dst = resolve(r, &e.path, false)?;
                match e.kind.as_str() {
                    "directory" => fs::create_dir(&dst)?,
                    "file" => fs::write(&dst, data)?,
                    "symlink" => {
                        std::os::unix::fs::symlink(e.link.ok_or_else(|| anyhow::anyhow!("missing link"))?, dst)?;
                        continue;
                    }
                    _ => bail!("unsupported snapshot kind"),
                }
                fs::set_permissions(dst, fs::Permissions::from_mode(e.mode))?;
            }
            Ok(())
        })
        .await
    }
    /// Resolve a path field only, leaving the original plist bytes untouched.
    pub async fn resolve_xctestrun_path(
        &self,
        value: &str,
        test_root: &str,
        test_host: Option<&str>,
    ) -> Result<PathBuf> {
        let value = value.replace("__TESTROOT__", test_root);
        let value = if let Some(host) = test_host {
            value.replace("__TESTHOST__", host)
        } else {
            value
        };
        self.op(move |r| resolve(r, &value, true)).await
    }
}
fn normalize(path: PathBuf) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                ensure!(result.pop(), "path traversal");
            }
            Component::CurDir => (),
            _ => result.push(c.as_os_str()),
        }
    }
    Ok(result)
}
fn lexical(root: &Path, remote: &str) -> Result<PathBuf> {
    ensure!(!remote.as_bytes().contains(&0), "NUL path");
    let suffix = remote
        .strip_prefix(ROOT)
        .ok_or_else(|| anyhow::anyhow!("path outside {ROOT}"))?;
    ensure!(
        suffix.is_empty() || suffix.starts_with('/'),
        "path outside virtual root"
    );
    ensure!(
        !Path::new(suffix)
            .components()
            .any(|c| matches!(c, Component::ParentDir)),
        "parent traversal"
    );
    // Rebuild components: a trailing slash must not make lstat/unlink follow a leaf link.
    Ok(root.join(suffix.trim_start_matches('/')).components().collect())
}
fn resolve_leaf(root: &Path, remote: &str) -> Result<PathBuf> {
    let p = lexical(root, remote)?;
    if p != root {
        let parent = p.parent().ok_or_else(|| anyhow::anyhow!("missing parent"))?;
        check(root, parent, false)?;
    }
    Ok(p)
}
fn resolve(root: &Path, remote: &str, follow: bool) -> Result<PathBuf> {
    checked(root, &lexical(root, remote)?, follow, 0)
}
fn check(root: &Path, p: &Path, follow: bool) -> Result<()> {
    checked(root, p, follow, 0).map(|_| ())
}
fn checked(root: &Path, p: &Path, follow: bool, depth: usize) -> Result<PathBuf> {
    ensure!(depth < 40, "symlink cycle");
    let components = p.strip_prefix(root)?.components().collect::<Vec<_>>();
    let mut cursor = root.to_owned();
    for (index, c) in components.iter().enumerate() {
        cursor.push(c);
        match fs::symlink_metadata(&cursor) {
            Ok(m) if m.file_type().is_symlink() => {
                ensure!(follow, "symlink operand forbidden for writes/children");
                let link = fs::read_link(&cursor)?;
                let mut target = if link.is_absolute() {
                    lexical(
                        root,
                        link.to_str().ok_or_else(|| anyhow::anyhow!("non-UTF8 virtual link"))?,
                    )?
                } else {
                    normalize(
                        cursor
                            .parent()
                            .ok_or_else(|| anyhow::anyhow!("missing parent"))?
                            .join(link),
                    )?
                };
                ensure!(target.starts_with(root), "symlink escapes root");
                for remaining in &components[index + 1..] {
                    target.push(remaining);
                }
                return checked(root, &target, follow, depth + 1);
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(cursor)
}
fn remove(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p)?,
        Ok(_) => fs::remove_file(p)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
fn copy(root: &Path, src: &Path, dst: &Path) -> Result<()> {
    // Every recursive node (including an appended basename) is a new operand.
    check(root, dst, false)?;
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
    } else if meta.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy(root, &e.path(), &dst.join(e.file_name()))?;
        }
        fs::set_permissions(dst, meta.permissions())?;
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}
fn copy_local(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
    } else if meta.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_local(&e.path(), &dst.join(e.file_name()))?;
        }
        fs::set_permissions(dst, meta.permissions())?;
    } else {
        fs::copy(src, dst)?;
        fs::set_permissions(dst, meta.permissions())?;
    }
    Ok(())
}
fn entry(_root: &Path, p: &Path, remote: &str) -> Result<Entry> {
    let m = fs::symlink_metadata(p)?;
    let link = m.file_type().is_symlink();
    Ok(Entry {
        path: remote.into(),
        kind: if link {
            "symlink"
        } else if m.is_dir() {
            "directory"
        } else {
            "file"
        }
        .into(),
        mode: m.mode() & 0o7777,
        size: m.len(),
        sha256: if m.is_file() { Some(hash(&fs::read(p)?)) } else { None },
        link: if link {
            Some(fs::read_link(p)?.to_string_lossy().into())
        } else {
            None
        },
    })
}
fn snapshot(root: &Path, p: &Path, out: &mut Vec<(Entry, Vec<u8>)>) -> Result<()> {
    let mut children = fs::read_dir(p)?.collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(|e| e.file_name());
    for child in children {
        let p = child.path();
        let remote = format!("{ROOT}/{}", p.strip_prefix(root)?.display());
        let e = entry(root, &p, &remote)?;
        let dir = e.kind == "directory";
        let data = if e.kind == "file" { fs::read(&p)? } else { vec![] };
        out.push((e, data));
        if dir {
            snapshot(root, &p, out)?;
        }
    }
    Ok(())
}
