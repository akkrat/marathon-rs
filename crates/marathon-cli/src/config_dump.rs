//! Port of tools/oracle/module/src/main/kotlin/oracle/ConfigDump.kt (docs/design/config-dump-format.md).
//!
//! The hidden `marathon config-dump <Marathonfile> [--env-file F] [--now <instant>]` (PLAN §6.3): stage (a) as
//! `execute` runs it, then stage (b), printed as one JSON document with the filesystem effects under the
//! Marathonfile's directory. `--env-file` replaces the environment directly; the JVM tool re-executes itself instead.

use crate::application::{
    CliOutput, Process, canonical_parent, configuration_failure, failure, read_failure, render_uncaught,
};
use jcompat::{
    file_manager::{absolute_file_path, file_path},
    jtime::{Instant, InstantParseError, parse_instant_detailed},
};
use marathon_config::{
    binding::{BindingContext, Profile},
    dump::{DumpValue, canonical_configuration, error_json},
    loader::{LogicalConfigurationValidator, MarathonConfigurationLoader},
};
use marathon_core::error::Failure;
use marathon_error::FailureKind;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::{collections::BTreeMap, fs, path::Path};

const USAGE: &str = "usage: config-dump <Marathonfile> [--env-file F] [--now <ISO instant>]";

/// Runs the command; returns the exit status (0 for every stage, 2 for a usage error, 1 for an uncaught error).
pub fn config_dump(args: &[String], process: &Process, output: &CliOutput) -> i32 {
    let mut file: Option<&str> = None;
    let mut env_file: Option<&str> = None;
    let mut now: Option<Instant> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--env-file" => {
                i += 1;
                match args.get(i) {
                    Some(v) => env_file = Some(v),
                    None => return usage(output, "--env-file needs a value"),
                }
            }
            "--now" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return usage(output, "--now needs a value");
                };
                // `now = Instant.parse(v)` inside the loop: a bad instant escapes `main` before any later argument,
                // usage check or the env file is looked at.
                match parse_instant_detailed(v) {
                    Ok(instant) => now = Some(instant),
                    Err(e) => {
                        output.err(&render_uncaught(&parse_failure(&e)));
                        return 1;
                    }
                }
            }
            _ => {
                if a.starts_with("--") || file.is_some() {
                    return usage(output, &format!("unexpected argument {a}"));
                }
                file = Some(a);
            }
        }
        i += 1;
    }
    let Some(file) = file else {
        return usage(output, "missing Marathonfile");
    };
    match document(file, env_file, now, process) {
        Ok(doc) => {
            output.out(&doc.pretty());
            0
        }
        Err(uncaught) => {
            output.err(&render_uncaught(&uncaught));
            1
        }
    }
}

fn usage(output: &CliOutput, message: &str) -> i32 {
    output.err(&format!("config-dump: {message}\n{USAGE}\n"));
    2
}

/// The `DateTimeParseException` and its causes.
fn parse_failure(error: &InstantParseError) -> Failure {
    error
        .chain
        .iter()
        .rev()
        .fold(None, |cause, (class, message)| {
            Some(Failure {
                kind: class.clone(),
                message: Some(message.to_owned()),
                cause: cause.map(Box::new),
                stacktrace: None,
            })
        })
        .unwrap_or_else(|| failure(FailureKind::DateTimeParse, ""))
}

/// `readEnvFile`: `KEY=VALUE` lines; blank lines and `#` lines are skipped; the value follows the first `=`.
fn read_env_file(path: &str) -> Result<Vec<(String, String)>, Failure> {
    let text = fs::read_to_string(path).map_err(|e| read_failure(&file_path(path), &e))?;
    let mut env: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        match line.find('=') {
            Some(eq) if eq > 0 => {
                let key = String::from(&line[..eq]);
                let value = String::from(&line[eq + 1..]);
                // `associate`: a later line replaces an earlier one in place.
                match env.iter_mut().find(|(k, _)| *k == key) {
                    Some(entry) => entry.1 = value,
                    None => env.push((key, value)),
                }
            }
            _ => {
                return Err(failure(
                    FailureKind::IllegalArgument,
                    &format!("not a KEY=VALUE line in {path}: {line}"),
                ));
            }
        }
    }
    Ok(env)
}

/// `dumpConfig`: the document for `file`, with the environment from `env_file` (else the process's) and `now` (else
/// the process's clock). `Err` is what escapes `main`.
pub fn document(
    file: &str,
    env_file: Option<&str>,
    now: Option<Instant>,
    process: &Process,
) -> Result<DumpValue, Failure> {
    let environment = match env_file {
        Some(path) => read_env_file(path)?,
        None => process.environment.clone(),
    };
    let now = now.unwrap_or(process.now);
    let cwd = PathBuf::from(process.cwd.to_string_lossy().into_owned());
    let given = file_path(file);
    let absolute = absolute_file_path(&given.to_string_lossy(), &cwd.to_string_lossy())
        .to_string_lossy()
        .into_owned();
    let parent = Path::new(&absolute).parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = if parent.exists() {
        fs::canonicalize(&parent).map_err(|e| io_failure(&e))?
    } else {
        parent
    };

    let before = snapshot(&dir)?;
    let (stage, error, config) = parse_and_validate(&given, &absolute, &cwd, environment, now);
    let after = snapshot(&dir)?;
    Ok(DumpValue::object(vec![
        ("stage", DumpValue::str(stage)),
        ("error", error.map_or(DumpValue::Null, |e| failure_json(&e))),
        (
            "config",
            config.map_or(DumpValue::Null, |c| canonical_configuration(&c, &cwd)),
        ),
        ("fsEffects", fs_effects(&before, &after)),
    ]))
}

fn failure_json(f: &Failure) -> DumpValue {
    let mut causes = Vec::new();
    let mut cause = f.cause.as_deref();
    while let Some(c) = cause {
        causes.push((c.kind.legacy_class().to_owned(), c.message.to_owned()));
        cause = c.cause.as_deref();
    }
    error_json(f.kind.legacy_class(), f.message.as_deref(), &causes)
}

/// `parseAndValidate`: stage (a) with both tracking flags true, then the logical validator.
fn parse_and_validate(
    given: &std::path::Path,
    absolute: &str,
    cwd: &std::path::Path,
    environment: Vec<(String, String)>,
    now: Instant,
) -> (&'static str, Option<Failure>, Option<marathon_config::Configuration>) {
    let parsed = (|| {
        if !fs::metadata(absolute).is_ok_and(|m| m.is_file()) {
            return Err(failure(
                FailureKind::Config(marathon_error::ConfigKind::Rejected),
                &format!("No config {absolute} present"),
            ));
        }
        let bytes = fs::read(absolute).map_err(|e| read_failure(given, &e))?;
        let context = BindingContext {
            profile: Profile::Marathonfile,
            marathonfile: given.to_path_buf(),
            analytics_tracking: true,
            bugsnag_reporting: true,
            marathonfile_dir: canonical_parent(absolute)?,
            process_dir: cwd.to_path_buf(),
            environment,
            now,
        };
        MarathonConfigurationLoader
            .parse_sync(&bytes, &context)
            .map_err(|e| configuration_failure(&e))
    })();
    match parsed {
        Err(e) => ("parse", Some(e), None),
        Ok(config) => match LogicalConfigurationValidator.validate_sync(&config) {
            Ok(()) => ("ok", None, Some(config)),
            Err(e) => ("logical", Some(configuration_failure(&e)), Some(config)),
        },
    }
}

fn io_failure(e: &std::io::Error) -> Failure {
    failure(FailureKind::UncheckedIo, &e.to_string())
}

#[derive(Clone, PartialEq, Eq)]
struct Entry {
    kind: &'static str,
    size: Option<u64>,
    sha256: Option<String>,
}

/// The oracle's `TreeMap<String, …>` key: Java's `String.compareTo` order (UTF-16 code units).
type Key = Vec<u16>;

/// Every path under `root`, relative to it and `/`-separated, without following symbolic links.
fn snapshot(root: &Path) -> Result<BTreeMap<Key, Entry>, Failure> {
    let mut out = BTreeMap::new();
    if root.is_dir() {
        walk(root, "", &mut out).map_err(|e| io_failure(&e))?;
    }
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<Key, Entry>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let rel = format!("{prefix}{}", entry.file_name().to_string_lossy());
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        let value = if meta.file_type().is_symlink() {
            Entry {
                kind: "symlink",
                size: None,
                sha256: Some(fs::read_link(&path)?.to_string_lossy().into_owned()),
            }
        } else if meta.is_dir() {
            walk(&path, &format!("{rel}/"), out)?;
            Entry {
                kind: "dir",
                size: None,
                sha256: None,
            }
        } else {
            let bytes = fs::read(&path)?;
            let digest = Sha256::digest(&bytes);
            Entry {
                kind: "file",
                size: Some(meta.len()),
                sha256: Some(digest.iter().map(|b| format!("{b:02x}")).collect()),
            }
        };
        out.insert(rel.encode_utf16().collect(), value);
    }
    Ok(())
}

/// `fsEffects`: created, deleted or modified paths in path order, each described after the change (before it for
/// `deleted`).
fn fs_effects(before: &BTreeMap<Key, Entry>, after: &BTreeMap<Key, Entry>) -> DumpValue {
    let mut paths: Vec<&Key> = before.keys().chain(after.keys()).collect();
    paths.sort();
    paths.dedup();
    let effects = paths
        .into_iter()
        .filter_map(|path| {
            let (b, a) = (before.get(path), after.get(path));
            let (op, e) = match (b, a) {
                (None, Some(a)) => ("created", a),
                (Some(b), None) => ("deleted", b),
                (Some(b), Some(a)) if a != b => ("modified", a),
                _ => return None,
            };
            Some(DumpValue::object(vec![
                ("op", DumpValue::str(op)),
                ("path", DumpValue::Str(String::from_utf16_lossy(&path.clone()))),
                ("kind", DumpValue::str(e.kind)),
                ("size", e.size.map_or(DumpValue::Null, |s| DumpValue::Int(s as i64))),
                ("sha256", e.sha256.as_deref().map_or(DumpValue::Null, DumpValue::str)),
            ]))
        })
        .collect();
    DumpValue::Array(effects)
}
