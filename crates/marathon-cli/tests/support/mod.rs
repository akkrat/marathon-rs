//! Shared helpers for the binary-level tests: running `marathon` with a clean environment and the normalisations the
//! JVM recordings use.
#![allow(dead_code)]

use std::{
    path::Path,
    process::{Command, Stdio},
};

pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub exit: i32,
}

/// Runs the built binary in `cwd` with only HOME, TMPDIR, PATH and `env` set and stdin closed.
pub fn run(cwd: &Path, home: &Path, tmp: &Path, args: &[String], env: &[(String, String)]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_marathon"))
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", home)
        .env("TMPDIR", format!("{}/", tmp.display()))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .output()
        .expect("the marathon binary runs");
    Output {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        exit: out.status.code().unwrap_or(-1),
    }
}

/// D59 changes only the generated error slot; all bytes outside it stay pinned.
pub fn config_error_comparison(actual: &str, expected: &str) -> (String, String) {
    use marathon_error::{ConfigKind, FailureKind};
    use marathon_test_support::recorded_errors::{config_error, kind};
    use serde_json::Value;
    fn span(text: &str) -> std::ops::Range<usize> {
        let start = text.find("  \"error\": ").expect("error field") + "  \"error\": ".len();
        let end = start + text[start..].find(",\n  \"config\":").expect("config follows error");
        start..end
    }
    let a = span(actual);
    let e = span(expected);
    let mut got: Value = serde_json::from_str(&actual[a.clone()]).expect("native error JSON");
    let mut want: Value = serde_json::from_str(&expected[e.clone()]).expect("recorded error JSON");
    let recorded_causes = want["causes"].as_array().cloned().unwrap_or_default();
    config_error(&mut want);
    if !got.is_null() {
        assert_eq!(
            got.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(),
            ["kind", "message", "causes"]
        );
        assert_eq!(got["causes"].as_array().unwrap().len(), recorded_causes.len());
    }
    for (i, cause) in recorded_causes.iter().enumerate() {
        let (class, _) = cause.as_str().unwrap().split_once(": ").expect("recorded cause");
        if let FailureKind::Config(k) = kind(class)
            && k != ConfigKind::Rejected
            && k.is_json()
        {
            let description = k.to_string();
            assert!(
                got["causes"][i]
                    .as_str()
                    .unwrap()
                    .starts_with(&(description.clone() + ": "))
            );
            got["causes"][i] = description.clone().into();
            want["causes"][i] = description.into();
        }
    }
    assert_eq!(got, want, "generated error kind, message and ordered causes");
    let mut actual = actual.to_owned();
    let mut expected = expected.to_owned();
    actual.replace_range(a, "<checked error>");
    expected.replace_range(e, "<checked error>");
    (actual, expected)
}

/// Only the CLI's generated envelope; the caller identifies recorded cause-bearing rows.
pub fn cli_error_comparison(actual: &str, expected: &str, generated_cause: bool) -> (String, String) {
    use marathon_error::FailureKind;
    use marathon_test_support::recorded_errors::{kind, rendered};
    if serde_json::from_str::<serde_json::Value>(expected).is_ok_and(|v| v.get("stage").is_some()) {
        return config_error_comparison(actual, expected);
    }
    const ENVELOPE: &str = "Exception in thread \"main\" ";
    let Some((prefix, trace)) = expected.split_once(ENVELOPE) else {
        return (actual.into(), expected.into());
    };
    if !generated_cause {
        return (
            actual.into(),
            format!(
                "{prefix}{ENVELOPE}{}",
                rendered(trace).unwrap_or_else(|e| panic!("{e}"))
            ),
        );
    }
    const CAUSE: &str = "\nCaused by: ";
    let (root, cause) = trace.split_once(CAUSE).expect("recorded generated cause");
    let class = cause
        .split_once(": ")
        .map_or(cause.trim_end_matches('\n'), |(class, _)| class);
    let typed = kind(class);
    let jackson = matches!(typed, FailureKind::Config(k) if k.is_json());
    let cause = if jackson {
        format!("{typed}\n")
    } else {
        rendered(cause).unwrap_or_else(|e| panic!("{e}"))
    };
    let expected = format!(
        "{prefix}{ENVELOPE}{}{CAUSE}{cause}",
        rendered(root).unwrap_or_else(|e| panic!("{e}")),
    );
    let actual = if jackson {
        let (root, cause) = actual.split_once(CAUSE).expect("native generated cause");
        let (description, _) = cause.split_once(": ").expect("native diagnostic message");
        format!("{root}{CAUSE}{description}\n")
    } else {
        actual.into()
    };
    (actual, expected)
}
