//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/model/AppleTestBundle.kt
//! Additional oracle tests (no Kotlin matrix rows): the `bundle/*` cases of fixtures/oracle/apple-bin/probes.jsonl.
//! tools/oracle/apple-bin-probes/run.py built the same witness directories from fixtures/bundles/*.zip and ran
//! the frozen Kotlin class on them; this test rebuilds them in a temp dir and compares construction failures,
//! the derived ids, every getter (including a retry after a failed first access) and the warnings.
mod support;
use jcompat::logging::{Level, Logger, MarathonLogging};
use marathon_apple::{
    cmd::remote::ssh::sshj::config::SshLogging,
    model::{AppleTestBundle, Sdk},
    plist::{PlistDict, PlistValue, PropertyList, bundle::BundleInfo},
};
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    vendor::TestBundle,
};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use support::*;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Extracts every entry of a fixture zip (plain deflate, no zip64) under `dir`.
fn extract(zip_name: &str, dir: &Path) {
    let zip = std::fs::read(fixtures().join("bundles").join(zip_name)).unwrap();
    let u16_at = |off: usize| usize::from(u16::from_le_bytes([zip[off], zip[off + 1]]));
    let u32_at = |off: usize| u32::from_le_bytes(zip[off..off + 4].try_into().unwrap()) as usize;
    let eocd = zip.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
    let mut pos = u32_at(eocd + 16);
    for _ in 0..u16_at(eocd + 10) {
        let (method, size) = (u16_at(pos + 10), u32_at(pos + 20));
        let (name_len, extra_len, comment_len) = (u16_at(pos + 28), u16_at(pos + 30), u16_at(pos + 32));
        let local = u32_at(pos + 42);
        let name = std::str::from_utf8(&zip[pos + 46..pos + 46 + name_len])
            .unwrap()
            .to_owned();
        let start = local + 30 + u16_at(local + 26) + u16_at(local + 28);
        let data = &zip[start..start + size];
        let bytes = match method {
            0 => data.to_vec(),
            _ => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(data).read_to_end(&mut out).unwrap();
                out
            }
        };
        if !name.ends_with('/') {
            let path = dir.join(&name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        pos += 46 + name_len + extra_len + comment_len;
    }
}

const IOS: [&str; 3] = [
    "sample-app.app",
    "sample-appUITests-Runner.app",
    "sample-appUITests-Runner.app/PlugIns/sample-appUITests.xctest",
];
const MAC: [&str; 3] = [
    "AppleWorld.app",
    "AppleWorldUITests-Runner.app",
    "AppleWorldUITests-Runner.app/Contents/PlugIns/AppleWorldUITests.xctest",
];
fn macho() -> Vec<u8> {
    let mut bytes = vec![0xcf, 0xfa, 0xed, 0xfe];
    bytes.extend([0; 28]);
    bytes
}

fn rewrite(path: &Path, edit: impl FnOnce(&mut Vec<(String, PlistValue)>)) {
    let mut entries = BundleInfo::from_path(path).unwrap().dict().snapshot();
    edit(&mut entries);
    let dict = PlistDict::new();
    for (key, value) in entries {
        dict.insert(&key, value);
    }
    BundleInfo::wrap(dict).save_as_xml_to_file(path).unwrap();
}
fn drop_key(path: &Path, key: &str) {
    rewrite(path, |entries| entries.retain(|(k, _)| k != key));
}
fn set_key(path: &Path, key: &str, value: PlistValue) {
    rewrite(path, |entries| match entries.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = value,
        None => entries.push((key.into(), value)),
    });
}
/// Replaces every regular file but Info.plist; a one-element tuple-like `Err` is a symlink target.
fn files_in(xctest: &Path, entries: &[(&str, Result<Vec<u8>, &str>)]) {
    for entry in std::fs::read_dir(xctest).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap() != "Info.plist" && path.is_file() {
            std::fs::remove_file(path).unwrap();
        }
    }
    for (name, content) in entries {
        let path = xctest.join(name);
        match content {
            Ok(bytes) => std::fs::write(&path, bytes).unwrap(),
            Err(target) => std::os::unix::fs::symlink(target, &path).unwrap(),
        }
    }
}

struct Case {
    dir: PathBuf,
    app: Option<PathBuf>,
    runner: Option<PathBuf>,
    xctest: PathBuf,
    sdk: Sdk,
}

/// The same directories as run.py's `prepare`, case by case.
fn prepare(root: &Path, name: &str) -> Case {
    let dir = root.join(name);
    std::fs::create_dir(&dir).unwrap();
    let ios = |dir: &Path| extract("ios.zip", dir);
    let paths = |p: [&str; 3]| (Some(dir.join(p[0])), Some(dir.join(p[1])), dir.join(p[2]));
    let (mut app, mut runner, mut xctest) = paths(IOS);
    let mut sdk = Sdk::IphoneSimulator;
    match name {
        "macos-ok" | "macos-as-ios" => {
            extract("macos.zip", &dir);
            (app, runner, xctest) = paths(MAC);
            sdk = if name == "macos-ok" { Sdk::Macos } else { Sdk::Iphoneos };
        }
        "ios-as-macos" => {
            ios(&dir);
            sdk = Sdk::Macos;
        }
        "ios-unit-no-runner" => {
            ios(&dir);
            runner = None;
            xctest = dir.join("sample-app.app/PlugIns/sample-appUnitTests.xctest");
            sdk = Sdk::Iphoneos;
        }
        "no-app" => {
            ios(&dir);
            app = None;
        }
        "app-missing-dir" => {
            ios(&dir);
            app = Some(dir.join("absent.app"));
        }
        "no-bundle-identifier" => {
            ios(&dir);
            drop_key(&dir.join(IOS[0]).join("Info.plist"), "CFBundleIdentifier");
        }
        "no-bundle-name" => {
            ios(&dir);
            drop_key(&xctest.join("Info.plist"), "CFBundleName");
        }
        "bundle-name-separators" => {
            ios(&dir);
            set_key(
                &xctest.join("Info.plist"),
                "CFBundleName",
                PlistValue::String("My-Test Name_x-".into()),
            );
        }
        "bundle-identifier-wrong-type" => {
            ios(&dir);
            set_key(
                &dir.join(IOS[0]).join("Info.plist"),
                "CFBundleIdentifier",
                PlistValue::Integer(5),
            );
        }
        "multi-stem" => {
            ios(&dir);
            files_in(
                &xctest,
                &[
                    ("Zother", Ok(macho())),
                    ("sample-appUITests", Ok(macho())),
                    ("Aother", Ok(macho())),
                ],
            );
        }
        "filters" => {
            ios(&dir);
            let mut fat64 = vec![0xca, 0xfe, 0xba, 0xbf];
            fat64.extend([0; 28]);
            files_in(
                &xctest,
                &[
                    ("text", Ok(b"#!/bin/sh\n".to_vec())),
                    ("lib.dylib", Ok(macho())),
                    (".hidden", Ok(macho())),
                    ("short", Ok(macho()[..3].to_vec())),
                    ("fat64", Ok(fat64)),
                    ("trailing.", Ok(macho())),
                    ("dangling", Err("/nonexistent/target")),
                    ("link", Err("trailing.")),
                ],
            );
        }
        "no-binaries" => {
            ios(&dir);
            files_in(&xctest, &[("text", Ok(b"x".to_vec()))]);
        }
        "unreadable" => {
            ios(&dir);
            files_in(&xctest, &[("unreadable", Ok(macho()))]);
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(xctest.join("unreadable"), std::fs::Permissions::from_mode(0o0)).unwrap();
        }
        "dotted-path" => {
            ios(&dir);
            // Kept unnormalized: java.io.File collapses `//` and the trailing slash but keeps `./`.
            xctest = PathBuf::from(format!(
                "{}/./sample-appUITests-Runner.app//PlugIns/sample-appUITests.xctest/",
                dir.display()
            ));
        }
        "app-no-binaries" => {
            ios(&dir);
            for entry in std::fs::read_dir(dir.join(IOS[0])).unwrap() {
                let path = entry.unwrap().path();
                if path.is_file() && path.file_name().unwrap() != "Info.plist" {
                    std::fs::remove_file(path).unwrap();
                }
            }
        }
        _ => ios(&dir),
    }
    Case {
        dir,
        app,
        runner,
        xctest,
        sdk,
    }
}

fn describe(failure: &Failure) -> Value {
    // Causes are not compared: Kotlin's carry JDK-internal NIO exceptions.
    json!({"class": failure.class.to_string_lossy(), "message": failure.message.as_ref().map(|m| m.to_string_lossy())})
}
fn fail(error: VendorError) -> Value {
    match error {
        VendorError::Other(f) => describe(&f),
        other => panic!("unexpected {other:?}"),
    }
}
fn strip(kotlin: &Value) -> Value {
    match kotlin {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| k.as_str() != "cause")
                .map(|(k, v)| (k.clone(), strip(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip).collect()),
        other => other.clone(),
    }
}

async fn observe(case: &Case, logs: &Arc<Mutex<Vec<Value>>>) -> Value {
    let logs_sink = logs.clone();
    let logging = SshLogging::new(
        Arc::new(Mutex::new(MarathonLogging::default())),
        Arc::new(
            move |logger: &Logger, level: Level, message: String, _: Option<Failure>| {
                logs_sink
                    .lock()
                    .unwrap()
                    .push(json!([level.as_str(), logger.name(), message]));
            },
        ),
    );
    let bundle = match AppleTestBundle::new(
        case.app.clone(),
        case.runner.clone(),
        case.xctest.clone(),
        case.sdk,
        Arc::new(Runtime),
        logging,
    )
    .await
    {
        Ok(bundle) => bundle,
        Err(error) => return json!({"construct": fail(error)}),
    };
    let prefix = format!("{}/", case.dir.display());
    let rel = |p: &Path| {
        json!(
            p.to_string_lossy()
                .strip_prefix(&prefix)
                .map(str::to_owned)
                .unwrap_or_else(|| p.display().to_string())
        )
    };
    let get = |r: VendorResult<Option<PathBuf>>| match r {
        Ok(Some(p)) => rel(&p),
        Ok(None) => Value::Null,
        Err(e) => json!({"error": fail(e)}),
    };
    let mut value = json!({
        "id": rel(Path::new(&bundle.id().to_string_lossy())),
        "appId": bundle.app_id(),
        "testBundleId": bundle.test_bundle_id(),
        "testBinary": get(bundle.test_binary().await.map(Some)),
        "testBinaryAgain": get(bundle.test_binary().await.map(Some)),
        "testRunnerBinary": get(bundle.test_runner_binary().await.map(Some)),
        "applicationBinary": get(bundle.application_binary().await),
        "relativeBinaryPath": bundle.relative_binary_path(),
        "relativeRootPath": bundle.relative_root_path(),
        "relativeFrameworksPath": bundle.relative_frameworks_path(),
    });
    if case.dir.ends_with("no-binaries") {
        std::fs::copy(
            case.dir.join(IOS[1]).join("sample-appUITests-Runner"),
            case.xctest.join("sample-appUITests"),
        )
        .unwrap();
        value["testBinaryRetry"] = get(bundle.test_binary().await.map(Some));
    }
    value["logs"] = json!(*logs.lock().unwrap());
    value
}

/// The scan order is the file system's (Kotlin and Rust both list with readdir); when this file system lists
/// the multi-candidate directories differently from the probe's, the expected warning and choice follow it.
fn adjust_for_listing_order(name: &str, case: &Case, expected: &mut Value) {
    let Some(message) = expected["logs"][0][2].as_str().map(str::to_owned) else {
        return;
    };
    let order: Vec<String> = std::fs::read_dir(&case.xctest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let listed = &message[message.find('[').unwrap() + 1..message.find(']').unwrap()];
    let mut names: Vec<&str> = listed.split(',').collect();
    names.sort_by_key(|n| order.iter().position(|o| o == n));
    expected["logs"][0][2] = json!(message.replace(listed, &names.join(",")));
    if name == "filters" {
        let binary = format!("{}/{}", IOS[2], names[0]);
        expected["testBinary"] = json!(binary);
        expected["testBinaryAgain"] = json!(binary);
    }
}

#[tokio::test]
async fn every_probed_bundle_matches_kotlin() {
    let root = tempfile::tempdir().unwrap();
    let bundles = root.path().join("bundles");
    std::fs::create_dir(&bundles).unwrap();
    let text = std::fs::read_to_string(fixtures().join("oracle/apple-bin/probes.jsonl")).unwrap();
    let mut checked = 0;
    for line in text.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let Some(name) = row["case"].as_str().unwrap().strip_prefix("bundle/") else {
            continue;
        };
        let case = prepare(&bundles, name);
        if name == "unreadable" && std::fs::File::open(case.xctest.join("unreadable")).is_ok() {
            // Running as root: the permission witness cannot fail here.
            continue;
        }
        let logs = Arc::default();
        let rust = observe(&case, &logs).await;
        let rust = serde_json::to_string(&rust)
            .unwrap()
            .replace(&bundles.display().to_string(), "<bundles>");
        let mut expected = strip(&row["value"]);
        adjust_for_listing_order(name, &case, &mut expected);
        let expected = match name {
            // dd-plist's ClassCastException text names JDK classes; the class is kept, the text is WP 4.5a's.
            "bundle-identifier-wrong-type" => json!({"construct": {"class": "java.lang.ClassCastException",
                "message": "plist property CFBundleIdentifier is a integer, not a string"}}),
            _ => expected,
        };
        assert_eq!(serde_json::from_str::<Value>(&rust).unwrap(), expected, "{name}");
        checked += 1;
    }
    assert!(checked >= 16, "{checked}");
}
