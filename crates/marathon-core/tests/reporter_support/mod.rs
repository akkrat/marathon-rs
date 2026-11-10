//! Port of core/src/test/kotlin/com/malinskiy/marathon/report/JUnitReporterTest.kt.
#![allow(dead_code)]

use marathon_logging::LogSettings;

use jcompat::{file_manager::FileManager, jtime::instant_of};
use marathon_core::{
    artifacts::FileManagerOutput,
    error::*,
    executor::{
        CoreExecutor,
        hooks::{ClockMode, RuntimeHooks, Tape},
    },
    logging::OutputRecord,
    model::*,
    output::ConsoleSink,
    report::*,
    runtime::*,
    time::{ReplayHooks, SystemTimer},
};
use marathon_error::FailureKind;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
pub fn checked<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("fixture operation: {e:?}"),
    }
}
pub fn rows() -> Vec<Value> {
    include_str!("../../../../fixtures/oracle/reporters/probes.jsonl")
        .lines()
        .chain(include_str!("../../../../fixtures/oracle/reporters/review-round1.jsonl").lines())
        .map(|l| checked(serde_json::from_str(l)))
        .collect()
}
pub fn row(id: &str) -> Value {
    rows().into_iter().find(|r| r["id"] == id).unwrap()
}
pub fn bytes(v: &Value) -> Vec<u8> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap() as u8)
        .collect()
}
pub fn text(v: &Value) -> String {
    v.as_str().unwrap().into()
}
pub fn device(v: &Value) -> DeviceInfo {
    DeviceInfo {
        operating_system: OperatingSystem {
            version: text(&v["operatingSystem"]["version"]),
        },
        serial_number: text(&v["serialNumber"]),
        model: text(&v["model"]),
        manufacturer: text(&v["manufacturer"]),
        network_state: if v["networkState"] == "CONNECTED" {
            NetworkState::Connected
        } else {
            NetworkState::Disconnected
        },
        device_features: v["deviceFeatures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                if v == "VIDEO" {
                    DeviceFeature::Video
                } else {
                    DeviceFeature::Screenshot
                }
            })
            .collect(),
        healthy: v["healthy"].as_bool().unwrap(),
    }
}
pub fn pool(v: &Value) -> DevicePoolId {
    DevicePoolId { name: text(v) }
}
pub fn metadata_test() -> Test {
    use MetaValue::*;
    let nested = || {
        Arc::new(MetaArray {
            jvm_class: "[Lcom.linkedin.dex.parser.DecodedValue;".into(),
            elements: vec![
                Decoded(DecodedWrapper::DecodedInt(7)),
                Decoded(DecodedWrapper::DecodedNull),
                Decoded(DecodedWrapper::DecodedArrayValue(Arc::from([
                    DecodedWrapper::DecodedString("nested".into()),
                ]))),
            ],
        })
    };
    let arr = nested();
    let equal = nested();
    let entries = vec![
        ("null", Null),
        ("empty", String("".into())),
        ("html", String("<>&='\u{2028}\u{2029}😀".into())),
        ("boolean", Boolean(true)),
        ("byte", Byte(1)),
        ("short", Short(2)),
        ("int", Int(3)),
        ("long", Long(4)),
        ("char", Char('é' as u16)),
        ("float", Float(-0.0)),
        ("double", Double(-0.0)),
        ("tinyFloat", Float(f32::from_bits(1))),
        ("largeDouble", Double(f64::MAX)),
        ("list", List(vec![Null, Int(1), String("x".into())])),
        (
            "map",
            Map([("null".into(), Null), ("x".into(), Int(7))].into_iter().collect()),
        ),
        ("array", Array(arr.clone())),
        ("alias", Array(arr)),
        ("equal", Array(equal)),
        ("literal", String("[I@123abc".into())),
        (
            "decoded",
            Decoded(DecodedWrapper::DecodedArrayValue(Arc::from([
                DecodedWrapper::DecodedNull,
                DecodedWrapper::DecodedDouble(1.5),
            ]))),
        ),
        (
            "bytes",
            Array(Arc::new(MetaArray {
                jvm_class: "[B".into(),
                elements: vec![Byte(1), Byte(2), Byte(-1)],
            })),
        ),
        (
            "chars",
            Array(Arc::new(MetaArray {
                jvm_class: "[C".into(),
                elements: vec![Char(97), Char('é' as u16)],
            })),
        ),
        (
            "wrappers",
            List(vec![
                Decoded(DecodedWrapper::DecodedByte(-128)),
                Decoded(DecodedWrapper::DecodedShort(-32768)),
                Decoded(DecodedWrapper::DecodedLong(i64::MIN)),
                Decoded(DecodedWrapper::DecodedFloat(1.5)),
                Decoded(DecodedWrapper::DecodedChar('é' as u16)),
                Decoded(DecodedWrapper::DecodedBoolean(true)),
                Decoded(DecodedWrapper::DecodedType("Lpkg/Type;".into())),
                Decoded(DecodedWrapper::DecodedEnum("ENUM".into())),
            ]),
        ),
    ];
    Test {
        pkg: "pkg".into(),
        clazz: "Class".into(),
        method: "meta".into(),
        meta_properties: vec![MetaProperty {
            name: "annotation".into(),
            values: entries.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        }],
    }
}
pub fn test(v: &Value) -> Test {
    if v["method"] == "meta" {
        return metadata_test();
    }
    Test {
        pkg: text(&v["pkg"]),
        clazz: text(&v["clazz"]),
        method: text(&v["method"]),
        meta_properties: vec![],
    }
}
pub fn result(v: &Value) -> TestResult {
    TestResult {
        test: test(&v["test"]),
        device: device(&v["device"]),
        test_batch_id: text(&v["testBatchId"]),
        status: match v["status"].as_str().unwrap() {
            "PASSED" => TestStatus::Passed,
            "FAILURE" => TestStatus::Failure,
            "INCOMPLETE" => TestStatus::Incomplete,
            "IGNORED" => TestStatus::Ignored,
            _ => TestStatus::AssumptionFailure,
        },
        start_time: v["startTime"].as_i64().unwrap(),
        end_time: v["endTime"].as_i64().unwrap(),
        stacktrace: v.get("stacktrace").map(text),
        attachments: v["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| Attachment {
                file: PathBuf::from(text(&v["file"])),
                kind: if v["type"] == "LOG" {
                    AttachmentType::Log
                } else {
                    AttachmentType::ScreenshotPng
                },
                name: v.get("name").map(text),
            })
            .collect(),
    }
}
pub fn report(v: &Value) -> ExecutionReport {
    let instant = |v: &Value, prefix: &str| {
        instant_of(
            v[format!("{prefix}Seconds")].as_i64().unwrap(),
            v[format!("{prefix}Nanos")].as_i64().unwrap(),
        )
        .unwrap()
    };
    ExecutionReport::new(
        v["connected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| DeviceConnectedEvent {
                instant: instant_of(0, 0).unwrap(),
                pool_id: pool(&e["pool"]),
                device: device(&e["device"]),
            })
            .collect(),
        v["disconnected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| DeviceDisconnectedEvent {
                instant: instant_of(0, 0).unwrap(),
                pool_id: pool(&e["pool"]),
                device: device(&e["device"]),
            })
            .collect(),
        v["preparing"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| DevicePreparingEvent {
                start: instant(e, "start"),
                finish: instant(e, "finish"),
                serial_number: text(&e["serial"]),
            })
            .collect(),
        v["provider"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| DeviceProviderPreparingEvent {
                start: instant(e, "start"),
                finish: instant(e, "finish"),
                serial_number: text(&e["serial"]),
            })
            .collect(),
        v["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| TestEvent {
                instant: instant(e, "instant"),
                pool_id: pool(&e["pool"]),
                device: device(&e["device"]),
                test_result: Arc::new({
                    let mut r = result(&e["result"]);
                    if e["nonfinite"] == true {
                        r.test.meta_properties = vec![MetaProperty {
                            name: "ann".into(),
                            values: [("nan".into(), MetaValue::Double(f64::NAN))].into_iter().collect(),
                        }];
                    }
                    r
                }),
                final_result: e["final"].as_bool().unwrap(),
            })
            .collect(),
    )
}
static NEXT: AtomicUsize = AtomicUsize::new(0);
pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "mrs-report-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        checked(std::fs::create_dir_all(&p));
        Self(p)
    }
    pub fn path(&self) -> PathBuf {
        PathBuf::from(self.0.to_str().unwrap())
    }
}
impl Default for Temp {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
pub struct Sink(pub Mutex<Vec<u8>>);
impl ConsoleSink for Sink {
    fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            assert_ne!(std::thread::current().name(), Some("marathon-core"));
            for r in records {
                let OutputRecord::Raw(t) = r else {
                    panic!("unexpected log")
                };
                self.0.lock().unwrap().extend(t.as_bytes().to_vec());
            }
            Ok(())
        })
    }
}
pub fn context() -> Value {
    checked(serde_json::from_str(include_str!(
        "../../../../fixtures/oracle/reporters/context.json"
    )))
}
pub fn files(io: Arc<dyn VendorRuntime>, path: PathBuf, max_filename: i32) -> ReporterFiles {
    ReporterFiles {
        files: Arc::new(FileManagerOutput::new(
            io.clone(),
            FileManager::new(0, max_filename, &path.to_string_lossy(), ""),
            path,
            LogSettings::default().component("FileManager"),
            Arc::new(|_, _, _, _| assert_ne!(std::thread::current().name(), Some("marathon-core"))),
        )),
        io,
        cwd: PathBuf::from(text(&context()["cwd"])),
    }
}
pub fn generate(id: &str) -> (Temp, Vec<u8>, Vec<String>) {
    let r = row(id);
    let report = report(&r["input"]);
    let temp = Temp::new();
    let path = temp.path();
    let sink = Arc::new(Sink::default());
    let captured = sink.clone();
    let warnings = Arc::new(Mutex::new(vec![]));
    let captured_warnings = warnings.clone();
    let id = id.to_owned();
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let mut tape = Tape {
                wall: ClockMode::Frozen(0),
                ..Tape::default()
            };
            tape.reads
                .insert("core.BillingReporter.defaultStart".into(), vec![1000]);
            let elapsed = match id.as_str() {
                "sort" => 90061999,
                "duration-negative" => -1,
                "duration-zero" => 0,
                "duration-positive" => 1001,
                _ => 999,
            };
            tape.reads.insert(
                "core.di.Clock".into(),
                if matches!(id.as_str(), "empty" | "fail-json") {
                    vec![0]
                } else {
                    vec![0, elapsed]
                },
            );
            let hooks = RuntimeHooks::taped(tape, &core.clock());
            let timer = Arc::new(SystemTimer::new(hooks.clone()).map_err(|e| {
                marathon_core::executor::shared::failure(FailureKind::ReplayClock, &e.message.to_owned())
            })?);
            let io = crate::event_support::CountingRuntime::new(core.vendor());
            let output = files(io.clone(), path.clone(), if id == "paths" { 120 } else { 0 });
            let bill = BillingReporter::new(
                output.clone(),
                hooks.as_ref(),
                LogSettings::default().component("marathon_core::construction"),
                Arc::new(move |_, level, message, _| {
                    assert_ne!(std::thread::current().name(), Some("marathon-core"));
                    assert_eq!(level, marathon_logging::Level::WARN);
                    captured_warnings.lock().unwrap().push(message.to_owned())
                }),
            )?;
            let registrations = reporter_registrations(
                DeviceInfoJsonReporter { output: output.clone() },
                bill,
                JUnitReporter {
                    io: io.clone(),
                    output_dir: path,
                    cwd: output.cwd.clone(),
                },
                RawJsonReporter { output: output.clone() },
                TestJsonReporter { output },
                StdoutReporter {
                    io: io.clone(),
                    timer,
                    sink: captured,
                },
            );
            let task = core.vendor().spawn(Box::pin(async move {
                for registration in registrations {
                    registration.reporter.generate(&report).await.map_err(|e| match e {
                        OperationError::Failed(f) => VendorError::Other(f),
                        OperationError::Cancelled(c) => VendorError::Cancelled(c),
                    })?;
                }
                Ok(())
            }));
            let outcome = task.join().await;
            if id == "fail-json" {
                let error = match outcome {
                    Err(e) => e,
                    Ok(()) => panic!("JSON writer accepted nonfinite metadata"),
                };
                let expected = row("fail-json");
                assert_eq!(
                    error.failure().kind,
                    marathon_test_support::recorded_errors::kind(&text(&expected["error"]["class"]))
                );
                assert_eq!(error.failure().message, Some(text(&expected["error"]["message"])));
            } else {
                outcome.map_err(|e| OperationError::Failed(e.failure().clone()))?;
            }
            assert_eq!(
                io.spawns.load(Ordering::SeqCst),
                usize::from(!matches!(id.as_str(), "empty" | "fail-json")),
                "only stdout dispatches its async sink to the I/O runtime"
            );
            assert!(io.blocking.load(Ordering::SeqCst) > 0);
            hooks.verify_consumed().map_err(|e| {
                marathon_core::executor::shared::failure(FailureKind::ReplayClock, &e.message.to_owned())
            })?;
            Ok(())
        })
    }));
    let output = sink.0.lock().unwrap().clone();
    let logs = warnings.lock().unwrap().clone();
    (temp, output, logs)
}
pub fn manifest(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in checked(std::fs::read_dir(dir)) {
            let p = checked(entry).path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_str().unwrap().to_owned(),
                    checked(std::fs::read(&p)),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}
/// Remove only D3's named field, preserving every other byte of the JVM output.
pub fn without_delegate(mut data: Vec<u8>) -> Vec<u8> {
    let key = b",\"safeSerialNumber$delegate\":";
    while let Some(start) = data.windows(key.len()).position(|s| s == key) {
        let mut end = start + key.len();
        let mut depth = 0;
        let mut string = false;
        let mut escape = false;
        loop {
            let c = data[end];
            end += 1;
            if string {
                if escape {
                    escape = false
                } else if c == b'\\' {
                    escape = true
                } else if c == b'"' {
                    string = false
                }
            } else {
                match c {
                    b'"' => string = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        data.drain(start..end);
    }
    data
}
pub fn compare_scenario(id: &str) {
    let expected = row(id);
    let (temp, stdout, warnings) = generate(id);
    let actual = manifest(&temp.0);
    let files = expected["files"].as_object().unwrap();
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        files.keys().collect::<Vec<_>>(),
        "manifest {id}"
    );
    for (name, expected) in files {
        let expected = without_delegate(bytes(expected));
        if name.ends_with(".json") {
            let actual: Value = checked(serde_json::from_slice(&actual[name]));
            let mut expected: Value = checked(serde_json::from_slice(&expected));
            if id == "metadata" && name == "test_result/raw.json" {
                // D58 changes only these historical identity values.
                let values = &mut expected[0]["metaProperties"][0]["values"];
                for key in ["array", "alias", "equal"] {
                    values[key] =
                        "[DecodedInt(value=7), null, DecodedArrayValue(values=[DecodedString(value=nested)])]".into();
                }
                values["decoded"] = "DecodedArrayValue(values=[null, DecodedDouble(value=1.5)])".into();
                values["bytes"] = "[1, 2, -1]".into();
                values["chars"] = "[a, é]".into();
            }
            assert_eq!(
                checked(serde_json::to_string(&actual)),
                checked(serde_json::to_string(&expected)),
                "{id}: {name}"
            );
        } else {
            assert_eq!(actual[name], expected, "byte boundary {id}: {name}");
        }
    }
    fn directories(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in checked(std::fs::read_dir(dir)) {
            let p = checked(entry).path();
            if p.is_dir() {
                out.push(p.strip_prefix(root).unwrap().to_str().unwrap().to_owned());
                directories(root, &p, out);
            }
        }
    }
    let mut actual_dirs = vec![];
    directories(&temp.0, &temp.0, &mut actual_dirs);
    actual_dirs.sort();
    let expected_dirs: Vec<_> = expected["directories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(actual_dirs, expected_dirs, "directories {id}");
    let mut expected_stdout = bytes(&expected["stdout"]);
    if id == "sort" {
        // D55's native string order reverses this supplementary/BMP pair.
        let mut text = String::from_utf8(expected_stdout).unwrap();
        for suffix in ["", " failed 2 time(s)"] {
            text = text.replace(
                &format!("\t\tcom.example#😀{suffix}\n\t\tcom.example#\u{e000}{suffix}\n"),
                &format!("\t\tcom.example#\u{e000}{suffix}\n\t\tcom.example#😀{suffix}\n"),
            );
        }
        expected_stdout = text.into_bytes();
    }
    assert_eq!(stdout, expected_stdout, "stdout {id}");
    let expected_warnings: Vec<_> = expected["warnings"].as_array().unwrap().iter().map(text).collect();
    assert_eq!(warnings, expected_warnings);
}
