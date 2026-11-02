//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/auth/SshAuthentication.kt

use super::{client::SshClient, error::SshFailure};
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq)]
pub enum SshAuthentication {
    Password { username: String, password: String },
    PublicKey { username: String, key: PathBuf },
}
impl std::fmt::Debug for SshAuthentication {
    // Kotlin's data class prints `password`; never let it reach a log here.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password { username, .. } => write!(f, "PasswordAuthentication(username={username})"),
            Self::PublicKey { username, key } => {
                write!(f, "PublicKeyAuthentication(username={username}, key={})", key.display())
            }
        }
    }
}
impl SshAuthentication {
    pub fn username(&self) -> &str {
        match self {
            Self::Password { username, .. } | Self::PublicKey { username, .. } => username,
        }
    }
    pub async fn authenticate<C: SshClient + ?Sized>(&self, client: &mut C) -> Result<(), SshFailure> {
        match self {
            Self::Password { username, password } => client.auth_password(username, password).await,
            Self::PublicKey { username, key } => {
                // File.absolutePath resolves against the working directory, not the worker.
                let path = std::path::absolute(key).unwrap_or_else(|_| key.clone());
                let keys = client.load_keys(&path).await?;
                client.auth_publickey(username, keys).await
            }
        }
    }
}
