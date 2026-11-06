//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamDeviceProvider.kt
//! (a device that disappears during prepare and returns with the same serial; the Kotlin CLI's outcome on
//! the same strict fake-adb scripts is `fixtures/fake-adb/android-reconnect/expected/*-kotlin.json`).
//!
//! The real provider, scheduler, pool, device actor, installer and runner run in one `run_virtual` executor
//! against in-process fake-adb servers. Marathon's clock is virtual: the driver steps it and lets the loopback
//! sockets settle in real time after every step, because a socket answer is not tracked work. The fake's own
//! timeline is switched off; this driver applies the script's `events` at their (virtual) times, but never before
//! the run has reached the state the event needs (devices set up, the dropped one hung in prepare, earlier
//! provider events received by the scheduler). A test
//! takes four virtual seconds on a device (`VirtualDuration`), where the fake in the CLI runs wait four real ones.
mod support;
use jcompat::{file_manager::JavaPath, logging::MarathonLogging};
use marathon_adb::server::{FakeAdbServer, Script, ServerOptions};
use marathon_android::{
    device::{AndroidBatchExecutor, AndroidDevice},
    runner::AndroidRunnerExecutor,
    services::create_services_with_executor,
};
use marathon_config::{
    binding::{BindingContext, Profile},
    loader::MarathonConfigurationLoader,
};
use marathon_core::{
    analytics::{BackendFactory, MetricsSetup},
    construction::{MetricsBackends, RunInputs, construct},
    error::*,
    executor::{CoreExecutor, hooks::RuntimeHooks},
    logging::host_millis,
    model::*,
    runtime::*,
    time::ReplayDriver,
    vendor::{DeviceEvent, DeviceProvider, DeviceRef, VendorContext, VendorServices},
};
use marathon_test_support::{
    console::{Line, RecordingConsole},
    factory::NoMetricsSetup,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use support::{checked, fixture};

/// How long a test runs on a device, in virtual time (the scripts' `delay_ms` for real-time runs).
const TEST_MILLIS: i64 = 4000;
/// Virtual time advanced between two real-time settle pauses.
const STEP_MILLIS: i64 = 50;
/// Real time given to the loopback sockets after each virtual step.
const SETTLE: Duration = Duration::from_millis(4);
const LIMIT_MILLIS: i64 = 400_000;
/// A scripted event may wait this long (virtual) for the state it needs before the scenario fails.
const MAX_LATE_MILLIS: i64 = 3000;
const HELD_IN_KOTLIN: &str = "rm ";
const HELD_NOW: &str = "logcat -c";
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
/// How long (real time) a due event may hold the virtual clock while its state is not reached.
const HOLD: Duration = Duration::from_secs(20);

/// A batch that ran: the device's server port and adb serial, and the virtual time it started.
type Started = Arc<Mutex<Vec<(u16, String, i64)>>>;
struct VirtualDuration {
    inner: AndroidRunnerExecutor,
    clock: Arc<dyn VendorClock>,
    now: Arc<AtomicI64>,
    started: Started,
}
impl AndroidBatchExecutor for VirtualDuration {
    fn execute<'a>(
        &'a self,
        device: &'a AndroidDevice,
        configuration: Arc<marathon_config::Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            // The CLIs' fake sees the `am instrument` request when the batch starts and answers four seconds
            // later, so a batch cut short by a disconnect still counts and is placed by its start.
            let start = self.now.load(Ordering::SeqCst);
            let serial = device.serial_number().to_string_lossy();
            let port = serial
                .split(':')
                .nth(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or_default();
            checked_lock(&self.started).push((port, device.adb_serial().to_owned(), start));
            self.clock.delay(TEST_MILLIS, context.cancellation.clone()).await?;
            self.inner
                .execute(device, configuration, pool, batch, result, context)
                .await
        })
    }
}

fn checked_lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone)]
pub struct Event {
    pub server: usize,
    pub at_ms: i64,
    pub connect: bool,
    pub serial: String,
    /// D22: instead of a device event, the whole adb server goes away, returning after this real time if any.
    pub kill: Option<Option<Duration>>,
}

/// The script with the fake's real-time behaviour removed: no timeline and no `delay_ms` on test runs.
fn load(name: &str) -> (Value, Vec<Event>, usize) {
    let path = fixture(&format!("fake-adb/android-reconnect/script/{name}.json"));
    let mut script: Value = checked(serde_json::from_str(&checked(std::fs::read_to_string(path))));
    let events = std::mem::replace(&mut script["events"], json!([]));
    // D27: the scripts hang a device in prepare's first `rm`, the `rm -r /sdcard` that the JVM runs and this port no longer does. The
    // scripts stay as the Kotlin runs used them; here the hold moves to the next step of prepare.
    for device in script["devices"].as_array_mut().into_iter().flatten() {
        let rules = device.get_mut("shell").and_then(Value::as_array_mut);
        for rule in rules.into_iter().flatten() {
            if rule["prefix"] == HELD_IN_KOTLIN {
                rule["prefix"] = json!(HELD_NOW);
            }
        }
    }
    for rule in script["shell"].as_array_mut().into_iter().flatten() {
        if rule["delay_ms"].as_i64().is_some_and(|d| d == TEST_MILLIS) {
            rule.as_object_mut().map(|r| r.remove("delay_ms"));
        }
    }
    let events = events
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| Event {
            server: 0,
            at_ms: e["at_ms"].as_i64().unwrap_or(0),
            connect: e["connect"].is_string(),
            serial: e["connect"]
                .as_str()
                .or(e["disconnect"].as_str())
                .unwrap_or_default()
                .to_owned(),
            kill: None,
        })
        .collect();
    (script, events, 0)
}

fn marathonfile(ports: &[u16], output: &Path) -> Vec<u8> {
    let apk = |name: &str| {
        fixture(&format!("vendor-android/apk/{name}"))
            .to_string_lossy()
            .into_owned()
    };
    json!({
        "name": "x1-reconnect",
        "outputDir": output.to_string_lossy(),
        "analyticsTracking": false,
        "bugsnagReporting": false,
        "debug": true,
        "vendorConfiguration": {
            "type": "Android",
            "androidSdk": "/nonexistent",
            "applicationApk": apk("app-debug.apk"),
            "testApplicationApk": apk("app-debug-androidTest.apk"),
            "autoGrantPermission": false,
            "mockLocation": false,
            "applicationPmClear": false,
            "testApplicationPmClear": false,
            "allureConfiguration": {"enabled": false},
            "screenRecordConfiguration": {
                "videoConfiguration": {"enabled": false},
                "screenshotConfiguration": {"enabled": false}
            },
            "testParserConfiguration": {"type": "remote"},
            "adbServers": ports.iter().map(|p| json!({"host": "127.0.0.1", "port": p})).collect::<Vec<_>>()
        }
    })
    .to_string()
    .into_bytes()
}

pub struct Observed {
    pub result: Option<VendorResult<bool>>,
    pub lines: Vec<Line>,
    pub transcripts: Vec<Vec<Value>>,
    pub virtual_millis: i64,
    /// Batches that reached the device: (server index, adb serial, virtual start).
    pub batches: Vec<(usize, String, i64)>,
    pub events: Vec<Event>,
    /// The provider's events as the scheduler received them.
    pub received: Vec<(bool, String, DeviceRef, bool)>,
    /// Fake-adb warnings per server (a strict fake records one for any unmatched command or broken session).
    pub warnings: Vec<usize>,
}

/// Requests of one serial in a server's transcript whose service contains `needle`.
fn requests(server: &FakeAdbServer, serial: &str, needle: &str) -> usize {
    server
        .transcript()
        .iter()
        .filter(|r| {
            r["type"] == "request" && r["serial"] == serial && r["service"].as_str().is_some_and(|s| s.contains(needle))
        })
        .count()
}

/// What the scheduler received from the provider, in order: (connected, serial text, device).
type Tapped = Arc<Mutex<Vec<(bool, String, DeviceRef, bool)>>>;

/// The real provider with its event channel observed where the scheduler consumes it.
struct TappedProvider {
    inner: Arc<dyn DeviceProvider>,
    tap: Tapped,
}
impl DeviceProvider for TappedProvider {
    fn initialize(&self, context: VendorContext) -> VendorFuture<'_, ()> {
        self.inner.initialize(context)
    }
    fn borrow(&self, context: VendorContext) -> VendorFuture<'_, DeviceRef> {
        self.inner.borrow(context)
    }
    fn terminate(&self) -> VendorFuture<'_, ()> {
        self.inner.terminate()
    }
    fn subscribe(&self) -> Arc<dyn EventChannel<DeviceEvent>> {
        Arc::new(TappedChannel {
            inner: self.inner.subscribe(),
            tap: self.tap.clone(),
        })
    }
}
struct TappedChannel {
    inner: Arc<dyn EventChannel<DeviceEvent>>,
    tap: Tapped,
}
impl EventChannel<DeviceEvent> for TappedChannel {
    fn try_send(&self, message: DeviceEvent) -> Result<(), SendError<DeviceEvent>> {
        self.inner.try_send(message)
    }
    fn receive(&self) -> VendorFuture<'_, Option<DeviceEvent>> {
        Box::pin(async move {
            let event = self.inner.receive().await?;
            if let Some(event) = &event {
                let (connected, device) = match event {
                    DeviceEvent::Connected(d) => (true, d.clone()),
                    DeviceEvent::Disconnected(d) => (false, d.clone()),
                };
                let serial = device.0.info().serial_number.to_string_lossy();
                let healthy = device.0.info().healthy;
                checked_lock(&self.tap).push((connected, serial, device, healthy));
            }
            Ok(event)
        })
    }
    fn close(&self, cause: Option<Failure>) {
        self.inner.close(cause)
    }
    fn cancel(&self, reason: Cancellation) {
        self.inner.cancel(reason)
    }
    fn state(&self) -> MailboxState {
        self.inner.state()
    }
}

/// Runs one scenario (one script per adb server) to completion in virtual time.
pub fn run_scenario(scripts: &[&str]) -> Observed {
    run_loaded(scripts.iter().map(|name| load(name)).collect())
}

/// One server from `script`, whose only event is an adb server outage at virtual `at_ms`.
fn run_outage(script: &str, at_ms: i64, restart_after: Option<Duration>) -> Observed {
    let (script, _, index) = load(script);
    let event = Event {
        server: 0,
        at_ms,
        connect: false,
        serial: String::new(),
        kill: Some(restart_after),
    };
    run_loaded(vec![(script, vec![event], index)])
}

fn run_loaded(loaded: Vec<(Value, Vec<Event>, usize)>) -> Observed {
    let runtime = checked(tokio::runtime::Builder::new_multi_thread().enable_all().build());
    let mut servers = Vec::new();
    for (script, _, _) in &loaded {
        let compiled = checked(checked(Script::from_yaml(&script.to_string())).compile(Path::new(".")));
        servers.push(checked(
            runtime.block_on(FakeAdbServer::start(compiled, ServerOptions::default())),
        ));
    }
    // (server, serial) -> how often the fake has listed the device so far.
    let mut listed: std::collections::HashMap<(usize, String), usize> = std::collections::HashMap::new();
    for (server, (script, _, _)) in loaded.iter().enumerate() {
        for device in script["devices"].as_array().into_iter().flatten() {
            if device["connected"] != false {
                listed.insert((server, device["serial"].as_str().unwrap_or_default().to_owned()), 1);
            }
        }
    }
    let ports: Vec<u16> = servers.iter().map(FakeAdbServer::port).collect();
    // Devices the script connects at the start and never drops. Kotlin's runs always put a batch on each of them before
    // the first drop (the recordings' `beforeDrop`); whether it does here depends on how fast the box sets the device
    // up against the virtual clock, so a drop waits for it (the dropped device itself never gets one before its drop).
    let healthy: Vec<(u16, String)> = listed
        .keys()
        .filter(|(server, serial)| {
            !loaded[*server]
                .1
                .iter()
                .any(|event| !event.connect && event.kill.is_none() && event.serial == *serial)
        })
        .map(|(server, serial)| (ports[*server], serial.clone()))
        .collect();
    let mut events: Vec<Event> = loaded
        .into_iter()
        .enumerate()
        .flat_map(|(server, (_, events, _))| events.into_iter().map(move |e| Event { server, ..e }))
        .collect();
    events.sort_by_key(|e| e.at_ms);
    let servers = Arc::new(servers);
    let started: Started = Arc::default();
    let clock_now = Arc::new(AtomicI64::new(0));
    let (log_started, log_now) = (started.clone(), clock_now.clone());
    let tap: Tapped = Arc::default();
    let log_tap = tap.clone();
    let final_tap = tap.clone();
    let script_events = events.clone();
    let gate_started = started.clone();
    let output = checked(tempfile::tempdir());
    let base = JavaPath::from_text("/");
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: JavaPath::from_text("/Marathonfile"),
        marathonfile_dir: base.clone(),
        process_dir: base,
        environment: vec![],
        now: jcompat::jtime::instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    };
    let configuration = checked(MarathonConfigurationLoader.parse_sync(&marathonfile(&ports, output.path()), &context));
    let driven = servers.clone();
    let observed = CoreExecutor::run_virtual(move |core, replay| {
        Box::pin(async move {
            let logging = Arc::new(Mutex::new(MarathonLogging::default()));
            let console = RecordingConsole::new(logging.clone());
            let hooks = RuntimeHooks::live();
            let barriers = hooks.barriers();
            let metrics = MetricsBackends {
                setup: Arc::new(NoMetricsSetup) as Arc<dyn MetricsSetup>,
                backend: None::<Arc<dyn BackendFactory>>,
            };
            let inputs = RunInputs {
                configuration: Arc::new(configuration),
                logging,
                console: console.clone(),
                stderr: Arc::new(|_| {}),
                cwd: JavaPath::from_text("/"),
                environment: vec![],
                system_information: vec![],
                vendor: Box::new(move |dependencies| {
                    Box::pin(async move {
                        let executor = Arc::new(VirtualDuration {
                            inner: AndroidRunnerExecutor::default(),
                            clock: dependencies.clock.clone(),
                            now: log_now,
                            started: log_started,
                        });
                        let mut services = create_services_with_executor(dependencies, executor).await?;
                        services.provider = Arc::new(TappedProvider {
                            inner: services.provider,
                            tap: log_tap,
                        });
                        Ok::<VendorServices, VendorError>(services)
                    })
                }),
                metrics: Box::new(move |_, _| metrics),
                analytics: None,
                extra_reporters: Box::new(marathon_core::construction::standard_reporters),
                hooks,
                barriers,
                timer: None,
                wall: Arc::new(host_millis),
                exit: None,
                signal_trigger: None,
                signal_hooks: Default::default(),
                live_output: false,
            };
            let marathon = construct(core.clone(), inputs).await.map_err(OperationError::Failed)?;
            let job = core.job(JobKind::Ordinary, None);
            let value: Rc<RefCell<Option<VendorResult<bool>>>> = Rc::new(RefCell::new(None));
            let output = value.clone();
            core.spawn(
                job.clone(),
                Box::new(move |task: TaskContext| {
                    Box::pin(async move {
                        let result = marathon.run_async(&ExecutionCommand::Run, &task).await;
                        *output.borrow_mut() = Some(result);
                        Ok(())
                    })
                }),
            );
            let step = |label: &'static str, e: marathon_core::time::ReplayError| {
                OperationError::Failed(marathon_core::executor::shared::failure(
                    label,
                    &e.message.to_string_lossy(),
                ))
            };
            let healthy_started = || {
                let batches = checked_lock(&gate_started);
                healthy
                    .iter()
                    .all(|(port, serial)| batches.iter().any(|b| b.0 == *port && b.1 == *serial))
            };
            let mut now = 0;
            let mut next_event = 0;
            let mut held_since: Option<std::time::Instant> = None;
            let mut applied = (listed.values().filter(|n| **n > 0).count(), 0);
            let deadline = std::time::Instant::now() + RUN_TIMEOUT;
            while value.borrow().is_none() && now < LIMIT_MILLIS {
                if std::time::Instant::now() > deadline {
                    return Err(OperationError::Failed(marathon_core::executor::shared::failure(
                        "RunTimeout",
                        &format!("no result after {RUN_TIMEOUT:?} real time, {now} virtual ms"),
                    )));
                }
                clock_now.store(now, Ordering::SeqCst);
                // An event is applied once it is due and the run has reached the state the scenario needs:
                // the provider's earlier events have reached the scheduler, every listed device has finished
                // setup (its logcat stream is open), and a device about to drop is in the hung step of prepare
                // (`logcat -c`) while the devices that are never dropped have started a batch. Until then virtual time is held (for
                // `HOLD`, then it keeps stepping), so an event is applied at its time, late only on a stall, never early.
                while next_event < events.len() && events[next_event].at_ms <= now {
                    let event = &events[next_event];
                    let received = checked_lock(&tap);
                    let delivered = (
                        received.iter().filter(|r| r.0).count(),
                        received.iter().filter(|r| !r.0).count(),
                    );
                    drop(received);
                    let ready = delivered.0 >= applied.0
                        && delivered.1 >= applied.1
                        && listed
                            .iter()
                            .filter(|(_, times)| **times > 0)
                            .all(|((server, serial), times)| {
                                requests(&driven[*server], serial, "logcat -v long") >= *times
                            })
                        && (event.connect
                            || (event.kill.is_some() && healthy_started())
                            || (requests(&driven[event.server], &event.serial, HELD_NOW) >= 1 && healthy_started()));
                    if !ready {
                        assert!(
                            now - event.at_ms <= MAX_LATE_MILLIS,
                            "event {} {} (server {}) still not ready {} virtual ms after its time; delivered {delivered:?}, expected {applied:?}",
                            if event.connect { "connect" } else { "disconnect" },
                            event.serial,
                            event.server,
                            now - event.at_ms
                        );
                        break;
                    }
                    let server = &driven[event.server];
                    if let Some(restart_after) = event.kill {
                        server.kill(restart_after);
                        next_event += 1;
                        continue;
                    }
                    checked(if event.connect {
                        server.connect_device(&event.serial)
                    } else {
                        server.disconnect_device(&event.serial)
                    });
                    let times = listed.entry((event.server, event.serial.clone())).or_default();
                    *times = if event.connect { *times + 1 } else { 0 };
                    if event.connect {
                        applied.0 += 1;
                    } else {
                        applied.1 += 1;
                    }
                    next_event += 1;
                }
                // A due event that is not ready yet holds the clock, so that a slow box does not turn the wait into
                // virtual lateness that the run's batches would be placed by. Stepping resumes after `HOLD` in case
                // the state the event needs depends on virtual time.
                // The clock also stops just before the next drop until the healthy devices have started a batch:
                // their first batch is placed by the virtual time at which the box gets to run it, so a clock that
                // ran ahead of the work would place it after the drop.
                let barrier = events[next_event..]
                    .iter()
                    .find(|e| !e.connect)
                    .map(|e| e.at_ms - STEP_MILLIS)
                    .filter(|barrier| now >= *barrier && !healthy_started());
                if barrier.is_some() || (next_event < events.len() && events[next_event].at_ms <= now) {
                    let since = *held_since.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() < HOLD {
                        replay.advance_to(now).await.map_err(|e| step("replay", e))?;
                        tokio::time::sleep(SETTLE).await;
                        continue;
                    }
                } else {
                    held_since = None;
                }
                now += STEP_MILLIS;
                replay.advance_to(now).await.map_err(|e| step("replay", e))?;
                tokio::time::sleep(SETTLE).await;
            }
            let finished = value.borrow().is_some();
            if !finished {
                job.cancel(marathon_core::executor::shared::requested("scenario teardown"));
                let _ = job.join().await;
            }
            let result = value.take();
            Ok(Observed {
                result,
                lines: console.lines(),
                transcripts: Vec::new(),
                virtual_millis: now,
                batches: Vec::new(),
                events: Vec::new(),
                received: Vec::new(),
                warnings: Vec::new(),
            })
        })
    });
    let mut observed = checked(observed);
    observed.events = script_events;
    observed.received = checked_lock(&final_tap).clone();
    observed.batches = checked_lock(&started)
        .iter()
        .map(|(port, serial, start)| {
            let index = ports.iter().position(|p| p == port).unwrap_or(usize::MAX);
            (index, serial.clone(), *start)
        })
        .collect();
    let mut servers = checked(Arc::try_unwrap(servers).map_err(|_| "server handles still shared"));
    for server in &mut servers {
        checked(runtime.block_on(server.shutdown()));
        observed.transcripts.push(server.transcript());
        observed.warnings.push(server.warnings());
    }
    drop(output);
    observed
}

/// The device-list lines of the provider, as the CLI's debug log prints them.
pub fn state_changes(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| match line {
            Line::Log { logger, message, .. }
                if logger == "AdamDeviceProvider" && message.contains("changed state to") =>
            {
                Some(message.clone())
            }
            _ => None,
        })
        .collect()
}

/// The oracle's `ranOn` and `batchesRun` (see `summarize` in `tools/oracle/android-reconnect-probes/run`): per server
/// and serial, whether a batch started before that server's first drop and after the connect that follows it.
pub fn ran_on(observed: &Observed) -> (usize, BTreeMap<String, (bool, bool)>) {
    let mut out = BTreeMap::new();
    for (server, serial, start) in &observed.batches {
        let mine = observed.events.iter().filter(|e| e.server == *server);
        let drop = mine.clone().find(|e| !e.connect).map(|e| e.at_ms);
        let back = drop.and_then(|d| mine.clone().find(|e| e.connect && e.at_ms > d).map(|e| e.at_ms));
        let entry = out
            .entry(format!("server{}/{serial}", server + 1))
            .or_insert((false, false));
        if drop.is_none_or(|d| *start < d) {
            entry.0 = true;
        } else if back.is_some_and(|b| *start >= b) {
            entry.1 = true;
        }
    }
    (observed.batches.len(), out)
}

pub fn oracle(scenario: &str) -> Value {
    let path: PathBuf = fixture(&format!("fake-adb/android-reconnect/expected/{scenario}-kotlin.json"));
    let value: Value = checked(serde_json::from_str(&checked(std::fs::read_to_string(path))));
    value["compare"].clone()
}

pub fn ran_on_json(ran: &BTreeMap<String, (bool, bool)>) -> Value {
    Value::Object(
        ran.iter()
            .map(|(k, (before, after))| (k.clone(), json!({"beforeDrop": before, "afterReconnect": after})))
            .collect(),
    )
}

/// The Rust run against the Kotlin CLI's recorded outcome for the same scripts.
fn matches_kotlin(scenario: &str, scripts: &[&str]) -> Observed {
    let observed = run_scenario(scripts);
    let (total, ran) = ran_on(&observed);
    let expected = oracle(scenario);
    assert!(
        matches!(observed.result, Some(Ok(true))),
        "run ended after {} virtual ms: {:?}",
        observed.virtual_millis,
        observed.result.as_ref().map(|r| r.is_ok())
    );
    assert_eq!(observed.warnings, vec![0; scripts.len()], "fake-adb warnings");
    assert_eq!(expected["stateChanges"], json!(state_changes(&observed.lines)));
    assert_eq!(expected["batchesRun"], total);
    assert_eq!(
        expected["ranOn"],
        ran_on_json(&ran),
        "virtual ms {} batches {:?}",
        observed.virtual_millis,
        observed.batches
    );
    observed
}

#[test]
fn reconnected_device_gets_work_after_prepare_was_interrupted() {
    let observed = matches_kotlin("same-server", &["same-server"]);
    // Disconnected(old) then Connected(new): the provider saw the serial leave and come back.
    assert_eq!(
        state_changes(&observed.lines)[2..],
        [
            "Device emulator-5556 changed state to DISCONNECTED",
            "Device emulator-5556 changed state to CONNECTED"
        ]
    );
    // The scheduler received Disconnected(old) and then Connected(new) for the serial: a different device
    // object, and the old one reported itself unhealthy.
    let of_5556: Vec<_> = observed
        .received
        .iter()
        .filter(|r| r.1.ends_with(":emulator-5556"))
        .collect();
    assert_eq!(
        of_5556.iter().map(|r| r.0).collect::<Vec<_>>(),
        [true, false, true],
        "Connected, Disconnected, Connected"
    );
    assert_ne!(of_5556[0].2, of_5556[2].2);
    assert_eq!(of_5556[0].2, of_5556[1].2);
    // (Healthy is the tracker's live view by adb serial, so it is read when the event arrives.)
    assert!(!of_5556[1].3);
    // The old actor is terminated and a fresh one initialized for the same serial.
    assert_eq!(actor_messages(&observed, "emulator-5556", "initialize "), 2);
    // (The run ends with a terminate of the survivors too.)
    assert!(actor_messages(&observed, "emulator-5556", "terminate ") >= 1);
}

#[test]
fn reconnected_device_gets_work_after_transient_prepare_failures() {
    let observed = matches_kotlin("same-server-flaky", &["same-server-flaky"]);
    // Two prepare attempts of three installer attempts each fail before the third succeeds (30 x 10 s retry).
    let failed = observed
        .lines
        .iter()
        .filter(
            |l| matches!(l, Line::Log { message, .. } if message.starts_with("Error while installing com.example,")),
        )
        .count();
    assert_eq!(failed, 6);
}

/// Device-actor log lines about one adb serial that start with `prefix`.
fn actor_messages(observed: &Observed, serial: &str, prefix: &str) -> usize {
    observed
        .lines
        .iter()
        .filter(|line| match line {
            Line::Log { logger, message, .. } => {
                (logger.contains("_DeviceActor") || logger.starts_with("D."))
                    && message.starts_with(prefix)
                    && message.contains(serial)
            }
            Line::Raw(_) => false,
        })
        .count()
}

fn pool_warnings(observed: &Observed) -> usize {
    observed
        .lines
        .iter()
        .filter(|l| matches!(l, Line::Log { message, .. } if message.ends_with("already present in pool omni")))
        .count()
}

/// Same adb serial on two servers, the dropped device's entry is the latest one. Kotlin and Rust agree.
#[test]
fn same_adb_serial_on_two_servers_drops_the_right_device_when_it_is_the_latest_entry() {
    let observed = matches_kotlin("cross-host", &["cross-host-a", "cross-host-b"]);
    assert_eq!(pool_warnings(&observed), 0);
}

/// D18 (supersedes the collision half of N27). B's healthy device appeared last, which in Kotlin made the drop on A
/// dispose B's device and return its batch, left A's actor in the pool and ignored A's reconnect. The provider now keys
/// devices by (adb server, serial): A's device is disposed, the reconnect gets a fresh actor that receives work, and
/// B's device keeps working without a returned batch (20 batches, as in the benign order). The state-change lines are
/// those of the Kotlin run; its batch counts are the old behaviour, kept as the witness below.
#[test]
fn same_adb_serial_on_two_servers_keeps_the_healthy_device_working_d18() {
    let observed = run_scenario(&["cross-host-shadowed-a", "cross-host-shadowed-b"]);
    assert!(
        matches!(observed.result, Some(Ok(true))),
        "run ended after {} virtual ms: {:?}",
        observed.virtual_millis,
        observed.result.as_ref().map(|r| r.is_ok())
    );
    assert_eq!(
        oracle("cross-host-shadowed")["stateChanges"],
        json!(state_changes(&observed.lines))
    );
    assert_eq!(observed.warnings, vec![0; 2], "fake-adb warnings");
    assert_eq!(pool_warnings(&observed), 0);
    // The scheduler received, per device (host:port:serial), B's single Connected and A's Connected, Disconnected
    // (the dropped device itself) and Connected (a new device object); the drop never named B's device.
    let mut per_device: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for received in &observed.received {
        per_device.entry(received.1.clone()).or_default().push(received);
    }
    assert_eq!(per_device.len(), 2, "{per_device:?}");
    let mut sequences: Vec<_> = per_device
        .values()
        .map(|events| events.iter().map(|r| r.0).collect::<Vec<_>>())
        .collect();
    sequences.sort();
    assert_eq!(sequences, [vec![true], vec![true, false, true]]);
    let dropped = per_device.values().find(|events| events.len() == 3).unwrap();
    assert_eq!(dropped[0].2, dropped[1].2);
    assert_ne!(dropped[0].2, dropped[2].2);
    let (total, ran) = ran_on(&observed);
    // Twenty tests, none run twice: B's running batch was not returned.
    assert_eq!(total, 20);
    // A's fresh actor gets work after the reconnect; B's healthy device ran before the drop and kept going.
    assert_eq!(ran["server1/emulator-5556"], (false, true));
    assert_eq!(ran["server2/emulator-5556"], (true, false));
    // Two actors were initialized for A's serial (the dropped one and its replacement), one for B's.
    assert_eq!(actor_messages(&observed, "emulator-5556", "initialize "), 3);
}

/// D22 (N16), the whole run: the adb server of the only pool goes away while tests run and returns. The pool keeps its
/// devices, as removing the last one would end the run; the batches wait for the server; every test passes. In Kotlin
/// the provider's job is over by then (its JVM probe: `subscriptions=1`).
#[test]
fn a_run_survives_an_adb_server_restart_d22() {
    let observed = run_outage("same-server", 6000, Some(Duration::from_millis(300)));
    assert!(
        matches!(observed.result, Some(Ok(true))),
        "run ended after {} virtual ms: {:?}",
        observed.virtual_millis,
        observed.result.as_ref().map(|r| r.is_ok())
    );
    // Tracking resumed, with no device event for a device that stayed listed.
    let tracks = observed.transcripts[0]
        .iter()
        .filter(|r| r["type"] == "request" && r["service"] == "host:track-devices")
        .count();
    assert_eq!(tracks, 2);
    assert!(observed.received.iter().all(|r| r.0), "{:?}", observed.received);
    assert_eq!(actor_messages(&observed, "emulator-5554", "initialize "), 1);
    assert_eq!(pool_warnings(&observed), 0);
}

/// D22: a server that never returns does not keep the run alive. Its devices are disconnected after
/// `waitForDevicesTimeoutMillis`, the pool ends with them, and the run reports the tests it could not run.
#[test]
fn a_run_ends_when_its_adb_server_never_returns_d22() {
    let observed = run_outage("same-server", 6000, None);
    assert!(
        matches!(observed.result, Some(Ok(false))),
        "run ended after {} virtual ms: {:?}",
        observed.virtual_millis,
        observed.result.as_ref().map(|r| r.is_ok())
    );
    assert!(
        observed.virtual_millis < LIMIT_MILLIS,
        "{} virtual ms",
        observed.virtual_millis
    );
    assert!(observed.received.iter().any(|r| !r.0));
}

/// The Kotlin CLI's recording of the same scenario is the witness of the behaviour D18 replaced; it is not re-recorded.
#[test]
fn kotlin_witness_of_the_serial_only_device_map_is_unchanged() {
    let kotlin = oracle("cross-host-shadowed");
    assert_eq!(kotlin["batchesRun"], 21);
    assert_eq!(
        kotlin["ranOn"],
        json!({
            "server1/emulator-5556": {"beforeDrop": false, "afterReconnect": true},
            "server2/emulator-5556": {"beforeDrop": true, "afterReconnect": false}
        })
    );
}
