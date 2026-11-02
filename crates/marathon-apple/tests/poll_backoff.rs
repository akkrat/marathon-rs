//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/extensions/PollBackoffTest.kt
use marathon_apple::extensions::poll_backoff::PollBackoff;
#[test]
fn delay_doubles_up_to_the_maximum_and_starts_over_after_a_reset() {
    let mut backoff = PollBackoff::new(1, 20).unwrap();
    assert_eq!(
        (0..7).map(|_| backoff.next_millis()).collect::<Vec<_>>(),
        [1, 2, 4, 8, 16, 20, 20]
    );
    backoff.reset();
    assert_eq!((0..3).map(|_| backoff.next_millis()).collect::<Vec<_>>(), [1, 2, 4]);
}
#[test]
fn delay_should_be_positive_and_not_above_the_maximum() {
    assert!(PollBackoff::new(0, 20).is_err());
    assert!(PollBackoff::new(30, 20).is_err());
}
