//! Java's simple case mappings and the case-insensitive sets that `java.util.regex` builds from them.
//!
//! Uses JDK 21 Character simple mappings, independent of the host Rust Unicode version.

use std::collections::HashMap;
use std::sync::OnceLock;

pub fn upper(c: u32) -> u32 {
    super::props::mappings().get(&c).map_or(c, |m| m.0)
}
pub fn lower(c: u32) -> u32 {
    super::props::mappings().get(&c).map_or(c, |m| m.1)
}

fn cased() -> &'static [(u32, u32, u32)] {
    static T: OnceLock<Vec<(u32, u32, u32)>> = OnceLock::new();
    T.get_or_init(|| {
        let mut values: Vec<_> = super::props::mappings()
            .keys()
            .map(|&c| (c, upper(c), lower(upper(c))))
            .collect();
        values.sort_unstable();
        values
    })
}

/// `lower(upper(c))` -> every c with that image.
fn fold_classes() -> &'static HashMap<u32, Vec<u32>> {
    static T: OnceLock<HashMap<u32, Vec<u32>>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m: HashMap<u32, Vec<u32>> = HashMap::new();
        for &(c, _, f) in cased() {
            m.entry(f).or_default().push(c);
        }
        m
    })
}

fn ascii_alpha(c: u32) -> bool {
    (c as u8 as u32 == c) && (c as u8).is_ascii_alphabetic()
}

fn ascii_swap(c: u32) -> u32 {
    if ascii_alpha(c) { c ^ 0x20 } else { c }
}

/// `Pattern.single(ch)` under CASE_INSENSITIVE: the chars a literal matches.
pub fn single_ci(c: u32, unicode: bool) -> Vec<u32> {
    if unicode {
        let up = upper(c);
        let lo = lower(up);
        if up != lo {
            // SingleU(lower): ch == lower || lower(upper(ch)) == lower
            let mut v = vec![lo];
            v.extend(fold_classes().get(&lo).into_iter().flatten().copied());
            if lower(upper(lo)) == lo {
                v.push(lo);
            }
            v.sort_unstable();
            v.dedup();
            return v;
        }
        vec![c]
    } else if ascii_alpha(c) {
        vec![c, ascii_swap(c)]
    } else {
        vec![c]
    }
}

/// A literal inside a class: `Pattern.bitsOrSingle`. Latin-1 chars go into the BitClass, which folds ASCII letters
/// (and, with UNICODE_CASE, Latin-1 letters through `toLowerCase`/`toUpperCase`), except the chars whose Unicode
/// fold leaves Latin-1; those go through `single`.
pub fn class_single_ci(c: u32, unicode: bool) -> Vec<u32> {
    const SPECIAL: [u32; 10] = [0xff, 0xb5, 0x49, 0x69, 0x53, 0x73, 0x4b, 0x6b, 0xc5, 0xe5];
    if c < 256 && !(unicode && SPECIAL.contains(&c)) {
        if c < 128 {
            let mut v = vec![c, ascii_swap(c)];
            v.dedup();
            return v;
        }
        if unicode {
            let mut v = vec![c, lower(c), upper(c)];
            v.sort_unstable();
            v.dedup();
            return v;
        }
        return vec![c];
    }
    single_ci(c, unicode)
}

/// `CIRange` / `CIRangeU`: the extra chars a case-insensitive range matches, beyond the range itself.
pub fn range_ci_extra(lo: u32, hi: u32, unicode: bool) -> Vec<u32> {
    let inr = |x: u32| lo <= x && x <= hi;
    if unicode {
        cased()
            .iter()
            .filter(|&&(c, u, f)| !inr(c) && (inr(u) || inr(f)))
            .map(|&(c, _, _)| c)
            .collect()
    } else {
        (0u32..128)
            .filter(|&c| !inr(c) && ascii_alpha(c) && inr(ascii_swap(c)))
            .collect()
    }
}
