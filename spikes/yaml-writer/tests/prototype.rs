//! Port of configuration/src/test/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationSerializationTest.kt.
#[path = "../../../crates/marathon-config/tests/support/load.rs"]
mod load;

use jcompat::writers::{JacksonYamlWriter, JavaNumber, WriterValue as V};
use marathon_config::{Configuration, WriterProjection, serialize};
use serde_json::Value;
use snakeyaml::{
    node,
    tokens::{Tok, tokens},
};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};
use yaml_writer_prototype::write;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn ordered_data(yaml: &str) -> Result<Vec<Value>> {
    let parsed = tokens(&node::load(yaml));
    if let Some(error) = parsed.error {
        return Err(error.message.into());
    }
    parsed
        .tokens
        .into_iter()
        .map(|token| {
            Ok(match token.tok {
                Tok::Int { value, .. } => serde_json::json!(["int", value.map_err(|e| format!("{e:?}"))?.to_decimal()]),
                Tok::Float { cleaned, .. } => {
                    let number = snakeyaml::jnum::parse_double(&cleaned).ok_or("invalid float")?;
                    serde_json::json!(["float", number.to_bits()])
                }
                Tok::True(_) => serde_json::json!(["bool", true]),
                Tok::False(_) => serde_json::json!(["bool", false]),
                Tok::Null(_) => serde_json::json!(["null"]),
                other => serde_json::json!([format!("{other:?}")]),
            })
        })
        .collect()
}

fn compare_projection(value: &V) -> Result<String> {
    let today = JacksonYamlWriter.write(value);
    let new = write(value)?;
    assert_eq!(
        ordered_data(&new)?,
        ordered_data(&today)?,
        "today:\n{today}\nprototype:\n{new}"
    );
    Ok(new)
}

fn round_trip(config: &Configuration, context: &marathon_config::binding::BindingContext) -> Result {
    let yaml = compare_projection(&config.writer_value())?;
    let recreated = marathon_config::loader::MarathonConfigurationLoader
        .parse_sync(yaml.as_bytes(), context)
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(config, &recreated, "{yaml}");
    Ok(())
}

fn collect_yaml(root: &Path, output: &mut Vec<PathBuf>) -> Result {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_yaml(&path, output)?;
        } else if path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
            output.push(path);
        }
    }
    Ok(())
}

#[test]
fn configuration_fixtures_round_trip_with_same_ordered_data() -> Result {
    let (_temp, root) = load::fixture_copy()?;
    let mut cases = vec![];
    collect_yaml(&root, &mut cases)?;
    cases.sort();
    let mut accepted = 0;
    for file in &cases {
        let context = load::context(file, file.parent().ok_or("fixture parent")?, Some("/android-sdk"));
        if let Ok(config) = load::parse(file, &context) {
            round_trip(&config, &context).map_err(|e| format!("{e:?}"))?;
            accepted += 1;
        }
    }
    assert!(accepted >= 17, "only {accepted} accepted configs");
    println!(
        "fixtures: {accepted}/{} accepted configurations round-trip; rejected inputs remain unchanged",
        cases.len()
    );
    Ok(())
}

#[test]
fn validation_corpus_round_trips_every_parseable_configuration() -> Result {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle");
    let text = fs::read_to_string(root.join("coverage-c95b2128/config-validation-corpus.jsonl"))?;
    let mut rows = text
        .lines()
        .skip(1)
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if cfg!(target_os = "linux") {
        for line in fs::read_to_string(root.join("config-validation-corpus-linux.jsonl"))?
            .lines()
            .skip(1)
        {
            let row: Value = serde_json::from_str(line)?;
            let slot = rows.iter_mut().find(|r| r["id"] == row["id"]).ok_or("Linux case")?;
            *slot = row;
        }
    }
    let mut accepted = 0;
    for row in &rows {
        let (_temp, dir) = load::temp()?;
        let (file, base) = if row["kind"] == "case" {
            load::copy_dir(
                &load::fixture_config().join("ios/derivedDataDir"),
                &dir.join("derivedDataDir"),
            )?;
            fs::write(dir.join("app.apk"), "")?;
            fs::create_dir(dir.join("apkdir"))?;
            fs::write(dir.join("known_hosts"), "")?;
            std::os::unix::fs::symlink(dir.join("loop"), dir.join("loop"))?;
            let file = dir.join("Marathonfile");
            fs::write(
                &file,
                format!(
                    "{}\n",
                    row["input"]
                        .as_str()
                        .ok_or("case input")?
                        .replace("@DIR@", &dir.to_string_lossy())
                ),
            )?;
            (file, dir.clone())
        } else {
            load::copy_dir(&load::fixture_config(), &dir)?;
            let file = dir.join(row["input"].as_str().ok_or("fixture input")?);
            let base = file.parent().ok_or("fixture parent")?.to_path_buf();
            (file, base)
        };
        let mut context = load::context(&file, &base, (row["noSdk"] != true).then_some("/android-sdk"));
        context
            .environment
            .extend([("A".into(), "${B}".into()), ("B".into(), "${A}".into())]);
        if let Ok(config) = load::parse(&file, &context) {
            round_trip(&config, &context).map_err(|e| format!("{}: {e}", row["id"]))?;
            accepted += 1;
        } else {
            assert_eq!(row["stage"], "parse", "{}", row["id"]);
        }
    }
    println!(
        "validation corpus: {accepted}/{} parseable configurations round-trip",
        rows.len()
    );
    Ok(())
}

const HAZARDS: &[&str] = &[
    "",
    "yes",
    "NO",
    "On",
    "off",
    "y",
    "N",
    "yEs",
    "oFf",
    "true",
    "False",
    "NULL",
    "~",
    "null",
    "0123",
    "08",
    "1_000",
    "0x10",
    "0b11",
    "1:20",
    "12:34:56.7",
    ".inf",
    "-.Inf",
    ".NaN",
    "NaN",
    "Infinity",
    "-Infinity",
    "2001-12-15",
    "2001-12-15T02:59:43.1Z",
    "<<",
    "=",
    "---",
    "...",
    ": x",
    "a: b",
    "#tag",
    "one\ntwo",
    "one\ntwo\n",
    "one\ntwo\n\n",
    " leading",
    "trailing ",
    "\t\0\u{1b}\r\u{85}\u{2028}\u{2029}",
    "🙂é漢字",
];

#[test]
fn strings_in_parse_shaped_data_and_configuration_survive_yaml_11() -> Result {
    let (_temp, dir) = load::temp()?;
    let file = dir.join("Marathonfile");
    let context = load::context(&file, &dir, None);
    let mut config = marathon_config::loader::MarathonConfigurationLoader
        .parse_sync(
            b"name: test\noutputDir: out\nvendorConfiguration: {type: stub}\n",
            &context,
        )
        .map_err(|e| format!("{e:?}"))?;
    // Fixed seed, varied lengths and every Unicode scalar including the control ranges.
    let mut state = 0x0089_2026_u64;
    let mut strings: Vec<String> = HAZARDS.iter().map(|s| (*s).into()).collect();
    for index in 0..512 {
        let mut text = String::new();
        for _ in 0..index % 96 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            if let Some(c) = char::from_u32((state >> 32) as u32 % 0x110000) {
                text.push(c);
            }
        }
        strings.push(text);
    }
    for text in &strings {
        let value = V::Object(vec![(
            "tests".into(),
            V::Array(vec![V::Object(vec![
                ("pkg".into(), V::String(text.clone())),
                ("clazz".into(), "Class".into()),
                ("method".into(), "method".into()),
                (
                    "metaProperties".into(),
                    V::Array(vec![V::Object(vec![
                        ("name".into(), "ann".into()),
                        (
                            "values".into(),
                            V::Object(vec![(text.clone(), V::String(text.clone()))]),
                        ),
                    ])]),
                ),
            ])]),
        )]);
        let yaml = compare_projection(&value)?;
        let data = ordered_data(&yaml)?;
        assert!(data.contains(&serde_json::json!([format!("{:?}", Tok::Str(text.clone()))])));
        config.name = text.clone();
        round_trip(&config, &context).map_err(|e| format!("{e:?}"))?;
    }
    println!(
        "{} hazard/random strings round-trip in parse-shaped data and configuration names",
        strings.len()
    );
    Ok(())
}

#[test]
fn special_values_null_omission_binary_and_float_types_match_today() -> Result {
    let mut entries = vec![
        ("missing".into(), V::Null),
        ("list".into(), V::Array(vec![V::Null])),
        ("emptyMap".into(), V::Object(vec![])),
        ("emptyList".into(), V::Array(vec![])),
        ("binary".into(), V::Binary(vec![0, 128, 255])),
    ];
    for text in [
        "NaN",
        "Infinity",
        "-Infinity",
        "-0.0",
        "0.0",
        "1.0E10",
        "1.0E-10",
        "9223372036854775807",
    ] {
        entries.push((text.into(), V::Number(JavaNumber::new(text)?)));
    }
    for size in [0, 1, 2, 3, 56, 57, 58, 114] {
        entries.push((format!("bytes{size}"), V::Binary((0..size).map(|n| n as u8).collect())));
    }
    compare_projection(&V::Object(entries))?;
    let (_temp, dir) = load::temp()?;
    let file = dir.join("Marathonfile");
    let context = load::context(&file, &dir, None);
    for special in ["NaN", "Infinity", "-Infinity", "-0.0"] {
        let input = format!(
            "name: test\noutputDir: out\nvendorConfiguration: {{type: stub}}\nanalyticsConfiguration:\n  type: graphite\n  host: localhost\n  defaults:\n    successRate: \"{special}\"\n"
        );
        let config = marathon_config::loader::MarathonConfigurationLoader
            .parse_sync(input.as_bytes(), &context)
            .map_err(|e| format!("{e:?}"))?;
        round_trip(&config, &context).map_err(|e| format!("{e:?}"))?;
        assert!(serialize(&config).contains(&format!("successRate: {special}")));
    }
    Ok(())
}

#[test]
fn candidate_quoting_and_special_float_defaults_are_observable() -> Result {
    for name in [
        "yes",
        "on",
        "0123",
        "1_000",
        "1:20",
        "2001-12-15",
        "<<",
        "yEs",
        "oFf",
        "y",
        "n",
    ] {
        let saphyr = serde_saphyr::to_string(&name)?;
        let fallback = yaml_serde::to_string(&name)?;
        println!("{name:?}: saphyr={saphyr:?}, yaml_serde={fallback:?}");
        assert!(saphyr.starts_with(['\'', '"']), "{name:?}: {saphyr:?}");
    }
    println!(
        "raw NaN: saphyr={:?}, yaml_serde={:?}",
        serde_saphyr::to_string(&f64::NAN)?,
        yaml_serde::to_string(&f64::NAN)?
    );
    Ok(())
}
