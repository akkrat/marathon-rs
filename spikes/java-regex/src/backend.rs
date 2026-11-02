//! The interface every candidate backend implements, and the Kotlin operations built on top of it.
//!
//! A backend only provides two primitives, full match and "find from a position"; `find`, `findAll`, `split` and
//! `replace` are written once here, the way Kotlin's `Regex` and Java's `Matcher` drive the engine. This is the
//! shape WP 1.4's `JavaRegex` takes (see docs/research/regex-spike-results.md, "API for WP 1.4").

/// Why a pattern did not compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// Java rejects the pattern too: `java.util.regex.PatternSyntaxException`.
    Syntax(String),
    /// D6: Java accepts the pattern but the backend cannot express it, so config load rejects it.
    Rejected { construct: String, reason: String },
    /// The backend failed on a pattern it should have accepted (a translator bug or an engine gap).
    Backend(String),
}

/// A match failure that is not "no match".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The engine exhausted a run-time resource budget.
    Aborted,
    Other(String),
}

/// A match, in byte offsets of the haystack. `groups[0]` is the whole match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub groups: Vec<Option<(usize, usize)>>,
}

impl Found {
    pub fn start(&self) -> usize {
        self.groups[0].map_or(0, |g| g.0)
    }
    pub fn end(&self) -> usize {
        self.groups[0].map_or(0, |g| g.1)
    }
}

pub trait Compiled {
    /// `Matcher.matches()`: the whole input.
    fn full_match(&self, input: &str) -> Result<bool, RunError>;
    /// `Matcher.find(from)` on a fresh matcher: the first match starting at or after byte offset `from`, with `\G`
    /// anchored at `from` and lookbehind seeing the whole input.
    fn find_at(&self, input: &str, from: usize) -> Result<Option<Found>, RunError>;
    /// Continuation on the same Matcher. After an empty match, `from` advances but `\G` stays at `previous_end`.
    fn find_next(&self, input: &str, from: usize, _previous_end: usize) -> Result<Option<Found>, RunError> {
        self.find_at(input, from)
    }
    /// Java's replacement-string parsing needs the group names.
    fn group_index(&self, name: &str) -> Option<usize>;
}

pub trait Backend: Sync {
    fn name(&self) -> &'static str;
    fn compile(&self, pattern: &str) -> Result<Box<dyn Compiled>, CompileError>;
}

/// The next search position after a match, as Java's `find()` loop and Kotlin's `MatchResult.next()` compute it:
/// the match end, plus one after an empty match. Java adds one UTF-16 unit; a `&str` can only advance by a whole
/// character, which is where the UTF-16 position model shows (docs/research/regex-spike-results.md).
fn next_from(input: &str, m: &Found) -> usize {
    let end = m.end();
    if end == m.start() {
        end + input[end..].chars().next().map_or(1, char::len_utf8)
    } else {
        end
    }
}

/// Kotlin `Regex.find(input)`.
pub fn find(re: &dyn Compiled, input: &str) -> Result<Option<Found>, RunError> {
    re.find_at(input, 0)
}

/// Kotlin `Regex.findAll(input)`: every next match comes from a fresh matcher's `find(nextIndex)`.
pub fn find_all(re: &dyn Compiled, input: &str) -> Result<Vec<Found>, RunError> {
    let mut out = Vec::new();
    let mut from = 0;
    while from <= input.len() {
        match re.find_at(input, from)? {
            None => break,
            Some(m) => {
                from = next_from(input, &m);
                out.push(m);
            }
        }
    }
    Ok(out)
}

/// Kotlin `Regex.split(input, limit)` (limit 0 = no limit). Unlike Java's `Pattern.split`, it keeps trailing empty
/// pieces and a leading empty piece before a zero-width match at 0.
pub fn split(re: &dyn Compiled, input: &str, limit: usize) -> Result<Vec<String>, RunError> {
    let mut first = if limit == 1 { None } else { re.find_at(input, 0)? };
    if first.is_none() {
        return Ok(vec![input.to_string()]);
    }
    let mut out = Vec::new();
    let mut last = 0;
    while let Some(m) = first {
        out.push(input[last..m.start()].to_string());
        last = m.end();
        if limit > 0 && out.len() == limit - 1 {
            break;
        }
        let from = next_from(input, &m);
        first = if from <= input.len() {
            re.find_next(input, from, m.end())?
        } else {
            None
        };
    }
    out.push(input[last..].to_string());
    Ok(out)
}

/// Kotlin `Regex.replace(input, replacement)` = Java `Matcher.replaceAll` with Java's replacement syntax.
pub fn replace(re: &dyn Compiled, input: &str, replacement: &str) -> Result<String, RunError> {
    let mut out = String::new();
    let mut last = 0;
    let mut from = 0;
    while from <= input.len() {
        let Some(m) = re.find_next(input, from, last)? else {
            break;
        };
        out.push_str(&input[last..m.start()]);
        expand(re, input, &m, replacement, &mut out)?;
        last = m.end();
        from = next_from(input, &m);
    }
    out.push_str(&input[last..]);
    Ok(out)
}

/// `Matcher.appendReplacement`'s parsing of `$n`, `${name}` and `\x`.
fn expand(re: &dyn Compiled, input: &str, m: &Found, repl: &str, out: &mut String) -> Result<(), RunError> {
    let cs: Vec<char> = repl.chars().collect();
    let mut i = 0;
    let err = |s: &str| RunError::Other(s.to_string());
    while i < cs.len() {
        match cs[i] {
            '\\' => {
                i += 1;
                let c = *cs.get(i).ok_or_else(|| err("character to be escaped is missing"))?;
                out.push(c);
                i += 1;
            }
            '$' => {
                i += 1;
                let group = match cs.get(i) {
                    None => return Err(err("Illegal group reference: group index is missing")),
                    Some('{') => {
                        let close = cs[i..]
                            .iter()
                            .position(|&c| c == '}')
                            .ok_or_else(|| err("named capturing group is missing trailing '}'"))?;
                        let name: String = cs[i + 1..i + close].iter().collect();
                        i += close + 1;
                        re.group_index(&name).ok_or_else(|| err("No group with name"))?
                    }
                    Some(c) if c.is_ascii_digit() => {
                        let mut n = c.to_digit(10).unwrap_or(0) as usize;
                        if n >= m.groups.len() {
                            return Err(err("No group"));
                        }
                        i += 1;
                        while let Some(d) = cs.get(i).and_then(|c| c.to_digit(10)) {
                            let next = n * 10 + d as usize;
                            if next >= m.groups.len() {
                                break;
                            }
                            n = next;
                            i += 1;
                        }
                        n
                    }
                    Some(_) => return Err(err("Illegal group reference")),
                };
                if let Some(Some((s, e))) = m.groups.get(group) {
                    out.push_str(&input[*s..*e]);
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(())
}
