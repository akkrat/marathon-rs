//! D20 (PLAN.md §3.11): the built `marathon` when an `xctest` run has no copy of the third-party library it needs.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const VARIABLE: &str = "MARATHON_XCTEST_PARSER_DIR";

fn marathon_config_files(root: &Path, vendor: &str, parser: &str) -> std::io::Result<()> {
    fs::write(root.join("Marathondevices"), "workers: []\n")?;
    // Binding wants the bundle folders to exist; nothing in these runs reads them.
    fs::create_dir_all(root.join("Sample.app"))?;
    fs::create_dir_all(root.join("Sample.xctest"))?;
    fs::write(
        root.join("Marathonfile"),
        format!(
            "name: supply\noutputDir: report\nanalyticsTracking: false\nbugsnagReporting: false\n\
             uncompletedTestRetryQuota: 5\ndeviceInitializationTimeoutMillis: 100\nvendorConfiguration:\n  type: {vendor}\n  devices: Marathondevices\n  \
             bundle:\n    application: Sample.app\n    testApplication: Sample.xctest\n    testType: xctest\n  \
             testParserConfiguration:\n    type: {parser}\n"
        ),
    )
}

fn marathon(root: &Path, vendor: &str, parser: &str, variable: Option<&str>) -> std::io::Result<Output> {
    marathon_config_files(root, vendor, parser)?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_marathon"));
    command
        .current_dir(root)
        .args(["run", "-m", "Marathonfile"])
        .env_remove(VARIABLE);
    if let Some(value) = variable {
        command.env(VARIABLE, value);
    }
    command.output()
}

#[test]
fn an_xctest_run_without_its_library_fails_during_construction_and_says_what_to_do() -> std::io::Result<()> {
    for vendor in ["iOS", "macOS"] {
        let root = tempfile::tempdir()?;
        let empty = root.path().join("empty");
        fs::create_dir(&empty)?;
        let empty = empty.to_string_lossy().into_owned();
        for (variable, problem) in [
            (None, "is not set"),
            (Some(""), "is not set"),
            (Some(empty.as_str()), "libxctest-parser.dylib cannot be read"),
        ] {
            let output = marathon(root.path(), vendor, "xctest", variable)?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(1), "{vendor} {variable:?}: {stderr}");
            for part in [
                "configuration rejected",
                problem,
                VARIABLE,
                "<platform name>/libxctest-parser.dylib",
                "EULA",
                "sends error reports to its server",
                "nm test parser",
            ] {
                assert!(
                    stderr.contains(part),
                    "{vendor} {variable:?}: {part} missing from {stderr}"
                );
            }
            // Construction failed, so no run started and nothing waited for a device.
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(!stdout.contains("temporary device"), "{stdout}");
            assert!(!stderr.contains("temporary device"), "{stderr}");
        }
    }
    Ok(())
}

#[test]
fn ignore_failures_does_not_hide_a_construction_failure() -> std::io::Result<()> {
    let root = tempfile::tempdir()?;
    let output = marathon(root.path(), "macOS", "xctest\nignoreFailures: true", None)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(VARIABLE), "{stderr}");
    Ok(())
}

#[test]
fn an_nm_run_never_mentions_the_variable_whatever_its_value() -> std::io::Result<()> {
    for vendor in ["iOS", "macOS"] {
        for variable in [None, Some("/no/such/directory")] {
            let root = tempfile::tempdir()?;
            let output = marathon(root.path(), vendor, "nm", variable)?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            for text in [&stdout, &stderr] {
                assert!(!text.contains(VARIABLE), "{vendor} {variable:?}: {text}");
                assert!(!text.contains("libxctest-parser"), "{vendor} {variable:?}: {text}");
            }
            // It got past construction: the run itself then ends on the empty worker list.
            assert!(stdout.contains("No workers found"), "{vendor}: {stdout}{stderr}");
            assert_eq!(output.status.code(), Some(1), "{vendor}: {stdout}{stderr}");
        }
    }
    Ok(())
}

#[test]
fn an_xctest_run_with_its_library_gets_past_construction() -> std::io::Result<()> {
    for (vendor, platform) in [("iOS", "iPhoneSimulator"), ("macOS", "MacOSX")] {
        let root = tempfile::tempdir()?;
        let supplied = root.path().join("supplied");
        fs::create_dir_all(supplied.join(platform))?;
        fs::write(
            supplied.join(platform).join("libxctest-parser.dylib"),
            b"fake stand-in, written by a test\n",
        )?;
        let output = marathon(root.path(), vendor, "xctest", Some(&supplied.to_string_lossy()))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("No workers found"), "{vendor}: {stdout}{stderr}");
        assert!(!stderr.contains(VARIABLE), "{vendor}: {stderr}");
    }
    Ok(())
}

/// A FIFO with no writer blocks a plain open forever, so this polls with the 60 s hang deadline of docs/testing.md.
#[test]
fn a_fifo_in_place_of_the_library_fails_construction_instead_of_blocking() -> std::io::Result<()> {
    let root = tempfile::tempdir()?;
    let supplied = root.path().join("supplied");
    fs::create_dir_all(supplied.join("MacOSX"))?;
    let fifo = supplied.join("MacOSX/libxctest-parser.dylib");
    assert!(Command::new("mkfifo").arg(&fifo).status()?.success());
    marathon_config_files(root.path(), "macOS", "xctest")?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_marathon"))
        .current_dir(root.path())
        .args(["run", "-m", "Marathonfile"])
        .env(VARIABLE, &supplied)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while child.try_wait()?.is_none() {
        if std::time::Instant::now() > deadline {
            child.kill()?;
            child.wait()?;
            panic!("construction blocked on the FIFO");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let output = child.wait_with_output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    for part in ["configuration rejected", "is not a file", VARIABLE, "EULA"] {
        assert!(stderr.contains(part), "{part} missing from {stderr}");
    }
    Ok(())
}
