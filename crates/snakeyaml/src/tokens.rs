//! The Jackson view of a [`Stream`]: the `JsonToken` sequence `YAMLParser.nextToken` (jackson-dataformat-yaml 2.17)
//! returns, with `getObjectId` (anchor) and `getTypeId` (tag) after each token.
//!
//! The rules that are Jackson's rather than SnakeYAML's live here and in [`check_key`] / [`check_depth`], which the
//! binder shares: an alias or a collection in key position is an error, nesting deeper than 1000 is an error, aliases
//! read as their name, and documents are concatenated.

use crate::node::{Mark, Node, NodeKind, Stream, YamlError, YamlErrorKind};
use crate::resolve::{Decoded, JInt, NumberError, ScalarToken, decode_scalar};

/// jackson-core `StreamReadConstraints.DEFAULT_MAX_DEPTH`.
pub const MAX_DEPTH: usize = 1000;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    StartObject,
    EndObject,
    StartArray,
    EndArray,
    Field(String),
    Str(String),
    Int {
        text: String,
        value: Result<JInt, NumberError>,
    },
    Float {
        text: String,
        cleaned: String,
    },
    True(String),
    False(String),
    Null(String),
    Embedded(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub span: crate::node::Span,
    pub java_text: Option<String>,
    pub tok: Tok,
    pub anchor: Option<String>,
    /// `getTypeId()`: the tag with every leading `!` removed.
    pub tag: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenErrorKind {
    Fixture,
    MarkedYaml,
    YamlParse,
    JsonParse,
    StreamConstraints,
    JsonEof,
    MismatchedInput,
}

/// An error `nextToken` throws.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenError {
    pub stage: &'static str,
    pub context: Option<Mark>,
    /// The Jackson exception class, for comparison with the oracle.
    pub kind: TokenErrorKind,
    pub message: String,
    pub problem: Option<Mark>,
    pub location: Option<Mark>,
}

pub const MARKED_YAML_EXCEPTION: TokenErrorKind = TokenErrorKind::MarkedYaml;
pub const JACKSON_YAML_PARSE_EXCEPTION: TokenErrorKind = TokenErrorKind::YamlParse;

impl From<&YamlError> for TokenError {
    fn from(e: &YamlError) -> Self {
        TokenError {
            stage: e.stage,
            kind: match e.kind {
                YamlErrorKind::Syntax => MARKED_YAML_EXCEPTION,
                YamlErrorKind::Depth => crate::resolve::STREAM_CONSTRAINTS_EXCEPTION,
                YamlErrorKind::NumberFormat => crate::resolve::JSON_PARSE_EXCEPTION,
                YamlErrorKind::Generic => JACKSON_YAML_PARSE_EXCEPTION,
                YamlErrorKind::SpecialCharacter | YamlErrorKind::CodePointLimit => JACKSON_YAML_PARSE_EXCEPTION,
            },
            message: e.message.to_owned(),
            context: e.context,
            problem: e.problem,
            location: if e.kind == YamlErrorKind::Depth {
                None
            } else {
                Some(e.location)
            },
        }
    }
}

/// The field name of a mapping key, or the error Jackson raises for anything but a scalar there.
pub fn check_key(key: &Node) -> Result<&str, KeyProblem> {
    match &key.kind {
        NodeKind::Scalar { text, .. } => Ok(text),
        NodeKind::Cut => Err(KeyProblem::Cut),
        _ => Err(KeyProblem::NotScalar(Box::new(TokenError {
            stage: "token",
            context: None,
            kind: crate::resolve::JSON_PARSE_EXCEPTION,
            message: "expected a field name (a scalar), found an alias or a collection".to_owned(),
            problem: None,
            location: Some(key_location(key)),
        }))),
    }
}

/// Jackson's location for a bad key: the end of the event it read (an alias's end; a collection's start event).
fn key_location(key: &Node) -> Mark {
    key.opening.end
}

pub enum KeyProblem {
    Cut,
    NotScalar(Box<TokenError>),
}

/// `StreamReadConstraints.validateNestingDepth` for a collection opened at `depth` (the root object is depth 1).
pub fn check_depth(depth: usize) -> Result<(), Box<TokenError>> {
    if depth > MAX_DEPTH {
        return Err(Box::new(TokenError {
            stage: "token",
            context: None,
            kind: crate::resolve::STREAM_CONSTRAINTS_EXCEPTION,
            message: format!("nesting depth {depth} exceeds the maximum of {MAX_DEPTH}"),
            problem: None,
            location: None,
        }));
    }
    Ok(())
}

/// Strips every leading `!`, as `YAMLParser.getTypeId` does.
fn type_id(tag: Option<&str>) -> Option<String> {
    tag.map(|t| t.trim_start_matches('!').to_owned())
}

/// The token stream and the error that ended it, if any.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Tokens {
    pub tokens: Vec<Token>,
    pub error: Option<TokenError>,
}

pub fn tokens(stream: &Stream) -> Tokens {
    walk(
        stream.documents.iter().map(|doc| &doc.root),
        stream.error.as_ref(),
        true,
    )
}
pub fn subtree(root: &Node, error: Option<&YamlError>) -> Tokens {
    walk(std::iter::once(root), error, false)
}
fn walk<'a>(
    roots: impl DoubleEndedIterator<Item = &'a Node>,
    error: Option<&YamlError>,
    trailing_error: bool,
) -> Tokens {
    enum Work<'a> {
        Value(&'a Node, usize),
        Field(&'a Node, Option<String>, Option<String>),
        End(&'a Node, bool),
    }
    let mut out = Tokens::default();
    let mut work = Vec::new();
    for root in roots.rev() {
        work.push(Work::Value(root, 0));
    }
    let stream_error = || {
        error.map(TokenError::from).unwrap_or_else(|| TokenError {
            stage: "token",
            context: None,
            kind: MARKED_YAML_EXCEPTION,
            message: "stream ended early".into(),
            problem: None,
            location: None,
        })
    };
    let mut ended = false;
    while let Some(w) = work.pop() {
        let mut push = |tok, anchor, tag, span, java_text| {
            out.tokens.push(Token {
                tok,
                anchor,
                tag,
                span,
                java_text,
            })
        };
        match w {
            Work::Value(n, depth) => {
                let tag = type_id(n.tag.as_deref());
                match &n.kind {
                    NodeKind::Cut => {
                        out.error = Some(stream_error());
                        break;
                    }
                    NodeKind::Alias { name } => {
                        push(Tok::Str(name.clone()), None, None, n.span, Some(name.as_str().into()))
                    }
                    NodeKind::Scalar { text, style } => {
                        let projected = text.to_owned();
                        let tok = match decode_scalar(&projected, *style, n.tag.as_deref()) {
                            Decoded::Token(t) => scalar_tok(&projected, t),
                            Decoded::EndOfInput => {
                                ended = true;
                                break;
                            }
                            Decoded::Error(class, message) => {
                                out.error = Some(TokenError {
                                    stage: "token",
                                    context: None,
                                    kind: class,
                                    message,
                                    problem: None,
                                    location: Some(n.span.end),
                                });
                                break;
                            }
                        };
                        let tok = if matches!(tok, Tok::Str(_)) {
                            Tok::Str(text.clone())
                        } else {
                            tok
                        };
                        push(tok, None, tag, n.span, Some(text.clone()));
                    }
                    NodeKind::Seq { items, .. } => {
                        if let Err(e) = check_depth(depth + 1) {
                            out.error = Some(*e);
                            break;
                        }
                        push(Tok::StartArray, n.anchor.clone(), tag, n.opening, None);
                        work.push(Work::End(n, false));
                        for item in items.iter().rev() {
                            work.push(Work::Value(item, depth + 1));
                        }
                    }
                    NodeKind::Map { entries, .. } => {
                        if let Err(e) = check_depth(depth + 1) {
                            out.error = Some(*e);
                            break;
                        }
                        push(Tok::StartObject, n.anchor.clone(), tag.clone(), n.opening, None);
                        work.push(Work::End(n, true));
                        for (i, (key, value)) in entries.iter().enumerate().rev() {
                            work.push(Work::Value(value, depth + 1));
                            let (anchor, key_tag) = if i == 0 {
                                (key.anchor.clone().or_else(|| n.anchor.clone()), tag.clone())
                            } else {
                                (key.anchor.clone(), type_id(key.tag.as_deref()))
                            };
                            work.push(Work::Field(key, anchor, key_tag));
                        }
                    }
                }
            }
            Work::Field(key, anchor, tag) => match check_key(key) {
                Ok(text) => push(
                    Tok::Field(text.to_string()),
                    anchor,
                    tag,
                    key.span,
                    Some(text.to_string()),
                ),
                Err(KeyProblem::Cut) => {
                    out.error = Some(stream_error());
                    break;
                }
                Err(KeyProblem::NotScalar(e)) => {
                    out.error = Some(*e);
                    break;
                }
            },
            Work::End(n, map) => {
                let complete = match &n.kind {
                    NodeKind::Map { complete, .. } | NodeKind::Seq { complete, .. } => *complete,
                    _ => true,
                };
                if !complete {
                    out.error = Some(stream_error());
                    break;
                }
                push(
                    if map { Tok::EndObject } else { Tok::EndArray },
                    None,
                    None,
                    n.closing.unwrap_or(crate::node::Span {
                        start: n.span.end,
                        end: n.span.end,
                    }),
                    None,
                );
            }
        }
    }
    if trailing_error && out.error.is_none() && !ended {
        out.error = error.map(TokenError::from);
    }
    out
}

fn scalar_tok(text: &str, t: ScalarToken) -> Tok {
    match t {
        ScalarToken::Null => Tok::Null(text.to_owned()),
        ScalarToken::True => Tok::True(text.to_owned()),
        ScalarToken::False => Tok::False(text.to_owned()),
        ScalarToken::Str => Tok::Str(text.into()),
        ScalarToken::Int(value) => Tok::Int {
            text: text.to_owned(),
            value,
        },
        ScalarToken::Float(cleaned) => Tok::Float {
            text: text.to_owned(),
            cleaned,
        },
        ScalarToken::Binary(bytes) => Tok::Embedded(bytes),
    }
}
