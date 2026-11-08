//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/PropertyList.kt

fn number(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    bytes
        .iter()
        .try_fold(0usize, |n, b| n.checked_mul(256)?.checked_add(usize::from(*b)))
}

/// D55: replace lone units in binary plist string objects before the strict
/// plist decoder runs. Replacements have the same width, so object offsets and
/// lengths remain intact; data objects are deliberately never interpreted.
pub(super) fn decode(bytes: &mut [u8]) {
    // Leave malformed layouts to the existing decoder; this boundary must not
    // introduce new validation rules or partially rewrite structural bytes.
    if let Some(replacements) = lone_units(bytes) {
        for at in replacements {
            bytes[at..at + 2].copy_from_slice(&0xfffdu16.to_be_bytes());
        }
    }
}

fn lone_units(bytes: &[u8]) -> Option<Vec<usize>> {
    let mut replacements = Vec::new();
    let trailer_start = bytes.len().checked_sub(32)?;
    let trailer = bytes.get(trailer_start..)?;
    let width = usize::from(trailer[6]);
    let count = number(&trailer[8..16])?;
    let table = number(&trailer[24..32])?;
    if width == 0 || width > 8 || table < 8 || table.checked_add(count.checked_mul(width)?)? > trailer_start {
        return None;
    }
    for index in 0..count {
        let entry = table.checked_add(index.checked_mul(width)?)?;
        let offset = number(bytes.get(entry..entry.checked_add(width)?)?)?;
        if offset < 8 || offset >= table {
            return None;
        }
        let marker = bytes[offset];
        if marker >> 4 != 6 {
            continue;
        }
        let mut start = offset.checked_add(1)?;
        let mut length = usize::from(marker & 15);
        if length == 15 {
            let marker = *bytes.get(start)?;
            // Match plist's existing compressed-length interpretation.
            let width = 1usize << (marker & 3);
            start = start.checked_add(1)?;
            length = number(bytes.get(start..start.checked_add(width)?)?)?;
            start = start.checked_add(width)?;
        }
        let end = start.checked_add(length.checked_mul(2)?)?;
        if end > table {
            return None;
        }
        let text = bytes.get(start..end)?;
        let mut i = 0;
        while i < text.len() {
            let unit = u16::from_be_bytes([text[i], text[i + 1]]);
            if (0xd800..=0xdbff).contains(&unit)
                && i + 3 < text.len()
                && (0xdc00..=0xdfff).contains(&u16::from_be_bytes([text[i + 2], text[i + 3]]))
            {
                i += 4;
                continue;
            }
            if (0xd800..=0xdfff).contains(&unit) {
                replacements.push(start + i);
            }
            i += 2;
        }
    }
    Some(replacements)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plist::PlistResult;

    fn binary(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = b"bplist00".to_vec();
        bytes.push(marker);
        bytes.extend_from_slice(payload);
        let table = bytes.len();
        bytes.push(8);
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]);
        bytes.extend_from_slice(&1u64.to_be_bytes());
        bytes.extend_from_slice(&0u64.to_be_bytes());
        bytes.extend_from_slice(&(table as u64).to_be_bytes());
        bytes
    }

    #[test]
    fn binary_strings_decode_lossily_without_touching_pairs_or_data() -> PlistResult<()> {
        let payload = [0xd8, 0, 0, b'x', 0xdc, 0, 0xd8, 0x3d, 0xde, 0];
        let mut bytes = binary(0x65, &payload);
        decode(&mut bytes);
        assert_eq!(
            super::super::parse_value(bytes)?,
            super::super::PlistValue::String("�x�😀".into())
        );
        let mut data = binary(0x4a, &payload);
        let original = data.clone();
        decode(&mut data);
        assert_eq!(data, original);
        Ok(())
    }

    #[test]
    fn extended_strings_and_invalid_tables_are_bounded() -> PlistResult<()> {
        let mut payload = vec![0x10, 16];
        payload.extend([0xd8, 0].repeat(16));
        let mut bytes = binary(0x6f, &payload);
        decode(&mut bytes);
        assert_eq!(
            super::super::parse_value(bytes)?,
            super::super::PlistValue::String("�".repeat(16))
        );
        // The existing decoder reads only the length marker's low two bits.
        let mut nonstandard = binary(0x6f, &[0x80, 1, 0xd8, 0]);
        decode(&mut nonstandard);
        assert_eq!(
            super::super::parse_value(nonstandard)?,
            super::super::PlistValue::String("�".into())
        );
        for size in 0..48 {
            let mut malformed = vec![0; size];
            decode(&mut malformed);
            assert_eq!(malformed, vec![0; size]);
            assert!(super::super::parse_value(malformed).is_err());
        }
        Ok(())
    }
}
