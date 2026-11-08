//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/SkipCopyingDerivedDataTest.kt (installation rows).
mod support;
#[path = "support/test_root_device.rs"]
mod test_root_device;
use jcompat::{file_manager::JavaPath, jtime::instant_of};
use marathon_apple::{
    apple_application_installer::{AfterInstall, AppleApplicationInstaller},
    cmd::{cancelled, failure},
    distributed_derived_data::require_distributed_test_bundle,
    model::AppleTestBundle,
    xctestrun::test_root_factory::{FactoryError, FactoryResult},
};
use marathon_config::{
    VendorConfiguration,
    bind_vendor::bind_vendor_configuration,
    binder::Binder,
    binding::{BindingContext, Profile},
    node,
    vendor::*,
};
use marathon_core::{
    error::VendorError,
    runtime::{CancellationSignal, VendorClock, VendorFuture, VendorTimeoutBody},
    vendor::{Device, VendorContext},
};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
};
use test_root_device::{Fake, TestResult, b, prepare, s, sdk, v};

fn rows() -> impl Iterator<Item = Value> {
    include_str!("../../../fixtures/oracle/installer/probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
}
fn vendor(c: &Value, root: &Path) -> TestResult<VendorConfiguration> {
    let ctx = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: JavaPath::from_text("/Marathonfile"),
        marathonfile_dir: JavaPath::from_text("/"),
        process_dir: JavaPath::from_text("/"),
        environment: vec![],
        now: instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    };
    let stream = node::load(if s(c, "sdk", "") == "MACOS" {
        "type: macOS"
    } else {
        "type: iOS"
    });
    let binder = Binder::new(&ctx, &stream);
    let mut vendor = v(binder.first(&stream, bind_vendor_configuration))?.flatten().unwrap();
    let path = |key| JavaPath::from_text(root.join(s(c, key, "")).to_str().unwrap());
    let kind = match s(c, "type", "") {
        "XCUITEST" => Some(TestType::Xcuitest),
        "XCTEST" => Some(TestType::Xctest),
        "LOGIC_TEST" => Some(TestType::LogicTest),
        _ => None,
    };
    let mut bundle = AppleTestBundleConfiguration::unresolved(
        (c["raw"] != false).then(|| path("app")),
        Some(path(if b(c, "reuse") { "runner" } else { "test" })),
        None,
        b(c, "derived").then(|| JavaPath::from_text(root.to_str().unwrap())),
        kind,
    );
    if b(c, "derived") {
        v(bundle.validate())?;
    } else {
        bundle.app = (s(c, "mode", "") != "no-app").then(|| path("app"));
        bundle.test_app = b(c, "reuse").then(|| path("runner"));
        bundle.xctest = Some(path("test"));
    }
    if b(c, "coverageFile") {
        bundle.xctestrun = Some(JavaPath::from_text(root.join("AllTests.xctestrun").to_str().unwrap()));
    }
    if s(c, "mode", "").starts_with("extras") {
        bundle.extra_applications = Some(
            ["first.app", "wrong.APP", ".app", "second.app"]
                .iter()
                .map(|n| JavaPath::from_text(root.join(n).to_str().unwrap()))
                .collect(),
        );
    }
    match &mut vendor {
        VendorConfiguration::Ios(ios) => {
            ios.bundle = (s(c, "mode", "") != "no-bundle").then_some(bundle);
            ios.skip_copying_derived_data = b(c, "skip");
        }
        VendorConfiguration::Macos(mac) => mac.bundle = Some(bundle),
        _ => unreachable!(),
    }
    if s(c, "mode", "") == "no-env" {
        vendor = VendorConfiguration::Stub;
    }
    Ok(vendor)
}
struct Hook<'a>(&'a Fake);
impl AfterInstall for Hook<'_> {
    fn after_install<'a>(
        &'a self,
        bundle: &'a AppleTestBundle,
        _: &'a VendorContext,
    ) -> Pin<Box<dyn Future<Output = FactoryResult<()>> + Send + 'a>> {
        Box::pin(async move {
            self.0
                .recorder
                .events
                .lock()
                .unwrap()
                .push(json!({"hook":bundle.app_id()}));
            if s(&self.0.input, "mode", "") == "hook-fail" {
                return Err(VendorError::Other(failure("java.io.IOException", "hook io")).into());
            }
            Ok(())
        })
    }
}
struct Clock {
    inner: support::Clock,
    delays: Mutex<Vec<i64>>,
    cancel_delay: bool,
}
impl VendorClock for Clock {
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.delays.lock().unwrap().push(millis);
            if self.cancel_delay {
                return Err(VendorError::Cancelled(cancelled("delay cancelled")));
            }
            self.inner.delay(millis, cancellation).await
        })
    }
    fn with_timeout<'a>(
        &'a self,
        p: Arc<dyn CancellationSignal>,
        m: i64,
        f: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        self.inner.with_timeout(p, m, f)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        p: Arc<dyn CancellationSignal>,
        m: i64,
        f: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        self.inner.with_timeout_or_null(p, m, f)
    }
}
async fn run(id: &str) -> TestResult<Value> {
    let row = rows().chain(coverage_rows()).find(|r| r["input"]["id"] == id).unwrap();
    let c = &row["input"];
    let temp = tempfile::tempdir()?;
    prepare(temp.path(), c)?;
    if b(c, "coverageFile") {
        prepare_coverage(temp.path(), c)?;
    }
    let mut device = Fake::new(c.clone(), temp.path());
    device.policy = true;
    let config = vendor(c, temp.path())?;
    let hook = Hook(&device);
    let installer = AppleApplicationInstaller::new(&config, Arc::new(support::Runtime), device.logging.clone())
        .with_code_coverage(b(c, "coverage"));
    let installer = if b(c, "noop") {
        installer
    } else {
        installer.with_after_install(&hook)
    };
    let clock = Arc::new(Clock {
        inner: support::Clock::default(),
        delays: Mutex::default(),
        cancel_delay: s(c, "mode", "") == "delay-cancel",
    });
    let mut context = support::context();
    context.clock = clock.clone();
    let outcome = if b(c, "requireOnly") {
        let bundle = v(AppleTestBundle::new(
            Some(temp.path().join(s(c, "app", ""))),
            b(c, "reuse").then(|| temp.path().join(s(c, "runner", ""))),
            temp.path().join(s(c, "test", "")),
            sdk(c),
            Arc::new(support::Runtime),
            device.logging.clone(),
        )
        .await)?;
        require_distributed_test_bundle(
            &device,
            &bundle,
            &support::Runtime,
            |message| {
                let mut detail = failure("com.malinskiy.marathon.exceptions.TestParsingException", "");
                detail.message = Some(message);
                VendorError::Other(detail)
            },
            &context,
        )
        .await
        .map(|_| ())
        .map_err(FactoryError::from)
    } else {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            installer.prepare_installation(&device, b(c, "parser"), &context),
        )
        .await?
    };
    let error = match outcome {
        Ok(()) => Value::Null,
        Err(error) => {
            let reason = match &error {
                FactoryError::DeviceFailure { reason, .. } => Some(reason.name()),
                _ => None,
            };
            let detail = error.failure();
            if detail.class.to_owned().ends_with("DeviceSetupException") {
                assert!(matches!(error, FactoryError::Vendor(VendorError::DeviceSetup(_))));
            }
            if detail.class.to_owned().ends_with("CancellationException") {
                assert!(matches!(error, FactoryError::Vendor(VendorError::Cancelled(_))));
            }
            json!({"class":detail.class.to_owned(),"message":detail.message.as_ref().map(|s|s.to_owned().replace(temp.path().to_str().unwrap(),"<bundle>")),"reason":reason})
        }
    };
    // Always join before comparing: even an assertion failure leaves no live fake worker.
    v(device.dispose().await)?;
    assert_eq!(error, row["error"], "{id}: error");
    assert_eq!(
        json!(*device.recorder.events.lock().unwrap()),
        row["events"],
        "{id}: ordered events"
    );
    assert_eq!(
        json!(*device.recorder.requests.lock().unwrap()),
        row["commands"],
        "{id}: commands"
    );
    assert_eq!(json!(*device.pushes.lock().unwrap()), row["pushes"], "{id}: sends");
    assert_eq!(json!(*device.xml.lock().unwrap()), row["xml"], "{id}: xctestrun bytes");
    assert_eq!(json!(*device.logs.lock().unwrap()), row["logs"], "{id}: logs");
    for path in device.staged.lock().unwrap().iter() {
        assert!(!path.exists());
    }
    let delays = clock.delays.lock().unwrap().clone();
    let expected = match s(c, "mode", "") {
        "retry-success" | "retry-fail" | "retry-throw" => vec![1000, 1000],
        "delay-cancel" => vec![1000],
        _ => vec![],
    };
    assert_eq!(delays, expected, "{id}: retry delays");
    assert_eq!(
        row["elapsedMillis"],
        if s(c, "mode", "") == "delay-cancel" {
            0
        } else {
            delays.iter().sum::<i64>()
        },
        "{id}: JVM retry time"
    );
    Ok(row)
}
#[tokio::test(start_paused = true)]
async fn installer_oracle_transcripts() -> TestResult {
    for row in rows() {
        let id = s(&row["input"], "id", "");
        // Kotlin reflection can null a nonnullable xcresult; Rust's configuration type cannot.
        if id != "no-result" {
            run(id).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn installation_does_not_push_the_distributed_xctest_and_app_under_test() -> TestResult {
    let row = run("cloud-skip").await?;
    assert_eq!(
        checked(&row),
        vec![
            "/tmp/marathon/shared/marathon.xctest/UITests",
            "/tmp/marathon/shared/appUnderTest.app/CloudApp"
        ]
    );
    let copy = run("cloud-copy").await?;
    assert_eq!(row["xml"], copy["xml"]);
    assert_eq!(row["pushes"].as_array().unwrap().len(), 1);
    assert_eq!(row["pushes"][0]["dst"], "/tmp/marathon/udid/marathon.xctestrun");
    Ok(())
}

#[tokio::test]
async fn installation_does_not_push_the_distributed_test_runner_application() -> TestResult {
    let row = run("runner-skip").await?;
    assert_eq!(
        checked(&row),
        vec![
            "/tmp/marathon/shared/xctestRunner.app/UITests-Runner",
            "/tmp/marathon/shared/xctestRunner.app/PlugIns/UITests.xctest/UITests",
            "/tmp/marathon/shared/appUnderTest.app/CloudApp"
        ]
    );
    let copy = run("runner-copy").await?;
    assert_eq!(row["xml"], copy["xml"]);
    assert_eq!(row["pushes"].as_array().unwrap().len(), 1);
    assert_eq!(row["pushes"][0]["dst"], "/tmp/marathon/udid/marathon.xctestrun");
    Ok(())
}

#[tokio::test]
async fn installation_does_not_push_the_xctest_into_the_distributed_app_for_xctest() -> TestResult {
    let row = run("unit-skip").await?;
    assert_eq!(
        checked(&row),
        vec![
            "/tmp/marathon/shared/marathon.xctest/UnitTests",
            "/tmp/marathon/shared/appUnderTest.app/CloudApp",
            "/tmp/marathon/shared/appUnderTest.app/PlugIns/UnitTests.xctest/UnitTests"
        ]
    );
    let copy = run("unit-copy").await?;
    assert_eq!(row["xml"], copy["xml"]);
    assert_eq!(row["pushes"].as_array().unwrap().len(), 1);
    assert_eq!(row["pushes"][0]["dst"], "/tmp/marathon/udid/marathon.xctestrun");
    Ok(())
}

#[tokio::test]
async fn installation_fails_with_the_missing_xctest() -> TestResult {
    let row = run("missing-test").await?;
    assert_eq!(
        row["error"]["class"],
        "com.malinskiy.marathon.exceptions.DeviceSetupException"
    );
    assert!(row["pushes"].as_array().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn installation_fails_with_the_missing_test_runner_application() -> TestResult {
    let row = run("missing-runner-test").await?;
    assert_eq!(
        row["error"]["class"],
        "com.malinskiy.marathon.exceptions.DeviceSetupException"
    );
    assert!(row["pushes"].as_array().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn installation_fails_with_the_missing_test_runner_executable() -> TestResult {
    let row = run("missing-runner-binary").await?;
    assert_eq!(
        row["error"]["class"],
        "com.malinskiy.marathon.exceptions.DeviceSetupException"
    );
    assert!(row["pushes"].as_array().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn installation_fails_with_the_missing_app_under_test() -> TestResult {
    let row = run("missing-app").await?;
    assert_eq!(
        row["error"]["class"],
        "com.malinskiy.marathon.exceptions.DeviceSetupException"
    );
    assert!(row["pushes"].as_array().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn installation_fails_with_the_xctest_missing_from_the_app_for_xctest() -> TestResult {
    let row = run("missing-hosted-test").await?;
    assert_eq!(
        row["error"]["class"],
        "com.malinskiy.marathon.exceptions.DeviceSetupException"
    );
    assert!(row["pushes"].as_array().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn installation_pushes_the_xctest_and_app_under_test_without_the_flag() -> TestResult {
    let row = run("cloud-copy").await?;
    assert!(row["error"].is_null());
    assert!(checked(&row).is_empty());
    assert_eq!(
        row["pushes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["dst"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "/tmp/marathon/shared/marathon.xctest",
            "/tmp/marathon/shared/appUnderTest.app",
            "/tmp/marathon/udid/marathon.xctestrun"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn installation_pushes_the_test_runner_application_without_the_flag() -> TestResult {
    let row = run("runner-copy").await?;
    assert!(row["error"].is_null());
    assert!(checked(&row).is_empty());
    assert_eq!(
        row["pushes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["dst"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "/tmp/marathon/shared/xctestRunner.app",
            "/tmp/marathon/shared/appUnderTest.app",
            "/tmp/marathon/udid/marathon.xctestrun"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn installation_pushes_the_xctest_into_the_app_without_the_flag_for_xctest() -> TestResult {
    let row = run("unit-copy").await?;
    assert!(row["error"].is_null());
    assert!(checked(&row).is_empty());
    assert_eq!(
        row["pushes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["dst"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "/tmp/marathon/shared/marathon.xctest",
            "/tmp/marathon/shared/appUnderTest.app",
            "/tmp/marathon/shared/appUnderTest.app/PlugIns/UnitTests.xctest",
            "/tmp/marathon/udid/marathon.xctestrun"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn generated_test_runner_loads_the_shared_xctest_without_a_copy_in_its_plugins() -> TestResult {
    use marathon_apple::plist::PropertyList;
    for id in ["cloud-copy", "cloud-skip"] {
        let row = run(id).await?;
        let commands = row["commands"].as_array().unwrap();
        assert!(commands.iter().any(|r| r["argv"]
            == json!([
                "cp",
                "-R",
                "/Xcode/Platforms/iPhoneSimulator.platform/Developer/Library/Xcode/Agents/XCTRunner.app",
                "/tmp/marathon/udid/UITests-Runner.app"
            ])));
        assert!(!commands.iter().any(|r| {
            r["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a.as_str().unwrap().contains("-Runner.app/PlugIns"))
        }));
        assert!(!commands.iter().any(|r| {
            r["argv"][0] == "cp"
                && r["argv"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("/tmp/marathon/shared/marathon.xctest"))
        }));
        let target = target(&row)?;
        assert_eq!(
            target.dict().required::<String>("TestBundlePath")?,
            "/tmp/marathon/shared/marathon.xctest"
        );
        assert_eq!(
            target.dict().required::<String>("TestHostPath")?,
            "/tmp/marathon/udid/UITests-Runner.app"
        );
        assert_eq!(
            target.dependent_product_paths()?,
            vec![
                "/tmp/marathon/udid/UITests-Runner.app",
                "/tmp/marathon/shared/marathon.xctest",
                "/tmp/marathon/shared/appUnderTest.app"
            ]
        );
    }
    Ok(())
}

#[tokio::test]
async fn reused_test_runner_application_keeps_its_xctest_in_plugins() -> TestResult {
    use marathon_apple::plist::PropertyList;
    let row = run("runner-copy").await?;
    let commands = row["commands"].as_array().unwrap();
    assert!(commands.iter().any(|r| r["argv"]
        == json!([
            "ln",
            "-s",
            "/tmp/marathon/shared/xctestRunner.app",
            "/tmp/marathon/udid/UITests-Runner.app"
        ])));
    assert!(!commands.iter().any(|r| r["argv"][0] == "cp"));
    let target = target(&row)?;
    assert_eq!(
        target.dict().required::<String>("TestBundlePath")?,
        "/tmp/marathon/udid/UITests-Runner.app/PlugIns/UITests.xctest"
    );
    assert_eq!(
        target.dict().required::<String>("TestHostPath")?,
        "/tmp/marathon/udid/UITests-Runner.app"
    );
    Ok(())
}

fn checked(row: &Value) -> Vec<&str> {
    row["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["argv"][0] == "test" && r["argv"][1] == "-e")
        .map(|r| r["argv"][2].as_str().unwrap())
        .collect()
}
fn target(row: &Value) -> TestResult<marathon_apple::xctestrun::v2::TestTarget> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use marathon_apple::{plist::PropertyList, xctestrun::v2::Xctestrun};
    let bytes = STANDARD.decode(row["xml"].as_str().unwrap())?;
    Ok(Xctestrun::from_bytes(&bytes)?.test_configurations()?[0].test_targets()?[0].clone())
}

fn coverage_rows() -> impl Iterator<Item = Value> {
    include_str!("../../../fixtures/oracle/coverage-c95b2128/installer.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
}
fn prepare_coverage(root: &Path, c: &Value) -> TestResult {
    let static_path = root.join("Debug-iphonesimulator/Static.framework/Static");
    std::fs::create_dir_all(static_path.parent().ok_or("parent")?)?;
    std::fs::write(&static_path, b"!<arch>\n")?;
    let info = |name: &str, path: &str, is_static: bool| {
        format!(
            "<dict><key>Name</key><string>{name}</string><key>IsStatic</key><{is_static}/><key>ProductPath</key><string>__TESTROOT__/{path}</string><key>Architecture</key><string>arm64</string></dict>"
        )
    };
    let binary = if s(c, "sdk", "") == "MACOS" {
        "/Contents/MacOS/"
    } else {
        "/"
    };
    let products = if b(c, "empty") {
        String::new()
    } else {
        info("CloudApp.app", &format!("{}{binary}CloudApp", s(c, "app", "")), false)
            + &info(
                "UITests.xctest",
                &format!(
                    "{}{binary}{}",
                    s(c, "test", ""),
                    if s(c, "test", "") == "UnitTests.xctest" {
                        "UnitTests"
                    } else {
                        "UITests"
                    }
                ),
                false,
            )
            + &info(
                "Static.framework",
                "Debug-iphonesimulator/Static.framework/Static",
                true,
            )
    };
    std::fs::write(
        root.join("AllTests.xctestrun"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>__xctestrun_metadata__</key><dict><key>FormatVersion</key><integer>1</integer><key>CodeCoverageBuildableInfos</key><array>{products}</array></dict></dict></plist>"
        ),
    )?;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn code_coverage_targets_of_the_built_xctestrun_go_into_the_generated_one_only_with_is_code_coverage_enabled()
-> TestResult {
    for row in coverage_rows() {
        run(s(&row["input"], "id", "")).await?;
    }
    Ok(())
}
