//! Audit probe: inputs whose answer is "match" but which need more than `java_regex`'s fixed internal budget
//! (recursion depth 500, 5,000,000 steps). The crate reports exhaustion as "no match", so a wrong `false` here is a
//! silent false negative. Expected answers are Java's (each pattern is linear-time on the JDK).
//!
//! Run: `cargo run --release --example limits`

fn main() {
    let cases: Vec<(&str, String, bool)> = vec![
        ("(?:ab)*", "ab".repeat(100), true),
        ("(?:ab)*", "ab".repeat(300), true),
        ("(a|b)*", "ab".repeat(300), true),
        (".*x", "a".repeat(10_000) + "x", true),
        ("[a-z]+", "a".repeat(10_000), true),
        ("(?:a+b)+", "aab".repeat(400), true),
        ("^((?!Abstract).)*Test[s]*$", "A".repeat(600) + "Test", true),
        ("com\\.example\\..*", format!("com.example.{}", "x".repeat(2_000)), true),
    ];
    for (p, input, java) in cases {
        let jr = java_regex::Regex::new(p).map(|r| r.matches(&input));
        let fancy = fancy_regex::Regex::new(&format!("\\A(?:{p})\\z")).map(|r| r.is_match(&input));
        println!(
            "{p:<32} len {:>6}  Java {java}  java_regex {:?}  fancy-regex {:?}",
            input.chars().count(),
            jr.map_err(|e| e.message),
            fancy.map_err(|e| e.to_string())
        );
    }
}
