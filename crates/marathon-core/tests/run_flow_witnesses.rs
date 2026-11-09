//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt and
//! execution/command/parse/MarathonTestParseCommand.kt (JVM witnesses beyond the source tests).
//!
//! Each case mirrors a row of fixtures/oracle/run-flow/probes.jsonl (tools/oracle/run-flow-probes):
//! the outcome, the order of lifecycle calls, the records of the Marathon, parse-command and
//! Timings loggers as the registry lets them through, and which pre-existing or vendor-written
//! files survive in outputDir (C9). Normalizations: `Parsing took N ms` (wall clock), the
//! outputDir path in `Output … already exists`, P1's OS facts after `System Information:`, and
//! D1's usage slot (a no-op here, so its close is absent from the Rust trace).

use marathon_logging::Level;
use marathon_core::{
    analytics::{
        Analytics, MetricsProviderImpl, ReporterRegistration, ReporterSlot,
        remote::{ExecutionTime, RemoteDataSource, SuccessRate},
    },
    error::*,
    executor::shared::failure,
    marathon::{LOGGER, TIMINGS},
    model::{ExecutionCommand, Test},
    report::{ExecutionReport, Reporter},
    runtime::*,
    vendor::{LocalTestParser, LogConfigurator, TestParser, VendorContext},
};
use marathon_error::FailureKind;
use marathon_test_support::{
    console::Line,
    factory::{FakeTimer, MarathonFactory},
    scenario::{self, Entry, Script},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const PARSE: &str = "marathon_core::execution::parse_command";
type Trace = Arc<Mutex<Vec<String>>>;

fn row(case: &str) -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/oracle/run-flow/probes.jsonl"
    );
    let text = std::fs::read_to_string(path).unwrap();
    text.lines()
        .skip(1)
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|r| r["case"] == case)
        .unwrap_or_else(|| panic!("no probe row {case}"))
}

fn push(trace: &Trace, name: &str, fail: &Option<String>) -> VendorResult<()> {
    trace.lock().unwrap().push(name.to_owned());
    if fail.as_deref() == Some(name) {
        return Err(VendorError::Other(failure(
            FailureKind::IllegalState,
            &format!("{name} failed"),
        )));
    }
    Ok(())
}

struct Configurator(Trace);
impl LogConfigurator for Configurator {
    fn configure(&self, _: bool, _: Arc<Mutex<marathon_logging::LogSettings>>) -> VendorFuture<'_, ()> {
        self.0.lock().unwrap().push("configure".into());
        Box::pin(async { Ok(()) })
    }
}
/// D52's witnesses: a call that doesn't return until the process would have exited.
type Hold = Arc<marathon_core::executor::SharedDeferred<()>>;
struct Parser {
    trace: Trace,
    out: PathBuf,
    markers: bool,
    tests: Vec<Test>,
    /// Signals when the parser starts, and holds it there until the exit.
    signal_and_hold: Option<(Arc<marathon_core::executor::SharedDeferred<i32>>, i32, Hold)>,
}
impl LocalTestParser for Parser {
    fn extract(&self, _: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        self.trace.lock().unwrap().push("extract".into());
        if self.markers {
            std::fs::create_dir_all(&self.out).unwrap();
            std::fs::write(self.out.join("parser-marker"), "parser").unwrap();
        }
        let tests = self.tests.clone();
        let held = self.signal_and_hold.clone();
        Box::pin(async move {
            if let Some((trigger, number, hold)) = held {
                Deferred::complete(trigger.as_ref(), Ok(number));
                let _ = Deferred::wait(hold.as_ref()).await;
            }
            Ok(tests)
        })
    }
}
struct NoTests;
impl marathon_core::vendor::RemoteTestParser for NoTests {
    fn extract(&self, _: marathon_core::vendor::DeviceRef, _: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async { Ok(vec![]) })
    }
}
/// D53: a remote parser that fails on the devices named, and records where each attempt ran.
struct FailsOn {
    failing: Vec<&'static str>,
    extracted: Trace,
}
impl marathon_core::vendor::RemoteTestParser for FailsOn {
    fn extract(&self, device: marathon_core::vendor::DeviceRef, _: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        let serial = device.0.info().serial_number.to_owned();
        self.extracted.lock().unwrap().push(serial.clone());
        let fails = self.failing.contains(&serial.as_str());
        Box::pin(async move {
            if fails {
                return Err(VendorError::Other(failure(
                    FailureKind::IllegalState,
                    &format!("parse failed on {serial}"),
                )));
            }
            Ok(vec![test("method1")])
        })
    }
}
struct Source {
    trace: Trace,
    fail: Option<String>,
}
impl RemoteDataSource for Source {
    fn request_all_success_rates(&self, _: jcompat::jtime::Instant) -> ServiceFuture<'_, Vec<SuccessRate>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn request_all_execution_times(&self, _: f64, _: jcompat::jtime::Instant) -> ServiceFuture<'_, Vec<ExecutionTime>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        let result = push(&self.trace, "analytics.close", &self.fail).map_err(|e| e.failure().clone().into());
        Box::pin(async move { result })
    }
}
/// Stands in for the probe's TrackerInternal: the pipeline's close reaches it last.
struct Closing {
    trace: Trace,
    fail: Option<String>,
    hold: Option<Hold>,
}
impl Reporter for Closing {
    fn generate<'a>(&'a self, _: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        let result = push(&self.trace, "tracker.close", &self.fail).map_err(|e| e.failure().clone().into());
        let hold = self.hold.clone();
        Box::pin(async move {
            if let Some(hold) = hold {
                let _ = Deferred::wait(hold.as_ref()).await;
            }
            result
        })
    }
}

#[derive(Default)]
struct Options {
    debug: bool,
    ignore_failures: bool,
    no_tests: bool,
    block_all: bool,
    markers: bool,
    prepare: bool,
    /// With `prepare`: `outputDir/link` points at a directory outside outputDir.
    symlink: bool,
    device_initialization_timeout_millis: Option<i64>,
    fail: Option<&'static str>,
    /// Remote parser; borrow fails (`Some(false)`) or never returns (`Some(true)`).
    remote_hang: Option<bool>,
    /// D53: a remote parse over these stub devices, failing on those also in the second list.
    remote_devices: Option<(Vec<&'static str>, Vec<&'static str>)>,
    /// A signal number delivered at 1000 ms through the relay.
    signal: Option<i32>,
    /// D52: the signal comes when the parser starts, before the output directory is wiped.
    signal_at_parse: bool,
    /// D52: the signal comes when the run's own `on_finish` reaches the provider.
    signal_at_terminate: bool,
    /// D52: this call doesn't return until the process would have exited.
    terminate_hangs: bool,
    reports_hang: bool,
}
/// Exit statuses with the trace as it stood when the process would have ended.
type Exits = Arc<Mutex<Vec<(i32, Vec<String>)>>>;

struct Result {
    exits: Vec<(i32, Vec<String>)>,
    /// D53: the serial number of each device a remote parse ran on, in order.
    extracted: Vec<String>,
    outcome: scenario::Outcome,
    trace: Vec<String>,
    out: PathBuf,
}

fn test(method: &str) -> Test {
    scenario::test("com.example", "SimpleTest", method)
}

fn run(o: Options, entry: Entry, command: ExecutionCommand) -> Result {
    let trace: Trace = Arc::default();
    let mut factory = MarathonFactory::new();
    let out = factory.configuration.output_dir().join("output");
    let c = &mut factory.configuration.configuration;
    c.output_dir = std::path::PathBuf::from(out.to_string_lossy().into_owned());
    c.debug = o.debug;
    c.ignore_failures = o.ignore_failures;
    if let Some(millis) = o.device_initialization_timeout_millis {
        c.device_initialization_timeout_millis = millis;
    }
    if o.block_all {
        let config = "name: \"\"\noutputDir: \"\"\nvendorConfiguration:\n  type: \"stub\"\nfilteringConfiguration:\n  blocklist:\n    - type: \"simple-class-name\"\n      regex: \".*\"\n";
        let context = marathon_config::binding::BindingContext {
            profile: marathon_config::binding::Profile::Marathonfile,
            marathonfile: std::path::PathBuf::from("/config/Marathonfile"),
            marathonfile_dir: std::path::PathBuf::from("/config"),
            process_dir: std::path::PathBuf::from("/"),
            environment: vec![],
            now: jcompat::jtime::instant_of(0, 0).unwrap(),
            analytics_tracking: false,
            bugsnag_reporting: false,
        };
        c.filtering_configuration = marathon_config::loader::MarathonConfigurationLoader
            .parse_sync(config.as_bytes(), &context)
            .unwrap()
            .filtering_configuration;
    }
    if o.prepare {
        std::fs::create_dir_all(out.join("old-dir")).unwrap();
        std::fs::write(out.join("old.txt"), "old").unwrap();
        std::fs::write(out.join("old-dir/nested.txt"), "nested").unwrap();
        if o.symlink {
            let external = out.with_file_name("external");
            std::fs::create_dir_all(&external).unwrap();
            std::fs::write(external.join("sentinel"), "keep").unwrap();
            std::os::unix::fs::symlink(&external, out.join("link")).unwrap();
        }
    }
    let fail = o.fail.map(str::to_owned);
    let hold: Hold = marathon_core::executor::SharedDeferred::new();
    let trigger = marathon_core::executor::SharedDeferred::<i32>::new();
    factory.log_configurator = Some(Arc::new(Configurator(trace.clone())));
    let extracted: Trace = Arc::default();
    if let Some((_, failing)) = &o.remote_devices {
        factory.parser = Some(TestParser::Remote(Arc::new(FailsOn {
            failing: failing.clone(),
            extracted: extracted.clone(),
        })));
    } else if o.remote_hang.is_some() {
        factory.parser = Some(TestParser::Remote(Arc::new(NoTests)));
    } else {
        factory.parser = Some(TestParser::Local(Arc::new(Parser {
            trace: trace.clone(),
            out: out.clone(),
            markers: o.markers,
            tests: if o.no_tests { vec![] } else { vec![test("method1")] },
            signal_and_hold: o
                .signal_at_parse
                .then(|| (trigger.clone(), o.signal.unwrap_or(15), hold.clone())),
        })));
    }
    let remote_hang = o.remote_hang;
    let remote_devices = o.remote_devices.clone();
    let source = Arc::new(Source {
        trace: trace.clone(),
        fail: fail.clone(),
    });
    factory.analytics = Some(Box::new(move |context| {
        let defaults = marathon_config::Defaults {
            success_rate: 0.0,
            duration: jcompat::jtime::duration_of(0, 0).unwrap(),
            observed_duration_min_tests: None,
        };
        Ok(Analytics::new(MetricsProviderImpl::new(source, defaults, context)?))
    }));
    factory.reporters = vec![ReporterRegistration {
        slot: ReporterSlot::Html,
        reporter: Arc::new(Closing {
            trace: trace.clone(),
            fail: fail.clone(),
            hold: o.reports_hang.then(|| hold.clone()),
        }),
    }];
    factory.timer = Some(Arc::new(FakeTimer(Arc::new(|| 0))));
    let exits: Exits = Arc::default();
    if o.signal.is_some() {
        let (recorded, at_exit, released) = (exits.clone(), trace.clone(), hold.clone());
        factory.exit = Some(Arc::new(move |status| {
            let trace = at_exit.lock().unwrap().clone();
            recorded.lock().unwrap().push((status, trace));
            Deferred::complete(released.as_ref(), Ok(()));
        }));
        factory.signal_trigger = Some(trigger.clone());
    }
    let signal = o.signal;
    let (hooks_trace, markers, marker_dir) = (trace.clone(), o.markers, out.clone());
    let script = Script {
        entry,
        command,
        events: vec![(600_000, "end".into(), None, serde_json::Value::Null)],
        devices: HashMap::new(),
        expect: Value::Null,
    };
    let at_subscribe = !o.signal_at_parse && !o.signal_at_terminate;
    let (subscribed_trigger, subscribed_signal) = (trigger.clone(), signal.filter(|_| at_subscribe));
    let terminating = (o.signal_at_terminate.then_some(signal).flatten(), trigger.clone());
    let terminate_hold = o.terminate_hangs.then(|| hold.clone());
    let outcome = scenario::drive(
        factory,
        script,
        None,
        Some(Box::new(move |built| {
            let (t1, t2, t3) = (hooks_trace.clone(), hooks_trace.clone(), hooks_trace);
            *built.provider.on_initialize.lock().unwrap() = Some(Arc::new(move |_| {
                t1.lock().unwrap().push("initialize".into());
                if markers {
                    std::fs::create_dir_all(&marker_dir).unwrap();
                    std::fs::write(marker_dir.join("provider-marker"), "provider").unwrap();
                }
                Box::pin(async { Ok(()) })
            }));
            *built.provider.on_subscribe.lock().unwrap() = Some(Arc::new(move || {
                t2.lock().unwrap().push("subscribe".into());
                // The probe signals once the run waits for devices (its `PROBE ready`).
                if let Some(number) = subscribed_signal {
                    marathon_core::runtime::Deferred::complete(subscribed_trigger.as_ref(), Ok(number));
                }
            }));
            match remote_hang {
                Some(true) => *built.provider.borrow_delay_millis.lock().unwrap() = i64::MAX,
                Some(false) => {
                    *built.provider.borrow_error.lock().unwrap() = Some(VendorError::Other(failure(
                        FailureKind::IllegalState,
                        "no device to borrow",
                    )))
                }
                None => {}
            }
            if let Some((serials, _)) = &remote_devices {
                *built.provider.borrowable.lock().unwrap() = serials
                    .iter()
                    .map(|s| {
                        marathon_core::vendor::DeviceRef(marathon_test_support::stub_device::StubDevice::with_serial(s))
                    })
                    .collect();
            }
            *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
                let result = push(&t3, "terminate", &fail);
                if let (Some(number), trigger) = &terminating {
                    Deferred::complete(trigger.as_ref(), Ok(*number));
                }
                let hold = terminate_hold.clone();
                Box::pin(async move {
                    if let Some(hold) = hold {
                        let _ = Deferred::wait(hold.as_ref()).await;
                    }
                    result
                })
            }));
        })),
    );
    let trace = trace.lock().unwrap().clone();
    let exits = exits.lock().unwrap().clone();
    let extracted = extracted.lock().unwrap().clone();
    Result {
        exits,
        extracted,
        outcome,
        trace,
        out,
    }
}

/// (level, message, throwable class) of one logger, normalized.
fn rust_events(lines: &[Line], logger: &str) -> Vec<(String, String, Option<String>)> {
    let mut after_heading = false;
    lines
        .iter()
        .filter_map(|line| match line {
            Line::Log {
                logger: name,
                level,
                message,
                cause,
            } if name == logger => {
                if message == "System Information:" {
                    after_heading = true;
                } else if after_heading && message.starts_with("os.name: ") {
                    return None;
                } else {
                    after_heading = false;
                }
                Some((
                    level_name(*level),
                    normalize(message),
                    cause.as_ref().map(|c| c.kind.description().to_owned()),
                ))
            }
            _ => None,
        })
        .collect()
}
fn level_name(level: Level) -> String {
    format!("{level:?}").to_uppercase()
}
fn normalize(message: &str) -> String {
    if let Some(rest) = message.strip_prefix("Parsing took ") {
        let _ = rest;
        return "Parsing took <N> ms".into();
    }
    if message.starts_with("Output ") && message.ends_with(" already exists") {
        return "Output <outputDir> already exists".into();
    }
    message.to_owned()
}
fn jvm_events(row: &Value, logger: &str) -> Vec<(String, String, Option<String>)> {
    row["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["logger"] == logger)
        .map(|e| {
            (
                e["level"].as_str().unwrap().to_owned(),
                normalize(e["message"].as_str().unwrap()),
                e["throwable"]
                    .as_str()
                    .map(|class| marathon_test_support::recorded_errors::kind(class).to_string()),
            )
        })
        .collect()
}

const FILES: [&str; 9] = [
    "old.txt",
    "old-dir/",
    "old-dir/nested.txt",
    "provider-marker",
    "parser-marker",
    "named.yml",
    "p.yml",
    "filtered.yml",
    ".yml",
];
fn surviving(out: &std::path::Path) -> Option<Vec<String>> {
    out.is_dir().then(|| {
        FILES
            .iter()
            .filter(|f| out.join(f.trim_end_matches('/')).exists())
            .map(|f| (*f).to_owned())
            .collect()
    })
}

/// D51's allowance. The JVM's trace stops at the slot that failed (C25); ours runs the later slots.
/// Returns the JVM trace with those added, so nothing else can differ.
fn d51_trace(case: &str, jvm: Vec<String>) -> Vec<String> {
    const SLOTS: [&str; 3] = ["analytics.close", "terminate", "tracker.close"];
    let failing = match case {
        "finish-fail-analytics" => "analytics.close",
        "finish-fail-terminate" | "exceptional-finish-fail" => "terminate",
        "finish-fail-tracker" => "tracker.close",
        _ => return jvm,
    };
    assert_eq!(jvm.last().map(String::as_str), Some(failing), "{case}: {jvm:?}");
    let later = SLOTS.iter().skip_while(|s| **s != failing).skip(1);
    jvm.iter().cloned().chain(later.map(|s| (*s).to_owned())).collect()
}
type Record = (String, String, Option<String>);
/// D51's allowance on the logs: exactly these extra Marathon records, which must be there.
fn d51_extra_records(case: &str) -> Vec<Record> {
    let error = |step: &str| {
        (
            "ERROR".to_owned(),
            format!("{step} failed during cleanup"),
            Some("invalid state".to_owned()),
        )
    };
    let filtered = (
        "ERROR".to_owned(),
        "No tests left to run after filtering: blocklist simple-class-name removed 1".to_owned(),
        None,
    );
    match case {
        "finish-fail-analytics" => vec![filtered, error("Closing the analytics history")],
        "finish-fail-terminate" => vec![filtered, error("Terminating the device provider")],
        "finish-fail-tracker" => vec![filtered, error("Closing the reports and tracker")],
        "exceptional-finish-fail" => vec![error("Terminating the device provider")],
        "run-filtered-empty" | "wipe-run" => vec![filtered],
        _ => vec![],
    }
}
fn without_d51_records(case: &str, mut events: Vec<Record>) -> Vec<Record> {
    for extra in d51_extra_records(case) {
        let at = events.iter().position(|e| *e == extra);
        events.remove(at.unwrap_or_else(|| panic!("{case}: no {extra:?} in {events:?}")));
    }
    events
}
#[test]
fn the_d51_allowances_hide_nothing_else() {
    let names = |names: &[&str]| names.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    // Other cases are untouched.
    assert_eq!(d51_trace("run-ok", names(&["a"])), names(&["a"]));
    assert!(d51_extra_records("run-ok").is_empty());
    assert!(
        d51_extra_records("filtered-empty-parse").is_empty(),
        "parse writes its empty list as before"
    );
    // The added slots are exactly those after the failing one.
    assert_eq!(
        d51_trace("finish-fail-analytics", names(&["x", "analytics.close"])),
        names(&["x", "analytics.close", "terminate", "tracker.close"])
    );
    assert_eq!(
        d51_trace("finish-fail-tracker", names(&["x", "tracker.close"])),
        names(&["x", "tracker.close"])
    );
    // A JVM trace that didn't stop at its slot isn't what the allowance is for.
    assert!(std::panic::catch_unwind(|| d51_trace("finish-fail-terminate", vec!["x".to_owned()])).is_err());
    // A missing record fails, and only one is removed.
    let extra = d51_extra_records("run-filtered-empty").remove(0);
    assert!(std::panic::catch_unwind(|| without_d51_records("run-filtered-empty", vec![])).is_err());
    assert_eq!(
        without_d51_records("run-filtered-empty", vec![extra.clone(), extra.clone()]),
        vec![extra]
    );
}

fn check(case: &str, r: &Result) {
    let row = row(case);
    match (&r.outcome.value, row.get("value")) {
        (Some(Ok(value)), Some(expected)) => assert_eq!(Value::Bool(*value), *expected, "{case}: value"),
        (Some(Err(e)), None) => {
            assert_eq!(
                e.failure().kind,
                marathon_test_support::recorded_errors::kind(row["exception"].as_str().unwrap()),
                "{case}: exception"
            );
            assert_eq!(
                e.failure().message.as_ref().map(|m| m.to_owned()),
                row["message"].as_str().map(str::to_owned),
                "{case}: message"
            );
        }
        (actual, _) => panic!(
            "{case}: {:?} vs {row}",
            actual
                .as_ref()
                .map(|v| v.as_ref().map_err(|e| e.failure().to_string().to_owned()))
        ),
    }
    let expected_trace: Vec<String> = row["trace"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_owned())
        .filter(|t| t != "usage.close")
        .collect();
    assert_eq!(r.trace, d51_trace(case, expected_trace), "{case}: trace");
    for logger in [LOGGER, PARSE, TIMINGS] {
        assert_eq!(
            if logger == LOGGER {
                without_d51_records(case, rust_events(&r.outcome.lines, logger))
            } else {
                rust_events(&r.outcome.lines, logger)
            },
            jvm_events(&row, logger),
            "{case}: {logger} records"
        );
    }
    let jvm_files: Option<Vec<String>> = row["manifest"].as_array().map(|m| {
        FILES
            .iter()
            .filter(|f| m.iter().any(|e| e == **f))
            .map(|f| (*f).to_owned())
            .collect()
    });
    // The probe's mock tracker writes no reports; the real pipeline here may create outputDir.
    assert_eq!(
        surviving(&r.out).unwrap_or_default(),
        jvm_files.unwrap_or_default(),
        "{case}: surviving files"
    );
}

fn parse(name: Option<&str>) -> ExecutionCommand {
    ExecutionCommand::Parse {
        output_file_name: name.map(Into::into),
        include_flaky_tests: false,
    }
}

#[test]
fn wipe_on_run_erases_provider_parser_and_previous_files() {
    let o = Options {
        debug: true,
        block_all: true,
        markers: true,
        prepare: true,
        ..Options::default()
    };
    check("wipe-run", &run(o, Entry::RunAsync, ExecutionCommand::Run));
}

/// C68 is deliberately not reproduced: the JVM wipe follows the link and deletes the external
/// sentinel (Sol R1 probe); deleting outside outputDir is destructive, so Rust removes only the link.
#[cfg(unix)]
#[test]
fn wipe_removes_a_directory_symlink_without_following_it() {
    let o = Options {
        debug: true,
        block_all: true,
        prepare: true,
        symlink: true,
        ..Options::default()
    };
    let r = run(o, Entry::RunAsync, ExecutionCommand::Run);
    assert!(std::fs::symlink_metadata(r.out.join("link")).is_err());
    assert!(!r.out.join("old.txt").exists());
    assert_eq!(
        std::fs::read_to_string(r.out.with_file_name("external").join("sentinel")).unwrap(),
        "keep"
    );
}

#[test]
fn named_parse_keeps_existing_files_and_writes_yml() {
    let o = Options {
        debug: true,
        markers: true,
        prepare: true,
        ..Options::default()
    };
    let r = run(o, Entry::RunAsync, parse(Some("named")));
    check("wipe-parse-named", &r);
    assert_eq!(
        std::fs::read_to_string(r.out.join("named.yml")).unwrap(),
        row("wipe-parse-named")["named.yml"].as_str().unwrap()
    );
}

#[test]
fn unnamed_parse_logs_yaml_only_with_debug_c13() {
    for (debug, case) in [(false, "parse-unnamed-debug-false"), (true, "parse-unnamed-debug-true")] {
        let o = Options {
            debug,
            ..Options::default()
        };
        check(case, &run(o, Entry::RunAsync, parse(None)));
    }
}

#[test]
fn empty_parse_name_still_writes_dot_yml() {
    let o = Options {
        debug: true,
        ..Options::default()
    };
    check("parse-empty-name", &run(o, Entry::RunAsync, parse(Some(""))));
}

#[test]
fn unfiltered_empty_extraction_fails_before_the_wipe() {
    let o = Options {
        debug: true,
        no_tests: true,
        markers: true,
        prepare: true,
        ..Options::default()
    };
    check("no-tests-runasync", &run(o, Entry::RunAsync, ExecutionCommand::Run));
}

#[test]
fn filtered_empty_parse_writes_an_empty_test_list() {
    let o = Options {
        debug: true,
        block_all: true,
        ..Options::default()
    };
    check(
        "filtered-empty-parse",
        &run(o, Entry::RunAsync, parse(Some("filtered"))),
    );
}

#[test]
fn run_maps_no_tests_to_ignore_failures_after_the_protected_finish() {
    for (ignore_failures, case) in [(false, "run-no-tests-ignore-false"), (true, "run-no-tests-ignore-true")] {
        let o = Options {
            debug: true,
            no_tests: true,
            ignore_failures,
            ..Options::default()
        };
        check(case, &run(o, Entry::Run, ExecutionCommand::Run));
    }
}

#[test]
fn run_maps_no_devices_to_false_whatever_ignore_failures_is() {
    for (debug, ignore_failures, case) in [
        (true, false, "run-no-devices-ignore-false"),
        (true, true, "run-no-devices-ignore-true"),
        (false, false, "run-no-devices-debug-false"),
    ] {
        let o = Options {
            debug,
            ignore_failures,
            device_initialization_timeout_millis: Some(50),
            ..Options::default()
        };
        check(case, &run(o, Entry::Run, ExecutionCommand::Run));
    }
}

#[test]
fn run_of_a_filtered_empty_list_succeeds_without_execute() {
    let o = Options {
        debug: true,
        block_all: true,
        ..Options::default()
    };
    check("run-filtered-empty", &run(o, Entry::Run, ExecutionCommand::Run));
}

#[test]
fn successful_parse_through_run_skips_on_finish_c26() {
    let o = Options {
        debug: true,
        ..Options::default()
    };
    check("run-parse", &run(o, Entry::Run, parse(Some("p"))));
}

/// D51: a failing close no longer skips the later slots, where the JVM rows do (C25). The guard is
/// kept, so the exceptional finish that follows is still a no-op, and the outcome is the JVM's.
#[test]
fn a_failing_finish_slot_runs_the_rest_and_keeps_the_outcome_d51() {
    for (slot, case) in [
        ("analytics.close", "finish-fail-analytics"),
        ("terminate", "finish-fail-terminate"),
        ("tracker.close", "finish-fail-tracker"),
    ] {
        let o = Options {
            debug: true,
            block_all: true,
            fail: Some(slot),
            ..Options::default()
        };
        check(case, &run(o, Entry::Run, ExecutionCommand::Run));
    }
}

/// The JVM also prints a CompletionHandlerException as uncaught on stderr (D12: not reproduced).
#[test]
fn a_failing_protected_finish_keeps_the_original_outcome() {
    let o = Options {
        debug: true,
        no_tests: true,
        fail: Some("terminate"),
        ..Options::default()
    };
    check("exceptional-finish-fail", &run(o, Entry::Run, ExecutionCommand::Run));
}

/// withRetry(3, 0): each attempt borrows again; the last attempt's exception survives.
#[test]
fn remote_parsing_retries_borrow_three_times() {
    let o = Options {
        debug: true,
        remote_hang: Some(false),
        ..Options::default()
    };
    let r = run(o, Entry::RunAsync, ExecutionCommand::Run);
    let row = row("remote-retry");
    let e = r.outcome.value.as_ref().unwrap().as_ref().unwrap_err();
    assert_eq!(
        e.failure().kind,
        marathon_test_support::recorded_errors::kind(row["exception"].as_str().unwrap())
    );
    assert_eq!(e.failure().message.as_ref().unwrap().to_owned(), row["message"]);
    let borrows = |t: &[String]| t.iter().filter(|c| *c == "borrow").count();
    assert_eq!(r.outcome.provider_calls.iter().filter(|c| **c == "borrow").count(), 3);
    let jvm: Vec<String> = serde_json::from_value(row["trace"].clone()).unwrap();
    assert_eq!(borrows(&jvm), 3);
}

/// D53 (A103): the parse's next attempt borrows a device it has not failed on.
#[test]
fn a_parse_that_fails_on_one_device_takes_the_other_next_d53() {
    let o = Options {
        debug: true,
        remote_devices: Some((vec!["a", "b"], vec!["a"])),
        ..Options::default()
    };
    let r = run(o, Entry::RunAsync, ExecutionCommand::Run);
    assert_eq!(r.extracted, ["a", "b"]);
    let parsed: Vec<_> = rust_events(&r.outcome.lines, TIMINGS)
        .into_iter()
        .map(|(_, message, _)| message)
        .filter(|m| m.starts_with("Parsed "))
        .collect();
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    assert!(
        parsed[0].starts_with("Parsed 1 tests in ") && parsed[0].contains(" on b, "),
        "{parsed:?}"
    );
    // The run goes on to wait for a pool's device, which this scenario never connects.
    let e = r.outcome.value.as_ref().unwrap().as_ref().unwrap_err();
    assert!(matches!(e, VendorError::NoDevices(_)), "{e:?}");
}

/// D53: with one device, its three attempts are Kotlin's, and the last failure is the run's.
#[test]
fn a_parse_on_one_device_tries_it_three_times_as_before_d53() {
    let o = Options {
        remote_devices: Some((vec!["a"], vec!["a"])),
        ..Options::default()
    };
    let r = run(o, Entry::RunAsync, ExecutionCommand::Run);
    assert_eq!(r.extracted, ["a", "a", "a"]);
    let e = r.outcome.value.as_ref().unwrap().as_ref().unwrap_err();
    assert_eq!(e.failure().message.as_ref().unwrap().to_owned(), "parse failed on a");
}

/// D53: once every device has been tried, an attempt takes any of them, without waiting.
#[test]
fn a_parse_that_has_tried_every_device_takes_any_again_d53() {
    let o = Options {
        remote_devices: Some((vec!["a", "b"], vec!["a", "b"])),
        ..Options::default()
    };
    let r = run(o, Entry::RunAsync, ExecutionCommand::Run);
    assert_eq!(r.extracted, ["a", "b", "a"]);
    let e = r.outcome.value.as_ref().unwrap().as_ref().unwrap_err();
    assert_eq!(e.failure().message.as_ref().unwrap().to_owned(), "parse failed on a");
}

/// Only the enclosing timeout becomes NoDevices with its own message.
#[test]
fn remote_parsing_timeout_is_no_devices() {
    let o = Options {
        debug: true,
        remote_hang: Some(true),
        device_initialization_timeout_millis: Some(50),
        ..Options::default()
    };
    let r = run(o, Entry::RunAsync, ExecutionCommand::Run);
    let row = row("remote-timeout");
    let e = r.outcome.value.as_ref().unwrap().as_ref().unwrap_err();
    assert!(matches!(e, VendorError::NoDevices(_)));
    assert_eq!(
        e.failure().kind,
        marathon_test_support::recorded_errors::kind(row["exception"].as_str().unwrap())
    );
    assert_eq!(e.failure().message.as_ref().unwrap().to_owned(), row["message"]);
    assert_eq!(r.outcome.completed_at, Some(50));
}

#[test]
fn missing_vendor_services_fail_construction_as_koin_does() {
    for (class, case) in [
        ("com.malinskiy.marathon.execution.TestParser", "koin-missing-parser"),
        (
            "com.malinskiy.marathon.log.MarathonLogConfigurator",
            "koin-missing-log-configurator",
        ),
    ] {
        let failure = marathon_core::construction::missing_definition(class);
        let chain: Vec<Value> = std::iter::successors(Some(&failure), |f| f.cause.as_deref())
            .map(
                |f| serde_json::json!({"class": f.kind.identifier(), "message": f.message.as_ref().unwrap().to_owned()}),
            )
            .collect();
        let mut expected = row(case)["chain"].clone();
        marathon_test_support::recorded_errors::error_chain(&mut expected);
        assert_eq!(Value::Array(chain), expected, "{case}");
    }
    // Through the factory: a vendor without a parser fails before the tracker is built.
    let mut factory = MarathonFactory::new();
    factory.parser = None;
    let result = marathon_core::executor::CoreExecutor::run(move |core| {
        Box::pin(async move {
            let mut factory = factory;
            factory.tests.clear();
            Ok(factory.build_without_parser(&core).await.err())
        })
    })
    .unwrap();
    assert_eq!(
        result,
        Some(marathon_core::construction::missing_definition(
            "com.malinskiy.marathon.execution.TestParser"
        ))
    );
}

/// D52's allowance over the JVM's `signal-*` rows, which stay as the witness: the hook always runs, and the reports
/// come first, with the rest after them in either order, since those run side by side. Nothing else may differ.
fn matches_jvm_finish_d52(jvm: &[&str], native: &[String]) -> bool {
    const ALL: [&str; 3] = ["analytics.close", "terminate", "tracker.close"];
    // With `debug: false` the JVM has no hook and runs none of this: exactly what D52 changes.
    if !jvm.is_empty() && jvm != ALL {
        return false;
    }
    let mut rest: Vec<&str> = native.iter().map(String::as_str).collect();
    if rest.first() != Some(&"tracker.close") {
        return false;
    }
    rest.remove(0);
    rest.sort_unstable();
    rest == ["analytics.close", "terminate"]
}
#[test]
fn the_d52_allowance_covers_only_the_order_and_the_debug_flag() {
    let native = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    let jvm = ["analytics.close", "terminate", "tracker.close"];
    for good in [
        ["tracker.close", "analytics.close", "terminate"],
        ["tracker.close", "terminate", "analytics.close"],
    ] {
        assert!(matches_jvm_finish_d52(&jvm, &native(&good)));
        assert!(matches_jvm_finish_d52(&[], &native(&good)));
    }
    for bad in [
        // The JVM's own order: reports last, what a kill during cleanup would lose.
        &["analytics.close", "terminate", "tracker.close"][..],
        &["terminate", "tracker.close", "analytics.close"],
        // Something missing or done twice.
        &["tracker.close", "terminate"],
        &["tracker.close", "analytics.close"],
        &["analytics.close", "terminate"],
        &["tracker.close", "tracker.close", "analytics.close", "terminate"],
        &["tracker.close", "analytics.close", "terminate", "terminate"],
        // Something new.
        &["tracker.close", "analytics.close", "terminate", "usage.close"],
        &[],
    ] {
        assert!(!matches_jvm_finish_d52(&jvm, &native(bad)), "{bad:?}");
    }
    assert!(
        !matches_jvm_finish_d52(&["terminate"], &native(&jvm)),
        "a JVM row of another shape"
    );
}
fn finish_calls(trace: &[String]) -> Vec<String> {
    trace
        .iter()
        .filter(|c| ["analytics.close", "terminate", "tracker.close"].contains(&c.as_str()))
        .cloned()
        .collect()
}

/// P3: one relay, and both exit as the JVM does. D52: whatever `debug` says, the hook's finish runs first, reports first
/// (C10). The JVM rows stay the witness: with `debug: false` it runs nothing, with `debug: true` its own order.
#[test]
fn signals_always_finish_reports_first_and_exit_like_the_jvm_d52() {
    for (number, name) in [(2, "SIGINT"), (15, "SIGTERM")] {
        for debug in [false, true] {
            let row = row(&format!("signal-{name}-debug-{debug}"));
            let o = Options {
                debug,
                signal: Some(number),
                device_initialization_timeout_millis: Some(600_000),
                ..Options::default()
            };
            let r = run(o, Entry::Run, ExecutionCommand::Run);
            assert_eq!(r.exits.len(), 1, "{name} debug={debug}");
            let (status, trace) = &r.exits[0];
            assert_eq!(
                i64::from(*status),
                row["exitStatus"].as_i64().unwrap(),
                "{name} debug={debug}"
            );
            let jvm: Vec<&str> = row["probe"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap())
                .filter(|p| *p != "ready" && *p != "usage.close")
                .collect();
            let finish = finish_calls(trace);
            assert!(
                matches_jvm_finish_d52(&jvm, &finish),
                "{name} debug={debug}: JVM {jvm:?}, native {finish:?}"
            );
        }
    }
}

/// D52: `cleaned_up` is set first, so the hook used to find nothing to do and the process exited mid-close, with the
/// provider in its 30 s and the reports, which come after it, not written.
#[test]
fn a_signal_during_the_runs_own_finish_writes_the_reports_now_d52() {
    let o = Options {
        block_all: true,
        signal: Some(15),
        signal_at_terminate: true,
        terminate_hangs: true,
        ..Options::default()
    };
    let r = run(o, Entry::Run, ExecutionCommand::Run);
    assert_eq!(r.exits.len(), 1, "{:?}", r.exits);
    let (status, trace) = &r.exits[0];
    assert_eq!(*status, 143);
    assert_eq!(
        finish_calls(trace),
        ["analytics.close", "terminate", "tracker.close"],
        "the run had started the first two, and nothing ran twice"
    );
}

/// D52: however long the provider takes, the process exits at the limit, with the reports written.
#[test]
fn a_provider_that_never_ends_cannot_hold_a_cancel_past_its_limit_d52() {
    let o = Options {
        signal: Some(2),
        terminate_hangs: true,
        device_initialization_timeout_millis: Some(600_000),
        ..Options::default()
    };
    let r = run(o, Entry::Run, ExecutionCommand::Run);
    assert_eq!(r.exits.len(), 1, "{:?}", r.exits);
    let (status, trace) = &r.exits[0];
    assert_eq!(*status, 130);
    assert_eq!(finish_calls(trace)[0], "tracker.close");
    assert!(trace.contains(&"terminate".to_owned()), "{trace:?}");
    assert_eq!(
        r.outcome.completed_at,
        Some(marathon_core::marathon::CANCEL_LIMIT_MILLIS),
        "on the virtual clock, from a signal at 0: the exit releases the provider, and the run ends with it"
    );
}

/// D52: file reports come before a remote backend's close, which can hang. It may not use up the provider's time.
#[test]
fn a_report_close_that_never_ends_does_not_keep_the_provider_from_terminating_d52() {
    let o = Options {
        signal: Some(15),
        reports_hang: true,
        device_initialization_timeout_millis: Some(600_000),
        ..Options::default()
    };
    let r = run(o, Entry::Run, ExecutionCommand::Run);
    assert_eq!(r.exits.len(), 1, "{:?}", r.exits);
    let (status, trace) = &r.exits[0];
    assert_eq!(*status, 143);
    assert_eq!(finish_calls(trace)[0], "tracker.close");
    assert!(trace.contains(&"terminate".to_owned()), "{trace:?}");
    assert!(trace.contains(&"analytics.close".to_owned()), "{trace:?}");
}

/// D52: before the output directory is wiped it holds the last run's reports, and there are no results to write over
/// them. That also keeps a cancelled `parse` from writing reports at all. The provider is still terminated.
#[test]
fn a_signal_before_the_output_directory_is_wiped_writes_no_reports_d52() {
    for debug in [false, true] {
        let o = Options {
            debug,
            prepare: true,
            signal: Some(15),
            signal_at_parse: true,
            ..Options::default()
        };
        let r = run(o, Entry::Run, ExecutionCommand::Run);
        assert_eq!(r.exits.len(), 1, "{:?}", r.exits);
        let (status, trace) = &r.exits[0];
        assert_eq!(*status, 143);
        let mut finish = finish_calls(trace);
        finish.sort();
        assert_eq!(finish, ["analytics.close", "terminate"], "debug={debug}");
        assert!(r.out.join("old.txt").exists(), "the last run's files stay");
    }
}
