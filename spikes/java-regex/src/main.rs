//! Corpus runner for WP 0.3c: runs every row of fixtures/oracle/regex-corpus.jsonl on every backend and reports the
//! agreement per backend and family, plus each divergent row.
//!
//! Usage: `cargo run --release -- [corpus.jsonl] [--backend NAME] [--id PREFIX] [--skip-family NAME] [--verbose]`

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use java_regex_spike::backend::{self, Backend, CompileError, Compiled, Found, RunError};
use java_regex_spike::backends;
use serde_json::{Value, json};

/// A row whose run takes longer than this counts as a hang. The JVM watchdog aborts after 20,000,000 character
/// reads, which takes well under a second.
const HANG: Duration = Duration::from_secs(10);

struct Row {
    id: String,
    family: String,
    pattern: String,
    op: String,
    inputs: Vec<String>,
    replacement: Option<String>,
    limit: usize,
    expected: Option<Vec<Value>>,
    scalar_expected: Option<Vec<Value>>,
    compile_error: Option<String>,
}

/// serde_json correctly refuses lone surrogates in strings. Preserve those JDK-only expected strings as
/// tagged UTF-16 units instead of dropping rows, replacing characters or changing the committed oracle bytes.
fn oracle_value(line: &str) -> Value {
    let bytes = line.as_bytes();
    let mut out = String::new();
    let mut from = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'"' => {
                    i += 1;
                    break;
                }
                _ => i += 1,
            }
        }
        let token = &line[start..i];
        if serde_json::from_str::<String>(token).is_ok() {
            continue;
        }
        let mut units = Vec::new();
        let mut chars = token[1..token.len() - 1].chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                match chars.next().unwrap_or_default() {
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        units.push(u16::from_str_radix(&hex, 16).unwrap_or_else(|e| panic!("{e}: {token}")));
                    }
                    c => units.push(match c {
                        'n' => 10,
                        'r' => 13,
                        't' => 9,
                        'b' => 8,
                        'f' => 12,
                        '"' => 34,
                        '\\' => 92,
                        '/' => 47,
                        _ => panic!("invalid JSON escape: {token}"),
                    }),
                }
            } else {
                units.extend(ch.encode_utf16(&mut [0; 2]).iter().copied());
            }
        }
        out.push_str(&line[from..start]);
        out.push_str(&json!({"utf16": units}).to_string());
        from = i;
    }
    out.push_str(&line[from..]);
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {line}"))
}

fn load(path: &str) -> Vec<Row> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    // split('\n'), not lines(): rows hold raw U+2028 and U+0085 inside strings.
    text.trim_end_matches('\n')
        .split('\n')
        .skip(1)
        .map(|line| {
            let v = oracle_value(line);
            let s = |k: &str| v[k].as_str().map(str::to_string);
            Row {
                id: s("id").unwrap_or_default(),
                family: s("family").unwrap_or_default(),
                pattern: s("pattern").unwrap_or_default(),
                op: s("op").unwrap_or_default(),
                inputs: v["inputs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|x| {
                        x.as_str()
                            .unwrap_or_else(|| panic!("non-scalar oracle input: {x}"))
                            .to_string()
                    })
                    .collect(),
                replacement: s("replacement"),
                limit: v["limit"].as_u64().unwrap_or(0) as usize,
                expected: v["expected"].as_array().cloned(),
                scalar_expected: v["scalar_expected"].as_array().cloned(),
                compile_error: v["compile_error"]["class"].as_str().map(str::to_string),
            }
        })
        .collect()
}

fn utf16_offset(input: &str, byte: usize) -> usize {
    input[..byte].encode_utf16().count()
}

fn match_json(input: &str, m: &Found) -> Value {
    let (s, e) = (m.start(), m.end());
    json!({
        "range": [utf16_offset(input, s), utf16_offset(input, e)],
        "groups": m.groups.iter().map(|g| g.map(|(s, e)| input[s..e].to_string())).collect::<Vec<_>>(),
    })
}

fn run_op(re: &dyn Compiled, row: &Row, input: &str) -> Value {
    let r: Result<Value, RunError> = match row.op.as_str() {
        "matches" => re.full_match(input).map(Value::Bool),
        "find" => backend::find(re, input).map(|m| m.map_or(Value::Null, |m| match_json(input, &m))),
        "findAll" => {
            backend::find_all(re, input).map(|ms| Value::Array(ms.iter().map(|m| match_json(input, m)).collect()))
        }
        "split" => backend::split(re, input, row.limit).map(|v| json!(v)),
        "replace" => backend::replace(re, input, row.replacement.as_deref().unwrap_or("")).map(Value::String),
        op => Err(RunError::Other(format!("unknown op {op}"))),
    };
    match r {
        Ok(v) => v,
        Err(RunError::Aborted) => json!({"aborted": true}),
        Err(RunError::Other(m)) => json!({"error": m}),
    }
}

#[derive(Debug, Clone)]
enum Outcome {
    Pass,
    /// D6: the backend refuses a pattern Java accepts.
    Rejected(String),
    Fail(String),
    /// The backend exhausted its budget where the oracle returned an answer. Never counted as agreement.
    Limit(String),
    /// The JDK watchdog aborted, but the answer is independently proved by the pattern's mandatory final literal.
    Proven,
    /// Authorized compile-depth safety policy, distinct from a Java syntax agreement or D6.
    CompileLimit,
    /// E1: exact agreement with the JDK oracle driven with scalar-only match boundaries and advancement.
    Approved,
    Hang,
}

fn is_aborted(v: &Value) -> bool {
    v.get("aborted").is_some() || v["error"]["class"] == "java.lang.StackOverflowError"
}

fn check_row(backend: &dyn Backend, row: &Row) -> Outcome {
    let compiled = backend.compile(&row.pattern);
    match (&row.compile_error, compiled) {
        (Some(_), Err(CompileError::Syntax(_))) => Outcome::Pass,
        (Some(class), Err(CompileError::Rejected { construct, .. })) => {
            Outcome::Fail(format!("Java throws {class}; the backend rejects {construct} under D6"))
        }
        (Some(class), Err(CompileError::Backend(m))) => {
            Outcome::Fail(format!("Java throws {class}; backend error: {m}"))
        }
        (Some(class), Ok(_)) => Outcome::Fail(format!("Java throws {class}; the backend compiles it")),
        (None, Err(CompileError::Rejected { construct, reason })) => {
            Outcome::Rejected(format!("{construct}: {reason}"))
        }
        (None, Err(CompileError::Syntax(message))) if message == "Stack overflow during pattern compilation" => {
            Outcome::CompileLimit
        }
        (None, Err(e)) => Outcome::Fail(format!("Java compiles it; the backend does not: {e:?}")),
        (None, Ok(re)) => {
            let expected = row.expected.clone().unwrap_or_default();
            assert_eq!(row.inputs.len(), expected.len(), "incomplete oracle row {}", row.id);
            let mut diffs = Vec::new();
            let mut limited = Vec::new();
            let mut proven = false;
            let mut approved = false;
            for (i, (input, exp)) in row.inputs.iter().zip(&expected).enumerate() {
                let got = run_op(re.as_ref(), row, input);
                if &got == exp || (is_aborted(exp) && is_aborted(&got)) {
                    continue;
                }
                // This is a proof, not an oracle comparison: every match of (.*a){12} ends in a.
                // Keep it separate in the scoreboard, rather than interpreting any exhaustion as false.
                if is_aborted(exp) && got == Value::Bool(false) && row.pattern == "(.*a){12}" && !input.ends_with('a') {
                    proven = true;
                    continue;
                }
                // These documented scan probes require a literal b that does not occur in the input. The oracle's
                // character-read watchdog supplies no semantic result; count the independent null proof separately.
                if is_aborted(exp)
                    && got.is_null()
                    && [r"(?=a*\z)b", r"(?!(?:a*\z))b"].contains(&row.pattern.as_str())
                    && input.bytes().all(|b| b == b'a')
                {
                    proven = true;
                    continue;
                }
                // The second expectation is independently generated with the same JDK matcher and E1's scalar
                // boundary/advancement policy. Do not excuse arbitrary mismatches just because input is astral.
                if row.op != "matches" && row.scalar_expected.as_ref().and_then(|v| v.get(i)) == Some(&got) {
                    approved = true;
                    continue;
                }
                let label = if input.len() > 256 {
                    format!(
                        "{:?}… ({} bytes)",
                        input.chars().take(128).collect::<String>(),
                        input.len()
                    )
                } else {
                    format!("{input:?}")
                };
                let line = format!("input[{i}] {label}: expected {exp}, got {got}");
                if is_aborted(&got) {
                    limited.push(line);
                } else {
                    diffs.push(line);
                }
            }
            if !diffs.is_empty() {
                diffs.extend(limited);
                Outcome::Fail(diffs.join("\n    "))
            } else if !limited.is_empty() {
                Outcome::Limit(limited.join("\n    "))
            } else if approved {
                Outcome::Approved
            } else if proven {
                Outcome::Proven
            } else {
                Outcome::Pass
            }
        }
    }
}

fn check_bounded(backend: &'static dyn Backend, row: Row, corpus: &str) -> (Row, Outcome, Duration) {
    let start = Instant::now();
    let outcome = (|| -> Result<Outcome, String> {
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new(executable)
            .args([corpus, "--backend", backend.name(), "--id", &row.id, "--worker"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < HANG => std::thread::sleep(Duration::from_millis(5)),
                Ok(None) => {
                    child.kill().map_err(|e| e.to_string())?;
                    child.wait().map_err(|e| e.to_string())?;
                    return Ok(Outcome::Hang);
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e.to_string());
                }
            }
        }
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "worker {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let value: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
        let detail = value["detail"].as_str().unwrap_or("").to_string();
        match value["kind"].as_str() {
            Some("pass") => Ok(Outcome::Pass),
            Some("rejected") => Ok(Outcome::Rejected(detail)),
            Some("fail") => Ok(Outcome::Fail(detail)),
            Some("limit") => Ok(Outcome::Limit(detail)),
            Some("proven") => Ok(Outcome::Proven),
            Some("approved") => Ok(Outcome::Approved),
            Some("compile-limit") => Ok(Outcome::CompileLimit),
            _ => Err("invalid worker outcome".into()),
        }
    })()
    .unwrap_or_else(Outcome::Fail);
    (row, outcome, start.elapsed())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut corpus = "../../fixtures/oracle/regex-round2-corpus.jsonl".to_string();
    let mut only_backend = None;
    let mut only_id = None;
    let mut skip_family = None;
    let mut verbose = false;
    let mut worker = false;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--backend" => only_backend = it.next(),
            "--id" => only_id = it.next(),
            "--skip-family" => skip_family = it.next(),
            "--verbose" => verbose = true,
            "--worker" => worker = true,
            _ => corpus = a,
        }
    }
    let backends: Vec<&'static dyn Backend> = backends::all()
        .into_iter()
        .filter(|b| only_backend.as_deref().is_none_or(|n| n == b.name()))
        .collect();

    let rows: Vec<Row> = load(&corpus)
        .into_iter()
        .filter(|row| skip_family.as_deref() != Some(row.family.as_str()))
        .collect();
    if worker {
        let Some(row) = rows.iter().find(|r| Some(r.id.as_str()) == only_id.as_deref()) else {
            std::process::exit(2)
        };
        let Some(backend) = backends.first() else {
            std::process::exit(2)
        };
        let outcome = check_row(*backend, row);
        let detail = match &outcome {
            Outcome::Rejected(s) | Outcome::Fail(s) | Outcome::Limit(s) => s.as_str(),
            _ => "",
        };
        println!("{}", json!({"kind":outcome_name(&outcome), "detail":detail}));
        return;
    }
    let families: Vec<String> = {
        let mut f: Vec<String> = Vec::new();
        for r in &rows {
            if !f.contains(&r.family) {
                f.push(r.family.clone());
            }
        }
        f
    };

    // backend -> family -> (pass, rejected, fail, hang, total, limit)
    let mut table: BTreeMap<&str, BTreeMap<String, [usize; 9]>> = BTreeMap::new();
    for &b in &backends {
        let mut slowest = (Duration::ZERO, String::new());
        let mut aborted_rows = Vec::new();
        for row in &rows {
            if only_id.as_deref().is_some_and(|p| !row.id.starts_with(p)) {
                continue;
            }
            let family = row.family.clone();
            let id = row.id.clone();
            let has_abort = row.expected.iter().flatten().any(is_aborted);
            let row_copy = Row {
                id: row.id.clone(),
                family: row.family.clone(),
                pattern: row.pattern.clone(),
                op: row.op.clone(),
                inputs: row.inputs.clone(),
                replacement: row.replacement.clone(),
                limit: row.limit,
                expected: row.expected.clone(),
                scalar_expected: row.scalar_expected.clone(),
                compile_error: row.compile_error.clone(),
            };
            let (_, outcome, took) = check_bounded(b, row_copy, &corpus);
            if took > slowest.0 {
                slowest = (took, id.clone());
            }
            if has_abort {
                aborted_rows.push(format!("{id}: {:?} in {} ms", outcome_name(&outcome), took.as_millis()));
            }
            let cell = table.entry(b.name()).or_default().entry(family).or_default();
            cell[4] += 1;
            match &outcome {
                Outcome::Pass => cell[0] += 1,
                Outcome::Rejected(why) => {
                    cell[1] += 1;
                    println!("{} REJECTED {id} {:?}: {why}", b.name(), row.pattern);
                }
                Outcome::Fail(why) => {
                    cell[2] += 1;
                    println!("{} FAIL {id} {} {:?}", b.name(), row.op, row.pattern);
                    if verbose {
                        println!("    {why}");
                    } else {
                        println!("    {}", why.lines().next().unwrap_or(""));
                    }
                }
                Outcome::Limit(why) => {
                    cell[5] += 1;
                    println!("{} LIMIT {id} {:?}", b.name(), row.pattern);
                    if verbose {
                        println!("    {why}");
                    }
                }
                Outcome::Proven => {
                    cell[6] += 1;
                    println!(
                        "{} PROVEN {id}: oracle aborted; mandatory-literal proof supplies the answer",
                        b.name()
                    );
                }
                Outcome::Approved => {
                    cell[7] += 1;
                    println!(
                        "{} E1 {id}: scalar-only match boundaries and empty-match advancement",
                        b.name()
                    );
                }
                Outcome::CompileLimit => {
                    cell[8] += 1;
                    println!("{} COMPILE-LIMIT {id}: authorized nesting bound", b.name());
                }
                Outcome::Hang => {
                    cell[3] += 1;
                    println!("{} HANG {id} {:?}", b.name(), row.pattern);
                }
            }
        }
        println!("{}: slowest row {} ({} ms)", b.name(), slowest.1, slowest.0.as_millis());
        for a in aborted_rows {
            println!("{}: aborted row {a}", b.name());
        }
    }

    println!();
    print!("| Family |");
    for &b in &backends {
        print!(" {} |", b.name());
    }
    println!();
    print!("|---|");
    for _ in &backends {
        print!("---|");
    }
    println!();
    let mut totals: BTreeMap<&str, [usize; 9]> = BTreeMap::new();
    for f in &families {
        print!("| {f} |");
        for &b in &backends {
            let c = table.get(b.name()).and_then(|t| t.get(f)).copied().unwrap_or_default();
            let t = totals.entry(b.name()).or_default();
            for i in 0..9 {
                t[i] += c[i];
            }
            print!(" {} |", cell_text(c));
        }
        println!();
    }
    print!("| **all** |");
    for &b in &backends {
        print!(
            " **{}** |",
            cell_text(totals.get(b.name()).copied().unwrap_or_default())
        );
    }
    println!();
    if totals.values().any(|c| c[2] > 0 || c[3] > 0 || c[5] > 0) {
        std::process::exit(1);
    }
}

fn cell_text(c: [usize; 9]) -> String {
    let mut s = format!("{}/{}", c[0], c[4]);
    if c[5] > 0 {
        s += &format!(" +{} limit", c[5]);
    }
    if c[2] > 0 {
        s += &format!(" +{} fail", c[2]);
    }
    if c[6] > 0 {
        s += &format!(" +{} proven", c[6]);
    }
    if c[7] > 0 {
        s += &format!(" +{} E1", c[7]);
    }
    if c[8] > 0 {
        s += &format!(" +{} compile-limit", c[8]);
    }
    if c[1] > 0 {
        s += &format!(" +{} D6", c[1]);
    }
    if c[3] > 0 {
        s += &format!(" ({} hang)", c[3]);
    }
    s
}

fn outcome_name(o: &Outcome) -> &'static str {
    match o {
        Outcome::Pass => "pass",
        Outcome::Rejected(_) => "rejected",
        Outcome::Fail(_) => "fail",
        Outcome::Limit(_) => "limit",
        Outcome::Hang => "hang",
        Outcome::Approved => "approved",
        Outcome::Proven => "proven",
        Outcome::CompileLimit => "compile-limit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_preserves_lone_utf16_halves_without_changing_scalar_strings() {
        let value = oracle_value(r#"{"pair":"\ud83d\ude00","high":"\ud83d","low":"\ude00","literal":"\\ud83d"}"#);
        assert_eq!(value["pair"], "😀");
        assert_eq!(value["high"], json!({"utf16": [0xd83d]}));
        assert_eq!(value["low"], json!({"utf16": [0xde00]}));
        assert_eq!(value["literal"], r"\ud83d");
    }

    #[test]
    fn e1_requires_the_scalar_oracle_answer_and_never_excuses_full_matches() {
        let mut row = Row {
            id: "scoring".into(),
            family: "scoring".into(),
            pattern: r"\B".into(),
            op: "split".into(),
            inputs: vec!["😀".into()],
            replacement: None,
            limit: 0,
            expected: Some(vec![oracle_value(r#"["","\ud83d","\ude00",""]"#)]),
            scalar_expected: Some(vec![json!(["", "😀", ""])]),
            compile_error: None,
        };
        assert!(matches!(
            check_row(&backends::fancy::FancyBackend, &row),
            Outcome::Approved
        ));
        row.scalar_expected = Some(vec![json!(["unrelated"])]);
        assert!(matches!(
            check_row(&backends::fancy::FancyBackend, &row),
            Outcome::Fail(_)
        ));
        row.op = "matches".into();
        row.pattern = "a".into();
        row.expected = Some(vec![json!(true)]);
        row.scalar_expected = Some(vec![json!(false)]);
        assert!(matches!(
            check_row(&backends::fancy::FancyBackend, &row),
            Outcome::Fail(_)
        ));
    }
}
