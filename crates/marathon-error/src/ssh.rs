//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExecutor.kt

use crate::FailureKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SshKind {
    KeyDecryptionFailed,
    Base64Decoding,
    Buffer,
    CircularBuffer,
    General,
    Connection,
    OpenChannel,
    Transport,
    Authentication,
    Encryption,
    PEM,
    Password,
    PKCS,
}

impl SshKind {
    pub const fn legacy_class(&self) -> &'static str {
        match self {
            Self::KeyDecryptionFailed => "com.hierynomus.sshj.common.KeyDecryptionFailedException",
            Self::Base64Decoding => "net.schmizz.sshj.common.Base64DecodingException",
            Self::Buffer => "net.schmizz.sshj.common.Buffer$BufferException",
            Self::CircularBuffer => "net.schmizz.sshj.common.CircularBuffer$CircularBufferException",
            Self::General => "net.schmizz.sshj.common.SSHException",
            Self::Connection => "net.schmizz.sshj.connection.ConnectionException",
            Self::OpenChannel => "net.schmizz.sshj.connection.channel.OpenFailException",
            Self::Transport => "net.schmizz.sshj.transport.TransportException",
            Self::Authentication => "net.schmizz.sshj.userauth.UserAuthException",
            Self::Encryption => "org.bouncycastle.openssl.EncryptionException",
            Self::PEM => "org.bouncycastle.openssl.PEMException",
            Self::Password => "org.bouncycastle.openssl.PasswordException",
            Self::PKCS => "org.bouncycastle.pkcs.PKCSException",
        }
    }
}

impl FailureKind {
    /// The existing SSH boundary catch graph deliberately leaves other inheritance unknown.
    fn ssh_parent(&self) -> Option<Self> {
        match self {
            Self::Ssh(SshKind::OpenChannel) => Some(Self::Ssh(SshKind::Connection)),
            Self::Ssh(SshKind::Connection) => Some(Self::Ssh(SshKind::General)),
            Self::Ssh(SshKind::CircularBuffer) => Some(Self::Ssh(SshKind::General)),
            Self::Ssh(SshKind::Transport) => Some(Self::Ssh(SshKind::General)),
            Self::Ssh(SshKind::Authentication) => Some(Self::Ssh(SshKind::General)),
            Self::Ssh(SshKind::General) => Some(Self::Io),
            Self::Ssh(SshKind::KeyDecryptionFailed) => Some(Self::Io),
            Self::Ssh(SshKind::Encryption) => Some(Self::Ssh(SshKind::PEM)),
            Self::Ssh(SshKind::Password) => Some(Self::Ssh(SshKind::PEM)),
            Self::Ssh(SshKind::PEM) => Some(Self::Io),
            Self::Ssh(SshKind::PKCS) => Some(Self::Generic),
            Self::PatternSyntax => Some(Self::IllegalArgument),
            Self::IllegalArgument => Some(Self::Runtime),
            Self::Connect => Some(Self::Socket),
            Self::NoRouteToHost => Some(Self::Socket),
            Self::Bind => Some(Self::Socket),
            Self::PortUnreachable => Some(Self::Socket),
            Self::Socket => Some(Self::Io),
            Self::SocketTimeout => Some(Self::InterruptedIO),
            Self::InterruptedIO => Some(Self::Io),
            Self::UnknownHost => Some(Self::Io),
            Self::FileNotFound => Some(Self::Io),
            Self::Io => Some(Self::Generic),
            Self::Timeout => Some(Self::Generic),
            Self::IllegalState => Some(Self::Runtime),
            Self::Runtime => Some(Self::Generic),
            _ => None,
        }
    }
    pub fn is_ssh_instance(&self, of: &Self) -> bool {
        if self == of {
            return true;
        }
        let mut current = self.ssh_parent();
        while let Some(kind) = current {
            if &kind == of {
                return true;
            }
            current = kind.ssh_parent();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FatalKind;
    #[test]
    fn ssh_catches_keep_subtypes_and_do_not_broaden_unrecorded_inheritance() {
        let open = FailureKind::Ssh(SshKind::OpenChannel);
        for parent in [
            FailureKind::Ssh(SshKind::Connection),
            FailureKind::Ssh(SshKind::General),
            FailureKind::Io,
            FailureKind::Generic,
        ] {
            assert!(open.is_ssh_instance(&parent));
        }
        for other in [FailureKind::Ssh(SshKind::Transport), FailureKind::Runtime] {
            assert!(!open.is_ssh_instance(&other));
        }
        for kind in [SshKind::Authentication, SshKind::Transport, SshKind::CircularBuffer] {
            let kind = FailureKind::Ssh(kind);
            assert!(kind.is_ssh_instance(&FailureKind::Ssh(SshKind::General)));
            assert!(kind.is_ssh_instance(&FailureKind::Io));
            assert!(!kind.is_ssh_instance(&FailureKind::Ssh(SshKind::Connection)));
        }
        for kind in [
            SshKind::KeyDecryptionFailed,
            SshKind::Encryption,
            SshKind::Password,
            SshKind::PEM,
        ] {
            let kind = FailureKind::Ssh(kind);
            assert!(kind.is_ssh_instance(&FailureKind::Io));
            assert!(!kind.is_ssh_instance(&FailureKind::Ssh(SshKind::General)));
        }
        assert!(FailureKind::Ssh(SshKind::PKCS).is_ssh_instance(&FailureKind::Generic));
        assert!(!FailureKind::Ssh(SshKind::PKCS).is_ssh_instance(&FailureKind::Io));
        for kind in [SshKind::Buffer, SshKind::Base64Decoding] {
            assert!(!FailureKind::Ssh(kind).is_ssh_instance(&FailureKind::Ssh(SshKind::General)));
            assert!(!FailureKind::Ssh(kind).is_ssh_instance(&FailureKind::Io));
        }
        assert!(!FailureKind::Fatal(FatalKind::IO).is_ssh_instance(&FailureKind::Io));
    }
    #[test]
    fn native_socket_and_argument_subtypes_keep_the_existing_catches() {
        for kind in [
            FailureKind::Connect,
            FailureKind::Bind,
            FailureKind::NoRouteToHost,
            FailureKind::PortUnreachable,
        ] {
            assert!(kind.is_ssh_instance(&FailureKind::Socket));
            assert!(kind.is_ssh_instance(&FailureKind::Io));
        }
        assert!(FailureKind::SocketTimeout.is_ssh_instance(&FailureKind::InterruptedIO));
        assert!(FailureKind::SocketTimeout.is_ssh_instance(&FailureKind::Io));
        assert!(!FailureKind::SocketTimeout.is_ssh_instance(&FailureKind::Socket));
        assert!(FailureKind::Timeout.is_ssh_instance(&FailureKind::Generic));
        assert!(!FailureKind::Timeout.is_ssh_instance(&FailureKind::Io));
        assert!(FailureKind::PatternSyntax.is_ssh_instance(&FailureKind::IllegalArgument));
        assert!(FailureKind::PatternSyntax.is_ssh_instance(&FailureKind::Runtime));
        assert!(FailureKind::IllegalState.is_ssh_instance(&FailureKind::Runtime));
    }
}
