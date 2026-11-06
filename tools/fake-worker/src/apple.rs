//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/FakeSimulatorWorker.kt
//! Minimal shared Apple world for the recorded SSH scenarios; no host Apple tools.
use crate::{
    fs::VirtualFs,
    router::{Handler, HandlerFuture, Request},
    script::{Action, Chunk, Matcher, PublishedFile},
};
use anyhow::{Result, ensure};
use base64::{Engine, prelude::BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Cursor,
    sync::{Arc, Mutex},
};

pub const UDID: &str = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
pub const DEVICE_TYPE: &str = "com.apple.CoreSimulator.SimDeviceType.iPhone-15-Pro";
pub const RUNTIME: &str = "com.apple.CoreSimulator.SimRuntime.iOS-27-1";
const HOME: &str = "/Users/worker";
const CONTROL: &str = "/tmp/marathon/.apple-world";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub schema: u32,
    pub name: String,
    pub platform: String,
    #[serde(default = "default_devices")]
    pub devices: Vec<String>,
    pub target: String,
    pub tests: Vec<Test>,
    #[serde(default)]
    pub media: Media,
    #[serde(default)]
    pub expectations: Vec<Expectation>,
    /// Separate line delivery from event consumption in paired listener probes.
    #[serde(default)]
    pub line_delay_ms: u64,
    #[serde(default)]
    pub parse_only: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Test {
    pub class: String,
    pub method: String,
    pub status: String,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Media {
    #[default]
    None,
    Video,
    Screenshot,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    pub name: String,
    pub matcher: Matcher,
    pub min: usize,
    pub max: usize,
}
#[derive(Default, Serialize)]
pub struct State {
    pub devices: BTreeMap<String, Device>,
    pub calls: BTreeMap<String, usize>,
    pub sessions: Vec<Session>,
    pub permissions: Vec<Vec<String>>,
    pub deferred: Vec<String>,
}
#[derive(Clone, Serialize)]
pub struct Device {
    pub state: String,
    pub available: bool,
    pub keyboard: bool,
    pub installed: Vec<String>,
}
#[derive(Serialize)]
pub struct Session {
    pub destination: String,
    pub discovery: bool,
    pub selected: Vec<String>,
    pub result: Option<String>,
    pub xctestrun: String,
    pub state: String,
    pub plist_base64: String,
}
#[derive(Clone)]
pub struct World {
    scenario: Arc<Scenario>,
    state: Arc<Mutex<State>>,
}
impl World {
    pub fn new(scenario: Scenario) -> Result<Self> {
        ensure!(scenario.schema == 1, "unsupported Apple scenario schema");
        ensure!(
            matches!(scenario.platform.as_str(), "ios" | "macos"),
            "invalid platform"
        );
        ensure!(!scenario.tests.is_empty(), "empty scenario");
        for test in &scenario.tests {
            ensure!(
                matches!(test.status.as_str(), "passed" | "failed" | "skipped"),
                "invalid test status"
            );
            ensure!(
                [&scenario.target, &test.class, &test.method]
                    .iter()
                    .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')),
                "nonplain test identity"
            );
        }
        ensure!(
            scenario.media == Media::None || scenario.tests.len() == 1,
            "media profile requires one test"
        );
        let mut names = std::collections::BTreeSet::new();
        for e in &scenario.expectations {
            ensure!(
                e.min <= e.max && names.insert(e.name.clone()),
                "invalid/duplicate expectation {}",
                e.name
            );
        }
        let mut state = State::default();
        for id in &scenario.devices {
            ensure!(
                id.len() == 36
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-'),
                "invalid device ID"
            );
            ensure!(!state.devices.contains_key(id), "duplicate device ID");
            state.devices.insert(
                id.clone(),
                Device {
                    state: "Shutdown".into(),
                    available: true,
                    keyboard: true,
                    installed: vec![],
                },
            );
        }
        Ok(Self {
            scenario: Arc::new(scenario),
            state: Arc::new(Mutex::new(state)),
        })
    }
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("Apple world state poisoned"))
    }
    pub fn snapshot(&self) -> Result<Value> {
        Ok(canonical_json(serde_json::to_value(&*self.state()?)?))
    }
    pub async fn final_state(&self, fs: &VirtualFs) -> Result<Value> {
        let n = self.state()?.sessions.len();
        let mut states = vec![];
        for i in 0..n {
            states.push(if fs.stat(&format!("{CONTROL}/session-{i}.finished")).await.is_ok() {
                "finished"
            } else {
                "cancelled"
            });
        }
        for (session, state) in self.state()?.sessions.iter_mut().zip(states) {
            session.state = state.into();
        }
        self.snapshot()
    }
    pub fn validate(&self) -> Result<()> {
        let state = self.state()?;
        ensure!(
            state.deferred.is_empty(),
            "deferred commands reached: {:?}",
            state.deferred
        );
        for e in &self.scenario.expectations {
            let n = state.calls.get(&e.name).copied().unwrap_or(0);
            ensure!(
                (e.min..=e.max).contains(&n),
                "expectation {}: got {n}, required {}..={}",
                e.name,
                e.min,
                e.max
            );
        }
        if self.scenario.parse_only {
            ensure!(
                state.sessions.iter().all(|s| s.discovery),
                "parse started an execution batch"
            );
        } else {
            ensure!(state.sessions.iter().any(|s| !s.discovery), "no batch session");
        }
        ensure!(
            state.sessions.iter().all(|s| s.state == "finished"),
            "unfinished session"
        );
        Ok(())
    }
    fn inventory(&self) -> Result<String> {
        let devices = self.state()?.devices.iter().map(|(id,d)| json!({"udid":id,"name":"Apple world","state":d.state,"isAvailable":d.available,"logPath":format!("{HOME}/Library/Logs/CoreSimulator/{id}"),"deviceTypeIdentifier":DEVICE_TYPE})).collect::<Vec<_>>();
        Ok(canonical_json(json!({"devicetypes":[{"name":"iPhone 15 Pro","identifier":DEVICE_TYPE}], "runtimes":[{"identifier":RUNTIME,"name":"iOS 27.1","version":"27.1"}], "devices":{RUNTIME:devices}})).to_string())
    }
    fn device(&self, id: &str) -> Result<Option<Device>> {
        Ok(self.state()?.devices.get(id).cloned())
    }
    fn deferred(&self, r: &Request) -> Result<Action> {
        self.state()?.deferred.push(r.text.clone());
        Ok(error(
            &format!("Apple world: recognised but deferred: {}\n", r.text),
            64,
        ))
    }
    fn reserve(&self, r: &Request) -> Result<Reservation> {
        let names = self
            .scenario
            .expectations
            .iter()
            .filter(|e| e.matcher.matches(&r.text))
            .collect::<Vec<_>>();
        let mut state = self.state()?;
        for e in &names {
            ensure!(
                state.calls.get(&e.name).copied().unwrap_or(0) < e.max,
                "expectation {} exhausted",
                e.name
            );
        }
        for e in &names {
            *state.calls.entry(e.name.clone()).or_default() += 1;
        }
        Ok(Reservation {
            world: self.clone(),
            names: names.iter().map(|e| e.name.clone()).collect(),
            committed: false,
        })
    }
    async fn answer(&self, r: &Request, fs: &VirtualFs) -> Result<Option<Action>> {
        if catalogue_deferral(&r.text)? {
            return Ok(Some(self.deferred(r)?));
        }
        let a = r.argv.iter().map(String::as_str).collect::<Vec<_>>();
        let out = match a.as_slice() {
            ["env"] => ok(&format!("HOME={HOME}\nUSER=worker\n")),
            ["uname", "-m"] => ok("arm64\n"),
            ["sw_vers", "--productVersion"] => ok("27.0\n"),
            ["xcrun", "xcodebuild", "-version"] => ok("Xcode 27.1\nBuild version 27A9275\n"),
            [
                "/usr/sbin/system_profiler",
                "-detailLevel",
                "mini",
                "-xml",
                "SPDisplaysDataType",
            ] => ok("<string>spdisplays_metalfamily</string>\n"),
            ["/opt/homebrew/bin/ffmpeg", "-version"] => ok("ffmpeg version scripted-0.4b\n"),
            ["csrutil", "status"] => ok("disabled\n"),
            ["xcrun", "simctl", "list", "--json"] => ok(&self.inventory()?),
            ["xcrun", "simctl", "getenv", id, key] => {
                if self.device(id)?.is_none() {
                    return Ok(Some(invalid(id)));
                }
                match *key {
                    "HOME" => ok(&format!("{HOME}/Library/Developer/CoreSimulator/Devices/{id}/data\n")),
                    "SIMULATOR_MODEL_IDENTIFIER" => ok("iPhone16,1\n"),
                    "SIMULATOR_RUNTIME_VERSION" => ok("27.1\n"),
                    "SIMULATOR_RUNTIME_BUILD_VERSION" => ok("24A94232\n"),
                    "SIMULATOR_VERSION_INFO" => ok("CoreSimulator scripted\n"),
                    _ => return Ok(Some(self.deferred(r)?)),
                }
            }
            ["xcrun", "simctl", op @ ("boot" | "shutdown"), id] => {
                let mut state = self.state()?;
                let Some(d) = state.devices.get_mut(*id) else {
                    return Ok(Some(invalid(id)));
                };
                d.state = if *op == "boot" { "Booted" } else { "Shutdown" }.into();
                ok("")
            }
            [
                "xcrun",
                "simctl",
                "spawn",
                id,
                "launchctl",
                "dumpstate",
                "user/foreground/com.apple.SpringBoard",
            ] => match self.device(id)? {
                Some(d) if d.state == "Booted" => ok("A com.apple.springboard.services\n"),
                Some(_) => error("SpringBoard not running\n", 1),
                None => invalid(id),
            },
            ["xcrun", "simctl", "install", id, path] => {
                if self.device(id)?.is_none() {
                    return Ok(Some(invalid(id)));
                }
                let bytes = fs.read(&format!("{path}/Info.plist")).await?;
                let plist = plist::Value::from_reader(Cursor::new(bytes))?;
                let bundle = plist
                    .as_dictionary()
                    .and_then(|d| d.get("CFBundleIdentifier"))
                    .and_then(plist::Value::as_string)
                    .ok_or_else(|| anyhow::anyhow!("bundle ID missing"))?;
                self.state()?
                    .devices
                    .get_mut(*id)
                    .ok_or_else(|| anyhow::anyhow!("device removed"))?
                    .installed
                    .push(bundle.into());
                ok("")
            }
            ["cat", path] if path.starts_with("/tmp/marathon/") => match fs.read(path).await {
                Ok(b) => bytes(b),
                Err(_) => error("No such file\n", 1),
            },
            ["cat", path]
                if path.starts_with(&format!("{HOME}/Library/Developer/CoreSimulator/Devices/"))
                    && path.ends_with("/device.plist") =>
            {
                let id = path
                    .strip_prefix(&format!("{HOME}/Library/Developer/CoreSimulator/Devices/"))
                    .and_then(|p| p.strip_suffix("/device.plist"))
                    .ok_or_else(|| anyhow::anyhow!("device resource"))?;
                let Some(d) = self.device(id)? else {
                    return Ok(Some(invalid(id)));
                };
                ok(&format!(
                    "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>state</key><integer>{}</integer><key>deviceType</key><string>{DEVICE_TYPE}</string><key>name</key><string>Apple world</string></dict></plist>\n",
                    if d.state == "Booted" { 3 } else { 1 }
                ))
            }
            ["/usr/libexec/PlistBuddy", "-c", command, path]
                if *path == format!("{HOME}/Library/Preferences/com.apple.iphonesimulator.plist") =>
            {
                let id = command
                    .strip_prefix("Add :DevicePreferences:")
                    .and_then(|s| s.strip_suffix(":ConnectHardwareKeyboard bool false"))
                    .or_else(|| {
                        command
                            .strip_prefix("Set :DevicePreferences:")
                            .and_then(|s| s.strip_suffix(":ConnectHardwareKeyboard false"))
                    })
                    .ok_or_else(|| anyhow::anyhow!("unsupported preference mutation"))?;
                self.state()?
                    .devices
                    .get_mut(id)
                    .ok_or_else(|| anyhow::anyhow!("device removed"))?
                    .keyboard = false;
                ok("")
            }
            ["chmod", "+x", path] => {
                fs.make_executable(path).await?;
                ok("")
            }
            ["lipo", path, "-archs"] => {
                fs.stat(path).await?;
                ok("arm64 x86_64\n")
            }
            [
                "xcrun",
                "--sdk",
                "iphonesimulator" | "macosx",
                "--show-sdk-platform-path",
            ] => ok("/Applications/Xcode.app/Contents/Developer/Platforms/iPhoneSimulator.platform\n"),
            ["sqlite3", path, query]
                if matches!(
                    *path,
                    "/Library/Application Support/com.apple.TCC/TCC.db"
                        | "/Users/worker/Library/Application Support/com.apple.TCC/TCC.db"
                ) =>
            {
                ensure!(query.starts_with("replace into access (service,client,client_type,auth_value,auth_reason,auth_version,indirect_object_identifier,flags,last_modified) values ("),"unsupported TCC query");
                self.state()?
                    .permissions
                    .push(vec![path.to_string(), query.to_string()]);
                ok("")
            }
            ["pkill", "-f", filter] if filter.starts_with("simctl io ") && filter.ends_with(" recordVideo") => {
                let id = filter.trim_start_matches("simctl io ").trim_end_matches(" recordVideo");
                ensure!(self.device(id)?.is_some(), "invalid recording device");
                // The baseline starts from a clean world. A live recording is stopped
                // through the owned pidfile/INT wrapper, never a fabricated host PID.
                error("", 1)
            }
            ["sh", "-c", command] => return self.shell(command, r, fs).await,
            ["xcrun", "xcodebuild", "test-without-building", rest @ ..] => {
                return Ok(Some(self.xcodebuild(r, rest, fs).await?));
            }
            [
                "xcrun",
                "xcresulttool",
                "get",
                "--legacy",
                "--format",
                "json",
                "--path",
                path,
                tail @ ..,
            ] => {
                let file = match *tail {
                    [] => "root.json",
                    ["--id", "tests-0"] => "tests-0.json",
                    _ => return Ok(Some(self.deferred(r)?)),
                };
                bytes(fs.read(&format!("{path}/{file}")).await?)
            }
            [
                "xcrun",
                "simctl",
                "io",
                id,
                "recordVideo",
                "--codec",
                "h264",
                "--display",
                "internal",
                "--mask",
                "black",
                "--force",
                path,
            ] => {
                ensure!(
                    self.device(id)?.is_some_and(|d| d.state == "Booted"),
                    "recording requires booted device"
                );
                ensure!(self.scenario.media == Media::Video, "video outside media scenario");
                let test = self
                    .scenario
                    .tests
                    .iter()
                    .find(|t| path.contains(&t.method))
                    .ok_or_else(|| anyhow::anyhow!("recording test not selected"))?;
                fs.mkdir(CONTROL).await?;
                let mut action = ok("");
                if let Action::Output { hang, .. } = &mut action {
                    *hang = true;
                }
                Action::Publish {
                    output: Box::new(action),
                    files: vec![file(
                        path,
                        include_bytes!("../../../fixtures/fake-worker/apple/media/recording.mp4"),
                    )],
                    started: vec![file(&device_barrier(id, &test.method), b"")],
                    on_signal: Some("INT".into()),
                }
            }
            [
                "xcrun",
                "simctl",
                "io",
                id,
                "screenshot",
                "--type=png",
                "--display=internal",
                "--mask=black",
                path,
            ] => {
                ensure!(
                    self.device(id)?.is_some_and(|d| d.state == "Booted"),
                    "screenshot requires booted device"
                );
                fs.write(
                    path,
                    include_bytes!("../../../fixtures/fake-worker/apple/media/screenshot.png").to_vec(),
                    0o600,
                )
                .await?;
                fs.mkdir(CONTROL).await?;
                // A second request proves Kotlin pulled and wrote the first frame.
                // Suspend it until cancellation so the GIF has exactly one frame.
                let seen = format!("{CONTROL}/{}screenshot-seen", device_prefix(id));
                if fs.stat(&seen).await.is_ok() {
                    fs.write(&device_barrier(id, &self.scenario.tests[0].method), vec![], 0o600)
                        .await?;
                    let mut action = ok("");
                    if let Action::Output { hang, .. } = &mut action {
                        *hang = true;
                    }
                    return Ok(Some(action));
                } else {
                    fs.write(&seen, vec![], 0o600).await?;
                }
                ok("")
            }
            // The separate device-log wrapper is parsed as a background job by
            // the router. Only this complete nested redirect is accepted here.
            [
                "xcrun",
                "simctl",
                "spawn",
                id,
                "log",
                "stream",
                "--type",
                "log",
                "--color",
                "none",
                "--style",
                "compact",
                "2>/dev/null",
                ">",
                path,
            ] => {
                ensure!(self.device(id)?.is_some(), "invalid log device");
                let mut action = ok("");
                if let Action::Output { hang, .. } = &mut action {
                    *hang = true;
                }
                Action::Publish {
                    output: Box::new(action),
                    files: vec![file(path, b"2000-01-01 00:00:00 AppleWorld scripted device log\n")],
                    started: vec![],
                    on_signal: Some("INT".into()),
                }
            }
            _ => {
                if known(&a) {
                    return Ok(Some(self.deferred(r)?));
                }
                return Ok(None);
            }
        };
        Ok(Some(out))
    }
    async fn shell(&self, command: &str, original: &Request, fs: &VirtualFs) -> Result<Option<Action>> {
        if command == "system_profiler SPHardwareDataType" {
            return Ok(Some(ok(&format!(
                "Provisioning UDID: {}\n",
                self.scenario
                    .devices
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("no provisioning device"))?
            ))));
        }
        for (key, value) in [("manufacturer", "Apple"), ("model", "MacAppleWorld")] {
            if command
                == format!(
                    "ioreg -ad2 -c IOPlatformExpertDevice | plutil -extract IORegistryEntryChildren.0.{key} raw -"
                )
            {
                return Ok(Some(ok(&format!("{value}\n"))));
            }
        }
        if command.starts_with("xcrun simctl spawn ") && command.ends_with(" & echo $!") {
            return Ok(None);
        }
        let nested = Request::parse(command.as_bytes())?;
        let a = nested.argv.iter().map(String::as_str).collect::<Vec<_>>();
        // Match the entire known pipeline, including surprising Kotlin quoting.
        if let ["nm", flag @ ("-gUj" | "-U"), path, ..] = a.as_slice() {
            let swift = format!(
                "nm -gUj \"{path}\" | xargs -s 131072 xcrun swift-demangle | cut -d\\  -f3 | grep -e \"[\\\\.|_]\"test"
            );
            let objc = format!(
                "nm -U \"{path}\" | grep \" t \" | cut -d\\  -f3,4 | cut -d \"-\" -f2 | cut -d \"[\" -f2 | cut -d \"]\" -f1 | grep \" test\""
            );
            ensure!(command == swift || command == objc, "unsupported nm pipeline");
            fs.stat(path).await?;
            let output = if *flag == "-gUj" {
                self.scenario
                    .tests
                    .iter()
                    .map(|t| format!("{}.{}.{}()\n", self.scenario.target, t.class, t.method))
                    .collect::<String>()
            } else {
                String::new()
            };
            return Ok(Some(ok(&output)));
        }
        if let [
            "/opt/homebrew/bin/ffmpeg",
            "-i",
            src,
            "-vf",
            "scale=852:852:force_original_aspect_ratio=decrease:force_divisible_by=2",
            "-preset",
            "ultrafast",
            "-movflags",
            "+faststart",
            dst,
        ] = a.as_slice()
        {
            ensure!(
                fs.read(src).await? == include_bytes!("../../../fixtures/fake-worker/apple/media/recording.mp4"),
                "recording not completed"
            );
            fs.write(
                dst,
                include_bytes!("../../../fixtures/fake-worker/apple/media/transcoded.mp4").to_vec(),
                0o600,
            )
            .await?;
            return Ok(Some(ok("")));
        }
        if known(&a) {
            return Ok(Some(self.deferred(original)?));
        }
        Ok(None)
    }
    async fn xcodebuild(&self, original: &Request, args: &[&str], fs: &VirtualFs) -> Result<Action> {
        ensure!(args.first() == Some(&"-xctestrun"), "xctestrun must be first");
        let path = *args.get(1).ok_or_else(|| anyhow::anyhow!("missing xctestrun"))?;
        let mut selected = vec![];
        let mut result = None;
        let mut destination = None;
        let mut i = 2;
        while i < args.len() {
            if let Some(test) = args[i].strip_prefix("-only-testing:") {
                selected.push(test.to_owned());
                i += 1;
                continue;
            }
            let value = || {
                args.get(i + 1)
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!("missing xcodebuild option value"))
            };
            match args[i] {
                "-enableCodeCoverage" => ensure!(matches!(value()?, "YES" | "NO"), "coverage"),
                "-resultBundlePath" => result = Some(value()?.to_owned()),
                "-destination-timeout" => {
                    value()?.parse::<u64>()?;
                }
                "-destination" => destination = Some(value()?.to_owned()),
                "-collect-test-diagnostics" => ensure!(value()? == "never", "diagnostics baseline"),
                _ => return self.deferred(original),
            }
            i += 2;
        }
        let plist_bytes = fs.read(path).await?;
        let value = plist::Value::from_reader(Cursor::new(&plist_bytes))?;
        let root = path
            .rsplit_once('/')
            .ok_or_else(|| anyhow::anyhow!("xctestrun parent"))?
            .0;
        let dict = value.as_dictionary().ok_or_else(|| anyhow::anyhow!("plist root"))?;
        let configs = dict
            .get("TestConfigurations")
            .and_then(plist::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("TestConfigurations"))?;
        let target = configs
            .first()
            .and_then(plist::Value::as_dictionary)
            .and_then(|d| d.get("TestTargets"))
            .and_then(plist::Value::as_array)
            .and_then(|a| a.first())
            .and_then(plist::Value::as_dictionary)
            .ok_or_else(|| anyhow::anyhow!("TestTargets"))?;
        let host = target
            .get("TestHostPath")
            .and_then(plist::Value::as_string)
            .ok_or_else(|| anyhow::anyhow!("TestHostPath"))?;
        validate_product(fs, host, root, None).await?;
        for key in ["TestBundlePath", "UITargetAppPath"] {
            if let Some(path) = target.get(key).and_then(plist::Value::as_string) {
                let path = validate_product(fs, path, root, Some(host)).await?;
                validate_bundle(fs, &path, self.scenario.platform == "macos").await?;
            }
        }
        if let Some(paths) = target.get("DependentProductPaths").and_then(plist::Value::as_array) {
            for path in paths {
                let path = path
                    .as_string()
                    .ok_or_else(|| anyhow::anyhow!("dependent product path"))?;
                validate_product(fs, path, root, Some(host)).await?;
            }
        }
        let application = target
            .get("UITargetAppPath")
            .and_then(plist::Value::as_string)
            .unwrap_or(host);
        let application = validate_product(fs, application, root, Some(host)).await?;
        let info_path = format!(
            "{application}/{}Info.plist",
            if self.scenario.platform == "macos" {
                "Contents/"
            } else {
                ""
            }
        );
        validate_bundle(fs, &application, self.scenario.platform == "macos").await?;
        let info = plist::Value::from_reader(Cursor::new(fs.read(&info_path).await?))?;
        let app_id = info
            .as_dictionary()
            .and_then(|d| d.get("CFBundleIdentifier"))
            .and_then(plist::Value::as_string)
            .ok_or_else(|| anyhow::anyhow!("application bundle ID"))?
            .to_owned();
        let env = target
            .get("TestingEnvironmentVariables")
            .and_then(plist::Value::as_dictionary);
        let discovery = env
            .and_then(|d| d.get("DYLD_INSERT_LIBRARIES"))
            .and_then(plist::Value::as_string)
            .is_some_and(|s| s.contains("libxctest-parser.dylib"));
        if discovery {
            let insert = env
                .and_then(|d| d.get("DYLD_INSERT_LIBRARIES"))
                .and_then(plist::Value::as_string)
                .ok_or_else(|| anyhow::anyhow!("parser env"))?;
            let parser = insert
                .split(':')
                .find(|p| p.ends_with("libxctest-parser.dylib"))
                .ok_or_else(|| anyhow::anyhow!("parser dylib"))?;
            ensure!(fs.stat(parser).await?.size > 0, "empty parser dylib");
        }
        let dest = destination.ok_or_else(|| anyhow::anyhow!("missing destination"))?;
        let prefix = format!(
            "platform={},id=",
            if self.scenario.platform == "ios" {
                "iOS Simulator"
            } else {
                "OS X"
            }
        );
        let id = dest
            .strip_prefix(&prefix)
            .ok_or_else(|| anyhow::anyhow!("wrong destination {dest}"))?
            .to_owned();
        ensure!(self.device(&id)?.is_some(), "wrong destination {dest}");
        let tests = self
            .scenario
            .tests
            .iter()
            .filter(|t| {
                selected.is_empty() || selected.contains(&format!("{}/{}/{}", self.scenario.target, t.class, t.method))
            })
            .collect::<Vec<_>>();
        ensure!(
            !tests.is_empty() && (selected.is_empty() || tests.len() == selected.len()),
            "unknown selected tests"
        );
        if let Some(result) = &result {
            fs.mkdir(result).await?;
            let failure=tests.iter().filter(|t|t.status=="failed").map(|t|json!({"testCaseName":format!("{}.{}()",t.class,t.method),"message":"scripted enriched failure","documentLocationInCreatingWorkspace":{"url":"file:///fixture/SmokeTests:1"}})).collect::<Vec<_>>();
            let metrics = json!({"testsCount":tests.len(),"testsFailedCount":failure.len(),"testsSkippedCount":tests.iter().filter(|t|t.status=="skipped").count()});
            let action_result = json!({"metrics":metrics,"issues":{},"coverage":{},"testsRef":{"id":"tests-0"}});
            let device = json!({"identifier":id,"platformRecord":{}});
            let root = json!({"metrics":metrics,"actions":[{"startedTime":"2000-01-01T00:00:00Z","endedTime":"2000-01-01T00:00:01Z","runDestination":{"targetDeviceRecord":device,"localComputerRecord":device,"targetSDKRecord":{}},"buildResult":action_result,"actionResult":action_result}],"issues":{"testFailureSummaries":failure}});
            let summaries = tests
                .iter()
                .map(|t| json!({"identifier":format!("{}/{}()",t.class,t.method),"name":t.method}))
                .collect::<Vec<_>>();
            let summary =
                json!({"summaries":[{"testableSummaries":[{"targetName":self.scenario.target,"tests":summaries}]}]});
            fs.write(
                &format!("{result}/root.json"),
                serde_json::to_vec(&apple_json(root))?,
                0o600,
            )
            .await?;
            fs.write(
                &format!("{result}/tests-0.json"),
                serde_json::to_vec(&apple_json(summary))?,
                0o600,
            )
            .await?;
            fs.write(
                &format!("{result}/SCRIPTED"),
                b"Scripted Apple world bundle; not readable by real xcresulttool.\n".to_vec(),
                0o600,
            )
            .await?;
        }
        let mut chunks = vec![];
        for test in tests {
            chunks.push(Chunk {
                data: format!(
                    "Test Case '-[{}.{} {}]' started.\n",
                    self.scenario.target, test.class, test.method
                ),
                ..Default::default()
            });
            chunks.push(Chunk {
                data: format!(
                    "Test Case '-[{}.{} {}]' {} (0.100 seconds).\n",
                    self.scenario.target, test.class, test.method, test.status
                ),
                wait_for_file: if !discovery && self.scenario.media != Media::None {
                    Some(device_barrier(&id, &test.method))
                } else {
                    None
                },
                ..Default::default()
            });
        }
        chunks.push(Chunk {
            data: "** TEST EXECUTE FINISHED **\n".into(),
            ..Default::default()
        });
        for chunk in &mut chunks {
            chunk.delay_ms = self.scenario.line_delay_ms;
        }
        fs.mkdir(CONTROL).await?;
        let index = {
            let mut state = self.state()?;
            let index = state.sessions.len();
            if self.scenario.platform == "ios" {
                let device = state
                    .devices
                    .get_mut(&id)
                    .ok_or_else(|| anyhow::anyhow!("missing destination device"))?;
                ensure!(device.state == "Booted", "batch requires booted simulator");
                if !device.installed.contains(&app_id) {
                    device.installed.push(app_id);
                }
            }
            state.sessions.push(Session {
                destination: dest,
                discovery,
                selected,
                result,
                xctestrun: path.into(),
                state: "active".into(),
                plist_base64: BASE64_STANDARD.encode(plist_bytes),
            });
            index
        };
        let mut action = ok("");
        if let Action::Output { chunks: c, status, .. } = &mut action {
            *c = chunks;
            *status = 65;
        }
        Ok(Action::Publish {
            output: Box::new(action),
            files: vec![file(&format!("{CONTROL}/session-{index}.finished"), b"")],
            started: vec![],
            on_signal: None,
        })
    }
}
struct Reservation {
    world: World,
    names: Vec<String>,
    committed: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.committed
            && let Ok(mut state) = self.world.state()
        {
            for name in &self.names {
                if let Some(n) = state.calls.get_mut(name) {
                    *n -= 1;
                }
            }
        }
    }
}
impl Handler for World {
    fn name(&self) -> &str {
        "apple-world"
    }
    fn route<'a>(&'a self, r: &'a Request, fs: &'a VirtualFs) -> HandlerFuture<'a> {
        Box::pin(async move {
            // D32: this prefix, on this one command, is no part of what the scenarios describe. The transcript keeps the
            // bytes as they came.
            let unprefixed = r
                .raw
                .strip_prefix(UNBUFFERED_PREFIX)
                .filter(|rest| rest.starts_with(b"xcrun xcodebuild test-without-building "))
                .map(Request::parse)
                .transpose()?;
            let r = unprefixed.as_ref().unwrap_or(r);
            // Counts commit only once routing completed. No state guard survives an await.
            let mut reservation = self.reserve(r)?;
            let action = self.answer(r, fs).await?;
            reservation.committed = action.is_some();
            Ok(action)
        })
    }
}
async fn validate_product(fs: &VirtualFs, value: &str, root: &str, host: Option<&str>) -> Result<String> {
    fs.resolve_xctestrun_path(value, root, host).await?;
    let resolved = value.replace("__TESTROOT__", root);
    let resolved = host.map_or(resolved.clone(), |h| {
        resolved.replace("__TESTHOST__", &h.replace("__TESTROOT__", root))
    });
    fs.stat(&resolved).await?;
    Ok(resolved)
}
async fn validate_bundle(fs: &VirtualFs, path: &str, macos: bool) -> Result<()> {
    let prefix = if macos { "Contents/" } else { "" };
    let info = plist::Value::from_reader(Cursor::new(fs.read(&format!("{path}/{prefix}Info.plist")).await?))?;
    let executable = info
        .as_dictionary()
        .and_then(|d| d.get("CFBundleExecutable"))
        .and_then(plist::Value::as_string)
        .ok_or_else(|| anyhow::anyhow!("bundle executable missing"))?;
    let binary = fs
        .read(&format!(
            "{path}/{prefix}{}{executable}",
            if macos { "MacOS/" } else { "" }
        ))
        .await?;
    ensure!(binary.len() > 4, "empty bundle executable");
    Ok(())
}
fn device_prefix(id: &str) -> String {
    // Retain the recorded baseline paths byte for byte.
    if id == UDID { String::new() } else { format!("{id}-") }
}
fn device_barrier(id: &str, method: &str) -> String {
    barrier(&format!("{}{method}", device_prefix(id)))
}
fn barrier(method: &str) -> String {
    format!("{CONTROL}/{method}.ready")
}
fn ok(s: &str) -> Action {
    Action::output(s, 0)
}
fn bytes(b: Vec<u8>) -> Action {
    let mut a = ok("");
    if let Action::Output { chunks, .. } = &mut a {
        chunks[0].base64 = Some(BASE64_STANDARD.encode(b));
    }
    a
}
fn error(s: &str, status: u32) -> Action {
    let mut a = Action::output(s, status);
    if let Action::Output { chunks, .. } = &mut a {
        chunks[0].extended = Some(1);
    }
    a
}
fn invalid(id: &str) -> Action {
    error(&format!("Invalid device: {id}\n"), 148)
}
fn file(path: &str, b: &[u8]) -> PublishedFile {
    PublishedFile {
        path: path.into(),
        base64: BASE64_STANDARD.encode(b),
    }
}
fn known(a: &[&str]) -> bool {
    matches!(
        a.first().copied(),
        Some(
            "xcrun"
                | "xcode-select"
                | "nm"
                | "lipo"
                | "/usr/libexec/PlistBuddy"
                | "sqlite3"
                | "chmod"
                | "codesign"
                | "csrutil"
                | "getconf"
                | "/usr/sbin/system_profiler"
                | "system_profiler"
                | "ioreg"
                | "sw_vers"
                | "uname"
                | "env"
                | "/opt/homebrew/bin/ffmpeg"
                | "pkill"
                | "/usr/bin/pkill"
                | "ps"
        )
    )
}
// AppleJsonMapper unwraps scalar _value and collection _values nodes; the
// scripted objects deliberately carry only fields consumed by these scenarios.
fn apple_json(v: Value) -> Value {
    canonical_json(match v {
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, apple_json(v))).collect()),
        Value::Array(v) => json!({"_values":v.into_iter().map(apple_json).collect::<Vec<_>>()}),
        v => json!({"_value":v}),
    })
}

fn canonical_json(mut value: Value) -> Value {
    // A workspace consumer can enable serde_json/preserve_order. Scripted
    // worker bytes must stay the same under Cargo feature unification.
    value.sort_all_objects();
    value
}

fn catalogue_deferral(command: &str) -> Result<bool> {
    #[derive(Deserialize)]
    struct Catalogue {
        forms: Vec<Form>,
    }
    #[derive(Deserialize)]
    struct Form {
        reachability: String,
        wire_pattern: String,
    }
    // The inventory is trusted source data, compiled into this test tool.
    static FORMS: std::sync::OnceLock<Vec<Form>> = std::sync::OnceLock::new();
    if FORMS.get().is_none() {
        let catalogue: Catalogue = serde_json::from_str(include_str!(
            "../../../fixtures/fake-worker/apple/command-catalogue.json"
        ))?;
        let _ = FORMS.set(catalogue.forms);
    }
    Ok(FORMS.get().is_some_and(|forms| {
        forms
            .iter()
            .any(|f| f.reachability == "deferred" && crate::script::glob(f.wire_pattern.as_bytes(), command.as_bytes()))
    }))
}

const UNBUFFERED_PREFIX: &[u8] = b"env NSUnbufferedIO=YES ";

fn default_devices() -> Vec<String> {
    vec![UDID.into()]
}
