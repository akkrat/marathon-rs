use jcompat::{file_manager::JavaPath, jtime::instant_of};
use marathon_config::{
    binding::{BindingContext, Profile},
    loader::MarathonConfigurationLoader,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn collect(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let kind = fs::symlink_metadata(&path)?.file_type();
        if kind.is_dir() {
            collect(&path, root, out)?;
        } else if kind.is_file() {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy();
            let name = path.file_name().unwrap().to_string_lossy();
            if name.starts_with("Marathonfile")
                || name.starts_with("Marathondevices")
                || (rel.starts_with("configuration/fixture/config/")
                    && name.starts_with("sample_")
                    && name.ends_with(".yaml"))
                || (rel.starts_with("oracle/config-dump-inputs/") && name.ends_with(".yaml"))
                || (rel.contains("/marathondevices/") && name.ends_with(".yaml"))
            {
                out.push(path);
            }
        }
    }
    Ok(())
}

#[test]
fn configuration_corpus_matches_explicit_golden() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()?;
    let tmp = tempfile::tempdir()?;
    let tmp_root = tmp.path().canonicalize()?;
    let mut cases = vec![];
    collect(&root, &root, &mut cases)?;
    cases.sort();
    let mut rows = BTreeMap::<String, Value>::new();
    for (index, source) in cases.iter().enumerate() {
        let rel = source.strip_prefix(&root)?.to_string_lossy().to_string();
        let devices = source
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("Marathondevices")
            || rel.contains("/marathondevices/");
        let source_dir = if rel.starts_with("configuration/fixture/config/") {
            root.join("configuration/fixture/config")
        } else {
            source.parent().unwrap().to_path_buf()
        };
        let work = tmp_root.join(index.to_string());
        fs::create_dir(&work)?;
        let copy = work.join("input");
        let status = Command::new("cp").args(["-R"]).arg(&source_dir).arg(&copy).status()?;
        assert!(status.success(), "copy failed: {rel}");
        let input = copy.join(source.strip_prefix(&source_dir)?);
        let context_path = copy.to_string_lossy();
        let row = if devices {
            let context = BindingContext {
                profile: Profile::Marathondevices,
                marathonfile: JavaPath::from_text(&input.to_string_lossy()),
                marathonfile_dir: JavaPath::from_text(&context_path),
                process_dir: JavaPath::from_text(&context_path),
                environment: vec![("ANDROID_HOME".into(), "/android-sdk".into())],
                now: instant_of(0, 0).unwrap(),
                analytics_tracking: true,
                bugsnag_reporting: true,
            };
            match MarathonConfigurationLoader.devices_sync(&fs::read(&input)?, &context) {
                Ok(value) => {
                    json!({"accepted": true, "config": format!("{value:?}").replace(&tmp_root.to_string_lossy().to_string(), "<ROOT>")})
                }
                Err(_) => json!({"accepted": false}),
            }
        } else {
            let env_file = work.join("env");
            fs::write(&env_file, "ANDROID_HOME=/android-sdk\n")?;
            let output = Command::new(env!("CARGO_BIN_EXE_marathon"))
                .current_dir(&copy)
                .env_clear()
                .args(["config-dump"])
                .arg(&input)
                .arg("--env-file")
                .arg(&env_file)
                .args(["--now", "1970-01-12T13:46:40Z"])
                .output()?;
            let code = output.status.code();
            if code != Some(0) {
                json!({"exit": code, "accepted": false})
            } else {
                let text = String::from_utf8(output.stdout)?.replace(&tmp_root.to_string_lossy().to_string(), "<ROOT>");
                let dump: Value = serde_json::from_str(&text)?;
                let accepted = dump["stage"] == "ok";
                json!({"exit": code, "accepted": accepted, "config": if accepted { dump["config"].clone() } else { Value::Null }})
            }
        };
        rows.insert(rel, row);
    }
    // The schema corpus embeds Marathonfiles in JSONL rather than storing each as a file.
    let schema = fs::read_to_string(root.join("oracle/config-validation-corpus.jsonl"))?;
    for (index, line) in schema.lines().enumerate() {
        let case: Value = serde_json::from_str(line)?;
        if case["kind"] != "case" {
            continue;
        }
        let work = tmp_root.join(format!("schema-{index}"));
        fs::create_dir(&work)?;
        let input = work.join("Marathonfile");
        let text = case["input"]
            .as_str()
            .ok_or("missing schema input")?
            .replace("$DIR", &work.to_string_lossy());
        fs::write(&input, text)?;
        let env_file = tmp_root.join(format!("schema-{index}.env"));
        fs::write(
            &env_file,
            if case["noSdk"] == true {
                ""
            } else {
                "ANDROID_HOME=/android-sdk\n"
            },
        )?;
        let output = Command::new(env!("CARGO_BIN_EXE_marathon"))
            .current_dir(&work)
            .env_clear()
            .arg("config-dump")
            .arg(&input)
            .arg("--env-file")
            .arg(&env_file)
            .args(["--now", "1970-01-12T13:46:40Z"])
            .output()?;
        let code = output.status.code();
        let row = if code != Some(0) {
            json!({"exit": code, "accepted": false})
        } else {
            let text = String::from_utf8(output.stdout)?.replace(&tmp_root.to_string_lossy().to_string(), "<ROOT>");
            let dump: Value = serde_json::from_str(&text)?;
            let accepted = dump["stage"] == "ok";
            json!({"exit": code, "accepted": accepted, "config": if accepted { dump["config"].clone() } else { Value::Null }})
        };
        rows.insert(
            format!("schema/{}", case["id"].as_str().ok_or("missing schema id")?),
            row,
        );
    }
    let golden = root.join("contract/config/corpus.json");
    if std::env::var_os("MARATHON_BLESS_CONFIG").as_deref() == Some(std::ffi::OsStr::new("1")) {
        fs::create_dir_all(golden.parent().unwrap())?;
        fs::write(&golden, format!("{}\n", serde_json::to_string_pretty(&rows)?))?;
    } else {
        let expected: BTreeMap<String, Value> = serde_json::from_slice(&fs::read(&golden)?)?;
        assert_eq!(rows, expected);
    }
    Ok(())
}
