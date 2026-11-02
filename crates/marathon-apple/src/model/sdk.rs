//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/model/Sdk.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sdk {
    Iphoneos,
    IphoneSimulator,
    Tv,
    TvSimulator,
    Watch,
    WatchSimulator,
    Vision,
    VisionSimulator,
    Macos,
}
impl Sdk {
    pub const ALL: [Self; 9] = [
        Self::Iphoneos,
        Self::IphoneSimulator,
        Self::Tv,
        Self::TvSimulator,
        Self::Watch,
        Self::WatchSimulator,
        Self::Vision,
        Self::VisionSimulator,
        Self::Macos,
    ];
    /// The Kotlin constant name, which `toString()` and string templates produce.
    pub fn name(self) -> &'static str {
        match self {
            Self::Iphoneos => "IPHONEOS",
            Self::IphoneSimulator => "IPHONESIMULATOR",
            Self::Tv => "TV",
            Self::TvSimulator => "TV_SIMULATOR",
            Self::Watch => "WATCH",
            Self::WatchSimulator => "WATCH_SIMULATOR",
            Self::Vision => "VISION",
            Self::VisionSimulator => "VISION_SIMULATOR",
            Self::Macos => "MACOS",
        }
    }
    pub fn value(self) -> &'static str {
        match self {
            Self::Iphoneos => "iphoneos",
            Self::IphoneSimulator => "iphonesimulator",
            Self::Tv => "appletvos",
            Self::TvSimulator => "appletvsimulator",
            Self::Watch => "watchos",
            Self::WatchSimulator => "watchsimulator",
            Self::Vision => "xros",
            Self::VisionSimulator => "xrsimulator",
            Self::Macos => "macosx",
        }
    }
    pub fn platform_name(self) -> &'static str {
        match self {
            Self::Iphoneos => "iPhoneOS",
            Self::IphoneSimulator => "iPhoneSimulator",
            Self::Macos => "MacOSX",
            Self::Tv => "AppleTVOS",
            Self::TvSimulator => "AppleTVSimulator",
            Self::Watch => "WatchOS",
            Self::WatchSimulator => "WatchSimulator",
            Self::Vision => "XROS",
            Self::VisionSimulator => "XRSimulator",
        }
    }
    /// The `-destination` platform for xcodebuild.
    pub fn destination(self) -> &'static str {
        match self {
            Self::Iphoneos => "iOS",
            Self::IphoneSimulator => "iOS Simulator",
            Self::Macos => "OS X",
            Self::Tv => "tvOS",
            Self::TvSimulator => "tvOS Simulator",
            Self::Watch => "watchOS",
            Self::WatchSimulator => "watchOS Simulator",
            Self::Vision => "visionOS",
            Self::VisionSimulator => "visionOS Simulator",
        }
    }
}
impl std::fmt::Display for Sdk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
