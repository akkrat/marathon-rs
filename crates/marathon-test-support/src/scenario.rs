//! Port of core/src/test/kotlin/com/malinskiy/marathon/scenario/SuccessScenariosTest.kt
//! (the shared `setupMarathon { ... }; launch { runAsync() }; advanceTimeBy(...)` shape of
//! every scenario test), driven by the version-1 scenario inputs in tools/oracle/scenarios.
//!
//! The inputs are not goldens. Connect/disconnect events are delivered with
//! `ControlledReplay::event`: work due at an event's time quiesces before delivery and
//! after it (docs/design/replay-tapes.md). `end` is Kotlin's `advanceTimeBy(end)`, which
//! runs tasks strictly before `end`. A run still active then is cancelled and joined as
//! harness teardown, which is not an application finish (C26).
use crate::{
    console::Line,
    factory::{Built, CounterTimer, MarathonFactory},
    lock,
    stub_device::{Recorder, StubDevice, StubDeviceOptions},
};
use jcompat::writers::JavaString;
use marathon_config::schema::*;
use marathon_core::{
    error::*,
    executor::{CoreExecutor, shared::failure},
    model::*,
    runtime::*,
    time::ReplayDriver,
    vendor::DeviceEvent,
};
use serde_json::Value;
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, sync::Arc};

pub struct Outcome {
    /// `job.isCompleted` at the end of the scenario.
    pub completed: bool,
    pub value: Option<VendorResult<bool>>,
    pub lines: Vec<Line>,
    /// Record count of each batch the sink received (the run's flush points).
    pub batches: Vec<usize>,
    pub output_dir: PathBuf,
    pub expect: Value,
    pub provider_calls: Vec<&'static str>,
    /// Scheduler time at which `runAsync` returned.
    pub completed_at: Option<i64>,
    /// Events and device calls in the replay transcript's shape.
    pub log: Vec<Value>,
    _output: Option<tempfile::TempDir>,
}
impl Outcome {
    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.output_dir.join(relative))
            .unwrap_or_else(|e| panic!("{relative} in {}: {e}", self.output_dir.display()))
    }
}

pub fn scenarios_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/oracle/scenarios")
}

fn json(name: &str) -> Value {
    let path = scenarios_dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(value["format"], "marathon-replay/scenario@1", "{name}");
    value
}

fn s(value: &Value) -> &str {
    value.as_str().unwrap_or_else(|| panic!("expected a string: {value}"))
}

pub fn test(pkg: &str, clazz: &str, method: &str) -> Test {
    Test {
        pkg: pkg.into(),
        clazz: clazz.into(),
        method: method.into(),
        meta_properties: vec![],
    }
}

fn tests_of(spec: &Value) -> Vec<Test> {
    spec.as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let mut test = test(s(&t["pkg"]), s(&t["clazz"]), s(&t["method"]));
            for meta in t["metaProperties"].as_array().into_iter().flatten() {
                let mut values = MetaMap::default();
                for (key, value) in meta["values"].as_object().into_iter().flatten() {
                    values.insert(key.as_str().into(), MetaValue::String(s(value).into()));
                }
                test.meta_properties.push(MetaProperty {
                    name: s(&meta["name"]).into(),
                    values,
                });
            }
            test
        })
        .collect()
}

fn status(name: &str) -> TestStatus {
    match name {
        "PASSED" => TestStatus::Passed,
        "FAILURE" => TestStatus::Failure,
        "INCOMPLETE" => TestStatus::Incomplete,
        "IGNORED" => TestStatus::Ignored,
        "ASSUMPTION_FAILURE" => TestStatus::AssumptionFailure,
        other => panic!("unknown status {other}"),
    }
}

fn regexes(spec: &Value) -> Vec<jcompat::JavaRegex> {
    spec.as_array()
        .into_iter()
        .flatten()
        .map(|r| jcompat::JavaRegex::compile(s(r), "serialRegex").unwrap_or_else(|e| panic!("regex {r}: {e:?}")))
        .collect()
}

fn instant(spec: &Value) -> jcompat::jtime::Instant {
    jcompat::jtime::parse_instant(s(spec)).unwrap_or_else(|| panic!("instant {spec}"))
}

/// The configuration keys the scenario inputs use, as `configuration { ... }` assignments.
fn configure(c: &mut marathon_config::Configuration, spec: &Value) {
    let Some(spec) = spec.as_object() else { return };
    for (key, value) in spec {
        match key.as_str() {
            "includeSerialRegexes" => c.include_serial_regexes = regexes(value),
            "excludeSerialRegexes" => c.exclude_serial_regexes = regexes(value),
            "uncompletedTestRetryQuota" => c.uncompleted_test_retry_quota = value.as_i64().unwrap_or(0) as i32,
            "name" => c.name = s(value).into(),
            "retryStrategy" => {
                c.retry_strategy = RetryStrategyConfiguration::FixedQuota {
                    total_allowed_retry_quota: value["totalAllowedRetryQuota"].as_i64().unwrap_or(0) as i32,
                    retry_per_test_quota: value["retryPerTestQuota"].as_i64().unwrap_or(0) as i32,
                }
            }
            "poolingStrategy" => {
                assert_eq!(value["type"], "device-model");
                c.pooling_strategy = PoolingStrategyConfiguration::Model;
            }
            "shardingStrategy" => {
                c.sharding_strategy = ShardingStrategyConfiguration::Count {
                    count: value["count"].as_i64().unwrap_or(0) as i32,
                }
            }
            "sortingStrategy" => {
                assert_eq!(value["type"], "random-order");
                c.sorting_strategy = SortingStrategyConfiguration::RandomOrder;
            }
            "flakinessStrategy" => {
                c.flakiness_strategy = FlakinessStrategyConfiguration::Probability {
                    min_success_rate: value["minSuccessRate"].as_f64().unwrap_or(0.0),
                    max_count: value["maxCount"].as_i64().unwrap_or(0) as i32,
                    time_limit: instant(&value["timeLimit"]),
                }
            }
            other => panic!("unsupported scenario configuration key {other}"),
        }
    }
}

fn device(spec: &Value) -> Arc<StubDevice> {
    let mut options = StubDeviceOptions {
        serial_number: s(&spec["serial"]).into(),
        ..StubDeviceOptions::default()
    };
    if let Some(model) = spec["model"].as_str() {
        options.model = model.into();
    }
    if let Some(millis) = spec["prepareTimeMillis"].as_i64() {
        options.prepare_time_millis = millis;
    }
    options.scripted = spec["behaviour"] == "scripted";
    if let Some(millis) = spec["testTimeMillis"].as_i64() {
        options.test_time_millis = millis;
    }
    options.report_results_before_crash = spec["reportResultsBeforeCrash"].as_bool().unwrap_or(false);
    options.crash_with_device_lost_exception = spec["crashWithDeviceLostException"].as_bool().unwrap_or(false);
    options.crash_with_test_batch_exception = spec["crashWithTestBatchException"].as_bool().unwrap_or(false);
    let device = StubDevice::new(options);
    if let Some(results) = spec["results"].as_object() {
        let mut map = HashMap::new();
        for (name, statuses) in results {
            let (class, method) = name.split_once('#').unwrap_or_else(|| panic!("test name {name}"));
            let (pkg, clazz) = class.rsplit_once('.').unwrap_or(("", class));
            map.insert(
                test(pkg, clazz, method),
                statuses
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| status(s(v)))
                    .collect(),
            );
        }
        device.set_execution_results(map);
    }
    device
}

/// A scenario's command, device events and devices by serial.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// `launch { marathon.runAsync(command) }`.
    RunAsync,
    /// `marathon.run(command)`: the supervised wrapper and its catch.
    Run,
}
pub struct Script {
    pub entry: Entry,
    pub command: ExecutionCommand,
    /// (time, event type, serial, the event's input object for device events).
    pub events: Vec<(i64, String, Option<String>, Value)>,
    pub devices: HashMap<String, Arc<StubDevice>>,
    pub expect: Value,
}
/// Runs inside the executor before the graph is built (e.g. a timer on virtual time).
pub type Prepare = Box<dyn FnOnce(&Rc<CoreExecutor>, &mut MarathonFactory) + Send>;
/// Runs after the graph is built, before `runAsync` starts (provider behaviour).
pub type Ready = Box<dyn FnOnce(&Built) + Send>;

/// `launch { marathon.runAsync(command) }`, the events, then `advanceTimeBy(end)`.
pub fn drive(mut factory: MarathonFactory, script: Script, prepare: Option<Prepare>, ready: Option<Ready>) -> Outcome {
    let output_dir = std::path::PathBuf::from(String::from_utf16_lossy(
        &factory.configuration.configuration.output_dir.0,
    ));
    let output = factory.configuration.output.take();
    let Script {
        entry,
        command,
        events,
        devices,
        expect,
    } = script;
    let result = CoreExecutor::run_virtual(move |core, replay| {
        Box::pin(async move {
            if let Some(prepare) = prepare {
                prepare(&core, &mut factory);
            }
            let built = factory.build(&core).await.map_err(OperationError::Failed)?;
            if let Some(ready) = ready {
                ready(&built);
            }
            let recorder = Recorder {
                log: Arc::default(),
                now: core.clock().now_source(),
            };
            for device in devices.values() {
                device.record_to(recorder.clone());
            }
            let job = core.job(JobKind::Ordinary, None);
            let value: Rc<RefCell<Option<VendorResult<bool>>>> = Rc::new(RefCell::new(None));
            let completed_at: Rc<RefCell<Option<i64>>> = Rc::new(RefCell::new(None));
            let output = value.clone();
            let finished = completed_at.clone();
            let marathon = built.marathon.clone();
            let clock = core.clock();
            core.spawn(
                job.clone(),
                Box::new(move |task: TaskContext| {
                    Box::pin(async move {
                        let result = match entry {
                            Entry::RunAsync => marathon.run_async(&command, &task).await,
                            Entry::Run => {
                                marathon
                                    .run_under(command, Some(task.job.clone()))
                                    .await
                                    .map_err(|e| match e {
                                        OperationError::Cancelled(reason) => VendorError::Cancelled(reason),
                                        OperationError::Failed(failure) => VendorError::Other(failure),
                                    })
                            }
                        };
                        *finished.borrow_mut() = Some(marathon_core::time::SchedulerClock::now_millis(&*clock));
                        *output.borrow_mut() = Some(result);
                        Ok(())
                    })
                }),
            );
            let replay_error = |e: marathon_core::time::ReplayError| {
                OperationError::Failed(failure("ReplayError", &e.message.to_string_lossy()))
            };
            for (at, event, serial, spec) in events {
                let mut entry = serde_json::json!({"event": event});
                if let Some(serial) = &serial {
                    entry["device"] = serial.as_str().into();
                }
                if event == "end" {
                    replay.advance_to(at - 1).await.map_err(replay_error)?;
                    let mut end = serde_json::Map::new();
                    end.insert("t".into(), at.into());
                    end.insert("event".into(), "end".into());
                    lock(&recorder.log).push(Value::Object(end));
                    continue;
                }
                replay.advance_to(at).await.map_err(replay_error)?;
                recorder.record(entry);
                let stub = serial
                    .as_ref()
                    .and_then(|serial| devices.get(serial))
                    .unwrap_or_else(|| panic!("event {event} needs a known device"))
                    .clone();
                let provider = built.provider.clone();
                replay
                    .event(at, move || {
                        let device = stub.reference();
                        let event = match event.as_str() {
                            "connect" => DeviceEvent::Connected(device),
                            "disconnect" => DeviceEvent::Disconnected(device),
                            _ => {
                                return match stub.deliver(&spec) {
                                    None => Ok(()),
                                    Some(error) => Err(marathon_core::time::ReplayError {
                                        label: "scenario event".into(),
                                        message: error.as_str().into(),
                                    }),
                                };
                            }
                        };
                        assert!(provider.send(event), "the provider queue is closed");
                        Ok(())
                    })
                    .await
                    .map_err(replay_error)?;
            }
            let completed = value.borrow().is_some();
            if !completed {
                job.cancel(marathon_core::executor::shared::requested("scenario teardown"));
                let _ = job.join().await;
            }
            let value = value.take();
            Ok(Outcome {
                completed,
                value,
                lines: built.console.lines(),
                batches: built.console.batch_sizes(),
                output_dir,
                expect,
                provider_calls: lock(&built.provider.calls).clone(),
                completed_at: *completed_at.borrow(),
                log: lock(&recorder.log).clone(),
                _output: None,
            })
        })
    });
    match result {
        Ok(mut outcome) => {
            outcome._output = output;
            outcome
        }
        Err(e) => panic!("scenario failed to run: {e:?}"),
    }
}

/// Loads a scenario input into a factory and script.
pub fn load(name: &str) -> (MarathonFactory, Script) {
    let spec = json(name);
    let mut factory = MarathonFactory::new();
    // As the oracle driver does (`outputDir = File(work, "output")`): not created beforehand.
    let output = factory.configuration.output_dir().join("output");
    factory.configuration.configuration.output_dir =
        jcompat::file_manager::JavaPath::from_text(&output.to_string_lossy());
    configure(&mut factory.configuration.configuration, &spec["configuration"]);
    factory.tests = tests_of(&spec["tests"]);
    if spec["timer"] == "counter" {
        factory.timer = Some(Arc::new(CounterTimer::default()));
    }
    let devices: HashMap<String, Arc<StubDevice>> = spec["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| (s(&d["serial"]).to_owned(), device(d)))
        .collect();
    let command = match spec["command"]["type"].as_str() {
        Some("parse") => ExecutionCommand::Parse {
            output_file_name: spec["command"]["outputFileName"].as_str().map(JavaString::from),
            include_flaky_tests: spec["command"]["includeFlakyTests"].as_bool().unwrap_or(false),
        },
        _ => ExecutionCommand::Run,
    };
    let events = spec["events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            (
                e["at"].as_i64().unwrap_or(0),
                s(&e["event"]).to_owned(),
                e["device"].as_str().map(str::to_owned),
                e.clone(),
            )
        })
        .collect();
    let entry = match spec["entry"].as_str() {
        None | Some("runAsync") => Entry::RunAsync,
        Some("run") => Entry::Run,
        Some(other) => panic!("{name}: entry {other} is not runAsync or run"),
    };
    let script = Script {
        entry,
        command,
        events,
        devices,
        expect: spec["expect"].clone(),
    };
    (factory, script)
}

/// Loads, adjusts and runs one scenario input. `adjust` sees the factory before the run.
pub fn run(name: &str, adjust: impl FnOnce(&mut MarathonFactory)) -> Outcome {
    let (mut factory, script) = load(name);
    adjust(&mut factory);
    drive(factory, script, None, None)
}

/// As [`run`], with its end `millis` later. The files stay as the Kotlin capture has them. D48's emptied pools wait for
/// longer than some of the Kotlin tests advance the clock.
pub fn run_longer(name: &str, millis: i64, adjust: impl FnOnce(&mut MarathonFactory)) -> Outcome {
    let (mut factory, mut script) = load(name);
    adjust(&mut factory);
    for (at, event, ..) in &mut script.events {
        if event == "end" {
            *at += millis;
        }
    }
    drive(factory, script, None, None)
}
