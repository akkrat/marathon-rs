//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/ (JVM reporter probes).
#![allow(dead_code)]
use jcompat::{
    file_manager::{FileManager, JavaPath},
    jtime::instant_of,
    logging::MarathonLogging,
    writers::{GsonWriter, WriterValue},
};
use marathon_core::{artifacts::FileManagerOutput, model::*, report::*, runtime::VendorRuntime};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
pub fn checked<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("fixture operation failed: {e:?}"),
    }
}
static NEXT: AtomicUsize = AtomicUsize::new(0);
pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "mrs39c-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        checked(std::fs::create_dir_all(&p));
        Self(p)
    }
    pub fn path(&self, name: &str) -> JavaPath {
        JavaPath::from_text(self.0.join(name).to_str().unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn output(io: Arc<dyn VendorRuntime>, path: JavaPath) -> Arc<FileManagerOutput> {
    Arc::new(FileManagerOutput::new(
        io,
        FileManager::new_utf16(4096, 255, &path.0, &[]),
        path,
        MarathonLogging::default().logger("FileManager", None),
        Arc::new(|_, _, _, _| assert_ne!(std::thread::current().name(), Some("marathon-core"))),
    ))
}
pub fn device() -> DeviceInfo {
    DeviceInfo {
        operating_system: OperatingSystem { version: "23".into() },
        serial_number: "serial #1".into(),
        model: "model<&>".into(),
        manufacturer: "unknown".into(),
        network_state: NetworkState::Connected,
        device_features: vec![],
        healthy: true,
    }
}
pub fn test(name: &str) -> Test {
    Test {
        pkg: "pkg".into(),
        clazz: "Clazz".into(),
        method: name.into(),
        meta_properties: vec![],
    }
}
pub fn result(name: &str, status: TestStatus, start: i64, end: i64) -> TestResult {
    TestResult {
        test: test(name),
        device: device(),
        test_batch_id: "batch".into(),
        status,
        start_time: start,
        end_time: end,
        stacktrace: None,
        attachments: vec![],
    }
}
pub fn epoch(ms: i64) -> jcompat::jtime::Instant {
    instant_of(0, ms * 1_000_000).unwrap()
}
pub fn event(r: TestResult, final_result: bool, pool: &str) -> TestEvent {
    TestEvent {
        instant: epoch(0),
        pool_id: DevicePoolId { name: pool.into() },
        device: r.device.clone(),
        test_result: Arc::new(r),
        final_result,
    }
}
pub fn trace() -> String {
    let mut s = String::from(
        "06 V verbose\r\nD debug\nI info\nW warning\nE <&>\nA assert\nα default\n\0\u{1}\u{b}\u{c}\u{e}\u{7f}\u{85}\u{9f}\u{fffe}\u{ffff}",
    );
    s.push('\u{fffd}');
    s.push('\n');
    s
}
pub fn mixed() -> ExecutionReport {
    let mut events: Vec<_> = [
        TestStatus::Failure,
        TestStatus::Passed,
        TestStatus::Ignored,
        TestStatus::Incomplete,
        TestStatus::AssumptionFailure,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, s)| {
        let mut r = result(&format!("test{i}"), s, i as i64, 20 - i as i64);
        r.stacktrace = match i {
            0 => Some(trace()),
            1 => Some("".into()),
            _ => None,
        };
        event(r, true, "pool")
    })
    .collect();
    events.push(event(result("test1", TestStatus::Failure, 1, 50), false, "pool"));
    events.push(event(result("negative", TestStatus::Passed, 40, 3), true, "pool two"));
    let mut other = result("test1", TestStatus::Passed, 20, 30);
    other.test.pkg = "other".into();
    events.push(event(other, true, "pool two"));
    let connected = ["pool", "pool two"]
        .into_iter()
        .map(|p| DeviceConnectedEvent {
            instant: epoch(0),
            pool_id: DevicePoolId { name: p.into() },
            device: device(),
        })
        .collect();
    let mut gone = device();
    gone.serial_number = "disconnect-only".into();
    ExecutionReport::new(
        connected,
        vec![DeviceDisconnectedEvent {
            instant: epoch(0),
            pool_id: DevicePoolId { name: "gone".into() },
            device: gone,
        }],
        vec![
            DevicePreparingEvent {
                start: epoch(10),
                finish: epoch(30),
                serial_number: "prepare-only".into(),
            },
            DevicePreparingEvent {
                start: epoch(1),
                finish: epoch(20),
                serial_number: "serial #1".into(),
            },
        ],
        vec![DeviceProviderPreparingEvent {
            start: epoch(0),
            finish: epoch(10),
            serial_number: "provider-only".into(),
        }],
        events,
    )
}
pub fn empty() -> ExecutionReport {
    ExecutionReport::new(vec![], vec![], vec![], vec![], vec![])
}
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/oracle/html-allure-gif")
        .join(name)
}
pub fn json(v: &WriterValue) -> Vec<u8> {
    checked(GsonWriter::default().write(v)).as_bytes().to_vec()
}
pub fn oracle(id: &str) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = checked(serde_json::from_str(include_str!(
        "../../../../fixtures/oracle/html-allure-gif/probe.json"
    )));
    rows.into_iter().find(|r| r["id"] == id).unwrap()
}
pub fn config(root: &JavaPath) -> marathon_config::Configuration {
    use marathon_config::{binding::BindingContext, binding::Profile, loader::MarathonConfigurationLoader};
    let ctx = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: JavaPath::from_text("/tmp/Marathonfile"),
        analytics_tracking: false,
        bugsnag_reporting: false,
        marathonfile_dir: root.clone(),
        process_dir: root.clone(),
        environment: vec![],
        now: epoch(0),
    };
    let mut cfg = checked(MarathonConfigurationLoader.parse_sync(
        "name: 'Report <&> 名'\noutputDir: output\nvendorConfiguration:\n  type: stub\n".as_bytes(),
        &ctx,
    ));
    cfg.output_dir = root.clone();
    cfg
}
/// Records raw console writes, as the CLI's stdout adapter would print them.
#[derive(Default)]
pub struct Console(pub std::sync::Mutex<Vec<String>>);
impl marathon_core::output::ConsoleSink for Console {
    fn write(
        &self,
        records: Vec<marathon_core::logging::OutputRecord>,
    ) -> marathon_core::runtime::VendorFuture<'_, ()> {
        Box::pin(async move {
            for record in records {
                if let marathon_core::logging::OutputRecord::Raw(text) = record {
                    self.0.lock().unwrap_or_else(|e| e.into_inner()).push(text)
                }
            }
            Ok(())
        })
    }
}
/// The JVM's stdout lines after each `probe: <id>` marker, up to the next marker.
pub fn jvm_stdout_after(marker: &str) -> Vec<String> {
    let text = checked(std::fs::read_to_string(fixture("stdout.txt")));
    let start = format!("probe: {marker}");
    text.lines()
        .skip_while(|l| *l != start)
        .skip(1)
        .take_while(|l| !l.starts_with("probe: "))
        .map(str::to_owned)
        .collect()
}
