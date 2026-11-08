//! Port of com/shazam/axmlparser 1.0 (AXMLParser.java, StringBlock.java,
//! ReadUtil.java, ExtDataInput.java; the sources jar carries no licence
//! header — behaviour ported, structure rewritten) reduced to the subset
//! marathon's ApkParser drives (WP 6.4 P5): chunk walking, UTF-8/UTF-16
//! string pools, start-tag names, attribute names and string values.
//! Resource-id mapping, namespaces, styled text and ResXmlEncoders are
//! not ported; the resource-id chunk is still required and skipped
//! exactly where upstream requires it.
//!
//! The streaming read order and its failures (EOFException, the chunk
//! type checks, "Invalid tag type") are preserved because ApkParser's
//! IOException wrap depends on the distinction. Attribute values come
//! from the raw valueString pool index only — no typed-value coercion —
//! so an unpooled value (index -1) reads as "" and an out-of-pool index
//! as null (both probed on the JVM; fixtures/oracle/dex/probes.jsonl).
//!
//! Hostile input is an error, never a panic or abort (WP 6.4 P3): offset
//! and length arithmetic wraps like Java `int` and is then bounds-checked,
//! chunk skipping loops where upstream recurses, and preallocation is
//! bounded by the bytes left in the document.

use crate::error::{DexError, DexResult};

pub const START_DOCUMENT: i32 = 0;
pub const END_DOCUMENT: i32 = 1;
pub const START_TAG: i32 = 2;
pub const END_TAG: i32 = 3;
pub const TEXT: i32 = 4;

const AXML_CHUNK_TYPE: i32 = 0x0008_0003;
const RESOURCEIDS_CHUNK_TYPE: i32 = 0x0008_0180;
const CHUNK_STRINGPOOL_TYPE: i32 = 0x001c_0001;
const UTF8_FLAG: i32 = 0x0000_0100;

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    /// ReadUtil.readInt(stream, 4): byte-wise little-endian; any missing
    /// byte is EOFException.
    fn read_int(&mut self) -> DexResult<i32> {
        let mut result: i32 = 0;
        for i in 0..4 {
            let byte = self.data.get(self.pos).copied().ok_or_else(DexError::eof)?;
            self.pos += 1;
            result |= i32::from(byte) << (i * 8);
        }
        Ok(result)
    }

    fn read_byte(&mut self) -> DexResult<u8> {
        let byte = self.data.get(self.pos).copied().ok_or_else(DexError::eof)?;
        self.pos += 1;
        Ok(byte)
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn read_fully(&mut self, n: usize) -> DexResult<Vec<u8>> {
        if self.data.len() - self.pos < n {
            return Err(DexError::eof());
        }
        let out = self.data[self.pos..self.pos + n].to_vec();
        self.pos += n;
        Ok(out)
    }

    fn read_int_array(&mut self, count: i32) -> DexResult<Vec<i32>> {
        let n = alloc_count(count)?;
        let mut out = Vec::with_capacity(n.min(self.remaining() / 4));
        for _ in 0..n {
            out.push(self.read_int()?);
        }
        Ok(out)
    }

    fn read_check_type(&mut self, expected: i32) -> DexResult<()> {
        let found = self.read_int()?;
        if found != expected {
            return Err(DexError::io(
                format!(
                    "Expected chunk of type 0x{:x}, read 0x{:x}.",
                    expected as u32, found as u32
                )
                .as_str(),
            ));
        }
        Ok(())
    }

    /// ExtDataInput.skipCheckChunkTypeInt: a chunk int equal to the
    /// "possible" value or below the expected one is skipped and the
    /// check retries on the next int (with possible = -1); anything else
    /// is an error. Upstream recurses once per skipped int, so a long run
    /// of zeros is StackOverflowError on the JVM; it reads to EOF here.
    fn skip_check_chunk_type(&mut self, expected: i32, possible: i32) -> DexResult<()> {
        let mut possible = possible;
        loop {
            let got = self.read_int()?;
            if got == possible || got < expected {
                possible = -1;
                continue;
            }
            if got != expected {
                return Err(DexError::io(
                    format!("Expected: 0x{expected:08x}, got: 0x{:08x}", got as u32).as_str(),
                ));
            }
            return Ok(());
        }
    }
}

fn alloc_count(count: i32) -> DexResult<usize> {
    if count < 0 {
        return Err(DexError::with_class(
            "java.lang.NegativeArraySizeException",
            Some(count.to_string().as_str().into()),
        ));
    }
    Ok(count as usize)
}

fn out_of_bounds() -> DexError {
    DexError::with_class("java.lang.ArrayIndexOutOfBoundsException", None)
}

/// ByteBuffer.wrap(array, offset, length) range check.
fn range_out_of_bounds() -> DexError {
    DexError::with_class("java.lang.IndexOutOfBoundsException", None)
}

/// StringBlock: the raw pool bytes plus offsets, decoded on demand.
struct StringBlock {
    offsets: Vec<i32>,
    strings: Vec<u8>,
    is_utf8: bool,
}

impl StringBlock {
    fn read(reader: &mut Reader) -> DexResult<Self> {
        reader.skip_check_chunk_type(CHUNK_STRINGPOOL_TYPE, 0)?;
        let chunk_size = reader.read_int()?;
        let string_count = reader.read_int()?;
        let style_count = reader.read_int()?;
        let flags = reader.read_int()?;
        let strings_offset = reader.read_int()?;
        let styles_offset = reader.read_int()?;
        let offsets = reader.read_int_array(string_count)?;
        if style_count != 0 {
            reader.read_int_array(style_count)?; // style offsets, unused (P5)
        }
        let size = (if styles_offset == 0 { chunk_size } else { styles_offset }).wrapping_sub(strings_offset);
        let strings = reader.read_fully(alloc_count(size)?)?;
        if styles_offset != 0 {
            let style_size = chunk_size.wrapping_sub(styles_offset);
            reader.read_int_array(style_size / 4)?; // styles, unused (P5)
            let mut remaining = style_size % 4;
            while remaining > 0 {
                reader.read_byte()?;
                remaining -= 1;
            }
        }
        Ok(Self {
            offsets,
            strings,
            is_utf8: flags & UTF8_FLAG != 0,
        })
    }

    fn byte(&self, offset: i32) -> DexResult<i32> {
        let byte = self
            .strings
            .get(usize::try_from(offset).map_err(|_| out_of_bounds())?)
            .copied()
            .ok_or_else(out_of_bounds)?;
        Ok(i32::from(byte as i8))
    }

    fn ubyte(&self, offset: i32) -> DexResult<i32> {
        Ok(self.byte(offset)? & 0xff)
    }

    /// StringBlock.getString: null (None) for an invalid index or a
    /// string the charset decoder rejects.
    fn get_string(&self, index: i32) -> DexResult<Option<String>> {
        if index < 0 || index as usize >= self.offsets.len() {
            return Ok(None);
        }
        let mut offset = self.offsets[index as usize];
        let length: i32;
        if self.is_utf8 {
            let (new_offset, len) = self.get_utf8(offset)?;
            offset = new_offset;
            length = len;
        } else {
            let (skip, len) = self.get_utf16_length(offset)?;
            offset = offset.wrapping_add(skip);
            length = len;
        }
        self.decode(offset, length)
    }

    /// StringBlock.getUtf8, signed-byte arithmetic included.
    fn get_utf8(&self, mut offset: i32) -> DexResult<(i32, i32)> {
        let mut value = self.byte(offset)?;
        offset = offset.wrapping_add(if value & 0x80 != 0 { 2 } else { 1 });
        value = self.byte(offset)?;
        offset = offset.wrapping_add(1);
        let length = if value & 0x80 != 0 {
            let low = self.ubyte(offset)?;
            offset = offset.wrapping_add(1);
            ((value & 0x7f) << 8) + low
        } else {
            value
        };
        Ok((offset, length))
    }

    /// StringBlock.getUtf16. `len_value * 2` overflows to a negative
    /// length for the largest prefixes, as in Java; decode() rejects it.
    fn get_utf16_length(&self, offset: i32) -> DexResult<(i32, i32)> {
        let value = (self.ubyte(offset.wrapping_add(1))? << 8) | self.ubyte(offset)?;
        if value & 0x8000 != 0 {
            let high = self.ubyte(offset.wrapping_add(3))? << 8;
            let low = self.ubyte(offset.wrapping_add(2))?;
            let len = ((value & 0x7fff) << 16) + (high + low);
            return Ok((4, len.wrapping_mul(2)));
        }
        Ok((2, value * 2))
    }

    fn decode(&self, offset: i32, length: i32) -> DexResult<Option<String>> {
        let (Ok(offset), Ok(length)) = (usize::try_from(offset), usize::try_from(length)) else {
            return Err(range_out_of_bounds());
        };
        let slice = offset
            .checked_add(length)
            .and_then(|end| self.strings.get(offset..end))
            .ok_or_else(range_out_of_bounds)?;
        if self.is_utf8 {
            // Java's UTF-8 CharsetDecoder in REPORT mode (decodeString
            // catches CharacterCodingException and returns null).
            let Ok(text) = std::str::from_utf8(slice) else {
                return Ok(None);
            };
            Ok(Some(text.into()))
        } else {
            // UTF-16LE in REPORT mode: an odd trailing byte or an unpaired
            // surrogate is malformed, and the whole string reads as null.
            if length % 2 != 0 {
                return Ok(None);
            }
            let units: Vec<u16> = slice
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect();
            if char::decode_utf16(units.iter().copied()).any(|c| c.is_err()) {
                return Ok(None);
            }
            Ok(Some(String::from_utf16_lossy(&units)))
        }
    }
}

#[derive(Clone, Copy, Default)]
struct TagAttribute {
    namespace: i32,
    name: i32,
    value_string: i32,
    value_type: i32,
    value: i32,
}

/// AXMLParser over an in-memory manifest. Upstream streams from the zip
/// entry; ApkParser reads the whole document either way, and the read
/// order (including EOF failures) is unchanged.
pub struct AxmlParser<'a> {
    reader: Reader<'a>,
    strings: StringBlock,
    next_exception: Option<DexError>,
    tag_type: i32,
    tag_name: i32,
    tag_attributes: Option<Vec<TagAttribute>>,
}

impl<'a> AxmlParser<'a> {
    pub fn new(data: &'a [u8]) -> DexResult<Self> {
        let mut reader = Reader { data, pos: 0 };
        reader.read_check_type(AXML_CHUNK_TYPE)?;
        reader.read_int()?; // chunk size
        let strings = StringBlock::read(&mut reader)?;
        reader.read_check_type(RESOURCEIDS_CHUNK_TYPE)?;
        let chunk_size = reader.read_int()?;
        if chunk_size < 8 || chunk_size % 4 != 0 {
            return Err(DexError::io(
                format!("Invalid resource ids size ({chunk_size}).").as_str(),
            ));
        }
        reader.read_int_array(chunk_size / 4 - 2)?; // resource ids, unused (P5)
        Ok(Self {
            reader,
            strings,
            next_exception: None,
            tag_type: -1,
            tag_name: -1,
            tag_attributes: None,
        })
    }

    pub fn event_type(&self) -> i32 {
        self.tag_type
    }

    // Mirrors upstream AXMLParser.next(); not an Iterator.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> DexResult<i32> {
        if let Some(err) = &self.next_exception {
            return Err(err.clone());
        }
        match self.do_next() {
            Ok(event) => Ok(event),
            Err(err) => {
                if err.is_io() {
                    self.next_exception = Some(err.clone());
                    self.reset_state();
                }
                Err(err)
            }
        }
    }

    fn reset_state(&mut self) {
        self.tag_type = -1;
        self.tag_name = -1;
        self.tag_attributes = None;
    }

    fn do_next(&mut self) -> DexResult<i32> {
        if self.tag_type == END_DOCUMENT {
            return Ok(END_DOCUMENT);
        }
        self.tag_type = self.reader.read_int()? & 0xff;
        self.reader.read_int()?;
        self.reader.read_int()?; // source line, unused by ApkParser
        self.reader.read_int()?;
        self.tag_name = -1;
        self.tag_attributes = None;
        match self.tag_type {
            START_DOCUMENT | END_DOCUMENT => {
                self.reader.read_int()?;
                self.reader.read_int()?;
            }
            START_TAG => {
                self.reader.read_int()?;
                self.tag_name = self.reader.read_int()?;
                self.reader.read_int()?;
                let attribute_count = self.reader.read_int()?;
                self.reader.read_int()?;
                let n = alloc_count(attribute_count)?;
                let mut attributes = Vec::with_capacity(n.min(self.reader.remaining() / 20));
                for _ in 0..n {
                    attributes.push(TagAttribute {
                        namespace: self.reader.read_int()?,
                        name: self.reader.read_int()?,
                        value_string: self.reader.read_int()?,
                        value_type: (self.reader.read_int()? >> 24) & 0xff,
                        value: self.reader.read_int()?,
                    });
                }
                self.tag_attributes = Some(attributes);
            }
            END_TAG => {
                self.reader.read_int()?;
                self.tag_name = self.reader.read_int()?;
            }
            TEXT => {
                self.tag_name = self.reader.read_int()?;
                self.reader.read_int()?;
                self.reader.read_int()?;
            }
            other => return Err(DexError::io(format!("Invalid tag type ({other}).").as_str())),
        }
        Ok(self.tag_type)
    }

    fn get_string(&self, index: i32) -> DexResult<Option<String>> {
        if index == -1 {
            return Ok(Some(String::default()));
        }
        self.strings.get_string(index)
    }

    /// getName: null when there is no current tag name.
    pub fn name(&self) -> DexResult<Option<String>> {
        if self.tag_name == -1 {
            return Ok(None);
        }
        self.get_string(self.tag_name)
    }

    pub fn attribute_count(&self) -> i32 {
        self.tag_attributes.as_ref().map_or(-1, |attrs| attrs.len() as i32)
    }

    fn attribute(&self, index: i32) -> DexResult<TagAttribute> {
        let Some(attrs) = &self.tag_attributes else {
            return Err(DexError::with_class(
                "java.lang.IndexOutOfBoundsException",
                Some("Attributes are not available.".into()),
            ));
        };
        if index < 0 || index as usize >= attrs.len() {
            return Err(DexError::with_class(
                "java.lang.IndexOutOfBoundsException",
                Some(format!("Invalid attribute index ({index}).").as_str().into()),
            ));
        }
        Ok(attrs[index as usize])
    }

    pub fn attribute_name(&self, index: i32) -> DexResult<Option<String>> {
        self.get_string(self.attribute(index)?.name)
    }

    /// getAttributeValueString: the pooled string for the raw
    /// valueString index, whatever the typed value says.
    pub fn attribute_value_string(&self, index: i32) -> DexResult<Option<String>> {
        self.get_string(self.attribute(index)?.value_string)
    }

    // Kept for parity with the upstream surface ApkParser could reach;
    // marathon 1.0 callers use only name/valueString.
    #[allow(dead_code)]
    fn attribute_namespace(&self, index: i32) -> DexResult<Option<String>> {
        self.get_string(self.attribute(index)?.namespace)
    }

    #[allow(dead_code)]
    fn attribute_value_type(&self, index: i32) -> DexResult<i32> {
        Ok(self.attribute(index)?.value_type)
    }

    #[allow(dead_code)]
    fn attribute_value(&self, index: i32) -> DexResult<i32> {
        Ok(self.attribute(index)?.value)
    }
}
