//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! JDK UTF-8 decoding at the File.readText boundary.
/// JDK UTF_8.Decoder replacement granularity, including a complete encoded
/// surrogate (ED A0 80) being ONE malformed sequence, unlike from_utf8_lossy.
pub fn decode_utf8(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 128 {
            out.push(b as char);
            i += 1;
            continue;
        }
        let n = match b {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 0,
        };
        if n == 0 {
            out.push('\u{fffd}');
            i += 1;
            continue;
        }
        let mut used = 1;
        while used < n && i + used < bytes.len() {
            let c = bytes[i + used];
            if c & 0xc0 != 0x80
                || (used == 1 && ((b == 0xe0 && c < 0xa0) || (b == 0xf0 && c < 0x90) || (b == 0xf4 && c > 0x8f)))
            {
                break;
            }
            used += 1;
        }
        if used != n {
            out.push('\u{fffd}');
            i += used;
            continue;
        }
        let mut cp = u32::from(b & (0x7f >> n));
        for c in &bytes[i + 1..i + n] {
            cp = (cp << 6) | u32::from(c & 63);
        }
        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
        i += n;
    }
    out
}
