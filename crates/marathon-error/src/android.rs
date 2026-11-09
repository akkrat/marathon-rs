//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/exception/InstallException.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AndroidKind {
    TestAnnotationProducerNotFound,
    CommandRejected,
    DeviceNotSupported,
    Install,
}

impl AndroidKind {
    pub const fn legacy_class(&self) -> &'static str {
        match self {
            Self::TestAnnotationProducerNotFound => {
                "com.malinskiy.marathon.android.adam.TestAnnotationProducerNotFoundException"
            }
            Self::CommandRejected => "com.malinskiy.marathon.android.exception.CommandRejectedException",
            Self::DeviceNotSupported => "com.malinskiy.marathon.android.exception.DeviceNotSupportedException",
            Self::Install => "com.malinskiy.marathon.android.exception.InstallException",
        }
    }
}
