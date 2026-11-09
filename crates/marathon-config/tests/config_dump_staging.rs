//! WP 2.4 against the 30 JVM config-dump goldens (`fixtures/oracle/config-dump/`,
//! `tools/oracle/config-dump-goldens.sh`): the production loader and logical
//! validator reach the same stage with the same error class, message and
//! cause classes, every one in order. The canonical configuration and the filesystem effects
//! are WP 2.6's comparison (through WP 2.5's `config-dump`).

#[path = "support/load.rs"]
mod load;

use serde_json::Value as Json;
use std::{fs, path::Path};

#[test]
fn loader_and_validator_staging_matches_the_config_dump_goldens() {
    let goldens = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/coverage-c95b2128/config-dump");
    let extra = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/config-dump-inputs");
    let mut names: Vec<_> = fs::read_dir(&goldens).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    assert_eq!(names.len(), 34);
    let mut failures = Vec::new();
    for golden_path in names {
        let golden: Json = serde_json::from_str(&fs::read_to_string(&golden_path).unwrap()).unwrap();
        let meta = &golden["golden"];
        // The script's sandbox: the fixture configs plus the extra inputs.
        let (_tmp, sandbox) = load::fixture_copy().unwrap();
        load::copy_dir(&extra, &sandbox).unwrap();
        let file = sandbox.join(meta["input"].as_str().unwrap());
        let sdk = meta["env"]["ANDROID_HOME"].as_str();
        let context = load::context(&file, file.parent().unwrap(), sdk);
        let (stage, error) = match load::parse(&file, &context) {
            Err(e) => ("parse", Some(e)),
            Ok(config) => match load::validate(&config) {
                Err(e) => ("logical", Some(e)),
                Ok(()) => ("ok", None),
            },
        };
        let clean = |s: String| {
            s.replace(&*sandbox.to_string_lossy(), "<FIXTURE_CONFIG>")
                .replace("/System/Volumes/Data/", "/")
        };
        let got = error.map(|e| {
            (
                load::class(&e),
                clean(load::message(&e)),
                e.causes
                    .iter()
                    .map(|c| c.kind.legacy_class().to_owned())
                    .collect::<Vec<_>>(),
            )
        });
        let want = golden["error"].as_object().map(|e| {
            (
                e["class"].as_str().unwrap().to_owned(),
                e["message"].as_str().unwrap_or_default().to_owned(),
                e["causes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c.as_str().unwrap().split(':').next().unwrap().to_owned())
                    .collect::<Vec<_>>(),
            )
        });
        let name = golden_path.file_name().unwrap().to_string_lossy().into_owned();
        if stage != golden["stage"] || got != want {
            failures.push(format!("{name}: {stage} {got:?}\n  want {} {want:?}", golden["stage"]));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
