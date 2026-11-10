//! Corpus runner: replays `fixtures/oracle/yaml-corpus.jsonl` against the production binder.
//!
//! For every row it rebuilds the input (`input` or `input_gen`), runs the
//! `Node` pipeline into Jackson tokens, and compares the token array and the
//! error class/position with the JVM ground truth. Mutation rows store their
//! token stream as a diff against `M-base`; the runner expands it first.
//! Binding comparison lives in `bind.rs`; this binary drives both and prints
//! per-family pass rates plus every divergence.
//!
//! Usage: `corpus [corpus.jsonl] [id-prefix-filter] [--baseline]`.

use sha2::{Digest, Sha256};
use snakeyaml::node;
use snakeyaml::resolve::{JInt, NumberError};
use snakeyaml::tokens::{Tok, Token, Tokens, tokens as walk_tokens};
use std::collections::BTreeMap;
#[path = "../tests/support/bind.rs"]
mod bind;
#[path = "../tests/support/schema_bind.rs"]
mod schema_bind;
#[path = "../tests/support/text.rs"]
mod text;

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let path = args.get(1).cloned().unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/oracle/yaml-corpus.jsonl")
            .to_string_lossy()
            .into_owned()
    });
    let filter = args
        .get(2)
        .filter(|s| !s.starts_with("--"))
        .cloned()
        .unwrap_or_default();
    let text = std::fs::read_to_string(&path)?;
    let mut lines = text.lines();
    let _header = lines.next();
    let mut rows: Vec<serde_json::Value> = lines
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    for row in &mut rows {
        d55_expectations(row);
        d57_expectations(row);
    }

    let base_tokens: Vec<serde_json::Value> = {
        rows.iter()
            .find(|r| r["id"] == "M-base")
            .map(|r| as_tokens(&r["tokens"]).to_vec())
            .unwrap_or_default()
    };

    let mut stats: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut bind_stats: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut failures = 0;
    let mut carried = 0;
    for row in &rows {
        let id = row["id"].as_str().unwrap_or("?");
        if !filter.is_empty() && !id.starts_with(&filter) {
            continue;
        }
        let family = row["family"].as_str().unwrap_or("?");
        if family == "emit" {
            // These are emitter checks over explicit bound-model projections;
            // ConfigurationFactory binding/defaults remain WP 2.1's contract.
            let actual = emit_case(id)?;
            let expected = row["emit"]["ok"].as_str().ok_or("emitter oracle output")?;
            let ok = marathon_test_support::yaml::same_data(&actual, expected).is_ok();
            let entry = stats.entry(family).or_insert((0, 0));
            entry.0 += 1;
            if ok {
                entry.1 += 1;
            } else {
                failures += 1;
                eprintln!("FAIL {id} (emit): {:?}", actual);
            }
            continue;
        }
        if row.get("tokens").is_none() {
            println!("SKIP {id} ({family}): no token stream");
            continue;
        }
        let input = expand_input(row);
        let stream = if let Some(bytes) = row["input_bytes"].as_array() {
            let bytes: Vec<u8> = bytes.iter().map(|b| b.as_u64().unwrap() as u8).collect();
            assert_eq!(
                String::from_utf8_lossy(&bytes).into_owned(),
                row["decoded"].as_str().unwrap()
            );
            node::load_bytes(&bytes)
        } else {
            node::load_bytes(input.as_bytes())
        };
        let toks = walk_tokens(&stream);
        let expected = expand_tokens(row, &base_tokens);
        let got = render_tokens(&toks);
        let tok_ok = compare_tokens(&expected, &got, &toks, &row["tokens"]["error"], id, &mut failures);
        let entry = stats.entry(family).or_insert((0, 0));
        entry.0 += 1;
        if tok_ok {
            entry.1 += 1;
        }
        if family == "strategy" {
            let entry = bind_stats.entry("strategy").or_insert((0, 0));
            entry.0 += 1;
            let mut ok = true;
            for (target, expected) in row["bind"]
                .as_object()
                .map(|m| m.iter().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                let got = schema_bind::bind_target(&stream, target);
                if let Err(diff) = schema_bind::outcome_matches(&got, expected) {
                    println!("BIND-DIFF {id} [{target}]: {diff}");
                    ok = false;
                }
            }
            if ok {
                entry.1 += 1;
            } else {
                failures += 1;
            }
        }
        if family == "spike" || family == "review" {
            let diffs = bind::compare_row(&stream, &row["bind"], &row["d9"]);
            if row.get("d9").is_some() {
                carried += 1;
            }
            let bentry = bind_stats.entry("spike-bind").or_insert((0, 0));
            bentry.0 += 1;
            if diffs.is_empty() {
                bentry.1 += 1;
            } else {
                failures += 1;
                for (target, detail) in &diffs {
                    let short = if detail.len() > 400 {
                        format!(
                            "{}…<{} chars>",
                            detail.chars().take(400).collect::<String>(),
                            detail.len()
                        )
                    } else {
                        detail.clone()
                    };
                    println!("BIND-DIFF {id} [{target}]: {short}");
                }
            }
        }
    }
    println!("--- token streams ---");
    for (family, (total, ok)) in &stats {
        println!("{family}: {ok}/{total}");
    }
    println!("--- binding ---");
    for (family, (total, ok)) in &bind_stats {
        println!("{family}: {ok}/{total}");
    }
    println!("D9-superseded rows: {carried} (asserted against approved policy)");
    println!("total divergences: {failures}");
    if failures > 0 {
        return Err(format!("{failures} corpus divergences").into());
    }
    Ok(())
}

// Original fixtures remain the JVM ground truth. Only D55 string envelopes,
// the derived long-string hash, and formerly unencodable file inputs differ.
fn d55_expectations(row: &mut serde_json::Value) {
    use serde_json::{Value, json};
    fn native(value: &mut Value) {
        if let Some(units) = value.get("$utf16").and_then(Value::as_array) {
            let units: Vec<u16> = units.iter().map(|u| u.as_u64().unwrap() as u16).collect();
            *value = String::from_utf16_lossy(&units).into();
            return;
        }
        match value {
            Value::Array(values) => values.iter_mut().for_each(native),
            Value::Object(values) => values.values_mut().for_each(native),
            Value::String(s)
                if s == "<257 chars, sha256 91e1ea347ec0bc028e3c6193579dc22e5445f528dc23f3855bca295c05b43d2b>" =>
            {
                *s = "<257 chars, sha256 9eb5f34e9b025c54c83b5664aea0fe1f66b590a362b26a8d88bf4183e2989f9a>".into();
            }
            _ => {}
        }
    }
    native(&mut row["tokens"]);
    native(&mut row["bind"]);
    if let Some(message) = row["bind"]["File?"]["error"]["message"].as_str()
        && let Some(path) = message.strip_prefix("Malformed input or input contains unmappable characters: ")
        && let Some((path, _)) = path.split_once(" (through reference chain:")
        && path.contains('�')
    {
        row["bind"]["File?"] = json!({"ok": text::short_json(&format!("/{path}"), 256)});
    }
}

// D57 overrides only the five encoded-surrogate byte rows; copied oracles stay intact.
fn d57_expectations(row: &mut serde_json::Value) {
    use serde_json::Value;
    let overrides: Value = serde_json::from_str(include_str!("../../../fixtures/phase8/d57-yaml.json")).unwrap();
    let Some(change) = overrides.get(row["id"].as_str().unwrap()) else {
        return;
    };
    fn replace(value: &mut Value, before: &str, after: &str) {
        match value {
            Value::String(s) => *s = s.replace(before, after),
            Value::Array(values) => values.iter_mut().for_each(|v| replace(v, before, after)),
            Value::Object(values) => values.values_mut().for_each(|v| replace(v, before, after)),
            _ => {}
        }
    }
    replace(
        &mut row["tokens"],
        change["before"].as_str().unwrap(),
        change["after"].as_str().unwrap(),
    );
    replace(
        &mut row["bind"],
        change["before"].as_str().unwrap(),
        change["after"].as_str().unwrap(),
    );
    row["decoded"] = change["decoded"].clone();
}

trait ShortJson {
    fn short_json(&self, limit: usize) -> serde_json::Value;
}
impl ShortJson for String {
    fn short_json(&self, limit: usize) -> serde_json::Value {
        text::short_json(self, limit)
    }
}

fn expand_input(row: &serde_json::Value) -> String {
    if let Some(s) = row["decoded"].as_str() {
        return s.to_owned();
    }
    if let Some(s) = row.get("input").and_then(|v| v.as_str()) {
        return s.to_owned();
    }
    let mut out = String::new();
    for seg in row["input_gen"].as_array().expect("input_gen array") {
        let text = seg[0].as_str().expect("segment text");
        let n = seg[1].as_u64().expect("segment count") as usize;
        out.push_str(&text.repeat(n));
    }
    out
}

fn as_tokens(v: &serde_json::Value) -> &[serde_json::Value] {
    v["tokens"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn expand_tokens<'a>(row: &'a serde_json::Value, base: &'a [serde_json::Value]) -> Vec<serde_json::Value> {
    let t = &row["tokens"];
    if let (Some(pre), Some(suf)) = (
        t.get("samePrefix").and_then(|v| v.as_u64()),
        t.get("sameSuffix").and_then(|v| v.as_u64()),
    ) {
        let (pre, suf) = (pre as usize, suf as usize);
        let mut full = Vec::new();
        full.extend_from_slice(&base[..pre.min(base.len())]);
        full.extend_from_slice(as_tokens(t));
        full.extend_from_slice(&base[base.len().saturating_sub(suf)..]);
        return full;
    }
    as_tokens(t).to_vec()
}

/// The corpus shortens long texts: `<N chars, sha256 HEX>`, N in UTF-16 units.
pub fn short(s: &str) -> String {
    let len = s.encode_utf16().count();
    if len <= 256 {
        return s.to_owned();
    }
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("<{len} chars, sha256 {}>", hex::encode(h.finalize()))
}

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn render_tokens(toks: &Tokens) -> Vec<serde_json::Value> {
    toks.tokens.iter().map(render_token).collect()
}

fn render_token(t: &Token) -> serde_json::Value {
    let mut tok: Vec<serde_json::Value> = match &t.tok {
        Tok::StartObject => vec!["{".into()],
        Tok::EndObject => vec!["}".into()],
        Tok::StartArray => vec!["[".into()],
        Tok::EndArray => vec!["]".into()],
        Tok::Field(name) => vec!["F".into(), name.short_json(256)],
        Tok::Str(text) => vec!["S".into(), text.short_json(256)],
        Tok::Int { text, value } => vec!["I".into(), short(text).into(), int_value(value).into()],
        Tok::Float { text, cleaned } => vec!["D".into(), short(text).into(), float_value(cleaned).into()],
        Tok::True(text) => vec!["T".into(), short(text).into()],
        Tok::False(text) => vec!["F0".into(), short(text).into()],
        Tok::Null(text) => vec!["N".into(), short(text).into()],
        Tok::Embedded(bytes) => vec!["E".into(), snakeyaml::resolve::base64_encode(bytes).into()],
    };
    if !matches!(t.tok, Tok::Embedded(_))
        && let Some(text) = &t.java_text
    {
        tok[1] = text.short_json(256);
    }
    let mut extra = BTreeMap::new();
    if let Some(a) = &t.anchor {
        extra.insert("anchor", serde_json::Value::from(a.clone()));
    }
    if let Some(tag) = &t.tag {
        extra.insert("tag", serde_json::Value::from(tag.clone()));
    }
    if !extra.is_empty() {
        tok.push(serde_json::Value::Object(
            extra.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        ));
    }
    serde_json::Value::Array(tok)
}

fn int_value(v: &Result<JInt, NumberError>) -> String {
    match v {
        Ok(JInt::Int(v)) => format!("INT:{v}"),
        Ok(JInt::Long(v)) => format!("LONG:{v}"),
        Ok(JInt::Big(s)) => format!("BIG_INTEGER:{s}"),
        Err(NumberError::Malformed) => "error:com.fasterxml.jackson.core.JsonParseException".to_owned(),
        Err(NumberError::TooLong) => "error:com.fasterxml.jackson.core.exc.StreamConstraintsException".to_owned(),
    }
}

fn float_value(cleaned: &str) -> String {
    match snakeyaml::jnum::parse_double(cleaned) {
        Some(f) => format!("DOUBLE:{}", snakeyaml::jnum::double_to_string(f)),
        None => "error:com.fasterxml.jackson.core.JsonParseException".to_owned(),
    }
}

/// Compares the expected token array with ours, then the error class and the
/// (line, column) positions named in the JVM message. Returns true on full
/// agreement, printing every divergence.
fn compare_tokens(
    expected: &[serde_json::Value],
    got: &[serde_json::Value],
    toks: &Tokens,
    expected_error: &serde_json::Value,
    id: &str,
    failures: &mut usize,
) -> bool {
    let mut ok = true;
    if expected != got {
        let n = expected.iter().zip(got.iter()).take_while(|(a, b)| a == b).count();
        let e = expected.get(n).map(|v| v.to_string()).unwrap_or("<end>".to_owned());
        let g = got.get(n).map(|v| v.to_string()).unwrap_or("<end>".into());
        println!(
            "TOKEN-DIFF {id}: token {n} of {}/{}: expected {e} got {g}",
            expected.len(),
            got.len()
        );
        *failures += 1;
        ok = false;
    }
    if !compare_error(toks, expected_error, id) {
        *failures += 1;
        ok = false;
    }
    ok
}

/// Compare mandatory marks by role, including explicit absence. Original
/// corpus rows predate structured marks; parse their two distinct formats.
fn compare_error(toks: &Tokens, expected_error: &serde_json::Value, id: &str) -> bool {
    match (&toks.error, expected_error) {
        (None, serde_json::Value::Null) => true,
        (Some(got), serde_json::Value::Object(exp)) => {
            let class = exp.get("class").and_then(|v| v.as_str()).unwrap_or("?");
            let expected = if let Some(m) = exp.get("marks") {
                m.clone()
            } else {
                message_marks(exp.get("message").and_then(|v| v.as_str()).unwrap_or(""))
            };
            let kind = marathon_config::binder::token_kind(got.kind);
            let mut ok = kind == marathon_test_support::recorded_errors::kind(class);
            if !ok {
                println!("ERROR-DIFF {id}: class expected {class} got {}", kind.description());
            }
            for (role, actual) in [
                ("context", got.context),
                ("problem", got.problem),
                ("location", got.location),
            ] {
                let actual = actual
                    .map(|m| serde_json::json!([m.line, m.column]))
                    .unwrap_or(serde_json::Value::Null);
                if expected[role] != actual {
                    println!("ERROR-DIFF {id}: {role} expected {} got {actual}", expected[role]);
                    ok = false;
                }
            }
            ok
        }
        (a, b) => {
            println!("ERROR-DIFF {id}: expected {b} got {a:?}");
            false
        }
    }
}
fn message_marks(message: &str) -> serde_json::Value {
    let mut yaml = Vec::new();
    let mut location = None;
    for line in message.split('\n') {
        if line.trim_start().starts_with("in '") {
            yaml.extend(error_positions(line));
        }
        if line.contains(" at [Source:") || line.trim_start().starts_with("at [Source:") {
            location = error_positions(line).last().copied();
        }
    }
    let problem = yaml.last().copied();
    let context = if yaml.len() > 1 { Some(yaml[0]) } else { None };
    serde_json::json!({"context":context,"problem":problem,"location":location})
}
fn error_positions(message: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (i, _) in message.match_indices("line") {
        let rest = message[i + 4..].trim_start_matches([':', ' ']);
        let digits = rest.bytes().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            continue;
        }
        let Some(column) = rest[digits..].strip_prefix(", column") else {
            continue;
        };
        let column = column.trim_start_matches([':', ' ']);
        let n = column.bytes().take_while(|c| c.is_ascii_digit()).count();
        if n > 0
            && let (Ok(l), Ok(c)) = (rest[..digits].parse(), column[..n].parse())
        {
            out.push((l, c));
        }
    }
    out
}

fn emit_case(id: &str) -> Result<String, Box<dyn std::error::Error>> {
    use marathon_config::yaml;
    use serde::{
        Serialize, Serializer,
        ser::{Error, SerializeMap},
    };
    fn units(v: &serde_json::Value) -> String {
        String::from_utf16_lossy(
            &v.as_array()
                .expect("UTF-16 units")
                .iter()
                .map(|c| c.as_u64().expect("code unit") as u16)
                .collect::<Vec<_>>(),
        )
    }
    struct Oracle<'a>(&'a serde_json::Value);
    impl Serialize for Oracle<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let v = self.0;
            match v["kind"].as_str().ok_or_else(|| S::Error::custom("projection kind"))? {
                "null" => serializer.serialize_none(),
                "bool" => v["value"]
                    .as_bool()
                    .ok_or_else(|| S::Error::custom("bool"))?
                    .serialize(serializer),
                "number" => {
                    let text = v["value"].as_str().ok_or_else(|| S::Error::custom("number"))?;
                    if text.contains(['.', 'e', 'E']) || matches!(text, "NaN" | "Infinity" | "-Infinity") {
                        yaml::Float(text.parse().map_err(S::Error::custom)?).serialize(serializer)
                    } else {
                        serializer.serialize_i64(text.parse().map_err(S::Error::custom)?)
                    }
                }
                "string" => units(&v["units"]).serialize(serializer),
                "array" => serializer.collect_seq(
                    v["items"]
                        .as_array()
                        .ok_or_else(|| S::Error::custom("items"))?
                        .iter()
                        .map(Oracle),
                ),
                "object" => {
                    let mut map = serializer.serialize_map(None)?;
                    for entry in v["entries"].as_array().ok_or_else(|| S::Error::custom("entries"))? {
                        if yaml::include_entry(entry[1]["kind"] == "null") {
                            map.serialize_entry(&units(&entry[0]), &Oracle(&entry[1]))?;
                        }
                    }
                    map.end()
                }
                _ => Err(S::Error::custom("projection kind")),
            }
        }
    }
    let row = include_str!("../../../fixtures/oracle/jcompat/writer_models.jsonl")
        .lines()
        .skip(1)
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|r| r["id"] == id)
        .ok_or("emitter projection")?;
    Ok(yaml::to_string(&Oracle(&row["input"]))?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run(std::env::args().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snakeyaml::node::Mark;
    use snakeyaml::tokens::TokenError;
    fn mark(column: usize) -> Option<Mark> {
        Some(Mark {
            index: 0,
            line: 1,
            column,
        })
    }
    #[test]
    fn marks_are_required_and_roles_cannot_swap() {
        let exp = serde_json::json!({"class":"C","marks":{"context":[1,3],"problem":[1,7],"location":[1,2]}});
        let mut toks = Tokens {
            tokens: vec![],
            error: Some(TokenError {
                stage: "token",
                kind: snakeyaml::tokens::TokenErrorKind::Fixture,
                message: String::new(),
                context: mark(3),
                problem: mark(7),
                location: mark(2),
            }),
        };
        assert!(compare_error(&toks, &exp, "correct"));
        toks.error.as_mut().unwrap().problem = None;
        assert!(!compare_error(&toks, &exp, "missing problem"));
        toks.error.as_mut().unwrap().problem = mark(2);
        toks.error.as_mut().unwrap().location = mark(7);
        assert!(!compare_error(&toks, &exp, "swapped"));
        toks.error.as_mut().unwrap().problem = None;
        toks.error.as_mut().unwrap().location = None;
        toks.error.as_mut().unwrap().context = None;
        assert!(!compare_error(&toks, &exp, "all missing"));
    }
    #[test]
    fn utf8_message_scan_and_truncation() {
        assert_eq!(error_positions("é: \"\\q\"; line: 1, column: 6"), vec![(1, 6)]);
        let detail = "é".repeat(401);
        assert_eq!(detail.chars().take(400).count(), 400);
    }
}
