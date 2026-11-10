//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommand.kt
//! and the metadata/binary cases from the command-mapper oracle.
use marathon_core::{model::*, report::projection::parse_results};
use marathon_test_support::yaml::{self, expected_data, expected_loader_data, loader_data};
use serde_json::{Value, json};
use std::{error::Error, sync::Arc};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
fn test(value: MetaValue) -> Test {
    Test {
        pkg: "p".into(),
        clazz: "C".into(),
        method: "m".into(),
        meta_properties: vec![MetaProperty {
            name: "ann".into(),
            values: [
                ("edge".into(), value),
                ("minimum".into(), MetaValue::Long(i64::MIN)),
                ("maximum".into(), MetaValue::Long(i64::MAX)),
            ]
            .into_iter()
            .collect(),
        }],
    }
}

#[test]
fn parse_string_properties_match_source_data_in_both_readers() -> Result {
    let strings = yaml::property_strings();
    let mut documents = vec![];
    let mut expected = vec![];
    for text in &strings {
        let test = Test {
            pkg: text.clone(),
            clazz: text.clone(),
            method: text.clone(),
            meta_properties: vec![MetaProperty {
                name: text.clone(),
                values: [(text.clone(), MetaValue::String(text.clone()))].into_iter().collect(),
            }],
        };
        let mut values = serde_json::Map::new();
        values.insert(text.clone(), json!(text));
        let source = json!({"tests": [{"pkg": text, "clazz": text, "method": text, "metaProperties": [{"name": text, "values": values}]}]});
        let document = parse_results(&[test], &[])?;
        assert_eq!(loader_data(&document)?, expected_loader_data(&source), "{text:?}");
        documents.push(document);
        expected.push(json!([expected_data(&source)]));
    }
    assert_eq!(yaml::independent_documents(&documents)?, expected);
    Ok(())
}

#[test]
fn nel_metadata_key_round_trips_without_folding() -> Result {
    let key = "a\u{85}b";
    let model = Test {
        meta_properties: vec![MetaProperty {
            name: "ann".into(),
            values: [(key.into(), MetaValue::String("v".into()))].into_iter().collect(),
        }],
        ..test(MetaValue::Null)
    };
    let mut values = serde_json::Map::new();
    values.insert(key.into(), json!("v"));
    let source = json!({"tests": [{"pkg": "p", "clazz": "C", "method": "m", "metaProperties": [{"name": "ann", "values": values}]}]});
    let document = parse_results(&[model], &[])?;
    assert_eq!(loader_data(&document)?, expected_loader_data(&source));
    assert_eq!(
        yaml::independent_documents(&[document])?,
        [json!([expected_data(&source)])]
    );
    Ok(())
}

#[test]
fn parse_numeric_edges_retain_legacy_decimal_data_and_scalar_types() -> Result {
    // Captured from the old command projection before removing it, including its shortest subnormal spellings.
    let doubles = [
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (-0.0, "-0.0"),
        (f64::MAX, "1.7976931348623157E308"),
        (f64::MIN_POSITIVE, "2.2250738585072014E-308"),
        (f64::from_bits(1), "4.9E-324"),
    ];
    let floats = [
        (f32::NAN, "NaN"),
        (f32::INFINITY, "Infinity"),
        (f32::NEG_INFINITY, "-Infinity"),
        (-0.0, "-0.0"),
        (f32::MAX, "3.4028235E38"),
        (f32::from_bits(1), "1.4E-45"),
    ];
    for (value, spelling) in doubles
        .into_iter()
        .map(|(v, s)| (MetaValue::Double(v), s))
        .chain(floats.into_iter().map(|(v, s)| (MetaValue::Float(v), s)))
    {
        let document = parse_results(&[test(value)], &[])?;
        let before = format!(
            "---\ntests:\n- pkg: \"p\"\n  clazz: \"C\"\n  method: \"m\"\n  metaProperties:\n  - name: \"ann\"\n    values:\n      edge: {spelling}\n      minimum: -9223372036854775808\n      maximum: 9223372036854775807\n"
        );
        yaml::same_data(&document, &before)?;
        let data = loader_data(&document)?;
        if matches!(spelling, "NaN" | "Infinity" | "-Infinity") {
            assert!(data.contains(&json!([format!(
                "{:?}",
                marathon_config::tokens::Tok::Str(spelling.into())
            )])));
        } else {
            assert!(data.contains(&json!([
                "float",
                jcompat::jnum::parse_double(spelling).ok_or("baseline float")?.to_bits()
            ])));
        }
        assert!(data.contains(&json!(["int", i64::MIN.to_string()])));
        assert!(data.contains(&json!(["int", i64::MAX.to_string()])));
    }
    Ok(())
}

#[test]
fn primitive_arrays_nulls_and_decoded_metadata_keep_their_shapes() -> Result {
    let byte_array = |bytes: Vec<MetaValue>| {
        MetaValue::Array(Arc::new(MetaArray {
            jvm_class: "[B".into(),
            elements: bytes,
        }))
    };
    for size in [0, 1, 2, 3, 56, 57, 58, 114] {
        let bytes: Vec<u8> = (0..size).map(|v| v as u8).collect();
        let value = byte_array(bytes.iter().map(|v| MetaValue::Byte(*v as i8)).collect());
        let document = parse_results(&[test(MetaValue::List(vec![value]))], &[])?;
        assert!(
            loader_data(&document)?.contains(&json!([format!("{:?}", marathon_config::tokens::Tok::Embedded(bytes))]))
        );
    }
    let chars = MetaValue::Array(Arc::new(MetaArray {
        jvm_class: "[C".into(),
        elements: vec![MetaValue::Char(0xD83D), MetaValue::Char(0xDE00)],
    }));
    let malformed = byte_array(vec![MetaValue::Byte(1), MetaValue::String("x".into())]);
    let decoded = MetaValue::Decoded(DecodedWrapper::DecodedArrayValue(
        vec![
            DecodedWrapper::DecodedNull,
            DecodedWrapper::DecodedDouble(f64::INFINITY),
        ]
        .into(),
    ));
    let map = MetaValue::Map(
        [
            ("missing".into(), MetaValue::Null),
            (
                "values".into(),
                MetaValue::List(vec![MetaValue::Null, chars, malformed, decoded]),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let document = parse_results(
        &[Test {
            meta_properties: vec![MetaProperty {
                name: "ann".into(),
                values: [("edge".into(), map)].into_iter().collect(),
            }],
            ..test(MetaValue::Null)
        }],
        &[],
    )?;
    let expected: Value = json!({"tests":[{"pkg":"p","clazz":"C","method":"m","metaProperties":[{"name":"ann","values":{"edge":{"values":[null,"😀",[1,"x"],{"values":[{},{"value":"Infinity"}]}]}}}]}]});
    assert_eq!(loader_data(&document)?, expected_loader_data(&expected));
    assert_eq!(
        yaml::independent_documents(&[document])?,
        [json!([expected_data(&expected)])]
    );
    Ok(())
}
