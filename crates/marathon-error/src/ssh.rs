//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExecutor.kt

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
