//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/Feature.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use std::fmt;

/// A service the device's adbd supports, as listed by `host-serial:<serial>:features`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    ShellV2,
    /// The `cmd` command is available, Android 24+.
    Cmd,
    StatV2,
    LsV2,
    Libusb,
    PushSync,
    Apex,
    FixedPushMkdir,
    Abb,
    FixedPushSymlinkTimestamp,
    AbbExec,
    RemountShell,
    TrackApp,
    SendrecvV2,
    SendrecvV2Brotli,
    SendrecvV2Lz4,
    SendrecvV2Zstd,
    SendrecvV2DryRunSend,
}

impl Feature {
    /// The Kotlin enum constant's `name`.
    pub fn name(self) -> &'static str {
        match self {
            Feature::ShellV2 => "SHELL_V2",
            Feature::Cmd => "CMD",
            Feature::StatV2 => "STAT_V2",
            Feature::LsV2 => "LS_V2",
            Feature::Libusb => "LIBUSB",
            Feature::PushSync => "PUSH_SYNC",
            Feature::Apex => "APEX",
            Feature::FixedPushMkdir => "FIXED_PUSH_MKDIR",
            Feature::Abb => "ABB",
            Feature::FixedPushSymlinkTimestamp => "FIXED_PUSH_SYMLINK_TIMESTAMP",
            Feature::AbbExec => "ABB_EXEC",
            Feature::RemountShell => "REMOUNT_SHELL",
            Feature::TrackApp => "TRACK_APP",
            Feature::SendrecvV2 => "SENDRECV_V2",
            Feature::SendrecvV2Brotli => "SENDRECV_V2_BROTLI",
            Feature::SendrecvV2Lz4 => "SENDRECV_V2_LZ4",
            Feature::SendrecvV2Zstd => "SENDRECV_V2_ZSTD",
            Feature::SendrecvV2DryRunSend => "SENDRECV_V2_DRY_RUN_SEND",
        }
    }

    /// `value()`: `name.lowercase(Locale.ENGLISH)`, which is the wire name.
    pub fn value(self) -> String {
        self.name().to_ascii_lowercase()
    }

    /// `Feature.of`: the wire name, or `None` for a name adam doesn't know.
    pub fn of(value: &str) -> Option<Feature> {
        Some(match value {
            "shell_v2" => Feature::ShellV2,
            "cmd" => Feature::Cmd,
            "stat_v2" => Feature::StatV2,
            "ls_v2" => Feature::LsV2,
            "libusb" => Feature::Libusb,
            "push_sync" => Feature::PushSync,
            "apex" => Feature::Apex,
            "fixed_push_mkdir" => Feature::FixedPushMkdir,
            "abb" => Feature::Abb,
            "fixed_push_symlink_timestamp" => Feature::FixedPushSymlinkTimestamp,
            "abb_exec" => Feature::AbbExec,
            "remount_shell" => Feature::RemountShell,
            "track_app" => Feature::TrackApp,
            "sendrecv_v2" => Feature::SendrecvV2,
            "sendrecv_v2_brotli" => Feature::SendrecvV2Brotli,
            "sendrecv_v2_lz4" => Feature::SendrecvV2Lz4,
            "sendrecv_v2_zstd" => Feature::SendrecvV2Zstd,
            "sendrecv_v2_dry_run_send" => Feature::SendrecvV2DryRunSend,
            _ => return None,
        })
    }
}

impl fmt::Display for Feature {
    /// Kotlin's enum `toString()`, the constant name, as `joinToString()` prints it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for name in [
            "shell_v2",
            "cmd",
            "stat_v2",
            "ls_v2",
            "libusb",
            "push_sync",
            "apex",
            "fixed_push_mkdir",
            "abb",
            "fixed_push_symlink_timestamp",
            "abb_exec",
            "remount_shell",
            "track_app",
            "sendrecv_v2",
            "sendrecv_v2_brotli",
            "sendrecv_v2_lz4",
            "sendrecv_v2_zstd",
            "sendrecv_v2_dry_run_send",
        ] {
            assert_eq!(Feature::of(name).map(Feature::value).as_deref(), Some(name));
        }
        assert_eq!(Feature::of("SHELL_V2"), None);
        assert_eq!(Feature::of("delayed_ack"), None);
    }
}
