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
    pub const fn legacy_class(&self) -> &'static str {
        match self {
            Self::NotAdmitted => "com.malinskiy.marathon.apple.ios.NotAdmitted",
            Self::NotBooted => "com.malinskiy.marathon.apple.ios.NotBooted",
            Self::TransferStalled => "com.malinskiy.marathon.apple.cmd.remote.rsync.TransferStalled",
            Self::SetupDropped => "com.malinskiy.marathon.apple.ios.device.SetupDropped",
            Self::Boot => "com.malinskiy.marathon.apple.ios.BootFailure",
            Self::Command => "com.malinskiy.marathon.apple.cmd.CommandException",
            Self::DeviceFailure => "com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException",
        }
    }
}
