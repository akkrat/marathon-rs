//! Port of SnakeYAML 2.2 org/yaml/snakeyaml/{error,events,tokens,scanner/Constant}.java.
// Copyright (c) 2008, SnakeYAML. Licensed under Apache-2.0; see LICENSE.
pub mod parser;
pub mod reader;
pub mod scanner;

use crate::node::{Mark, Span, Style};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug)]
pub struct Error {
    pub kind: crate::node::YamlErrorKind,
    pub stage: &'static str,
    pub message: String,
    pub context: Option<Mark>,
    pub problem: Option<Mark>,
}
impl Error {
    pub fn marked(stage: &'static str, context: Option<Mark>, message: &str, problem: Mark) -> Self {
        Self {
            kind: crate::node::YamlErrorKind::Syntax,
            stage,
            context,
            message: message.into(),
            problem: Some(problem),
        }
    }
    pub fn plain(kind: crate::node::YamlErrorKind, message: &str) -> Self {
        Self {
            kind,
            stage: if kind == crate::node::YamlErrorKind::SpecialCharacter {
                "reader"
            } else {
                "scanner"
            },
            message: message.into(),
            context: None,
            problem: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Id {
    StreamStart,
    StreamEnd,
    Directive,
    DocumentStart,
    DocumentEnd,
    BlockSequenceStart,
    BlockMappingStart,
    BlockEnd,
    FlowSequenceStart,
    FlowMappingStart,
    FlowSequenceEnd,
    FlowMappingEnd,
    BlockEntry,
    FlowEntry,
    Key,
    Value,
    Alias,
    Anchor,
    Tag,
    Scalar,
}
#[derive(Clone, Debug)]
pub enum Data {
    None,
    Text(String),
    Scalar(String, Style),
    Tag(Option<String>, String),
    Directive(String, Vec<String>),
}
#[derive(Clone, Debug)]
pub struct Token {
    pub id: Id,
    pub data: Data,
    pub span: Span,
}
impl Token {
    pub fn new(id: Id, start: Mark, end: Mark) -> Self {
        Self {
            id,
            data: Data::None,
            span: Span { start, end },
        }
    }
    pub fn text(&self) -> String {
        match &self.data {
            Data::Text(s) => s.clone(),
            _ => String::new(),
        }
    }
}
#[derive(Clone, Debug)]
pub enum EventKind {
    StreamStart,
    StreamEnd,
    DocumentStart(bool),
    DocumentEnd,
    Scalar(String, Style),
    Alias(String),
    SequenceStart,
    MappingStart,
    SequenceEnd,
    MappingEnd,
}
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: EventKind,
    pub span: Span,
    pub anchor: Option<String>,
    pub tag: Option<String>,
}
impl Event {
    pub fn new(kind: EventKind, span: Span) -> Self {
        Self {
            kind,
            span,
            anchor: None,
            tag: None,
        }
    }
}
pub fn has(s: &str, c: u32) -> bool {
    char::from_u32(c).is_some_and(|c| s.contains(c))
}
pub const LINEBR: &str = "\n\u{85}\u{2028}\u{2029}";
pub const NULL_OR_LINEBR: &str = "\0\r\n\u{85}\u{2028}\u{2029}";
pub const NULL_BL_LINEBR: &str = " \0\r\n\u{85}\u{2028}\u{2029}";
pub const NULL_BL_T_LINEBR: &str = "\t \0\r\n\u{85}\u{2028}\u{2029}";
pub const ALPHA: &str = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-_";
pub const URI_CHARS: &str = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-_-;/?:@&=+$,_.!~*'()[]%";
