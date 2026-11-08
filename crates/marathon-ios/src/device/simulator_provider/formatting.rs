//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:374

use super::*;
use jcompat::writers::KotlinValue;
fn data(name: &str, field: &str, value: KotlinValue) -> KotlinValue {
    KotlinValue::DataClass {
        name: name.into(),
        fields: vec![(field.into(), value)],
    }
}
fn target(target: &AppleTarget) -> KotlinValue {
    match target {
        AppleTarget::Host => KotlinValue::from("Host"),
        AppleTarget::Simulator { udid } => data("Simulator", "udid", KotlinValue::Text(udid.clone())),
        AppleTarget::Physical { udid } => data("Physical", "udid", KotlinValue::Text(udid.clone())),
        AppleTarget::SimulatorProfile(profile) => KotlinValue::Text(super::super::timing::profile_text(profile)),
    }
}
pub(super) fn update_text(update: &TrackingUpdate) -> String {
    match update {
        TrackingUpdate::Connected(t) => data("Connected", "target", target(t)),
        TrackingUpdate::Disconnected(t) => data("Disconnected", "target", target(t)),
    }
    .render()
}
