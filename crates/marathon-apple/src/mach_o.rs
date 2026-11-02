//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/model/AppleTestBundle.kt
//!
//! Only the Mach-O half of that file: the six magic constants and the `File.isAppleBinary`
//! prefix check, extracted for WP 4.4 to call (the file stays owned by 4.4 in the source
//! inventory; see `docs/wp/4.5a-findings.md`). Directory scanning, the extensionless-file
//! filter and the 0/1/many binary selection are 4.4's (WP 4.5a decision 1).

use std::io::{self, Read};

pub const FAT_MAGIC: [u8; 4] = [0xca, 0xfe, 0xba, 0xbe];
pub const FAT_CIGAM: [u8; 4] = [0xbe, 0xba, 0xfe, 0xca];
pub const MH_MAGIC: [u8; 4] = [0xfe, 0xed, 0xfa, 0xce];
pub const MH_CIGAM: [u8; 4] = [0xce, 0xfa, 0xed, 0xfe];
pub const MH_MAGIC_64: [u8; 4] = [0xfe, 0xed, 0xfa, 0xcf];
pub const MH_CIGAM_64: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];

const MAGICS: [[u8; 4]; 6] = [FAT_MAGIC, FAT_CIGAM, MH_MAGIC, MH_CIGAM, MH_MAGIC_64, MH_CIGAM_64];

/// A 4-byte prefix test against the fixed magic set. There is deliberately no 64-bit
/// fat magic (`ca fe ba bf`): a fat-64-only executable is not detected, exactly as in
/// Kotlin (kept under D5). Fewer than 4 bytes are not a binary.
pub fn is_apple_binary(bytes: &[u8]) -> bool {
    bytes.first_chunk::<4>().is_some_and(|header| MAGICS.contains(header))
}

/// Reads the first 4 bytes actually available (P7); a missing or unreadable file is the
/// caller's I/O error, and a short read is `false`, not Kotlin's single-`read()` hazard.
pub fn is_apple_binary_reader<R: Read>(mut reader: R) -> io::Result<bool> {
    let mut header = [0u8; 4];
    let mut read = 0;
    while read < 4 {
        let n = reader.read(&mut header[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    Ok(read == 4 && MAGICS.contains(&header))
}
