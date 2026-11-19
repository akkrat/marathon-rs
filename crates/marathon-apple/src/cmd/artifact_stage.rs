//! D97: a receive owns its private tree until it can publish the whole artifact.

use super::{failure, io_error};
use marathon_core::error::{Cancellation, VendorError, VendorResult};
use marathon_error::FailureKind;
use rustix::fs::{CWD, RenameFlags, renameat_with};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

pub(crate) struct ArtifactStage {
    root: tempfile::TempDir,
    destination: PathBuf,
    content: PathBuf,
    exclusive: bool,
}

impl ArtifactStage {
    pub(crate) fn new(destination: PathBuf, exclusive: bool) -> VendorResult<Self> {
        let name = destination
            .file_name()
            .ok_or_else(|| VendorError::Other(failure(FailureKind::FileSystem, destination.to_string_lossy())))?;
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let root = tempfile::Builder::new()
            .prefix(".marathon-artifact-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(parent)
            .map_err(io_error)?;
        let content = root.path().join(name);
        Ok(Self {
            root,
            destination,
            content,
            exclusive,
        })
    }

    /// Receive a named source into an existing directory. This gives both rsync
    /// variants the same destination layout for a directory source.
    pub(crate) fn rsync_directory(&mut self, source: &Path) -> VendorResult<PathBuf> {
        let name = source
            .file_name()
            .ok_or_else(|| VendorError::Other(failure(FailureKind::FileSystem, source.to_string_lossy())))?;
        self.content = self.root.path().join(name);
        Ok(self.root.path().to_owned())
    }

    pub(crate) fn content(&self) -> &Path {
        &self.content
    }

    /// Preserve the contents ordinary directory receives merge into.
    pub(crate) fn seed_directory(&self, stopped: &dyn Fn() -> Option<Cancellation>) -> VendorResult<()> {
        if !self.exclusive && self.destination.is_dir() {
            copy_tree(&self.destination, &self.content, stopped)?;
        }
        Ok(())
    }

    pub(crate) fn publish(self, stopped: impl FnOnce() -> Option<Cancellation>) -> VendorResult<()> {
        let replace_tree = !self.exclusive && self.content.is_dir() && self.destination.is_dir();
        // The stop is sampled by the owner at commit, on the blocking thread that renames.
        // Once admitted, the owner waits for rename and returns its receipt even if stop arrives.
        if let Some(reason) = stopped() {
            return Err(VendorError::Cancelled(reason));
        }
        let result = if self.exclusive || replace_tree {
            let flags = if self.exclusive {
                RenameFlags::NOREPLACE
            } else {
                RenameFlags::EXCHANGE
            };
            renameat_with(CWD, &self.content, CWD, &self.destination, flags).map_err(std::io::Error::from)
        } else {
            std::fs::rename(&self.content, &self.destination)
        };
        result.map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                VendorError::Other(failure(
                    FailureKind::FileAlreadyExists,
                    self.destination.to_string_lossy(),
                ))
            } else if error.kind() == std::io::ErrorKind::NotFound {
                VendorError::Other(failure(FailureKind::NoSuchFile, self.destination.to_string_lossy()))
            } else {
                io_error(error)
            }
        })
    }

    pub(crate) fn discard(self) {
        let _ = self.root.close();
    }
}

fn copy_tree(src: &Path, dst: &Path, stopped: &dyn Fn() -> Option<Cancellation>) -> VendorResult<()> {
    if let Some(reason) = stopped() {
        return Err(VendorError::Cancelled(reason));
    }
    let metadata = fs::symlink_metadata(src).map_err(io_error)?;
    if metadata.is_dir() {
        fs::create_dir(dst).map_err(io_error)?;
        for entry in fs::read_dir(src).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()), stopped)?;
        }
        fs::set_permissions(dst, metadata.permissions()).map_err(io_error)?;
    } else if metadata.is_symlink() {
        symlink(fs::read_link(src).map_err(io_error)?, dst).map_err(io_error)?;
    } else if metadata.is_file() {
        fs::copy(src, dst).map_err(io_error)?;
    } else {
        return Err(VendorError::Other(failure(
            FailureKind::FileSystem,
            src.to_string_lossy(),
        )));
    }
    Ok(())
}
