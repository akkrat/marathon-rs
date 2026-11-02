//! Emits a parsed Java pattern as a flag-free fancy-regex or PCRE2 pattern.

use super::{Assertion, CSet, GroupKind, Mode, Node, Parsed, has_capture, tree_info};
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Fancy,
    Pcre2,
}

const ANY: &str = r"[\x{0}-\x{10FFFF}]";
/// Java's line terminators (`.` without DOTALL excludes them).
const TERMINATORS: &str = r"[\x{A}\x{D}\x{85}\x{2028}\x{2029}]";

struct Emitter {
    d: Dialect,
    out: String,
    groups: usize,
    full: bool,
}

#[derive(Clone, Copy)]
struct Ctx {
    /// Inside the body of a quantifier that Java evaluates one iteration at a time (see `TreeInfo`).
    atomic_iter: bool,
    /// Nothing but the end of the match follows.
    tail: bool,
}

/// The whole pattern, wrapped for `Matcher.find` (`full == false`) or `Matcher.matches`.
pub fn emit(parsed: &Parsed, d: Dialect, full: bool) -> String {
    let mut e = Emitter {
        d,
        out: String::new(),
        groups: parsed.groups,
        full,
    };
    if full {
        e.out.push_str(r"\A(?:");
    }
    e.node(
        &parsed.node,
        Ctx {
            atomic_iter: false,
            tail: true,
        },
    );
    if full {
        e.out.push_str(r")\z");
    }
    e.out
}

fn hex(out: &mut String, c: u32) {
    let _ = write!(out, r"\x{{{c:X}}}");
}

impl Emitter {
    fn node(&mut self, n: &Node, ctx: Ctx) {
        match n {
            Node::Set(s) => {
                let t = self.set(s);
                self.out.push_str(&t);
            }
            Node::Seq(v) => {
                self.out.push_str("(?:");
                for (i, x) in v.iter().enumerate() {
                    // A leading, capture-free empty lookahead is always true. Leaving it in the emitted pattern
                    // needlessly forces an otherwise regular loop into the VM (A10/A11's benign stack/budget cases).
                    if i == 0
                        && v.len() > 1
                        && matches!(x, Node::Group(GroupKind::Ahead(false), b) if matches!(&**b, Node::Seq(v) if v.is_empty()))
                    {
                        continue;
                    }
                    self.node(
                        x,
                        Ctx {
                            tail: ctx.tail && i + 1 == v.len(),
                            ..ctx
                        },
                    );
                }
                self.out.push(')');
            }
            Node::Alt(v) => {
                self.out.push_str("(?:");
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        self.out.push('|');
                    }
                    self.node(x, ctx);
                }
                self.out.push(')');
            }
            Node::Group(GroupKind::Ahead(true), body) if regular_dollar_tail(body) => {
                self.negative_dollar(body);
            }
            Node::Group(kind, body) => {
                self.out.push_str(match kind {
                    GroupKind::Capture => "(",
                    GroupKind::NonCapture => "(?:",
                    GroupKind::Atomic => "(?>",
                    GroupKind::Ahead(false) => "(?=",
                    GroupKind::Ahead(true) => "(?!",
                    GroupKind::Behind(false) => "(?<=",
                    GroupKind::Behind(true) => "(?<!",
                });
                let lookaround = matches!(kind, GroupKind::Ahead(_) | GroupKind::Behind(_));
                self.node(
                    body,
                    Ctx {
                        tail: ctx.tail && !lookaround && !matches!(kind, GroupKind::Atomic),
                        ..ctx
                    },
                );
                self.out.push(')');
            }
            Node::Repeat { body, max: Some(0), .. } => {
                // Keep group numbering and nonparticipation without making the backend analyze a dead variable body.
                if has_capture(body) {
                    self.out.push_str("(?:(?!)");
                    self.zero_captures(body);
                    self.out.push_str("|)");
                }
            }
            // An empty body repeated is still empty (and fancy-regex refuses to repeat an empty group).
            Node::Repeat { body, .. } if only_empty(body) => {}
            Node::Repeat { body, min, max, mode } => {
                // Java runs a quantifier over a deterministic body (Curly, GroupCurly, Ques) without letting
                // the body backtrack into an earlier choice; only \R has such a choice (\r\n or \r).
                self.out.push_str("(?:");
                self.node(
                    body,
                    Ctx {
                        atomic_iter: tree_info(body).deterministic
                            && !(matches!(&**body, Node::Group(GroupKind::Capture | GroupKind::NonCapture, _))
                                && *min == 0
                                && *max == Some(1)
                                && *mode != Mode::Possessive),
                        tail: false,
                    },
                );
                // Java (like PCRE2) runs one more, empty, iteration of a body that can match empty and records its
                // captures; the regex-automata engine fancy-regex delegates plain loops to skips it. An empty
                // lookahead keeps the loop in fancy-regex's own backtracking VM, which iterates as Java does.
                // `matches` never reads groups, so the full-match pattern keeps the faster delegate.
                if self.d == Dialect::Fancy
                    && !self.full
                    && max.is_none_or(|m| m > 1)
                    && nullable(body)
                    && has_capture(body)
                {
                    self.out.push_str("(?=)");
                }
                self.out.push(')');
                match (min, max) {
                    (0, None) => self.out.push('*'),
                    (1, None) => self.out.push('+'),
                    (0, Some(1)) => self.out.push('?'),
                    (n, None) => {
                        let _ = write!(self.out, "{{{n},}}");
                    }
                    (n, Some(m)) if n == m => {
                        let _ = write!(self.out, "{{{n}}}");
                    }
                    (n, Some(m)) => {
                        let _ = write!(self.out, "{{{n},{m}}}");
                    }
                }
                match mode {
                    Mode::Greedy => {}
                    Mode::Lazy => self.out.push('?'),
                    Mode::Possessive => self.out.push('+'),
                }
            }
            Node::Grapheme => unreachable!("unsupported node rejected after syntax validation"),
            Node::Backref(k) => {
                if *k <= self.groups {
                    let _ = write!(self.out, r"(?:\{k})");
                } else {
                    // Java compiles a reference to a group that does not exist; it never matches.
                    self.out.push_str(r"[^\x{0}-\x{10FFFF}]");
                }
            }
            Node::Linebreak => {
                let body = r"\x{D}\x{A}|[\x{A}-\x{D}\x{85}\x{2028}\x{2029}]";
                if ctx.atomic_iter {
                    let _ = write!(self.out, "(?>{body})");
                } else {
                    let _ = write!(self.out, "(?:{body})");
                }
            }
            // In the full-match pattern `$` and `\Z` in tail position stand right before the wrapper's `\z`, where
            // they always hold; dropping them keeps the pattern free of lookarounds, so fancy-regex can delegate it
            // to its linear-time engine (as Java answers e.g. `(a|aa)+$` quickly, through its loop memoisation).
            Node::Assert(Assertion::Dollar(..)) if self.full && ctx.tail => {}
            Node::Assert(a) => self.assertion(*a),
        }
    }

    fn zero_captures(&mut self, n: &Node) {
        match n {
            Node::Seq(v) | Node::Alt(v) => {
                for n in v {
                    self.zero_captures(n);
                }
            }
            Node::Group(kind, b) => {
                if *kind == GroupKind::Capture {
                    self.out.push('(');
                }
                self.zero_captures(b);
                if *kind == GroupKind::Capture {
                    self.out.push(')');
                }
            }
            Node::Repeat { body, .. } => self.zero_captures(body),
            _ => {}
        }
    }

    fn negative_dollar(&mut self, body: &Node) {
        let Node::Seq(nodes) = body else { unreachable!() };
        let Some(Node::Assert(Assertion::Dollar(false, unix))) = nodes.last() else {
            unreachable!()
        };
        let prefix = Node::Seq(nodes[..nodes.len() - 1].to_vec());
        let ctx = Ctx {
            atomic_iter: false,
            tail: false,
        };
        self.out.push_str("(?!");
        self.node(&prefix, ctx);
        self.out.push_str(if *unix {
            r"(?:\x{A})?\z"
        } else {
            r"(?:\x{D}\x{A}|[\x{D}\x{85}\x{2028}\x{2029}])?\z"
        });
        if !unix {
            // A lone final LF is allowed only if it is not the second half of CRLF. Test that before the
            // prefix, so each prefix+terminator stays regular and the engine can delegate expensive loops.
            // The second guard alternative also covers a search starting at the LF inside final CRLF.
            self.out
                .push_str(r"|(?![\x{0}-\x{10FFFF}]*\x{D}\x{A}\z|(?<=\x{D})\x{A}\z)");
            self.node(&prefix, ctx);
            self.out.push_str(r"\x{A}\z");
        }
        self.out.push(')');
    }

    fn assertion(&mut self, a: Assertion) {
        let s: String = match a {
            Assertion::Begin => r"\A".into(),
            Assertion::End => r"\z".into(),
            Assertion::LastMatch => r"\G".into(),
            // Pattern.Dollar: at the end, or before a final terminator (\r\n counts as one); with MULTILINE before
            // any terminator, but never between \r and \n.
            Assertion::Dollar(false, false) => {
                "(?:\\z|(?=\\x{D}\\x{A}\\z)|(?=[\\x{D}\\x{85}\\x{2028}\\x{2029}]\\z)|(?<!\\x{D})(?=\\x{A}\\z))"
                    .to_string()
            }
            Assertion::Dollar(true, false) => r"(?:\z|(?=[\x{D}\x{85}\x{2028}\x{2029}])|(?<!\x{D})(?=\x{A}))".into(),
            Assertion::Dollar(false, true) => r"(?:\z|(?=\x{A}\z))".into(),
            Assertion::Dollar(true, true) => r"(?:\z|(?=\x{A}))".into(),
            // Pattern.Caret: never at the very end; at the start or after a terminator, but not between \r and \n.
            Assertion::Caret(false) => {
                format!(r"(?:(?=[\x{{0}}-\x{{10FFFF}}])(?:\A|(?<={TERMINATORS}))(?:(?<!\x{{D}})|(?!\x{{A}})))")
            }
            Assertion::Caret(true) => r"(?:(?=[\x{0}-\x{10FFFF}])(?:\A|(?<=\x{A})))".into(),
            Assertion::Bound(neg, uc) => {
                // Pattern.Bound: a side is a word char, or a non-spacing mark whose run of marks follows a letter or
                // digit (Character.isLetterOrDigit, Unicode even without (?U)). PCRE2 lookbehind must be bounded,
                // hence 254 marks at most there.
                let w = if uc {
                    self.set(&super::props::word())
                } else {
                    r"[0-9A-Z_a-z]".to_string()
                };
                let (plus, star) = match self.d {
                    Dialect::Fancy => ("+", "*"),
                    Dialect::Pcre2 => ("{1,254}", "{0,254}"),
                };
                let letters = self.set(&super::CSet::Union(vec![
                    super::props::property("L"),
                    super::props::property("Nd"),
                ]));
                let marks = self.set(&super::props::property("Mn"));
                let left = format!(r"(?:(?<={w})|(?<={letters}{marks}{plus}))");
                let right = format!(r"(?:(?={w})|(?={marks})(?<={letters}{marks}{star}))");
                if neg {
                    format!("(?:{left}{right}|(?!{left})(?!{right}))")
                } else {
                    format!("(?:{left}(?!{right})|(?!{left}){right})")
                }
            }
        };
        self.out.push_str(&s);
    }

    /// A one-code-point matcher for `s`.
    fn set(&self, s: &CSet) -> String {
        match self.d {
            // regex-syntax classes nest, intersect (&&) and negate directly.
            Dialect::Fancy => format!("[{}]", self.fancy_items(s)),
            Dialect::Pcre2 => self.pcre_set(s),
        }
    }

    fn fancy_items(&self, s: &CSet) -> String {
        let mut o = String::new();
        match s {
            CSet::Ranges(v) if v.is_empty() => o.push_str(r"[^\x{0}-\x{10FFFF}]"),
            CSet::Ranges(v) => ranges(&mut o, v),
            CSet::Union(v) if v.is_empty() => o.push_str(r"[^\x{0}-\x{10FFFF}]"),
            CSet::Union(v) => {
                for x in v {
                    let _ = write!(o, "[{}]", self.fancy_items(x));
                }
            }
            CSet::Inter(a, b) => {
                let _ = write!(o, "[{}]&&[{}]", self.fancy_items(a), self.fancy_items(b));
            }
            CSet::Not(a) => {
                let _ = write!(o, "[^{}]", self.fancy_items(a));
            }
        }
        o
    }

    /// PCRE2 classes do not nest: unions of plain items stay one class, the rest becomes lookarounds.
    fn pcre_set(&self, s: &CSet) -> String {
        if let Some(items) = pcre_plain(s) {
            return if items.is_empty() {
                r"[^\x{0}-\x{10FFFF}]".into()
            } else {
                format!("[{items}]")
            };
        }
        match s {
            CSet::Not(a) => {
                if let Some(items) = pcre_plain(a) {
                    return if items.is_empty() {
                        ANY.into()
                    } else {
                        format!("[^{items}]")
                    };
                }
                format!("(?:(?!{}){ANY})", self.pcre_set(a))
            }
            CSet::Union(v) => {
                let parts: Vec<String> = v.iter().map(|x| self.pcre_set(x)).collect();
                if parts.is_empty() {
                    r"[^\x{0}-\x{10FFFF}]".into()
                } else {
                    format!("(?:{})", parts.join("|"))
                }
            }
            CSet::Inter(a, b) => format!("(?:(?={}){})", self.pcre_set(a), self.pcre_set(b)),
            _ => unreachable!("plain sets are handled above"),
        }
    }
}

/// The items of a PCRE2 class for `s`, if `s` is a union of ranges and properties.
fn pcre_plain(s: &CSet) -> Option<String> {
    let mut o = String::new();
    match s {
        CSet::Ranges(v) => ranges(&mut o, v),
        CSet::Union(v) => {
            for x in v {
                o.push_str(&pcre_plain(x)?);
            }
        }
        CSet::Not(_) => return None,
        CSet::Inter(..) => return None,
    }
    Some(o)
}

fn ranges(o: &mut String, v: &[(u32, u32)]) {
    for &(lo, hi) in v {
        hex(o, lo);
        if hi != lo {
            o.push('-');
            hex(o, hi);
        }
    }
}

fn nullable(n: &Node) -> bool {
    match n {
        Node::Set(_) | Node::Linebreak | Node::Grapheme => false,
        Node::Seq(v) => v.iter().all(nullable),
        Node::Alt(v) => v.iter().any(nullable),
        Node::Group(GroupKind::Ahead(_) | GroupKind::Behind(_), _) => true,
        Node::Group(_, b) => nullable(b),
        Node::Repeat { body, min, .. } => *min == 0 || nullable(body),
        Node::Backref(_) | Node::Assert(_) => true,
    }
}

/// Removing these bodies cannot erase captures or assertions that affect matching.
fn only_empty(n: &Node) -> bool {
    match n {
        Node::Seq(v) | Node::Alt(v) => v.iter().all(only_empty),
        Node::Group(GroupKind::NonCapture | GroupKind::Atomic, b) => only_empty(b),
        Node::Repeat { body, .. } => only_empty(body),
        _ => false,
    }
}

/// Only regular bodies with an exposed terminal dollar qualify. A capture around the dollar would expose the consumed
/// terminator, and atomic choices or other assertions can depend on the original zero-width assertion.
fn regular_dollar_tail(n: &Node) -> bool {
    fn plain(n: &Node) -> bool {
        match n {
            Node::Set(_) => true,
            Node::Seq(v) | Node::Alt(v) => v.iter().all(plain),
            Node::Group(GroupKind::Capture | GroupKind::NonCapture, b) => plain(b),
            Node::Repeat { body, mode, .. } => *mode != Mode::Possessive && plain(body),
            _ => false,
        }
    }
    match n {
        Node::Seq(v) => {
            v.last()
                .is_some_and(|n| matches!(n, Node::Assert(Assertion::Dollar(false, _))))
                && v[..v.len() - 1].iter().all(plain)
        }
        _ => false,
    }
}
