//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandHost.kt

use super::auth::SshAuthentication;
use crate::cmd::CommandHost;

/// SSH coordinates. sshj's `remoteCharset` is UTF-8 unless reconfigured, and decoding is UTF-8 (D10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshCommandHost {
    pub addr: String,
    pub port: u16,
    pub authentication: SshAuthentication,
    pub host: CommandHost,
}
impl SshCommandHost {
    pub fn new(addr: &str, port: u16, authentication: SshAuthentication) -> Self {
        Self {
            host: CommandHost {
                id: format!("{}@{addr}:{port}", authentication.username()),
                charset: "UTF-8".into(),
            },
            addr: addr.into(),
            port,
            authentication,
        }
    }
    pub fn id(&self) -> &str {
        &self.host.id
    }
}
