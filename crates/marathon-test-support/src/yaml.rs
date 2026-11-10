//! YAML output assertions using the gate's differential reader, independently of configuration binding.
use marathon_config::{
    node,
    tokens::{Tok, tokens},
};
use serde_json::{Value, json};
use std::{
    error::Error,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// The same external reader and canonicalization used for native YAML golden results.
pub fn independent_documents(documents: &[String]) -> Result<Vec<Value>> {
    let script = r#"
import json, pathlib, sys
sys.path.insert(0, str(pathlib.Path.cwd() / 'tools/differential'))
import evidence
print(json.dumps([evidence.structured_yaml(text.encode('utf-8')) for text in json.load(sys.stdin)]))
"#;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut child = Command::new("python3")
        .args(["-I", "-c", script])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().ok_or("reader stdin")?;
    serde_json::to_writer(&mut input, documents)?;
    input.flush()?;
    drop(input);
    let result = child.wait_with_output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
    }
    Ok(serde_json::from_slice(&result.stdout)?)
}

/// Ordered typed tokens from the production loader's parser; float spellings compare as binary values.
pub fn loader_data(text: &str) -> Result<Vec<Value>> {
    let parsed = tokens(&node::load(text));
    if let Some(error) = parsed.error {
        return Err(error.message.into());
    }
    parsed
        .tokens
        .into_iter()
        .map(|token| {
            Ok(match token.tok {
                Tok::Int { value, .. } => json!(["int", value.map_err(|e| format!("{e:?}"))?.to_decimal()]),
                Tok::Float { cleaned, .. } => {
                    json!(["float", jcompat::jnum::parse_double(&cleaned).ok_or("float")?.to_bits()])
                }
                Tok::True(_) => json!(["bool", true]),
                Tok::False(_) => json!(["bool", false]),
                Tok::Null(_) => json!(["null"]),
                other => json!([format!("{other:?}")]),
            })
        })
        .collect()
}

pub fn same_data(actual: &str, expected: &str) -> Result<()> {
    assert_eq!(loader_data(actual)?, loader_data(expected)?, "{actual}");
    let data = independent_documents(&[actual.into(), expected.into()])?;
    assert_eq!(data[0], data[1], "{actual}");
    Ok(())
}

/// Expected ordered reader data, constructed from source values rather than emitted YAML.
pub fn expected_data(value: &Value) -> Value {
    match value {
        Value::String(v) => json!(["scalar", "str", null, v]),
        Value::Bool(v) => json!(["scalar", "bool", null, v]),
        Value::Null => json!(["scalar", "null", null, null]),
        Value::Number(v) => {
            assert!(v.is_i64() || v.is_u64());
            json!(["scalar", "int", null, v.to_string()])
        }
        Value::Array(items) => json!(["seq", null, items.iter().map(expected_data).collect::<Vec<_>>()]),
        Value::Object(entries) => json!([
            "map",
            null,
            entries
                .iter()
                .map(|(k, v)| [expected_data(&json!(k)), expected_data(v)])
                .collect::<Vec<_>>()
        ]),
    }
}

pub fn field<'a>(mapping: &'a mut Value, name: &str) -> &'a mut Value {
    assert_eq!(mapping[0], "map");
    &mut mapping[2]
        .as_array_mut()
        .expect("mapping entries")
        .iter_mut()
        .find(|entry| entry[0] == expected_data(&json!(name)))
        .expect("field")[1]
}

pub fn property_strings() -> Vec<String> {
    let mut strings: Vec<String> = [
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
    ]
    .into_iter()
    .map(String::from)
    .collect();
    // A fixed seed makes failures reproducible; include short ASCII as well as arbitrary Unicode scalars.
    let mut state = 0x0089_2026_u64;
    for index in 0..512 {
        let mut text = String::new();
        for _ in 0..index % 96 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let value = (state >> 32) as u32 % if index % 2 == 0 { 128 } else { 0x110000 };
            if let Some(c) = char::from_u32(value) {
                text.push(c);
            }
        }
        strings.push(text);
    }
    strings
}

pub fn expected_loader_data(value: &Value) -> Vec<Value> {
    fn append(value: &Value, out: &mut Vec<Value>) {
        match value {
            Value::Object(entries) => {
                out.push(json!([format!("{:?}", Tok::StartObject)]));
                for (key, value) in entries {
                    out.push(json!([format!("{:?}", Tok::Field(key.clone()))]));
                    append(value, out);
                }
                out.push(json!([format!("{:?}", Tok::EndObject)]));
            }
            Value::Array(items) => {
                out.push(json!([format!("{:?}", Tok::StartArray)]));
                for item in items {
                    append(item, out);
                }
                out.push(json!([format!("{:?}", Tok::EndArray)]));
            }
            Value::String(v) => out.push(json!([format!("{:?}", Tok::Str(v.clone()))])),
            Value::Number(v) => {
                assert!(v.is_i64() || v.is_u64());
                out.push(json!(["int", v.to_string()]));
            }
            Value::Bool(v) => out.push(json!(["bool", v])),
            Value::Null => out.push(json!(["null"])),
        }
    }
    let mut tokens = vec![];
    append(value, &mut tokens);
    tokens
}
