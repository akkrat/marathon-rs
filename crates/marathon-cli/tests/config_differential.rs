//! Differential replay of tools/oracle/module/src/main/kotlin/oracle/ConfigDump.kt.
//! The driver compares committed JVM documents and final trees with fresh production CLI runs.

use std::{path::Path, process::Command};

#[test]
fn config_differential_replays_committed_jvm_evidence() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = tempfile::tempdir()?;
    let output = Command::new("python3")
        .arg("-I")
        .arg(root.join("tools/oracle/config-differential-lib/suite.py"))
        .args(["replay", "--binary", env!("CARGO_BIN_EXE_marathon"), "--run-dir"])
        .arg(run.path())
        .current_dir(&root)
        .output()?;
    assert!(
        output.status.success(),
        "{}\n{}\nevidence: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        run.keep().display(),
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}

#[test]
fn document_reuses_the_production_canonical_configuration() -> Result<(), Box<dyn std::error::Error>> {
    use jcompat::{file_manager::JavaPath, jtime::parse_instant};
    use marathon_cli::{application::Process, config_dump::document};
    use marathon_config::{
        binding::{BindingContext, Profile},
        dump::{DumpValue, canonical_configuration},
        loader::MarathonConfigurationLoader,
    };

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let config_dir = root.join("config");
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&cwd)?;
    let file = config_dir.join("Marathonfile");
    let bytes = b"name: renderer\noutputDir: out\nvendorConfiguration: {type: stub}\n";
    std::fs::write(&file, bytes)?;
    let now = parse_instant("1970-01-12T13:46:40Z").ok_or("fixed instant")?;
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: JavaPath::from_text(&file.to_string_lossy()),
        marathonfile_dir: JavaPath::from_text(&config_dir.to_string_lossy()),
        process_dir: JavaPath::from_text(&cwd.to_string_lossy()),
        analytics_tracking: true,
        bugsnag_reporting: true,
        environment: Vec::new(),
        now,
    };
    let config = MarathonConfigurationLoader
        .parse_sync(bytes, &context)
        .map_err(|e| format!("{e:?}"))?;
    let canonical = canonical_configuration(&config, &context.process_dir);
    let process = Process {
        cwd,
        environment: Vec::new(),
        now,
    };
    let result = document(&file.to_string_lossy(), None, Some(now), &process).map_err(|e| format!("{e:?}"))?;
    let DumpValue::Object(fields) = result else {
        return Err("expected a dump object".into());
    };
    assert_eq!(
        fields
            .iter()
            .find(|(key, _)| key == &"config".into())
            .map(|(_, value)| value),
        Some(&canonical)
    );
    assert!(
        config
            .to_map(&context.process_dir)
            .iter()
            .any(|(key, _)| key == &"pooling".into())
    );
    Ok(())
}
