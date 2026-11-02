//! Unit tests for behaviours no APK fixture reaches (WP 6.4 brief):
//! magic/endian rejection, padded signed values, boolean valueArg,
//! unsupported encoded values, formatDescriptor on non-`L…;` input,
//! header and table edges, superclass chains, and malformed input never
//! panicking. Each test that needs files makes its own temp directory.

use marathon_dex::{
    parser::format_descriptor,
    spec::{Cursor, DexFile, EncodedValue},
};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn probe_dex_bytes() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dex/probe.apk");
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut bytes = Vec::new();
    archive.by_name("classes.dex").unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn dex_040_magic_is_rejected_with_dex_magic_message() {
    let mut bytes = probe_dex_bytes();
    bytes[4..7].copy_from_slice(b"040");
    let err = DexFile::parse(bytes).map(|_| ()).unwrap_err();
    assert_eq!(err.jvm_class.to_string_lossy(), "com.linkedin.dex.spec.DexException");
    assert_eq!(
        err.message.unwrap().to_string_lossy(),
        "Invalid dexMagic:\nDexMagic(dex=[100, 101, 120], newline=10, version=[48, 52, 48], zero=0)\n"
    );
}

#[test]
fn dex_039_magic_is_accepted_and_035_untouched() {
    let mut bytes = probe_dex_bytes();
    assert!(DexFile::parse(bytes.clone()).is_ok());
    bytes[4..7].copy_from_slice(b"039");
    assert!(DexFile::parse(bytes).is_ok());
}

#[test]
fn bad_endian_tag_is_rejected() {
    let mut bytes = probe_dex_bytes();
    bytes[40..44].copy_from_slice(&0x7856_3412_i32.to_le_bytes());
    let err = DexFile::parse(bytes).map(|_| ()).unwrap_err();
    assert_eq!(
        err.message.unwrap().to_string_lossy(),
        format!("Invalid endian tag:{}", 0x7856_3412_i32)
    );
}

fn encoded(bytes: &[u8]) -> EncodedValue {
    let mut cursor = Cursor::at(bytes, 0).unwrap();
    EncodedValue::parse(&mut cursor, 0).unwrap()
}

/// P2 (rejected simplification): getPaddedBuffer zero-pads signed types,
/// so a one-byte -1 decodes as 255 — confirmed on the JVM in
/// fixtures/oracle/dex/probes.jsonl and pinned here.
#[test]
fn negative_short_int_long_are_zero_padded_not_sign_extended() {
    // valueArg 0 (1 byte) in the top 3 bits, type in the low 5.
    assert!(matches!(encoded(&[0x02, 0xff]), EncodedValue::Short(255)));
    assert!(matches!(encoded(&[0x04, 0xff]), EncodedValue::Int(255)));
    assert!(matches!(encoded(&[0x06, 0xff]), EncodedValue::Long(255)));
    // Full-width encodings keep the sign.
    assert!(matches!(
        encoded(&[0x04 | (3 << 5), 0x00, 0x00, 0x00, 0x80]),
        EncodedValue::Int(i32::MIN)
    ));
}

#[test]
fn float_and_double_are_right_padded() {
    // Float -1.5 == 0xBFC00000, stored as its 2 high bytes (valueArg 1).
    let EncodedValue::Float(v) = encoded(&[0x10 | (1 << 5), 0xc0, 0xbf]) else {
        panic!("float")
    };
    assert_eq!(v, -1.5f32);
    let EncodedValue::Double(v) = encoded(&[0x11 | (7 << 5), 0, 0, 0, 0, 0, 0, 0x04, 0xc0]) else {
        panic!("double")
    };
    assert_eq!(v, -2.5f64);
}

#[test]
fn boolean_is_true_only_for_value_arg_one() {
    assert!(matches!(encoded(&[0x1f | (1 << 5)]), EncodedValue::Boolean(true)));
    assert!(matches!(encoded(&[0x1f]), EncodedValue::Boolean(false)));
    assert!(matches!(encoded(&[0x1f | (2 << 5)]), EncodedValue::Boolean(false)));
    assert!(matches!(encoded(&[0x1f | (7 << 5)]), EncodedValue::Boolean(false)));
}

#[test]
fn unknown_value_type_is_bad_value_type_error() {
    let mut cursor = Cursor::at(&[0x05], 0).unwrap();
    let err = EncodedValue::parse(&mut cursor, 0).unwrap_err();
    assert_eq!(err.message.unwrap().to_string_lossy(), "Bad value type: 5");
}

#[test]
fn format_descriptor_blindly_drops_first_and_last_unit() {
    assert_eq!(
        format_descriptor(&"Lorg/junit/Test;".into()).unwrap().to_string_lossy(),
        "org.junit.Test"
    );
    // Not an L…; descriptor: the first and last characters still go.
    assert_eq!(format_descriptor(&"I".into()).unwrap().to_string_lossy(), "");
    assert_eq!(format_descriptor(&"[I".into()).unwrap().to_string_lossy(), "");
    assert_eq!(format_descriptor(&"XYZ".into()).unwrap().to_string_lossy(), "Y");
    assert!(format_descriptor(&"".into()).is_err());
}

/// One stored classes.dex entry, as a build tool would write it.
fn write_apk(dir: &Path, name: &str, dex: &[u8]) -> PathBuf {
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("classes.dex", options).unwrap();
    zip.write_all(dex).unwrap();
    zip.finish().unwrap();
    path
}

/// Every prefix truncation of a real dex must be an error (or, at a few
/// lengths that still contain a whole header + empty tables, an error
/// from a later read) — never a panic, hang, or huge allocation.
#[test]
fn truncated_dex_never_panics() {
    let bytes = probe_dex_bytes();
    for len in (0..bytes.len()).step_by(7) {
        let _ = DexFile::parse(bytes[..len].to_vec());
    }
    // Corrupt counts/offsets in place: header fields are i32s from
    // offset 32 on; blast them with extremes.
    for off in (32..112).step_by(4) {
        for value in [i32::MIN, -1, i32::MAX] {
            let mut mutated = bytes.clone();
            mutated[off..off + 4].copy_from_slice(&value.to_le_bytes());
            let _ = DexFile::parse(mutated);
        }
    }
}

/// The same hostile bytes through full discovery (JUnit3 and JUnit4,
/// annotations, superclass walks): every prefix of the probe dex, and
/// seeded random byte overwrites, must return, never panic.
#[test]
fn truncated_and_garbled_dex_through_discovery_never_panics() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = probe_dex_bytes();
    for len in 0..bytes.len() {
        let apk = write_apk(dir.path(), "truncated.apk", &bytes[..len]);
        let _ = marathon_dex::find_test_methods(&apk, &[]);
    }
    // xorshift64: a fixed, dependency-free sequence.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..3000 {
        let mut mutated = bytes.clone();
        for _ in 0..1 + next() % 4 {
            let at = (next() % mutated.len() as u64) as usize;
            mutated[at] = match next() % 4 {
                0 => 0x00,
                1 => 0xff,
                2 => 0x80,
                _ => next() as u8,
            };
        }
        let apk = write_apk(dir.path(), "garbled.apk", &mutated);
        let _ = marathon_dex::find_test_methods(&apk, &[]);
    }
}

/// A synthetic dex: `count` classes `LCn;`, each extending the previous
/// one (the first extends `LC0;`'s own predecessor, absent from the
/// file), all abstract but the last; no class data, no annotations.
/// `super_of` overrides the superclass type index of class n.
fn chain_dex(count: usize, super_of: impl Fn(usize) -> i32) -> Vec<u8> {
    const HEADER: usize = 0x70;
    let string_ids_off = HEADER;
    let type_ids_off = string_ids_off + 4 * (count + 1);
    let class_defs_off = type_ids_off + 4 * (count + 1);
    let data_off = class_defs_off + 32 * count;
    let mut data = Vec::new();
    let mut string_offsets = Vec::new();
    for n in 0..=count {
        string_offsets.push((data_off + data.len()) as i32);
        let text = format!("LC{n};");
        data.push(text.len() as u8); // utf16 size; every name is short
        data.extend_from_slice(text.as_bytes());
        data.push(0);
    }
    let mut dex = vec![0u8; HEADER];
    dex[..8].copy_from_slice(b"dex\n035\0");
    let mut put = |at: usize, value: usize| dex[at..at + 4].copy_from_slice(&(value as i32).to_le_bytes());
    put(40, 0x1234_5678);
    put(56, count + 1);
    put(60, string_ids_off);
    put(64, count + 1);
    put(68, type_ids_off);
    put(96, count);
    put(100, class_defs_off);
    for offset in &string_offsets {
        dex.extend_from_slice(&offset.to_le_bytes());
    }
    for n in 0..=count {
        dex.extend_from_slice(&(n as i32).to_le_bytes()); // type n -> string n
    }
    for n in 1..=count {
        let access: i32 = if n == count { 0x1 } else { 0x401 }; // public (abstract)
        let fields = [n as i32, access, super_of(n), 0, -1, 0, 0, 0];
        for field in fields {
            dex.extend_from_slice(&field.to_le_bytes());
        }
    }
    dex.extend_from_slice(&data);
    dex
}

/// createAllTestMethods walks the superclass chain without recursing,
/// so a chain far deeper than any stack holds still completes.
#[test]
fn deep_superclass_chain_does_not_exhaust_the_stack() {
    let dir = tempfile::tempdir().unwrap();
    let apk = write_apk(dir.path(), "chain.apk", &chain_dex(200_000, |n| n as i32 - 1));
    assert_eq!(marathon_dex::find_test_methods(&apk, &[]).unwrap().len(), 0);
}

/// A two-class superclass cycle is StackOverflowError on the JVM (the
/// self-cycle is probed in probes.jsonl); here it is a DexError (P3).
#[test]
fn superclass_cycle_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let apk = write_apk(dir.path(), "cycle.apk", &chain_dex(2, |n| if n == 1 { 2 } else { 1 }));
    let err = marathon_dex::find_test_methods(&apk, &[]).unwrap_err();
    assert_eq!(err.jvm_class.to_string_lossy(), "com.linkedin.dex.spec.DexException");
}

/// HeaderItem reads dataSize/dataOff too: a file that ends inside them
/// (0x68..0x6F bytes) underflows on the JVM, and is an error here.
#[test]
fn dex_ending_inside_the_header_is_rejected() {
    let bytes = probe_dex_bytes();
    for len in 0x68..0x70 {
        assert!(DexFile::parse(bytes[..len].to_vec()).is_err(), "length {len:#x}");
    }
    let mut header_only = bytes[..0x70].to_vec();
    for field in (56..112).step_by(4) {
        header_only[field..field + 4].copy_from_slice(&0i32.to_le_bytes());
    }
    assert!(DexFile::parse(header_only).is_ok());
}

/// An empty id table is never positioned upstream, so a garbage offset
/// next to a zero count is accepted.
#[test]
fn empty_table_ignores_its_offset() {
    let mut bytes = probe_dex_bytes();
    // proto_ids: size @72, off @76; nothing reads protos during parse.
    bytes[72..76].copy_from_slice(&0i32.to_le_bytes());
    bytes[76..80].copy_from_slice(&(-5i32).to_le_bytes());
    assert!(DexFile::parse(bytes.clone()).is_ok());
    bytes[72..76].copy_from_slice(&1i32.to_le_bytes());
    assert!(DexFile::parse(bytes).is_err());
}

/// Truncated and garbage APK streams must not panic discovery or the
/// manifest reader.
#[test]
fn garbage_and_truncated_apks_never_panic() {
    let dir = tempfile::tempdir().unwrap();
    for (name, contents) in [
        ("empty.apk", Vec::new()),
        ("text.apk", b"this is not a zip file at all".to_vec()),
        (
            "prefix.apk",
            std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dex/probe.apk")).unwrap()
                [..64]
                .to_vec(),
        ),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        let _ = marathon_dex::find_test_methods(&path, &[]);
        let _ = marathon_dex::parse_instrumentation_info(&path);
        let _ = marathon_dex::parse_app_package_name(&path);
    }
}

/// ZipInputStream tolerance: a stream that is not a zip at all yields
/// no entries and therefore no tests (and no error) for discovery,
/// while ApkParser's ZipFile rejects it inside the IOException wrap.
#[test]
fn non_zip_apk_yields_no_tests_but_manifest_parse_fails_wrapped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("text.apk");
    std::fs::write(&path, b"this is not a zip file at all").unwrap();
    assert_eq!(marathon_dex::find_test_methods(&path, &[]).unwrap().len(), 0);
    let err = marathon_dex::parse_app_package_name(&path).unwrap_err();
    assert_eq!(err.jvm_class.to_string_lossy(), "java.lang.RuntimeException");
    assert_eq!(
        err.message.unwrap().to_string_lossy(),
        "Unable to parse test app AndroidManifest.xml."
    );
}

/// DEX string bytes decode exactly as dex-test-parser's `String(bytes)`
/// does on the reference JDK (default charset UTF-8, JEP 400): one
/// U+FFFD per maximal invalid subpart, and an encoded surrogate (`ED`
/// then `A0..BF`) is ONE U+FFFD whether complete or truncated. The table
/// is JVM-generated by tools/oracle/dex/StringBytesProbe.java.
#[test]
fn dex_string_bytes_decode_as_jvm_string_constructor() {
    let table = include_str!("../../../fixtures/oracle/dex/string-bytes.txt");
    let mut checked = 0;
    for line in table.lines() {
        let (hex, units) = line.split_once(' ').unwrap();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let expected: Vec<u16> = units.split(',').map(|u| u16::from_str_radix(u, 16).unwrap()).collect();
        assert_eq!(
            marathon_dex::parser::decode_string_bytes(&bytes).0,
            expected,
            "bytes {hex}"
        );
        checked += 1;
    }
    assert_eq!(checked, 11420);
}

fn manifest_bytes(apk: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(apk);
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut bytes = Vec::new();
    archive
        .by_name("AndroidManifest.xml")
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

/// Drives the parser the way ApkParser does, reading every name and
/// attribute; returns whether the document ended cleanly.
fn walk_axml(bytes: &[u8]) -> Result<(), marathon_dex::DexError> {
    use marathon_dex::axml::{AxmlParser, END_DOCUMENT};
    let mut parser = AxmlParser::new(bytes)?;
    loop {
        let event = parser.next()?;
        if event == END_DOCUMENT {
            return Ok(());
        }
        parser.name()?;
        for index in 0..parser.attribute_count().max(0) {
            parser.attribute_name(index)?;
            parser.attribute_value_string(index)?;
        }
    }
}

/// A valid AXML header followed by zeros: upstream's skipCheckChunkType
/// recurses once per int (StackOverflowError on the JVM for long runs);
/// the loop here reads to EOF and returns an error, even for 64 MiB.
#[test]
fn axml_header_followed_by_zeros_is_an_error() {
    for zeros in [400_000usize, 64 << 20] {
        let mut bytes = Vec::with_capacity(8 + zeros);
        bytes.extend_from_slice(&0x0008_0003i32.to_le_bytes());
        bytes.extend_from_slice(&((8 + zeros) as i32).to_le_bytes());
        bytes.resize(8 + zeros, 0);
        let err = walk_axml(&bytes).unwrap_err();
        assert_eq!(err.jvm_class.to_string_lossy(), "java.io.EOFException", "{zeros} zeros");
    }
}

/// A UTF-16 pool length prefix of `ff ff ff ff`: len_value * 2 overflows
/// to -2 as in Java, and the range check rejects it (no arithmetic panic).
#[test]
fn axml_utf16_length_overflow_is_an_error() {
    let mut bytes = manifest_bytes("dex/mutants/manifest-unpooled-package.apk");
    let read = |b: &[u8], at: usize| i32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
    // Pool header at 8: stringsOffset at +20, the offset table at +28;
    // string 2 is "manifest", the first start tag's name.
    let strings_offset = read(&bytes, 28);
    let manifest_offset = read(&bytes, 36 + 2 * 4);
    let at = 8 + strings_offset + manifest_offset;
    bytes[at..at + 4].copy_from_slice(&[0xff; 4]);
    let err = walk_axml(&bytes).unwrap_err();
    assert_eq!(err.jvm_class.to_string_lossy(), "java.lang.IndexOutOfBoundsException");
}

/// Every aligned int of a real manifest overwritten with extreme values
/// (string/attribute counts, offsets, chunk sizes, pool indices): the
/// reader returns or fails, never panics or preallocates the count.
#[test]
fn axml_extreme_ints_never_panic() {
    let original = manifest_bytes("vendor-android/android_test_1.apk");
    for at in (0..original.len() - 3).step_by(4) {
        for value in [i32::MIN, -2, -1, i32::MAX, 0x7fff_fffe, 0x4000_0000] {
            let mut bytes = original.clone();
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            let _ = walk_axml(&bytes);
        }
    }
    assert!(walk_axml(&original).is_ok());
}
