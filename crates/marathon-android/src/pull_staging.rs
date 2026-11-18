//! D92: private, same-filesystem staging for one Android run's pulls.

use crate::{services::AndroidRuntime, support::*};
use marathon_core::{error::*, executor::shared::requested};
use marathon_logging::Level;
use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::{DirBuilderExt, MetadataExt},
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

pub(crate) struct PullStaging {
    path: PathBuf,
    state: Mutex<State>,
    warned: AtomicBool,
}

impl PullStaging {
    pub(crate) fn new(output: &Path) -> Self {
        Self {
            path: output.join(DIRECTORY),
            state: Mutex::default(),
            warned: AtomicBool::new(false),
        }
    }

    pub(crate) async fn allocate(
        self: &Arc<Self>,
        runtime: &AndroidRuntime,
        destination: PathBuf,
    ) -> VendorResult<Option<PathBuf>> {
        let staging = self.clone();
        match blocking(runtime.dependencies.io.as_ref(), move || staging.reserve(&destination)).await {
            Ok(path) => Ok(Some(path)),
            Err(error @ VendorError::Cancelled(_)) => Err(error),
            Err(error) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    runtime.emit(
                        &runtime.component(module_path!()),
                        Level::WARN,
                        "Unable to stage Android pulls in the output directory. Writing directly to the destination.",
                        Some(error.failure()),
                    );
                }
                Ok(None)
            }
        }
    }

    fn reserve(&self, destination: &Path) -> VendorResult<PathBuf> {
        let mut state = lock(&self.state);
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
        if !on_filesystem(destination, filesystem).map_err(io)? {
            return Err(cross_device());
        }
        if matches!(*state, State::Unused) {
            // The core wipes the output before execution. Keep ownership of a directory
            // we created and leave any unexpected pre-existing entry to the output owner.
            fs::DirBuilder::new().mode(0o700).create(&self.path).map_err(io)?;
            *state = State::Ready(filesystem);
        }
        Ok(self.path.join(uuid::Uuid::new_v4().to_string()))
    }

    pub(crate) async fn close(self: &Arc<Self>, runtime: &AndroidRuntime) -> VendorResult<()> {
        let staging = self.clone();
        blocking(runtime.dependencies.io.as_ref(), move || {
            let previous = std::mem::replace(&mut *lock(&staging.state), State::Closed);
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

fn cross_device() -> VendorError {
    io(std::io::Error::new(
        ErrorKind::CrossesDevices,
        "the destination is on a different filesystem from Android pull staging",
    ))
}

fn on_filesystem(destination: &Path, filesystem: u64) -> std::io::Result<bool> {
    match fs::symlink_metadata(destination) {
        // Direct writes retain the original handling of symlinks and special files.
        Ok(metadata) if !metadata.is_file() => {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                "the pull destination is not a regular file",
            ));
        }
        Ok(metadata) if metadata.dev() != filesystem => return Ok(false),
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Check only existing ancestors. A rejected ADB handshake must leave a missing parent absent.
    for ancestor in destination.ancestors().skip(1) {
        let ancestor = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        };
        match fs::metadata(ancestor) {
            Ok(metadata) => return Ok(metadata.dev() == filesystem),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    fs::metadata(".").map(|metadata| metadata.dev() == filesystem)
}
