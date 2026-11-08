//! Port of core/src/main/kotlin/com/malinskiy/marathon/extension/StringExtensions.kt
//! and core/src/main/kotlin/com/malinskiy/marathon/execution/filter/FragmentationFilter.kt.

/// BigInteger(byte[]) interprets the digest as a signed, big-endian two's complement integer.
pub fn signed_md5(input: &str) -> i128 {
    i128::from_be_bytes(md5::compute(input.as_bytes()).0)
}

/// The Kotlin filter adds count once when the signed remainder is negative.
/// A zero divisor returns None (BigInteger.remainder throws ArithmeticException).
pub fn fragment_index(input: &str, count: i32) -> Option<i64> {
    fragment_index_for_hash(signed_md5(input), count)
}

pub fn fragment_index_for_hash(hash: i128, count: i32) -> Option<i64> {
    if count == 0 {
        return None;
    }
    let power = i128::from(count);
    // i128::MIN % -1 overflows in Rust; BigInteger returns zero.
    let remainder = if power == -1 { 0 } else { hash % power };
    Some((if remainder < 0 { remainder + power } else { remainder }) as i64)
}
