//! The translator of docs/research/regex.md §3: parses a Java pattern the way `java.util.regex.Pattern` does
//! (JDK 21), resolves every inline flag at parse time, and emits a flag-free pattern for fancy-regex or PCRE2.
//!
//! Resolving the flags here is what keeps the semantics Java's: each literal and class becomes an explicit code
//! point set (Java's ASCII-only or Unicode case folding included), `.` `^` `$` `\b` become explicit lookarounds over
//! Java's line terminators and word chars, so the target engine's own flag semantics never come into play.

mod case;
mod emit;
mod props;

pub use emit::{Dialect, emit};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Java throws `PatternSyntaxException` with this description.
    Syntax(String),
    /// D6.
    Rejected { construct: String, reason: String },
}

/// A set of code points; always matches exactly one.
#[derive(Debug, Clone)]
pub enum CSet {
    Ranges(Vec<(u32, u32)>),
    Union(Vec<CSet>),
    Inter(Box<CSet>, Box<CSet>),
    Not(Box<CSet>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Greedy,
    Lazy,
    Possessive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKind {
    Capture,
    NonCapture,
    Atomic,
    Ahead(bool),
    Behind(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assertion {
    /// `\A`, and `^` without MULTILINE.
    Begin,
    /// `\z`.
    End,
    /// `$` and `\Z`: (multiline, unix_lines).
    Dollar(bool, bool),
    /// `^` with MULTILINE: unix_lines.
    Caret(bool),
    /// `\G`.
    LastMatch,
    /// `\b` / `\B`: (negated, unicode word chars).
    Bound(bool, bool),
}

#[derive(Debug, Clone)]
pub enum Node {
    Set(CSet),
    Seq(Vec<Node>),
    Alt(Vec<Node>),
    Group(GroupKind, Box<Node>),
    Repeat {
        body: Box<Node>,
        min: u32,
        max: Option<u32>,
        mode: Mode,
    },
    Backref(usize),
    Assert(Assertion),
    /// `\R`.
    Linebreak,
    /// `\X`, retained until complete Java syntax validation before D6.
    Grapheme,
}

#[derive(Debug, Clone, Copy, Default)]
struct Flags {
    i: bool,
    d: bool,
    m: bool,
    s: bool,
    u: bool,
    x: bool,
    uc: bool,
}

pub struct Parsed {
    pub(crate) node: Node,
    pub groups: usize,
    pub names: Vec<(String, usize)>,
}

struct Parser {
    p: Vec<char>,
    pos: usize,
    flags: Flags,
    groups: usize,
    names: Vec<(String, usize)>,
    depth: usize,
    source: Vec<char>,
    origins: Vec<usize>,
    rejection: Option<ParseError>,
}

fn syntax<T>(m: &str) -> Result<T, ParseError> {
    Err(ParseError::Syntax(m.to_string()))
}

fn single(c: u32) -> CSet {
    CSet::Ranges(vec![(c, c)])
}

fn chars(v: Vec<u32>) -> CSet {
    CSet::Ranges(v.into_iter().map(|c| (c, c)).collect())
}

fn is_line_sep(c: char, unix: bool) -> bool {
    if unix {
        c == '\n'
    } else {
        matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
    }
}

/// `Pattern.RemoveQEQuoting`: rewrites `\Q…\E` spans into escaped literals before parsing, so quoting also works
/// inside classes and in comments mode.
fn remove_qe_quoting(p: &[char]) -> (Vec<char>, Vec<usize>) {
    let mut out = Vec::new();
    let mut origins = Vec::new();
    let mut push = |c, origin| {
        out.push(c);
        origins.push(origin);
    };
    let mut i = 0;
    let mut quoted = false;
    let mut begin = false;
    while i < p.len() {
        let origin = i;
        let c = p[i];
        i += 1;
        if quoted {
            if c == '\\' && p.get(i) == Some(&'E') {
                quoted = false;
                i += 1;
                continue;
            }
            if c.is_ascii_digit() && begin {
                for c in ['\\', 'x', '3'] {
                    push(c, origin);
                }
            } else if c.is_ascii() && !c.is_ascii_alphanumeric() {
                push('\\', origin);
            }
            push(c, origin);
            begin = false;
        } else if c == '\\' && p.get(i) == Some(&'Q') {
            quoted = true;
            begin = true;
            i += 1;
        } else {
            push(c, origin);
            if c == '\\' && i < p.len() {
                push(p[i], i);
                i += 1;
            }
        }
    }
    origins.push(p.len());
    (out, origins)
}

pub fn parse(pattern: &str) -> Result<Parsed, ParseError> {
    let chars: Vec<char> = pattern.chars().collect();
    let (p, origins) = remove_qe_quoting(&chars);
    let mut ps = Parser {
        p,
        source: chars,
        origins,
        pos: 0,
        flags: Flags::default(),
        groups: 0,
        names: Vec::new(),
        depth: 0,
        rejection: None,
    };
    let node = ps.expr()?;
    match ps.peek() {
        None => {}
        Some(')') => return syntax("Unmatched closing ')'"),
        Some(_) => return syntax("Unexpected internal error"),
    }
    if let Some(rejection) = ps.rejection {
        return Err(rejection);
    }
    Ok(Parsed {
        node,
        groups: ps.groups,
        names: ps.names,
    })
}

impl Parser {
    fn reject(&mut self, construct: String, reason: &str) {
        if self.rejection.is_none() {
            self.rejection = Some(ParseError::Rejected {
                construct,
                reason: reason.to_string(),
            });
        }
    }

    fn peek(&mut self) -> Option<char> {
        if self.flags.x {
            self.skip_x();
        }
        self.p.get(self.pos).copied()
    }

    fn read(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn hex_digits(&mut self, count: usize) -> Result<u32, ParseError> {
        let mut value = 0;
        for _ in 0..count {
            let Some(digit) = self.read().and_then(|c| c.to_digit(16)) else {
                return syntax("Illegal hexadecimal escape sequence");
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn raw(&self) -> Option<char> {
        self.p.get(self.pos).copied()
    }

    fn at(&self, k: usize) -> Option<char> {
        self.p.get(self.pos + k).copied()
    }

    /// Comments mode: ASCII whitespace and `#` to the end of the line.
    fn skip_x(&mut self) {
        loop {
            match self.p.get(self.pos) {
                Some(' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r') => self.pos += 1,
                Some('#') => {
                    while let Some(&c) = self.p.get(self.pos) {
                        if is_line_sep(c, self.flags.d) {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn expr(&mut self) -> Result<Node, ParseError> {
        let mut alts = vec![self.seq()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            alts.push(self.seq()?);
        }
        Ok(if alts.len() == 1 {
            alts.pop().unwrap_or(Node::Seq(vec![]))
        } else {
            Node::Alt(alts)
        })
    }

    fn seq(&mut self) -> Result<Node, ParseError> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            let atom = match c {
                '|' | ')' => break,
                '(' => {
                    self.pos += 1;
                    match self.group()? {
                        Some(n) => n,
                        None => continue,
                    }
                }
                '[' => {
                    self.pos += 1;
                    Node::Set(self.class()?)
                }
                '\\' => {
                    self.pos += 1;
                    self.escape_atom()?
                }
                '^' => {
                    self.pos += 1;
                    Node::Assert(if self.flags.m {
                        Assertion::Caret(self.flags.d)
                    } else {
                        Assertion::Begin
                    })
                }
                '$' => {
                    self.pos += 1;
                    Node::Assert(Assertion::Dollar(self.flags.m, self.flags.d))
                }
                '.' => {
                    self.pos += 1;
                    Node::Set(if self.flags.s {
                        CSet::Ranges(vec![(0, 0x10FFFF)])
                    } else if self.flags.d {
                        CSet::Not(Box::new(single(10)))
                    } else {
                        CSet::Not(Box::new(CSet::Ranges(vec![
                            (10, 10),
                            (13, 13),
                            (0x85, 0x85),
                            (0x2028, 0x2029),
                        ])))
                    })
                }
                '?' | '*' | '+' => return syntax(&format!("Dangling meta character '{c}'")),
                // A `{` where an atom should be quantifies an empty atom (Pattern.atom returns an empty slice).
                '{' => Node::Seq(vec![]),
                _ => {
                    self.pos += 1;
                    self.literal(c as u32)
                }
            };
            let atom = self.closure(atom)?;
            items.push(atom);
        }
        Ok(if items.len() == 1 {
            items.pop().unwrap_or(Node::Seq(vec![]))
        } else {
            Node::Seq(items)
        })
    }

    fn literal(&self, c: u32) -> Node {
        Node::Set(if self.flags.i {
            chars(case::single_ci(c, self.flags.u))
        } else {
            single(c)
        })
    }

    fn closure(&mut self, atom: Node) -> Result<Node, ParseError> {
        let (min, max) = match self.peek() {
            Some('?') => {
                self.pos += 1;
                (0, Some(1))
            }
            Some('*') => {
                self.pos += 1;
                (0, None)
            }
            Some('+') => {
                self.pos += 1;
                (1, None)
            }
            Some('{') => {
                if !self.at(1).is_some_and(|c| c.is_ascii_digit()) {
                    return syntax("Illegal repetition");
                }
                self.pos += 1;
                let min = self.number()?;
                let mut max = Some(min);
                if self.peek() == Some(',') {
                    self.pos += 1;
                    max = if self.peek() == Some('}') {
                        None
                    } else {
                        Some(self.number()?)
                    };
                }
                if self.peek() != Some('}') {
                    return syntax("Unclosed counted closure");
                }
                self.pos += 1;
                if max.is_some_and(|m| m < min) {
                    return syntax("Illegal repetition range");
                }
                (min, max)
            }
            _ => return Ok(atom),
        };
        let mode = match self.peek() {
            Some('?') => {
                self.pos += 1;
                Mode::Lazy
            }
            Some('+') => {
                self.pos += 1;
                Mode::Possessive
            }
            _ => Mode::Greedy,
        };
        Ok(Node::Repeat {
            body: Box::new(atom),
            min,
            max,
            mode,
        })
    }

    fn number(&mut self) -> Result<u32, ParseError> {
        let mut n: u32 = 0;
        let mut any = false;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            any = true;
            n = match n
                .checked_mul(10)
                .and_then(|n| n.checked_add(d))
                .filter(|&n| n <= i32::MAX as u32)
            {
                Some(n) => n,
                None => return syntax("Illegal repetition range"),
            };
            self.pos += 1;
        }
        if !any {
            return syntax("Unclosed counted closure");
        }
        Ok(n)
    }

    /// After `(`. `None` for a bare flag group `(?i)`, whose flags stay in force to the end of the enclosing group.
    fn group(&mut self) -> Result<Option<Node>, ParseError> {
        self.enter()?;
        let result = self.group_inner();
        self.depth -= 1;
        result
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        if self.depth >= 64 {
            return syntax("Stack overflow during pattern compilation");
        }
        self.depth += 1;
        Ok(())
    }

    fn spelling(&self, start: usize) -> String {
        self.source[self.origins[start]..self.origins[self.pos]]
            .iter()
            .collect()
    }

    fn group_inner(&mut self) -> Result<Option<Node>, ParseError> {
        let start = self.pos - 1;
        let save = self.flags;
        let kind = if self.peek() == Some('?') {
            self.pos += 1;
            match self.peek() {
                Some(':') => {
                    self.pos += 1;
                    GroupKind::NonCapture
                }
                Some('=') => {
                    self.pos += 1;
                    GroupKind::Ahead(false)
                }
                Some('!') => {
                    self.pos += 1;
                    GroupKind::Ahead(true)
                }
                Some('>') => {
                    self.pos += 1;
                    GroupKind::Atomic
                }
                Some('<') => {
                    self.pos += 1;
                    match self.peek() {
                        Some('=') => {
                            self.pos += 1;
                            GroupKind::Behind(false)
                        }
                        Some('!') => {
                            self.pos += 1;
                            GroupKind::Behind(true)
                        }
                        Some(c) if c.is_ascii_alphabetic() => {
                            let name = self.group_name()?;
                            if self.names.iter().any(|n| n.0 == name) {
                                return syntax(&format!("Named capturing group <{name}> is already defined"));
                            }
                            self.groups += 1;
                            self.names.push((name, self.groups));
                            GroupKind::Capture
                        }
                        _ => return syntax("capturing group name does not start with a Latin letter"),
                    }
                }
                _ => {
                    self.inline_flags()?;
                    match self.peek() {
                        Some(')') => {
                            self.pos += 1;
                            return Ok(None);
                        }
                        Some(':') => {
                            self.pos += 1;
                            GroupKind::NonCapture
                        }
                        _ => return syntax("Unknown inline modifier"),
                    }
                }
            }
        } else {
            self.groups += 1;
            GroupKind::Capture
        };
        let body = self.expr()?;
        if self.peek() != Some(')') {
            return syntax("Unclosed group");
        }
        self.pos += 1;
        self.flags = save;
        if let GroupKind::Behind(_) = kind {
            if !tree_info(&body).max_valid {
                return syntax("Look-behind group does not have an obvious maximum length");
            }
            if fixed_len(&body).is_none() {
                self.reject(
                    self.spelling(start),
                    "variable-length lookbehind has different matching semantics in the Rust regex backend",
                );
            } else if can_match_surrogate_or_supplementary(&body) {
                self.reject(
                    self.spelling(start),
                    "lookbehind over supplementary code points or surrogate code units has different matching semantics in the Rust regex backend",
                );
            }
        }
        if matches!(kind, GroupKind::Ahead(true) | GroupKind::Behind(true)) && has_capture(&body) {
            self.reject(
                self.spelling(start),
                "captures in negative lookaround have different lifetimes in the Rust regex backend; use noncapturing groups",
            );
        }
        Ok(Some(Node::Group(kind, Box::new(body))))
    }

    fn group_name(&mut self) -> Result<String, ParseError> {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() {
                name.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.peek() != Some('>') {
            return syntax("named capturing group is missing trailing '>'");
        }
        self.pos += 1;
        Ok(name)
    }

    fn inline_flags(&mut self) -> Result<(), ParseError> {
        let mut on = true;
        while let Some(c) = self.peek() {
            let f = &mut self.flags;
            match c {
                'i' => f.i = on,
                'm' => f.m = on,
                's' => f.s = on,
                'd' => f.d = on,
                'u' => f.u = on,
                // `(?c)` compiles on the JDK but changes nothing (probed: CANON_EQ flag on vs `(?c)` differ), so
                // ignoring it is faithful, not a silent reinterpretation.
                'c' => {}
                'x' => f.x = on,
                // UNICODE_CHARACTER_CLASS sets and clears UNICODE_CASE with it (Pattern.addFlag, subFlag).
                'U' => {
                    f.uc = on;
                    f.u = on;
                }
                '-' if on => {
                    on = false;
                }
                '-' => return syntax("Unknown inline modifier"),
                _ => return Ok(()),
            }
            self.pos += 1;
        }
        Ok(())
    }

    fn word_set(&self) -> CSet {
        if self.flags.uc {
            props::word()
        } else {
            CSet::Ranges(vec![(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)])
        }
    }

    /// `\d \D \s \S \w \W \h \H \v \V \p \P`: `Some` when `c` is one of them.
    fn predefined(&mut self, c: char) -> Result<Option<CSet>, ParseError> {
        let uc = self.flags.uc;
        let s = match c {
            'd' | 'D' => {
                if uc {
                    props::property("Nd")
                } else {
                    CSet::Ranges(vec![(0x30, 0x39)])
                }
            }
            's' | 'S' => {
                if uc {
                    props::white_space()
                } else {
                    CSet::Ranges(vec![(9, 13), (0x20, 0x20)])
                }
            }
            'w' | 'W' => self.word_set(),
            'h' | 'H' => CSet::Ranges(vec![
                (9, 9),
                (0x20, 0x20),
                (0xA0, 0xA0),
                (0x1680, 0x1680),
                (0x180E, 0x180E),
                (0x2000, 0x200A),
                (0x202F, 0x202F),
                (0x205F, 0x205F),
                (0x3000, 0x3000),
            ]),
            'v' | 'V' => CSet::Ranges(vec![(10, 13), (0x85, 0x85), (0x2028, 0x2029)]),
            'p' | 'P' => {
                let name: String = match self.peek() {
                    Some('{') => {
                        self.pos += 1;
                        let start = self.pos;
                        while self.peek().is_some_and(|c| c != '}') {
                            self.pos += 1;
                        }
                        if self.raw() != Some('}') {
                            return syntax("Unclosed character family");
                        }
                        let end = self.pos;
                        self.pos += 1;
                        if end == start {
                            return syntax("Empty character family");
                        }
                        self.p[start..end].iter().collect()
                    }
                    Some(ch) => {
                        self.pos += 1;
                        ch.to_string()
                    }
                    None => return syntax("Illegal/unsupported escape sequence"),
                };
                props::family(&name, self.flags.i, uc)?
            }
            _ => return Ok(None),
        };
        Ok(Some(if c.is_ascii_uppercase() {
            CSet::Not(Box::new(s))
        } else {
            s
        }))
    }

    /// An escape that stands for one code point (`\t`, `\x41`, `A`, `\0101`, `\cA`, `\\`), after the backslash
    /// and the escape letter `c`.
    fn char_escape(&mut self, c: char) -> Result<Option<u32>, ParseError> {
        let start = self.pos - 2;
        let v = match c {
            't' => 9,
            'n' => 10,
            'r' => 13,
            'f' => 12,
            'a' => 7,
            'e' => 0x1B,
            '0' => {
                let Some(first) = self.read().and_then(|c| c.to_digit(8)) else {
                    return syntax("Illegal octal escape sequence");
                };
                let mut value = first;
                for count in 1..3 {
                    if count == 2 && first > 3 {
                        break;
                    }
                    let Some(digit) = self.peek().and_then(|c| c.to_digit(8)) else {
                        break;
                    };
                    self.pos += 1;
                    value = value * 8 + digit;
                }
                value
            }
            'c' => match self.read() {
                Some(c) => (c as u32) ^ 64,
                None => return syntax("Illegal control escape sequence"),
            },
            'x' => {
                if self.peek() == Some('{') {
                    self.pos += 1;
                    let mut value = 0u32;
                    let mut digits = 0;
                    while let Some(digit) = self.peek().and_then(|c| c.to_digit(16)) {
                        self.pos += 1;
                        digits += 1;
                        value = value * 16 + digit;
                        if value > 0x10ffff {
                            return syntax("Hexadecimal codepoint is too big");
                        }
                    }
                    if digits == 0 || self.read() != Some('}') {
                        return syntax("Unclosed hexadecimal escape sequence");
                    }
                    value
                } else {
                    self.hex_digits(2)?
                }
            }
            'u' => {
                let high = self.hex_digits(4)?;
                if (0xd800..0xdc00).contains(&high) {
                    let save = self.pos;
                    if self.read() == Some('\\') && self.read() == Some('u') {
                        let low = self.hex_digits(4)?;
                        if (0xdc00..0xe000).contains(&low) {
                            return Ok(Some(0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)));
                        }
                    }
                    self.pos = save;
                }
                high
            }
            'N' => {
                if self.read() != Some('{') {
                    return syntax("Illegal character name escape sequence");
                }
                let name_start = self.pos;
                while self.peek().is_some_and(|c| c != '}') {
                    self.pos += 1;
                }
                if self.peek() != Some('}') {
                    return syntax("Unclosed character name escape sequence");
                }
                let name: String = self.p[name_start..self.pos].iter().collect();
                self.pos += 1;
                match props::named(&name) {
                    Some(value) => value,
                    None => return syntax("Unknown character name"),
                }
            }
            c if c.is_ascii_alphanumeric() => return Ok(None),
            c => c as u32,
        };
        if (0xD800..0xE000).contains(&v) {
            self.reject(self.spelling(start), "a lone surrogate cannot occur in Rust strings");
        }
        Ok(Some(v))
    }

    /// After a backslash, outside a class.
    fn escape_atom(&mut self) -> Result<Node, ParseError> {
        let start = self.pos - 1;
        let Some(c) = self.raw() else {
            return syntax("Unescaped trailing backslash");
        };
        self.pos += 1;
        if let Some(s) = self.predefined(c)? {
            return Ok(Node::Set(s));
        }
        let node = match c {
            'A' => Node::Assert(Assertion::Begin),
            'z' => Node::Assert(Assertion::End),
            'Z' => Node::Assert(Assertion::Dollar(false, self.flags.d)),
            'G' => Node::Assert(Assertion::LastMatch),
            'b' | 'B' => {
                if c == 'b' && self.peek() == Some('{') && self.at(1) == Some('g') {
                    self.pos += 2;
                    if self.read() != Some('}') {
                        return syntax("Illegal Unicode extended grapheme cluster boundary");
                    }
                    self.reject(
                        self.spelling(start),
                        "grapheme cluster boundaries are not supported by the Rust regex backend",
                    );
                    return Ok(Node::Assert(Assertion::Bound(false, self.flags.uc)));
                }
                Node::Assert(Assertion::Bound(c == 'B', self.flags.uc))
            }
            'R' => Node::Linebreak,
            'X' => {
                self.reject(
                    self.spelling(start),
                    "extended grapheme cluster matching is not supported by the Rust regex backend",
                );
                Node::Grapheme
            }
            'k' => {
                if self.peek() != Some('<') {
                    return syntax("\\k is not followed by '<' for named capturing group");
                }
                self.pos += 1;
                let name = self.group_name()?;
                let Some(&(_, n)) = self.names.iter().find(|x| x.0 == name) else {
                    return syntax(&format!("named capturing group <{name}> does not exist"));
                };
                self.backref(n, start)?
            }
            '1'..='9' => {
                // Java takes further digits only while the number names an existing group.
                let mut n = c.to_digit(10).unwrap_or(0) as usize;
                loop {
                    let end = self.pos;
                    let Some(d) = self.peek().and_then(|c| c.to_digit(10)) else {
                        self.pos = end;
                        break;
                    };
                    let next = n * 10 + d as usize;
                    if next > self.groups {
                        self.pos = end;
                        break;
                    }
                    n = next;
                    self.pos += 1;
                }
                self.backref(n, start)?
            }
            _ => match self.char_escape(c)? {
                Some(v) => self.literal(v),
                None => return syntax("Illegal/unsupported escape sequence"),
            },
        };
        Ok(node)
    }

    fn backref(&mut self, n: usize, start: usize) -> Result<Node, ParseError> {
        if self.flags.i && n <= self.groups {
            self.reject(
                self.spelling(start),
                "case-insensitive backreferences fold differently in the Rust regex backend",
            );
        }
        Ok(Node::Backref(n))
    }

    /// After `[`. `Pattern.clazz`: union of items, `&&` intersection, `^` negating the whole class.
    fn class(&mut self) -> Result<CSet, ParseError> {
        let negated = if self.raw() == Some('^') {
            self.pos += 1;
            true
        } else {
            false
        };
        let body = self.class_body(true)?;
        Ok(if negated { CSet::Not(Box::new(body)) } else { body })
    }

    fn class_body(&mut self, consume: bool) -> Result<CSet, ParseError> {
        self.enter()?;
        let result = self.class_body_inner(consume).and_then(|set| {
            if set_depth(&set) > 64 {
                syntax("Stack overflow during pattern compilation")
            } else {
                Ok(set)
            }
        });
        self.depth -= 1;
        result
    }

    fn class_body_inner(&mut self, consume: bool) -> Result<CSet, ParseError> {
        let mut intersections = 0;
        let mut items: Vec<CSet> = Vec::new();
        let mut first = true;
        loop {
            match self.peek() {
                None => return syntax("Unclosed character class"),
                Some(']') if first => {
                    self.pos += 1;
                    items.push(self.class_literal(']' as u32));
                }
                Some(']') => {
                    if consume {
                        self.pos += 1;
                    }
                    return Ok(CSet::Union(items));
                }
                Some('[') => {
                    self.pos += 1;
                    items.push(self.class()?);
                }
                Some('&') if self.intersection_follows() => {
                    self.pos += 1;
                    self.peek();
                    self.pos += 1;
                    let rhs = self.intersection_rhs()?;
                    intersections += 1;
                    if intersections >= 64 {
                        return syntax("Stack overflow during pattern compilation");
                    }
                    if items.is_empty() {
                        items.push(rhs);
                    } else if !matches!(&rhs, CSet::Union(v) if v.is_empty()) {
                        let intersection = CSet::Inter(Box::new(CSet::Union(items)), Box::new(rhs));
                        if set_depth(&intersection) > 64 {
                            return syntax("Stack overflow during pattern compilation");
                        }
                        items = vec![intersection];
                    }
                }
                Some(_) => {
                    let item = self.class_item()?;
                    if let ClassItem::Char(lo) = item
                        && self.peek() == Some('-')
                        && self.at(1).is_some_and(|c| c != ']')
                    {
                        self.pos += 1;
                        match self.class_item()? {
                            ClassItem::Char(hi) => {
                                if hi < lo {
                                    return syntax("Illegal character range");
                                }
                                items.push(self.class_range(lo, hi));
                            }
                            ClassItem::Set(_) => return syntax("Illegal character range"),
                        }
                    } else {
                        items.push(match item {
                            ClassItem::Char(c) => self.class_literal(c),
                            ClassItem::Set(s) => s,
                        });
                    }
                }
            }
            first = false;
        }
    }

    fn intersection_rhs(&mut self) -> Result<CSet, ParseError> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None => return syntax("Unclosed character class"),
                Some(']') => break,
                Some('&') if self.intersection_follows() => break,
                Some('[') => {
                    self.pos += 1;
                    items.push(self.class()?);
                }
                Some(_) => items.push(self.class_body(false)?),
            }
        }
        Ok(CSet::Union(items))
    }

    fn class_literal(&self, c: u32) -> CSet {
        if self.flags.i {
            chars(case::class_single_ci(c, self.flags.u))
        } else {
            single(c)
        }
    }

    fn class_range(&self, lo: u32, hi: u32) -> CSet {
        let mut v = vec![(lo, hi)];
        if self.flags.i {
            v.extend(case::range_ci_extra(lo, hi, self.flags.u).into_iter().map(|c| (c, c)));
        }
        CSet::Ranges(v)
    }

    fn intersection_follows(&mut self) -> bool {
        let save = self.pos;
        self.pos += 1;
        let follows = self.peek() == Some('&');
        self.pos = save;
        follows
    }

    fn class_item(&mut self) -> Result<ClassItem, ParseError> {
        let Some(c) = self.peek() else {
            return syntax("Unclosed character class");
        };
        self.pos += 1;
        if c != '\\' {
            return Ok(ClassItem::Char(c as u32));
        }
        let Some(e) = self.raw() else {
            return syntax("Unclosed character class");
        };
        self.pos += 1;
        if let Some(s) = self.predefined(e)? {
            return Ok(ClassItem::Set(s));
        }
        match self.char_escape(e)? {
            Some(v) => Ok(ClassItem::Char(v)),
            None => syntax("Illegal/unsupported escape sequence"),
        }
    }
}

enum ClassItem {
    Char(u32),
    Set(CSet),
}

/// Port of OpenJDK 21 Pattern.TreeInfo and Node.study (Curly/GroupCurly, Ques, Branch, Loop).
/// In particular, a nondeterministic *group* closure becomes Loop even for {0} or {1}; its maximum is invalid.
#[derive(Clone, Copy)]
struct TreeInfo {
    min: i32,
    max: i32,
    max_valid: bool,
    deterministic: bool,
}

impl Default for TreeInfo {
    fn default() -> Self {
        Self {
            min: 0,
            max: 0,
            max_valid: true,
            deterministic: true,
        }
    }
}

fn tree_info(node: &Node) -> TreeInfo {
    let mut info = TreeInfo::default();
    study(node, &mut info);
    info
}

fn study(node: &Node, info: &mut TreeInfo) {
    match node {
        Node::Set(_) | Node::Linebreak => {
            info.min = info.min.wrapping_add(1);
            info.max = info
                .max
                .wrapping_add(if matches!(node, Node::Linebreak) { 2 } else { 1 });
        }
        Node::Grapheme => {
            info.min = info.min.wrapping_add(1);
            info.deterministic = false;
        }
        Node::Seq(_) => {
            fn flatten<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
                match node {
                    Node::Seq(v) => {
                        for node in v {
                            flatten(node, out);
                        }
                    }
                    Node::Group(GroupKind::Capture | GroupKind::NonCapture | GroupKind::Atomic, body) => {
                        flatten(body, out)
                    }
                    _ => out.push(node),
                }
            }
            fn chain(nodes: &[&Node], info: &mut TreeInfo) {
                // Branch.study resets the accumulated info while studying its continuation, then adds its
                // saved bounds back. Keep those continuations on a vector so a wide sequence cannot overflow.
                let mut branches = Vec::new();
                for node in nodes {
                    let branch = matches!(node, Node::Alt(_))
                        || matches!(node,
                        Node::Repeat { body, min: 0, max: Some(1), mode, .. }
                            if *mode != Mode::Possessive && matches!(&**body,
                                Node::Group(GroupKind::Capture | GroupKind::NonCapture, _)));
                    if branch {
                        let prefix = *info;
                        let branch = tree_info(node);
                        branches.push((prefix, branch));
                        *info = TreeInfo::default();
                    } else {
                        study(node, info);
                    }
                }
                for (prefix, branch) in branches.into_iter().rev() {
                    info.min = info.min.wrapping_add(prefix.min).wrapping_add(branch.min);
                    info.max = info.max.wrapping_add(prefix.max).wrapping_add(branch.max);
                    info.max_valid &= prefix.max_valid && branch.max_valid;
                    info.deterministic = false;
                }
            }
            let mut nodes = Vec::new();
            flatten(node, &mut nodes);
            chain(&nodes, info);
        }
        Node::Alt(nodes) => {
            let mut min = i32::MAX;
            let mut max = -1;
            for node in nodes {
                let branch = tree_info(node);
                min = min.min(branch.min);
                max = max.max(branch.max);
                info.max_valid &= branch.max_valid;
            }
            info.min = info.min.wrapping_add(min);
            info.max = info.max.wrapping_add(max);
            info.deterministic = false;
        }
        Node::Group(GroupKind::Ahead(_) | GroupKind::Behind(_), _) | Node::Assert(_) => {}
        Node::Group(_, body) => study(body, info),
        Node::Backref(_) => info.max_valid = false,
        Node::Repeat { body, min, max, mode } => {
            let atom = tree_info(body);
            let optional = *min == 0 && *max == Some(1);
            let group = matches!(&**body, Node::Group(GroupKind::Capture | GroupKind::NonCapture, _));
            if group && !optional && *mode != Mode::Possessive && !atom.deterministic {
                info.max_valid = false;
                info.deterministic = false;
            } else if optional {
                let min = info.min;
                study(body, info);
                info.min = min;
                info.deterministic = false;
            } else if matches!(&**body, Node::Set(_)) && max.is_none() && *mode == Mode::Greedy {
                // CharPropertyGreedy uses unchecked int addition, unlike Curly's overflow validation.
                info.min = info.min.wrapping_add(*min as i32);
                info.max = info.max.wrapping_add(i32::MAX);
                info.deterministic = false;
            } else {
                let new_min = atom.min.wrapping_mul(*min as i32).wrapping_add(info.min);
                info.min = if new_min < info.min { 0x0fffffff } else { new_min };
                let new_max = atom
                    .max
                    .wrapping_mul(max.unwrap_or(i32::MAX as u32) as i32)
                    .wrapping_add(info.max);
                info.max_valid &= atom.max_valid && new_max >= info.max;
                info.max = new_max;
                info.deterministic &= atom.deterministic && Some(*min) == *max;
            }
        }
    }
}

pub(super) fn has_capture(n: &Node) -> bool {
    match n {
        Node::Seq(v) | Node::Alt(v) => v.iter().any(has_capture),
        Node::Group(GroupKind::Capture, _) => true,
        Node::Group(_, b) | Node::Repeat { body: b, .. } => has_capture(b),
        _ => false,
    }
}

fn can_match_surrogate_or_supplementary(node: &Node) -> bool {
    match node {
        Node::Set(set) => {
            fn contains(set: &CSet, cp: u32) -> bool {
                match set {
                    CSet::Ranges(v) => v.iter().any(|&(lo, hi)| lo <= cp && cp <= hi),
                    CSet::Union(v) => v.iter().any(|set| contains(set, cp)),
                    CSet::Inter(a, b) => contains(a, cp) && contains(b, cp),
                    CSet::Not(a) => !contains(a, cp),
                }
            }
            fn boundaries(set: &CSet, points: &mut Vec<u32>) {
                match set {
                    CSet::Ranges(v) => {
                        for &(lo, hi) in v {
                            points.extend([lo, hi + 1]);
                        }
                    }
                    CSet::Union(v) => {
                        for set in v {
                            boundaries(set, points);
                        }
                    }
                    CSet::Inter(a, b) => {
                        boundaries(a, points);
                        boundaries(b, points);
                    }
                    CSet::Not(a) => boundaries(a, points),
                }
            }
            let mut points = vec![0xd800, 0x10000];
            boundaries(set, &mut points);
            points
                .into_iter()
                .any(|cp| ((0xd800..0xe000).contains(&cp) || (0x10000..=0x10ffff).contains(&cp)) && contains(set, cp))
        }
        Node::Seq(v) | Node::Alt(v) => v.iter().any(can_match_surrogate_or_supplementary),
        Node::Group(_, b) => can_match_surrogate_or_supplementary(b),
        Node::Repeat { max: Some(0), .. } => false,
        Node::Repeat { body, .. } => can_match_surrogate_or_supplementary(body),
        Node::Backref(_) | Node::Grapheme => true,
        _ => false,
    }
}

/// A lookbehind is supported only when every successful path consumes the same number of code points.
fn fixed_len(n: &Node) -> Option<u64> {
    match n {
        Node::Set(_) => Some(1),
        Node::Seq(v) => v.iter().try_fold(0u64, |sum, n| sum.checked_add(fixed_len(n)?)),
        Node::Alt(v) => {
            let first = v.first().map_or(Some(0), fixed_len)?;
            v.iter().all(|n| fixed_len(n) == Some(first)).then_some(first)
        }
        Node::Group(GroupKind::Ahead(_) | GroupKind::Behind(_), _) | Node::Assert(_) => Some(0),
        Node::Group(_, b) => fixed_len(b),
        Node::Repeat { max: Some(0), .. } => Some(0),
        Node::Repeat { body, min, max, .. } if Some(*min) == *max => fixed_len(body)?.checked_mul(*min as u64),
        Node::Repeat { body, .. } if fixed_len(body) == Some(0) => Some(0),
        _ => None,
    }
}

fn set_depth(set: &CSet) -> usize {
    match set {
        CSet::Ranges(_) => 1,
        CSet::Union(v) => 1 + v.iter().map(set_depth).max().unwrap_or(0),
        CSet::Inter(a, b) => 1 + set_depth(a).max(set_depth(b)),
        CSet::Not(a) => 1 + set_depth(a),
    }
}
