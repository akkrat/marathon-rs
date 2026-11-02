//! Port of configuration/src/test/kotlin/com/malinskiy/marathon/config/vendor/ios/AppleTestBundleConfigurationTest.kt
//! and config/vendor/IOSConfigurationTest.kt (the six rows WP 2.3a
//! assigned here because every parse routes through `bundle.validate()`;
//! the back-off row is WP 2.3a's in tests/vendor.rs).
//!
//! The IOS rows run at section level, as WP 2.3a's vendor rows do: the
//! fixture's `vendorConfiguration` subtree binds, then WP 2.3a's split
//! post-processing runs with the real `bundle.validate()` in its factory
//! position. The assembled `ConfigurationFactory.parse` is WP 2.4's.
//! Plus the JVM probe comparison (`fixtures/oracle/bundle-probe.jsonl`).

use jcompat::{
    file_manager::JavaPath,
    jtime::{Duration, instant_of},
    writers::JavaString,
};
use marathon_config::{
    VendorConfiguration, WorkerConnectionConfiguration,
    bind_vendor::bind_vendor_configuration,
    binder::{Binder, Result as BindResult, Value},
    binding::{BindingContext, ConfigurationError, Profile, ValidationStage},
    loader::validation_error_to_configuration_error,
    node,
    vendor::{AppleTestBundleConfiguration, IosConfiguration, post_process_ios_with_bundle_validation},
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

fn ios_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/configuration/fixture/config/ios")
}

fn jp(path: &Path) -> JavaPath {
    JavaPath::from_text(&path.to_string_lossy())
}

fn path_text(path: &JavaPath) -> String {
    String::from_utf16_lossy(&path.0)
}

fn bundle(
    application: Option<JavaPath>,
    test_application: Option<JavaPath>,
    derived: Option<JavaPath>,
) -> AppleTestBundleConfiguration {
    AppleTestBundleConfiguration::unresolved(application, test_application, None, derived, None)
}

/// The Kotlin test's injected `tempDirFor`: a fresh temp directory per
/// archive, named after it.
fn temp_dir_for(
    root: &Path,
) -> impl FnMut(&JavaPath) -> std::result::Result<JavaPath, marathon_config::ValidationError> {
    let root = root.to_path_buf();
    let mut counter = 0;
    move |file: &JavaPath| {
        counter += 1;
        let name = Path::new(&path_text(file))
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = root.join(format!("{name}-{counter}"));
        fs::create_dir_all(&dir).map_err(|e| marathon_config::ValidationError::io(e.to_string()))?;
        Ok(jp(&dir))
    }
}

// ---- AppleTestBundleConfigurationTest ---------------------------------------

#[test]
fn test_explicit_config() {
    let dir = ios_fixture_dir().join("derivedDataDir");
    let tmp = tempfile::tempdir().unwrap();
    let mut config = bundle(
        Some(jp(&dir.join("sample.app"))),
        Some(jp(&dir.join("ui.xctest"))),
        None,
    );
    config
        .validate_with_temp_dir_for(&mut temp_dir_for(tmp.path()))
        .unwrap();
    assert_eq!(
        config.app.as_ref().map(path_text),
        Some(dir.join("sample.app").to_string_lossy().into_owned())
    );
    // A .xctest test application leaves testApp null.
    assert_eq!(config.test_app, None);
    assert_eq!(
        config.xctest.as_ref().map(path_text),
        Some(dir.join("ui.xctest").to_string_lossy().into_owned())
    );
}

#[test]
fn test_derived_dir() {
    let dir = ios_fixture_dir().join("derivedDataDir");
    let tmp = tempfile::tempdir().unwrap();
    let mut config = bundle(None, None, Some(jp(&dir)));
    config
        .validate_with_temp_dir_for(&mut temp_dir_for(tmp.path()))
        .unwrap();
    assert_eq!(
        config.app.as_ref().map(path_text),
        Some(dir.join("sample.app").to_string_lossy().into_owned())
    );
    assert_eq!(
        config.xctest.as_ref().map(path_text),
        Some(dir.join("ui.xctest").to_string_lossy().into_owned())
    );
}

fn archive_pair(kind: &str) -> AppleTestBundleConfiguration {
    let dir = ios_fixture_dir().join(kind);
    let (app, test) = if kind == "ipa" {
        (dir.join("app.ipa"), dir.join("testApp.ipa"))
    } else {
        (dir.join("app.zip"), dir.join("testApp.zip"))
    };
    bundle(Some(jp(&app)), Some(jp(&test)), None)
}

#[test]
fn test_ipa() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = archive_pair("ipa");
    config
        .validate_with_temp_dir_for(&mut temp_dir_for(tmp.path()))
        .unwrap();
    assert!(config.app.as_ref().map(path_text).unwrap().ends_with("sample.app"));
    assert!(config.xctest.as_ref().map(path_text).unwrap().ends_with("ui.xctest"));
    assert_eq!(config.test_app, None);
}

#[test]
fn test_zip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = archive_pair("zip");
    config
        .validate_with_temp_dir_for(&mut temp_dir_for(tmp.path()))
        .unwrap();
    assert!(config.app.as_ref().map(path_text).unwrap().ends_with("sample.app"));
    assert!(config.xctest.as_ref().map(path_text).unwrap().ends_with("ui.xctest"));
    assert_eq!(config.test_app, None);
}

#[test]
fn real_ios_bundle_smoke() {
    // fixtures/bundles/ios.zip holds the real build products (brief's
    // smoke case): unpack to a disposable dir and validate explicit
    // .app folders from it, as a derived-data user would point at them.
    let tmp = tempfile::tempdir().unwrap();
    let archive =
        fs::File::open(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bundles/ios.zip")).unwrap();
    let mut zip = zip::ZipArchive::new(archive).unwrap();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let target = tmp.path().join(entry.name());
        if entry.is_dir() {
            fs::create_dir_all(&target).unwrap();
        } else {
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::io::copy(&mut entry, &mut fs::File::create(&target).unwrap()).unwrap();
        }
    }
    let mut config = bundle(
        Some(jp(&tmp.path().join("sample-app.app"))),
        Some(jp(&tmp.path().join("sample-appUITests-Runner.app"))),
        None,
    );
    config.validate().unwrap();
    assert_eq!(
        config.xctest.as_ref().map(path_text),
        Some(
            tmp.path()
                .join("sample-appUITests-Runner.app/PlugIns/sample-appUITests.xctest")
                .to_string_lossy()
                .into_owned()
        )
    );
    assert!(
        config
            .test_app
            .as_ref()
            .map(path_text)
            .unwrap()
            .ends_with("sample-appUITests-Runner.app")
    );
}

#[test]
fn test_missing() {
    let mut config = bundle(None, None, None);
    let error = config.validate().unwrap_err();
    assert_eq!(
        error.fqcn(),
        "com.malinskiy.marathon.config.exceptions.ConfigurationException"
    );
    assert_eq!(
        error.message,
        "please specify your application and test application either with files or provide derived data folder"
    );
}

// ---- IOSConfigurationTest (section level) -----------------------------------
// `ConfigurationFactory(marathonfileDir).parse` becomes: bind the vendor
// subtree with that marathonfile dir, then post-process with the real
// bundle validate() in its step-2 position (before devices/SSH canonical
// steps), as WP 2.4's assembled loader will.

fn context(dir: &Path) -> BindingContext {
    BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: jp(&dir.join("Marathonfile")),
        marathonfile_dir: jp(dir),
        process_dir: JavaPath::from_text("/"),
        environment: Vec::<(JavaString, JavaString)>::new(),
        now: instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    }
}

fn parse_ios(
    marathonfile_dir: &Path,
    extra_vendor_yaml: &str,
) -> std::result::Result<IosConfiguration, Box<ConfigurationError>> {
    let sample = fs::read_to_string(ios_fixture_dir().join("sample_1.yaml")).unwrap();
    let text = format!("{}{extra_vendor_yaml}", sample.trim_end());
    let ctx = context(marathonfile_dir);
    let stream = node::load(&text);
    let binder = Binder::new(&ctx, &stream);
    let vendor = binder
        .first(&stream, |v: Value<'_>| -> BindResult<Option<VendorConfiguration>> {
            let child = v.get("vendorConfiguration")?.expect("fixture has vendor");
            bind_vendor_configuration(child)
        })?
        .flatten()
        .expect("vendor binds");
    let VendorConfiguration::Ios(mut ios) = vendor else {
        panic!("expected iOS")
    };
    post_process_ios_with_bundle_validation(&mut ios, &ctx, |bundle| {
        bundle
            .validate()
            .map_err(|e| Box::new(validation_error_to_configuration_error(e, ValidationStage::Parse)))
    })?;
    Ok(ios)
}

#[test]
fn when_applications_are_not_specified_should_search_for_them_under_derived_data_folder() {
    let dir = ios_fixture_dir();
    let ios = parse_ios(&dir, "").unwrap();
    let xctest = ios.bundle.as_ref().unwrap().xctest.as_ref().unwrap();
    let expected = fs::canonicalize(dir.join("derivedDataDir/ui.xctest")).unwrap();
    assert_eq!(Path::new(&path_text(xctest)), expected.as_path());
}

#[test]
fn when_xctest_is_not_specified_should_throw_an_exception_if_such_file_is_not_available() {
    let tmp = tempfile::tempdir().unwrap();
    let error = parse_ios(tmp.path(), "").unwrap_err();
    assert_eq!(
        error.class.to_string_lossy(),
        "com.malinskiy.marathon.config.exceptions.ConfigurationException"
    );
}

#[test]
fn batches_without_progress_don_t_take_a_simulator_out_unless_configured() {
    let dir = ios_fixture_dir();
    assert_eq!(parse_ios(&dir, "").unwrap().max_batches_without_progress, 0);
    assert_eq!(
        parse_ios(&dir, "\n  maxBatchesWithoutProgress: 3\n")
            .unwrap()
            .max_batches_without_progress,
        3
    );
}

#[test]
fn negative_number_of_batches_without_progress_is_rejected() {
    let dir = ios_fixture_dir();
    let ios = parse_ios(&dir, "\n  maxBatchesWithoutProgress: -1\n").unwrap();
    let error = ios.validate().unwrap_err();
    assert_eq!(
        error.message,
        "maxBatchesWithoutProgress should not be negative, got -1"
    );
}

#[test]
fn worker_connections_are_retried_and_late_join_is_on_by_default() {
    let dir = ios_fixture_dir();
    let defaults = parse_ios(&dir, "").unwrap().worker_connection;
    assert_eq!(
        defaults,
        WorkerConnectionConfiguration {
            attempts: 3,
            backoff: Duration::of_seconds(10),
            retry_interval: Duration::of_seconds(60),
            late_join: true,
            late_join_cutoff: Duration::of_seconds(600),
        }
    );
    let configured = parse_ios(
        &dir,
        "\n  workerConnection:\n    attempts: 5\n    backoff: PT2S\n    retryInterval: PT30S\n    lateJoin: false\n    lateJoinCutoff: PT0S\n",
    )
    .unwrap()
    .worker_connection;
    assert_eq!(
        configured,
        WorkerConnectionConfiguration {
            attempts: 5,
            backoff: Duration::of_seconds(2),
            retry_interval: Duration::of_seconds(30),
            late_join: false,
            late_join_cutoff: Duration::of_seconds(0),
        }
    );
}

#[test]
fn invalid_worker_connection_settings_are_rejected() {
    let dir = ios_fixture_dir();
    let cases = [
        ("attempts: 0", "workerConnection.attempts should be at least 1, got 0"),
        (
            "backoff: PT-1S",
            "workerConnection.backoff should not be negative, got PT-1S",
        ),
        (
            "retryInterval: PT0S",
            "workerConnection.retryInterval should be positive, got PT0S",
        ),
        (
            "lateJoinCutoff: PT-1M",
            "workerConnection.lateJoinCutoff should not be negative, got PT-1M",
        ),
    ];
    for (setting, message) in cases {
        let ios = parse_ios(&dir, &format!("\n  workerConnection:\n    {setting}\n")).unwrap();
        assert_eq!(
            ios.worker_connection.validate().unwrap_err().message,
            message,
            "{setting}"
        );
    }
}

// ---- JVM probe comparison ---------------------------------------------------
// The same layouts BundleProbe.kt builds, against
// fixtures/oracle/bundle-probe.jsonl. S1/S6 rows are the two approved
// divergences; everything else must match class, message and tree.

fn mkdirs(parent: &Path, rel: &str) -> PathBuf {
    let dir = parent.join(rel);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn bundle_dir(parent: &Path, rel: &str) -> PathBuf {
    let dir = mkdirs(parent, rel);
    fs::write(dir.join("empty"), "").unwrap();
    dir
}

fn make_zip(parent: &Path, name: &str, entries: &[(&str, Option<&str>)]) -> PathBuf {
    let file = parent.join(name);
    let mut out = zip::ZipWriter::new(fs::File::create(&file).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (entry_name, content) in entries {
        if let Some(content) = content {
            out.start_file(*entry_name, options).unwrap();
            std::io::Write::write_all(&mut out, content.as_bytes()).unwrap();
        } else {
            out.add_directory(*entry_name, options).unwrap();
        }
    }
    out.finish().unwrap();
    file
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One stored entry for [`raw_zip`]: raw name bytes (no UTF-8 flag), the
/// extra field, and the CRC to record (`None`: the real one).
struct RawEntry<'a> {
    name: &'a [u8],
    data: &'a [u8],
    extra: Vec<u8>,
    crc: Option<u32>,
}

impl<'a> RawEntry<'a> {
    fn new(name: &'a [u8], data: &'a [u8]) -> Self {
        Self {
            name,
            data,
            extra: Vec::new(),
            crc: None,
        }
    }
}

/// A stored-only zip written byte by byte, for what `zip::ZipWriter` will
/// not write: unflagged UTF-8 names, a 0x7075 field, duplicate names, a
/// wrong CRC.
fn raw_zip(parent: &Path, name: &str, entries: &[RawEntry<'_>]) -> PathBuf {
    let le16 = |out: &mut Vec<u8>, v: usize| out.extend_from_slice(&u16::try_from(v).unwrap().to_le_bytes());
    let le32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for entry in entries {
        let offset = u32::try_from(out.len()).unwrap();
        let crc = entry.crc.unwrap_or_else(|| crc32(entry.data));
        let size = u32::try_from(entry.data.len()).unwrap();
        // version, flags, method (stored), time, date (2020-01-01)
        let common = |buf: &mut Vec<u8>| {
            for v in [20, 0, 0, 0, 0x5021] {
                le16(buf, v);
            }
            le32(buf, crc);
            le32(buf, size);
            le32(buf, size);
            le16(buf, entry.name.len());
            le16(buf, entry.extra.len());
        };
        le32(&mut out, 0x0403_4b50);
        common(&mut out);
        out.extend_from_slice(entry.name);
        out.extend_from_slice(&entry.extra);
        out.extend_from_slice(entry.data);
        le32(&mut central, 0x0201_4b50);
        le16(&mut central, 20);
        common(&mut central);
        // comment length, disk, internal and external attributes, offset
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, 0);
        le32(&mut central, offset);
        central.extend_from_slice(entry.name);
        central.extend_from_slice(&entry.extra);
    }
    let central_offset = u32::try_from(out.len()).unwrap();
    let central_size = u32::try_from(central.len()).unwrap();
    out.extend_from_slice(&central);
    le32(&mut out, 0x0605_4b50);
    for v in [0, 0, entries.len(), entries.len()] {
        le16(&mut out, v);
    }
    le32(&mut out, central_size);
    le32(&mut out, central_offset);
    le16(&mut out, 0);
    let file = parent.join(name);
    fs::write(&file, out).unwrap();
    file
}

/// The Info-ZIP Unicode Path extra field (0x7075) naming `unicode` for an
/// entry stored as `raw`.
fn unicode_path_extra(raw: &[u8], unicode: &str) -> Vec<u8> {
    unicode_path_extra_bytes(raw, unicode.as_bytes())
}

fn unicode_path_extra_bytes(raw: &[u8], unicode: &[u8]) -> Vec<u8> {
    let mut data = vec![1];
    data.extend_from_slice(&crc32(raw).to_le_bytes());
    data.extend_from_slice(unicode);
    let mut out = vec![0x75, 0x70];
    out.extend_from_slice(&u16::try_from(data.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&data);
    out
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Undoes the cases' chmods so the temp dir can be removed.
fn restore_modes(dir: &Path) {
    let Ok(metadata) = fs::symlink_metadata(dir) else {
        return;
    };
    if metadata.is_dir() {
        chmod(dir, 0o755);
        for entry in fs::read_dir(dir).unwrap().flatten() {
            restore_modes(&entry.path());
        }
    }
}

fn probe_config(id: &str, dir: &Path) -> AppleTestBundleConfiguration {
    match id {
        "empty-config" => bundle(None, None, None),
        "root-entry" => {
            let app = make_zip(
                dir,
                "app.ipa",
                &[
                    ("Info.plist", Some("ROOT")),
                    ("Payload/Sample.app/", None),
                    ("Payload/Sample.app/empty", Some("")),
                ],
            );
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "macosx-entries" => {
            let app = make_zip(
                dir,
                "app.zip",
                &[
                    ("__MACOSX", Some("EXACT")),
                    (".DS_Store", Some("DSROOT")),
                    ("sub/.DS_Store", Some("DSNESTED")),
                    ("__MACOSX/Payload/._Sample.app", Some("FORK")),
                    ("Payload/Sample.app/empty", Some("")),
                ],
            );
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "ambiguous-2-apps" => {
            bundle_dir(dir, "derived/ui.xctest");
            bundle_dir(dir, "derived/a.app");
            bundle_dir(dir, "derived/b.app");
            bundle(None, None, Some(jp(&dir.join("derived"))))
        }
        "ambiguous-3-xctest" => {
            bundle_dir(dir, "derived/a.xctest");
            bundle_dir(dir, "derived/b.xctest");
            bundle_dir(dir, "derived/c.xctest");
            bundle_dir(dir, "derived/a.app");
            bundle(None, None, Some(jp(&dir.join("derived"))))
        }
        "watch-pair" => {
            bundle_dir(dir, "derived/ui.xctest");
            bundle_dir(dir, "derived/X.app/Watch/Y.app");
            fs::write(dir.join("derived/X.app/empty"), "").unwrap();
            bundle(None, None, Some(jp(&dir.join("derived"))))
        }
        "derived-no-app" => {
            bundle_dir(dir, "derived/ui.xctest");
            bundle(None, None, Some(jp(&dir.join("derived"))))
        }
        "derived-no-xctest" => {
            bundle_dir(dir, "derived/a.app");
            bundle(None, None, Some(jp(&dir.join("derived"))))
        }
        "corrupt-archive" => {
            let app = dir.join("app.ipa");
            fs::write(&app, "not a zip").unwrap();
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "missing-archive" => bundle(
            Some(jp(&dir.join("nope.ipa"))),
            Some(jp(&bundle_dir(dir, "ui.xctest"))),
            None,
        ),
        "same-stem-archives" => {
            let app = make_zip(dir, "bundle.zip", &[("A.app/empty", Some(""))]);
            let test = make_zip(
                dir,
                "bundle.ipa",
                &[("T.app/PlugIns/ui.xctest/empty", Some("")), ("T.app/empty", Some(""))],
            );
            bundle(Some(jp(&app)), Some(jp(&test)), None)
        }
        "zip-slip" => {
            let app = make_zip(
                dir,
                "app.zip",
                &[("../escape.txt", Some("ESCAPED")), ("A.app/empty", Some(""))],
            );
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "app-regular-file" => {
            let app = dir.join("X.app");
            fs::write(&app, "").unwrap();
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "ipa-directory" => bundle(
            Some(jp(&mkdirs(dir, "X.ipa"))),
            Some(jp(&bundle_dir(dir, "ui.xctest"))),
            None,
        ),
        "uppercase-extension" => {
            let app = make_zip(dir, "APP.IPA", &[("A.app/empty", Some(""))]);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "test-archive-xctest-only" => {
            let test = make_zip(dir, "test.zip", &[("ui.xctest/empty", Some(""))]);
            bundle(Some(jp(&bundle_dir(dir, "a.app"))), Some(jp(&test)), None)
        }
        "test-archive-app-scopes-xctest" => {
            let test = make_zip(
                dir,
                "test.zip",
                &[("T.app/empty", Some("")), ("ui.xctest/empty", Some(""))],
            );
            bundle(Some(jp(&bundle_dir(dir, "a.app"))), Some(jp(&test)), None)
        }
        "lone-application-with-derived" => {
            bundle_dir(dir, "derived/ui.xctest");
            bundle_dir(dir, "derived/a.app");
            bundle(Some(jp(&dir.join("missing.ipa"))), None, Some(jp(&dir.join("derived"))))
        }
        "hidden-archive-stem" => {
            let sub = mkdirs(dir, "sub");
            let app = make_zip(&sub, ".zip", &[("A.app/empty", Some(""))]);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "stem-dot" => {
            let sub = mkdirs(dir, "a");
            fs::write(sub.join("KEEP"), "KEEP").unwrap();
            let app = make_zip(&sub, "..zip", &[("A.app/empty", Some(""))]);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "stem-dotdot" => {
            fs::write(mkdirs(dir, "a").join("KEEP"), "KEEP").unwrap();
            let sub = mkdirs(dir, "a/b");
            fs::write(sub.join("KEEP"), "KEEP").unwrap();
            let app = make_zip(&sub, "...zip", &[("A.app/empty", Some(""))]);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "utf8-unflagged-name" => {
            let app = raw_zip(dir, "app.zip", &[RawEntry::new("Café.app/empty".as_bytes(), b"")]);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "unicode-path-extra" => {
            let mut entry = RawEntry::new(b"A.app/empty", b"");
            entry.extra = unicode_path_extra(b"A.app/empty", "B.app/empty");
            let app = raw_zip(dir, "app.zip", &[entry]);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "symlink-escape" => {
            let outside = mkdirs(dir, "outside");
            fs::write(outside.join("KEEP"), "KEEP").unwrap();
            let locked = mkdirs(dir, "input/locked");
            symlink(&outside, locked.join("link")).unwrap();
            chmod(&locked, 0o555);
            let app = make_zip(
                dir,
                "input.zip",
                &[("locked/link/owned", Some("ESCAPE")), ("B.app/empty", Some(""))],
            );
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "symlinked-destination" => {
            let target = mkdirs(dir, "target");
            fs::write(target.join("KEEP"), "KEEP").unwrap();
            symlink(&target, dir.join("app")).unwrap();
            let app = make_zip(dir, "app.zip", &[("A.app/empty", Some(""))]);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "unreadable-dir-in-derived" => {
            bundle_dir(dir, "derived/ui.xctest");
            bundle_dir(dir, "derived/A.app");
            chmod(&mkdirs(dir, "derived/locked"), 0o000);
            bundle(None, None, Some(jp(&dir.join("derived"))))
        }
        "bad-crc-stored" => {
            let mut entry = RawEntry::new(b"B.app/data", b"WORLD");
            entry.crc = Some(crc32(b"HELLO"));
            let app = raw_zip(dir, "app.zip", &[entry]);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "mkdirs-ignored" => {
            let app = make_zip(
                dir,
                "app.zip",
                &[
                    ("junk", Some("FILE")),
                    ("junk/junk/child/", None),
                    ("B.app/empty", Some("")),
                ],
            );
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "duplicate-entries" => {
            let app = raw_zip(
                dir,
                "app.zip",
                &[
                    RawEntry::new(b"B.app/data", b"FIRST"),
                    RawEntry::new(b"parent/file", b"FILE"),
                    RawEntry::new(b"parent/file/child", b"next"),
                    RawEntry::new(b"B.app/data", b"LAST"),
                ],
            );
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "destination-link-locked" => {
            let outside = mkdirs(dir, "outside");
            let locked = mkdirs(dir, "locked");
            symlink(&outside, locked.join("input")).unwrap();
            let app = make_zip(&locked, "input.zip", &[("B.app/owned", Some("ESCAPE"))]);
            chmod(&locked, 0o555);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "stale-hard-link" => {
            let outside = mkdirs(dir, "outside");
            fs::write(outside.join("owned"), "KEEP").unwrap();
            let locked = mkdirs(dir, "input/locked");
            fs::hard_link(outside.join("owned"), locked.join("link")).unwrap();
            chmod(&locked, 0o555);
            let app = make_zip(
                dir,
                "input.zip",
                &[("locked/link", Some("ESCAPE")), ("B.app/empty", Some(""))],
            );
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "unicode-path-collision" => {
            let entries: Vec<RawEntry<'_>> = [(&b"A.app/empty"[..], &b"FIRST"[..]), (b"B.app/empty", b"LAST")]
                .into_iter()
                .map(|(name, data)| {
                    let mut entry = RawEntry::new(name, data);
                    entry.extra = unicode_path_extra(name, "Z.app/empty");
                    entry
                })
                .collect();
            let app = raw_zip(dir, "app.zip", &entries);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        "unicode-path-invalid" => {
            let mut entry = RawEntry::new(b"A.app/empty", b"");
            entry.extra = unicode_path_extra_bytes(b"A.app/empty", &[0xff]);
            let app = raw_zip(dir, "app.zip", &[entry]);
            bundle(Some(jp(&app)), Some(jp(&bundle_dir(dir, "ui.xctest"))), None)
        }
        other => panic!("probe case {other} not mirrored"),
    }
}

fn tree(root: &Path) -> Vec<String> {
    // Like walkTopDown: a directory that cannot be listed is not entered.
    fn visit(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                out.push(format!("{rel}/"));
                visit(&path, root, out);
            } else if path.file_name().is_some_and(|n| n == ".zip")
                || path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("zip") || e.eq_ignore_ascii_case("ipa"))
            {
                out.push(format!("{rel}=<archive>"));
            } else {
                out.push(format!("{rel}={}", fs::read_to_string(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn bundle_probe_matches_jvm() {
    let table =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/bundle-probe.jsonl"))
            .unwrap();
    let mut checked = 0;
    for line in table.lines().skip(1) {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let id = row["id"].as_str().unwrap();
        if id == "ktor-facts" {
            continue;
        }
        checked += 1;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(id);
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let mut config = probe_config(id, &dir);
        let outcome = config.validate();
        let result = &row["result"];
        let actual_tree: Vec<serde_json::Value> = tree(&dir).into_iter().map(serde_json::Value::String).collect();
        let jvm_tree: Vec<&str> = result["tree"]
            .as_array()
            .or_else(|| result["ok"]["tree"].as_array())
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let jvm_error_class = result["error"]["class"].as_str();
        restore_modes(&dir);
        match (id, outcome) {
            // S1: Kotlin extracts ../escape.txt; the port rejects it.
            ("zip-slip", Err(error)) => {
                assert_eq!(
                    error.message,
                    "zip entry escapes the extraction directory: ../escape.txt"
                );
                assert!(!dir.join("escape.txt").exists(), "nothing was written outside");
            }
            // S6: Kotlin deletes the parent dir (and the archive with it);
            // the port rejects the empty stem up front.
            ("hidden-archive-stem", Err(error)) => {
                assert!(
                    error.message.starts_with("archive file name has no usable stem"),
                    "{}",
                    error.message
                );
                assert!(dir.join("sub/.zip").exists(), "the archive survives");
            }
            // S6 (round 1): stems "." and ".." wipe the parent/grandparent
            // on the JVM; the port rejects them up front.
            ("stem-dot" | "stem-dotdot", Err(error)) => {
                assert_eq!(jvm_error_class, Some("java.nio.file.NoSuchFileException"));
                assert!(!jvm_tree.contains(&"a/KEEP=KEEP"), "the JVM wiped a/");
                assert!(
                    error.message.starts_with("archive file name has no usable stem"),
                    "{}",
                    error.message
                );
                assert!(dir.join("a/KEEP").exists());
                let archive = if id == "stem-dot" { "a/..zip" } else { "a/b/...zip" };
                assert!(dir.join(archive).exists(), "the archive survives");
            }
            // S1 (rounds 1, 2): the JVM writes through the stale link left
            // by a failed cleanup; the port refuses a destination that
            // survives its cleanup and writes nothing.
            ("symlink-escape", Err(error)) => {
                assert!(jvm_tree.contains(&"outside/owned=ESCAPE"));
                assert_eq!(error.fqcn(), "java.io.IOException");
                assert_eq!(
                    error.message,
                    format!(
                        "cannot clean the extraction directory {}: Permission denied (os error 13)",
                        dir.join("input").to_string_lossy()
                    )
                );
                assert!(!dir.join("outside/owned").exists(), "nothing was written outside");
                assert!(dir.join("outside/KEEP").exists(), "cleanup did not follow the link");
            }
            // R2-sol-1: the extraction dir is a link out that cannot be
            // unlinked; the JVM extracts through it.
            ("destination-link-locked", Err(error)) => {
                assert!(jvm_tree.contains(&"outside/B.app/owned=ESCAPE"));
                assert_eq!(
                    error.message,
                    format!(
                        "cannot clean the extraction directory {}: Permission denied (os error 13)",
                        dir.join("locked/input").to_string_lossy()
                    )
                );
                assert_eq!(
                    fs::read_dir(dir.join("outside")).unwrap().count(),
                    0,
                    "nothing was written outside"
                );
            }
            // R2-sol-1: the JVM truncates the stale hard link's shared inode.
            ("stale-hard-link", Err(error)) => {
                assert!(jvm_tree.contains(&"outside/owned=ESCAPE"));
                assert_eq!(
                    error.message,
                    format!(
                        "cannot clean the extraction directory {}: Permission denied (os error 13)",
                        dir.join("input").to_string_lossy()
                    )
                );
                assert_eq!(fs::read_to_string(dir.join("outside/owned")).unwrap(), "KEEP");
            }
            // R2 accepted: the zip crate rejects a 0x7075 payload that is
            // not UTF-8 while parsing the directory; ZipFile never reads it.
            ("unicode-path-invalid", Err(error)) => {
                assert!(result["ok"].is_object());
                assert_eq!(error.fqcn(), "java.util.zip.ZipException");
                assert!(!dir.join("app/A.app").exists());
            }
            // R1-claude-5: deleteRecursively follows a symlinked extraction
            // dir and empties its target; the port unlinks the link only.
            ("symlinked-destination", Ok(())) => {
                assert!(!jvm_tree.contains(&"target/KEEP=KEEP"));
                assert_eq!(result["ok"]["app"], "app/A.app");
                assert_eq!(
                    path_text(config.app.as_ref().unwrap()),
                    dir.join("app/A.app").to_string_lossy()
                );
                assert!(dir.join("target/KEEP").exists());
                assert!(!fs::symlink_metadata(dir.join("app")).unwrap().file_type().is_symlink());
            }
            // R1-sol-7: ZipFile does not check stored CRCs; the zip crate does.
            ("bad-crc-stored", Err(error)) => {
                assert!(jvm_tree.contains(&"app/B.app/data=WORLD"));
                assert_eq!(error.fqcn(), "java.io.IOException");
                assert!(error.message.contains("checksum"), "{}", error.message);
            }
            // R1-sol-4: Kotlin ignores the failed mkdirs(); the port reports it.
            ("mkdirs-ignored", Err(error)) => {
                assert!(result["ok"].is_object());
                assert_eq!(error.fqcn(), "java.io.IOException");
                assert_eq!(
                    error.message,
                    format!(
                        "cannot create directory {}: a file is in the way",
                        dir.join("app/junk/junk").to_string_lossy()
                    )
                );
            }
            // R1-sol-3/-4: ZipFile keeps both copies (the first is written
            // before the collision fails); the zip crate keeps the last.
            ("duplicate-entries", Err(error)) => {
                assert_eq!(jvm_error_class, Some("java.io.FileNotFoundException"));
                assert!(jvm_tree.contains(&"app/B.app/data=FIRST"));
                assert_eq!(error.fqcn(), "java.io.IOException");
                assert_eq!(fs::read_to_string(dir.join("app/B.app/data")).unwrap(), "LAST");
            }
            (_, Ok(())) => {
                let ok = &result["ok"];
                assert!(!ok.is_null(), "{id}: Kotlin failed with {result}, Rust validated");
                let rel = |p: &Option<JavaPath>| {
                    p.as_ref().map(|p| {
                        Path::new(&path_text(p))
                            .strip_prefix(&dir)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    })
                };
                assert_eq!(serde_json::json!(rel(&config.app)), ok["app"], "{id} app");
                assert_eq!(serde_json::json!(rel(&config.test_app)), ok["testApp"], "{id} testApp");
                assert_eq!(
                    serde_json::json!(rel(&config.xctest).unwrap()),
                    ok["xctest"],
                    "{id} xctest"
                );
                assert_eq!(
                    serde_json::json!(config.app.as_ref().map(|p| Path::new(&path_text(p)).exists())),
                    ok["appExists"],
                    "{id} appExists"
                );
                assert_eq!(serde_json::Value::Array(actual_tree), ok["tree"], "{id} tree");
            }
            (_, Err(error)) => {
                let expected = &result["error"];
                assert!(!expected.is_null(), "{id}: Kotlin validated, Rust failed: {error:?}");
                assert_eq!(error.fqcn(), expected["class"].as_str().unwrap(), "{id} class");
                let expected_message = expected["message"]
                    .as_str()
                    .unwrap()
                    .replace("$DIR", &dir.to_string_lossy());
                if error.fqcn().ends_with("ConfigurationException") {
                    assert_eq!(error.message, expected_message, "{id} message");
                }
                assert_eq!(serde_json::Value::Array(actual_tree), result["tree"], "{id} tree");
            }
        }
    }
    assert_eq!(checked, 33);
}

#[test]
fn s6_rejects_dot_and_dotdot_stems() {
    for (stem_name, archive) in [("empty", ".zip"), ("dot", "..zip"), ("dotdot", "...ipa")] {
        let tmp = tempfile::tempdir().unwrap();
        let sub = mkdirs(tmp.path(), "a/b");
        fs::write(tmp.path().join("a/KEEP"), "KEEP").unwrap();
        let file = make_zip(&sub, archive, &[("A.app/empty", Some(""))]);
        let error = marathon_config::bundle::default_temp_dir_for(&jp(&file)).unwrap_err();
        assert_eq!(
            error.fqcn(),
            "com.malinskiy.marathon.config.exceptions.ConfigurationException"
        );
        assert_eq!(
            error.message,
            format!(
                "archive file name has no usable stem to derive an extraction directory from: {}",
                file.to_string_lossy()
            ),
            "{stem_name}"
        );
        assert!(
            file.exists() && tmp.path().join("a/KEEP").exists(),
            "{stem_name}: nothing deleted"
        );
    }
}

/// R1-sol-1's and R2-sol-1's witnesses: a stale extraction dir whose
/// cleanup fails (an r-x subdir) is refused before anything is written.
#[test]
fn extraction_never_writes_through_a_stale_symlink() {
    for id in ["symlink-escape", "destination-link-locked", "stale-hard-link"] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(tmp.path()).unwrap();
        let mut config = probe_config(id, &dir);
        let error = config.validate().unwrap_err();
        restore_modes(&dir);
        assert!(
            error.message.starts_with("cannot clean the extraction directory "),
            "{id}: {}",
            error.message
        );
        let outside: Vec<String> = tree(&dir.join("outside"));
        assert!(outside.iter().all(|e| e.ends_with("=KEEP")), "{id}: {outside:?}");
    }
    // A stale tree handed over by a custom tempDirFor: a directory link out
    // of it is refused, a dangling file link is replaced, not followed.
    let tmp = tempfile::tempdir().unwrap();
    let dir = fs::canonicalize(tmp.path()).unwrap();
    let outside = mkdirs(&dir, "outside");
    let stale = mkdirs(&dir, "stale");
    symlink(&outside, stale.join("link")).unwrap();
    symlink(dir.join("created-outside"), mkdirs(&stale, "d").join("f")).unwrap();
    let mut extract_into_stale = |_: &JavaPath| Ok(jp(&stale));
    let app = make_zip(&dir, "app.zip", &[("link/owned", Some("ESCAPE"))]);
    let mut config = bundle(Some(jp(&app)), Some(jp(&bundle_dir(&dir, "ui.xctest"))), None);
    let error = config.validate_with_temp_dir_for(&mut extract_into_stale).unwrap_err();
    assert_eq!(error.message, "zip entry escapes the extraction directory: link/owned");
    let app = make_zip(&dir, "app.zip", &[("d/f", Some("REPLACED")), ("A.app/empty", Some(""))]);
    let mut config = bundle(Some(jp(&app)), Some(jp(&bundle_dir(&dir, "ui.xctest"))), None);
    config.validate_with_temp_dir_for(&mut extract_into_stale).unwrap();
    assert!(!dir.join("created-outside").exists());
    assert!(fs::symlink_metadata(stale.join("d/f")).unwrap().is_file());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

/// R2-sol-1: an existing file is unlinked and created anew, never
/// truncated, so a stale hard link's other name keeps its content; when
/// the unlink fails, nothing is written.
#[test]
fn extraction_never_writes_through_a_stale_hard_link() {
    for locked in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(tmp.path()).unwrap();
        let outside = mkdirs(&dir, "outside");
        fs::write(outside.join("owned"), "KEEP").unwrap();
        let stale = mkdirs(&dir, "stale/sub");
        fs::hard_link(outside.join("owned"), stale.join("link")).unwrap();
        if locked {
            chmod(&stale, 0o555);
        }
        let stale = dir.join("stale");
        let mut extract_into_stale = |_: &JavaPath| Ok(jp(&stale));
        let app = make_zip(
            &dir,
            "app.zip",
            &[("sub/link", Some("ESCAPE")), ("B.app/empty", Some(""))],
        );
        let mut config = bundle(Some(jp(&app)), Some(jp(&bundle_dir(&dir, "ui.xctest"))), None);
        let outcome = config.validate_with_temp_dir_for(&mut extract_into_stale);
        restore_modes(&dir);
        assert_eq!(
            fs::read_to_string(outside.join("owned")).unwrap(),
            "KEEP",
            "locked={locked}"
        );
        if locked {
            let error = outcome.unwrap_err();
            assert_eq!(error.fqcn(), "java.io.IOException");
            assert_eq!(
                error.message,
                format!(
                    "cannot replace {}: Permission denied (os error 13)",
                    stale.join("sub/link").to_string_lossy()
                )
            );
            assert!(!stale.join("B.app").exists(), "nothing after the failure");
        } else {
            outcome.unwrap();
            assert_eq!(fs::read_to_string(stale.join("sub/link")).unwrap(), "ESCAPE");
        }
    }
}

/// R2-sol-2: entries come from the central directory, not from the zip
/// crate's 0x7075-keyed index, so two stored names sharing one Unicode
/// Path name stay two apps (Ambiguous, as on the JVM); an exact duplicate
/// name still keeps the last copy (R1-sol-3).
#[test]
fn unicode_path_names_do_not_merge_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = fs::canonicalize(tmp.path()).unwrap();
    let mut config = probe_config("unicode-path-collision", &dir);
    let error = config.validate().unwrap_err();
    assert_eq!(
        error.message,
        "Ambiguous app configuration in derived data folder []. Please specify parameters explicitly"
    );
    assert_eq!(fs::read_to_string(dir.join("app/A.app/empty")).unwrap(), "FIRST");
    assert_eq!(fs::read_to_string(dir.join("app/B.app/empty")).unwrap(), "LAST");
    let app = raw_zip(
        &dir,
        "dup.zip",
        &[
            RawEntry::new(b"B.app/data", b"FIRST"),
            RawEntry::new(b"B.app/data", b"LAST"),
        ],
    );
    let mut config = bundle(Some(jp(&app)), Some(jp(&bundle_dir(&dir, "ui.xctest"))), None);
    config.validate().unwrap();
    assert_eq!(fs::read_to_string(dir.join("dup/B.app/data")).unwrap(), "LAST");
}

/// R1-claude-4 / R1-sol-5: walkTopDown skips a directory it cannot list.
#[test]
fn unreadable_directory_does_not_abort_the_search() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = fs::canonicalize(tmp.path()).unwrap();
    let mut config = probe_config("unreadable-dir-in-derived", &dir);
    let outcome = config.validate();
    restore_modes(&dir);
    outcome.unwrap();
    assert_eq!(
        path_text(config.app.as_ref().unwrap()),
        dir.join("derived/A.app").to_string_lossy()
    );
    assert_eq!(
        path_text(config.xctest.as_ref().unwrap()),
        dir.join("derived/ui.xctest").to_string_lossy()
    );
}
