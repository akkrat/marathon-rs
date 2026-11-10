use marathon_cli::args::{CliConfiguration, Invocation, parse};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn invocation(args: &[&str]) -> TestResult<Invocation> {
    let parsed = parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    let expected_exit = match &parsed {
        Invocation::Help(_) | Invocation::Execute(CliConfiguration::Version) => 0,
        Invocation::UsageError(_) | Invocation::Execute(_) => 1,
        Invocation::ConfigDump(_) => panic!("unexpected hidden command"),
    };
    // Every non-version execution here names a missing Marathonfile in an isolated directory.
    let cwd = tempfile::tempdir()?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_marathon"))
        .args(args)
        .current_dir(cwd.path())
        .env_clear()
        .output()?;
    assert_eq!(output.status.code(), Some(expected_exit), "{args:?}");
    Ok(parsed)
}

#[test]
fn cli_syntax_and_effects() -> TestResult {
    for args in [
        &["--help"][..],
        &["-h"],
        &["run", "--help"],
        &["parse", "--help"],
        &["version", "--help"],
    ] {
        assert!(matches!(invocation(args)?, Invocation::Help(_)), "{args:?}");
    }
    assert_eq!(
        invocation(&["version"])?,
        Invocation::Execute(CliConfiguration::Version)
    );
    for args in [
        &["--unknown"][..],
        &["run", "--marathonfile"],
        &["run", "--analyticsTracking"],
        &["run", "--bugsnag"],
        &["parse", "--marathonfile"],
        &["parse", "--output"],
        &["parse", "--include-flaky-tests"],
    ] {
        assert!(matches!(invocation(args)?, Invocation::UsageError(_)), "{args:?}");
    }
    for args in [&[][..], &["run"]] {
        let Invocation::Execute(CliConfiguration::Run {
            marathonfile,
            analytics_tracking,
            bugsnag_reporting,
        }) = invocation(args)?
        else {
            panic!("{args:?}")
        };
        assert_eq!(marathonfile.to_string_lossy().into_owned(), "Marathonfile");
        assert!(analytics_tracking && bugsnag_reporting);
    }
    let Invocation::Execute(CliConfiguration::Parse {
        marathonfile,
        output_file_name,
        include_flaky_tests,
    }) = invocation(&["parse"])?
    else {
        panic!("parse defaults")
    };
    assert_eq!(marathonfile.to_string_lossy().into_owned(), "Marathonfile");
    assert_eq!(output_file_name, None);
    assert!(!include_flaky_tests);
    let Invocation::Execute(CliConfiguration::Run {
        analytics_tracking,
        bugsnag_reporting,
        ..
    }) = invocation(&["--analyticsTracking", "false", "--bugsnag", "false"])?
    else {
        panic!("root options")
    };
    assert!(!analytics_tracking && !bugsnag_reporting);
    for flag in ["-m", "--marathonfile"] {
        let Invocation::Execute(CliConfiguration::Run { marathonfile, .. }) = invocation(&["run", flag, "custom"])?
        else {
            panic!("{flag}")
        };
        assert_eq!(marathonfile.to_string_lossy().into_owned(), "custom");
        let Invocation::Execute(CliConfiguration::Parse { marathonfile, .. }) = invocation(&["parse", flag, "custom"])?
        else {
            panic!("{flag}")
        };
        assert_eq!(marathonfile.to_string_lossy().into_owned(), "custom");
    }
    for (value, expected) in [
        ("true", true),
        ("TRUE", true),
        ("tRuE", true),
        ("false", false),
        ("invalid", false),
        ("yes", false),
        ("1", false),
        ("", false),
        (" true ", false),
        ("--help", false),
    ] {
        let Invocation::Execute(CliConfiguration::Run {
            analytics_tracking,
            bugsnag_reporting,
            ..
        }) = invocation(&["run", "--analyticsTracking", value, "--bugsnag", value])?
        else {
            panic!("{value}")
        };
        assert_eq!(analytics_tracking, expected);
        assert_eq!(bugsnag_reporting, expected);
        for flag in ["-f", "--include-flaky-tests"] {
            let Invocation::Execute(CliConfiguration::Parse {
                include_flaky_tests, ..
            }) = invocation(&["parse", flag, value])?
            else {
                panic!("{flag}")
            };
            assert_eq!(include_flaky_tests, expected);
        }
    }
    for flag in ["-o", "--output"] {
        let Invocation::Execute(CliConfiguration::Parse { output_file_name, .. }) =
            invocation(&["parse", flag, "tests"])?
        else {
            panic!("{flag}")
        };
        assert_eq!(output_file_name.as_deref(), Some("tests"));
    }
    Ok(())
}

#[test]
fn process_exit_codes_for_help_version_errors_and_missing_file() -> TestResult {
    let binary = env!("CARGO_BIN_EXE_marathon");
    for (args, code) in [
        (&["--help"][..], 0),
        (&["version"][..], 0),
        (&["--version"][..], 1),
        (&["--unknown"][..], 1),
        (&["run", "-m", "this-file-does-not-exist"][..], 1),
    ] {
        let output = std::process::Command::new(binary).args(args).output()?;
        let status = output.status;
        if args == ["--help"] {
            assert!(output.stderr.is_empty());
            assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
        } else if args == ["--version"] || args == ["--unknown"] {
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains(args[0]));
        }
        assert_eq!(status.code(), Some(code), "{args:?}");
    }
    Ok(())
}

#[test]
fn root_options_are_validated_but_not_inherited() -> TestResult {
    let root = ["-m", "root.yaml", "--analyticsTracking", "false", "--bugsnag", "false"];
    for subcommand in ["run", "parse", "version"] {
        let mut args = root.to_vec();
        args.push(subcommand);
        let expected = match subcommand {
            "run" => CliConfiguration::Run {
                marathonfile: "Marathonfile".into(),
                analytics_tracking: true,
                bugsnag_reporting: true,
            },
            "parse" => CliConfiguration::Parse {
                marathonfile: "Marathonfile".into(),
                output_file_name: None,
                include_flaky_tests: false,
            },
            _ => CliConfiguration::Version,
        };
        assert_eq!(invocation(&args)?, Invocation::Execute(expected));
    }
    assert_eq!(
        invocation(&["-m", "root.yaml", "run", "-m", "child.yaml", "--bugsnag", "false"])?,
        Invocation::Execute(CliConfiguration::Run {
            marathonfile: "child.yaml".into(),
            analytics_tracking: true,
            bugsnag_reporting: false,
        })
    );
    assert!(matches!(
        invocation(&["--unknown", "version"])?,
        Invocation::UsageError(_)
    ));
    assert!(matches!(
        invocation(&["version", "-m", "x"])?,
        Invocation::UsageError(_)
    ));
    Ok(())
}

#[test]
fn repeated_options_keep_the_last_value() -> TestResult {
    for command in [&[][..], &["run"][..]] {
        let mut args = command.to_vec();
        args.extend([
            "-m",
            "first",
            "--marathonfile",
            "last",
            "--analyticsTracking",
            "false",
            "--analyticsTracking",
            "TRUE",
            "--bugsnag",
            "true",
            "--bugsnag",
            "anything",
        ]);
        assert_eq!(
            invocation(&args)?,
            Invocation::Execute(CliConfiguration::Run {
                marathonfile: "last".into(),
                analytics_tracking: true,
                bugsnag_reporting: false,
            })
        );
    }
    assert_eq!(
        invocation(&[
            "parse",
            "-m",
            "first",
            "--marathonfile",
            "last",
            "-o",
            "first",
            "--output",
            "last",
            "-f",
            "true",
            "--include-flaky-tests",
            "anything"
        ])?,
        Invocation::Execute(CliConfiguration::Parse {
            marathonfile: "last".into(),
            output_file_name: Some("last".into()),
            include_flaky_tests: false,
        })
    );
    assert!(matches!(invocation(&["-m", "first", "-m"])?, Invocation::UsageError(_)));
    Ok(())
}
