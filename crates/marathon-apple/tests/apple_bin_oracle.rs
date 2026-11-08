//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/AppleBinaryEnvironment.kt
//! Additional oracle tests (no Kotlin matrix rows): every case of fixtures/oracle/apple-bin/probes.jsonl, which
//! tools/oracle/apple-bin-probes ran against the frozen Kotlin wrappers, replayed here. Each case compares the
//! argv, timeouts, env and workdir of every request (before executor quoting), the return value or error, and
//! the log events. The few approved differences (P1, P2, D38) are listed in `deviation`.
mod support;
use jcompat::{
    jtime::Duration,
    logging::{Level, Logger, MarathonLogging},
};
use marathon_apple::{
    bin::{
        AppleBinaryEnvironment, Tools,
        xcrun::{
            simctl::service::ContainerType,
            xcodebuild::simulator_test_args,
            xcresulttool::{ResultBundleFormat, describe_timeout},
        },
    },
    cmd::{
        BaseCommand, CommandExecutor, CommandHost, CommandRequest, CommandResult, CommandSession, SessionControl,
        failure, local::KotlinProcessHost, remote::ssh::sshj::config::SshLogging,
    },
    device::connection_factory::ConnectionError,
    model::{AppleOperatingSystemVersion, Arch, Os, Platform, Sdk, XcodeVersion},
    test::TestRequest,
    xcresult::ActionsInvocationRecord,
};
use marathon_config::vendor::{
    AppleTimeoutParameters, Codec, Display, IosPermission, Mask, ScreenshotType, apple_timeout_configuration,
};
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    model::Test,
    runtime::VendorFuture,
    vendor::VendorContext,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use support::*;

#[derive(Clone)]
struct Answer {
    stdout: Vec<String>,
    stderr: Vec<String>,
    exit: Option<i32>,
    throws: Option<&'static str>,
}
fn answer(stdout: &[&str], stderr: &[&str], exit: Option<i32>) -> Answer {
    Answer {
        stdout: stdout.iter().map(|s| s.to_string()).collect(),
        stderr: stderr.iter().map(|s| s.to_string()).collect(),
        exit,
        throws: None,
    }
}
fn ok() -> Answer {
    answer(&[" out1 ", "out2"], &["err"], Some(0))
}
fn throws(class: &'static str) -> Answer {
    Answer {
        throws: Some(class),
        ..answer(&[], &[], Some(0))
    }
}
fn variant(name: &str) -> Answer {
    match name {
        "ok" => ok(),
        "fail" => answer(&["o"], &["e"], Some(3)),
        "null" | "nul" => answer(&["o"], &["e"], None),
        "conn" => throws("net.schmizz.sshj.connection.ConnectionException"),
        "other" => throws("java.lang.IllegalStateException"),
        other => panic!("variant {other}"),
    }
}

struct Scripted {
    host: CommandHost,
    logger: Logger,
    answers: Mutex<VecDeque<Answer>>,
    requests: Mutex<Vec<Value>>,
}
impl CommandExecutor for Scripted {
    fn host(&self) -> &CommandHost {
        &self.host
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn connected(&self) -> bool {
        true
    }
    fn log_error(&self, _: String) {}
    fn execute(&self, request: CommandRequest, _: VendorContext) -> VendorFuture<'_, CommandSession> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(json!({
                "argv": request.command,
                "timeout": request.timeout.to_string(),
                "idle": request.idle_timeout.to_string(),
                "env": request.env.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
                "workdir": request.workdir,
            }));
            let answer = self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| answer(&[], &[], Some(0)));
            if let Some(class) = answer.throws {
                return Err(VendorError::Other(failure(class, "scripted")));
            }
            let base = BaseCommand::new(answer.stdout.len().max(answer.stderr.len()) + 1, Arc::new(Runtime));
            for line in answer.stdout {
                base.stdout.send(line).await?;
            }
            for line in answer.stderr {
                base.stderr.send(line).await?;
            }
            base.stdout.close(None);
            base.stderr.close(None);
            base.exit_code.complete(Ok(answer.exit));
            Ok(CommandSession::new(
                base,
                SessionControl {
                    is_alive: Box::new(|| false),
                    terminate: Box::new(|| Ok(())),
                    kill: Box::new(|| Ok(())),
                    close: Box::new(|| Box::pin(async { Ok(()) })),
                },
            ))
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

type Logs = Arc<Mutex<Vec<Value>>>;
fn logging(logs: &Logs) -> SshLogging {
    let logs = logs.clone();
    SshLogging::new(
        Arc::new(Mutex::new(MarathonLogging::default())),
        Arc::new(
            move |logger: &Logger, level: Level, message: String, cause: Option<Failure>| {
                logs.lock().unwrap().push(json!({
                "level": level.as_str(), "logger": logger.name(), "message": message,
                "throwable": cause.map(|f| format!("{}: {}", f.class.to_owned(), f.message.map(|m| m.to_owned()).unwrap_or_default())),
            }));
            },
        ),
    )
}
fn timeouts() -> marathon_config::vendor::AppleTimeoutConfiguration {
    let s = Duration::of_seconds;
    let mut p = AppleTimeoutParameters::empty();
    p.shell = Some(s(31));
    p.shell_idle = Some(s(32));
    p.screenshot = Some(s(11));
    p.video = Some(s(301));
    p.erase = Some(s(33));
    p.shutdown = Some(s(34));
    p.delete = Some(s(35));
    p.create = Some(s(36));
    p.boot = Some(s(37));
    p.install = Some(s(38));
    p.uninstall = Some(s(39));
    p.import_media = Some(s(40));
    p.test_destination = Some(duration(41_500));
    p.xcresulttool = Some(s(121));
    apple_timeout_configuration(p)
}

fn describe(failure: &Failure) -> Value {
    json!({
        "class": failure.class.to_owned(),
        "message": failure.message.as_ref().map(|m| m.to_owned()),
        "cause": failure.cause.as_deref().map(describe),
    })
}
fn fail(error: VendorError) -> Value {
    match error {
        VendorError::Other(f) => describe(&f),
        other => panic!("unexpected {other:?}"),
    }
}
fn conn(error: ConnectionError) -> VendorError {
    error.into_vendor()
}
fn result(r: CommandResult) -> Value {
    json!({"stdout": r.stdout, "stderr": r.stderr, "exit": r.exit_code})
}
fn version(v: XcodeVersion) -> Value {
    json!([v.major, v.minor, v.patch])
}
fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}
fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}
fn test(pkg: &str, clazz: &str, method: &str) -> Test {
    Test {
        pkg: pkg.into(),
        clazz: clazz.into(),
        method: method.into(),
        meta_properties: vec![],
    }
}
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}
fn kotlin_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect()
}
fn xcode(name: &str) -> Option<XcodeVersion> {
    (name != "null").then(|| XcodeVersion::from(name).unwrap())
}
const R: &str = "/tmp/r p";

/// The scripted answers of a case that is not a plain `<form>/<variant>`.
fn answers(case: &str) -> Vec<Answer> {
    let springboard = [
        "\tA   com.apple.springboard.services",
        "x D com.apple.springboard.services",
    ];
    let sample =
        std::fs::read_to_string(fixtures().join("vendor-apple-base/fixtures/xctesttool/sample_1.json")).unwrap();
    let sample_lines = kotlin_lines(&sample);
    let lines = |v: &[String]| Answer {
        stdout: v.to_vec(),
        ..answer(&[], &[], Some(0))
    };
    match case {
        "codesign.identity/stdout" => vec![answer(
            &["Executable=/x", "Authority=Apple Development: A (B)", "Authority=Apple"],
            &[],
            Some(0),
        )],
        "codesign.identity/stderr-only" => vec![answer(&[], &["Authority=Apple Development: A (B)"], Some(0))],
        "codesign.sign/identity-query" => vec![answer(&["Authority=Q"], &[], Some(0)), answer(&[], &[], Some(0))],
        "csrutil/single-line" => vec![answer(&["System Integrity Protection status: disabled."], &[], Some(0))],
        "csrutil/line-disabled" => vec![answer(&["x", "disabled"], &[], Some(0))],
        "lipo/archs" => vec![answer(&[" x86_64  arm64 "], &[], Some(0))],
        "lipo/multiline" => vec![answer(&["x86_64", "arm64"], &[], Some(0))],
        "lipo/unknown" => vec![answer(&["arm64 arm64_32"], &[], Some(0))],
        "lipo/empty" => vec![answer(&[], &[], Some(0))],
        "nm.list/quote" => vec![],
        "nm.swift/lines" => vec![answer(&["  a  ", "", " ", "b"], &[], Some(0))],
        "nm.list/fail-blank" => vec![answer(&[" "], &[""], Some(1))],
        "nm.list/fail-multiline" => vec![answer(&["a", "  b"], &["c"], Some(1))],
        "nm.objc/fail-stderr" => vec![answer(&[], &["  nm: error"], Some(1))],
        "systemprofiler/udid" => vec![answer(
            &["Hardware:", "", "      Provisioning UDID: 0000-AB:CD ", "x"],
            &[],
            Some(0),
        )],
        "systemprofiler/none" => vec![answer(&["Hardware:"], &[], Some(0))],
        "xcodeselect.swift5/test-fails" => vec![answer(&[" /Dev "], &[], Some(0)), answer(&[], &[], Some(1))],
        "xcodeselect.swift5/test-conn" => vec![answer(&["/Dev"], &[], Some(0)), variant("conn")],
        "sim.create/udid" => vec![answer(
            &[
                "No runtime specified, using 'iOS 16.2'",
                " FE547FCD-A71D-4B09-BE3C-97C18FD0F667 ",
                "trailing words",
            ],
            &[],
            Some(0),
        )],
        "sim.create/empty-line" => vec![answer(&["FE547FCD-A71D", ""], &[], Some(0))],
        "sim.create/none" => vec![answer(&["lower-case"], &[], Some(0))],
        "sim.getenv/value" => vec![answer(&[" v ", ""], &[], Some(0))],
        "sim.running/blank-token" => vec![answer(
            &[
                "A \u{2003}com.apple.springboard.services",
                "com.apple.springboard.services",
            ],
            &[],
            Some(0),
        )],
        "xcodebuild.test/conn" => vec![variant("conn")],
        c if c.starts_with("xcodebuild.test/") => vec![],
        c if c.starts_with("sim.running/") => {
            let v = c.rsplit('/').next().unwrap();
            vec![match v {
                "ok" => answer(&springboard, &[], Some(0)),
                "fail" => answer(&springboard, &[], Some(1)),
                "nul" => answer(&springboard, &[], None),
                "D" => answer(&["D com.apple.springboard.services"], &[], Some(0)),
                other => panic!("{other}"),
            }]
        }
        c if c.starts_with("xcodebuild.version/") && !["ok", "fail", "null", "conn", "other"].contains(&&c[19..]) => {
            let out = &c["xcodebuild.version/".len()..];
            let out: Vec<&str> = if out.is_empty() {
                vec![]
            } else {
                out.split('|').collect()
            };
            vec![answer(&out, &["warning"], Some(0))]
        }
        c if c.starts_with("xcresulttool.get/") && c.ends_with("/json") => vec![lines(&sample_lines)],
        c if c.ends_with("/raw-id-version") => vec![answer(&["  "], &[], Some(0))],
        "xcresulttool.get/exit64" => vec![Answer {
            stdout: sample_lines.clone(),
            ..answer(&[], &["Error: deprecated", "Usage"], Some(64))
        }],
        "xcresulttool.get/null-exit" => vec![Answer {
            stdout: sample_lines.clone(),
            ..answer(&[], &["e"], None)
        }],
        "xcresulttool.get/conn" => vec![variant("conn")],
        "xcresulttool.get/other" => vec![variant("other")],
        "xcresulttool.get/truncated" => vec![lines(&[sample[..sample.len() / 2].to_owned()])],
        "xcresulttool.get/json-null" => vec![answer(&[" null "], &[], Some(0))],
        "xcresulttool.get/malformed" => vec![answer(&["{\"issues\": 5}"], &[], Some(0))],
        "xcresulttool.get/blank" => vec![answer(&["", " \t"], &[], Some(0))],
        "xcresulttool.get/trailing" => vec![answer(&["{} {"], &[], Some(0))],
        c if c.starts_with("device.list/list_output") => {
            let text = std::fs::read_to_string(
                fixtures()
                    .join("vendor-apple-base/fixtures/simctl")
                    .join(&c["device.list/".len()..]),
            )
            .unwrap();
            // File.readLines() drops the final empty line.
            let mut l = kotlin_lines(&text);
            if l.last().is_some_and(String::is_empty) {
                l.pop();
            }
            vec![lines(&l)]
        }
        c if c.contains("/shape/") => {
            let name = c.rsplit('/').next().unwrap();
            vec![lines(&kotlin_lines(shape(name)))]
        }
        c => {
            let (_, v) = c.rsplit_once('/').unwrap();
            vec![variant(v)]
        }
    }
}

fn shape(name: &str) -> &'static str {
    match name {
        "modern" => {
            r#"{"devicetypes":[{"name":"iPhone","identifier":"t","productFamily":"iPhone"}],"runtimes":[{"bundlePath":"/b","buildversion":"22A","isAvailable":true,"runtimeRoot":"/r","name":"iOS 18","identifier":"rt","version":"18.0"}],"devices":{"rt":[{"state":"Booted","name":"A","udid":"U1","logPath":"/l","isAvailable":false,"dataPath":"/d","availabilityError":"e","deviceTypeIdentifier":"t"}]}}"#
        }
        "order" => {
            r#"{"devices":{"z":[{"state":"Shutdown","name":"Z","udid":"1"}],"a":[{"state":"ShuttingDown","name":"A","udid":"2"},{"state":"Bogus","name":"B","udid":"3"}],"z":[{"state":"Creating","name":"Z2","udid":"4"}]},"devicetypes":[],"runtimes":[]}"#
        }
        "scalars" => {
            r#"{"devicetypes":[{"name":7,"identifier":true}],"runtimes":[{"name":1.50,"version":null}],"devices":{"r":[{"state":3,"name":12,"udid":"u","isAvailable":"TRUE","logPath":1e3},{"state":null,"name":"n","udid":"u2","isAvailable":1},{"state":{"x":1},"name":"o","udid":"u3"},{"state":true,"name":"b","udid":"u4"},{"name":"nostate","udid":"u5"},{"state":"Booted","udid":"noname"}]}}"#
        }
        "missing-lists" => r#"{"devices":{}}"#,
        "null-lists" => r#"{"devicetypes":null,"runtimes":null,"devices":null}"#,
        "null-name" => {
            r#"{"devicetypes":[],"runtimes":[],"devices":{"r":[{"state":"Booted","name":null,"udid":"u"}]}}"#
        }
        "object-name" => {
            r#"{"devicetypes":[],"runtimes":[],"devices":{"r":[{"state":"Booted","name":{},"udid":"u"}]}}"#
        }
        "group-object" => r#"{"devicetypes":[],"runtimes":[],"devices":{"r":{}}}"#,
        "devices-array" => r#"{"devicetypes":[],"runtimes":[],"devices":[]}"#,
        "runtime-object-field" => r#"{"devicetypes":[],"runtimes":[{"name":{}}],"devices":{}}"#,
        "null-element" => r#"{"devicetypes":[null],"runtimes":[],"devices":{}}"#,
        "syntax" => r#"{"devices":"#,
        "trailing" => r#"{"devicetypes":[],"runtimes":[],"devices":{}} x"#,
        "lenient" => r#"{devicetypes:[],'runtimes':[],devices:{}}"#,
        "empty" => "",
        "json-null" => "null",
        "array-root" => "[]",
        other => panic!("shape {other}"),
    }
}

async fn run(case: &str, e: &AppleBinaryEnvironment, c: &VendorContext) -> VendorResult<Value> {
    let key = case.split('/').next().unwrap();
    let parts: Vec<&str> = case.split('/').collect();
    let sim = &e.xcrun.simctl;
    let unit = |r: VendorResult<()>| r.map(|_| json!("unit"));
    Ok(match key {
        "chmod" => json!(e.chmod.make_executable("/a b/it's", c).await?),
        "codesign.identity" => json!(e.codesign.get_identity_for(R, c).await?),
        "codesign.team" => json!(e.codesign.get_development_team(R, c).await?),
        "codesign.sign.entitlements" => unit(e.codesign.sign(R, Some("/e.plist"), Some("ID"), c).await)?,
        "codesign.sign.preserve" => unit(e.codesign.sign(R, None, Some(""), c).await)?,
        "codesign.sign" => unit(e.codesign.sign(R, None, None, c).await)?,
        "csrutil" => json!(e.csrutil.status(c).await?),
        "getconf" => json!(e.getconf.get_darwin_user_cache_dir(c).await?),
        "getconf.key" => json!(e.getconf.get("", c).await?),
        "ioreg.udid" => json!(e.ioreg.get_udid(c).await?),
        "ioreg.manufacturer" => json!(e.ioreg.get_manufacturer(c).await?),
        "ioreg.model" => json!(e.ioreg.get_model(c).await?),
        "lipo.arch" | "lipo" => json!(
            e.lipo
                .get_arch(R, c)
                .await?
                .into_iter()
                .map(Arch::value)
                .collect::<Vec<_>>()
        ),
        "lipo.remove" => unit(e.lipo.remove_arch(R, Arch::arm64e, c).await)?,
        "nm.list" if case == "nm.list/quote" => json!(e.nm.list(R, &s(&["a'b"]), c).await?),
        "nm.list" if case.starts_with("nm.list/fail-") => json!(e.nm.list(R, &s(&["x"]), c).await?),
        "nm.list" => json!(e.nm.list(R, &s(&["b", "a b", ""]), c).await?),
        "nm.list.empty" => json!(e.nm.list(R, &[], c).await?),
        "nm.swift" => json!(e.nm.swift_tests(R, c).await?),
        "nm.objc" => json!(e.nm.objective_c_tests(R, c).await?),
        "plistbuddy.set" => json!(e.plist_buddy.set("/f.plist", ":K", "", c).await?),
        "plistbuddy.add" => json!(e.plist_buddy.add("/f.plist", ":K", "string", "v w", c).await?),
        "sqlite3" => unit(e.sqlite3.query("/d b", "select 'x'", true, c).await)?,
        "swvers" => json!(e.swvers.get_version(c).await?),
        "systemprofiler" => json!(e.system_profiler.get_provisioning_udid(c).await?),
        "xcodeselect.path" => json!(e.xcodeselect.get_developer_path(c).await?),
        "xcodeselect.swift5" => json!(e.xcodeselect.get_swift5_fallback_libs_dir(c).await?),
        "xcrun.platform" => json!(e.xcrun.get_sdk_platform_path(Sdk::WatchSimulator, c).await?),
        "xcrun.version" => json!(e.xcrun.get_sdk_version(Sdk::Macos, c).await?),
        "xcrun.xctest" => json!(e.xcrun.get_xctest_path(Sdk::Iphoneos, c).await?),
        "app.install" => result(sim.application.install("U", R, c).await?),
        "app.container" => result(
            sim.application
                .container_path("U", "b.id", ContainerType::Groups, c)
                .await?,
        ),
        "app.terminate" => sim
            .application
            .terminate_application("U", "b.id", c)
            .await?
            .map_or(Value::Null, result),
        "app.uninstall" => result(sim.application.uninstall_application("U", "b.id", c).await?),
        "sim.create" if ["udid", "empty-line", "none"].contains(&parts[1]) => {
            json!(sim.simulator.create("n", "t", None, c).await?)
        }
        "sim.create" => json!(sim.simulator.create("n m", "type", None, c).await?),
        "sim.create.runtime" => json!(sim.simulator.create("n", "type", Some("rt"), c).await?),
        "sim.boot" => json!(sim.simulator.boot("U", &pairs(&[("B", "2"), ("A", "")]), c).await?),
        "sim.getenv" => json!(sim.simulator.getenv("U", "K", c).await?),
        "sim.bootstatus" => json!(sim.simulator.monitor_status("U", c).await?),
        "sim.shutdown" => json!(sim.simulator.shutdown("U", c).await?),
        "sim.delete" => json!(sim.simulator.delete("U", c).await?),
        "sim.delete.unavailable" => json!(sim.simulator.delete_unavailable(c).await?),
        "sim.erase.all" => json!(sim.simulator.erase_all(c).await?),
        "sim.erase.one" => json!(sim.simulator.erase_udid("U", c).await?),
        "sim.erase.empty" => json!(sim.simulator.erase(&[], c).await?),
        "sim.running.15" => json!(sim.simulator.is_running("U", "15.5", c).await?),
        "sim.running.16" => json!(sim.simulator.is_running("U", "16.0", c).await?),
        "sim.running" if case == "sim.running/blank-token" => json!(sim.simulator.is_running("U", "17.0", c).await?),
        "sim.running" => json!(sim.simulator.is_running("U", parts[1], c).await?),
        "io.screenshot" => json!(
            sim.io
                .screenshot("U", R, ScreenshotType::Jpeg, Display::External, Mask::Black, c)
                .await?
        ),
        "io.video" => sim
            .io
            .record_video("U", R, Codec::Hevc, Display::Internal, Mask::Ignored, "/p id", c)
            .await?
            .map_or(Value::Null, result),
        "media.add" => result(sim.media_service.add_media("U", R, c).await?),
        "privacy.grant" => result(sim.privacy.grant("U", IosPermission::LocationAlways, "b.id", c).await?),
        "privacy.revoke" => result(sim.privacy.revoke("U", IosPermission::PhotosAdd, "b.id", c).await?),
        "privacy.reset" => result(sim.privacy.reset("U", IosPermission::All, "b.id", c).await?),
        "privacy.resetAll" => result(sim.privacy.reset_all("U", IosPermission::ContactsLimited, c).await?),
        "spawn.default" => result(sim.spawn.spawn("U", &s(&["a", ""]), None, c).await?),
        "spawn.timeout" => result(sim.spawn.spawn("U", &[], Some(duration(1)), c).await?),
        "device.list" if parts.len() == 2 && parts[1].starts_with("list_output") => {
            let o = sim.device.list(c).await.map_err(conn)?;
            json!({
                "devices": o.devices.devices.iter().map(|d| json!([d.runtime, d.state.name(), d.name, d.udid, d.log_path, d.is_available, d.availability_error, d.data_path, d.device_type_identifier])).collect::<Vec<_>>(),
                "runtimes": o.runtimes.iter().map(|r| json!([r.bundle, r.build_version, r.is_available, r.runtime_root, r.name, r.identifier, r.version])).collect::<Vec<_>>(),
                "devicetypes": o.devicetypes.iter().map(|t| json!([t.name, t.identifier])).collect::<Vec<_>>(),
            })
        }
        "device.list" if parts.get(1) == Some(&"shape") => {
            let o = sim.device.list(c).await.map_err(conn)?;
            json!({
                "devices": o.devices.devices.iter().map(|d| json!([d.runtime, d.state.name(), d.name, d.udid, d.log_path, d.is_available, d.device_type_identifier])).collect::<Vec<_>>(),
                "runtimes": o.runtimes.iter().map(|r| json!([r.bundle, r.build_version, r.is_available, r.name, r.version])).collect::<Vec<_>>(),
                "devicetypes": o.devicetypes.iter().map(|t| json!([t.name, t.identifier])).collect::<Vec<_>>(),
            })
        }
        "device.list" => {
            sim.device.list(c).await.map_err(conn)?;
            json!("listed")
        }
        "device.listDevices" => json!(sim.device.list_devices(c).await.map_err(conn)?.len()),
        "xcodebuild.version" => version(e.xcrun.xcodebuild.get_version(c).await?),
        "xcodebuild.test" => {
            let x = &e.xcrun.xcodebuild;
            match parts[1] {
                "full" => {
                    let mut request = TestRequest::new("/w d", "/x.xctestrun", true);
                    request.tests = Some(vec![
                        test("com.x", "Cls", "m1"),
                        test("", "Cls", "m 2"),
                        test("com.x", "Cls", "m1"),
                    ]);
                    request.xcresult = Some("/r.xcresult".into());
                    request.test_target_name = Some("Target".into());
                    let user = pairs(&[
                        ("-destination", "user"),
                        ("-xctestrun", "/user.xctestrun"),
                        ("-empty", ""),
                        ("-enableCodeCoverage", "maybe"),
                        ("-z", "1"),
                    ]);
                    x.test_without_building("U-1", Sdk::TvSimulator, &request, &user, c)
                        .await?;
                }
                "minimal" => {
                    let mut request = TestRequest::new("/w", "/x.xctestrun", false);
                    request.tests = Some(vec![]);
                    x.test_without_building("U", Sdk::Macos, &request, &[], c).await?;
                }
                "null-tests" => {
                    let request = TestRequest::new("/w", "/x.xctestrun", false);
                    x.test_without_building(
                        "U",
                        Sdk::Iphoneos,
                        &request,
                        &pairs(&[("-collect-test-diagnostics", "never")]),
                        c,
                    )
                    .await?;
                }
                "conn" => {
                    x.test_without_building("U", Sdk::Iphoneos, &TestRequest::new("/w", "/x", false), &[], c)
                        .await?;
                }
                other => panic!("{other}"),
            }
            json!("session")
        }
        "xcresulttool.merge" => json!(e.xcrun.xcresulttool.merge(R, &s(&["/a", ""]), c).await?),
        "xcresulttool.merge.none" => json!(e.xcrun.xcresulttool.merge(R, &[], c).await?),
        "xcresulttool.get" => {
            let tool = &e.xcrun.xcresulttool;
            match parts.as_slice() {
                [_, v, "json"] => json!(
                    tool.get::<ActionsInvocationRecord>(R, xcode(v), ResultBundleFormat::Json, None, None, c)
                        .await?
                        .map(|r| r.issues.test_failure_summaries.len())
                ),
                [_, v, "raw-id-version"] => json!(
                    tool.get::<ActionsInvocationRecord>(R, xcode(v), ResultBundleFormat::Raw, Some(""), Some("3"), c)
                        .await?
                        .map(|_| "record")
                ),
                _ => json!(
                    tool.get::<ActionsInvocationRecord>(
                        R,
                        Some(XcodeVersion::new(26, 6, 0)),
                        ResultBundleFormat::Json,
                        None,
                        None,
                        c
                    )
                    .await?
                    .is_some()
                ),
            }
        }
        other => panic!("no Rust call for {other} ({case})"),
    })
}

/// The approved differences: Kotlin's measured value is replaced by the Rust one, with the reason.
fn deviation(case: &str, kotlin: &Value) -> Option<Value> {
    let unavailable = |stdout: &str| {
        json!({"error": {"class": "com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException",
                         "message": format!("Error parsing simctl output on local: [{stdout}]"), "cause": null}})
    };
    let shape_name = case.rsplit('/').next().unwrap_or_default();
    // P2: shapes Gson accepts leniently, leaks nulls from, or rejects outside JsonSyntaxException are one
    // ServicesUnavailable failure.
    if case.contains("/shape/")
        && [
            "missing-lists",
            "null-lists",
            "null-name",
            "object-name",
            "null-element",
            "lenient",
            "empty",
            "json-null",
        ]
        .contains(&shape_name)
    {
        let mut value = kotlin.clone();
        value["outcome"] = unavailable(shape(shape_name));
        return Some(value);
    }
    // P1: non-ASCII digits parse in Kotlin only.
    if case.starts_with("sim.running/１６.0/") {
        let mut value = kotlin.clone();
        value["requests"][0]["argv"] = json!(["xcrun", "simctl", "spawn", "U", "launchctl", "print", "system"]);
        if case.ends_with("/fail") || case.ends_with("/nul") {
            value["outcome"] = json!({"return": false});
        }
        return Some(value);
    }
    // D38: a failed or timed-out dumpstate probe is not running. Only the return value changes.
    if d38_row(case, kotlin) {
        let mut value = kotlin.clone();
        value["outcome"] = json!({"return": false});
        return Some(value);
    }
    None
}

/// The JVM rows that stay the witness of A41: a dumpstate probe that names a started SpringBoard
/// service, reported as running although it failed or has no exit status.
fn d38_row(case: &str, kotlin: &Value) -> bool {
    let dumpstate = kotlin["requests"][0]["argv"][5] == json!("dumpstate");
    // The non-ASCII digit rows run the older probe here, under P1.
    case.starts_with("sim.running/")
        && !case.starts_with("sim.running/１６")
        && (case.ends_with("/fail") || case.ends_with("/nul"))
        && dumpstate
        && kotlin["outcome"] == json!({"return": true})
}

fn plain(value: &Value) -> Value {
    let mut value = value.clone();
    // Rust env maps are sorted (the executors apply them as a set); Kotlin records LinkedHashMap order.
    if let Some(requests) = value.get_mut("requests").and_then(Value::as_array_mut) {
        for request in requests {
            if let Some(env) = request.get_mut("env").and_then(Value::as_array_mut) {
                env.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
            }
        }
    }
    // The parse cause is WP 4.6's structured failure, not Jackson's message (decision 5): compare presence.
    if let Some(logs) = value.get_mut("logs").and_then(Value::as_array_mut) {
        for log in logs {
            if log["throwable"].is_string() {
                log["throwable"] = json!("<cause>");
            }
        }
    }
    value
}

fn rows() -> Vec<(String, Value)> {
    std::fs::read_to_string(fixtures().join("oracle/apple-bin/probes.jsonl"))
        .unwrap()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            (v["case"].as_str().unwrap().to_owned(), v["value"].clone())
        })
        .collect()
}

/// Runs one case against the scripted executor and returns what the Rust wrapper requested and answered.
async fn observe(case: &str) -> Value {
    let executor = Arc::new(Scripted {
        host: KotlinProcessHost::host(),
        logger: MarathonLogging::default().logger("probe.Recording", None),
        answers: Mutex::new(answers(case).into()),
        requests: Mutex::new(vec![]),
    });
    let logs: Logs = Arc::default();
    let tools = Tools {
        executor: executor.clone(),
        timeouts: Arc::new(timeouts()),
        runtime: Arc::new(Runtime),
        logging: logging(&logs),
    };
    let env = AppleBinaryEnvironment::with_tools(tools, 1_234_567, 7_654);
    let outcome = match run(case, &env, &context()).await {
        Ok(value) => json!({"return": value}),
        Err(error) => json!({"error": fail(error)}),
    };
    json!({
        "requests": *executor.requests.lock().unwrap(),
        "outcome": outcome,
        "logs": *logs.lock().unwrap(),
    })
}

#[tokio::test]
async fn every_probed_wrapper_call_matches_kotlin() {
    let (mut checked, mut mismatches) = (0, Vec::new());
    for (case, kotlin) in rows() {
        if kotlin.get("requests").is_none() {
            continue;
        }
        let rust = observe(&case).await;
        let expected = deviation(&case, &kotlin).unwrap_or(kotlin);
        if plain(&rust) != plain(&expected) {
            mismatches.push(format!(
                "{case}\n  rust:   {}\n  kotlin: {}",
                plain(&rust),
                plain(&expected)
            ));
        }
        checked += 1;
    }
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert_eq!(checked, 409);
}

#[test]
fn d38_allowance_covers_only_failed_dumpstate_probes_that_kotlin_reported_as_running() {
    let rows = rows();
    let mut covered: Vec<&str> = rows
        .iter()
        .filter(|(case, kotlin)| d38_row(case, kotlin))
        .map(|(case, _)| case.as_str())
        .collect();
    covered.sort_unstable();
    assert_eq!(
        covered,
        [
            "sim.running/+16.1/fail",
            "sim.running/+16.1/nul",
            "sim.running/16.0/fail",
            "sim.running/16.0/nul",
            "sim.running/16/fail",
            "sim.running/16/nul",
        ]
    );
    let row = |case: &str| rows.iter().find(|(c, _)| c == case).map(|(_, kotlin)| kotlin).unwrap();
    // A healthy probe, a starting one and the older runtime's probe stay exactly as recorded.
    for case in ["sim.running/16.0/ok", "sim.running/16.0/D", "sim.running/15.9/fail"] {
        assert!(deviation(case, row(case)).is_none(), "{case}");
    }
    // The allowance needs both the dumpstate request and Kotlin's old answer.
    let mut print = row("sim.running/16.0/fail").clone();
    print["requests"][0]["argv"][5] = json!("print");
    assert!(!d38_row("sim.running/16.0/fail", &print));
    let mut not_running = row("sim.running/16.0/fail").clone();
    not_running["outcome"] = json!({"return": false});
    assert!(!d38_row("sim.running/16.0/fail", &not_running));
}

/// D46: what a JVM `sim.running/16.0` row becomes on a runtime of 18 or later. Only the probe's argv changes; the
/// answers, the timeouts, the environment and the outcome stay what Kotlin's row recorded (and D38's, where it applies).
fn d46_expected(case: &str, kotlin: &Value) -> Value {
    let mut value = deviation(case, kotlin).unwrap_or_else(|| kotlin.clone());
    let argv = &mut value["requests"][0]["argv"];
    assert_eq!(
        argv,
        &json!([
            "xcrun",
            "simctl",
            "spawn",
            "U",
            "launchctl",
            "dumpstate",
            "user/foreground/com.apple.SpringBoard"
        ])
    );
    *argv = json!(["xcrun", "simctl", "spawn", "U", "launchctl", "print", "user/foreground"]);
    value
}

#[tokio::test]
async fn d46_runtime_18_and_later_probe_with_print_user_foreground_and_nothing_else_changes() {
    let rows = rows();
    let row = |case: &str| rows.iter().find(|(c, _)| c == case).map(|(_, kotlin)| kotlin).unwrap();
    for runtime in ["18", "18.0", "18.6", "26.4", "27.0", "+18.1", "99.9"] {
        for variant in ["ok", "fail", "nul", "D"] {
            let witness = format!("sim.running/16.0/{variant}");
            let case = format!("sim.running/{runtime}/{variant}");
            assert_eq!(
                plain(&observe(&case).await),
                plain(&d46_expected(&witness, row(&witness))),
                "{case}"
            );
        }
    }
}

#[tokio::test]
async fn d46_runtimes_16_and_17_keep_the_dumpstate_probe_exactly_as_recorded() {
    let rows = rows();
    let row = |case: &str| rows.iter().find(|(c, _)| c == case).map(|(_, kotlin)| kotlin).unwrap();
    for runtime in ["16", "16.0", "16.9", "17", "17.0", "17.5", "+17.0"] {
        for variant in ["ok", "fail", "nul", "D"] {
            let witness = format!("sim.running/16.0/{variant}");
            let case = format!("sim.running/{runtime}/{variant}");
            let kotlin = row(&witness);
            let expected = deviation(&witness, kotlin).unwrap_or_else(|| kotlin.clone());
            assert_eq!(plain(&observe(&case).await), plain(&expected), "{case}");
        }
    }
}

#[tokio::test]
async fn d46_a_runtime_that_does_not_parse_keeps_the_print_system_probe() {
    let rows = rows();
    for runtime in ["", "x", "15.9", "-1.0", "2147483648.0", "１８.0", "１６.0", "18x"] {
        let rust = observe(&format!("sim.running/{runtime}/ok")).await;
        assert_eq!(
            rust["requests"][0]["argv"],
            json!(["xcrun", "simctl", "spawn", "U", "launchctl", "print", "system"]),
            "{runtime:?}"
        );
    }
    // The JVM rows are the witness of Kotlin's own choice for the same versions.
    for case in [
        "sim.running//ok",
        "sim.running/x/ok",
        "sim.running/15.9/ok",
        "sim.running/-1.0/ok",
    ] {
        let kotlin = rows.iter().find(|(c, _)| c == case).map(|(_, kotlin)| kotlin).unwrap();
        assert_eq!(kotlin["requests"][0]["argv"][6], "system", "{case}");
    }
}

#[test]
fn d46_allowance_changes_the_dumpstate_witness_in_its_argv_only() {
    let rows = rows();
    let row = |case: &str| rows.iter().find(|(c, _)| c == case).map(|(_, kotlin)| kotlin).unwrap();
    // The witnesses: Kotlin asks every runtime from 16 on, 18 included, for dumpstate.
    for variant in ["ok", "D", "fail", "nul"] {
        let case = format!("sim.running/16.0/{variant}");
        let kotlin = deviation(&case, row(&case)).unwrap_or_else(|| row(&case).clone());
        assert_eq!(kotlin["requests"][0]["argv"][5], "dumpstate", "{case}");
        let expected = d46_expected(&case, row(&case));
        assert_eq!(
            expected["requests"][0]["argv"],
            json!(["xcrun", "simctl", "spawn", "U", "launchctl", "print", "user/foreground"])
        );
        // Everything but that argv is Kotlin's row (with D38's answer where it applies).
        let (mut left, mut right) = (expected.clone(), kotlin.clone());
        left["requests"][0]["argv"] = json!(null);
        right["requests"][0]["argv"] = json!(null);
        assert_eq!(left, right, "{case}");
        // A call that kept the old argv on runtime 18 would not equal the expectation.
        assert_ne!(plain(&expected), plain(&kotlin), "{case}");
    }
}

#[test]
fn pure_helpers_match_kotlin() {
    let mut checked = 0;
    for (case, kotlin) in rows() {
        let (key, arg) = case.split_once('/').unwrap_or((&case, ""));
        let rust = match key {
            "simulatorTestArgs" => {
                let user = pairs(&[("-b", "1"), ("-collect-test-diagnostics", "on-failure"), ("-a", "")]);
                let render = |v: Vec<(String, String)>| v.into_iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>();
                json!([
                    render(simulator_test_args(&user, xcode(arg))),
                    render(simulator_test_args(&[], xcode(arg)))
                ])
            }
            "describeTimeout" => {
                let mut p = AppleTimeoutParameters::empty();
                if arg == "fractional" {
                    p.shell = Some(duration(120_500));
                } else {
                    let (shell, x) = arg.split_once('/').unwrap();
                    p.shell = Some(Duration::of_seconds(shell.parse().unwrap()));
                    p.xcresulttool = x.parse().ok().map(Duration::of_seconds);
                }
                json!(describe_timeout(&apple_timeout_configuration(p)))
            }
            "XcodeVersion" => match XcodeVersion::from(arg) {
                Ok(v) => version(v),
                Err(e) => fail(e),
            },
            "XcodeVersion.compare" => {
                let (a, b) = arg.split_once('/').unwrap();
                let parse = |t: &str| -> Vec<i32> { serde_json::from_str(t).unwrap() };
                let (a, b) = (parse(a), parse(b));
                let ordering = XcodeVersion::new(a[0], a[1], a[2]).cmp(&XcodeVersion::new(b[0], b[1], b[2]));
                // Only the sign is meaningful; P1 drops Kotlin's subtraction overflow.
                let kotlin_sign = kotlin.as_i64().unwrap().signum();
                let rust_sign = ordering as i64;
                let expected = match (a.as_slice(), b.as_slice()) {
                    ([i32::MIN, ..], _) => -1,
                    ([_, _, i32::MAX], [_, _, -1]) => 1,
                    _ => kotlin_sign,
                };
                assert_eq!(rust_sign, expected, "{case}");
                checked += 1;
                continue;
            }
            "AppleOperatingSystemVersion" => match AppleOperatingSystemVersion::new(arg) {
                Ok(v) => json!([v.major, v.minor, v.patch]),
                Err(e) => fail(e),
            },
            "AppleOperatingSystemVersion.compare" => {
                let v = |t| AppleOperatingSystemVersion::new(t).unwrap();
                json!([v("17.4") > v("17.3.9"), false, v("17").cmp(&v("17.0.0")) as i32])
            }
            "enums" => {
                let containers = [ContainerType::Application, ContainerType::Data, ContainerType::Groups]
                    .map(|c| json!([format!("{c:?}").to_uppercase(), c.value()]));
                let formats = [ResultBundleFormat::Json, ResultBundleFormat::Raw]
                    .map(|f| json!([format!("{f:?}").to_uppercase(), f.value()]));
                json!({
                    "Sdk": Sdk::ALL.map(|s| json!([s.name(), s.value(), s.platform_name(), s.destination(), s.to_string()])),
                    "Arch": Arch::ALL.map(|a| json!([a.value(), a.value()])),
                    "OS": Os::ALL.map(|o| json!([o.name(), o.value()])),
                    "Platform": Platform::ALL.map(|p| json!([p.name(), p.value()])),
                    "ContainerType": containers,
                    "ResultBundleFormat": formats,
                })
            }
            _ => continue,
        };
        // P1: non-ASCII digits are rejected.
        let expected = match case.as_str() {
            "XcodeVersion/１４.0" => {
                json!({"class": "java.lang.NumberFormatException", "message": "For input string: \"１４\"", "cause": null})
            }
            "AppleOperatingSystemVersion/１７" => {
                json!({"class": "com.malinskiy.marathon.exceptions.IncompatibleDeviceException",
                                                       "message": "Unable to parser operating system version １７", "cause": null})
            }
            _ => kotlin,
        };
        assert_eq!(rust, expected, "{case}");
        checked += 1;
    }
    assert_eq!(checked, 39);
}
