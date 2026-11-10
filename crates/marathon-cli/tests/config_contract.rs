use jcompat::jtime::instant_of;

use marathon_config::{
    binding::{BindingContext, Profile},
    devices::{AppleTarget, MarathonDevices, Transport},
    loader::MarathonConfigurationLoader,
    vendor::SshAuthentication,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn devices_dump(devices: &MarathonDevices, root: &Path) -> Value {
    let path = |path: &Path| {
        path.to_string_lossy()
            .replace(&root.to_string_lossy().to_string(), "<ROOT>")
    };
    json!(devices.workers.iter().map(|worker| {
        let transport = match &worker.transport {
            Transport::Local => json!({"type": "local"}),
            Transport::Ssh { addr, port, authentication, check_reachability } => {
                let authentication = match authentication {
                    None => Value::Null,
                    Some(SshAuthentication::Password { username, password }) => json!({"type": "password", "username": username, "password": password}),
                    Some(SshAuthentication::PublicKey { username, key }) => json!({"type": "public-key", "username": username, "key": path(key)}),
                };
                json!({"type": "ssh", "addr": addr, "port": port, "authentication": authentication, "checkReachability": check_reachability})
            }
        };
        let targets: Vec<Value> = worker.devices.iter().map(|target| match target {
            AppleTarget::Host => json!({"type": "host"}),
            AppleTarget::Simulator { udid } => json!({"type": "simulator", "udid": udid}),
            AppleTarget::Physical { udid } => json!({"type": "physical", "udid": udid}),
            AppleTarget::SimulatorProfile(profile) => json!({"type": "simulator-profile", "deviceTypeId": profile.device_type_id, "runtimeId": profile.runtime_id.as_ref(), "newNamePrefix": profile.new_name_prefix}),
        }).collect();
        json!({"transport": transport, "devices": targets})
    }).collect::<Vec<_>>())
}

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

fn round_trip_if_parseable(
    input: &Path,
    process_dir: &Path,
    environment: Vec<(String, String)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: input.to_path_buf(),
        marathonfile_dir: input.parent().ok_or("Marathonfile parent")?.canonicalize()?,
        process_dir: process_dir.to_path_buf(),
        environment,
        now: instant_of(1_000_000, 0).ok_or("reference instant")?,
        analytics_tracking: true,
        bugsnag_reporting: true,
    };
    if let Ok(config) = MarathonConfigurationLoader.parse_sync(&fs::read(input)?, &context) {
        let serialized = marathon_config::serialize(&config)?;
        let recreated = MarathonConfigurationLoader
            .parse_sync(serialized.as_bytes(), &context)
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(config, recreated, "{}", input.display());
    }
    Ok(())
}

const PLATFORM_CASES: [&str; 2] = ["a-devices-symlink-loop", "a-known-hosts-symlink-loop"];

fn platform_rows(
    rows: &mut BTreeMap<String, Value>,
    previous: &BTreeMap<String, Value>,
    linux: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    for id in PLATFORM_CASES {
        let key = format!("schema/{id}");
        let actual = rows.remove(&key).ok_or("missing platform case")?;
        let linux_value = json!({"exit": 0, "stage": "parse", "accepted": false, "config": null});
        let macos_value = if linux {
            assert_eq!(actual, linux_value, "Linux schema {id}");
            let old = previous.get(&key).ok_or("missing macOS platform golden")?;
            old.get("platforms").map_or_else(|| old.clone(), |v| v["macos"].clone())
        } else {
            actual
        };
        if macos_value["stage"] != "ok" || macos_value["config"].is_null() {
            return Err(format!("missing accepted macOS golden for {id}").into());
        }
        rows.insert(key, json!({"platforms": {"macos": macos_value, "linux": linux_value}}));
    }
    Ok(())
}

#[test]
fn platform_golden_is_identical_for_either_host() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("MARATHON_BLESS_CONFIG").as_deref() == Some(std::ffi::OsStr::new("1")) {
        return Ok(());
    }
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contract/config/corpus.json");
    let previous: BTreeMap<String, Value> = serde_json::from_slice(&fs::read(golden)?)?;
    for linux in [false, true] {
        let mut actual = previous.clone();
        for id in PLATFORM_CASES {
            let row = actual.get_mut(&format!("schema/{id}")).ok_or("missing platform case")?;
            *row = row["platforms"][if linux { "linux" } else { "macos" }].clone();
        }
        platform_rows(&mut actual, &previous, linux)?;
        assert_eq!(
            serde_json::to_vec_pretty(&actual)?,
            serde_json::to_vec_pretty(&previous)?
        );
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
                marathonfile: PathBuf::from(input.to_string_lossy().into_owned()),
                marathonfile_dir: PathBuf::from(context_path.as_ref()),
                process_dir: PathBuf::from(context_path.as_ref()),
                environment: vec![("ANDROID_HOME".into(), "/android-sdk".into())],
                now: instant_of(0, 0).unwrap(),
                analytics_tracking: true,
                bugsnag_reporting: true,
            };
            match MarathonConfigurationLoader.devices_sync(&fs::read(&input)?, &context) {
                Ok(value) => {
                    json!({"accepted": true, "config": devices_dump(&value, &tmp_root)})
                }
                Err(_) => json!({"accepted": false}),
            }
        } else {
            round_trip_if_parseable(&input, &copy, vec![("ANDROID_HOME".into(), "/android-sdk".into())])?;
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
    // These cases use the same filesystem and environment as validation_corpus.rs.
    let schema = fs::read_to_string(root.join("oracle/coverage-c95b2128/config-validation-corpus.jsonl"))?;
    let mut cases: Vec<Value> = schema.lines().map(serde_json::from_str).collect::<Result<_, _>>()?;
    if cfg!(target_os = "linux") {
        let linux = fs::read_to_string(root.join("oracle/config-validation-corpus-linux.jsonl"))?;
        for line in linux.lines().skip(1) {
            let replacement: Value = serde_json::from_str(line)?;
            let row = cases
                .iter_mut()
                .find(|row| row["id"] == replacement["id"])
                .ok_or("missing Linux schema case")?;
            *row = replacement;
        }
    }
    for (index, case) in cases.iter().enumerate() {
        let kind = case["kind"].as_str().unwrap_or_default();
        if kind != "case" && kind != "fixture" {
            continue;
        }
        let work = tmp_root.join(format!("schema-{index}"));
        fs::create_dir(&work)?;
        let input = if kind == "case" {
            let source = root.join("configuration/fixture/config/ios/derivedDataDir");
            let status = Command::new("cp")
                .arg("-R")
                .arg(source)
                .arg(work.join("derivedDataDir"))
                .status()?;
            assert!(status.success(), "copy schema fixture");
            fs::write(work.join("app.apk"), "")?;
            fs::create_dir(work.join("apkdir"))?;
            fs::write(work.join("known_hosts"), "")?;
            std::os::unix::fs::symlink(work.join("loop"), work.join("loop"))?;
            let input = work.join("Marathonfile");
            let text = case["input"]
                .as_str()
                .ok_or("missing schema input")?
                .replace("@DIR@", &work.to_string_lossy());
            fs::write(&input, format!("{text}\n"))?;
            input
        } else {
            let source = root.join("configuration/fixture/config");
            let status = Command::new("cp").arg("-R").arg(source.join(".")).arg(&work).status()?;
            assert!(status.success(), "copy schema fixtures");
            work.join(case["input"].as_str().ok_or("missing fixture input")?)
        };
        let env_file = tmp_root.join(format!("schema-{index}.env"));
        let sdk = if case["noSdk"] == true {
            ""
        } else {
            "ANDROID_HOME=/android-sdk\n"
        };
        fs::write(&env_file, format!("{sdk}A=${{B}}\nB=${{A}}\n"))?;
        let mut environment = vec![("A".into(), "${B}".into()), ("B".into(), "${A}".into())];
        if case["noSdk"] != true {
            environment.push(("ANDROID_HOME".into(), "/android-sdk".into()));
        }
        round_trip_if_parseable(&input, &work, environment)?;
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
        let id = case["id"].as_str().ok_or("missing schema id")?;
        assert_eq!(
            code,
            Some(0),
            "schema {id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout)?.replace(&tmp_root.to_string_lossy().to_string(), "<ROOT>");
        let dump: Value = serde_json::from_str(&text)?;
        assert_eq!(dump["stage"], case["stage"], "schema {id}: {}", dump["error"]);
        let stage = dump["stage"].as_str().ok_or("missing stage")?;
        let accepted = stage == "ok";
        let row = json!({"exit": code, "stage": stage, "accepted": accepted, "config": if accepted { dump["config"].clone() } else { Value::Null }});
        rows.insert(format!("schema/{id}"), row);
    }
    let golden = root.join("contract/config/corpus.json");
    let previous: BTreeMap<String, Value> = serde_json::from_slice(&fs::read(&golden)?)?;
    platform_rows(&mut rows, &previous, cfg!(target_os = "linux"))?;
    if std::env::var_os("MARATHON_BLESS_CONFIG").as_deref() == Some(std::ffi::OsStr::new("1")) {
        fs::write(&golden, format!("{}\n", serde_json::to_string_pretty(&rows)?))?;
    } else {
        assert_eq!(rows, previous);
    }
    Ok(())
}
