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
    Pem,
    Password,
    Pkcs,
}

impl SshKind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::KeyDecryptionFailed => "SSH key decryption failure",
            Self::Base64Decoding => "SSH base64 decoding failure",
            Self::Buffer => "SSH buffer failure",
            Self::CircularBuffer => "SSH circular buffer failure",
            Self::General => "SSH failure",
            Self::Connection => "SSH connection failure",
            Self::OpenChannel => "SSH channel opening failure",
            Self::Transport => "SSH transport failure",
            Self::Authentication => "SSH authentication failed",
            Self::Encryption => "SSH key encryption failure",
            Self::Pem => "SSH PEM decoding failure",
            Self::Password => "SSH key password failure",
            Self::Pkcs => "SSH PKCS decoding failure",
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::KeyDecryptionFailed => "ssh.key-decryption-failed",
            Self::Base64Decoding => "ssh.base64-decoding",
            Self::Buffer => "ssh.buffer",
            Self::CircularBuffer => "ssh.circular-buffer",
            Self::General => "ssh.general",
            Self::Connection => "ssh.connection",
            Self::OpenChannel => "ssh.open-channel",
            Self::Transport => "ssh.transport",
            Self::Authentication => "ssh.authentication",
            Self::Encryption => "ssh.encryption",
            Self::Pem => "ssh.pem",
            Self::Password => "ssh.password",
            Self::Pkcs => "ssh.pkcs",
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
            Self::Ssh(SshKind::Encryption) => Some(Self::Ssh(SshKind::Pem)),
            Self::Ssh(SshKind::Password) => Some(Self::Ssh(SshKind::Pem)),
            Self::Ssh(SshKind::Pem) => Some(Self::Io),
            Self::Ssh(SshKind::Pkcs) => Some(Self::Generic),
            Self::PatternSyntax => Some(Self::IllegalArgument),
            Self::IllegalArgument => Some(Self::Runtime),
            Self::Connect => Some(Self::Socket),
            Self::NoRouteToHost => Some(Self::Socket),
            Self::Bind => Some(Self::Socket),
            Self::PortUnreachable => Some(Self::Socket),
            Self::Socket => Some(Self::Io),
            Self::SocketTimeout => Some(Self::InterruptedIo),
            Self::InterruptedIo => Some(Self::Io),
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

impl std::fmt::Display for SshKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
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
            SshKind::Pem,
        ] {
            let kind = FailureKind::Ssh(kind);
            assert!(kind.is_ssh_instance(&FailureKind::Io));
            assert!(!kind.is_ssh_instance(&FailureKind::Ssh(SshKind::General)));
        }
        assert!(FailureKind::Ssh(SshKind::Pkcs).is_ssh_instance(&FailureKind::Generic));
        assert!(!FailureKind::Ssh(SshKind::Pkcs).is_ssh_instance(&FailureKind::Io));
        for kind in [SshKind::Buffer, SshKind::Base64Decoding] {
            assert!(!FailureKind::Ssh(kind).is_ssh_instance(&FailureKind::Ssh(SshKind::General)));
            assert!(!FailureKind::Ssh(kind).is_ssh_instance(&FailureKind::Io));
        }
        assert!(!FailureKind::Fatal(FatalKind::Io).is_ssh_instance(&FailureKind::Io));
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
        assert!(FailureKind::SocketTimeout.is_ssh_instance(&FailureKind::InterruptedIo));
        assert!(FailureKind::SocketTimeout.is_ssh_instance(&FailureKind::Io));
        assert!(!FailureKind::SocketTimeout.is_ssh_instance(&FailureKind::Socket));
        assert!(FailureKind::Timeout.is_ssh_instance(&FailureKind::Generic));
        assert!(!FailureKind::Timeout.is_ssh_instance(&FailureKind::Io));
        assert!(FailureKind::PatternSyntax.is_ssh_instance(&FailureKind::IllegalArgument));
        assert!(FailureKind::PatternSyntax.is_ssh_instance(&FailureKind::Runtime));
        assert!(FailureKind::IllegalState.is_ssh_instance(&FailureKind::Runtime));
    }
}
