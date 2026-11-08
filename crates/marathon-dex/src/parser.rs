//! Port of com/linkedin/dex/parser/ (dex-test-parser 2.3.4, BSD-2-Clause,
//! Copyright (c) LinkedIn Corporation): DexParser.findTestMethods,
//! JUnit3Extensions, JUnit4Extensions, AnnotationUtils, DecodedValue,
//! TestMethod, TestAnnotation, FormatUtils, ParseUtils, DexFileUtils.
//! The Clikt CLI (`main`, findTestNames) is not ported (WP 6.4 P6).
//!
//! Strings are jcompat `String` (UTF-16) exactly once, at decode.
//! DEX string data is MUTF-8, but ParseUtils decodes it with
//! `String(bytes)`, the default charset, which is UTF-8 on the reference
//! JDK (JEP 400). That decode is reproduced (WP 6.4 P1 withdrawn): an
//! encoded NUL (`C0 80`) becomes two U+FFFD and each half of a surrogate
//! pair one U+FFFD, so 𝄞 written by d8 reads as two replacement characters.

use crate::{
    error::{DexError, DexResult},
    spec::{
        ACC_ABSTRACT, ACC_INTERFACE, AnnotationItem, AnnotationSetItem, AnnotationsDirectoryItem, ClassDataItem,
        ClassDefItem, Cursor, DexFile, EncodedValue, MethodIdItem, TypeIdItem,
    },
};

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    path::Path,
    sync::Arc,
};

pub fn java_string(text: &str) -> String {
    text.into()
}

/// TestAnnotation (TestAnnotation.kt): insertion-ordered values; duplicate
/// element names keep the first position with the last value (Kotlin Map).
#[derive(Clone, Debug)]
pub struct TestAnnotation {
    pub name: String,
    pub values: Vec<(String, DecodedValue)>,
    pub inherited: bool,
}

impl TestAnnotation {
    pub fn value(&self, name: &str) -> Option<&DecodedValue> {
        let key = java_string(name);
        self.values.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }
}

impl PartialEq for TestAnnotation {
    /// Kotlin data-class equals: name, inherited, and Map equals (which
    /// ignores entry order).
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.inherited == other.inherited
            && self.values.len() == other.values.len()
            && self.values.iter().all(|(k, v)| {
                other
                    .values
                    .iter()
                    .find(|(ok, _)| ok == k)
                    .is_some_and(|(_, ov)| ov == v)
            })
    }
}
impl Eq for TestAnnotation {}

/// TestMethod (TestMethod.kt); ordering is testName only (Comparable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestMethod {
    pub test_name: String,
    pub annotations: Vec<TestAnnotation>,
}

/// DecodedValue (parser/DecodedValue.kt). Equality mirrors the Kotlin
/// data classes: Float/Double use Java compare semantics (NaNs equal,
/// signed zeros distinct); arrays compare by backing-array identity —
/// probed on the pinned JAR (a fresh equal-content array is NOT equal).
/// That identity rule is why JUnit3 discovery can emit duplicates.
#[derive(Clone, Debug)]
pub enum DecodedValue {
    Byte(i8),
    Short(i16),
    Char(u16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(String),
    Type(String),
    Null,
    Boolean(bool),
    Enum(String),
    Array(Arc<[DecodedValue]>),
}

impl PartialEq for DecodedValue {
    fn eq(&self, other: &Self) -> bool {
        use DecodedValue::*;
        match (self, other) {
            (Byte(a), Byte(b)) => a == b,
            (Short(a), Short(b)) => a == b,
            (Char(a), Char(b)) => a == b,
            (Int(a), Int(b)) => a == b,
            (Long(a), Long(b)) => a == b,
            (Float(a), Float(b)) => a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
            (Double(a), Double(b)) => a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
            (String(a), String(b)) | (Type(a), Type(b)) | (Enum(a), Enum(b)) => a == b,
            (Null, Null) => true,
            (Boolean(a), Boolean(b)) => a == b,
            (Array(a), Array(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}
impl Eq for DecodedValue {}

/// DexParser.findTestMethods (DexParser.kt:84-95): JUnit3 then JUnit4,
/// each sorted by testName, concatenated and sorted again; duplicates of
/// the same method from the two engines are both kept.
pub fn find_test_methods(apk_path: &Path, custom_annotations: &[String]) -> DexResult<Vec<TestMethod>> {
    let dex_files = read_dex_files(apk_path)?;
    let mut junit3 = find_junit3_tests(&dex_files)?;
    junit3.sort_by(|a, b| a.test_name.cmp(&b.test_name));
    let mut junit4 = find_all_junit4_tests(&dex_files, custom_annotations)?;
    junit4.sort_by(|a, b| a.test_name.cmp(&b.test_name));
    let mut all = junit3;
    all.extend(junit4);
    all.sort_by(|a, b| a.test_name.cmp(&b.test_name));
    Ok(all)
}

/// DexParser.readDexFiles: a streaming walk in archive order (never
/// sorted), keeping every entry whose name ends with ".dex". No dex
/// entries yields an empty vec, not an error.
pub fn read_dex_files(apk_path: &Path) -> DexResult<Vec<DexFile>> {
    enum Failure {
        Dex(DexError),
        /// The streaming reader rejects every entry with a data
        /// descriptor; ZipInputStream reads the deflated ones, so fall
        /// back below.
        UnsupportedStreaming,
    }
    fn streaming(apk_path: &Path) -> Result<Vec<DexFile>, Failure> {
        let mut file = File::open(apk_path).map_err(|err| Failure::Dex(DexError::open_file(apk_path, &err)))?;
        let mut dex_files = Vec::new();
        loop {
            let entry = match zip::read::read_zipfile_from_stream(&mut file) {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                // ZipInputStream stops (null entry) at the first non
                // local-header signature instead of failing; a
                // truncated trailing entry downstream of good entries
                // behaves the same.
                Err(zip::result::ZipError::InvalidArchive(_)) => break,
                // A stream too short to hold even a local header reads
                // as "no (more) entries" the same way: the crate asks
                // for the whole header up front (UnexpectedEof) where
                // ZipInputStream checks the 4-byte signature first and
                // stops (probed: a text file yields no tests, no error).
                Err(zip::result::ZipError::Io(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(zip::result::ZipError::UnsupportedArchive(_)) => return Err(Failure::UnsupportedStreaming),
                Err(err) => return Err(Failure::Dex(DexError::from_zip(err))),
            };
            let is_dex = entry.name().ends_with(".dex");
            let mut entry = entry;
            if is_dex {
                let mut bytes = Vec::new();
                match entry.read_to_end(&mut bytes) {
                    Ok(_) => dex_files.extend(DexFile::parse_all(bytes).map_err(Failure::Dex)?),
                    Err(err)
                        if err
                            .get_ref()
                            .is_some_and(|e| e.downcast_ref::<zip::result::ZipError>().is_some()) =>
                    {
                        return Err(Failure::UnsupportedStreaming);
                    }
                    Err(err) => return Err(Failure::Dex(DexError::io(err.to_string().as_str()))),
                }
            } else {
                match std::io::copy(&mut entry, &mut std::io::sink()) {
                    Ok(_) => {}
                    Err(err)
                        if err
                            .get_ref()
                            .is_some_and(|e| e.downcast_ref::<zip::result::ZipError>().is_some()) =>
                    {
                        return Err(Failure::UnsupportedStreaming);
                    }
                    Err(err) => return Err(Failure::Dex(DexError::io(err.to_string().as_str()))),
                }
            }
        }
        Ok(dex_files)
    }

    match streaming(apk_path) {
        Ok(dex_files) => Ok(dex_files),
        Err(Failure::Dex(err)) => Err(err),
        Err(Failure::UnsupportedStreaming) => {
            // Same selection through the central directory, whose order
            // is the order entries were added (the local order for any
            // zip a build tool writes; hand-reordered or directory-less
            // archives may differ, an accepted simplification). An
            // archive the directory cannot describe at all reads as
            // empty, as with ZipInputStream.
            let file = File::open(apk_path).map_err(|err| DexError::open_file(apk_path, &err))?;
            let mut local_headers = File::open(apk_path).map_err(|err| DexError::open_file(apk_path, &err))?;
            let mut archive = match zip::ZipArchive::new(file) {
                Ok(archive) => archive,
                Err(zip::result::ZipError::InvalidArchive(_)) => return Ok(Vec::new()),
                Err(err) => return Err(DexError::from_zip(err)),
            };
            let mut dex_files = Vec::new();
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index).map_err(DexError::from_zip)?;
                check_local_header(&mut local_headers, entry.header_start())?;
                if entry.name().ends_with(".dex") {
                    let mut bytes = Vec::new();
                    entry
                        .read_to_end(&mut bytes)
                        .map_err(|err| DexError::io(err.to_string().as_str()))?;
                    dex_files.extend(DexFile::parse_all(bytes)?);
                }
            }
            Ok(dex_files)
        }
    }
}

/// ZipInputStream.readLOC's check on every entry it reaches: a data
/// descriptor (flag bit 3) is only allowed on DEFLATED entries. Only the
/// central-directory path needs it; the streaming path never gets past
/// an entry with a descriptor.
fn check_local_header(file: &mut File, header_start: u64) -> DexResult<()> {
    use std::io::{Seek, SeekFrom};
    let mut header = [0u8; 10];
    file.seek(SeekFrom::Start(header_start))
        .and_then(|_| file.read_exact(&mut header))
        .map_err(|err| DexError::io(err.to_string().as_str()))?;
    let flags = u16::from_le_bytes([header[6], header[7]]);
    let method = u16::from_le_bytes([header[8], header[9]]);
    if flags & 8 == 8 && method != 8 {
        return Err(DexError::zip_exception("only DEFLATED entries can have EXT descriptor"));
    }
    Ok(())
}

/// ParseUtils.parseStringBytes + the size skip every caller does before
/// it: bytes up to the terminating 0x00, then `String(bytes)`.
pub fn parse_string_at(dex: &DexFile, offset: i32) -> DexResult<String> {
    let mut c = Cursor::at(&dex.data, offset)?;
    c.get_uleb128()?; // utf16 size, parsed and unused upstream
    let mut bytes = Vec::new();
    loop {
        let byte = c.get_u8()?;
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    Ok(decode_string_bytes(&bytes))
}

/// `String(bytes)` with the UTF-8 default charset: the JDK's replacing
/// decode, one U+FFFD per maximal invalid subpart, where `ED A0..BF`
/// starts a (surrogate) sequence that becomes ONE U+FFFD. Checked against
/// the JVM in fixtures/oracle/dex/string-bytes.txt.
pub fn decode_string_bytes(bytes: &[u8]) -> String {
    jcompat::utf8::decode_utf8(bytes).as_str().into()
}

/// ParseUtils.parseDescriptor: the raw `L…;` string for a type id.
pub fn parse_descriptor(dex: &DexFile, type_id: TypeIdItem) -> DexResult<String> {
    let string_id = dex.string_id(type_id.descriptor_idx)?;
    parse_string_at(dex, string_id.string_data_off)
}

/// ParseUtils.parseClassName.
fn parse_class_name(dex: &DexFile, class_def: &ClassDefItem) -> DexResult<String> {
    parse_descriptor(dex, dex.type_id(class_def.class_idx)?)
}

/// ParseUtils.parseMethodName.
fn parse_method_name(dex: &DexFile, method_id: &MethodIdItem) -> DexResult<String> {
    let string_id = dex.string_id(method_id.name_idx)?;
    parse_string_at(dex, string_id.string_data_off)
}

/// formatDescriptor (FormatUtils.kt): blindly drops the first and last
/// UTF-16 unit and maps '/' to '.', whatever the descriptor was.
pub fn format_descriptor(descriptor: &String) -> DexResult<String> {
    if descriptor.is_empty() {
        // Kotlin: "".substring(1) throws StringIndexOutOfBoundsException.
        return Err(DexError::dex_exception("Empty type descriptor"));
    }
    let mut units: Vec<u16> = descriptor.encode_utf16().collect::<Vec<_>>()[1..]
        .iter()
        .map(|&u| if u == u16::from(b'/') { u16::from(b'.') } else { u })
        .collect();
    units.pop();
    Ok(String::from_utf16_lossy(&units))
}

/// DexFile.formatClassName: formatted class name plus '#'.
fn format_class_name(dex: &DexFile, class_def: &ClassDefItem) -> DexResult<String> {
    let mut name = format_descriptor(&parse_class_name(dex, class_def)?)?;
    name.push('#');
    Ok(name)
}

/// DecodedValue.create (DecodedValue.kt:40-73): encoded field/method/
/// annotation values are the three upstream TODOs and become Null.
fn decode_value(dex: &DexFile, value: &EncodedValue) -> DexResult<DecodedValue> {
    Ok(match value {
        EncodedValue::Byte(v) => DecodedValue::Byte(*v),
        EncodedValue::Short(v) => DecodedValue::Short(*v),
        EncodedValue::Char(v) => DecodedValue::Char(*v),
        EncodedValue::Int(v) => DecodedValue::Int(*v),
        EncodedValue::Long(v) => DecodedValue::Long(*v),
        EncodedValue::Float(v) => DecodedValue::Float(*v),
        EncodedValue::Double(v) => DecodedValue::Double(*v),
        EncodedValue::String(idx) => DecodedValue::String(parse_string_at(dex, dex.string_id(*idx)?.string_data_off)?),
        EncodedValue::Type(idx) => {
            let descriptor_idx = dex.type_id(*idx)?.descriptor_idx;
            DecodedValue::Type(parse_string_at(dex, dex.string_id(descriptor_idx)?.string_data_off)?)
        }
        EncodedValue::Boolean(v) => DecodedValue::Boolean(*v),
        EncodedValue::Null => DecodedValue::Null,
        EncodedValue::Enum(idx) => {
            let name_idx = dex.field_id(*idx)?.name_idx;
            DecodedValue::Enum(parse_string_at(dex, dex.string_id(name_idx)?.string_data_off)?)
        }
        EncodedValue::Array(array) => {
            let mut values = Vec::with_capacity(array.values.len());
            for element in &array.values {
                values.push(decode_value(dex, element)?);
            }
            DecodedValue::Array(Arc::from(values))
        }
        EncodedValue::Field(_) | EncodedValue::Method(_) | EncodedValue::Annotation(_) => DecodedValue::Null,
    })
}

/// AnnotationUtils: annotationsOff == 0 means no directory at all.
fn annotations_directory(dex: &DexFile, class_def: &ClassDefItem) -> DexResult<Option<AnnotationsDirectoryItem>> {
    if class_def.annotations_off != 0 {
        Ok(Some(AnnotationsDirectoryItem::parse(
            &dex.data,
            class_def.annotations_off,
        )?))
    } else {
        Ok(None)
    }
}

fn test_annotation(dex: &DexFile, item: &AnnotationItem) -> DexResult<TestAnnotation> {
    let encoded = &item.encoded_annotation;
    let name = format_descriptor(&parse_descriptor(dex, dex.type_id(encoded.type_idx)?)?)?;
    let mut values: Vec<(String, DecodedValue)> = Vec::new();
    for element in &encoded.elements {
        let value = decode_value(dex, &element.value)?;
        let value_name = parse_string_at(dex, dex.string_id(element.name_idx)?.string_data_off)?;
        match values.iter_mut().find(|(k, _)| *k == value_name) {
            Some((_, current)) => *current = value,
            None => values.push((value_name, value)),
        }
    }
    let inherited = check_if_annotation_is_inherited(dex, dex.class_def_for_type(encoded.type_idx))?;
    Ok(TestAnnotation {
        name,
        values,
        inherited,
    })
}

fn annotation_set(dex: &DexFile, offset: i32) -> DexResult<Vec<TestAnnotation>> {
    let set = AnnotationSetItem::parse(&dex.data, offset)?;
    let mut out = Vec::with_capacity(set.entries.len());
    for entry in set.entries {
        out.push(test_annotation(dex, &AnnotationItem::parse(&dex.data, entry)?)?);
    }
    Ok(out)
}

fn class_annotation_values(
    dex: &DexFile,
    directory: Option<&AnnotationsDirectoryItem>,
) -> DexResult<Vec<TestAnnotation>> {
    let Some(directory) = directory else {
        return Ok(Vec::new());
    };
    if directory.class_annotations_off == 0 {
        return Ok(Vec::new());
    }
    annotation_set(dex, directory.class_annotations_off)
}

/// Method annotations match by whole MethodIdItem equality, not by index.
fn method_annotation_values(
    dex: &DexFile,
    method_id: &MethodIdItem,
    directory: Option<&AnnotationsDirectoryItem>,
) -> DexResult<Vec<TestAnnotation>> {
    let Some(directory) = directory else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for annotation in &directory.method_annotations {
        if dex.method_id(annotation.method_idx)? == *method_id {
            out.extend(annotation_set(dex, annotation.annotations_off)?);
        }
    }
    Ok(out)
}

/// The annotation type must be defined in the SAME dex file and carry
/// Ljava/lang/annotation/Inherited; there (AnnotationUtils.kt:80-95).
fn check_if_annotation_is_inherited(dex: &DexFile, annotation_class_def: Option<ClassDefItem>) -> DexResult<bool> {
    let Some(inherited_index) = dex.inherited_annotation_type_id_index()? else {
        return Ok(false);
    };
    let Some(class_def) = annotation_class_def else {
        return Ok(false);
    };
    let Some(directory) = annotations_directory(dex, &class_def)? else {
        return Ok(false);
    };
    if directory.class_annotations_off == 0 {
        return Ok(false);
    }
    let set = AnnotationSetItem::parse(&dex.data, directory.class_annotations_off)?;
    for entry in set.entries {
        if AnnotationItem::parse(&dex.data, entry)?.encoded_annotation.type_idx == inherited_index {
            return Ok(true);
        }
    }
    Ok(false)
}

/// DexFileUtils.findMethodIdxs: virtual methods only (private/static/
/// constructor methods live in directMethods and are never candidates),
/// indices accumulated from methodIdxDiff.
fn find_method_idxs(dex: &DexFile, class_def: &ClassDefItem) -> DexResult<Vec<i32>> {
    if class_def.class_data_off == 0 {
        return Ok(Vec::new());
    }
    let class_data = ClassDataItem::parse(&dex.data, class_def.class_data_off)?;
    let mut method_ids = Vec::with_capacity(class_data.virtual_methods.len());
    let mut previous = 0i32;
    for (index, method) in class_data.virtual_methods.iter().enumerate() {
        let mut idx = method.method_idx_diff;
        if index != 0 {
            idx = idx.wrapping_add(previous);
        }
        previous = idx;
        method_ids.push(idx);
    }
    Ok(method_ids)
}

/// The methodIdFinder of TestMethod.kt: which methods of a class are
/// potential tests (varies between JUnit3 and JUnit4).
type MethodIdFinder<'a> =
    dyn Fn(&DexFile, &ClassDefItem, Option<&AnnotationsDirectoryItem>) -> DexResult<Vec<MethodIdItem>> + 'a;

/// TestMethod.kt createTestMethods for one class: merged annotations are
/// class-level first, then the method's own.
fn create_test_methods(
    dex: &DexFile,
    class_def: &ClassDefItem,
    method_id_finder: &MethodIdFinder,
) -> DexResult<Vec<TestMethod>> {
    let directory = annotations_directory(dex, class_def)?;
    let class_annotations = class_annotation_values(dex, directory.as_ref())?;
    let method_ids = method_id_finder(dex, class_def, directory.as_ref())?;
    let class_name = format_class_name(dex, class_def)?;
    let mut out = Vec::with_capacity(method_ids.len());
    for method_id in &method_ids {
        let mut annotations = class_annotations.clone();
        annotations.extend(method_annotation_values(dex, method_id, directory.as_ref())?);
        let mut test_name = class_name.clone();
        test_name.push_str(&parse_method_name(dex, method_id)?);
        out.push(TestMethod { test_name, annotations });
    }
    Ok(out)
}

fn push_unique(set: &mut Vec<TestMethod>, method: TestMethod) {
    if !set.contains(&method) {
        set.push(method);
    }
}

fn test_name_without_class(test_name: &String) -> String {
    // substring(indexOf('#') + 1); indexOf is -1 without a '#', giving the
    // whole string.
    let start = test_name
        .encode_utf16()
        .collect::<Vec<_>>()
        .iter()
        .position(|&u| u == u16::from(b'#'))
        .map_or(0, |i| i + 1);
    String::from_utf16_lossy(&test_name.encode_utf16().collect::<Vec<_>>()[start..])
}

// ---- JUnit4 (JUnit4Extensions.kt) -----------------------------------------

struct ClassParsingResult {
    dex_index: usize,
    class_def: ClassDefItem,
    class_name: String,
    super_class_name: String,
    test_methods: Vec<TestMethod>, // Kotlin Set: insertion-ordered, unique
    is_concrete: bool,
}

fn is_interface(class_def: &ClassDefItem) -> bool {
    class_def.access_flags & ACC_INTERFACE == ACC_INTERFACE
}

fn is_concrete(class_def: &ClassDefItem) -> bool {
    class_def.access_flags & ACC_ABSTRACT != ACC_ABSTRACT && !is_interface(class_def)
}

const JUNIT_TEST_ANNOTATION_NAME: &str = "org.junit.Test";

/// containsTestAnnotation checks the MERGED annotation list, so a
/// class-level @Test would promote every annotated method.
fn contains_test_annotation(method: &TestMethod, custom_annotations: &[String]) -> bool {
    let junit = java_string(JUNIT_TEST_ANNOTATION_NAME);
    method
        .annotations
        .iter()
        .any(|a| a.name == junit || custom_annotations.contains(&a.name))
}

/// getSuperclassName: typeIds[superclassIdx] with no NO_INDEX check — a
/// -1 superclass is ArrayIndexOutOfBoundsException on the JVM (probed).
/// It collapses to DexError here (P3), via DexFile::type_id.
fn superclass_name(dex: &DexFile, class_def: &ClassDefItem) -> DexResult<String> {
    parse_descriptor(dex, dex.type_id(class_def.superclass_idx)?)
}

/// findAllJUnit4Tests: abstract classes are mapped (inheritance source)
/// but produce no tests of their own; interfaces are excluded entirely.
fn find_all_junit4_tests(dex_files: &[DexFile], custom_annotations: &[String]) -> DexResult<Vec<TestMethod>> {
    // parseClasses + associateBy(className): later duplicates replace the
    // value, the key keeps its first position (LinkedHashMap).
    let mut results: Vec<ClassParsingResult> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let annotated_finder = |dex: &DexFile,
                            class_def: &ClassDefItem,
                            directory: Option<&AnnotationsDirectoryItem>|
     -> DexResult<Vec<MethodIdItem>> {
        // findMethodIds (JUnit4): virtual methods present in the
        // annotations directory's methodAnnotations.
        let annotated: Vec<i32> = directory
            .map(|d| d.method_annotations.iter().map(|m| m.method_idx).collect())
            .unwrap_or_default();
        let mut out = Vec::new();
        for idx in find_method_idxs(dex, class_def)? {
            if annotated.contains(&idx) {
                out.push(dex.method_id(idx)?);
            }
        }
        Ok(out)
    };
    for (dex_index, dex) in dex_files.iter().enumerate() {
        for class_def in &dex.class_defs {
            if is_interface(class_def) {
                continue;
            }
            let mut test_methods: Vec<TestMethod> = Vec::new();
            for method in create_test_methods(dex, class_def, &annotated_finder)? {
                if contains_test_annotation(&method, custom_annotations) {
                    push_unique(&mut test_methods, method);
                }
            }
            let result = ClassParsingResult {
                dex_index,
                class_def: *class_def,
                class_name: parse_class_name(dex, class_def)?,
                super_class_name: superclass_name(dex, class_def)?,
                test_methods,
                is_concrete: is_concrete(class_def),
            };
            match by_name.get(&result.class_name) {
                Some(&position) => results[position] = result,
                None => {
                    by_name.insert(result.class_name.clone(), results.len());
                    results.push(result);
                }
            }
        }
    }

    let mut out = Vec::new();
    for position in 0..results.len() {
        if results[position].is_concrete {
            out.extend(create_all_test_methods(position, dex_files, &results, &by_name)?);
        }
    }
    Ok(out)
}

/// createAllTestMethods. Upstream recurses into the superclass first,
/// without a cycle guard: a superclass cycle is StackOverflowError on the
/// JVM (probed) and a DexError here (P3). The recursion is unrolled into
/// a walk up the chain and a fold back down, so a long hostile chain
/// cannot exhaust the stack.
///
/// Upstream wraps the body in `classAllTestMethods.getOrPut(className)`,
/// but the body ends in a non-local `return` out of the inline lambda, so
/// the cache is never filled: every call re-reads the class annotations
/// of each class on the chain into fresh arrays. Nothing is memoised
/// here either, because DecodedArrayValue equality (and C19 identity)
/// sees the difference between a shared and a re-read array.
fn create_all_test_methods(
    position: usize,
    dex_files: &[DexFile],
    results: &[ClassParsingResult],
    by_name: &HashMap<String, usize>,
) -> DexResult<Vec<TestMethod>> {
    let mut chain = vec![position];
    let mut on_chain = HashSet::from([position]);
    let mut current = position;
    while let Some(&super_position) = by_name.get(&results[current].super_class_name) {
        if !on_chain.insert(super_position) {
            return Err(DexError::dex_exception(
                format!("Superclass cycle at {}", results[super_position].class_name.to_owned()).as_str(),
            ));
        }
        chain.push(super_position);
        current = super_position;
    }
    let mut super_methods = Vec::new();
    for &class_position in chain.iter().rev() {
        super_methods = combine_with_superclass(&results[class_position], dex_files, &super_methods)?;
    }
    Ok(super_methods)
}

/// The getOrPut body of createAllTestMethods for one class, given the
/// superclass's already combined methods.
fn combine_with_superclass(
    result: &ClassParsingResult,
    dex_files: &[DexFile],
    super_methods: &[TestMethod],
) -> DexResult<Vec<TestMethod>> {
    let dex = &dex_files[result.dex_index];
    let class_name = format_class_name(dex, &result.class_def)?;
    let directory = annotations_directory(dex, &result.class_def)?;
    let child_annotations = class_annotation_values(dex, directory.as_ref())?;
    let child_names: Vec<&String> = child_annotations.iter().map(|a| &a.name).collect();
    let own_method_names: Vec<String> = result
        .test_methods
        .iter()
        .map(|m| test_name_without_class(&m.test_name))
        .collect();
    let super_method_names: Vec<String> = super_methods
        .iter()
        .map(|m| test_name_without_class(&m.test_name))
        .collect();

    let mut combined: Vec<TestMethod> = Vec::new();
    // Inherited (not overridden) superclass methods, renamed to the
    // subclass, parent annotations dropped where the child class
    // redeclares the same annotation name.
    for method in super_methods {
        if own_method_names.contains(&test_name_without_class(&method.test_name)) {
            continue;
        }
        let mut annotations: Vec<TestAnnotation> = method
            .annotations
            .iter()
            .filter(|a| !child_names.contains(&&a.name))
            .cloned()
            .collect();
        annotations.extend(child_annotations.iter().cloned());
        let mut test_name = class_name.clone();
        test_name.push_str(&test_name_without_class(&method.test_name));
        push_unique(&mut combined, TestMethod { test_name, annotations });
    }
    // Overridden methods: only @Inherited-marked superclass annotations
    // survive, ahead of the method's own.
    for method in &result.test_methods {
        let without = test_name_without_class(&method.test_name);
        if !super_method_names.contains(&without) {
            continue;
        }
        let super_method = super_methods
            .iter()
            .find(|m| test_name_without_class(&m.test_name) == without);
        let mut annotations: Vec<TestAnnotation> = super_method
            .map(|m| {
                m.annotations
                    .iter()
                    .filter(|a| a.inherited && !child_names.contains(&&a.name))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        annotations.extend(method.annotations.iter().cloned());
        push_unique(
            &mut combined,
            TestMethod {
                test_name: method.test_name.clone(),
                annotations,
            },
        );
    }
    // Methods with no superclass counterpart, unchanged.
    for method in &result.test_methods {
        if !super_method_names.contains(&test_name_without_class(&method.test_name)) {
            push_unique(&mut combined, method.clone());
        }
    }

    Ok(combined)
}

// ---- JUnit3 (JUnit3Extensions.kt) -----------------------------------------

const DEFAULT_DESCRIPTORS: [&str; 15] = [
    "Ljunit/framework/TestCase;",
    "Landroid/test/ActivityInstrumentationTestCase;",
    "Landroid/test/ActivityInstrumentationTestCase2;",
    "Landroid/test/ActivityTestCase;",
    "Landroid/test/ActivityUnitTestCase;",
    "Landroid/test/AndroidTestCase;",
    "Landroid/test/ApplicationTestCase;",
    "Landroid/test/FailedToCreateTests;",
    "Landroid/test/InstrumentationTestCase;",
    "Landroid/test/LoaderTestCase;",
    "Landroid/test/ProviderTestCase;",
    "Landroid/test/ProviderTestCase2;",
    "Landroid/test/ServiceTestCase;",
    "Landroid/test/SingleLaunchActivityTestCase;",
    "Landroid/test/SyncBaseInstrumentation;",
];

/// findJUnit3Tests. WP 6.4 approved P4 (iterative fixpoint) on the
/// premise that the result SET is identical — a JVM probe disproved it:
/// TestMethods are re-created on every re-scan, DecodedArrayValue
/// equality is array identity, so the drop-last recursion emits
/// duplicates for any test carrying an array-valued annotation (8
/// duplicated tests in abstract-class-in-second-dex.apk). The recursion
/// shape is therefore replicated exactly; see docs/wp/6.4-findings.md.
fn find_junit3_tests(dex_files: &[DexFile]) -> DexResult<Vec<TestMethod>> {
    let mut descriptors: Vec<String> = DEFAULT_DESCRIPTORS.iter().map(|d| java_string(d)).collect();
    let mut test_names: Vec<TestMethod> = Vec::new();
    for prefix in (1..=dex_files.len()).rev() {
        for dex in &dex_files[..prefix] {
            for method in find_junit3_tests_in_dex(dex, &mut descriptors)? {
                push_unique(&mut test_names, method);
            }
        }
    }
    Ok(test_names)
}

fn find_junit3_tests_in_dex(dex: &DexFile, descriptors: &mut Vec<String>) -> DexResult<Vec<TestMethod>> {
    // findClassesWithSuperClass: direct-superclass match against the
    // growing set; matching classes add their own descriptor immediately
    // (later classes in the same dex can chain off it).
    let mut test_classes: Vec<ClassDefItem> = Vec::new();
    for class_def in &dex.class_defs {
        if has_direct_super_class(dex, class_def, descriptors)? {
            test_classes.push(*class_def);
            let descriptor = parse_descriptor(dex, dex.type_id(class_def.class_idx)?)?;
            if !descriptors.contains(&descriptor) {
                descriptors.push(descriptor);
            }
        }
    }
    let all_finder = |dex: &DexFile,
                      class_def: &ClassDefItem,
                      _: Option<&AnnotationsDirectoryItem>|
     -> DexResult<Vec<MethodIdItem>> {
        let mut out = Vec::new();
        for idx in find_method_idxs(dex, class_def)? {
            out.push(dex.method_id(idx)?);
        }
        Ok(out)
    };
    let needle: Vec<u16> = "#test".encode_utf16().collect();
    let mut out = Vec::new();
    for class_def in &test_classes {
        // No visibility/arity/abstract checks: an abstract TestCase
        // subclass reports its own #test… methods (kept deliberately).
        for method in create_test_methods(dex, class_def, &all_finder)? {
            if method
                .test_name
                .encode_utf16()
                .collect::<Vec<_>>()
                .windows(needle.len())
                .any(|w| w == needle.as_slice())
            {
                out.push(method);
            }
        }
    }
    Ok(out)
}

fn has_direct_super_class(dex: &DexFile, class_def: &ClassDefItem, descriptors: &[String]) -> DexResult<bool> {
    if class_def.superclass_idx == DexFile::NO_INDEX {
        return Ok(false);
    }
    let super_descriptor = parse_descriptor(dex, dex.type_id(class_def.superclass_idx)?)?;
    Ok(descriptors.contains(&super_descriptor))
}
