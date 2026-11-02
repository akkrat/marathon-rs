//! Port of tools/oracle/module/src/main/kotlin (WP 0.2a jcompat tables).
use jcompat::{formatting::*, jnum::*, jtime::*};
use serde_json::Value;

fn rows(text: &str) -> Vec<Value> {
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let rows: Vec<Value> = lines.map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(rows.len(), header["rows"].as_u64().unwrap() as usize);
    rows
}
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap()
}
fn n(v: &Value, key: &str) -> i64 {
    v[key].as_i64().unwrap()
}

#[test]
fn double_and_float_to_string_oracle() {
    for row in rows(include_str!("../../../fixtures/oracle/jcompat/double_to_string.jsonl")) {
        let bits = u64::from_str_radix(s(&row, "bits"), 16).unwrap();
        let actual = if s(&row, "kind") == "double" {
            double_to_string(f64::from_bits(bits))
        } else {
            float_to_string(f32::from_bits(bits as u32))
        };
        assert_eq!(actual, s(&row, "toString"), "{row}");
        if s(&row, "kind") == "double" {
            let parsed = parse_double(&actual).unwrap();
            assert!(
                parsed.to_bits() == bits || (parsed.is_nan() && f64::from_bits(bits).is_nan()),
                "{row}"
            );
        }
    }
}

#[test]
fn instant_and_duration_oracle() {
    for row in rows(include_str!("../../../fixtures/oracle/jcompat/instant_duration.jsonl")) {
        let op = s(&row, "op");
        if op.ends_with("toString") {
            let actual = if op.starts_with("Instant") {
                instant_of(n(&row, "seconds"), n(&row, "nanos")).unwrap().to_string()
            } else {
                duration_of(n(&row, "seconds"), n(&row, "nanos")).unwrap().to_string()
            };
            assert_eq!(actual, s(&row, "result"), "{row}");
        } else {
            let actual = if op.starts_with("Instant") {
                parse_instant(s(&row, "input")).map(|t| (t.seconds(), t.nanos(), t.to_string()))
            } else {
                parse_duration(s(&row, "input")).map(|t| (t.seconds(), t.nanos(), t.to_string()))
            };
            if let Some(ok) = row["result"].get("ok") {
                assert_eq!(
                    actual,
                    Some((n(ok, "seconds"), n(ok, "nanos"), s(ok, "toString").to_owned())),
                    "{row}"
                );
            } else {
                assert!(actual.is_none(), "{row}: {actual:?}");
            }
        }
    }
}

/// `Parsed.toString` prints a `HashMap` whose order varies between JVM runs: compare its entries as a set.
fn parsed_fields_unordered(message: &str) -> (String, Vec<String>) {
    match (message.find('{'), message.find('}')) {
        (Some(open), Some(close)) => {
            let mut fields: Vec<String> = message[open + 1..close].split(", ").map(str::to_owned).collect();
            fields.sort();
            (format!("{}{{}}{}", &message[..open], &message[close + 1..]), fields)
        }
        _ => (message.to_owned(), Vec::new()),
    }
}

/// (class, message with the `Parsed` fields apart) per level of the exception chain.
type Chain = Vec<(String, (String, Vec<String>))>;

fn instant_parse_chain(input: &str) -> Result<String, Chain> {
    parse_instant_detailed(input).map(|i| i.to_string()).map_err(|e| {
        e.chain
            .iter()
            .map(|(class, message)| (class.to_string(), parsed_fields_unordered(&message.to_string_lossy())))
            .collect()
    })
}

#[test]
fn instant_parse_errors_oracle() {
    for row in rows(include_str!(
        "../../../fixtures/oracle/jcompat/instant_parse_errors.jsonl"
    )) {
        let expected = match row.get("ok") {
            Some(ok) => Ok(ok.as_str().unwrap().to_owned()),
            None => Err(row["error"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| (s(e, "class").to_owned(), parsed_fields_unordered(s(e, "message"))))
                .collect()),
        };
        assert_eq!(instant_parse_chain(s(&row, "input")), expected, "{row}");
    }
    for row in rows(include_str!("../../../fixtures/oracle/jcompat/instant_duration.jsonl")) {
        if let (Some("Instant.parse"), Some(error)) = (row["op"].as_str(), row["result"].get("error")) {
            let actual = instant_parse_chain(s(&row, "input")).unwrap_err();
            assert_eq!(actual[0].0, s(error, "class"), "{row}");
            assert_eq!(actual[0].1, parsed_fields_unordered(s(error, "message")), "{row}");
        }
    }
}

#[test]
fn date_format_oracle() {
    for row in rows(include_str!("../../../fixtures/oracle/jcompat/date_format.jsonl")) {
        let pattern = match s(&row, "pattern") {
            "HH:mm:ss z, MMM d yyyy" => DatePattern::Html,
            "HH:mm_yyyyMMdd" => DatePattern::Graphite,
            _ => DatePattern::Junit,
        };
        if s(&row, "locale") == "en-US" {
            assert_eq!(format_date(n(&row, "epochMillis"), pattern), s(&row, "output"));
        }
        if pattern == DatePattern::Graphite {
            assert_eq!(
                format_graphite(instant_of(n(&row, "epochMillis").div_euclid(1000), 0).unwrap()).as_deref(),
                row["output"].as_str()
            );
        }
        assert_eq!(
            format_date_with_locale(
                n(&row, "epochMillis"),
                pattern,
                Locale::from_language_tag(s(&row, "locale")).unwrap()
            ),
            s(&row, "output"),
            "{row}"
        );
    }
}

#[test]
fn string_format_oracle() {
    let mut failures = Vec::new();
    for row in rows(include_str!("../../../fixtures/oracle/jcompat/string_format.jsonl")) {
        let locale = Locale::from_language_tag(s(&row, "locale")).unwrap();
        let actual = if s(&row, "pattern") == "%02d%%" {
            format_percent_with_locale(n(&row["arg"], "int") as i32, locale)
        } else {
            let bits = u64::from_str_radix(s(&row["arg"], "double"), 16).unwrap();
            format_fixed_with_locale(
                f64::from_bits(bits),
                if s(&row, "pattern") == "%.2f" { 2 } else { 4 },
                locale,
            )
            .unwrap()
        };
        if locale == Locale::Us {
            let default = if s(&row, "pattern") == "%02d%%" {
                format_percent(n(&row["arg"], "int") as i32)
            } else {
                let bits = u64::from_str_radix(s(&row["arg"], "double"), 16).unwrap();
                format_fixed(f64::from_bits(bits), if s(&row, "pattern") == "%.2f" { 2 } else { 4 }).unwrap()
            };
            assert_eq!(default, s(&row, "output"), "{row}");
        }
        if actual != s(&row, "output") {
            failures.push(format!("{row}: actual {actual}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn duration_format_utils_oracle() {
    for row in rows(include_str!(
        "../../../fixtures/oracle/jcompat/duration_format_utils.jsonl"
    )) {
        assert_eq!(
            format_duration(n(&row, "millis")).as_deref(),
            row["formatDuration"]["ok"].as_str(),
            "{row}"
        );
        assert_eq!(
            format_stdout_duration(n(&row, "millis")),
            s(&row, "stdoutReporter"),
            "{row}"
        );
    }
}

#[test]
fn formatting_hardening_oracle() {
    let mut failures = Vec::new();
    for row in rows(include_str!(
        "../../../fixtures/oracle/jcompat/formatting_hardening.jsonl"
    )) {
        let input = s(&row, "input");
        let output = match s(&row, "op") {
            "double" => Some(Value::String(double_to_string(f64::from_bits(
                u64::from_str_radix(input, 16).unwrap(),
            )))),
            "float" => Some(Value::String(float_to_string(f32::from_bits(
                u32::from_str_radix(input, 16).unwrap(),
            )))),
            "fixed" => {
                format_fixed_with_locale(f64::from_bits(u64::from_str_radix(input, 16).unwrap()), 4, Locale::Root)
                    .map(Value::String)
            }
            "parseDouble" => parse_double(input).map(|v| Value::String(format!("{:016x}", v.to_bits()))),
            "decimal" => Decimal::parse(input).map(|d| Value::from(d.scale())),
            "instant" => parse_instant(input).map(|v| Value::String(v.to_string())),
            "duration" => parse_duration(input).map(|v| Value::String(v.to_string())),
            "digit" => {
                let (code, radix) = input.split_once(':').unwrap();
                digit(code.parse().unwrap(), radix.parse().unwrap()).map(Value::from)
            }
            op => panic!("unknown op {op}"),
        }
        .unwrap_or(Value::Null);
        if output != row["output"] {
            failures.push(format!("{row}: actual {output}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.iter().take(30).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn formatting_round1_oracle() {
    for row in rows(include_str!("../../../fixtures/oracle/jcompat/formatting_round1.jsonl")) {
        let input = s(&row, "input");
        let actual = match s(&row, "op") {
            "graphite" => parse_instant(input).and_then(format_graphite).map(Value::String),
            "digit" => {
                let (code, radix) = input.split_once(':').unwrap();
                digit(code.parse().unwrap(), radix.parse().unwrap()).map(Value::from)
            }
            "decimal" => Decimal::parse(input).map(|d| Value::from(d.scale())),
            "instantOf" | "durationOf" => {
                let (seconds, nanos) = input.split_once(':').unwrap();
                let (seconds, nanos) = (seconds.parse().unwrap(), nanos.parse().unwrap());
                if s(&row, "op") == "instantOf" {
                    instant_of(seconds, nanos).map(|t| serde_json::json!([t.seconds(), t.nanos(), t.to_string()]))
                } else {
                    duration_of(seconds, nanos).map(|t| serde_json::json!([t.seconds(), t.nanos(), t.to_string()]))
                }
            }
            op => panic!("unknown op {op}"),
        }
        .unwrap_or(Value::Null);
        assert_eq!(actual, row["output"], "{row}");
    }
}
