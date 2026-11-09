//! Port of core/src/test/kotlin/com/malinskiy/marathon/extension/StringExtensionsTest.kt
//! and core/src/test/kotlin/com/malinskiy/marathon/io/FileManagerTest.kt.
use jcompat::{file_manager::*, fragment_hash::*};
use serde_json::Value;
use std::ffi::OsStr;

fn rows(data: &str) -> Vec<Value> {
    let mut lines = data.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let rows: Vec<Value> = lines.map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.len(), header["rows"].as_u64().unwrap() as usize);
    rows
}
const FILES: &str = include_str!("../../../fixtures/oracle/jcompat/file_manager.jsonl");
const EDGES: &str = include_str!("../../../fixtures/oracle/jcompat/file_manager_edges.jsonl");
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap()
}
fn optional<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v[key].as_str()
}
fn units(v: &Value) -> Vec<u16> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap() as u16)
        .collect()
}
fn kind(s: &str) -> FileType {
    match s {
        "TEST" => FileType::Test,
        "TEST_RESULT" => FileType::TestResult,
        "LOG" => FileType::Log,
        "DEVICE_LOG" => FileType::DeviceLog,
        "DEVICE_INFO" => FileType::DeviceInfo,
        "VIDEO" => FileType::Video,
        "SCREENSHOT" => FileType::Screenshot,
        "SCREENSHOT_PNG" => FileType::ScreenshotPng,
        "SCREENSHOT_JPG" => FileType::ScreenshotJpg,
        "SCREENSHOT_WEBP" => FileType::ScreenshotWebp,
        "SCREENSHOT_GIF" => FileType::ScreenshotGif,
        "XCTESTRUN" => FileType::Xctestrun,
        "BILL" => FileType::Bill,
        "TRACING" => FileType::Tracing,
        _ => panic!("unknown file type {s}"),
    }
}
#[test]
fn fragment_hash_oracle() {
    for row in rows(include_str!("../../../fixtures/oracle/jcompat/fragment_hash.jsonl")) {
        let input = text(&row, "input");
        let hash = signed_md5(input);
        assert_eq!(hash.to_string(), text(&row, "signed"));
        assert_eq!(format!("{:032x}", hash as u128), text(&row, "md5"));
        for (count, index) in row["fragment"].as_object().unwrap() {
            assert_eq!(fragment_index(input, count.parse().unwrap()), index.as_i64());
        }
    }
    assert_eq!(fragment_index("x", 0), None);
}
#[test]
fn file_manager_oracle() {
    for row in rows(FILES) {
        let op = text(&row, "op");
        if op == "escape" {
            assert_eq!(escape(text(&row, "input")), text(&row, "result"));
            continue;
        }
        let fm = FileManager::new(
            row["maxPath"].as_i64().unwrap() as i32,
            row["maxFilename"].as_i64().unwrap() as i32,
            "/tmp/oracle-file-manager/out",
            "/",
        );
        let a = &row["args"];
        let plan = if op == "createFolder" {
            fm.plan_folder(
                if text(a, "folderType") == "HTML" {
                    FolderType::Html
                } else {
                    FolderType::DeviceFiles
                },
                optional(a, "pool"),
                optional(a, "serial"),
            )
        } else {
            let test_name = a["test"].as_array().map(|t| {
                let pkg = t[0].as_str().unwrap();
                format!(
                    "{}{}{}#{}",
                    pkg,
                    if pkg.is_empty() { "" } else { "." },
                    t[1].as_str().unwrap(),
                    t[2].as_str().unwrap()
                )
            });
            fm.plan_file(FileRequest {
                file_type: kind(text(a, "fileType")),
                pool: text(a, "pool"),
                serial: text(a, "serial"),
                test_name: test_name.as_deref(),
                batch_id: optional(a, "testBatchId"),
                id: optional(a, "id"),
                chunk: optional(a, "chunk"),
            })
        };
        assert_eq!(plan.path.as_os_str(), OsStr::new(text(&row["result"], "ok")), "{row}");
    }
}
fn input_text(value: &Value) -> Option<String> {
    if value.is_null() {
        None
    } else if let Some(s) = value.as_str() {
        Some(s.to_owned())
    } else {
        Some(String::from_utf16_lossy(&units(value)))
    }
}
fn check_edge(row: &Value) {
    let a = &row["args"];
    match text(row, "op") {
        // n/a: 8.3: D55 — no surviving text-path boundary accepts this row's lone surrogate.
        "native_file" => {}
        "filesystem" => {} // The filesystem_effects test executes this row in its own sandbox.
        "fragment" => {
            let actual = fragment_index_for_hash(
                text(row, "hash").parse().unwrap(),
                row["count"].as_i64().unwrap() as i32,
            );
            assert_eq!(
                actual
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "ArithmeticException".to_owned()),
                text(row, "result")
            );
        }
        op if op.starts_with("escape") => assert_eq!(
            escape(&String::from_utf16_lossy(&units(a))),
            String::from_utf16_lossy(&units(&row["result"]))
        ),
        op if op.starts_with("md5") => {
            let raw = units(a);
            let expected = if String::from_utf16(&raw).is_ok() {
                row["result"].as_str().unwrap().to_owned()
            } else {
                // D55: these four JVM cases hash U+FFFD after input decoding.
                match raw.as_slice() {
                    [0xd800] | [0xdc00] => "-133641603088736614231142061705423321178",
                    [0xd800, 120, 0xdc00] => "142515326707125455296205615886103672735",
                    [0xd800, 0xd800, 0xdc00] => "45687870830446274565826030865081490136",
                    _ => panic!("unexpected D55 hash input"),
                }
                .to_owned()
            };
            assert_eq!(signed_md5(&String::from_utf16_lossy(&raw)).to_string(), expected);
        }
        "absolute" => assert_eq!(
            absolute_file_path(a.as_str().unwrap(), text(row, "cwd")).as_os_str(),
            OsStr::new(&String::from_utf16_lossy(&units(&row["result"])))
        ),
        op => {
            let output = input_text(&a["output"]).unwrap();
            let fm = FileManager::new(
                a["maxPath"].as_i64().unwrap() as i32,
                a["maxFilename"].as_i64().unwrap() as i32,
                &output,
                "/",
            );
            let pool = input_text(&a["pool"]);
            let serial = input_text(&a["serial"]);
            let test = input_text(&a["testName"]);
            let batch = input_text(&a["batch"]);
            let id = input_text(&a["id"]);
            let chunk = input_text(&a["chunk"]);
            let plan = match op.split('_').next().unwrap() {
                "folder" => fm.plan_folder(FolderType::DeviceFiles, pool.as_deref(), serial.as_deref()),
                "result" => fm.plan_test_result_file(&input_text(&a["filename"]).unwrap()),
                _ => fm.plan_file(FileRequest {
                    file_type: kind(text(a, "type")),
                    pool: pool.as_deref().unwrap(),
                    serial: serial.as_deref().unwrap(),
                    test_name: test.as_deref(),
                    batch_id: batch.as_deref(),
                    id: id.as_deref(),
                    chunk: chunk.as_deref(),
                }),
            };
            // Unit-only inputs cross D55's replacement boundary before the text API.
            if row["result"]["error"].is_string()
                && a.as_object()
                    .unwrap()
                    .values()
                    .any(|v| v.is_array() && String::from_utf16(&units(v)).is_err())
            {
                let expected = if op.starts_with("result") {
                    "/tmp/oracle-wp17/�/test_result/raw.json"
                } else {
                    "/"
                };
                assert_eq!(plan.path.to_string_lossy(), expected, "{}", text(row, "id"));
                assert_eq!(plan.diagnostics.len(), usize::from(!op.starts_with("result")));
                return;
            }
            let actual_messages = if row["id"] == "filename-1" {
                // D56: dropping the first pair leaves no filename; materialization rejects it.
                assert_eq!(
                    plan.path.as_os_str(),
                    OsStr::new("/tmp/oracle-wp17/out/logs/testPoolId/xxyyzz")
                );
                plan.diagnostics.clone()
            } else if row["id"] == "folder-18" || row["id"] == "path-18" {
                // D56: dropping the first pair in a component leaves its normalized parent.
                // Materialization rejects this empty component instead of returning that parent.
                assert_eq!(plan.path.to_string_lossy(), "/tmp/oracle-wp17");
                plan.diagnostics.clone()
            } else if row["result"]["error"].is_string() {
                assert_eq!(
                    text(&row["result"], "error"),
                    "java.nio.file.InvalidPathException",
                    "{row}"
                );
                let mut messages = Vec::new();
                assert!(plan.materialize(|m| messages.push(m.to_owned())).is_err(), "{row}");
                messages
            } else {
                // D56: a cut drops a trailing high surrogate instead of retaining it.
                let raw = units(&row["result"]["units"]);
                let expected: String = if a
                    .as_object()
                    .unwrap()
                    .values()
                    .any(|v| v.is_array() && String::from_utf16(&units(v)).is_err())
                {
                    String::from_utf16_lossy(&raw)
                } else {
                    char::decode_utf16(raw).flatten().collect()
                };
                assert_eq!(plan.path.as_os_str(), OsStr::new(&expected), "{}", text(row, "id"));
                plan.diagnostics.clone()
            };
            let messages: Vec<String> = row["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| char::decode_utf16(units(v)).flatten().collect())
                .collect();
            assert_eq!(actual_messages, messages, "{}", text(row, "id"));
        }
    }
}

#[test]
fn filesystem_effects() -> std::io::Result<()> {
    let row = rows(EDGES).into_iter().find(|r| r["op"] == "filesystem").unwrap();
    let expected = &row["result"];
    let root = std::env::temp_dir().join(format!(
        "jcompat-paths-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&root)?;
    let output = root.join("filesystem");
    let fm = FileManager::new(0, 255, &output.to_string_lossy(), "/");
    let request = FileRequest {
        file_type: FileType::Log,
        pool: "p",
        serial: "s",
        test_name: Some("C#m"),
        batch_id: None,
        id: None,
        chunk: None,
    };
    let plan = fm.plan_file(request);
    plan.create_directories()?;
    assert_eq!(plan.path.exists(), expected["fileExists"].as_bool().unwrap());
    assert_eq!(plan.directory.is_dir(), expected["directoryExists"].as_bool().unwrap());
    let folder = fm.plan_folder(FolderType::Html, None, None);
    folder.create_directories()?;
    assert_eq!(folder.path.is_dir(), expected["folderExists"].as_bool().unwrap());
    let blocked = root.join("blocked");
    std::fs::write(&blocked, "x")?;
    let bad = FileManager::new(0, 255, &blocked.to_string_lossy(), "/");
    let ignored = bad.plan_test_result_file("raw.json");
    ignored.create_directories()?;
    assert_eq!(ignored.path.exists(), expected["resultExists"].as_bool().unwrap());
    assert_eq!(
        bad.plan_file(request).create_directories().is_err(),
        expected["nioFailed"].as_bool().unwrap()
    );
    std::fs::remove_dir_all(root)
}

#[test]
fn file_manager_edges_oracle() {
    for row in rows(EDGES) {
        check_edge(&row);
    }
}
fn original(name: &str) {
    check_edge(&rows(EDGES).into_iter().find(|row| row["id"] == name).unwrap());
}
#[test]
fn test_string_escaping() {
    original("test_string_escaping");
}
#[test]
fn create_filename_normal_length_test() {
    original("create_filename_normal_length_test");
}
#[test]
fn create_filename_long_length_method_limited_path_test() {
    original("create_filename_long_length_method_limited_path_test");
}
#[test]
fn test_create_filename_named_parameterized_long() {
    original("test_create_filename_named_parameterized_long");
}
#[test]
fn test_device_serial_escaping() {
    original("test_device_serial_escaping");
}
#[test]
fn test_screenshotfile() {
    original("test_screenshotfile");
}
#[test]
fn test_too_long_output_path_unlimited_filename() {
    original("test_too_long_output_path_unlimited_filename");
}

#[test]
fn split_pair_limits_write_complete_paths_d56() -> std::io::Result<()> {
    let root = std::env::temp_dir().join(format!("path-limit-d56-{}", std::process::id()));
    let output = root.to_string_lossy();
    let filename = FileManager::new(0, 2, &output, "/").plan_test_result_file("a😀z");
    filename.create_directories()?;
    assert_eq!(filename.path.as_os_str(), root.join("test_result/a").as_os_str());
    std::fs::write(&filename.path, b"filename")?;
    let prefix = format!("{output}/test_result/b");
    let file =
        FileManager::new(prefix.encode_utf16().count() as i32 + 1, 0, &output, "/").plan_test_result_file("b😀z");
    file.create_directories()?;
    assert_eq!(file.path.as_os_str(), root.join("test_result/b").as_os_str());
    std::fs::write(&file.path, b"path")?;
    let prefix = format!("{output}/device-files/c");
    let folder = FileManager::new(prefix.encode_utf16().count() as i32 + 1, 0, &output, "/").plan_folder(
        FolderType::DeviceFiles,
        Some("c😀z"),
        None,
    );
    folder.create_directories()?;
    assert_eq!(folder.path.as_os_str(), root.join("device-files/c").as_os_str());
    assert!(folder.path.is_dir());
    assert_eq!(std::fs::read(&filename.path)?, b"filename");
    assert_eq!(std::fs::read(&file.path)?, b"path");
    std::fs::remove_dir_all(root)
}

#[test]
fn split_pair_limits_reject_empty_components_d56() -> std::io::Result<()> {
    let root = std::env::temp_dir().join(format!("empty-path-limit-d56-{}", std::process::id()));
    let output = root.to_string_lossy();
    let filename = FileManager::new(0, 1, &output, "/").plan_test_result_file("😀.json");
    let prefix = format!("{output}/test_result/");
    let file =
        FileManager::new(prefix.encode_utf16().count() as i32 + 1, 0, &output, "/").plan_test_result_file("😀.json");
    let prefix = format!("{output}/device-files/");
    let folder = FileManager::new(prefix.encode_utf16().count() as i32 + 1, 0, &output, "/").plan_folder(
        FolderType::DeviceFiles,
        Some("😀pool"),
        None,
    );
    let batch = FileManager::new(0, 1, &output, "/").plan_file(FileRequest {
        file_type: FileType::Log,
        pool: "pool",
        serial: "serial",
        test_name: None,
        batch_id: Some("😀batch"),
        id: None,
        chunk: None,
    });
    for plan in [filename, file, folder, batch] {
        let error = plan
            .materialize(|_| {})
            .expect_err("an empty cut must not return its parent");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "invalid utf-16: lone surrogate found");
    }
    assert!(!root.join("device-files").exists());
    // A shorter maxPath removes the invalid filename before the native boundary.
    let masked = FileManager::new(1, 1, &output, "/").plan_test_result_file("😀.json");
    assert_eq!(masked.materialize(|_| {})?.as_os_str(), OsStr::new("/"));
    std::fs::remove_dir_all(root)
}

#[test]
fn materialize_rejects_nul_before_filesystem_access() {
    let manager = FileManager::new(0, 0, "out\0tail", "/");
    for plan in [
        manager.plan_test_result_file("result.json"),
        manager.plan_folder(FolderType::Html, None, None),
    ] {
        let error = plan.materialize(|_| {}).expect_err("NUL must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "Nul character not allowed");
    }
}
