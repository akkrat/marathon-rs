//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/xctestrun/v2/XctestrunTest.kt

use std::path::PathBuf;

use marathon_apple::plist::PropertyList;
use marathon_apple::xctestrun::v2::{
    ArtifactReinstallParams, CodeCoverageBuildableInfo, CodeCoverageBuildableInfoParams, Metadata, TestConfiguration,
    TestPlan, TestTarget, Xctestrun,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vendor-apple-base/fixtures/xctestrun")
        .join(name)
}

fn ordered(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn sample() -> Xctestrun {
    let mut params = ArtifactReinstallParams::new(
        "MyAppTests".to_owned(),
        "__TESTHOST__/PlugIns/MyAppTests.xctest".to_owned(),
        "__TESTROOT__/Debug-iphonesimulator/MyApp.app".to_owned(),
        ordered(&[
            ("DYLD_FRAMEWORK_PATH", "__TESTROOT__/Debug-iphonesimulator"),
            ("DYLD_LIBRARY_PATH", "__TESTROOT__/Debug-iphonesimulator"),
        ]),
    );
    params.ui_target_app_path = Some("__TESTROOT__/Debug-iphonesimulator/MyApp.app".to_owned());
    params.dependent_product_paths = Some(vec![
        "__TESTROOT__/Debug-iphonesimulator/MyApp.app".to_owned(),
        "__TESTROOT__/Debug-iphonesimulator/MyApp.app/PlugIns/MyAppTests.xctest".to_owned(),
    ]);
    params.product_module_name = Some("MyAppTests".to_owned());
    params.system_attachment_lifetime = Some("deleteOnSuccess".to_owned());
    params.user_attachment_lifetime = Some("deleteOnSuccess".to_owned());
    let target = TestTarget::with_artifact_reinstall(params);
    let configuration = TestConfiguration::new("Configuration 1".to_owned(), &[target]);
    let coverage = CodeCoverageBuildableInfo::with_params(CodeCoverageBuildableInfoParams {
        name: "MyApp.app".to_owned(),
        buildable_identifier: "1234567890ABCDEF:primary".to_owned(),
        include_in_report: true,
        is_static: false,
        product_paths: vec!["__TESTROOT__/Debug-iphonesimulator/MyApp.app/MyApp".to_owned()],
        architectures: vec!["x86_64".to_owned()],
        source_files: vec!["ViewController.swift".to_owned(), "AppDelegate.swift".to_owned()],
        source_files_common_path_prefix: "/Users/johnappleseed/MyApp/MyApp/".to_owned(),
        toolchains: vec!["com.apple.dt.toolchain.XcodeDefault".to_owned()],
    });
    Xctestrun::new(
        &Metadata::new(2),
        &[configuration],
        Some(&TestPlan::new("MyTestPlan".to_owned(), true)),
        Some(&[coverage]),
    )
}

#[test]
fn test_read() {
    let actual = Xctestrun::from_path(&fixture("man_v2.xctestrun")).expect("read man_v2");
    assert_eq!(actual, sample());
    // The nested-model reads the Kotlin test never performs (its list delegate throws;
    // P5 intended read): the same values must come back through the accessors.
    assert_eq!(actual.metadata().expect("metadata").format_version().expect("fv"), 2);
    let configurations = actual.test_configurations().expect("configurations");
    assert_eq!(configurations.len(), 1);
    assert_eq!(configurations[0].name().expect("name"), "Configuration 1");
    let targets = configurations[0].test_targets().expect("targets");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].name().expect("blueprint"), "MyAppTests");
    assert_eq!(
        targets[0].test_bundle_path().expect("bundle path"),
        "__TESTHOST__/PlugIns/MyAppTests.xctest"
    );
    let infos = actual.code_coverage_buildable_infos().expect("coverage");
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].architectures().expect("archs"), vec!["x86_64".to_owned()]);
    let plan = actual.test_plan().expect("plan read").expect("plan present");
    assert_eq!(plan.name().expect("plan name"), "MyTestPlan");
    assert!(plan.is_default().expect("plan default"));
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut table = [255u8; 256];
    for (i, c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .iter()
        .enumerate()
    {
        table[*c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | u32::from(table[c as usize]);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// The 0.4b evidence of the exact bytes Kotlin's writer pushed in real runs
/// (base64 + SHA-256 in `pushed-xctestrun.json`): parsing them with the v2 model and
/// writing them back must reproduce the bytes exactly. Re-deriving them from
/// `TestRootFactory`'s inputs is 4.5b/7.1; the writer half is pinned here.
#[test]
fn pushed_xctestrun_bytes_round_trip_exactly() {
    for scenario in ["ios-smoke", "macos-smoke", "ios-media", "ios-screenshot"] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/fake-worker/apple")
            .join(scenario)
            .join("expected/raw/pushed-xctestrun.json");
        let json = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut found = 0;
        for part in json.split("\"base64\": \"").skip(1) {
            let encoded = part.split('"').next().expect("base64 value");
            let bytes = base64_decode(encoded);
            let parsed = Xctestrun::from_bytes(&bytes).expect("parse pushed xctestrun");
            assert_eq!(parsed.to_xml_bytes(), bytes, "scenario {scenario}");
            found += 1;
        }
        assert!(found >= 1, "no pushed xctestrun in {scenario}");
    }
}

/// R1-claude-6: each 0.4b pushed xctestrun, rebuilt from scratch through the public
/// constructors (`with_artifact_reinstall` -> `TestConfiguration::new` ->
/// `Xctestrun::new(Metadata::new(2), ...)`) instead of parse-and-rewrite, must produce
/// the recorded bytes (checked against the recorded SHA-256). The inputs are read back
/// through the parsed model's accessors; every pushed file carries exactly the fields
/// the reinstall constructor writes, in its order.
#[test]
fn pushed_xctestrun_rebuilt_through_constructors() {
    use sha2::{Digest, Sha256};

    for scenario in ["ios-smoke", "macos-smoke", "ios-media", "ios-screenshot"] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/fake-worker/apple")
            .join(scenario)
            .join("expected/raw/pushed-xctestrun.json");
        let json = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let hashes: Vec<&str> = json
            .split("\"sha256\": \"")
            .skip(1)
            .map(|part| part.split('"').next().expect("sha256 value"))
            .collect();
        let mut found = 0;
        for part in json.split("\"base64\": \"").skip(1) {
            let encoded = part.split('"').next().expect("base64 value");
            let bytes = base64_decode(encoded);
            let parsed = Xctestrun::from_bytes(&bytes).expect("parse pushed xctestrun");
            assert_eq!(parsed.metadata().expect("metadata").format_version().expect("fv"), 2);
            assert!(parsed.test_plan().expect("plan").is_none(), "scenario {scenario}");
            assert!(
                parsed.code_coverage_buildable_infos().expect("coverage").is_empty(),
                "scenario {scenario}"
            );
            let configurations = parsed.test_configurations().expect("configurations");
            assert_eq!(configurations.len(), 1, "scenario {scenario}");
            let targets = configurations[0].test_targets().expect("targets");
            assert_eq!(targets.len(), 1, "scenario {scenario}");
            let target = &targets[0];

            let mut params = ArtifactReinstallParams::new(
                target.name().expect("blueprint name"),
                target.test_bundle_path().expect("bundle path"),
                target.test_host_path().expect("host path"),
                target.testing_environment_variables().expect("testing env"),
            );
            if target.dict().contains_key("UITargetAppPath") {
                params.ui_target_app_path = Some(target.ui_target_app_path().expect("ui target path"));
            }
            if target.dict().contains_key("DependentProductPaths") {
                params.dependent_product_paths = Some(target.dependent_product_paths().expect("dependent paths"));
            }
            params.product_module_name = target.product_module_name().expect("module name");
            params.system_attachment_lifetime = target.system_attachment_lifetime().expect("system lifetime");
            params.user_attachment_lifetime = target.user_attachment_lifetime().expect("user lifetime");
            params.is_ui_test_bundle = target.is_ui_test_bundle().expect("ui bundle");
            params.is_app_hosted_test_bundle = target.is_app_hosted_test_bundle().expect("app hosted");

            let rebuilt = Xctestrun::new(
                &Metadata::new(2),
                &[TestConfiguration::new(
                    configurations[0].name().expect("configuration name"),
                    &[TestTarget::with_artifact_reinstall(params)],
                )],
                None,
                None,
            );
            let out = rebuilt.to_xml_bytes();
            assert_eq!(out, bytes, "scenario {scenario}");
            assert_eq!(
                format!("{:x}", Sha256::digest(&out)),
                hashes[found],
                "scenario {scenario} entry {found}"
            );
            found += 1;
        }
        assert!(found >= 1, "no pushed xctestrun in {scenario}");
        assert_eq!(found, hashes.len(), "scenario {scenario}");
    }
}

#[test]
fn test_write() {
    let expected = std::fs::read(fixture("expect_2.xctestrun")).expect("read expect_2");
    // The fixture file ends with an editor newline that the Kotlin trim() hid;
    // dd-plist writes no trailing newline (0.4b pushed bytes end at </plist>).
    let expected = expected.strip_suffix(b"\n").expect("fixture newline");
    assert_eq!(sample().to_xml_bytes(), expected);
}
