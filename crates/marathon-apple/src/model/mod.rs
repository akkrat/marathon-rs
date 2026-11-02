//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/model/Arch.kt,
//! model/OS.kt and model/Platform.kt (the other model files are the submodules).

mod apple_operating_system_version;
mod apple_test_bundle;
mod sdk;
mod xcode_version;

pub use apple_operating_system_version::AppleOperatingSystemVersion;
pub use apple_test_bundle::AppleTestBundle;
pub use sdk::Sdk;
pub use xcode_version::XcodeVersion;

/// Exactly Kotlin's six constants, in declaration order.
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arch {
    armv7,
    armv7s,
    arm64,
    arm64e,
    i386,
    x86_64,
}
impl Arch {
    pub const ALL: [Self; 6] = [
        Self::armv7,
        Self::armv7s,
        Self::arm64,
        Self::arm64e,
        Self::i386,
        Self::x86_64,
    ];
    pub fn value(self) -> &'static str {
        match self {
            Self::armv7 => "armv7",
            Self::armv7s => "armv7s",
            Self::arm64 => "arm64",
            Self::arm64e => "arm64e",
            Self::i386 => "i386",
            Self::x86_64 => "x86_64",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Os {
    Ios,
    Watchos,
    Tvos,
}
impl Os {
    pub const ALL: [Self; 3] = [Self::Ios, Self::Watchos, Self::Tvos];
    pub fn name(self) -> &'static str {
        match self {
            Self::Ios => "IOS",
            Self::Watchos => "WATCHOS",
            Self::Tvos => "TVOS",
        }
    }
    pub fn value(self) -> &'static str {
        match self {
            Self::Ios => "iOS",
            Self::Watchos => "watchOS",
            Self::Tvos => "tvOS",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    IosDevice,
    IosSimulator,
}
impl Platform {
    pub const ALL: [Self; 2] = [Self::IosDevice, Self::IosSimulator];
    pub fn name(self) -> &'static str {
        match self {
            Self::IosDevice => "IOS_DEVICE",
            Self::IosSimulator => "IOS_SIMULATOR",
        }
    }
    pub fn value(self) -> &'static str {
        match self {
            Self::IosDevice => "ios_device",
            Self::IosSimulator => "ios_simulator",
        }
    }
}
