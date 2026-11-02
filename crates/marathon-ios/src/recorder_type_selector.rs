//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/RecorderTypeSelector.kt
use marathon_config::vendor::{IosRecorderType, IosScreenRecordConfiguration};
use marathon_core::model::DeviceFeature;

pub fn select_recorder_type(
    supported: &[DeviceFeature],
    config: &IosScreenRecordConfiguration,
) -> Option<DeviceFeature> {
    let preferred = config.preferable_recorder_type.map(|p| match p {
        IosRecorderType::Video => DeviceFeature::Video,
        IosRecorderType::Screenshot => DeviceFeature::Screenshot,
    });
    let enabled = |feature| match feature {
        DeviceFeature::Video => config.video_configuration.enabled,
        DeviceFeature::Screenshot => config.screenshot_configuration.enabled,
    };
    preferred.filter(|p| supported.contains(p) && enabled(*p)).or_else(|| {
        [DeviceFeature::Video, DeviceFeature::Screenshot]
            .into_iter()
            .find(|p| supported.contains(p) && enabled(*p))
    })
}
