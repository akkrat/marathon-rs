//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/Target.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

/// The prefix that tells the adb server what a request is about (`SERVICES.TXT`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Target {
    /// `host:`: the server itself, or the only device.
    Host,
    /// `host-serial:<serial>:`: a specific device.
    Serial(String),
    /// `host-usb:`: the only USB device.
    Usb,
    /// `host-local:`: the only emulator.
    Local,
    /// No prefix, for requests sent after a transport switch.
    #[default]
    NonSpecified,
}

impl Target {
    pub fn serialize(&self) -> String {
        match self {
            Target::Host => "host:".into(),
            Target::Serial(serial) => format!("host-serial:{serial}:"),
            Target::Usb => "host-usb:".into(),
            Target::Local => "host-local:".into(),
            Target::NonSpecified => String::new(),
        }
    }
}

#[cfg(test)]
mod target_test {
    use super::*;

    #[test]
    fn test_serialization() {
        assert_eq!(Target::Serial("xxx".into()).serialize(), "host-serial:xxx:");
        assert_eq!(Target::Usb.serialize(), "host-usb:");
        assert_eq!(Target::Local.serialize(), "host-local:");
    }
}
