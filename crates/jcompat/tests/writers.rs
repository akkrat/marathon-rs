use jcompat::writers::{JacksonYamlWriter, JavaNumber, KotlinValue, WriterValue};
use serde_json::Value;
fn units(v: &Value) -> String {
    String::from_utf16_lossy(
        &v.as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_u64().unwrap() as u16)
            .collect::<Vec<_>>(),
    )
}
fn input(v: &Value) -> WriterValue {
    match v["kind"].as_str().unwrap() {
        "null" => WriterValue::Null,
        "bool" => WriterValue::Bool(v["value"].as_bool().unwrap()),
        "number" => WriterValue::Number(JavaNumber::new(v["value"].as_str().unwrap()).unwrap()),
        "string" => WriterValue::String(units(&v["units"])),
        "array" => WriterValue::Array(v["items"].as_array().unwrap().iter().map(input).collect()),
        "object" => WriterValue::Object(
            v["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| (units(&e[0]), input(&e[1])))
                .collect(),
        ),
        _ => panic!("bad oracle input"),
    }
}
#[test]
fn jvm_writer_edges() -> Result<(), Box<dyn std::error::Error>> {
    check_writer_rows(include_str!("../../../fixtures/oracle/jcompat/writer_edges.jsonl"))
}
#[test]
fn jvm_writer_round1() -> Result<(), Box<dyn std::error::Error>> {
    let table = include_str!("../../../fixtures/oracle/jcompat/writer_round1.jsonl");
    assert_eq!(table.lines().count() - 1, 18);
    check_writer_rows(table)
}
fn check_writer_rows(table: &str) -> Result<(), Box<dyn std::error::Error>> {
    for line in table.lines().skip(1) {
        let row: Value = serde_json::from_str(line)?;
        let value = input(&row["input"]);
        // Same pinned YAML writer, with only the input units normalized for D55.
        let native = include_str!("../../../fixtures/oracle/jcompat/writer_native_strings.jsonl")
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let row = native.iter().find(|r| r["id"] == row["id"]).unwrap_or(&row);
        assert!(
            row["yaml"].get("ok").is_some(),
            "unhandled JVM YAML error: {}",
            row["id"]
        );
        {
            let actual = JacksonYamlWriter.write(&value);
            assert_eq!(actual.as_bytes().to_vec(), bytes(&row["yaml"]["bytes"]));
            assert_eq!(
                actual,
                units(&row["yaml"]["ok"]),
                "{}\nactual {:?}\nexpected {:?}",
                row["id"],
                actual,
                units(&row["yaml"]["ok"])
            );
        }
    }
    Ok(())
}
fn dc(name: &str, fields: Vec<(&str, KotlinValue)>) -> KotlinValue {
    KotlinValue::DataClass {
        name: name.into(),
        fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    }
}
#[test]
fn kotlin_events_oracle() -> Result<(), Box<dyn std::error::Error>> {
    let test = dc(
        "Test",
        vec![
            ("pkg", "sample_appUITests".into()),
            ("clazz", "FlakyTests".into()),
            ("method", "testTextFlaky1".into()),
            ("metaProperties", KotlinValue::List(vec![])),
        ],
    );
    let rows: Vec<Value> = include_str!("../../../fixtures/oracle/jcompat/kotlin_to_string.jsonl")
        .lines()
        .skip(1)
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    for (id, start, end, trace) in [
        ("TestStarted", None, None, None),
        ("TestPassed", Some("1537187691585"), Some("1537187696000"), None),
        ("TestFailed", Some("1"), Some("2"), Some(Some("a\nb, c=(d)"))),
        ("TestFailed-null", Some("1"), Some("2"), Some(None)),
        ("TestIgnored", Some("0"), Some("0"), None),
    ] {
        let mut fields = vec![("id", test.clone())];
        if let Some(s) = start {
            fields.push(("startTime", s.into()));
        }
        if let Some(s) = end {
            fields.push(("endTime", s.into()));
        }
        if let Some(trace) = trace {
            fields.push(("trace", trace.map_or(KotlinValue::Null, Into::into)));
        }
        let actual = dc(if id == "TestFailed-null" { "TestFailed" } else { id }, fields).render();
        assert_eq!(
            actual,
            rows.iter().find(|r| r["id"] == id).unwrap()["toString"]
                .as_str()
                .unwrap()
        );
    }
    Ok(())
}

#[test]
fn existing_report_and_yaml_oracles() -> Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in include_str!("../../../fixtures/oracle/jcompat/writer_models.jsonl")
        .lines()
        .skip(1)
    {
        let row: Value = serde_json::from_str(line)?;
        if row["writer"] != "yaml" {
            continue;
        }
        let actual = JacksonYamlWriter.write(&input(&row["input"]));
        assert_eq!(actual, units(&row["expected"]["ok"]), "{}", row["id"]);
        count += 1;
    }
    assert_eq!(count, 9);
    Ok(())
}

fn kotlin_input(v: &Value) -> KotlinValue {
    match v["kind"].as_str().unwrap() {
        "null" => KotlinValue::Null,
        "text" => KotlinValue::Text(units(&v["units"])),
        "data" => KotlinValue::DataClass {
            name: v["name"].as_str().unwrap().into(),
            fields: v["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| (p[0].as_str().unwrap().into(), kotlin_input(&p[1])))
                .collect(),
        },
        "list" => KotlinValue::List(v["items"].as_array().unwrap().iter().map(kotlin_input).collect()),
        "tuple" => KotlinValue::Tuple(v["items"].as_array().unwrap().iter().map(kotlin_input).collect()),
        "map" => KotlinValue::Map(
            v["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| (kotlin_input(&p[0]), kotlin_input(&p[1])))
                .collect(),
        ),
        _ => panic!("bad Kotlin input"),
    }
}
#[test]
fn all_kotlin_writer_models() -> Result<(), Box<dyn std::error::Error>> {
    let original: Vec<Value> = include_str!("../../../fixtures/oracle/jcompat/kotlin_to_string.jsonl")
        .lines()
        .skip(1)
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let mut count = 0;
    for line in include_str!("../../../fixtures/oracle/jcompat/kotlin_writer_models.jsonl")
        .lines()
        .skip(1)
    {
        let row: Value = serde_json::from_str(line)?;
        let expected = &original.iter().find(|r| r["id"] == row["id"]).unwrap()["toString"];
        assert_eq!(&row["toString"], expected);
        if let Some(inputs) = row["inputs"].as_object() {
            for (key, value) in inputs {
                assert_eq!(
                    kotlin_input(value).render(),
                    expected[key].as_str().unwrap(),
                    "{} {key}",
                    row["id"]
                );
            }
        } else {
            assert_eq!(
                kotlin_input(&row["input"]).render(),
                expected.as_str().unwrap(),
                "{}",
                row["id"]
            );
        }
        count += 1;
    }
    assert_eq!(count, 11);
    Ok(())
}

#[test]
fn kotlin_utf16_edges() -> Result<(), Box<dyn std::error::Error>> {
    for line in include_str!("../../../fixtures/oracle/jcompat/kotlin_writer_edges.jsonl")
        .lines()
        .skip(1)
    {
        let row: Value = serde_json::from_str(line)?;
        assert_eq!(
            kotlin_input(&row["input"]).render(),
            units(&row["expected"]),
            "{}",
            row["id"]
        );
    }
    Ok(())
}

fn bytes(v: &Value) -> Vec<u8> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u8)
        .collect()
}
