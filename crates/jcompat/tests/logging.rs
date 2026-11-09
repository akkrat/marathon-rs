//! Port of cli/src/main/resources/logback.xml and core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt.

use jcompat::logging::*;
use serde_json::Value;

fn rows(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines().map(|line| serde_json::from_str(line).unwrap())
}

fn level(text: &str) -> Level {
    match text {
        "TRACE" => Level::Trace,
        "DEBUG" => Level::Debug,
        "INFO" => Level::Info,
        "WARN" => Level::Warn,
        "ERROR" => Level::Error,
        _ => panic!("unknown oracle level {text}"),
    }
}

fn trace(value: &Value) -> String {
    trace_with_parent(value, &[])
}

fn trace_with_parent(value: &Value, parent: &[Value]) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut output = format!(
        "{}: {}\n",
        value["class"].as_str().unwrap(),
        value["message"].as_str().unwrap()
    );
    let frames = value["frames"].as_array().unwrap();
    let common = frames
        .iter()
        .rev()
        .zip(parent.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    for frame in &frames[..frames.len() - common] {
        output.push_str(&format!("\tat {}\n", frame.as_str().unwrap()));
    }
    if common > 0 {
        output.push_str(&format!("\t... {common} common frames omitted\n"));
    }
    if !value["cause"].is_null() {
        output.push_str("Caused by: ");
        output.push_str(&trace_with_parent(&value["cause"], frames));
    }
    output
}

#[test]
fn logback_oracle_all_200_rows() {
    let mut count = 0;
    for row in rows(include_str!("../../../fixtures/oracle/jcompat/logback.jsonl")).skip(1) {
        let output = if row["op"] == "abbreviate" {
            abbreviate(
                row["logger"].as_str().unwrap(),
                row["length"].as_u64().unwrap() as usize,
            )
        } else {
            let event = &row["event"];
            let layout = [Layout::Startup, Layout::Runtime, Layout::Compact]
                .into_iter()
                .find(|l| l.pattern() == row["pattern"].as_str().unwrap())
                .unwrap();
            let args: Vec<&str> = event["args"]
                .as_array()
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("null")).collect())
                .unwrap_or_default();
            let message = format_message(event["message"].as_str().unwrap(), &args);
            let throwable = trace(&event["throwable"]);
            layout.format(
                &LogEvent {
                    level: level(event["level"].as_str().unwrap()),
                    logger: event["logger"].as_str().unwrap(),
                    thread: event["thread"].as_str().unwrap(),
                    local_time_millis: event["timeMillis"].as_i64().unwrap(),
                    message: &message,
                    throwable: &throwable,
                },
                JansiMode::Force,
                OutputTarget::Pipe,
            )
        };
        assert_eq!(output, row["output"].as_str().unwrap(), "{row}");
        count += 1;
    }
    assert_eq!(count, 200);
}

#[test]
fn real_console_appenders_terminal_pipe_file_each_jansi_mode() {
    let mut count = 0;
    for row in rows(include_str!("../../../fixtures/oracle/logging/console.jsonl")) {
        let mut logging = MarathonLogging::default();
        match row["layout"].as_str().unwrap() {
            "startup" => (),
            "android" => logging.configure(true, Vendor::Android),
            "apple" => logging.configure(true, Vendor::Apple { compact_output: false }),
            "compact" => logging.configure(true, Vendor::Apple { compact_output: true }),
            _ => unreachable!(),
        }
        let mode = match row["mode"].as_str().unwrap() {
            "default" => JansiMode::Default,
            "force" => JansiMode::Force,
            "strip" => JansiMode::Strip,
            _ => unreachable!(),
        };
        let target = match row["target"].as_str().unwrap() {
            "terminal" => OutputTarget::Terminal,
            "pipe" => OutputTarget::Pipe,
            "file" => OutputTarget::File,
            _ => unreachable!(),
        };
        let logger = logging.logger("com.malinskiy.marathon.execution.queue.QueueActor", Some(Level::Trace));
        let mut output = String::new();
        let mut stream = ConsoleOutput::new(mode, target);
        let mut stream_output = Vec::new();
        for (level, message) in [
            (Level::Trace, "trace"),
            (Level::Debug, "debug"),
            (Level::Info, "info ü"),
            (Level::Warn, "warning"),
            (Level::Info, "embedded \x1b[32mgreen\x1b[0m \x1b]0;title\x07done"),
            (Level::Error, "failed\nsecond line"),
        ] {
            stream_output.extend(stream.format(
                logging.layout(),
                &LogEvent {
                    level,
                    logger: logger.name(),
                    thread: "main",
                    local_time_millis: 0,
                    message,
                    throwable: "",
                },
            ));
            output.push_str(
                &logging
                    .log(&logger, level, "main", 0, (mode, target), || message.to_owned())
                    .unwrap(),
            );
        }
        let logger = logging.timings();
        let message = logging
            .timing_message(|| Some(1234), || "Phase complete".into())
            .unwrap();
        stream_output.extend(stream.format(
            logging.layout(),
            &LogEvent {
                level: Level::Info,
                logger: logger.name(),
                thread: "main",
                local_time_millis: 0,
                message: &message,
                throwable: "",
            },
        ));
        output.push_str(
            &logging
                .log(&logger, Level::Info, "main", 0, (mode, target), || message)
                .unwrap(),
        );
        let logger = logging.xcodebuild();
        let message = logging
            .xcodebuild_message("simulator udid", "destination warning")
            .unwrap();
        stream_output.extend(stream.format(
            logging.layout(),
            &LogEvent {
                level: Level::Warn,
                logger: logger.name(),
                thread: "main",
                local_time_millis: 0,
                message: &message,
                throwable: "",
            },
        ));
        output.push_str(
            &logging
                .log(&logger, Level::Warn, "main", 0, (mode, target), || message)
                .unwrap(),
        );
        stream_output.extend_from_slice(stream.finish(logging.layout()));
        assert_eq!(stream_output, row["output"].as_str().unwrap().as_bytes(), "{row}");
        output.push_str(logging.layout().finish(target));
        assert_eq!(output, row["output"].as_str().unwrap(), "{row}");
        count += 1;
    }
    assert_eq!(count, 36);
}

#[test]
fn real_logger_state_transitions_preserve_c18() {
    let mut logging = MarathonLogging::default();
    let mut old_handle = None;
    let mut count = 0;
    for row in rows(include_str!("../../../fixtures/oracle/logging/state.jsonl")) {
        let name = row["name"].as_str().unwrap();
        match row["op"].as_str().unwrap() {
            "startup" => (),
            "lookup" => {
                if count == 5 {
                    logging.debug = false;
                }
                if count == 8 {
                    logging.debug = true;
                }
                let handle = logging.logger(name, None);
                if name == "early" && old_handle.is_none() {
                    old_handle = Some(handle);
                }
            }
            "configured" => logging.configure(false, Vendor::Android),
            "explicit" => {
                logging.logger(name, Some(Level::Trace));
            }
            "apple" => logging.configure(true, Vendor::Apple { compact_output: true }),
            _ => unreachable!(),
        }
        assert_eq!(
            logging.effective_level(name).as_str(),
            row["level"].as_str().unwrap(),
            "{row}"
        );
        if name == "early" {
            assert_eq!(
                logging.enabled(old_handle.as_ref().unwrap(), Level::Info),
                logging.effective_level(name) <= Level::Info
            );
        }
        count += 1;
    }
    assert_eq!(count, 16);
}

#[test]
fn supplementary_names_long_names_and_slf4j_escaping() {
    let mut count = 0;
    for row in rows(include_str!("../../../fixtures/oracle/logging/details.jsonl")) {
        let output = match row["op"].as_str().unwrap() {
            "abbreviate" => abbreviate(
                row["logger"].as_str().unwrap(),
                row["length"].as_u64().unwrap() as usize,
            ),
            "ansi" => {
                let input = row["input"].as_str().unwrap();
                // Every byte boundary must preserve the same state as a single JVM write.
                for split in 0..=input.len() {
                    let mut stream = JansiStripper::default();
                    let mut bytes = stream.write(&input.as_bytes()[..split]);
                    bytes.extend(stream.write(&input.as_bytes()[split..]));
                    assert_eq!(
                        bytes,
                        row["output"].as_str().unwrap().as_bytes(),
                        "{row}, split {split}"
                    );
                }
                strip_ansi(input)
            }
            "throwable" => trace(&row["throwable"]),
            "message" => {
                let args: Vec<_> = row["arguments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect();
                format_message(row["pattern"].as_str().unwrap(), &args)
            }
            _ => unreachable!(),
        };
        // D55 keeps supplementary logger characters whole instead of the JVM's `?`.
        let expected = if row["op"] == "abbreviate" && row["output"] == "c.?.Foo" {
            "c.😀.Foo"
        } else {
            row["output"].as_str().unwrap()
        };
        assert_eq!(output, expected, "{row}");
        count += 1;
    }
    assert_eq!(count, 117);
}

#[test]
fn disabled_messages_are_lazy_and_special_loggers_are_initialized_once() {
    let mut logging = MarathonLogging::default();
    logging.configure(false, Vendor::Android);
    let logger = logging.logger("late", None);
    assert!(
        logging
            .log(
                &logger,
                Level::Info,
                "main",
                0,
                (JansiMode::Default, OutputTarget::Pipe),
                || panic!("disabled closure")
            )
            .is_none()
    );
    assert!(logging.timing_message(|| None, || "available".into()).is_some());
    logging.logger("Timings", None);
    assert!(
        logging
            .timing_message(
                || panic!("disabled elapsed read"),
                || panic!("timings was reset by name lookup")
            )
            .is_none()
    );
    let calls = std::cell::Cell::new(0);
    let mut enabled = MarathonLogging::default();
    enabled.timing_message(
        || {
            assert_eq!(calls.get(), 1);
            calls.set(2);
            Some(1234)
        },
        || {
            calls.set(1);
            "Phase complete".into()
        },
    );
    assert_eq!(calls.get(), 2);
    assert!(logging.xcodebuild_message("device", "warning").is_some());
    logging.logger("Xcodebuild", None);
    assert!(logging.xcodebuild_message("device", "warning").is_none());
}

#[test]
fn non_logback_fallback_preserves_vararg_array_and_single_warning() {
    let mut logging = MarathonLogging::default();
    let mut count = 0;
    for row in rows(include_str!("../../../fixtures/oracle/logging/fallback.jsonl")) {
        if row["op"] == "warning" {
            logging.debug = row["debug"].as_bool().unwrap();
            assert_eq!(
                logging
                    .fallback_warning(row["backend"].as_str().unwrap())
                    .unwrap_or_default(),
                row["output"].as_str().unwrap()
            );
        } else {
            let args: Vec<_> = row["arguments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let (level, output) = KLoggerDebug::message(
                level(row["inputLevel"].as_str().unwrap()),
                row["pattern"].as_str().unwrap(),
                &args,
                row["varargs"].as_bool().unwrap(),
            );
            assert_eq!(level.as_str(), row["level"].as_str().unwrap());
            assert_eq!(output, row["output"].as_str().unwrap());
        }
        count += 1;
    }
    assert_eq!(count, 13);
}

#[test]
fn generated_hierarchy_and_root_aliases() {
    let mut logging = MarathonLogging::default();
    let old_root = logging.logger("ROOT", Some(Level::Debug));
    for row in rows(include_str!("../../../fixtures/oracle/logging/hierarchy.jsonl")) {
        let handle = logging.logger(
            row["setName"].as_str().unwrap(),
            Some(level(row["setLevel"].as_str().unwrap())),
        );
        assert_eq!(handle.name(), row["handleName"].as_str().unwrap());
        let lookup = row["lookup"].as_str().unwrap();
        let effective = logging.effective_level(lookup);
        assert_eq!(effective.as_str(), row["level"].as_str().unwrap(), "{row}");
        let mut names = MarathonLogging::default();
        let handle = names.logger(lookup, Some(effective));
        assert_eq!(handle.name(), row["name"].as_str().unwrap());
        assert_eq!(
            names
                .log(
                    &handle,
                    Level::Error,
                    "main",
                    0,
                    (JansiMode::Default, OutputTarget::Pipe),
                    || "probe".into()
                )
                .unwrap(),
            row["output"].as_str().unwrap()
        );
        assert_eq!(
            logging.enabled(&old_root, Level::Info),
            logging.effective_level("ROOT") <= Level::Info
        );
    }
}

#[test]
fn jansi_failures_preserve_partial_writes_and_recovery_state() {
    let mut stream = JansiStripper::default();
    let mut previous = String::new();
    let mut output = Vec::new();
    for row in rows(include_str!("../../../fixtures/oracle/logging/failures.jsonl")) {
        let sequence = row["sequence"].as_str().unwrap();
        if sequence != previous {
            stream = JansiStripper::default();
            output.clear();
            previous = sequence.into();
        }
        output.extend(stream.write(row["input"].as_str().unwrap().as_bytes()));
        assert_eq!(output, row["output"].as_str().unwrap().as_bytes(), "{row}");
        let failure = match row["error"].as_str().map(marathon_test_support::recorded_errors::kind) {
            Some(marathon_error::FailureKind::NumberFormat) => Some(JansiFailure::NumberOverflow),
            Some(marathon_error::FailureKind::ClassCast) => Some(JansiFailure::InvalidOption),
            None => None,
            _ => unreachable!(),
        };
        assert_eq!(stream.failure(), failure, "{row}");
    }
}

#[test]
fn failing_console_events_all_targets_and_modes() {
    for row in rows(include_str!("../../../fixtures/oracle/logging/failure-console.jsonl")) {
        let mode = match row["mode"].as_str().unwrap() {
            "force" => JansiMode::Force,
            "strip" => JansiMode::Strip,
            _ => JansiMode::Default,
        };
        let target = match row["target"].as_str().unwrap() {
            "terminal" => OutputTarget::Terminal,
            "file" => OutputTarget::File,
            _ => OutputTarget::Pipe,
        };
        let sequence = match row["layout"].as_str().unwrap() {
            "failure-m" => "\x1b[9999999999999m",
            "failure-G" => "\x1b[2147483648G",
            _ => "\x1b[?J",
        };
        let mut stream = ConsoleOutput::new(mode, target);
        let mut output = Vec::new();
        for message in [
            format!("before{sequence}after"),
            "next event".into(),
            "third event".into(),
        ] {
            output.extend(stream.format(
                Layout::Compact,
                &LogEvent {
                    level: Level::Error,
                    logger: "probe",
                    thread: "main",
                    local_time_millis: 0,
                    message: &message,
                    throwable: "",
                },
            ));
        }
        let shutdown_failed =
            target == OutputTarget::Terminal && stream.failure() == Some(JansiFailure::NumberOverflow);
        assert_eq!(i64::from(shutdown_failed), row["exitCode"].as_i64().unwrap());
        output.extend_from_slice(stream.finish(Layout::Compact));
        assert_eq!(output, row["output"].as_str().unwrap().as_bytes(), "{row}");
    }
}

#[test]
fn abbreviation_keeps_supplementary_characters_whole() {
    assert_eq!(
        abbreviate("a.😀bcdefghijklmnop.qrstuvwxyz0123456789", 36),
        "a.😀.qrstuvwxyz0123456789"
    );
}
