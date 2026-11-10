//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt (JVM evidence for the run flow).
//!
//! Every scenario input is compared with the reference build's replay transcript
//! (fixtures/oracle/run-flow/scenarios.jsonl, tools/oracle/run-flow-probes): the outcome and
//! its scheduler time, every event and device call with its time, batch and tests, raw.json
//! byte for byte, ordered YAML data for parse output files, and the outputDir manifest. Batch UUIDs are renamed
//! by their execute call's (time, device); calls at one scheduler time are compared as a set,
//! since devices run concurrently on the vendor runtime. The Allure and HTML reporter outputs
//! are compared as text with random UUIDs, wall-clock dates, the temporary outputDir and
//! identity hashes masked.
use marathon_test_support::scenario;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

/// Results the DeviceActor completes itself (lost devices) carry `timer.currentTimeMillis`,
/// the host wall clock; only those 13-digit epoch values are replaced.
fn wall_times(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find("\"timestamp\":") {
        let (head, tail) = rest.split_at(at + "\"timestamp\":".len());
        out.push_str(head);
        let digits = tail.chars().take_while(char::is_ascii_digit).count();
        if digits >= 13 {
            out.push_str("<wall>");
        } else {
            out.push_str(&tail[..digits]);
        }
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out
}

fn rows() -> Vec<Value> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/oracle/run-flow/scenarios.jsonl"
    );
    let text = std::fs::read_to_string(path).unwrap();
    text.lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn normalize(log: &[Value]) -> Vec<Value> {
    let mut batches: Vec<(i64, String, String)> = log
        .iter()
        .filter(|e| e["call"] == "execute")
        .map(|e| {
            (
                e["t"].as_i64().unwrap(),
                e["device"].as_str().unwrap().to_owned(),
                e["batch"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    batches.sort();
    let names: BTreeMap<String, String> = batches
        .into_iter()
        .enumerate()
        .map(|(i, (_, _, id))| (id, format!("batch-{}", i + 1)))
        .collect();
    let mut entries: Vec<(i64, String)> = log
        .iter()
        .map(|e| {
            let mut e = e.clone();
            if let Some(id) = e["batch"].as_str() {
                e["batch"] = names.get(id).cloned().unwrap_or_else(|| id.to_owned()).into();
            }
            (e["t"].as_i64().unwrap(), e.to_string())
        })
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|(_, e)| serde_json::from_str(&e).unwrap())
        .collect()
}

fn manifest(root: &Path) -> Option<Vec<String>> {
    if !root.is_dir() {
        return None;
    }
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        for path in entries {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                out.push(format!("{rel}/"));
                walk(root, &path, out);
            } else {
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    Some(out)
}

/// Allure names its result files by random UUID: replace each with `<uuid>`.
fn uuids(entries: &[String]) -> Vec<String> {
    let mut out: Vec<String> = entries.iter().map(|e| mask_uuids(e)).collect();
    out.sort();
    out
}

fn mask_uuids(text: &str) -> String {
    let mut text = text.to_owned();
    while let Some(at) = uuid_at(&text) {
        text.replace_range(at..at + 36, "<uuid>");
    }
    text
}

/// Replaces what follows each `start` up to the next `end` with `with`.
fn mask_between(text: &str, start: &str, end: &str, with: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(start) {
        let (head, tail) = rest.split_at(at + start.len());
        out.push_str(head);
        out.push_str(with);
        rest = tail.find(end).map_or("", |stop| &tail[stop..]);
    }
    out.push_str(rest);
    out
}

/// Allure/HTML text: random UUIDs, the HTML footer's wall-clock date, the epoch-millisecond
/// times of lost devices' results (as in raw.json), and the outputDir path in environment.xml
/// (both runs use their own temporary directory). Only those
/// fields are masked; any other text compares literally.
fn mask_report(path: &str, text: &str) -> String {
    let text = mask_uuids(text);
    let text = mask_between(&text, "by Juno at ", "</div>", "<date>");
    let text = EPOCH_FIELDS
        .iter()
        .fold(text, |text, field| epoch_after(&text, &format!("\"{field}\":")));
    if !path.ends_with("allure-results/environment.xml") {
        return text;
    }
    mask_between(&text, "<key>outputDir</key><value>", "</value>", "<outputDir>")
}

/// D58 adapts only historical configuration identities; actual output stays exact.
fn native_environment(text: &str) -> String {
    jcompat::JavaRegex::compile(
        r"(?:[A-Za-z0-9_]+[.$])+([A-Za-z0-9_]+)@[0-9a-f]+",
        "historical config objects",
    )
    .expect("supported regex")
    .replace(text, "$1")
    .expect("capture replacement")
}
/// Allure result `start`/`stop` and HTML `startDate`/`endDate`: a lost device's result carries
/// the host wall clock there.
const EPOCH_FIELDS: [&str; 4] = ["start", "stop", "startDate", "endDate"];

/// Replaces a 13-or-more-digit number right after each `field`.
fn epoch_after(text: &str, field: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(field) {
        let (head, tail) = rest.split_at(at + field.len());
        out.push_str(head);
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        out.push_str(if digits >= 13 { "<wall>" } else { &tail[..digits] });
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out
}

fn reporter_files(root: &Path, entries: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = entries
        .iter()
        .filter(|e| (e.starts_with("allure-results/") || e.starts_with("html/")) && !e.ends_with('/'))
        .filter(|e| !STATIC.contains(&e.as_str()))
        .map(|e| {
            (
                mask_uuids(e),
                mask_report(e, &std::fs::read_to_string(root.join(e)).unwrap()),
            )
        })
        .collect();
    out.sort();
    out
}

/// Copied unchanged from the reporter resources (pinned by WP 3.9c), as the capture skips them.
const STATIC: [&str; 6] = [
    "html/app.min.css",
    "html/app.min.js",
    "html/app.min.js.LICENSE.txt",
    "html/timeline/chart.css",
    "html/timeline/chart.js",
    "html/timeline/iframeResizer.contentWindow.min.js",
];

fn uuid_at(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(35)).find(|&at| {
        bytes[at..at + 36].iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
    })
}

fn outcome(outcome: &scenario::Outcome) -> Value {
    let mut value = serde_json::json!({"completed": outcome.completed});
    match &outcome.value {
        Some(Ok(v)) => value["value"] = (*v).into(),
        Some(Err(e)) => {
            value["exception"] = e.failure().kind.identifier().into();
            value["message"] = e.failure().message.as_ref().map(|m| m.to_owned()).into();
        }
        None => {}
    }
    value["completedAt"] = outcome.completed_at.into();
    value
}

/// D48: the wait a pool that lost its last device now makes, in the two rows where it does, `deviceInitializationTimeoutMillis`
/// of `MarathonFactory`. These rows stay the witness that Kotlin ended the run at once.
const D48_WAIT: i64 = 60_000;

/// D48's allowance over a row's outcome: nothing differs but `completedAt`, later by exactly the wait.
fn d48_outcome(actual: &Value, jvm: &Value) -> Result<(), String> {
    let mut moved = actual.clone();
    let Some(at) = actual["completedAt"].as_i64() else {
        return Err("the run did not complete".into());
    };
    moved["completedAt"] = (at - D48_WAIT).into();
    if moved == *jvm {
        Ok(())
    } else {
        Err(format!("{actual} is not {jvm} with the wait of D48 added"))
    }
}

/// D48's allowance over a row's log, the calls devices got: only the driver's own `end` differs, by the wait the test added.
fn d48_log(actual: &[Value], jvm: &[Value]) -> Vec<Value> {
    let end = |log: &[Value]| {
        log.iter().find(|entry| entry["event"] == "end").unwrap()["t"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(end(actual), end(jvm) + D48_WAIT);
    actual
        .iter()
        .map(|entry| {
            let mut entry = entry.clone();
            if entry["event"] == "end" {
                entry["t"] = end(jvm).into();
            }
            entry
        })
        .collect()
}

fn check(name: &str) {
    check_with(name, false);
}

fn check_with(name: &str, d48: bool) {
    let mut row = rows().into_iter().find(|r| r["scenario"] == name).unwrap();
    if let Some(exception) = row["outcome"].get_mut("exception") {
        *exception = marathon_test_support::recorded_errors::scenario_kind(exception.as_str().unwrap())
            .identifier()
            .into();
    }
    for entry in row["log"].as_array_mut().unwrap() {
        if let Some(exception) = entry.get_mut("exception") {
            *exception = marathon_test_support::recorded_errors::scenario_kind(exception.as_str().unwrap())
                .identifier()
                .into();
        }
    }
    let rust = if d48 {
        scenario::run_longer(&format!("{name}.json"), D48_WAIT, |_| {})
    } else {
        scenario::run(&format!("{name}.json"), |_| {})
    };
    let jvm_log: Vec<Value> = row["log"].as_array().unwrap().clone();
    let log = if d48 {
        d48_outcome(&outcome(&rust), &row["outcome"]).unwrap();
        d48_log(&rust.log, &jvm_log)
    } else {
        assert_eq!(outcome(&rust), row["outcome"], "{name}: outcome");
        rust.log.clone()
    };
    assert_eq!(normalize(&log), normalize(&jvm_log), "{name}: log");
    let raw = rust.output_dir.join("test_result/raw.json");
    let raw = raw.is_file().then(|| std::fs::read_to_string(raw).unwrap());
    assert_eq!(
        raw.as_deref().map(wall_times),
        row["rawJson"].as_str().map(wall_times),
        "{name}: raw.json"
    );
    for (file, content) in row["files"].as_object().unwrap() {
        if file.ends_with(".yml") {
            if let Err(error) =
                marathon_test_support::yaml::same_data(&rust.read(file), content.as_str().expect("oracle YAML"))
            {
                panic!("{name}: {file}: {error}");
            }
        } else {
            assert_eq!(rust.read(file), content.as_str().unwrap(), "{name}: {file}");
        }
    }
    let jvm_manifest: Option<Vec<String>> = row["manifest"]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_owned()).collect());
    let rust_manifest = manifest(&rust.output_dir);
    assert_eq!(
        rust_manifest.as_deref().map(uuids),
        jvm_manifest.as_deref().map(uuids),
        "{name}: manifest"
    );
    let mut jvm_reports: Vec<(String, String)> = row["reporterFiles"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(path, text)| {
            (
                mask_uuids(path),
                mask_report(
                    path,
                    &if path.ends_with("allure-results/environment.xml") {
                        native_environment(text.as_str().unwrap())
                    } else {
                        text.as_str().unwrap().to_owned()
                    },
                ),
            )
        })
        .collect();
    jvm_reports.sort();
    let rust_reports = rust_manifest.map_or_else(Vec::new, |m| reporter_files(&rust.output_dir, &m));
    assert_eq!(rust_reports.len(), jvm_reports.len(), "{name}: reporter files");
    for (rust, jvm) in rust_reports.iter().zip(&jvm_reports) {
        assert_eq!(rust, jvm, "{name}: {}", jvm.0);
    }
}

#[test]
fn success_matches_the_jvm_transcript() {
    check("success");
}
#[test]
fn parse_matches_the_jvm_transcript() {
    check("parse");
}
#[test]
fn filtering_1_matches_the_jvm_transcript() {
    check("filtering-1");
}
#[test]
fn filtering_2_matches_the_jvm_transcript() {
    check("filtering-2");
}
#[test]
fn filtering_3_matches_the_jvm_transcript() {
    check("filtering-3");
}
#[test]
fn disconnect_matches_the_jvm_transcript() {
    check("disconnect");
}
#[test]
fn all_lost_matches_the_jvm_transcript() {
    check_with("all-lost", true);
}
#[test]
fn invalid_config_matches_the_jvm_transcript() {
    check("invalid-config");
}
#[test]
fn two_pools_disconnect_matches_the_jvm_transcript() {
    check_with("two-pools-disconnect", true);
}
#[test]
fn the_d48_allowance_hides_nothing_else() {
    let jvm = serde_json::json!({"completed": true, "value": false, "completedAt": 16000});
    let at = |completed: i64| serde_json::json!({"completed": true, "value": false, "completedAt": completed});
    assert!(d48_outcome(&at(16000 + D48_WAIT), &jvm).is_ok());
    // Not at once, not a moment off.
    assert!(d48_outcome(&at(16000), &jvm).is_err());
    assert!(d48_outcome(&at(16000 + D48_WAIT + 1), &jvm).is_err());
    // A different answer stays a difference.
    let other = serde_json::json!({"completed": true, "value": true, "completedAt": 16000 + D48_WAIT});
    assert!(d48_outcome(&other, &jvm).is_err());
    let hung = serde_json::json!({"completed": false, "value": false});
    assert!(d48_outcome(&hung, &jvm).is_err());
    // The log allowance moves only the driver's end, and only by the wait.
    let log = |t: i64, end: i64| {
        vec![
            serde_json::json!({"t": t, "device": "d", "call": "execute"}),
            serde_json::json!({"t": end, "event": "end"}),
        ]
    };
    assert_eq!(d48_log(&log(1, 2 + D48_WAIT), &log(1, 2)), log(1, 2));
    assert_ne!(d48_log(&log(5, 2 + D48_WAIT), &log(1, 2)), log(1, 2));
    assert!(std::panic::catch_unwind(|| d48_log(&log(1, 3), &log(1, 2))).is_err());
}
#[test]
fn uncompleted_1_matches_the_jvm_transcript() {
    check("uncompleted-1");
}
#[test]
fn uncompleted_2_matches_the_jvm_transcript() {
    check("uncompleted-2");
}
#[test]
fn uncompleted_3_matches_the_jvm_transcript() {
    check("uncompleted-3");
}
#[test]
fn uncompleted_4_matches_the_jvm_transcript() {
    check("uncompleted-4");
}
/// Scripted device events (`prepare-done`, `batch-result`) and `entry: run`, against the patched
/// build's taped transcript.
#[test]
fn scripted_matches_the_jvm_transcript() {
    check("scripted");
}
/// `prepare-failed` and `batch-failed` (TestBatchExecutionException after empty results).
#[test]
fn scripted_failures_match_the_jvm_transcript() {
    check("scripted-failures");
}

#[test]
fn report_masks_keep_ordinary_data_literal() {
    let env = "allure-results/environment.xml";
    // A `@hex` suffix outside the identity keys and in other files is data.
    assert_ne!(
        mask_report(env, "<key>name</key><value>contact.foo@deadbeef</value>"),
        mask_report(env, "<key>name</key><value>contact.foo@cafebabe</value>")
    );
    assert_ne!(
        mask_report("html/index.html", "contact.foo@deadbeef</value>"),
        mask_report("html/index.html", "contact.foo@cafebabe</value>")
    );
    assert_ne!(
        mask_report(env, "<key>pooling</key><value>Omni@1a2b</value>"),
        mask_report(env, "<key>pooling</key><value>Omni@3c4d</value>")
    );
    // Long digit runs outside the epoch fields are data, too.
    assert_ne!(
        mask_report("html/index.html", "\"name\":\"test1234567890123\""),
        mask_report("html/index.html", "\"name\":\"test9876543210987\"")
    );
    assert_eq!(
        mask_report("html/index.html", "\"startDate\":1700000000000,"),
        "\"startDate\":<wall>,"
    );
    assert_eq!(mask_report("html/index.html", "\"startDate\":12,"), "\"startDate\":12,");
    // The outputDir path is masked in environment.xml only.
    assert_eq!(
        mask_report("html/index.html", "<key>outputDir</key><value>/a</value>"),
        "<key>outputDir</key><value>/a</value>"
    );
}
