//! D97: a receive owns its private tree until it can publish the whole artifact.

use super::{ReceiveCheck, Received, failure, io_error};
use marathon_core::error::{Cancellation, VendorError, VendorResult};
use marathon_error::FailureKind;
use rustix::{
    fs::{CWD, RenameFlags, renameat_with},
    io::Errno,
};
use std::{
    fs, io,
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
        self.publish_with(stopped, |from, to, flags| renameat_with(CWD, from, CWD, to, flags))
    }

    /// U267: publishes the received content only if `check` accepts it where it lies, in this stage; a rejected
    /// content is discarded with the stage, leaving the destination as it was.
    ///
    /// The delivery must still be valid once the check is done, by the same test as without one: `lost` (the
    /// receive's connection, for the rsync bridge) is sampled before the check and again after it, and `stopped` after
    /// it and at the rename, as `publish` does. A stop that comes while the check runs therefore ends the receive as it
    /// would have just before it, whatever the check found, a failure to read the file included: a lost connection
    /// first (as before the rename without a check), then a cancellation, and only then the check's own result.
    pub(crate) fn publish_checked(
        self,
        check: Option<ReceiveCheck>,
        mut lost: impl FnMut() -> bool,
        stopped: impl Fn() -> Option<Cancellation>,
    ) -> VendorResult<Received> {
        if lost() {
            self.discard();
            return Ok(Received::NotReceived);
        }
        let Some(check) = check else {
            // Without a check the receive commits exactly as base did: one loss sample, then `publish`.
            return self.publish(&stopped).map(|()| Received::Published);
        };
        let verdict = check(&self.content);
        if lost() {
            self.discard();
            return Ok(Received::NotReceived);
        }
        let verdict = match verdict {
            Ok(Ok(())) => return self.publish(&stopped).map(|()| Received::Published),
            Ok(Err(why)) => Ok(Received::Rejected(why)),
            Err(error) => Err(io_error(error)),
        };
        self.discard();
        match stopped() {
            Some(reason) => Err(VendorError::Cancelled(reason)),
            None => verdict,
        }
    }

    fn publish_with(
        self,
        stopped: impl FnOnce() -> Option<Cancellation>,
        flagged_rename: impl FnOnce(&Path, &Path, RenameFlags) -> rustix::io::Result<()>,
    ) -> VendorResult<()> {
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
            match flagged_rename(&self.content, &self.destination, flags) {
                Err(Errno::INVAL | Errno::NOTSUP | Errno::NOSYS) if self.exclusive => {
                    rename_if_absent(&self.content, &self.destination)
                }
                Err(Errno::INVAL | Errno::NOTSUP | Errno::NOSYS) => self.replace_without_exchange(),
                result => result.map_err(io::Error::from),
            }
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

    fn replace_without_exchange(&self) -> io::Result<()> {
        let backup = tempfile::Builder::new()
            .prefix(".marathon-artifact-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(self.root.path().parent().unwrap_or(Path::new(".")))?;
        let old = backup.path().join("previous");
        fs::rename(&self.destination, &old)?;
        if let Err(publish) = rename_if_absent(&self.content, &self.destination) {
            if let Err(restore) = rename_if_absent(&old, &self.destination) {
                let recovery = backup.keep();
                return Err(io::Error::other(format!(
                    "{publish}; could not restore old artifact ({restore}); retained in {}",
                    recovery.display()
                )));
            }
            return Err(publish);
        }
        Ok(())
    }

    pub(crate) fn discard(self) {
        let _ = self.root.close();
    }
}

fn rename_if_absent(from: &Path, to: &Path) -> io::Result<()> {
    // Network filesystems can reject rename flags. As with the old move, another writer can
    // create the destination after this check and before the atomic plain rename.
    match fs::symlink_metadata(to) {
        Ok(_) => Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(from, to),
        Err(error) => Err(error),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsupported_exclusive_rename_publishes_a_new_bundle_but_keeps_a_winning_tree_d97() {
        for errno in [Errno::INVAL, Errno::NOTSUP, Errno::NOSYS] {
            let output = tempfile::tempdir().unwrap();
            let bundle = output.path().join("run.xcresult");
            for (payload, succeeds) in [(b"first", true), (b"later", false)] {
                let stage = ArtifactStage::new(bundle.clone(), true).unwrap();
                fs::create_dir(stage.content()).unwrap();
                fs::write(stage.content().join("payload"), payload).unwrap();
                let result = stage.publish_with(
                    || None,
                    |_, _, flags| {
                        assert_eq!(flags, RenameFlags::NOREPLACE);
                        Err(errno)
                    },
                );
                if succeeds {
                    result.unwrap();
                } else {
                    assert_eq!(result.unwrap_err().failure().kind, FailureKind::FileAlreadyExists);
                }
                assert_eq!(fs::read(bundle.join("payload")).unwrap(), b"first");
            }
            assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn an_unsupported_exchange_publishes_the_complete_merged_tree_d97() {
        let output = tempfile::tempdir().unwrap();
        let directory = output.path().join("logs");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("first"), b"first").unwrap();
        let stage = ArtifactStage::new(directory.clone(), false).unwrap();
        stage.seed_directory(&|| None).unwrap();
        fs::write(stage.content().join("later"), b"later").unwrap();
        stage
            .publish_with(
                || None,
                |_, _, flags| {
                    assert_eq!(flags, RenameFlags::EXCHANGE);
                    Err(Errno::NOTSUP)
                },
            )
            .unwrap();
        assert_eq!(fs::read(directory.join("first")).unwrap(), b"first");
        assert_eq!(fs::read(directory.join("later")).unwrap(), b"later");
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);

        let stage = ArtifactStage::new(directory.clone(), false).unwrap();
        stage.seed_directory(&|| None).unwrap();
        let result = stage.publish_with(
            || None,
            |from, _, _| {
                fs::rename(from, from.with_file_name("unavailable")).unwrap();
                Err(Errno::NOTSUP)
            },
        );
        assert_eq!(result.unwrap_err().failure().kind, FailureKind::NoSuchFile);
        assert_eq!(fs::read(directory.join("first")).unwrap(), b"first");
        assert_eq!(fs::read(directory.join("later")).unwrap(), b"later");
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
    }
}
