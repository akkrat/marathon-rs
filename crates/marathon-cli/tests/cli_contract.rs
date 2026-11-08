use marathon_cli::args::{CliConfiguration, Invocation, parse};

fn invocation(args: &[&str]) -> Invocation {
    parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
}

#[test]
fn cli_syntax_and_effects() {
    for args in [
        &["--help"][..],
        &["-h"],
        &["run", "--help"],
        &["parse", "--help"],
        &["version", "--help"],
    ] {
        assert!(matches!(invocation(args), Invocation::Help(_)), "{args:?}");
    }
    assert_eq!(invocation(&["version"]), Invocation::Execute(CliConfiguration::Version));
    for args in [
        &["--unknown"][..],
        &["run", "--marathonfile"],
        &["run", "--analyticsTracking"],
        &["run", "--bugsnag"],
        &["parse", "--marathonfile"],
        &["parse", "--output"],
        &["parse", "--include-flaky-tests"],
    ] {
        assert!(matches!(invocation(args), Invocation::UsageError(_)), "{args:?}");
    }
    for args in [&[][..], &["run"]] {
        let Invocation::Execute(CliConfiguration::Run {
            marathonfile,
            analytics_tracking,
            bugsnag_reporting,
        }) = invocation(args)
        else {
            panic!("{args:?}")
        };
        assert_eq!(String::from_utf16_lossy(&marathonfile.0), "Marathonfile");
        assert!(analytics_tracking && bugsnag_reporting);
    }
    let Invocation::Execute(CliConfiguration::Parse {
        marathonfile,
        output_file_name,
        include_flaky_tests,
    }) = invocation(&["parse"])
    else {
        panic!("parse defaults")
    };
    assert_eq!(String::from_utf16_lossy(&marathonfile.0), "Marathonfile");
    assert_eq!(output_file_name, None);
    assert!(!include_flaky_tests);
    let Invocation::Execute(CliConfiguration::Run {
        analytics_tracking,
        bugsnag_reporting,
        ..
    }) = invocation(&["--analyticsTracking", "false", "--bugsnag", "false"])
    else {
        panic!("root options")
    };
    assert!(!analytics_tracking && !bugsnag_reporting);
    for flag in ["-m", "--marathonfile"] {
        let Invocation::Execute(CliConfiguration::Run { marathonfile, .. }) = invocation(&["run", flag, "custom"])
        else {
            panic!("{flag}")
        };
        assert_eq!(String::from_utf16_lossy(&marathonfile.0), "custom");
        let Invocation::Execute(CliConfiguration::Parse { marathonfile, .. }) = invocation(&["parse", flag, "custom"])
        else {
            panic!("{flag}")
        };
        assert_eq!(String::from_utf16_lossy(&marathonfile.0), "custom");
    }
    for value in ["true", "false", "invalid"] {
        let Invocation::Execute(CliConfiguration::Run {
            analytics_tracking,
            bugsnag_reporting,
            ..
        }) = invocation(&["run", "--analyticsTracking", value, "--bugsnag", value])
        else {
            panic!("{value}")
        };
        assert_eq!(analytics_tracking, value == "true");
        assert_eq!(bugsnag_reporting, value == "true");
        for flag in ["-f", "--include-flaky-tests"] {
            let Invocation::Execute(CliConfiguration::Parse {
                include_flaky_tests, ..
            }) = invocation(&["parse", flag, value])
            else {
                panic!("{flag}")
            };
            assert_eq!(include_flaky_tests, value == "true");
        }
    }
    for flag in ["-o", "--output"] {
        let Invocation::Execute(CliConfiguration::Parse { output_file_name, .. }) =
            invocation(&["parse", flag, "tests"])
        else {
            panic!("{flag}")
        };
        assert_eq!(output_file_name.as_deref(), Some("tests"));
    }
}

#[test]
fn process_exit_codes_for_help_version_errors_and_missing_file() {
    let binary = env!("CARGO_BIN_EXE_marathon");
    for (args, code) in [
        (&["--help"][..], 0),
        (&["version"][..], 0),
        (&["--version"][..], 1),
        (&["--unknown"][..], 1),
        (&["run", "-m", "this-file-does-not-exist"][..], 1),
    ] {
        let status = std::process::Command::new(binary).args(args).output().unwrap().status;
        assert_eq!(status.code(), Some(code), "{args:?}");
    }
}
