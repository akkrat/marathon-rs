//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/RegexSerializer.kt (regex operation and pattern tests).

use super::*;

fn re(pattern: &str) -> JavaRegex {
    JavaRegex::compile(pattern, "filtering.regex").unwrap()
}

#[test]
fn full_match_versus_find() {
    let regex = re("a|ab");
    assert!(regex.matches("ab"));
    assert!(!regex.matches("xab"));
    assert!(regex.contains("xab"));
    assert_eq!(regex.find("xab").unwrap().get(0).unwrap().as_str(), "a");
    assert!(!re("(?m)^a$").matches("a\nb"));
    assert!(re("(?x)a # final comment").matches("a"));
    assert!(!re("a").matches("a\n"));
}

#[test]
fn field_naming_error_and_source_pattern() {
    for pattern in ["(?=a)", r"(a)\1", "["] {
        let error = JavaRegex::compile(pattern, "excludeSerialRegexes[2]").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("excludeSerialRegexes[2]"));
        assert!(message.contains(&format!("{pattern:?}")));
        assert!(message.contains(&error.source.to_string()));
    }
    let regex = re("(?i)a+");
    assert_eq!(regex.pattern(), "(?i)a+");
    assert_eq!(regex.to_string(), "(?i)a+");
    assert!(re(r"\d\w\s").matches("١é\u{a0}"));
    // Rust accepts nested repetition here; it does not mean Java possessiveness.
    assert!(re("a*+a").matches("a"));
}

#[test]
fn empty_matches_and_split_limits() {
    assert_eq!(
        re("x*")
            .find_all("x")
            .map(|c| c.get(0).unwrap().range())
            .collect::<Vec<_>>(),
        vec![0..1, 1..1]
    );
    assert_eq!(re("").split("ab", 0).unwrap(), ["", "a", "b", ""]);
    assert_eq!(re("").split("", 0).unwrap(), ["", ""]);
    assert_eq!(re(",").split("a,b,,", 0).unwrap(), ["a", "b", "", ""]);
    assert_eq!(re(",").split("a,b,,", 1).unwrap(), ["a,b,,"]);
    assert_eq!(re(",").split("a,b,,", 2).unwrap(), ["a", "b,,"]);
    assert!(re(",").split("a", -1).is_err());
    assert_eq!(re("x*").replace("x", "-").unwrap(), "--");
}

#[test]
fn kotlin_replacement_expansion() {
    let regex = re("(?<word>a)(b)?");
    assert_eq!(
        regex.replace("a ab", r"${word}:$2:$0:$12:\$\\").unwrap(),
        "a::a:a2:$\\ a:b:ab:a2:$\\"
    );
    for invalid in ["$", "$3", "${missing}", "\\", "$word", "$$"] {
        assert!(regex.replace("a", invalid).is_err(), "{invalid}");
        assert_eq!(regex.replace("z", invalid).unwrap(), "z");
    }
}

#[test]
fn shared_syntax_jvm_operations() {
    // Explicit selected rows: flavour differences are D6, never silently skipped.
    let ids = [
        "split-001",
        "split-002",
        "split-003",
        "split-004",
        "advanced-018",
        "advanced-019",
        "advanced-021",
        "advanced-023",
        "advanced-024",
        "advanced-036",
    ];
    let mut count = 0;
    for line in include_str!("../../../../fixtures/oracle/regex-corpus.jsonl")
        .lines()
        .skip(1)
    {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        if !ids.contains(&row["id"].as_str().unwrap()) {
            continue;
        }
        count += 1;
        let regex = re(row["pattern"].as_str().unwrap());
        for (input, expected) in row["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .zip(row["expected"].as_array().unwrap())
        {
            let input = input.as_str().unwrap();
            let actual = if row["op"] == "split" {
                serde_json::json!(regex.split(input, row["limit"].as_i64().unwrap_or(0) as i32).unwrap())
            } else {
                serde_json::Value::Array(regex.find_all(input).map(|c| {
                    let m = c.get(0).unwrap();
                    serde_json::json!({"range": [input[..m.start()].encode_utf16().count(), input[..m.end()].encode_utf16().count()], "groups": c.iter().map(|m| m.map(|m| m.as_str())).collect::<Vec<_>>()})
                }).collect())
            };
            assert_eq!(&actual, expected, "{} {input:?}", row["id"]);
        }
    }
    assert_eq!(count, ids.len());
}

#[test]
fn kotlin_operation_probes() {
    let mut count = 0;
    for line in include_str!("../../../../fixtures/oracle/jcompat/regex_operations.jsonl")
        .lines()
        .skip(1)
    {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let regex = re(row["pattern"].as_str().unwrap());
        let input = row["input"].as_str().unwrap();
        let expected = &row["expected"];
        let actual = match row["op"].as_str().unwrap() {
            "split" => regex.split(input, row["limit"].as_i64().unwrap() as i32).map(|v| serde_json::json!(v)),
            "replace" => regex.replace(input, row["replacement"].as_str().unwrap()).map(|v| serde_json::json!(v)),
            "findAll" => Ok(serde_json::Value::Array(regex.find_all(input).map(|c| {
                let m = c.get(0).unwrap();
                serde_json::json!({"range": [m.start(), m.end()], "groups": c.iter().map(|m| m.map(|m| m.as_str())).collect::<Vec<_>>()})
            }).collect())),
            _ => unreachable!(),
        };
        if expected.get("error").is_some() {
            assert!(actual.is_err(), "{row}");
        } else {
            assert_eq!(&actual.unwrap(), expected, "{row}");
        }
        count += 1;
    }
    assert_eq!(count, 77);
}

#[test]
fn unicode_scalar_offsets_under_d6() {
    let regex = re("");
    assert_eq!(
        regex
            .find_all("😀")
            .map(|c| c.get(0).unwrap().range())
            .collect::<Vec<_>>(),
        [0..0, 4..4]
    );
    assert_eq!(regex.split("😀", 0).unwrap(), ["", "😀", ""]);
}

#[test]
fn pattern_only_equality_and_hash() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let hash = |r: &JavaRegex| {
        let mut h = DefaultHasher::new();
        r.hash(&mut h);
        h.finish()
    };
    assert_eq!(re("a+"), re("a+"));
    assert_eq!(hash(&re("a+")), hash(&re("a+")));
    assert_ne!(re("a+"), re("a*"));
    assert_ne!(hash(&re("a+")), hash(&re("a*")));
}
