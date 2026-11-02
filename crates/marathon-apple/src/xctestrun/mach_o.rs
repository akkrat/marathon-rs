//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/MachO.kt

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slice {
    pub architecture: Option<&'static str>,
    pub uuid: Option<[u8; 16]>,
}

fn at(file: &mut File, offset: u64, size: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn offsets(file: &mut File) -> io::Result<Option<Vec<u64>>> {
    let header = at(file, 0, 8)?;
    let magic = u32::from_be_bytes(header[..4].try_into().unwrap());
    if ![0xcafebabe, 0xcafebabf].contains(&magic) {
        return Ok(None);
    }
    let count = i32::from_be_bytes(header[4..].try_into().unwrap()).clamp(0, 64);
    let wide = magic == 0xcafebabf;
    let mut result = vec![];
    for index in 0..count as u64 {
        let bytes = at(file, 16 + index * if wide { 32 } else { 20 }, if wide { 8 } else { 4 })?;
        result.push(if wide {
            u64::from_be_bytes(bytes.try_into().unwrap())
        } else {
            u32::from_be_bytes(bytes.try_into().unwrap()) as u64
        });
    }
    Ok(Some(result))
}
fn thin(file: &mut File, offset: u64, length: u64) -> io::Result<Option<Slice>> {
    if offset.checked_add(32).is_none_or(|end| end > length) {
        return Ok(None);
    }
    let header = at(file, offset, 32)?;
    let header_size = match le(&header, 0) {
        0xfeedface => 28,
        0xfeedfacf => 32,
        _ => return Ok(None),
    };
    let architecture = match le(&header, 4) {
        0x0100000c => Some(if le(&header, 8) & 0x00ffffff == 2 {
            "arm64e"
        } else {
            "arm64"
        }),
        0x01000007 => Some("x86_64"),
        7 => Some("i386"),
        _ => None,
    };
    let mut slice = Slice {
        architecture,
        uuid: None,
    };
    let size = le(&header, 20) as usize;
    if size > 16 * 1024 * 1024 || offset + header_size + size as u64 > length {
        return Ok(Some(slice));
    }
    let commands = at(file, offset + header_size, size)?;
    let mut position = 0;
    for _ in 0..(le(&header, 16) as i32).max(0) {
        if position + 8 > size {
            break;
        }
        let command = le(&commands, position);
        let command_size = le(&commands, position + 4) as usize;
        if command_size < 8 || command_size > size - position {
            break;
        }
        if command == 0x1b && command_size >= 24 {
            slice.uuid = Some(commands[position + 8..position + 24].try_into().unwrap());
            break;
        }
        position += command_size;
    }
    Ok(Some(slice))
}
/// Kotlin absorbs I/O errors, including a malformed universal header, for the whole file.
pub fn slices(path: &Path) -> Vec<Slice> {
    (|| -> io::Result<Vec<Slice>> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        if length < 8 {
            return Ok(vec![]);
        }
        let offsets = offsets(&mut file)?.unwrap_or_else(|| vec![0]);
        offsets
            .into_iter()
            .filter_map(|offset| thin(&mut file, offset, length).transpose())
            .collect()
    })()
    .unwrap_or_default()
}
pub fn is_static(path: &Path) -> bool {
    (|| -> io::Result<bool> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        if length < 8 {
            return Ok(false);
        }
        let offsets = offsets(&mut file)?.unwrap_or_else(|| vec![0]);
        if offsets.is_empty() {
            return Ok(false);
        }
        for offset in offsets {
            if offset.checked_add(8).is_none_or(|end| end > length) {
                return Ok(false);
            }
            let header = at(&mut file, offset, (length - offset).min(28) as usize)?;
            if header.starts_with(b"!<arch>\n") {
                continue;
            }
            if header.len() < 28 || ![0xfeedface, 0xfeedfacf].contains(&le(&header, 0)) || le(&header, 12) != 1 {
                return Ok(false);
            }
        }
        Ok(true)
    })()
    .unwrap_or(false)
}
