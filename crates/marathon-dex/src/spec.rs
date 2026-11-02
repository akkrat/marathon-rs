//! Port of com/linkedin/dex/spec/ (dex-test-parser 2.3.4, BSD-2-Clause,
//! Copyright (c) LinkedIn Corporation). Derived upstream from the DEX
//! format spec named in DexParser.kt:24-27. Behaviour is ported, including
//! thin header validation and zero-padded signed encoded values (a JVM
//! probe in fixtures/oracle/dex/probes.jsonl confirms -1 decodes as 255;
//! WP 6.4 rejected proposal P2). Offsets and counts that the Kotlin reader
//! dereferences are checked here: malformed input is a DexError, never a
//! panic or an unbounded allocation (WP 6.4 P3).

use crate::error::{DexError, DexResult};

pub const ACC_INTERFACE: i32 = 0x200;
pub const ACC_ABSTRACT: i32 = 0x400;

/// Checked little-endian cursor over one dex entry's bytes. Kotlin reads
/// through a shared ByteBuffer whose position jumps around; every parse
/// here takes an explicit offset instead.
#[derive(Clone, Copy)]
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn at(data: &'a [u8], offset: i32) -> DexResult<Self> {
        let pos = usize::try_from(offset)
            .map_err(|_| DexError::dex_exception(format!("Negative offset: {offset}").as_str()))?;
        if pos > data.len() {
            return Err(DexError::dex_exception(
                format!("Offset {offset} past end of dex file ({})", data.len()).as_str(),
            ));
        }
        Ok(Self { data, pos })
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn get_u8(&mut self) -> DexResult<u8> {
        let byte = self
            .data
            .get(self.pos)
            .copied()
            .ok_or_else(|| DexError::dex_exception("Unexpected end of dex file"))?;
        self.pos += 1;
        Ok(byte)
    }

    pub fn get_i8(&mut self) -> DexResult<i8> {
        Ok(self.get_u8()? as i8)
    }

    pub fn get_i16(&mut self) -> DexResult<i16> {
        let bytes = self.take(2)?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn get_i32(&mut self) -> DexResult<i32> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn take(&mut self, n: usize) -> DexResult<&'a [u8]> {
        if self.remaining() < n {
            return Err(DexError::dex_exception("Unexpected end of dex file"));
        }
        let start = self.pos;
        self.pos += n;
        Ok(&self.data[start..start + n])
    }

    /// Leb128.readUnsignedLeb128: at most 5 bytes; a 5th byte with the
    /// continuation bit set is DexException("invalid LEB128 sequence").
    pub fn get_uleb128(&mut self) -> DexResult<i32> {
        let mut result: i32 = 0;
        let mut count = 0;
        let mut current: i32;
        loop {
            current = i32::from(self.get_u8()?);
            result |= (current & 0x7f) << (count * 7);
            count += 1;
            if current & 0x80 != 0x80 || count >= 5 {
                break;
            }
        }
        if current & 0x80 == 0x80 {
            return Err(DexError::dex_exception("invalid LEB128 sequence"));
        }
        Ok(result)
    }

    /// Count from a header size or a length prefix: Kotlin allocates
    /// `Array(count)`, so negative counts crash there; here they are an
    /// error, and counts are bounded by the bytes that remain.
    pub fn count(&self, count: i32, min_item_bytes: usize) -> DexResult<usize> {
        if count < 0 {
            return Err(DexError::dex_exception(format!("Negative size: {count}").as_str()));
        }
        let count = count as usize;
        if count.saturating_mul(min_item_bytes) > self.remaining() {
            return Err(DexError::dex_exception(
                format!("Size {count} exceeds dex file bounds").as_str(),
            ));
        }
        Ok(count)
    }
}

#[derive(Clone, Debug)]
pub struct HeaderItem {
    pub string_ids_size: i32,
    pub string_ids_off: i32,
    pub type_ids_size: i32,
    pub type_ids_off: i32,
    pub proto_ids_size: i32,
    pub proto_ids_off: i32,
    pub field_ids_size: i32,
    pub field_ids_off: i32,
    pub method_ids_size: i32,
    pub method_ids_off: i32,
    pub class_defs_size: i32,
    pub class_defs_off: i32,
}

impl HeaderItem {
    /// The HeaderItem constructor reads every field (0x70 bytes, so a
    /// shorter file underflows) and only then validate() checks the
    /// magic and the endian tag.
    fn parse(data: &[u8]) -> DexResult<Self> {
        let mut c = Cursor::at(data, 0)?;
        // DexMagic: dex\n, version (3 bytes), zero.
        let dex = [c.get_i8()?, c.get_i8()?, c.get_i8()?];
        let newline = c.get_i8()?;
        let version = [c.get_i8()?, c.get_i8()?, c.get_i8()?];
        let zero = c.get_i8()?;
        let _checksum = c.get_i32()?;
        c.take(20)?; // signature, never verified upstream
        let _file_size = c.get_i32()?;
        let _header_size = c.get_i32()?;
        let endian_tag = c.get_i32()?;
        let _link_size = c.get_i32()?;
        let _link_off = c.get_i32()?;
        let _map_off = c.get_i32()?;
        let header = Self {
            string_ids_size: c.get_i32()?,
            string_ids_off: c.get_i32()?,
            type_ids_size: c.get_i32()?,
            type_ids_off: c.get_i32()?,
            proto_ids_size: c.get_i32()?,
            proto_ids_off: c.get_i32()?,
            field_ids_size: c.get_i32()?,
            field_ids_off: c.get_i32()?,
            method_ids_size: c.get_i32()?,
            method_ids_off: c.get_i32()?,
            class_defs_size: c.get_i32()?,
            class_defs_off: c.get_i32()?,
        };
        c.take(8)?; // dataSize, dataOff: read upstream, never used
        // The version check pins the parser to DEX 035-039: 040 is rejected
        // exactly as on the JVM (WP 6.4 decision 6; message is DexMagic's,
        // the Kotlin data-class toString included).
        let valid = i32::from(dex[0]) == 0x64
            && i32::from(dex[1]) == 0x65
            && i32::from(dex[2]) == 0x78
            && i32::from(newline) == 0x0a
            && i32::from(version[0]) == 0x30
            && i32::from(version[1]) == 0x33
            && i32::from(version[2]) >= 0x35
            && i32::from(zero) == 0x00;
        if !valid {
            return Err(DexError::dex_exception(
                format!(
                    "Invalid dexMagic:\nDexMagic(dex=[{}, {}, {}], newline={}, version=[{}, {}, {}], zero={})\n",
                    dex[0], dex[1], dex[2], newline, version[0], version[1], version[2], zero
                )
                .as_str(),
            ));
        }
        if endian_tag != 0x1234_5678 {
            return Err(DexError::dex_exception(
                format!("Invalid endian tag:{endian_tag}").as_str(),
            ));
        }
        Ok(header)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StringIdItem {
    pub string_data_off: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeIdItem {
    pub descriptor_idx: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtoIdItem {
    pub shorty_idx: i32,
    pub return_type_idx: i32,
    pub parameters_off: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldIdItem {
    pub class_idx: i16,
    pub type_idx: i16,
    pub name_idx: i32,
}

/// MethodIdItem equality is the whole data class (AnnotationUtils matches
/// method annotations by it, not by index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodIdItem {
    pub class_idx: i16,
    pub proto_idx: i16,
    pub name_idx: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct EncodedField {
    pub field_idx_diff: i32,
    pub access_flags: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct EncodedMethod {
    pub method_idx_diff: i32,
    pub access_flags: i32,
    pub code_off: i32,
}

#[derive(Clone, Debug, Default)]
pub struct ClassDataItem {
    pub static_fields: Vec<EncodedField>,
    pub instance_fields: Vec<EncodedField>,
    pub direct_methods: Vec<EncodedMethod>,
    pub virtual_methods: Vec<EncodedMethod>,
}

impl ClassDataItem {
    pub fn parse(data: &[u8], offset: i32) -> DexResult<Self> {
        let mut c = Cursor::at(data, offset)?;
        let static_fields_size = c.get_uleb128()?;
        let instance_fields_size = c.get_uleb128()?;
        let direct_methods_size = c.get_uleb128()?;
        let virtual_methods_size = c.get_uleb128()?;
        let fields = |c: &mut Cursor, size: i32| -> DexResult<Vec<EncodedField>> {
            let n = c.count(size, 1)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(EncodedField {
                    field_idx_diff: c.get_uleb128()?,
                    access_flags: c.get_uleb128()?,
                });
            }
            Ok(out)
        };
        let methods = |c: &mut Cursor, size: i32| -> DexResult<Vec<EncodedMethod>> {
            let n = c.count(size, 1)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(EncodedMethod {
                    method_idx_diff: c.get_uleb128()?,
                    access_flags: c.get_uleb128()?,
                    code_off: c.get_uleb128()?,
                });
            }
            Ok(out)
        };
        Ok(Self {
            static_fields: fields(&mut c, static_fields_size)?,
            instance_fields: fields(&mut c, instance_fields_size)?,
            direct_methods: methods(&mut c, direct_methods_size)?,
            virtual_methods: methods(&mut c, virtual_methods_size)?,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FieldAnnotation {
    pub field_idx: i32,
    pub annotations_off: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct MethodAnnotation {
    pub method_idx: i32,
    pub annotations_off: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct ParameterAnnotation {
    pub parameter_idx: i32,
    pub annotations_off: i32,
}

#[derive(Clone, Debug, Default)]
pub struct AnnotationsDirectoryItem {
    pub class_annotations_off: i32,
    pub field_annotations: Vec<FieldAnnotation>,
    pub method_annotations: Vec<MethodAnnotation>,
    pub parameter_annotations: Vec<ParameterAnnotation>,
}

impl AnnotationsDirectoryItem {
    pub fn parse(data: &[u8], offset: i32) -> DexResult<Self> {
        let mut c = Cursor::at(data, offset)?;
        let class_annotations_off = c.get_i32()?;
        let fields_size = c.get_i32()?;
        let annotated_methods_size = c.get_i32()?;
        let annotated_parameters_size = c.get_i32()?;
        let pairs = |c: &mut Cursor, size: i32| -> DexResult<Vec<(i32, i32)>> {
            let n = c.count(size, 8)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push((c.get_i32()?, c.get_i32()?));
            }
            Ok(out)
        };
        Ok(Self {
            class_annotations_off,
            field_annotations: pairs(&mut c, fields_size)?
                .into_iter()
                .map(|(field_idx, annotations_off)| FieldAnnotation {
                    field_idx,
                    annotations_off,
                })
                .collect(),
            method_annotations: pairs(&mut c, annotated_methods_size)?
                .into_iter()
                .map(|(method_idx, annotations_off)| MethodAnnotation {
                    method_idx,
                    annotations_off,
                })
                .collect(),
            parameter_annotations: pairs(&mut c, annotated_parameters_size)?
                .into_iter()
                .map(|(parameter_idx, annotations_off)| ParameterAnnotation {
                    parameter_idx,
                    annotations_off,
                })
                .collect(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct AnnotationSetItem {
    pub entries: Vec<i32>, // AnnotationOffItem.annotationOff values
}

impl AnnotationSetItem {
    pub fn parse(data: &[u8], offset: i32) -> DexResult<Self> {
        let mut c = Cursor::at(data, offset)?;
        let size = c.get_i32()?;
        let n = c.count(size, 4)?;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            entries.push(c.get_i32()?);
        }
        Ok(Self { entries })
    }
}

#[derive(Clone, Debug)]
pub struct AnnotationItem {
    pub visibility: i8,
    pub encoded_annotation: EncodedAnnotation,
}

impl AnnotationItem {
    pub fn parse(data: &[u8], offset: i32) -> DexResult<Self> {
        let mut c = Cursor::at(data, offset)?;
        Ok(Self {
            visibility: c.get_i8()?,
            encoded_annotation: EncodedAnnotation::parse(&mut c, 0)?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct AnnotationElement {
    pub name_idx: i32,
    pub value: EncodedValue,
}

#[derive(Clone, Debug)]
pub struct EncodedAnnotation {
    pub type_idx: i32,
    pub elements: Vec<AnnotationElement>,
}

impl EncodedAnnotation {
    pub fn parse(c: &mut Cursor, depth: usize) -> DexResult<Self> {
        let type_idx = c.get_uleb128()?;
        let size = c.get_uleb128()?;
        let n = c.count(size, 1)?;
        let mut elements = Vec::with_capacity(n);
        for _ in 0..n {
            elements.push(AnnotationElement {
                name_idx: c.get_uleb128()?,
                value: EncodedValue::parse(c, depth)?,
            });
        }
        Ok(Self { type_idx, elements })
    }
}

#[derive(Clone, Debug)]
pub struct EncodedArray {
    pub values: Vec<EncodedValue>,
}

impl EncodedArray {
    pub fn parse(c: &mut Cursor, depth: usize) -> DexResult<Self> {
        let size = c.get_uleb128()?;
        let n = c.count(size, 1)?;
        let mut values = Vec::with_capacity(n);
        for _ in 0..n {
            values.push(EncodedValue::parse(c, depth)?);
        }
        Ok(Self { values })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClassDefItem {
    pub class_idx: i32,
    pub access_flags: i32,
    pub superclass_idx: i32,
    pub interfaces_off: i32,
    pub source_file_idx: i32,
    pub annotations_off: i32,
    pub class_data_off: i32,
    pub static_values_off: i32,
}

/// Nested encoded arrays/annotations recurse upstream until the JVM stack
/// gives out; P3 caps the depth instead.
const MAX_ENCODED_VALUE_DEPTH: usize = 256;

#[derive(Clone, Debug)]
pub enum EncodedValue {
    Byte(i8),
    Short(i16),
    Char(u16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(i32),
    Type(i32),
    Field(i32),
    Method(i32),
    Enum(i32),
    Array(EncodedArray),
    Annotation(EncodedAnnotation),
    Null,
    Boolean(bool),
}

impl EncodedValue {
    pub fn parse(c: &mut Cursor, depth: usize) -> DexResult<Self> {
        if depth > MAX_ENCODED_VALUE_DEPTH {
            return Err(DexError::dex_exception("Encoded value nested too deeply"));
        }
        let arg_and_type = i32::from(c.get_u8()?);
        let value_arg = (arg_and_type >> 5) as i8;
        let value_type = (arg_and_type & 0x1f) as i8;
        // EncodedValue.create: sizeOf(valueArg) == valueArg + 1.
        let size = i32::from(value_arg) + 1;
        Ok(match value_type {
            0x00 => Self::Byte(c.get_i8()?),
            0x02 => Self::Short(i16::from_le_bytes(arr(padded(c, size, 2)?))),
            0x03 => Self::Char(u16::from_le_bytes(arr(padded(c, size, 2)?))),
            0x04 => Self::Int(i32::from_le_bytes(arr(padded(c, size, 4)?))),
            0x06 => Self::Long(i64::from_le_bytes(arr(padded(c, size, 8)?))),
            0x10 => Self::Float(f32::from_le_bytes(arr(padded_right(c, size, 4)?))),
            0x11 => Self::Double(f64::from_le_bytes(arr(padded_right(c, size, 8)?))),
            0x17 => Self::String(i32::from_le_bytes(arr(padded(c, size, 4)?))),
            0x18 => Self::Type(i32::from_le_bytes(arr(padded(c, size, 4)?))),
            0x19 => Self::Field(i32::from_le_bytes(arr(padded(c, size, 4)?))),
            0x1a => Self::Method(i32::from_le_bytes(arr(padded(c, size, 4)?))),
            0x1b => Self::Enum(i32::from_le_bytes(arr(padded(c, size, 4)?))),
            0x1c => Self::Array(EncodedArray::parse(c, depth + 1)?),
            0x1d => Self::Annotation(EncodedAnnotation::parse(c, depth + 1)?),
            0x1e => Self::Null,
            // Exactly valueArg == 1 is true; other nonzero args are false.
            0x1f => Self::Boolean(value_arg == 1),
            other => return Err(DexError::dex_exception(format!("Bad value type: {other}").as_str())),
        })
    }
}

/// padded()/padded_right() always return exactly the type width.
fn arr<const N: usize>(bytes: Vec<u8>) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes[..N]);
    out
}

/// getPaddedBuffer: value bytes first (little-endian low bytes), then zero
/// bytes — including for the signed types, where the DEX spec would
/// sign-extend. Negative short/int/long values therefore decode as large
/// positives; reproduced deliberately (P2 rejected, probe-verified).
/// A size beyond the type width overflows the Kotlin buffer; an error here.
fn padded(c: &mut Cursor, size: i32, full_size: usize) -> DexResult<Vec<u8>> {
    if size < 0 || size as usize > full_size {
        return Err(DexError::dex_exception(
            format!("Encoded value size {size} exceeds type width {full_size}").as_str(),
        ));
    }
    let mut out = vec![0u8; full_size];
    out[..size as usize].copy_from_slice(c.take(size as usize)?);
    Ok(out)
}

/// getPaddedBufferToTheRight: zero bytes in the low positions, value bytes
/// in the high positions (float/double little-endian assembly).
fn padded_right(c: &mut Cursor, size: i32, full_size: usize) -> DexResult<Vec<u8>> {
    if size < 0 || size as usize > full_size {
        return Err(DexError::dex_exception(
            format!("Encoded value size {size} exceeds type width {full_size}").as_str(),
        ));
    }
    let mut out = vec![0u8; full_size];
    let start = full_size - size as usize;
    out[start..].copy_from_slice(c.take(size as usize)?);
    Ok(out)
}

/// One parsed dex entry: header plus the id tables DexFile materialises in
/// its init block. Class data and annotations are parsed on demand, as
/// upstream (ClassDataItem.create / AnnotationsDirectoryItem.create at use).
pub struct DexFile {
    pub data: Vec<u8>,
    pub header: HeaderItem,
    pub string_ids: Vec<StringIdItem>,
    pub type_ids: Vec<TypeIdItem>,
    pub proto_ids: Vec<ProtoIdItem>,
    pub field_ids: Vec<FieldIdItem>,
    pub method_ids: Vec<MethodIdItem>,
    pub class_defs: Vec<ClassDefItem>,
    inherited_annotation_type_id_index: std::sync::OnceLock<DexResult<Option<i32>>>,
    type_id_to_class_def: std::sync::OnceLock<std::collections::HashMap<i32, ClassDefItem>>,
}

impl DexFile {
    pub const NO_INDEX: i32 = -1;

    pub fn parse(data: Vec<u8>) -> DexResult<Self> {
        let header = HeaderItem::parse(&data)?;
        fn table<T>(
            data: &[u8],
            count: i32,
            off: i32,
            item_size: usize,
            mut read: impl FnMut(&mut Cursor) -> DexResult<T>,
        ) -> DexResult<Vec<T>> {
            // Array(count) { position(off + i * size) }: an empty table never
            // positions the buffer, so its offset is not checked.
            if count == 0 {
                return Ok(Vec::new());
            }
            let base = Cursor::at(data, off)?;
            let n = base.count(count, item_size)?;
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                let mut c = Cursor::at(data, off + (i * item_size) as i32)?;
                out.push(read(&mut c)?);
            }
            Ok(out)
        }
        Ok(Self {
            string_ids: table(&data, header.string_ids_size, header.string_ids_off, 4, |c| {
                Ok(StringIdItem {
                    string_data_off: c.get_i32()?,
                })
            })?,
            type_ids: table(&data, header.type_ids_size, header.type_ids_off, 4, |c| {
                Ok(TypeIdItem {
                    descriptor_idx: c.get_i32()?,
                })
            })?,
            proto_ids: table(&data, header.proto_ids_size, header.proto_ids_off, 12, |c| {
                Ok(ProtoIdItem {
                    shorty_idx: c.get_i32()?,
                    return_type_idx: c.get_i32()?,
                    parameters_off: c.get_i32()?,
                })
            })?,
            field_ids: table(&data, header.field_ids_size, header.field_ids_off, 8, |c| {
                Ok(FieldIdItem {
                    class_idx: c.get_i16()?,
                    type_idx: c.get_i16()?,
                    name_idx: c.get_i32()?,
                })
            })?,
            method_ids: table(&data, header.method_ids_size, header.method_ids_off, 8, |c| {
                Ok(MethodIdItem {
                    class_idx: c.get_i16()?,
                    proto_idx: c.get_i16()?,
                    name_idx: c.get_i32()?,
                })
            })?,
            class_defs: table(&data, header.class_defs_size, header.class_defs_off, 0x20, |c| {
                Ok(ClassDefItem {
                    class_idx: c.get_i32()?,
                    access_flags: c.get_i32()?,
                    superclass_idx: c.get_i32()?,
                    interfaces_off: c.get_i32()?,
                    source_file_idx: c.get_i32()?,
                    annotations_off: c.get_i32()?,
                    class_data_off: c.get_i32()?,
                    static_values_off: c.get_i32()?,
                })
            })?,
            data,
            header,
            inherited_annotation_type_id_index: std::sync::OnceLock::new(),
            type_id_to_class_def: std::sync::OnceLock::new(),
        })
    }

    pub fn string_id(&self, idx: i32) -> DexResult<StringIdItem> {
        self.string_ids
            .get(usize::try_from(idx).map_err(|_| index_error("string", idx))?)
            .copied()
            .ok_or_else(|| index_error("string", idx))
    }

    pub fn type_id(&self, idx: i32) -> DexResult<TypeIdItem> {
        self.type_ids
            .get(usize::try_from(idx).map_err(|_| index_error("type", idx))?)
            .copied()
            .ok_or_else(|| index_error("type", idx))
    }

    pub fn field_id(&self, idx: i32) -> DexResult<FieldIdItem> {
        self.field_ids
            .get(usize::try_from(idx).map_err(|_| index_error("field", idx))?)
            .copied()
            .ok_or_else(|| index_error("field", idx))
    }

    pub fn method_id(&self, idx: i32) -> DexResult<MethodIdItem> {
        self.method_ids
            .get(usize::try_from(idx).map_err(|_| index_error("method", idx))?)
            .copied()
            .ok_or_else(|| index_error("method", idx))
    }

    /// DexFile.inheritedAnnotationTypeIdIndex (lazy upstream): the LAST
    /// type id whose descriptor is Ljava/lang/annotation/Inherited;, in
    /// this dex file only.
    pub fn inherited_annotation_type_id_index(&self) -> DexResult<Option<i32>> {
        self.inherited_annotation_type_id_index
            .get_or_init(|| {
                let mut result = None;
                for (index, type_id) in self.type_ids.iter().enumerate() {
                    if crate::parser::parse_descriptor(self, *type_id)?
                        == crate::parser::java_string("Ljava/lang/annotation/Inherited;")
                    {
                        result = Some(index as i32);
                    }
                }
                Ok(result)
            })
            .clone()
    }

    /// DexFile.typeIdToClassDefMap (lazy upstream); later defs win.
    pub fn class_def_for_type(&self, type_idx: i32) -> Option<ClassDefItem> {
        self.type_id_to_class_def
            .get_or_init(|| self.class_defs.iter().map(|def| (def.class_idx, *def)).collect())
            .get(&type_idx)
            .copied()
    }
}

fn index_error(kind: &str, idx: i32) -> DexError {
    DexError::dex_exception(format!("{kind} index {idx} out of bounds").as_str())
}
