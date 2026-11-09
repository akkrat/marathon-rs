//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/bill/BillingReporter.kt
//! and execution/command/parse/MarathonTestParseCommand.kt (Java leaf semantics).
use jcompat::{
    jtime::{instant_duration_millis, instant_of, instant_to_epoch_milli},
    writers::{JacksonYamlWriter, WriterValue},
};
use serde_json::Value;
#[test]
fn jackson_binary_scalars_match_the_command_mapper() {
    for line in include_str!("../../../fixtures/oracle/reporters/binary.jsonl").lines() {
        let r: Value = serde_json::from_str(line).unwrap();
        let size = r["size"].as_u64().unwrap() as usize;
        let value = WriterValue::Binary((0..size).map(|i| i as u8).collect());
        let value = if r["list"] == true {
            WriterValue::Array(vec![value])
        } else {
            WriterValue::Object(vec![("binary".into(), value)])
        };
        let bytes = |key: &str| {
            r[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            JacksonYamlWriter.write(&value).as_bytes().to_vec(),
            bytes("yaml"),
            "YAML size {size}, list {}",
            r["list"]
        );
    }
}
#[test]
fn billing_fractional_millis_keep_java_rounding_and_overflow() {
    let start = instant_of(1, 1).unwrap();
    let end = instant_of(0, 999999).unwrap();
    assert_eq!(instant_to_epoch_milli(start), Some(1000));
    assert_eq!(instant_to_epoch_milli(end), Some(0));
    assert_eq!(instant_duration_millis(start, end), Some(-999));
    assert_eq!(instant_to_epoch_milli(instant_of(-1, 999999999).unwrap()), Some(-1));
    assert_eq!(
        instant_to_epoch_milli(instant_of(i64::MAX / 1000 + 1, 0).unwrap()),
        None
    );
    assert_eq!(
        instant_duration_millis(
            instant_of(-10000000000000, 0).unwrap(),
            instant_of(10000000000000, 0).unwrap()
        ),
        Some(20000000000000000)
    );
}
