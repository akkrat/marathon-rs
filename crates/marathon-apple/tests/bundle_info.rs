//! New Rust tests (no Kotlin counterpart): BundleInfo and Mach-O detection against the
//! real bundle entries of fixtures/bundles/{ios,macos}.zip (binary and XML Info.plists,
//! real extensionless executables). PLAN §6.4 host-side coverage for WP 4.5a.

use std::io::Read;
use std::path::PathBuf;

use marathon_apple::mach_o::{is_apple_binary, is_apple_binary_reader};
use marathon_apple::plist::PropertyList;
use marathon_apple::plist::bundle::BundleInfo;

fn bundle_zip(which: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/bundles")
        .join(which);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Minimal single-entry zip reader for the fixtures: finds the central-directory record
/// for `name` and inflates it. Test-only; the fixtures use plain deflate, no zip64.
fn zip_entry(zip: &[u8], name: &str) -> Vec<u8> {
    fn u16_at(b: &[u8], off: usize) -> usize {
        usize::from(u16::from_le_bytes(b[off..off + 2].try_into().expect("u16")))
    }
    fn u32_at(b: &[u8], off: usize) -> usize {
        u32::from_le_bytes(b[off..off + 4].try_into().expect("u32")) as usize
    }
    let eocd = zip
        .windows(4)
        .rposition(|w| w == b"PK\x05\x06")
        .expect("end of central directory");
    let mut pos = u32_at(zip, eocd + 16);
    let count = u16_at(zip, eocd + 10);
    for _ in 0..count {
        assert_eq!(&zip[pos..pos + 4], b"PK\x01\x02", "central directory header");
        let method = u16_at(zip, pos + 10);
        let compressed_size = u32_at(zip, pos + 20);
        let name_len = u16_at(zip, pos + 28);
        let extra_len = u16_at(zip, pos + 30);
        let comment_len = u16_at(zip, pos + 32);
        let local_header = u32_at(zip, pos + 42);
        let entry_name = &zip[pos + 46..pos + 46 + name_len];
        if entry_name == name.as_bytes() {
            assert_eq!(&zip[local_header..local_header + 4], b"PK\x03\x04", "local header");
            let data_start = local_header + 30 + u16_at(zip, local_header + 26) + u16_at(zip, local_header + 28);
            let compressed = &zip[data_start..data_start + compressed_size];
            return match method {
                0 => compressed.to_vec(),
                8 => {
                    let mut out = Vec::new();
                    flate2::read::DeflateDecoder::new(compressed)
                        .read_to_end(&mut out)
                        .expect("inflate");
                    out
                }
                other => panic!("unsupported zip method {other}"),
            };
        }
        pos += 46 + name_len + extra_len + comment_len;
    }
    panic!("zip entry {name} not found");
}

#[test]
fn bundle_info_reads_real_binary_info_plist() {
    let zip = bundle_zip("ios.zip");
    let bytes = zip_entry(&zip, "sample-app.app/Info.plist");
    assert!(bytes.starts_with(b"bplist00"), "the iOS app plist is binary");
    let info = BundleInfo::from_bytes(&bytes).expect("parse binary plist");
    assert_eq!(info.naming().bundle_name().expect("CFBundleName"), "sample-app");
    let identifier = info.identification().bundle_identifier().expect("CFBundleIdentifier");
    assert!(identifier.contains("sample-app"), "identifier: {identifier}");
    assert_eq!(
        info.undocumented().ui_device_family().expect("device family"),
        vec![1, 2]
    );
    assert_eq!(
        info.undocumented().bundle_supported_platforms().expect("platforms"),
        vec!["iPhoneSimulator".to_owned()]
    );
    assert_eq!(info.categorization().package_type().expect("package type"), "APPL");
}

#[test]
fn bundle_info_reads_real_xml_info_plists() {
    let zip = bundle_zip("macos.zip");
    let bytes = zip_entry(&zip, "AppleWorld.app/Contents/Info.plist");
    assert!(bytes.starts_with(b"<?xml"), "the macOS app plist is XML");
    let info = BundleInfo::from_bytes(&bytes).expect("parse xml plist");
    assert_eq!(info.naming().bundle_name().expect("CFBundleName"), "AppleWorld");
    let identifier = info.identification().bundle_identifier().expect("CFBundleIdentifier");
    assert!(identifier.contains("AppleWorld"), "identifier: {identifier}");
    assert_eq!(
        info.undocumented().bundle_supported_platforms().expect("platforms"),
        vec!["MacOSX".to_owned()]
    );
    // The macOS minimum-version key 4.5b reads (A28 surface: present here, so no throw).
    assert_eq!(
        info.operating_system_version()
            .ls_minimum_system_version()
            .expect("lsMinimum"),
        "13.0"
    );

    let zip = bundle_zip("ios.zip");
    let runner = zip_entry(&zip, "sample-appUITests-Runner.app/Info.plist");
    assert!(runner.starts_with(b"<?xml"), "the UI-test runner plist is XML");
    let info = BundleInfo::from_bytes(&runner).expect("parse runner plist");
    assert_eq!(
        info.naming().bundle_name().expect("runner name"),
        "sample-appUITests-Runner"
    );
}

#[test]
fn mach_o_detection_on_real_bundle_executables() {
    let zip = bundle_zip("ios.zip");
    for entry in [
        "sample-app.app/sample-app",
        "sample-app.app/PlugIns/sample-appUnitTests.xctest/sample-appUnitTests",
        "sample-appUITests-Runner.app/sample-appUITests-Runner",
    ] {
        let bytes = zip_entry(&zip, entry);
        assert!(is_apple_binary(&bytes), "{entry} must be detected");
        assert!(is_apple_binary_reader(&bytes[..]).expect("read"), "{entry} via reader");
    }
    let mac = bundle_zip("macos.zip");
    let app = zip_entry(&mac, "AppleWorld.app/Contents/MacOS/AppleWorld");
    assert!(is_apple_binary(&app));
    // Non-binaries inside the same bundles are not detected.
    let plist = zip_entry(&mac, "AppleWorld.app/Contents/Info.plist");
    assert!(!is_apple_binary(&plist));
    let pkg_info = zip_entry(&mac, "AppleWorld.app/Contents/PkgInfo");
    assert!(!is_apple_binary(&pkg_info));
}

#[test]
fn mach_o_magic_set_is_exactly_kotlins_six() {
    use marathon_apple::mach_o::{FAT_CIGAM, FAT_MAGIC, MH_CIGAM, MH_CIGAM_64, MH_MAGIC, MH_MAGIC_64};
    for magic in [FAT_MAGIC, FAT_CIGAM, MH_MAGIC, MH_CIGAM, MH_MAGIC_64, MH_CIGAM_64] {
        assert!(is_apple_binary(&magic));
        let mut longer = magic.to_vec();
        longer.extend_from_slice(&[0u8; 16]);
        assert!(is_apple_binary(&longer));
    }
    // The 64-bit fat magic is absent from the Kotlin set (kept under D5).
    assert!(!is_apple_binary(&[0xca, 0xfe, 0xba, 0xbf]));
    assert!(!is_apple_binary(&[0xbf, 0xba, 0xfe, 0xca]));
    // Short and empty inputs are not binaries.
    assert!(!is_apple_binary(&[]));
    assert!(!is_apple_binary(&[0xfe, 0xed, 0xfa]));
    assert!(!is_apple_binary_reader(&b"\xfe\xed"[..]).expect("read"));
    assert!(!is_apple_binary(&[0xff, 0xed, 0xfa, 0xcf]));
}
