//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/model/Simulator.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Creating,
    Shutdown,
    Booting,
    Booted,
    Unknown,
}
impl State {
    pub fn name(self) -> &'static str {
        match self {
            Self::Creating => "CREATING",
            Self::Shutdown => "SHUTDOWN",
            Self::Booting => "BOOTING",
            Self::Booted => "BOOTED",
            Self::Unknown => "UNKNOWN",
        }
    }
}
