//! Port of SnakeYAML 2.2 org/yaml/snakeyaml/parser/ParserImpl.java.
// Copyright (c) 2008, SnakeYAML. Licensed under Apache-2.0; see LICENSE.
// Production objects become enum states. Tail calls use the dispatch loop;
// the scanner has processComments=false, as on Jackson's default path.
use super::scanner::Scanner;
use super::*;
use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug)]
enum State {
    StreamStart,
    ImplicitDocumentStart,
    DocumentStart,
    DocumentEnd,
    DocumentContent,
    BlockNode,
    BlockSequenceFirstEntry,
    BlockSequenceEntryKey,
    IndentlessSequenceEntryKey,
    BlockMappingFirstKey,
    BlockMappingKey,
    BlockMappingValue,
    FlowSequenceFirstEntry,
    FlowSequenceEntry(bool),
    FlowSequenceEntryMappingKey,
    FlowSequenceEntryMappingValue,
    FlowSequenceEntryMappingEnd,
    FlowMappingFirstKey,
    FlowMappingKey(bool),
    FlowMappingValue,
    FlowMappingEmptyValue,
    End,
}
pub struct Parser {
    scanner: Scanner,
    state: State,
    states: Vec<State>,
    marks: Vec<Mark>,
    tags: BTreeMap<String, String>,
}
impl Parser {
    pub fn new(text: &str) -> Self {
        Self {
            scanner: Scanner::new(text),
            state: State::StreamStart,
            states: Vec::new(),
            marks: Vec::new(),
            tags: BTreeMap::from([("!".into(), "!".into()), ("!!".into(), "tag:yaml.org,2002:".into())]),
        }
    }
    fn check(&mut self, ids: &[Id]) -> Result<bool> {
        self.scanner.check(ids)
    }
    fn peek(&mut self) -> Result<Token> {
        Ok(self.scanner.peek()?.clone())
    }
    fn take(&mut self) -> Token {
        self.scanner.take()
    }
    fn pop(&mut self) {
        self.state = self.states.pop().unwrap_or(State::End);
    }
    fn empty(&self, m: Mark) -> Event {
        Event::new(
            EventKind::Scalar(String::default(), Style::Plain),
            Span { start: m, end: m },
        )
    }
    fn error(&mut self, ctx: Option<Mark>, message: &str) -> Result<Event> {
        let t = self.peek()?;
        Err(Error::marked("parser", ctx, message, t.span.start))
    }
    pub fn next_event(&mut self) -> Result<Option<Event>> {
        loop {
            match self.state {
                State::End => return Ok(None),
                State::StreamStart => {
                    let t = self.take();
                    self.state = State::ImplicitDocumentStart;
                    return Ok(Some(Event::new(EventKind::StreamStart, t.span)));
                }
                State::ImplicitDocumentStart => {
                    if !self.check(&[Id::Directive, Id::DocumentStart, Id::StreamEnd])? {
                        let t = self.peek()?;
                        self.states.push(State::DocumentEnd);
                        self.state = State::BlockNode;
                        return Ok(Some(Event::new(
                            EventKind::DocumentStart(false),
                            Span {
                                start: t.span.start,
                                end: t.span.start,
                            },
                        )));
                    }
                    self.state = State::DocumentStart;
                }
                State::DocumentStart => {
                    while self.check(&[Id::DocumentEnd])? {
                        self.take();
                    }
                    if !self.check(&[Id::StreamEnd])? {
                        self.scanner.reader.document_index = 0;
                        let start = self.peek()?.span.start;
                        self.process_directives()?;
                        if !self.check(&[Id::StreamEnd])? {
                            if !self.check(&[Id::DocumentStart])? {
                                return self.error(None, "expected document start").map(Some);
                            }
                            let t = self.take();
                            self.states.push(State::DocumentEnd);
                            self.state = State::DocumentContent;
                            return Ok(Some(Event::new(
                                EventKind::DocumentStart(true),
                                Span { start, end: t.span.end },
                            )));
                        }
                    }
                    let t = self.take();
                    self.state = State::End;
                    return Ok(Some(Event::new(EventKind::StreamEnd, t.span)));
                }
                State::DocumentEnd => {
                    let t = self.peek()?;
                    let mut end = t.span.start;
                    if self.check(&[Id::DocumentEnd])? {
                        end = self.take().span.end;
                    }
                    self.state = State::DocumentStart;
                    return Ok(Some(Event::new(
                        EventKind::DocumentEnd,
                        Span {
                            start: t.span.start,
                            end,
                        },
                    )));
                }
                State::DocumentContent => {
                    if self.check(&[Id::Directive, Id::DocumentStart, Id::DocumentEnd, Id::StreamEnd])? {
                        let m = self.peek()?.span.start;
                        self.pop();
                        return Ok(Some(self.empty(m)));
                    }
                    self.state = State::BlockNode;
                }
                State::BlockNode => return self.parse_node(true, false).map(Some),
                State::BlockSequenceFirstEntry => {
                    let t = self.take();
                    self.marks.push(t.span.start);
                    self.state = State::BlockSequenceEntryKey;
                }
                State::BlockSequenceEntryKey => {
                    if self.check(&[Id::BlockEntry])? {
                        let t = self.take();
                        if !self.check(&[Id::BlockEntry, Id::BlockEnd])? {
                            self.states.push(State::BlockSequenceEntryKey);
                            return self.parse_node(true, false).map(Some);
                        }
                        self.state = State::BlockSequenceEntryKey;
                        return Ok(Some(self.empty(t.span.end)));
                    }
                    if !self.check(&[Id::BlockEnd])? {
                        let ctx = self.marks.pop();
                        return self.error(ctx, "expected block end").map(Some);
                    }
                    let t = self.take();
                    self.pop();
                    self.marks.pop();
                    return Ok(Some(Event::new(EventKind::SequenceEnd, t.span)));
                }
                State::IndentlessSequenceEntryKey => {
                    if self.check(&[Id::BlockEntry])? {
                        let t = self.take();
                        if !self.check(&[Id::BlockEntry, Id::Key, Id::Value, Id::BlockEnd])? {
                            self.states.push(State::IndentlessSequenceEntryKey);
                            return self.parse_node(true, false).map(Some);
                        }
                        self.state = State::IndentlessSequenceEntryKey;
                        return Ok(Some(self.empty(t.span.end)));
                    }
                    let t = self.peek()?;
                    self.pop();
                    return Ok(Some(Event::new(EventKind::SequenceEnd, t.span)));
                }
                State::BlockMappingFirstKey => {
                    let t = self.take();
                    self.marks.push(t.span.start);
                    self.state = State::BlockMappingKey;
                }
                State::BlockMappingKey => {
                    if self.check(&[Id::Key])? {
                        let t = self.take();
                        if !self.check(&[Id::Key, Id::Value, Id::BlockEnd])? {
                            self.states.push(State::BlockMappingValue);
                            return self.parse_node(true, true).map(Some);
                        }
                        self.state = State::BlockMappingValue;
                        return Ok(Some(self.empty(t.span.end)));
                    }
                    if !self.check(&[Id::BlockEnd])? {
                        let ctx = self.marks.pop();
                        return self.error(ctx, "expected block end").map(Some);
                    }
                    let t = self.take();
                    self.pop();
                    self.marks.pop();
                    return Ok(Some(Event::new(EventKind::MappingEnd, t.span)));
                }
                State::BlockMappingValue => {
                    if self.check(&[Id::Value])? {
                        let t = self.take();
                        if !self.check(&[Id::Key, Id::Value, Id::BlockEnd])? {
                            self.states.push(State::BlockMappingKey);
                            return self.parse_node(true, true).map(Some);
                        }
                        self.state = State::BlockMappingKey;
                        return Ok(Some(self.empty(t.span.end)));
                    } else if self.check(&[Id::Scalar])? {
                        self.states.push(State::BlockMappingKey);
                        return self.parse_node(true, true).map(Some);
                    }
                    self.state = State::BlockMappingKey;
                    let m = self.peek()?.span.start;
                    return Ok(Some(self.empty(m)));
                }
                State::FlowSequenceFirstEntry => {
                    let t = self.take();
                    self.marks.push(t.span.start);
                    self.state = State::FlowSequenceEntry(true);
                }
                State::FlowSequenceEntry(first) => {
                    if !self.check(&[Id::FlowSequenceEnd])? {
                        if !first {
                            if self.check(&[Id::FlowEntry])? {
                                self.take();
                            } else {
                                let ctx = self.marks.pop();
                                return self.error(ctx, "expected comma or closing bracket").map(Some);
                            }
                        }
                        if self.check(&[Id::Key])? {
                            let t = self.peek()?;
                            self.state = State::FlowSequenceEntryMappingKey;
                            return Ok(Some(Event::new(EventKind::MappingStart, t.span)));
                        } else if !self.check(&[Id::FlowSequenceEnd])? {
                            self.states.push(State::FlowSequenceEntry(false));
                            return self.parse_node(false, false).map(Some);
                        }
                    }
                    let t = self.take();
                    self.check(&[])?;
                    self.pop();
                    self.marks.pop();
                    return Ok(Some(Event::new(EventKind::SequenceEnd, t.span)));
                }
                State::FlowSequenceEntryMappingKey => {
                    let t = self.take();
                    if !self.check(&[Id::Value, Id::FlowEntry, Id::FlowSequenceEnd])? {
                        self.states.push(State::FlowSequenceEntryMappingValue);
                        return self.parse_node(false, false).map(Some);
                    }
                    self.state = State::FlowSequenceEntryMappingValue;
                    return Ok(Some(self.empty(t.span.end)));
                }
                State::FlowSequenceEntryMappingValue => {
                    if self.check(&[Id::Value])? {
                        let t = self.take();
                        if !self.check(&[Id::FlowEntry, Id::FlowSequenceEnd])? {
                            self.states.push(State::FlowSequenceEntryMappingEnd);
                            return self.parse_node(false, false).map(Some);
                        }
                        self.state = State::FlowSequenceEntryMappingEnd;
                        return Ok(Some(self.empty(t.span.end)));
                    }
                    self.state = State::FlowSequenceEntryMappingEnd;
                    let m = self.peek()?.span.start;
                    return Ok(Some(self.empty(m)));
                }
                State::FlowSequenceEntryMappingEnd => {
                    self.state = State::FlowSequenceEntry(false);
                    let t = self.peek()?;
                    return Ok(Some(Event::new(EventKind::MappingEnd, t.span)));
                }
                State::FlowMappingFirstKey => {
                    let t = self.take();
                    self.marks.push(t.span.start);
                    self.state = State::FlowMappingKey(true);
                }
                State::FlowMappingKey(first) => {
                    if !self.check(&[Id::FlowMappingEnd])? {
                        if !first {
                            if self.check(&[Id::FlowEntry])? {
                                self.take();
                            } else {
                                let ctx = self.marks.pop();
                                return self.error(ctx, "expected comma or closing brace").map(Some);
                            }
                        }
                        if self.check(&[Id::Key])? {
                            let t = self.take();
                            if !self.check(&[Id::Value, Id::FlowEntry, Id::FlowMappingEnd])? {
                                self.states.push(State::FlowMappingValue);
                                return self.parse_node(false, false).map(Some);
                            }
                            self.state = State::FlowMappingValue;
                            return Ok(Some(self.empty(t.span.end)));
                        } else if !self.check(&[Id::FlowMappingEnd])? {
                            self.states.push(State::FlowMappingEmptyValue);
                            return self.parse_node(false, false).map(Some);
                        }
                    }
                    let t = self.take();
                    self.marks.pop();
                    self.check(&[])?;
                    self.pop();
                    return Ok(Some(Event::new(EventKind::MappingEnd, t.span)));
                }
                State::FlowMappingValue => {
                    if self.check(&[Id::Value])? {
                        let t = self.take();
                        if !self.check(&[Id::FlowEntry, Id::FlowMappingEnd])? {
                            self.states.push(State::FlowMappingKey(false));
                            return self.parse_node(false, false).map(Some);
                        }
                        self.state = State::FlowMappingKey(false);
                        return Ok(Some(self.empty(t.span.end)));
                    }
                    self.state = State::FlowMappingKey(false);
                    let m = self.peek()?.span.start;
                    return Ok(Some(self.empty(m)));
                }
                State::FlowMappingEmptyValue => {
                    self.state = State::FlowMappingKey(false);
                    let m = self.peek()?.span.start;
                    return Ok(Some(self.empty(m)));
                }
            }
        }
    }
    fn process_directives(&mut self) -> Result<()> {
        self.tags.remove("!");
        self.tags.remove("!!");
        let mut version = false;
        while self.check(&[Id::Directive])? {
            let t = self.take();
            if let Data::Directive(name, values) = t.data {
                let msg = if name == "YAML" {
                    if version {
                        Some("duplicate YAML directive")
                    } else if values[0] != "1" {
                        Some("incompatible YAML version")
                    } else {
                        version = true;
                        None
                    }
                } else if name == "TAG" {
                    if self.tags.contains_key(&values[0]) {
                        Some("duplicate tag handle")
                    } else {
                        self.tags.insert(values[0].clone(), values[1].clone());
                        None
                    }
                } else {
                    None
                };
                if let Some(m) = msg {
                    return Err(Error::marked("parser", None, m, t.span.start));
                }
            }
        }
        self.tags.entry("!".into()).or_insert("!".into());
        self.tags.entry("!!".into()).or_insert("tag:yaml.org,2002:".into());
        Ok(())
    }
    fn parse_node(&mut self, block: bool, indentless: bool) -> Result<Event> {
        if self.check(&[Id::Alias])? {
            let t = self.take();
            self.pop();
            return Ok(Event::new(EventKind::Alias(t.text()), t.span));
        }
        let mut start = None;
        let mut end = None;
        let mut anchor = None;
        let mut tag_tuple = None;
        let mut tag_mark = None;
        if self.check(&[Id::Anchor])? {
            let t = self.take();
            start = Some(t.span.start);
            end = Some(t.span.end);
            anchor = Some(t.text());
            if self.check(&[Id::Tag])? {
                let t = self.take();
                tag_mark = Some(t.span.start);
                end = Some(t.span.end);
                tag_tuple = Some(t.data);
            }
        } else if self.check(&[Id::Tag])? {
            let t = self.take();
            start = Some(t.span.start);
            tag_mark = start;
            end = Some(t.span.end);
            tag_tuple = Some(t.data);
            if self.check(&[Id::Anchor])? {
                let t = self.take();
                end = Some(t.span.end);
                anchor = Some(t.text());
            }
        }
        let tag = if let Some(Data::Tag(handle, suffix)) = tag_tuple {
            Some(if let Some(h) = handle {
                let prefix = self
                    .tags
                    .get(&h)
                    .ok_or_else(|| Error::marked("parser", start, "undefined tag handle", tag_mark.unwrap()))?;
                format!("{prefix}{suffix}")
            } else {
                suffix
            })
        } else {
            None
        };
        if start.is_none() {
            let t = self.peek()?;
            start = Some(t.span.start);
            end = start;
        }
        let start = start.unwrap();
        let mut end = end.unwrap();
        let kind;
        if indentless && self.check(&[Id::BlockEntry])? {
            end = self.peek()?.span.end;
            kind = EventKind::SequenceStart;
            self.state = State::IndentlessSequenceEntryKey;
        } else if self.check(&[Id::Scalar])? {
            let t = self.take();
            end = t.span.end;
            if let Data::Scalar(s, style) = t.data {
                kind = EventKind::Scalar(s, style);
            } else {
                unreachable!();
            }
            self.pop();
        } else if self.check(&[Id::FlowSequenceStart])? {
            end = self.peek()?.span.end;
            kind = EventKind::SequenceStart;
            self.state = State::FlowSequenceFirstEntry;
        } else if self.check(&[Id::FlowMappingStart])? {
            end = self.peek()?.span.end;
            kind = EventKind::MappingStart;
            self.state = State::FlowMappingFirstKey;
        } else if block && self.check(&[Id::BlockSequenceStart])? {
            end = self.peek()?.span.start;
            kind = EventKind::SequenceStart;
            self.state = State::BlockSequenceFirstEntry;
        } else if block && self.check(&[Id::BlockMappingStart])? {
            end = self.peek()?.span.start;
            kind = EventKind::MappingStart;
            self.state = State::BlockMappingFirstKey;
        } else if anchor.is_some() || tag.is_some() {
            kind = EventKind::Scalar(String::default(), Style::Plain);
            self.pop();
        } else {
            return self.error(Some(start), "expected node content");
        }
        Ok(Event {
            kind,
            anchor,
            tag,
            span: Span { start, end },
        })
    }
}
