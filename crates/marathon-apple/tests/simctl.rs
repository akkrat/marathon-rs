//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/simctl/model/SimctlDeviceListDeserializerTest.kt

use marathon_apple::bin::xcrun::simctl::model::decode_list_devices_output;
use std::path::PathBuf;

#[test]
fn should_deserialize_properly() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vendor-apple-base/fixtures/simctl/list_output.json");
    let json = std::fs::read_to_string(path).unwrap();
    let devices_list = decode_list_devices_output(&json).unwrap();
    assert_eq!(devices_list.devices.devices.len(), 45);
}
