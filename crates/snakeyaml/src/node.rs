//! The `Node` tree: YAML text as SnakeYAML 2.2's event parser sees it, before any Jackson typing.
//!
//! The tree keeps everything the Jackson layer (`crate::tokens`) and the configuration binder need: the raw scalar
//! text and style, the tag in SnakeYAML's spelling, anchor and alias names, every mapping entry in source order
//! (duplicates included), and source positions. A stream that breaks (scanner error, a SnakeYAML reader limit) keeps
//! everything read before the break and marks the break with [`NodeKind::Cut`], because Jackson binds lazily: an error
//! further on is only seen if binding gets that far.

use crate::java_string::JavaString;
use crate::snakeyaml::parser::Parser;
use crate::snakeyaml::{Event, EventKind};

/// SnakeYAML 2.2 `LoaderOptions.codePointLimit` default (3 MiB), enforced by its scanner on the Jackson path.
pub const CODE_POINT_LIMIT: usize = 3 * 1024 * 1024;

/// A position in the input. `index` counts code points from the start of the input (a leading BOM included);
/// `line` and `column` are 1-based, as Jackson and SnakeYAML print them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mark {
    pub index: usize,
    pub line: usize,
    pub column: usize,
}

/// The source range of a node: for collections, from the start event to the end event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub start: Mark,
    pub end: Mark,
}

/// How a scalar was written. Jackson ignores it except for one thing: only plain scalars are typed implicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    SingleQuoted,
    DoubleQuoted,
    Literal,
    Folded,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: NodeKind,
    pub opening: Span,
    pub closing: Option<Span>,
    /// The anchor name (`&name`), if any.
    pub anchor: Option<String>,
    /// The tag as SnakeYAML reports it: the resolved handle followed by the suffix (`tag:yaml.org,2002:str`,
    /// `!custom`, `!` for the non-specific tag).
    pub tag: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Scalar {
        text: JavaString,
        style: Style,
    },
    /// `complete` is false when the stream broke inside the sequence, after the last item.
    Seq {
        items: Vec<Node>,
        complete: bool,
    },
    /// Entries in source order, duplicates kept. `complete` is false when the stream broke after the last entry.
    Map {
        entries: Vec<(Node, Node)>,
        complete: bool,
    },
    /// `*name`. Never resolved: Jackson reports an alias as its name, defined or not.
    Alias {
        name: String,
    },
    /// The stream broke here; see [`Stream::error`].
    Cut,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// Whether the document started with `---`.
    pub explicit_start: bool,
    /// The root node: an empty document has an empty plain scalar, which Jackson reads as null.
    pub root: Node,
}

/// Every document of the input, and the error that ended the stream early, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub documents: Vec<Document>,
    pub error: Option<YamlError>,
}

/// The stage (and Jackson exception class) of a stream error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YamlErrorKind {
    /// Scanner or parser error: Jackson's `MarkedYAMLException`.
    Syntax,
    Depth,
    Generic,
    /// NumberFormatException escapes SnakeYAML and uses Jackson's numeric catch.
    NumberFormat,
    /// A non-printable character: `JacksonYAMLParseException` ("special characters are not allowed").
    SpecialCharacter,
    /// More than [`CODE_POINT_LIMIT`] code points: `JacksonYAMLParseException`.
    CodePointLimit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct YamlError {
    pub stage: &'static str,
    pub context: Option<Mark>,
    pub kind: YamlErrorKind,
    /// Our own wording (PLAN §3.10 does not reproduce Jackson's).
    pub message: String,
    /// Where the scanner or parser found the problem (SnakeYAML's problem mark), when there is one.
    pub problem: Option<Mark>,
    /// Jackson's location: the end of the last event read before the error.
    pub location: Mark,
}

/// Kotlin File.readText() boundary: JDK UTF-8 replacement before YAML parsing.
pub fn load_bytes(bytes: &[u8]) -> Stream {
    load(&jcompat::utf8::decode_utf8(bytes))
}

/// Read events lazily, limiting construction before a hostile tree exists.
pub fn load(text: &str) -> Stream {
    let mut parser = Parser::new(text);
    let mut events = Vec::new();
    let mut depth = 0usize;
    let mut location = Mark {
        index: 0,
        line: 1,
        column: 1,
    };
    let mut error = None;
    loop {
        match parser.next_event() {
            Ok(Some(event)) => {
                match event.kind {
                    EventKind::SequenceStart | EventKind::MappingStart => {
                        depth += 1;
                        if depth > crate::tokens::MAX_DEPTH {
                            error = Some(YamlError {
                                kind: YamlErrorKind::Depth,
                                stage: "token",
                                context: None,
                                problem: None,
                                location,
                                message: "nesting depth 1001 exceeds the maximum of 1000".into(),
                            });
                            break;
                        }
                    }
                    EventKind::SequenceEnd | EventKind::MappingEnd => depth = depth.saturating_sub(1),
                    _ => {}
                }
                location = event.span.end;
                let end = matches!(event.kind, EventKind::StreamEnd);
                events.push(event);
                if end {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                error = Some(YamlError {
                    kind: e.kind,
                    stage: e.stage,
                    message: e.message,
                    context: e.context,
                    problem: e.problem,
                    location,
                });
                break;
            }
        }
    }
    let documents = build(events, error.is_some());
    Stream { documents, error }
}

/// SnakeYAML 2.2 `StreamReader.isPrintable`.
pub fn is_printable(c: char) -> bool {
    let c = c as u32;
    (0x20..=0x7E).contains(&c)
        || c == 0x9
        || c == 0xA
        || c == 0xD
        || c == 0x85
        || (0xA0..=0xD7FF).contains(&c)
        || (0xE000..=0xFFFD).contains(&c)
        || (0x10000..=0x10FFFF).contains(&c)
}

/// A collection under construction.
enum Open {
    Seq(Node, Vec<Node>),
    Map(Node, Vec<(Node, Node)>, Option<Box<Node>>),
}

fn build(events: Vec<Event>, broken: bool) -> Vec<Document> {
    let mut documents = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    let mut doc: Option<(bool, Span)> = None;
    let mut root: Option<Node> = None;
    let mut consumed_end = Mark::default();
    for ev in events {
        consumed_end = ev.span.end;
        let span = ev.span;
        let anchor = ev.anchor;
        let tag = ev.tag;
        let ev = ev.kind;
        let node = match ev {
            EventKind::StreamStart | EventKind::StreamEnd => continue,
            EventKind::DocumentStart(explicit) => {
                doc = Some((explicit, span));
                continue;
            }
            EventKind::DocumentEnd => {
                if let (Some((explicit, _)), Some(r)) = (doc.take(), root.take()) {
                    documents.push(Document {
                        explicit_start: explicit,
                        root: r,
                    });
                }
                continue;
            }
            EventKind::Alias(name) => Some(Node {
                opening: span,
                closing: None,
                kind: NodeKind::Alias { name },
                anchor: None,
                tag: None,
                span,
            }),
            EventKind::Scalar(text, style) => Some(Node {
                opening: span,
                closing: None,
                kind: NodeKind::Scalar { text, style },
                anchor,
                tag,
                span,
            }),
            EventKind::SequenceStart => {
                let header = header(anchor, tag, span);
                stack.push(Open::Seq(header, Vec::new()));
                None
            }
            EventKind::MappingStart => {
                let header = header(anchor, tag, span);
                stack.push(Open::Map(header, Vec::new(), None));
                None
            }
            EventKind::SequenceEnd | EventKind::MappingEnd => stack.pop().map(|open| close(open, span, true)),
        };
        if let Some(node) = node {
            attach(&mut stack, &mut root, node);
        }
    }
    if broken {
        // Close everything still open. The innermost collection takes the break: as the value of a key read
        // just before it, or as its own `complete: false`.
        let mut pending = cut_node();
        while let Some(mut open) = stack.pop() {
            let is_cut = matches!(pending.kind, NodeKind::Cut);
            match &mut open {
                Open::Seq(_, items) => {
                    if !is_cut {
                        items.push(pending);
                    }
                }
                Open::Map(_, entries, key) => match key.take() {
                    Some(k) => entries.push((*k, pending)),
                    // A collection key cut short: its value was never read.
                    None if !is_cut => entries.push((pending, cut_node())),
                    None => {}
                },
            }
            let end = consumed_end;
            pending = close(open, Span { start: end, end }, false);
        }
        let pending = Some(pending);
        if root.is_none() {
            root = pending;
        }
        if doc.is_some() || root.is_some() {
            let explicit = doc.map(|(e, _)| e).unwrap_or(false);
            if let Some(r) = root.take() {
                documents.push(Document {
                    explicit_start: explicit,
                    root: r,
                });
            }
        }
    }
    documents
}

fn cut_node() -> Node {
    Node {
        opening: Span::default(),
        closing: None,
        kind: NodeKind::Cut,
        anchor: None,
        tag: None,
        span: Span::default(),
    }
}

fn header(anchor: Option<String>, tag: Option<String>, span: Span) -> Node {
    Node {
        opening: span,
        closing: None,
        kind: NodeKind::Cut,
        anchor,
        tag,
        span,
    }
}

fn close(open: Open, span: Span, complete: bool) -> Node {
    match open {
        Open::Seq(mut h, items) => {
            h.kind = NodeKind::Seq { items, complete };
            h.span.end = span.end;
            h.closing = complete.then_some(span);
            h
        }
        Open::Map(mut h, entries, pending_key) => {
            // A key without a value only survives a break, and then the value is the Cut.
            debug_assert!(pending_key.is_none() || !complete);
            h.kind = NodeKind::Map { entries, complete };
            h.span.end = span.end;
            h.closing = complete.then_some(span);
            h
        }
    }
}

fn attach(stack: &mut [Open], root: &mut Option<Node>, node: Node) {
    match stack.last_mut() {
        None => *root = Some(node),
        Some(Open::Seq(_, items)) => items.push(node),
        Some(Open::Map(_, entries, key)) => match key.take() {
            None => *key = Some(Box::new(node)),
            Some(k) => entries.push((*k, node)),
        },
    }
}

// Draining children before dropping each Node bounds destructor stack usage,
// including trees assembled directly by callers rather than by load().
impl Drop for Node {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        fn drain(kind: &mut NodeKind, pending: &mut Vec<Node>) {
            match std::mem::replace(kind, NodeKind::Cut) {
                NodeKind::Seq { items, .. } => pending.extend(items),
                NodeKind::Map { entries, .. } => {
                    for (k, v) in entries {
                        pending.push(k);
                        pending.push(v);
                    }
                }
                _ => {}
            }
        }
        drain(&mut self.kind, &mut pending);
        while let Some(mut n) = pending.pop() {
            drain(&mut n.kind, &mut pending);
        }
    }
}
