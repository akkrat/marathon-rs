//! D92: private, same-filesystem staging for one Android run's pulls.

use crate::{services::AndroidRuntime, support::*};
use marathon_core::{
    error::*,
    executor::shared::{Signal, requested},
    runtime::CancellationSignal,
};
use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const DIRECTORY: &str = ".marathon-staging";

#[derive(Default)]
enum State {
    #[default]
    Unused,
    Ready(u64),
    Closed,
}

pub(crate) struct StagedPull {
    pub(crate) path: PathBuf,
    _private_dir: Option<tempfile::TempDir>,
}

pub(crate) struct PullStaging {
    path: PathBuf,
    state: Mutex<State>,
    sealed: AtomicBool,
}

impl PullStaging {
    pub(crate) fn new(output: &Path) -> Self {
        Self {
            path: output.join(DIRECTORY),
            state: Mutex::default(),
            sealed: AtomicBool::new(false),
        }
    }

    pub(crate) async fn allocate(
        self: &Arc<Self>,
        runtime: &AndroidRuntime,
        destination: PathBuf,
    ) -> VendorResult<StagedPull> {
        let staging = self.clone();
        blocking(runtime.dependencies.io.as_ref(), move || staging.reserve(&destination)).await
    }

    fn reserve(&self, destination: &Path) -> VendorResult<StagedPull> {
        let mut state = lock(&self.state);
        if self.sealed.load(Ordering::Acquire) {
            return Err(VendorError::Cancelled(requested("Android pull staging is closed")));
        }
        let filesystem = match *state {
            State::Closed => return Err(VendorError::Cancelled(requested("Android pull staging is closed"))),
            State::Ready(filesystem) => filesystem,
            State::Unused => {
                let output = self
                    .path
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                fs::metadata(output).map_err(io)?.dev()
            }
        };
        let (ancestor, destination_filesystem) = destination_filesystem(destination).map_err(io)?;
        if destination_filesystem != filesystem {
            let dir = tempfile::Builder::new()
                .prefix(".marathon-artifact-")
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in(ancestor)
                .map_err(io)?;
            return Ok(StagedPull {
                path: dir.path().join("download"),
                _private_dir: Some(dir),
            });
        }
        if matches!(*state, State::Unused) {
            // The core wipes the output before execution. Keep ownership of a directory
            // we created and leave any unexpected pre-existing entry to the output owner.
            fs::DirBuilder::new().mode(0o700).create(&self.path).map_err(io)?;
            *state = State::Ready(filesystem);
        }
        Ok(StagedPull {
            path: self.path.join(uuid::Uuid::new_v4().to_string()),
            _private_dir: None,
        })
    }

    pub(crate) fn seal(&self) {
        self.sealed.store(true, Ordering::Release);
    }

    pub(crate) async fn close(self: &Arc<Self>, runtime: &AndroidRuntime, stop: Arc<Signal>) -> VendorResult<()> {
        let staging = self.clone();
        blocking(runtime.dependencies.io.as_ref(), move || {
            let previous = std::mem::replace(&mut *lock(&staging.state), State::Closed);
            if let Some(reason) = stop.reason() {
                return Err(VendorError::Cancelled(reason));
            }
            if matches!(previous, State::Ready(_)) {
                return match fs::remove_dir_all(&staging.path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(io(error)),
                };
            }
            Ok(())
        })
        .await
    }
}

fn destination_filesystem(destination: &Path) -> std::io::Result<(&Path, u64)> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                "the pull destination is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // A rejected ADB handshake must leave a missing parent absent.
    for ancestor in destination.ancestors().skip(1) {
        let ancestor = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        };
        match fs::metadata(ancestor) {
            Ok(metadata) => return Ok((ancestor, metadata.dev())),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    fs::metadata(".").map(|metadata| (Path::new("."), metadata.dev()))
}
