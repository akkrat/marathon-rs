//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/allure/AllureReporter.kt.
mod report_probe_support;
use jcompat::report_text::allure_history_id;
use marathon_core::{
    artifacts::LocalAttachmentFiles,
    executor::{
        CoreExecutor,
        hooks::{ClockMode, Ids, RuntimeHooks, Tape},
    },
    model::*,
    report::Reporter,
    reporters::allure::*,
};
use report_probe_support::*;
use std::sync::{Arc, Mutex};
fn model_json(value: &AllureResult) -> String {
    checked(serde_json::to_string(value))
}
#[test]
fn all_statuses_attempts_defaults_and_package_collision_match_library() {
    let root = std::path::PathBuf::from(include_str!(
        "../../../fixtures/oracle/html-allure-gif/runtime-root.txt"
    ));
    let cfg = config(&root);
    for (i, e) in mixed().test_events().iter().enumerate() {
        let uuid: String = format!("00000000-0000-0000-0000-{i:012}");
        if i == 0 {
            let expected = checked(std::fs::read(fixture(
                "allure/00000000-0000-0000-0000-000000000000-result.json",
            )));
            // D55 changes the single escaped lone surrogate in this trace.
            let expected = String::from_utf8(expected)
                .unwrap()
                .replace("\\uD800", "�")
                .into_bytes();
            let expected: serde_json::Value = checked(serde_json::from_slice(&expected));
            assert_eq!(
                model_json(&result_model(&uuid, &e.device, &e.test_result, &[], &cfg, &root)),
                checked(serde_json::to_string(&expected))
            );
            continue;
        }
        let expected: serde_json::Value = checked(serde_json::from_slice(&checked(std::fs::read(fixture(&format!(
            "allure/{}-result.json",
            uuid.to_owned()
        ))))));
        assert_eq!(
            model_json(&result_model(&uuid, &e.device, &e.test_result, &[], &cfg, &root)),
            checked(serde_json::to_string(&expected))
        );
    }
    assert_eq!(allure_history_id("Clazz", "test1"), oracle("history")["hash"]);
}
fn annotations() -> Vec<MetaProperty> {
    [
        ("io.qameta.allure.Description", MetaValue::String("java".into())),
        (
            "io.qameta.allure.kotlin.Description",
            MetaValue::String("kotlin".into()),
        ),
        ("io.qameta.allure.Issue", MetaValue::String("issue".into())),
        ("io.qameta.allure.TmsLink", MetaValue::String("tms".into())),
        ("io.qameta.allure.kotlin.Issue", MetaValue::String("ki".into())),
        ("io.qameta.allure.kotlin.TmsLink", MetaValue::String("kt".into())),
        ("io.qameta.allure.Epic", MetaValue::Int(3)),
        ("io.qameta.allure.Epic", MetaValue::String("ignored second".into())),
        ("io.qameta.allure.Feature", MetaValue::String("feature".into())),
        ("io.qameta.allure.Severity", MetaValue::String("critical".into())),
        ("io.qameta.allure.kotlin.Owner", MetaValue::String("owner".into())),
    ]
    .into_iter()
    .map(|(name, value)| {
        let mut values = MetaMap::default();
        values.insert("value".into(), value);
        MetaProperty {
            name: name.into(),
            values,
        }
    })
    .collect()
}
#[test]
fn annotation_casts_duplicate_links_and_all_attachment_types_match_library() {
    let root = std::path::PathBuf::from(include_str!(
        "../../../fixtures/oracle/html-allure-gif/runtime-root.txt"
    ));
    let cfg = config(&root);
    let path = jcompat::file_manager::child_file(&root, "artifact # name.bin");
    let mut attachments: Vec<_> = [
        AttachmentType::ScreenshotGif,
        AttachmentType::ScreenshotJpeg,
        AttachmentType::ScreenshotPng,
        AttachmentType::ScreenshotWebp,
        AttachmentType::Video,
        AttachmentType::Log,
        AttachmentType::Profiling,
    ]
    .into_iter()
    .map(|kind| Attachment {
        file: path.clone(),
        kind,
        name: None,
    })
    .collect();
    attachments.push(Attachment {
        file: path,
        kind: AttachmentType::Log,
        name: Some("named".into()),
    });
    let mut r = result("annotations", TestStatus::Passed, 1, 2);
    r.stacktrace = Some("".into());
    r.test.meta_properties = annotations();
    let actual = model_json(&result_model(
        "00000000-0000-0000-0000-000000000008",
        &device(),
        &r,
        &attachments,
        &cfg,
        &root,
    ));
    let expected: serde_json::Value = checked(serde_json::from_slice(&checked(std::fs::read(fixture(
        "allure/00000000-0000-0000-0000-000000000008-result.json",
    )))));
    assert_eq!(actual, checked(serde_json::to_string(&expected)));
}
#[test]
fn environment_xml_preserves_entry_order_and_values() {
    let text = checked(environment_xml(&[
        ("first".into(), "<&> 名\n\"'".into()),
        ("second".into(), "two".into()),
    ]));
    assert_eq!(
        text.as_bytes().to_vec(),
        checked(std::fs::read(fixture("environment/environment.xml")))
    );
}
#[test]
fn empty_run_writes_complete_environment_and_attempts_use_uuid_tape() {
    for report in [empty(), mixed()] {
        let temp = Temp::new();
        let root = temp.path("");
        let cfg = Arc::new(config(&root));
        let entries = cfg.to_map(&root);
        let count = report.test_events().len();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let es = errors.clone();
        let stdout = Arc::new(Console::default());
        let console = stdout.clone();
        checked(CoreExecutor::run(move |core| {
            Box::pin(async move {
                let mut tape = Tape {
                    wall: ClockMode::Frozen(0),
                    ..Tape::default()
                };
                tape.ids.insert(
                    "core.AllureReporter.uuid".into(),
                    Ids::Pattern {
                        prefix: "uuid-".into(),
                        width: 1,
                        hex: false,
                        suffix: "".into(),
                        start: 0,
                    },
                );
                let hooks = RuntimeHooks::taped(tape, &core.clock());
                let reporter = AllureReporter::new(
                    core.vendor(),
                    cfg,
                    root.clone(),
                    root,
                    hooks,
                    Arc::new(LocalAttachmentFiles { io: core.vendor() }),
                    Arc::new(move |f| es.lock().unwrap().push(f.clone())),
                    console,
                );
                reporter.generate(&report).await
            })
        }));
        assert!(errors.lock().unwrap().is_empty());
        // The JVM's AllureReporter.generate(empty) prints environment-writer's line.
        assert_eq!(jvm_stdout_after("empty-allure"), ["Allure environment data saved."]);
        assert_eq!(*stdout.0.lock().unwrap(), [String::from(ENVIRONMENT_SAVED)]);
        assert_eq!(
            checked(std::fs::read(temp.0.join("environment.xml"))),
            checked(environment_xml(&entries)).as_bytes().to_vec()
        );
        assert_eq!(checked(std::fs::read_dir(&temp.0)).count(), count + 1);
    }
}
#[test]
fn environment_failure_is_caught_but_result_write_failure_propagates() {
    let temp = Temp::new();
    checked(std::fs::write(temp.0.join("blocked"), b"file"));
    let path = temp.path("blocked");
    let cfg = Arc::new(config(&temp.path("")));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let es = errors.clone();
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let hooks = RuntimeHooks::live();
            let reporter = AllureReporter::new(
                core.vendor(),
                cfg,
                path.clone(),
                path,
                hooks,
                Arc::new(LocalAttachmentFiles { io: core.vendor() }),
                Arc::new(move |f| es.lock().unwrap().push(f.clone())),
                Arc::new(Console::default()),
            );
            reporter.generate(&empty()).await?;
            assert!(matches!(
                reporter.generate(&mixed()).await,
                Err(marathon_core::error::OperationError::Failed(_))
            ));
            Ok(())
        })
    }));
    assert_eq!(errors.lock().unwrap().len(), 1);
}
/// Replaces the JVM identity hash in `Class@1a2b3c</value>` with a fixed token (F29).
fn without_identities(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(at) = rest.find('@') {
        let hex = rest[at + 1..].chars().take_while(char::is_ascii_hexdigit).count();
        out.push_str(&rest[..at]);
        if hex > 0 && rest[at + 1 + hex..].starts_with("</value>") {
            out.push_str("@ID");
        } else {
            out.push_str(&rest[at..at + 1 + hex]);
        }
        rest = &rest[at + 1 + hex..];
    }
    out.push_str(rest);
    out
}
#[test]
fn complete_environment_matches_jvm_keys_order_and_values() {
    let root = std::path::PathBuf::from(include_str!(
        "../../../fixtures/oracle/html-allure-gif/runtime-root.txt"
    ));
    let entries = config(&root).to_map(&root);
    let actual = checked(environment_xml(&entries)).to_owned();
    let expected = checked(std::fs::read_to_string(fixture(
        "manifest/allure-empty/environment.xml",
    )));
    assert_eq!(entries.len(), 23);
    assert!(expected.matches("@").count() >= 7);
    assert_eq!(without_identities(&actual), without_identities(&expected));
}
/// The values written by the JVM `environment-case-*` rows.
fn environment_cases() -> Vec<String> {
    vec![
        "a\u{1}b".into(),
        String::from_utf16_lossy(
            &"\u{7f}\u{85}\u{9f}\t😀\r\n\u{fffe}\u{ffff}<&>\"'"
                .encode_utf16()
                .collect::<Vec<_>>(),
        ),
        String::from_utf16_lossy(&"trailing".encode_utf16().chain([0xd800]).collect::<Vec<_>>()),
        String::from_utf16_lossy(&[0x78, 0xd800, 0x79]),
        String::from_utf16_lossy(&[0xdc00]),
    ]
}
#[test]
fn invalid_environment_characters_leave_empty_file_and_report_transformer_exception() {
    for (i, value) in environment_cases().into_iter().enumerate() {
        let id = format!("environment-case-{i}");
        let row = oracle(&id);
        let expected = checked(std::fs::read(fixture(&format!(
            "environment-cases/{i}/environment.xml"
        ))));
        assert_eq!(row["size"], expected.len());
        let single = environment_xml(&[("key".into(), value.clone())]);
        // The configuration name is the first to_map entry, so the reporter sees the same text node.
        let temp = Temp::new();
        let root = temp.path("");
        let mut cfg = config(&root);
        cfg.name = value;
        let errors = Arc::new(Mutex::new(Vec::new()));
        let (es, stdout) = (errors.clone(), Arc::new(Console::default()));
        let console = stdout.clone();
        checked(CoreExecutor::run(move |core| {
            Box::pin(async move {
                AllureReporter::new(
                    core.vendor(),
                    Arc::new(cfg),
                    root.clone(),
                    root,
                    RuntimeHooks::live(),
                    Arc::new(LocalAttachmentFiles { io: core.vendor() }),
                    Arc::new(move |f| es.lock().unwrap().push(f.clone())),
                    console,
                )
                .generate(&empty())
                .await
            })
        }));
        let written = checked(std::fs::read(temp.0.join("environment.xml")));
        if i >= 2 {
            // D55: malformed input units have already become XML-safe U+FFFD.
            let value = ["trailing�", "x�y", "�"][i - 2];
            let single = checked(single);
            assert!(single.contains(&format!("<value>{value}</value>")));
            assert!(
                std::str::from_utf8(&written)
                    .unwrap()
                    .contains(&format!("<value>{value}</value>"))
            );
            assert!(errors.lock().unwrap().is_empty());
            assert_eq!(stdout.0.lock().unwrap().len(), 1);
            continue;
        }
        let headers: Vec<_> = row["stderrHeaders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h.as_str().unwrap().to_owned())
            .collect();
        let saved = jvm_stdout_after(&id) == ["Allure environment data saved."];
        if headers.is_empty() {
            assert!(saved, "{id}");
            assert_eq!(checked(single).as_bytes().to_vec(), expected, "{id}");
            assert!(errors.lock().unwrap().is_empty());
            assert_eq!(stdout.0.lock().unwrap().len(), 1);
            assert!(!written.is_empty());
        } else {
            assert!(!saved && jvm_stdout_after(&id).is_empty(), "{id}");
            assert!(expected.is_empty() && written.is_empty() && single.is_err(), "{id}");
            assert!(stdout.0.lock().unwrap().is_empty());
            let errors = errors.lock().unwrap();
            assert_eq!(errors.len(), 1);
            // Each Failure in the cause chain is one header line printStackTrace emits.
            let mut failure = Some(&errors[0]);
            let mut depth = 0;
            while let Some(f) = failure {
                let line = f.to_java_string().to_owned();
                let line = if depth == 0 { line } else { format!("Caused by: {line}") };
                assert!(headers.contains(&line), "{id}: {line} not in {headers:?}");
                failure = f.cause.as_deref();
                depth += 1;
            }
            assert_eq!(depth, if i == 3 { 3 } else { 2 }, "{id}");
        }
    }
}

#[test]
fn real_registration_failure_does_not_stop_later_html() {
    use jcompat::logging::MarathonLogging;
    use marathon_core::{
        analytics::{ExecutionReportGenerator, TrackerInternal},
        report::{DeviceConnectedEvent, Event},
        reporters::{html::HtmlSummaryReporter, timeline::TimelineReporter},
    };
    let temp = Temp::new();
    checked(std::fs::write(temp.0.join("blocked"), b"file"));
    let root = temp.path("");
    let blocked = temp.path("blocked");
    let cfg = Arc::new(config(&root));
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let hooks = RuntimeHooks::live();
            let mut registrations = vec![
                HtmlSummaryReporter::new(
                    core.vendor(),
                    output(core.vendor(), root.clone()),
                    hooks.clone(),
                    "Report <&> 名".into(),
                    root.clone(),
                )
                .registration(),
                AllureReporter::new(
                    core.vendor(),
                    cfg,
                    blocked,
                    root.clone(),
                    hooks,
                    Arc::new(LocalAttachmentFiles { io: core.vendor() }),
                    Arc::new(|_| {}),
                    Arc::new(Console::default()),
                )
                .registration(),
                TimelineReporter::new(core.vendor(), root, &mut MarathonLogging::default()).registration(),
            ];
            registrations.sort_by_key(|r| r.slot);
            let generator =
                ExecutionReportGenerator::new(registrations.into_iter().map(|r| r.reporter.clone()).collect());
            let io = core.vendor();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let task = io.spawn(Box::pin(async move {
                checked(
                    generator
                        .track(&Event::DeviceConnected(DeviceConnectedEvent {
                            instant: epoch(0),
                            pool_id: DevicePoolId { name: "pool".into() },
                            device: device(),
                        }))
                        .await,
                );
                checked(
                    generator
                        .track(&Event::Test(event(result("x", TestStatus::Passed, 1, 2), true, "pool")))
                        .await,
                );
                let result = generator.close().await;
                let _ = tx.send(result);
                Ok(())
            }));
            checked(task.join().await);
            let error = checked(rx.await).err().unwrap();
            match error {
                marathon_core::error::OperationError::Failed(f) => {
                    assert_eq!(f.to_java_string().to_owned(), oracle("allure-write-failure")["error"])
                }
                _ => panic!("wrong error"),
            };
            Ok(())
        })
    }));
    assert!(temp.0.join("html/timeline/index.html").is_file());
    // D51: the JVM row stops at Allure (C11). Html now runs, and the error is still Allure's.
    assert!(temp.0.join("html/index.html").exists());
}

#[test]
fn profiling_links_use_form_encoding_and_replace_every_placeholder() {
    use marathon_config::{
        bind_vendor::bind_vendor_configuration,
        binding::{BindingContext, Profile},
        pipeline::bind_bytes,
    };
    let root = std::path::PathBuf::from(include_str!(
        "../../../fixtures/oracle/html-allure-gif/runtime-root.txt"
    ));
    let mut cfg = config(&root);
    let ctx = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: root.clone(),
        analytics_tracking: false,
        bugsnag_reporting: false,
        marathonfile_dir: root.clone(),
        process_dir: root.clone(),
        environment: vec![],
        now: epoch(0),
    };
    cfg.vendor_configuration = checked(bind_bytes(
        b"type: Android\nprofilingConfiguration:\n  urlTemplate: 'https://trace.invalid/{}?copy={}'",
        &ctx,
        bind_vendor_configuration,
    ))
    .unwrap();
    let attachment = Attachment {
        file: jcompat::file_manager::child_file(&root, "artifact # name.bin"),
        kind: AttachmentType::Profiling,
        name: None,
    };
    let actual = model_json(&result_model(
        "android",
        &device(),
        &result("profile", TestStatus::Passed, 1, 2),
        &[attachment],
        &cfg,
        &root,
    ));
    let expected: serde_json::Value = checked(serde_json::from_slice(&checked(std::fs::read(fixture(
        "allure/android-result.json",
    )))));
    assert_eq!(actual, checked(serde_json::to_string(&expected)));
    let actual: serde_json::Value = checked(serde_json::from_str(&actual));
    // Negative witnesses through the same structural comparison: status and link URL.
    let mut changed_status = actual.clone();
    changed_status["status"] = "broken".into();
    assert_ne!(changed_status, expected);
    let mut wrong_link = actual;
    wrong_link["links"][0]["url"] = "wrong".into();
    assert_ne!(wrong_link, expected);
}
#[test]
fn attachment_emptiness_is_checked_and_bytes_are_not_copied() {
    let temp = Temp::new();
    checked(std::fs::write(temp.0.join("present"), b"content"));
    checked(std::fs::write(temp.0.join("empty"), b""));
    let root = temp.path("");
    let out = temp.path("allure");
    let cfg = Arc::new(config(&root));
    let mut r = result("attachments", TestStatus::Passed, 1, 2);
    r.attachments = ["present", "empty", "missing", "present"]
        .into_iter()
        .map(|p| Attachment {
            file: temp.path(p),
            kind: AttachmentType::Log,
            name: None,
        })
        .collect();
    let report =
        marathon_core::report::ExecutionReport::new(vec![], vec![], vec![], vec![], vec![event(r, true, "pool")]);
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let mut tape = Tape::default();
            tape.ids.insert(
                "core.AllureReporter.uuid".into(),
                Ids::Values(vec!["attachments".into()]),
            );
            let hooks = RuntimeHooks::taped(tape, &core.clock());
            AllureReporter::new(
                core.vendor(),
                cfg,
                out,
                root,
                hooks,
                Arc::new(LocalAttachmentFiles { io: core.vendor() }),
                Arc::new(|_| {}),
                Arc::new(Console::default()),
            )
            .generate(&report)
            .await
        })
    }));
    let model: serde_json::Value = checked(serde_json::from_slice(&checked(std::fs::read(
        temp.0.join("allure/attachments-result.json"),
    ))));
    let attachments = model["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0], attachments[1]);
    assert_eq!(checked(std::fs::read_dir(temp.0.join("allure"))).count(), 2);
}
