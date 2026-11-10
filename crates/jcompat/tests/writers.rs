use jcompat::writers::KotlinValue;
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
