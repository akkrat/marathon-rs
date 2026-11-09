//! WP 2.5: the built `marathon` against the reference CLI matrix (`fixtures/oracle/cli-matrix.jsonl`,
//! `tools/oracle/cli-matrix/generate.py`): stdout, stderr and exit status for every recorded invocation, after the
//! header's normalisation. Rows changed by an approved deviation (D1, D12) or a recorded simplification are compared
//! with their `rust` expectation; `config-dump` rows run `marathon config-dump`.

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

#[test]
fn cli_matrix_matches_the_reference_build() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/cli-matrix.jsonl");
    let text = fs::read_to_string(path).unwrap();
    let mut lines = text.lines();
    let header: Json = serde_json::from_str(lines.next().unwrap()).unwrap();
    let rows: Vec<Json> = lines.map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.len() as u64, header["rows"].as_u64().unwrap());

    let mut failures = Vec::new();
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        prepare(tmp.path(), row).unwrap();
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
            fs::set_permissions(input.join(path), fs::Permissions::from_mode(0o644)).unwrap();
        }
        let mut sandbox = vec![
            input.to_string_lossy().into_owned(),
            input.canonicalize().unwrap().to_string_lossy().into_owned(),
        ];
        sandbox.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let want = if row["rust"].is_null() { row } else { &row["rust"] };
        let expected_stdout = want["stdout"].as_str().unwrap().replace(
            "com.malinskiy.marathon.cli.args.VersionCommandCliConfiguration@<HASH>",
            "VersionCommandCliConfiguration",
        );
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
