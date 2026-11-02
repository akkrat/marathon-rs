//! Port of JDK 21 java.util.regex.CharPredicates and Pattern.family, using generated Unicode 15.0 tables.

use super::{CSet, ParseError};
use std::{collections::HashMap, sync::OnceLock};

struct Tables {
    sets: HashMap<String, Vec<(u32, u32)>>,
    aliases: HashMap<String, String>,
    pub case: HashMap<u32, (u32, u32)>,
    names: HashMap<String, u32>,
}

fn tables() -> &'static Tables {
    static DATA: OnceLock<Tables> = OnceLock::new();
    DATA.get_or_init(|| {
        let text = include_str!(concat!(env!("OUT_DIR"), "/jdk21-unicode.tsv"));
        let mut t = Tables {
            sets: HashMap::new(),
            aliases: HashMap::new(),
            case: HashMap::new(),
            names: HashMap::new(),
        };
        assert_eq!(text.lines().next(), Some("JDK21-Unicode15.0"));
        for line in text.lines().skip(1) {
            let (key, value) = line.split_once('\t').expect("table field");
            match &key[..2] {
                "s:" | "b:" => {
                    t.aliases.insert(key.into(), value.into());
                }
                "N:" => {
                    t.names.insert(key[2..].into(), value.parse().expect("code point"));
                }
                "C:" => {
                    let (u, l) = value.split_once(',').expect("case pair");
                    t.case.insert(
                        key[2..].parse().expect("code point"),
                        (u.parse().expect("upper"), l.parse().expect("lower")),
                    );
                }
                _ => {
                    let ranges = value
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .flat_map(|r| {
                            let (a, b) = r.split_once('-').expect("range");
                            let (a, b) = (
                                u32::from_str_radix(a, 16).expect("start"),
                                u32::from_str_radix(b, 16).expect("end"),
                            );
                            // Rust strings cannot contain isolated UTF-16 surrogates. Properties containing them remain
                            // valid; their intersection with scalar inputs omits that interval.
                            [(a, b.min(0xd7ff)), (a.max(0xe000), b)]
                                .into_iter()
                                .filter(|(a, b)| a <= b)
                        })
                        .collect();
                    t.sets.insert(key.into(), ranges);
                }
            }
        }
        t
    })
}

pub fn mappings() -> &'static HashMap<u32, (u32, u32)> {
    &tables().case
}
pub fn named(name: &str) -> Option<u32> {
    tables()
        .names
        .get(&name.trim_matches(|c: char| c <= ' ').to_uppercase())
        .copied()
}
fn get(name: &str) -> Option<CSet> {
    tables().sets.get(name).cloned().map(CSet::Ranges)
}
pub fn property(name: &str) -> CSet {
    get(&format!("P0:{name}")).expect("internal JDK property")
}
pub fn white_space() -> CSet {
    get("U0:WHITE_SPACE").expect("internal JDK property")
}
pub fn word() -> CSet {
    get("U0:WORD").expect("internal JDK property")
}

fn script(name: &str) -> Option<CSet> {
    let name = name.to_uppercase();
    get(&format!(
        "S:{}",
        tables().aliases.get(&format!("s:{name}")).unwrap_or(&name)
    ))
}
fn block(name: &str) -> Option<CSet> {
    let name = tables().aliases.get(&format!("b:{}", name.to_uppercase()))?;
    get(&format!("B:{name}"))
}

pub fn family(name: &str, ci: bool, unicode_class: bool) -> Result<CSet, ParseError> {
    let property = |name: &str| get(&format!("P{}:{name}", u8::from(ci)));
    let binary = |name: &str| get(&format!("U{}:{}", u8::from(ci), name.to_uppercase()));
    let value = if let Some((key, value)) = name.split_once('=') {
        match key.to_lowercase().as_str() {
            "sc" | "script" => script(value),
            "blk" | "block" => block(value),
            "gc" | "general_category" => property(value),
            _ => None,
        }
    } else if let Some(name) = name.strip_prefix("In") {
        block(name)
    } else if let Some(name) = name.strip_prefix("Is") {
        binary(name).or_else(|| property(name)).or_else(|| script(name))
    } else {
        (if unicode_class {
            get(&format!("X{}:{}", u8::from(ci), name.to_uppercase()))
        } else {
            None
        })
        .or_else(|| property(name))
    };
    value.ok_or_else(|| ParseError::Syntax(format!("Unknown character property name {{{name}}}")))
}
