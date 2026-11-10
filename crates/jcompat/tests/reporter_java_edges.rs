//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/bill/BillingReporter.kt (Java leaf semantics).
use jcompat::jtime::{instant_duration_millis, instant_of, instant_to_epoch_milli};
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
