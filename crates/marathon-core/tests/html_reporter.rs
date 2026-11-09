//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/html/HtmlSummaryReporter.kt.
mod report_probe_support;

use marathon_core::{
    executor::{
        CoreExecutor,
        hooks::{ClockMode, RuntimeHooks, Tape},
    },
    report::Reporter,
    reporters::html::*,
};
use report_probe_support::*;
fn without_date(s: String) -> String {
    let marker = "by Juno at ";
    let start = s.find(marker).unwrap() + marker.len();
    let end = start + s[start..].find("</div>").unwrap();
    let mut out = s;
    out.replace_range(start..end, "DATE");
    out
}
// D55: the trace's lone surrogate is retained as U+FFFD, including XML output.
fn native_trace_html(text: String) -> String {
    text.replace("\u{ffff}?", "\u{ffff}�")
        .replace("&#159;</div>", "&#159;�</div>")
}
#[test]
fn jvm_html_model_bytes_and_xml11_log() {
    let r = mixed();
    assert_eq!(
        json(&checked(index(r.summary(), "Report <&> 名"))),
        checked(std::fs::read(fixture("index.json")))
    );
    assert_eq!(
        json(&pool_summary(&r.summary().pools[0])),
        checked(std::fs::read(fixture("pool.json")))
    );
    assert_eq!(
        logcat_html(&trace()),
        native_trace_html(oracle("log")["html"].as_str().unwrap().to_owned())
    );
    assert_eq!(logcat_html(&String::default()), String::default());
}
#[test]
fn full_pages_match_and_missing_artifact_directories_are_created() {
    let temp = Temp::new();
    let root = temp.path("");
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let hooks = RuntimeHooks::taped(
                Tape {
                    wall: ClockMode::Frozen(0),
                    ..Tape::default()
                },
                &core.clock(),
            );
            HtmlSummaryReporter::new(
                core.vendor(),
                output(core.vendor(), root.clone()),
                hooks,
                "Report <&> 名".into(),
                root,
            )
            .generate(&mixed())
            .await
        })
    }));
    fn compare(actual: &std::path::Path, expected: &std::path::Path) {
        let names = |dir: &std::path::Path| {
            let mut names: Vec<_> = checked(std::fs::read_dir(dir))
                .map(|e| checked(e).file_name())
                .filter(|n| n != "timeline")
                .collect();
            names.sort();
            names
        };
        assert_eq!(names(actual), names(expected), "page manifest");
        for entry in checked(std::fs::read_dir(actual)) {
            let entry = checked(entry);
            let name = entry.file_name();
            let want = expected.join(&name);
            if checked(entry.file_type()).is_dir() {
                compare(&entry.path(), &want)
            } else {
                let a = checked(std::fs::read(entry.path()));
                let b = checked(std::fs::read(&want));
                if entry.path().extension().is_some_and(|e| e == "html") {
                    assert_eq!(
                        without_date(String::from_utf8(a).unwrap()),
                        without_date(native_trace_html(String::from_utf8(b).unwrap())),
                        "{name:?}"
                    )
                } else {
                    assert_eq!(a, b)
                }
            }
        }
    }
    compare(&temp.0.join("html"), &fixture("html-mixed"));
    assert!(temp.0.join("screenshot/pool/serial-#1").is_dir());
    assert!(temp.0.join("logs/pool/serial-#1").is_dir());
}
#[test]
fn empty_html_does_not_read_clock_or_touch_output() {
    let temp = Temp::new();
    let root = temp.path("absent");
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let hooks = RuntimeHooks::taped(Tape::default(), &core.clock());
            HtmlSummaryReporter::new(
                core.vendor(),
                output(core.vendor(), root.clone()),
                hooks,
                "x".into(),
                root,
            )
            .generate(&empty())
            .await
        })
    }));
    assert!(!temp.0.join("absent").exists());
}
#[test]
fn filename_is_fixed_128_units_independent_of_file_manager_limit() {
    let r = result(&"<&>".repeat(150), marathon_core::model::TestStatus::Passed, 0, 7);
    assert_eq!(filename(&r).encode_utf16().count(), 133);
    let value: serde_json::Value = checked(serde_json::from_slice(&json(&short_test(&r))));
    assert_eq!(value["duration_millis"], 7);
}

#[test]
fn raw_pool_names_overwrites_artifact_gates_and_log_urls() {
    use jcompat::file_manager::{FileManager, FileRequest, FileType};
    use marathon_core::{model::*, report::*};
    let temp = Temp::new();
    let root = temp.path("");
    let manager = FileManager::new(4096, 255, &root.to_string_lossy(), "");
    checked(std::fs::write(temp.0.join("artifact # name.bin"), b"content"));
    let mut events = Vec::new();
    for features in [
        vec![],
        vec![DeviceFeature::Screenshot],
        vec![DeviceFeature::Video],
        vec![DeviceFeature::Screenshot, DeviceFeature::Video],
    ] {
        for present in [false, true] {
            let n = events.len();
            let name = format!("artifacts{n}");
            let mut r = result(&name, TestStatus::Passed, 0, 7);
            r.device.device_features = features.clone();
            r.attachments = ["artifact # name.bin", "missing", "artifact # name.bin"]
                .into_iter()
                .map(|p| Attachment {
                    file: temp.path(p),
                    kind: AttachmentType::Video,
                    name: None,
                })
                .collect();
            if present {
                for kind in [FileType::Screenshot, FileType::Log] {
                    let plan = manager.plan_file(FileRequest {
                        file_type: kind,
                        pool: "pool",
                        serial: "serial #1",
                        test_name: Some(&format!("pkg.Clazz#{name}")),
                        batch_id: Some("batch"),
                        id: None,
                        chunk: None,
                    });
                    let path = checked(plan.materialize(|_| {}));
                    checked(std::fs::write(path.clone(), b"not stacktrace"));
                }
            }
            events.push(event(r, true, "pool"));
        }
    }
    events.push(event(result("overwrite", TestStatus::Passed, 1, 2), true, "pool"));
    events.push(event(result("overwrite", TestStatus::Failure, 3, 4), true, "pool"));
    events.push(event(result("raw", TestStatus::Passed, 0, 1), true, "raw #& pool"));
    let report = ExecutionReport::new(
        ["pool", "raw #& pool"]
            .into_iter()
            .map(|name| DeviceConnectedEvent {
                instant: epoch(0),
                pool_id: DevicePoolId { name: name.into() },
                device: device(),
            })
            .collect(),
        vec![],
        vec![],
        vec![],
        events,
    );
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let hooks = RuntimeHooks::taped(
                Tape {
                    wall: ClockMode::Frozen(0),
                    ..Tape::default()
                },
                &core.clock(),
            );
            HtmlSummaryReporter::new(
                core.vendor(),
                output(core.vendor(), root.clone()),
                hooks,
                "Report <&> 名".into(),
                root,
            )
            .generate(&report)
            .await
        })
    }));
    assert!(temp.0.join("html/pools/raw #& pool.html").exists());
    let artifact_matches = |page: &str, expected: &str| page.contains(&format!("window.test = {expected}"));
    for n in 0..8 {
        let page = checked(std::fs::read_to_string(
            temp.0
                .join(format!("html/pools/pool/serial-#1/pkg.Clazz.artifacts{n}.html")),
        ));
        let expected = checked(std::fs::read_to_string(fixture(&format!("html-artifact-{n}.json"))));
        assert!(artifact_matches(&page, &expected), "artifact case {n}");
        assert!(!page.contains("not stacktrace"));
        if n == 7 {
            // Negative witness: dropping the present screenshot must fail the same comparison.
            let shot: serde_json::Value = checked(serde_json::from_str(&expected));
            let field = format!("\"screenshot\":\"{}\"", shot["screenshot"].as_str().unwrap());
            let missing = page.replacen(&field, "\"screenshot\":\"\"", 1);
            assert_ne!(missing, page);
            assert!(!artifact_matches(&missing, &expected));
        }
    }
    assert_eq!(
        without_date(checked(std::fs::read_to_string(
            temp.0.join("html/pools/pool/serial-#1/pkg.Clazz.overwrite.html")
        ))),
        without_date(checked(std::fs::read_to_string(fixture("overwrite.html"))))
    );
}

#[test]
fn connected_without_tests_and_empty_helper_arithmetic_match_jvm() {
    use marathon_core::report::*;
    let report = ExecutionReport::new(
        vec![DeviceConnectedEvent {
            instant: epoch(0),
            pool_id: marathon_core::model::DevicePoolId { name: "only".into() },
            device: device(),
        }],
        vec![],
        vec![],
        vec![],
        vec![],
    );
    assert_eq!(
        json(&checked(index(report.summary(), "Report <&> 名"))),
        checked(std::fs::read(fixture("index-connected-empty.json")))
    );
    let error = index(empty().summary(), "title").err().unwrap();
    assert_eq!(
        error.failure().to_java_string().to_owned(),
        oracle("empty-average")["error"]
    );
}
