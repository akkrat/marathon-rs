//! D26 (N10): DEX 040 and 041, against real d8 output (`tools/oracle/dex/build-dex-versions.sh`).
use marathon_dex::{find_test_methods, spec::DexFile};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn apk(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dex/versions")
        .join(format!("{name}.apk"))
}
fn classes_dex(name: &str) -> Vec<u8> {
    let file = std::fs::File::open(apk(name)).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut bytes = Vec::new();
    archive.by_name("classes.dex").unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}
/// The tests found with their annotations, in an order that does not depend on how d8 laid out the classes.
fn tests(path: &Path) -> Vec<String> {
    let mut rendered: Vec<String> = find_test_methods(path, &[])
        .unwrap()
        .iter()
        .map(|method| format!("{} {:?}", method.test_name.to_owned(), method.annotations))
        .collect();
    rendered.sort();
    rendered
}
#[test]
fn the_fixtures_are_what_they_claim_to_be() {
    for (name, version) in [
        ("dex039", b"039"),
        ("dex040", b"040"),
        ("dex041", b"041"),
        ("dex041-two-images", b"041"),
    ] {
        assert_eq!(&classes_dex(name)[4..7], version, "{name}");
    }
}
#[test]
fn newer_versions_give_the_same_tests_as_039() {
    let expected = tests(&apk("dex039"));
    assert!(expected.iter().any(|t| t.contains("ProbeTest")), "{expected:?}");
    // In the container these two classes are in the second image, and the others in the first.
    assert!(
        expected.iter().any(|t| t.starts_with("probe.ChainMiddle#")),
        "{expected:?}"
    );
    assert!(
        expected.iter().any(|t| t.starts_with("probe.InheritanceProbe#")),
        "{expected:?}"
    );
    for name in ["dex040", "dex041", "dex041-two-images"] {
        assert_eq!(tests(&apk(name)), expected, "{name}");
    }
}
/// Reading only the first image would lose the tests of the second, the trap of accepting the magic alone.
#[test]
fn every_image_of_a_container_counts() {
    let images = DexFile::parse_all(classes_dex("dex041-two-images")).unwrap();
    assert_eq!(images.len(), 2);
    assert!(images.iter().all(|image| !image.class_defs.is_empty()));
    let classes: usize = images.iter().map(|image| image.class_defs.len()).sum();
    let whole = DexFile::parse_all(classes_dex("dex039")).unwrap();
    assert_eq!(whole.len(), 1);
    assert_eq!(classes, whole[0].class_defs.len());
    let first_only = DexFile::parse(classes_dex("dex041-two-images")).unwrap();
    assert!(first_only.class_defs.len() < classes);
}
#[test]
fn a_container_read_through_a_stored_zip_entry_also_works() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stored.apk");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("classes.dex", options).unwrap();
    writer.write_all(&classes_dex("dex041-two-images")).unwrap();
    writer.finish().unwrap();
    assert_eq!(tests(&path), tests(&apk("dex039")));
}
fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn invalid(bytes: Vec<u8>, message: &str) {
    let error = DexFile::parse_all(bytes).map(|_| ()).unwrap_err();
    assert_eq!(error.jvm_class.to_owned(), "com.linkedin.dex.spec.DexException");
    let text = error.message.unwrap().to_owned();
    assert!(text.contains(message), "{text}");
}
const FILE_SIZE: usize = 0x20;
const HEADER_SIZE: usize = 0x24;
const CONTAINER_SIZE: usize = 0x70;
const HEADER_OFFSET: usize = 0x74;
#[test]
fn a_malformed_container_is_a_dex_exception_and_not_a_hang_or_panic() {
    let good = classes_dex("dex041-two-images");
    let second = i32::from_le_bytes(good[FILE_SIZE..FILE_SIZE + 4].try_into().unwrap()) as usize;
    let mut bytes = good.clone();
    put_i32(&mut bytes, HEADER_SIZE, 0x70);
    invalid(bytes, "header size");
    let mut bytes = good.clone();
    put_i32(&mut bytes, FILE_SIZE, 0);
    invalid(bytes, "has size 0");
    let mut bytes = good.clone();
    put_i32(&mut bytes, FILE_SIZE, i32::MAX);
    invalid(bytes, "has size");
    let mut bytes = good.clone();
    put_i32(&mut bytes, CONTAINER_SIZE, 1);
    invalid(bytes, "in a file of");
    let mut bytes = good.clone();
    put_i32(&mut bytes, second + HEADER_OFFSET, 0);
    invalid(bytes, "says it is at 0");
    let mut bytes = good.clone();
    bytes[second + 4..second + 7].copy_from_slice(b"040");
    invalid(bytes, "is not version 041");
    let mut bytes = good.clone();
    bytes[second + 4..second + 7].copy_from_slice(b"042");
    invalid(bytes, "Invalid dexMagic");
    // Truncated in the second image: its size no longer fits the file.
    invalid(good[..good.len() - 1].to_vec(), "in a file of");
}
