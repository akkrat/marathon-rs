//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/exception/InstallException.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AndroidKind {
    TestAnnotationProducerNotFound,
    CommandRejected,
    DeviceNotSupported,
    Install,
}

impl AndroidKind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::TestAnnotationProducerNotFound => "Android test annotation producer not found",
            Self::CommandRejected => "Android command rejected",
            Self::DeviceNotSupported => "unsupported Android device",
            Self::Install => "Android installation failure",
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::TestAnnotationProducerNotFound => "android.test-annotation-producer-not-found",
            Self::CommandRejected => "android.command-rejected",
            Self::DeviceNotSupported => "android.device-not-supported",
            Self::Install => "android.install",
        }
    }
}

impl std::fmt::Display for AndroidKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}
