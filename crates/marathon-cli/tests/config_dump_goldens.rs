//! WP 2.5: `marathon config-dump` against the 30 JVM goldens (`fixtures/oracle/config-dump/`,
//! `tools/oracle/config-dump-goldens.sh`), whole documents byte for byte: stage, error, canonical configuration and
//! filesystem effects, after docs/design/config-dump-format.md's path normalisation. Jackson-raised cause messages
//! compare by class only (PLAN §3.10).

mod support;

use serde_json::Value as Json;
use std::{fs, io, path::Path};

fn copy_dir(src: &Path, dst: &Path, only_yaml: bool) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target, only_yaml)?;
        } else if !only_yaml || entry.path().extension().is_some_and(|e| e == "yaml") {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The golden without its `golden` metadata key (the script appends it as the last key).
fn document_of(golden: &str) -> &str {
    let at = golden.rfind(",\n  \"golden\": ").expect("a golden key");
    &golden[..at]
}

#[test]
fn config_dump_matches_the_jvm_goldens() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let mut names: Vec<_> = fs::read_dir(root.join("oracle/coverage-c95b2128/config-dump"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    assert_eq!(names.len(), 34);
    let mut failures = Vec::new();
    for path in &names {
        let text = fs::read_to_string(path).unwrap();
        let meta: Json = serde_json::from_str::<Json>(&text).unwrap()["golden"].clone();
        let tmp = tempfile::tempdir().unwrap();
        let sandbox = tmp.path().join("config");
        copy_dir(&root.join("configuration/fixture/config"), &sandbox, false).unwrap();
        copy_dir(&root.join("oracle/config-dump-inputs"), &sandbox, true).unwrap();
        let env_file = tmp.path().join("env");
        let env: String = meta["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| format!("{k}={}\n", v.as_str().unwrap()))
            .collect();
        fs::write(&env_file, env).unwrap();
        let input = sandbox.join(meta["input"].as_str().unwrap());
        let args = [
            "config-dump".to_owned(),
            input.to_string_lossy().into_owned(),
            "--env-file".into(),
            env_file.to_string_lossy().into_owned(),
            "--now".into(),
            meta["now"].as_str().unwrap().to_owned(),
        ];
        let out = support::run(&sandbox, tmp.path(), tmp.path(), &args, &[]);
        let mut spellings = vec![
            sandbox.to_string_lossy().into_owned(),
            sandbox.canonicalize().unwrap().to_string_lossy().into_owned(),
        ];
        spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let mut got = out.stdout.clone();
        for s in &spellings {
            got = got.replace(s.as_str(), "<FIXTURE_CONFIG>");
        }
        let got = got.replace("/System/Volumes/Data/", "/");
        let got = support::jackson_classes_only(got.strip_suffix("\n}\n").unwrap_or(&got));
        let want = support::jackson_classes_only(document_of(&text));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if out.exit != 0 || got != want {
            let line = got
                .lines()
                .zip(want.lines())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| got.lines().count().min(want.lines().count()));
            failures.push(format!(
                "{name}: exit {} stderr {:?}\n  first difference at line {}: got {:?} want {:?}",
                out.exit,
                out.stderr,
                line + 1,
                got.lines().nth(line),
                want.lines().nth(line)
            ));
        }
    }
    eprintln!(
        "config-dump goldens: {}/{} match",
        names.len() - failures.len(),
        names.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
