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

/// PLAN §3.10: messages raised by Jackson may be worded differently; their classes and order may not. A cause
/// `com.fasterxml.jackson.<Class>: <message>` keeps only its class, in a `Caused by:` line (dropping the message's
/// continuation lines) or in a config-dump `causes` string.
pub fn jackson_classes_only(text: &str) -> String {
    let mut out = Vec::new();
    let mut in_jackson_cause = false;
    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix("Caused by: com.fasterxml.jackson.") {
            let class = rest.split(':').next().unwrap_or(rest);
            out.push(format!("Caused by: com.fasterxml.jackson.{class}"));
            in_jackson_cause = true;
            continue;
        }
        if in_jackson_cause && !line.starts_with("Caused by: ") && !line.is_empty() {
            continue;
        }
        in_jackson_cause = false;
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("\"com.fasterxml.jackson.") {
            let indent = &line[..line.len() - trimmed.len()];
            let class = rest.split(':').next().unwrap_or(rest);
            let comma = if line.ends_with(',') { "," } else { "" };
            out.push(format!("{indent}\"com.fasterxml.jackson.{class}\"{comma}"));
            continue;
        }
        out.push(line.to_owned());
    }
    out.join("\n")
}
