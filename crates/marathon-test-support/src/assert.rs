//! Port of vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/assert/FileExtensions.kt
//!
//! `shouldBeEqualToAsJson` is JSONAssert's LENIENT mode: objects may carry extra fields and
//! arrays match in any order (one actual element per expected element). `shouldBeEqualToAsXML`
//! is XMLUnit's identical comparison ignoring whitespace between and around text.
use serde_json::Value;

fn lenient(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => e.iter().all(|(k, v)| a.get(k).is_some_and(|w| lenient(v, w))),
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                return false;
            }
            let mut used = vec![false; a.len()];
            e.iter().all(|v| {
                let found = a.iter().enumerate().find(|(i, w)| !used[*i] && lenient(v, w));
                match found {
                    Some((i, _)) => {
                        used[i] = true;
                        true
                    }
                    None => false,
                }
            })
        }
        (Value::Number(e), Value::Number(a)) => e.as_f64() == a.as_f64(),
        (e, a) => e == a,
    }
}

/// Panics with both documents when `actual` does not leniently match `expected`.
pub fn should_be_equal_to_as_json(actual: &str, expected: &str) {
    let parsed = |text: &str, what: &str| -> Value {
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{what} is not JSON ({e}): {text}"))
    };
    let (a, e) = (parsed(actual, "actual"), parsed(expected, "expected"));
    assert!(
        lenient(&e, &a),
        "JSON mismatch (LENIENT)\nexpected: {expected}\nactual: {actual}"
    );
}

fn collapse(xml: &str) -> String {
    let mut out = String::new();
    for piece in xml.split('<') {
        if let Some((tag, text)) = piece.split_once('>') {
            out.push('<');
            out.push_str(tag.trim());
            out.push('>');
            out.push_str(text.trim());
        } else {
            out.push_str(piece.trim());
        }
    }
    out
}

pub fn should_be_equal_to_as_xml(actual: &str, expected: &str) {
    assert_eq!(
        collapse(actual),
        collapse(expected),
        "XML mismatch (whitespace ignored)"
    );
}
