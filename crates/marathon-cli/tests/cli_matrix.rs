//! WP 2.5: the built `marathon` against the reference CLI matrix (`fixtures/oracle/cli-matrix.jsonl`,
//! `tools/oracle/cli-matrix/generate.py`): stdout, stderr and exit status for every recorded invocation, after the
//! header's normalisation. Rows changed by an approved deviation (D1, D12) or a recorded simplification are compared
//! with their `rust` expectation; `config-dump` rows run `marathon config-dump`. D61 compares parser
//! messages by stable fragments while retaining every exit code and stream.

mod support;

use marathon_cli::application::VERSION;
use serde_json::Value as Json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

/// The header's normalisation, Rust side: the binary prints no SLF4J line and no frames (D12), and its reporter line
/// is the D1 line.
fn normalise(text: &str, sandbox: &[String]) -> String {
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let mut line = line.to_owned();
            for s in sandbox {
                line = line.replace(s.as_str(), "<SANDBOX>");
            }
            let b = line.as_bytes();
            if b.len() > 13
                && b[..12].iter().enumerate().all(|(i, c)| match i {
                    2 | 5 => *c == b':',
                    8 => *c == b'.',
                    _ => c.is_ascii_digit(),
                })
                && b[12] == b' '
            {
                line = format!("<TIME>{}", &line[12..]);
            }
            line = line.replace("[main] INFO  c.m.marathon.cli.ApplicationView - ", "INFO marathon_cli::application: ");
            line = line.replace("[main] ERROR c.m.marathon.cli.ApplicationView - ", "ERROR marathon_cli::application: ");
            line = line.replace(&format!("Starting marathon v{VERSION},"), "Starting marathon v<VERSION>,");
            if line == VERSION {
                line = "<VERSION>".into();
            }
            if line.starts_with("<TIME> INFO marathon_cli::application: Crash reporting and usage analytics are not supported by this build;") {
                line = "<TIME> INFO <D1>".into();
            }
            line
        })
        .collect();
    lines.join("\n")
}

fn strings(v: &Json) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().map(|s| s.as_str().unwrap_or_default().to_owned()).collect())
        .unwrap_or_default()
}

fn pairs(v: &Json) -> Vec<(String, String)> {
    v.as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn prepare(root: &Path, row: &Json) -> std::io::Result<()> {
    let input = root.join("in");
    for d in ["in", "home", "tmp"] {
        fs::create_dir_all(root.join(d))?;
    }
    for d in strings(&row["dirs"]) {
        fs::create_dir_all(input.join(d))?;
    }
    for (path, content) in pairs(&row["files"]) {
        let p = input.join(path);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(p, content)?;
    }
    for (path, target) in pairs(&row["symlinks"]) {
        std::os::unix::fs::symlink(target, input.join(path))?;
    }
    for (path, mode) in pairs(&row["modes"]) {
        let mode = u32::from_str_radix(&mode, 8).map_err(std::io::Error::other)?;
        fs::set_permissions(input.join(path), fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// D61: these fragments describe the rejected input without pinning a parser's prose or layout.
fn parser_error_fragment(id: &str) -> &str {
    match id {
        "help-after-root-error-not-reached"
        | "help-short-equals-is-a-group"
        | "error-unknown-long-suggestion"
        | "error-unknown-long-two"
        | "error-unknown-long-equals"
        | "error-unknown-run"
        | "error-unknown-parse"
        | "error-unknown-version"
        | "error-unknown-version-extra"
        | "error-missing-value-after-unknown" => "--bogus",
        "help-unknown-short-in-group"
        | "error-unknown-short"
        | "error-unknown-short-run"
        | "error-unknown-short-group" => "-x",
        "help-with-value" | "help-with-value-run" => "--help",
        "error-unknown-long-no-suggestion" | "error-option-then-subcommand" | "error-run-extra-after-unknown" => {
            "--frob"
        }
        "error-version-root-option" | "error-missing-value-root" => "-m",
        "error-short-single-dash-long" => "-b",
        "error-unknown-triple-dash" => "---x",
        "error-partial-long-name" => "--marathon",
        "error-partial-help" => "--hel",
        "error-missing-value-long" => "--analyticsTracking",
        "error-missing-value-parse-o" => "--output",
        "error-missing-value-parse-f" => "--include-flaky-tests",
        "error-missing-value-run-bugsnag" => "--bugsnag",
        "error-no-such-subcommand" | "error-subcommand-and-option" => "frob",
        "error-no-such-subcommand-typo" => "rnu",
        "error-no-such-subcommand-near" => "parsee",
        "error-no-such-subcommand-dash" => "'-'",
        "error-root-extra-two" => "'a'",
        "error-version-flag" => "--version",
        "error-version-short-flag" => "-V",
        "error-plus-prefix-equals" => "+a=b",
        "error-double-dash-options-become-arguments" => "-m",
        "error-double-dash-then-unknown-subcommand" => "'b'",
        "argfile-not-expanded" => "@args.txt",
        "error-unknown-short-supplementary" => "-😀",
        "error-run-extra"
        | "error-run-extra-many"
        | "error-parse-extra-two"
        | "error-parse-extra-four"
        | "error-version-extra"
        | "error-run-double-dash-extra" => "unexpected argument",
        _ => panic!("parser error case missing a stable fragment: {id}"),
    }
}

#[test]
fn cli_matrix_matches_the_reference_build() -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/cli-matrix.jsonl");
    let text = fs::read_to_string(path)?;
    let mut lines = text.lines();
    let header: Json = serde_json::from_str(lines.next().unwrap()).unwrap();
    let rows: Vec<Json> = lines.map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.len() as u64, header["rows"].as_u64().unwrap());

    let mut failures = Vec::new();
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        let tmp = tempfile::tempdir()?;
        prepare(tmp.path(), row)?;
        let input = tmp.path().join("in");
        let mut argv = strings(&row["argv"]);
        if row["program"] == "config-dump" {
            argv.insert(0, "config-dump".into());
        }
        let out = support::run(
            &input,
            &tmp.path().join("home"),
            &tmp.path().join("tmp"),
            &argv,
            &pairs(&row["env"]),
        );
        for path in pairs(&row["modes"]).into_iter().map(|(p, _)| p) {
            fs::set_permissions(input.join(path), fs::Permissions::from_mode(0o644))?;
        }
        let mut sandbox = vec![
            input.to_string_lossy().into_owned(),
            input.canonicalize()?.to_string_lossy().into_owned(),
        ];
        sandbox.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let want = if row["rust"].is_null() { row } else { &row["rust"] };
        let expected_exit = want["exit"].as_i64().unwrap();
        let expected_stdout = want["stdout"].as_str().unwrap();
        let expected_stderr = want["stderr"].as_str().unwrap();
        if i64::from(out.exit) != expected_exit
            || out.stdout.is_empty() != expected_stdout.is_empty()
            || out.stderr.is_empty() != expected_stderr.is_empty()
        {
            failures.push(format!(
                "{id}: exit/stream mismatch: exit {}, stdout {:?}, stderr {:?}",
                out.exit, out.stdout, out.stderr
            ));
            continue;
        }
        if expected_stdout.starts_with("Usage:") {
            let expected_target = support::help_target(expected_stdout);
            let actual_target = support::help_target(&out.stdout);
            if expected_target.is_none() || actual_target != expected_target || !out.stdout.contains("--help") {
                failures.push(format!(
                    "{id}: expected help for {expected_target:?}, got {actual_target:?}: {:?}",
                    out.stdout
                ));
            }
            continue;
        }
        if expected_stderr.starts_with("Usage:") {
            let fragment = parser_error_fragment(id);
            if !out.stderr.contains("error:") || !out.stderr.contains(fragment) {
                failures.push(format!("{id}: missing error fragment {fragment:?}: {:?}", out.stderr));
            }
            continue;
        }
        // §3.12.5: the recorded JVM startup prefix is the native one now (WP 8.8).
        let expected_stdout = want["stdout"]
            .as_str()
            .unwrap()
            .replace(
                "com.malinskiy.marathon.cli.args.VersionCommandCliConfiguration@<HASH>",
                "VersionCommandCliConfiguration",
            )
            .replace(
                "[main] INFO  c.m.marathon.cli.ApplicationView - ",
                "INFO marathon_cli::application: ",
            )
            .replace(
                "[main] ERROR c.m.marathon.cli.ApplicationView - ",
                "ERROR marathon_cli::application: ",
            )
            .replace("[main] INFO  <D1>", "INFO <D1>");
        // These closed matrix rows record exactly one generated cause. Other messages stay opaque.
        let generated_cause = matches!(
            id,
            "marathonfile-unreadable"
                | "config-binding-error"
                | "config-empty-file"
                | "config-default-marathonfile-binding-error"
                | "parse-binding-error"
                | "config-dump-now-offset-hour-24"
        );
        let (stdout, expected_stdout) =
            support::cli_error_comparison(&normalise(&out.stdout, &sandbox), &expected_stdout, false);
        let (stderr, expected_stderr) = support::cli_error_comparison(
            &normalise(&out.stderr, &sandbox),
            want["stderr"].as_str().unwrap(),
            generated_cause,
        );
        let got = (stdout, stderr, i64::from(out.exit));
        let expected = (expected_stdout, expected_stderr, want["exit"].as_i64().unwrap());
        if got != expected {
            failures.push(format!("{id} {:?}\n  got  {got:?}\n  want {expected:?}", row["argv"]));
        }
    }
    eprintln!("cli matrix: {}/{} rows pass", rows.len() - failures.len(), rows.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn cli_error_comparison_keeps_external_class_names_in_message_lines() {
    let message = "external java.io.IOException: remote\nCaused by: java.lang.IllegalStateException: remote\n";
    let expected = format!("Exception in thread \"main\" java.io.IOException: {message}");
    let actual = format!("Exception in thread \"main\" I/O error: {message}");
    let (got, want) = support::cli_error_comparison(&actual, &expected, false);
    assert_eq!(got, want);
    let expected = format!(
        "Exception in thread \"main\" java.lang.IllegalStateException: root\nCaused by: java.io.IOException: {message}"
    );
    let actual = format!("Exception in thread \"main\" invalid state: root\nCaused by: I/O error: {message}");
    let (got, want) = support::cli_error_comparison(&actual, &expected, true);
    assert_eq!(got, want);
    assert_ne!(got.replace("java.io.IOException", "I/O error"), want);
}
