//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:374

use super::*;
fn target(target: &AppleTarget) -> String {
    match target {
        AppleTarget::Host => "host".into(),
        AppleTarget::Simulator { udid } => format!("simulator {udid}"),
        AppleTarget::Physical { udid } => format!("device {udid}"),
        AppleTarget::SimulatorProfile(profile) => super::super::timing::profile_text(profile),
    }
}
pub(super) fn update_text(update: &TrackingUpdate) -> String {
    match update {
        TrackingUpdate::Connected(t) => format!("connected {}", target(t)),
        TrackingUpdate::Disconnected(t) => format!("disconnected {}", target(t)),
    }
}
