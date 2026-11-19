use crate::cmd::{blocking, io_error};
use marathon_core::runtime::VendorRuntime;
use marathon_process::{Command, askpass};
use std::{
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::watch;

/// The transport string handed to rsync starts with `ssh`, so the version probe uses
/// that same PATH lookup. A timed-out probe drops its owned process group.
pub(super) async fn supports_forced_askpass(runtime: &Arc<dyn VendorRuntime>) -> bool {
    static SUPPORTS: OnceLock<watch::Receiver<Option<bool>>> = OnceLock::new();
    let mut supported = SUPPORTS
        .get_or_init(|| {
            let (finish, supported) = watch::channel(None);
            let probe_runtime = runtime.clone();
            // A cancelled transfer can stop waiting without cancelling the shared probe.
            runtime.spawn(Box::pin(async move {
                let probe = async {
                    let child = blocking(probe_runtime.as_ref(), || {
                        Command::new("ssh")
                            .arg("-V")
                            .stdin(Stdio::null())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped())
                            .spawn()
                            .map_err(io_error)
                    })
                    .await?;
                    child.wait_with_output().await.map_err(io_error)
                };
                let found = match tokio::time::timeout(Duration::from_secs(5), probe).await {
                    Ok(Ok(output)) if output.status.success() => {
                        supports_version(&String::from_utf8_lossy(&output.stderr))
                            || supports_version(&String::from_utf8_lossy(&output.stdout))
                    }
                    _ => false,
                };
                finish.send_replace(Some(found));
                Ok(())
            }));
            supported
        })
        .clone();
    supported
        .wait_for(Option::is_some)
        .await
        .is_ok_and(|value| *value == Some(true))
}

fn supports_version(output: &str) -> bool {
    output.lines().any(supports_version_line)
}

fn supports_version_line(line: &str) -> bool {
    let Some(version) = line.trim_start().strip_prefix("OpenSSH_") else {
        return false;
    };
    let Some((major, release)) = version.split_once('.') else {
        return false;
    };
    let minor = release.split(|c: char| !c.is_ascii_digit()).next().unwrap_or_default();
    matches!((major.parse::<u32>(), minor.parse::<u32>()), (Ok(major), Ok(minor)) if (major, minor) >= (8, 4))
}

pub(super) struct Helper {
    path: PathBuf,
    _directory: Option<TempDir>,
}

impl Helper {
    pub(super) fn new() -> io::Result<Self> {
        if let Some(path) = askpass::controller_executable()? {
            return Ok(Self { path, _directory: None });
        }
        let directory = tempfile::Builder::new().prefix("marathon-askpass-").tempdir()?;
        let path = directory.path().join("askpass");
        // An embedded caller may have no entry point. Keep the helper private and
        // keep the credential out of both the file and argv.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path)?;
        write!(
            file,
            r#"#!/bin/sh
[ "${{SSH_ASKPASS_PROMPT-}}" != confirm ] && [ "$#" -eq 1 ] || exit 1
case "$1" in
    *[Pp]assword:*|*[Pp]assword\ for\ *:*) printf '%s\n' "${{{}-}}" ;;
    *) exit 1 ;;
esac
"#,
            askpass::PASSWORD_ENV
        )?;
        Ok(Self {
            path,
            _directory: Some(directory),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_openssh_with_force_support_uses_askpass_d96() {
        for version in [
            "OpenSSH_8.4p1, LibreSSL",
            "OpenSSH_9.0",
            "OpenSSH_10.5p1",
            "warning: wrapper diagnostic\n OpenSSH_9.5p1",
        ] {
            assert!(supports_version(version));
        }
        for version in ["OpenSSH_8.3p9", "OpenSSH_9.", "OpenSSH_9.x", "another client 9.1", ""] {
            assert!(!supports_version(version));
        }
    }
}
