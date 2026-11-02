//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/extension/ByteArray.kt` and
//! `.../extension/Int.kt`. Only 6.2's sync requests use them.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

/// `ByteArray.toInt()`: four bytes, little-endian. Kotlin reads the first four of a longer array and throws
/// on a shorter one; the array type leaves the slicing to the caller.
pub fn to_int(bytes: &[u8; 4]) -> i32 {
    i32::from_le_bytes(*bytes)
}

/// `ByteArray.toUInt()`.
pub fn to_uint(bytes: &[u8; 4]) -> u32 {
    to_int(bytes) as u32
}

/// `ByteArray.toLong()`, with its bug kept (known bug N18). Each byte is shifted as a Java `int`, whose
/// shift distance is taken mod 32, so bytes 4-7 land on bits 0-31 again; each `int` term is then widened
/// with sign extension, so a set top bit in byte 3 or 7 fills the high half with ones.
pub fn to_long(bytes: &[u8; 8]) -> i64 {
    let term = |i: usize| -> i64 {
        let shifted = (i32::from(bytes[i]) & 0xFF).wrapping_shl(8 * i as u32);
        i64::from(shifted)
    };
    (0..8).fold(0i64, |acc, i| acc | term(i))
}

/// `ByteArray.toULong()`.
pub fn to_ulong(bytes: &[u8; 8]) -> u64 {
    to_long(bytes) as u64
}

/// `Int.toByteArray()`: big-endian.
pub fn int_to_byte_array(value: i32) -> [u8; 4] {
    value.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints() {
        assert_eq!(to_int(&[1, 2, 3, 4]), 0x0403_0201);
        assert_eq!(to_int(&[0xff, 0xff, 0xff, 0xff]), -1);
        assert_eq!(to_uint(&[0xff, 0xff, 0xff, 0xff]), u32::MAX);
        assert_eq!(int_to_byte_array(0x0102_0304), [1, 2, 3, 4]);
    }

    #[test]
    fn to_long_keeps_the_int_shift_bug() {
        // Little-endian 2^32 reads as 1: byte 4 is shifted by 32 mod 32 = 0.
        assert_eq!(to_long(&[0, 0, 0, 0, 1, 0, 0, 0]), 1);
        assert_eq!(to_long(&[1, 2, 0, 0, 0, 0, 0, 0]), 0x0201);
        // A top bit in byte 3 sign-extends: 0x80000000 becomes negative.
        assert_eq!(to_long(&[0, 0, 0, 0x80, 0, 0, 0, 0]), i64::from(i32::MIN));
        assert_eq!(to_ulong(&[0, 0, 0, 0x80, 0, 0, 0, 0]), 0xFFFF_FFFF_8000_0000);
    }
}
