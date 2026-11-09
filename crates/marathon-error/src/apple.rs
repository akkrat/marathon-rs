//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/CommandException.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppleKind {
    NotAdmitted,
    NotBooted,
    TransferStalled,
    SetupDropped,
    Boot,
    Command,
    DeviceFailure,
}

impl AppleKind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::NotAdmitted => "Apple device not admitted",
            Self::NotBooted => "Apple device not booted",
            Self::TransferStalled => "Apple file transfer stalled",
            Self::SetupDropped => "Apple device setup dropped",
            Self::Boot => "Apple device boot failure",
            Self::Command => "Apple command failure",
            Self::DeviceFailure => "Apple device failure",
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::NotAdmitted => "apple.not-admitted",
            Self::NotBooted => "apple.not-booted",
            Self::TransferStalled => "apple.transfer-stalled",
            Self::SetupDropped => "apple.setup-dropped",
            Self::Boot => "apple.boot",
            Self::Command => "apple.command",
            Self::DeviceFailure => "apple.device-failure",
        }
    }
}

impl std::fmt::Display for AppleKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}
