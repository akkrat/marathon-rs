//! Audit fuzzing: mutates the corpus patterns (insert, delete, replace and splice with regex metacharacters and
//! non-ASCII chars), compiles each mutant and runs `matches` and `find` over the corpus inputs, catching panics.
//! A stack overflow aborts the process, so a crash there shows as the process dying.
//!
//! Run: `cargo run --release --example fuzz -- <java_regex|fancy-regex|pcre2> [seconds] [seed]`

use java_regex_spike::{backend, backends};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

const PIECES: &[&str] = &[
    "(", ")", "[", "]", "{", "}", "*", "+", "?", "|", "^", "$", ".", "\\", "&&", "-", "(?", "(?<", "(?<=", "(?<!",
    "(?=", "(?!", "(?>", "(?i)", "(?u)", "(?U)", "(?x)", "(?d)", "(?m)", "(?s)", "(?-i)", "\\Q", "\\E", "\\p{L}",
    "\\P{Lu}", "\\p{", "\\x{", "\\u", "\\0", "\\c", "\\k<", "\\1", "\\9", "\\b", "\\B", "\\R", "\\X", "\\G", "\\Z",
    "\\z", "{2,}", "{0}", "{,", "#", " ", "\n", "a", "ж", "😀", "\u{301}", "ſ", "\u{212A}", "]]", "[^", "[[", ",", "0",
    "9", "}",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, pats: &[String]) -> String {
    let mut s: Vec<char> = pats[rng.below(pats.len())].chars().collect();
    for _ in 0..1 + rng.below(4) {
        let at = rng.below(s.len() + 1);
        let piece: Vec<char> = PIECES[rng.below(PIECES.len())].chars().collect();
        match rng.below(4) {
            0 => {
                s.splice(at..at, piece);
            }
            1 if !s.is_empty() => {
                s.remove(at.min(s.len() - 1));
            }
            2 if !s.is_empty() => {
                let i = at.min(s.len() - 1);
                s.splice(i..i + 1, piece);
            }
            _ => {
                let other: Vec<char> = pats[rng.below(pats.len())].chars().collect();
                let a = rng.below(other.len() + 1);
                let b = a + rng.below(other.len() - a + 1);
                s.splice(at..at, other[a..b].iter().copied());
            }
        }
    }
    s.into_iter().collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let name = args.first().cloned().unwrap_or_else(|| "java_regex".into());
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    let seed: u64 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let Some(b) = backends::all().into_iter().find(|b| b.name() == name) else {
        eprintln!("unknown backend {name}");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string("../../fixtures/oracle/regex-corpus.jsonl").unwrap_or_default();
    let mut pats = Vec::new();
    let mut inputs = Vec::new();
    for line in text.trim_end_matches('\n').split('\n').skip(1) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
        pats.push(v["pattern"].as_str().unwrap_or("").to_string());
        for i in v["inputs"].as_array().into_iter().flatten() {
            let s = i.as_str().unwrap_or("").to_string();
            if !inputs.contains(&s) && s.len() < 64 {
                inputs.push(s);
            }
        }
    }
    std::panic::set_hook(Box::new(|_| {}));
    let mut rng = Rng(seed);
    let deadline = Instant::now() + Duration::from_secs(secs);
    let (mut n, mut compiled, mut panics, mut slow) = (0u64, 0u64, Vec::new(), 0u64);
    while Instant::now() < deadline {
        let p = mutate(&mut rng, &pats);
        n += 1;
        let t = Instant::now();
        let r = catch_unwind(AssertUnwindSafe(|| {
            let Ok(re) = b.compile(&p) else { return false };
            for input in &inputs {
                let _ = re.full_match(input);
                let _ = backend::find_all(re.as_ref(), input);
            }
            true
        }));
        match r {
            Ok(true) => compiled += 1,
            Ok(false) => {}
            Err(_) => panics.push(p),
        }
        if t.elapsed() > Duration::from_secs(1) {
            slow += 1;
        }
    }
    println!(
        "{name}: {n} patterns, {compiled} compiled, {} panics, {slow} took over 1 s",
        panics.len()
    );
    for p in panics.iter().take(10) {
        println!("  panic on {p:?}");
    }
}
