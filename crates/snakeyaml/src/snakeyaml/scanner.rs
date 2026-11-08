//! Port of SnakeYAML 2.2 org/yaml/snakeyaml/scanner/{ScannerImpl,SimpleKey}.java.
// Copyright (c) 2008, SnakeYAML. Licensed under Apache-2.0; see LICENSE.
// Jackson disables comment tokens; comment text is still read and checked.
use super::reader::StreamReader;
use super::*;
use crate::node::CODE_POINT_LIMIT;
use std::collections::VecDeque;

#[derive(Clone)]
struct SimpleKey {
    level: i32,
    number: usize,
    required: bool,
    mark: Mark,
}
pub struct Scanner {
    pub reader: StreamReader,
    done: bool,
    flow_level: i32,
    tokens: VecDeque<Token>,
    tokens_taken: usize,
    indent: i32,
    indents: Vec<i32>,
    allow_simple_key: bool,
    possible_simple_keys: Vec<SimpleKey>,
}
impl Scanner {
    pub fn new(text: &str) -> Self {
        let reader = StreamReader::new(text);
        let m = reader.mark();
        Self {
            reader,
            done: false,
            flow_level: 0,
            tokens: VecDeque::from([Token::new(Id::StreamStart, m, m)]),
            tokens_taken: 0,
            indent: -1,
            indents: Vec::new(),
            allow_simple_key: true,
            possible_simple_keys: Vec::new(),
        }
    }
    pub fn check(&mut self, ids: &[Id]) -> Result<bool> {
        let id = self.peek()?.id;
        Ok(ids.contains(&id))
    }
    pub fn peek(&mut self) -> Result<&Token> {
        while self.need_more_tokens()? {
            self.fetch_more_tokens()?;
        }
        Ok(&self.tokens[0])
    }
    pub fn take(&mut self) -> Token {
        self.tokens_taken += 1;
        self.tokens.pop_front().unwrap()
    }
    fn need_more_tokens(&mut self) -> Result<bool> {
        if self.done {
            return Ok(false);
        }
        if self.tokens.is_empty() {
            return Ok(true);
        }
        self.stale_possible_simple_keys()?;
        Ok(self
            .possible_simple_keys
            .first()
            .is_some_and(|k| k.number == self.tokens_taken))
    }
    fn err(&self, context: Option<Mark>, msg: &str) -> Error {
        Error::marked("scanner", context, msg, self.reader.mark())
    }
    fn add(&mut self, id: Id, start: Mark, end: Mark) {
        self.tokens.push_back(Token::new(id, start, end));
    }
    fn indicator(&mut self, id: Id, n: usize) -> Result<()> {
        let start = self.reader.mark();
        self.reader.forward(n)?;
        self.add(id, start, self.reader.mark());
        Ok(())
    }
    fn fetch_more_tokens(&mut self) -> Result<()> {
        if self.reader.document_index > CODE_POINT_LIMIT {
            return Err(Error::plain(
                crate::node::YamlErrorKind::CodePointLimit,
                "document exceeds the code point limit",
            ));
        }
        self.scan_to_next_token()?;
        self.stale_possible_simple_keys()?;
        self.unwind_indent(self.reader.column as i32);
        let c = self.reader.peek(0)?;
        match c {
            0 => return self.fetch_stream_end(),
            37 if self.reader.column == 0 => return self.fetch_directive(),
            45 | 46 if self.check_document_indicator(c)? => return self.fetch_document_indicator(c == 45),
            91 | 123 => return self.fetch_flow_collection_start(c == 123),
            93 | 125 => return self.fetch_flow_collection_end(c == 125),
            44 => return self.fetch_flow_entry(),
            45 if has(NULL_BL_T_LINEBR, self.reader.peek(1)?) => return self.fetch_block_entry(),
            63 if self.flow_level != 0 || has(NULL_BL_T_LINEBR, self.reader.peek(1)?) => return self.fetch_key(),
            58 if self.flow_level != 0 || has(NULL_BL_T_LINEBR, self.reader.peek(1)?) => return self.fetch_value(),
            42 | 38 => return self.fetch_anchor(c == 38),
            33 => return self.fetch_tag(),
            124 | 62 if self.flow_level == 0 => return self.fetch_block_scalar(c == 62),
            39 | 34 => return self.fetch_flow_scalar(c == 34),
            _ => {}
        }
        if self.check_plain()? {
            return self.fetch_plain();
        }
        Err(self.err(None, "character cannot start any token"))
    }
    fn fetch_stream_end(&mut self) -> Result<()> {
        self.unwind_indent(-1);
        self.remove_possible_simple_key()?;
        self.allow_simple_key = false;
        self.possible_simple_keys.clear();
        let m = self.reader.mark();
        self.add(Id::StreamEnd, m, m);
        self.done = true;
        Ok(())
    }
    fn fetch_directive(&mut self) -> Result<()> {
        self.unwind_indent(-1);
        self.remove_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_directive()?;
        self.tokens.push_back(t);
        Ok(())
    }
    fn fetch_document_indicator(&mut self, start: bool) -> Result<()> {
        self.unwind_indent(-1);
        self.remove_possible_simple_key()?;
        self.allow_simple_key = false;
        self.indicator(if start { Id::DocumentStart } else { Id::DocumentEnd }, 3)
    }
    fn fetch_flow_collection_start(&mut self, mapping: bool) -> Result<()> {
        self.save_possible_simple_key()?;
        self.flow_level += 1;
        self.allow_simple_key = true;
        self.indicator(
            if !mapping {
                Id::FlowSequenceStart
            } else {
                Id::FlowMappingStart
            },
            1,
        )
    }
    fn fetch_flow_collection_end(&mut self, mapping: bool) -> Result<()> {
        self.remove_possible_simple_key()?;
        self.flow_level -= 1;
        self.allow_simple_key = false;
        self.indicator(
            if !mapping {
                Id::FlowSequenceEnd
            } else {
                Id::FlowMappingEnd
            },
            1,
        )
    }
    fn fetch_flow_entry(&mut self) -> Result<()> {
        self.allow_simple_key = true;
        self.remove_possible_simple_key()?;
        self.indicator(Id::FlowEntry, 1)
    }
    fn fetch_block_entry(&mut self) -> Result<()> {
        if self.flow_level == 0 {
            if !self.allow_simple_key {
                return Err(self.err(None, "sequence entries are not allowed here"));
            }
            if self.add_indent(self.reader.column as i32) {
                let m = self.reader.mark();
                self.add(Id::BlockSequenceStart, m, m);
            }
        }
        self.allow_simple_key = true;
        self.remove_possible_simple_key()?;
        self.indicator(Id::BlockEntry, 1)
    }
    fn fetch_key(&mut self) -> Result<()> {
        if self.flow_level == 0 {
            if !self.allow_simple_key {
                return Err(self.err(None, "mapping keys are not allowed here"));
            }
            if self.add_indent(self.reader.column as i32) {
                let m = self.reader.mark();
                self.add(Id::BlockMappingStart, m, m);
            }
        }
        self.allow_simple_key = self.flow_level == 0;
        self.remove_possible_simple_key()?;
        self.indicator(Id::Key, 1)
    }
    fn fetch_value(&mut self) -> Result<()> {
        if let Some(pos) = self
            .possible_simple_keys
            .iter()
            .position(|k| k.level == self.flow_level)
        {
            let k = self.possible_simple_keys.remove(pos);
            let at = k.number - self.tokens_taken;
            self.tokens.insert(at, Token::new(Id::Key, k.mark, k.mark));
            if self.flow_level == 0 && self.add_indent((k.mark.column - 1) as i32) {
                self.tokens
                    .insert(at, Token::new(Id::BlockMappingStart, k.mark, k.mark));
            }
            self.allow_simple_key = false;
        } else {
            if self.flow_level == 0 {
                if !self.allow_simple_key {
                    return Err(self.err(None, "mapping values are not allowed here"));
                }
                if self.add_indent(self.reader.column as i32) {
                    let m = self.reader.mark();
                    self.add(Id::BlockMappingStart, m, m);
                }
            }
            self.allow_simple_key = self.flow_level == 0;
            self.remove_possible_simple_key()?;
        }
        self.indicator(Id::Value, 1)
    }
    fn fetch_anchor(&mut self, anchor: bool) -> Result<()> {
        self.save_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_anchor(anchor)?;
        self.tokens.push_back(t);
        Ok(())
    }
    fn fetch_tag(&mut self) -> Result<()> {
        self.save_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_tag()?;
        self.tokens.push_back(t);
        Ok(())
    }
    fn fetch_block_scalar(&mut self, folded: bool) -> Result<()> {
        self.allow_simple_key = true;
        self.remove_possible_simple_key()?;
        let t = self.scan_block_scalar(folded)?;
        self.tokens.push_back(t);
        Ok(())
    }
    fn fetch_flow_scalar(&mut self, double: bool) -> Result<()> {
        self.save_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_flow_scalar(double)?;
        self.tokens.push_back(t);
        Ok(())
    }
    fn fetch_plain(&mut self) -> Result<()> {
        self.save_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_plain()?;
        self.tokens.push_back(t);
        Ok(())
    }
    fn stale_possible_simple_keys(&mut self) -> Result<()> {
        let mut i = 0;
        while i < self.possible_simple_keys.len() {
            let k = &self.possible_simple_keys[i];
            if k.mark.line != self.reader.line + 1 || self.reader.index - k.mark.index > 1024 {
                if k.required {
                    return Err(self.err(Some(k.mark), "could not find expected ':'"));
                }
                self.possible_simple_keys.remove(i);
            } else {
                i += 1;
            }
        }
        Ok(())
    }
    fn save_possible_simple_key(&mut self) -> Result<()> {
        let required = self.flow_level == 0 && self.indent == self.reader.column as i32;
        if self.allow_simple_key {
            self.remove_possible_simple_key()?;
            self.possible_simple_keys.push(SimpleKey {
                level: self.flow_level,
                number: self.tokens_taken + self.tokens.len(),
                required,
                mark: self.reader.mark(),
            });
        }
        Ok(())
    }
    fn remove_possible_simple_key(&mut self) -> Result<()> {
        if let Some(i) = self
            .possible_simple_keys
            .iter()
            .position(|k| k.level == self.flow_level)
        {
            let k = self.possible_simple_keys.remove(i);
            if k.required {
                return Err(self.err(Some(k.mark), "could not find expected ':'"));
            }
        }
        Ok(())
    }
    fn unwind_indent(&mut self, col: i32) {
        if self.flow_level != 0 {
            return;
        }
        while self.indent > col {
            let m = self.reader.mark();
            self.indent = self.indents.pop().unwrap();
            self.add(Id::BlockEnd, m, m);
        }
    }
    fn add_indent(&mut self, col: i32) -> bool {
        if self.indent < col {
            self.indents.push(self.indent);
            self.indent = col;
            true
        } else {
            false
        }
    }
    fn check_document_indicator(&mut self, c: u32) -> Result<bool> {
        Ok(self.reader.column == 0
            && self.reader.prefix(3)? == if c == 45 { "---" } else { "..." }
            && has(NULL_BL_T_LINEBR, self.reader.peek(3)?))
    }
    fn check_plain(&mut self) -> Result<bool> {
        let c = self.reader.peek(0)?;
        Ok((!has(NULL_BL_T_LINEBR, c) && !has("-?:,[]{}#&*!|>'\"%@`", c))
            || (!has(NULL_BL_T_LINEBR, self.reader.peek(1)?) && (c == 45 || (self.flow_level == 0 && has("?:", c)))))
    }
    fn scan_to_next_token(&mut self) -> Result<()> {
        if self.reader.index == 0 && self.reader.peek(0)? == 0xfeff {
            self.reader.forward(1)?;
        }
        loop {
            let mut ff = 0;
            while self.reader.peek(ff)? == 32 {
                ff += 1;
            }
            if ff > 0 {
                self.reader.forward(ff)?;
            }
            if self.reader.peek(0)? == 35 {
                self.scan_comment()?;
            }
            if self.scan_line_break()?.is_empty() {
                break;
            }
            if self.flow_level == 0 {
                self.allow_simple_key = true;
            }
        }
        Ok(())
    }
    fn scan_comment(&mut self) -> Result<()> {
        self.reader.forward(1)?;
        let mut length = 0;
        while !has(NULL_OR_LINEBR, self.reader.peek(length)?) {
            length += 1;
        }
        self.reader.prefix_forward(length)?;
        Ok(())
    }
    fn scan_directive(&mut self) -> Result<Token> {
        let start = self.reader.mark();
        self.reader.forward(1)?;
        let name = self.scan_directive_name(start)?;
        let data;
        let end;
        if name == "YAML" {
            while self.reader.peek(0)? == 32 {
                self.reader.forward(1)?;
            }
            let major = self.scan_yaml_directive_number(start)?;
            if self.reader.peek(0)? != 46 {
                return Err(self.err(Some(start), "expected a digit or '.'"));
            }
            self.reader.forward(1)?;
            let minor = self.scan_yaml_directive_number(start)?;
            if !has(NULL_BL_LINEBR, self.reader.peek(0)?) {
                return Err(self.err(Some(start), "expected a digit or space"));
            }
            data = vec![major, minor];
            end = self.reader.mark();
        } else if name == "TAG" {
            while self.reader.peek(0)? == 32 {
                self.reader.forward(1)?;
            }
            let handle = self.scan_tag_handle(start)?;
            if self.reader.peek(0)? != 32 {
                return Err(self.err(Some(start), "expected space after tag handle"));
            }
            while self.reader.peek(0)? == 32 {
                self.reader.forward(1)?;
            }
            let prefix = self.scan_tag_uri(start)?;
            if !has(NULL_BL_LINEBR, self.reader.peek(0)?) {
                return Err(self.err(Some(start), "expected space after tag prefix"));
            }
            data = vec![handle, prefix];
            end = self.reader.mark();
        } else {
            end = self.reader.mark();
            data = Vec::new();
            let mut ff = 0;
            while !has(NULL_OR_LINEBR, self.reader.peek(ff)?) {
                ff += 1;
            }
            if ff > 0 {
                self.reader.forward(ff)?;
            }
        }
        self.scan_ignored_line(start)?;
        Ok(Token {
            id: Id::Directive,
            data: Data::Directive(name, data),
            span: Span { start, end },
        })
    }
    fn scan_directive_name(&mut self, start: Mark) -> Result<String> {
        let mut length = 0;
        while has(ALPHA, self.reader.peek(length)?) {
            length += 1;
        }
        if length == 0 {
            return Err(self.err(Some(start), "expected alphabetic or numeric character"));
        }
        let v = self.reader.prefix_forward(length)?.to_owned();
        if !has(NULL_BL_LINEBR, self.reader.peek(0)?) {
            return Err(self.err(Some(start), "expected alphabetic or numeric character"));
        }
        Ok(v)
    }
    fn scan_yaml_directive_number(&mut self, start: Mark) -> Result<String> {
        let mut length = 0;
        while crate::jnum::is_digit(self.reader.peek(length)?) {
            length += 1;
        }
        if length == 0 {
            return Err(self.err(Some(start), "expected a digit"));
        }
        let v = self.reader.prefix_forward(length)?.to_owned();
        if length > 3 {
            return Err(self.err(Some(start), "invalid YAML version number"));
        }
        crate::jnum::parse_long(&v)
            .map(|n| n.to_string())
            .ok_or_else(|| Error::plain(crate::node::YamlErrorKind::NumberFormat, "invalid version integer"))
    }
    fn scan_ignored_line(&mut self, start: Mark) -> Result<()> {
        while self.reader.peek(0)? == 32 {
            self.reader.forward(1)?;
        }
        if self.reader.peek(0)? == 35 {
            self.scan_comment()?;
        }
        let c = self.reader.peek(0)?;
        if self.scan_line_break()?.is_empty() && c != 0 {
            return Err(self.err(Some(start), "expected a comment or line break"));
        }
        Ok(())
    }
    fn scan_anchor(&mut self, anchor: bool) -> Result<Token> {
        let start = self.reader.mark();
        self.reader.forward(1)?;
        let mut length = 0;
        loop {
            let c = self.reader.peek(length)?;
            if has(NULL_BL_T_LINEBR, c) || has(":,[]{}/.*&", c) {
                break;
            }
            length += 1;
        }
        if length == 0 {
            return Err(self.err(Some(start), "unexpected anchor/alias character"));
        }
        let v = self.reader.prefix_forward(length)?.to_owned();
        let c = self.reader.peek(0)?;
        if !has(NULL_BL_T_LINEBR, c) && !has("?:,]}%@`", c) {
            return Err(self.err(Some(start), "unexpected anchor/alias character"));
        }
        Ok(Token {
            id: if anchor { Id::Anchor } else { Id::Alias },
            data: Data::Text(v),
            span: Span {
                start,
                end: self.reader.mark(),
            },
        })
    }
    fn scan_tag(&mut self) -> Result<Token> {
        let start = self.reader.mark();
        let c = self.reader.peek(1)?;
        let (handle, suffix);
        if c == 60 {
            self.reader.forward(2)?;
            suffix = self.scan_tag_uri(start)?;
            handle = None;
            if self.reader.peek(0)? != 62 {
                return Err(self.err(Some(start), "expected '>'"));
            }
            self.reader.forward(1)?;
        } else if has(NULL_BL_T_LINEBR, c) {
            handle = None;
            suffix = "!".into();
            self.reader.forward(1)?;
        } else {
            let mut length = 1;
            let mut c = c;
            let mut use_handle = false;
            while !has(NULL_BL_LINEBR, c) {
                if c == 33 {
                    use_handle = true;
                    break;
                }
                length += 1;
                c = self.reader.peek(length)?;
            }
            handle = Some(if use_handle {
                self.scan_tag_handle(start)?
            } else {
                self.reader.forward(1)?;
                "!".into()
            });
            suffix = self.scan_tag_uri(start)?;
        }
        if !has(NULL_BL_LINEBR, self.reader.peek(0)?) {
            return Err(self.err(Some(start), "expected space after tag"));
        }
        Ok(Token {
            id: Id::Tag,
            data: Data::Tag(handle, suffix),
            span: Span {
                start,
                end: self.reader.mark(),
            },
        })
    }
    fn scan_tag_handle(&mut self, start: Mark) -> Result<String> {
        if self.reader.peek(0)? != 33 {
            return Err(self.err(Some(start), "expected '!'"));
        }
        let mut length = 1;
        let mut c = self.reader.peek(length)?;
        if c != 32 {
            while has(ALPHA, c) {
                length += 1;
                c = self.reader.peek(length)?;
            }
            if c != 33 {
                self.reader.forward(length)?;
                return Err(self.err(Some(start), "expected '!'"));
            }
            length += 1;
        }
        Ok(self.reader.prefix_forward(length)?.to_owned())
    }
    fn scan_tag_uri(&mut self, start: Mark) -> Result<String> {
        let mut chunks = String::new();
        let mut length = 0;
        let mut c = self.reader.peek(length)?;
        while has(URI_CHARS, c) {
            if c == 37 {
                chunks.push_str(&self.reader.prefix_forward(length)?.to_owned());
                length = 0;
                chunks.push_str(&self.scan_uri_escapes(start)?);
            } else {
                length += 1;
            }
            c = self.reader.peek(length)?;
        }
        if length != 0 {
            chunks.push_str(&self.reader.prefix_forward(length)?.to_owned());
        }
        if chunks.is_empty() {
            return Err(self.err(Some(start), "expected URI"));
        }
        Ok(chunks)
    }
    fn scan_uri_escapes(&mut self, start: Mark) -> Result<String> {
        let mut length = 1;
        while self.reader.peek(length * 3)? == 37 {
            length += 1;
        }
        let beginning = self.reader.mark();
        let mut bytes = Vec::with_capacity(length);
        while self.reader.peek(0)? == 37 {
            self.reader.forward(1)?;
            let s = self.reader.prefix(2)?.to_owned();
            let code = crate::jnum::parse_radix(&s, 16)
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| self.err(Some(start), "expected URI escape of two hexadecimal digits"))?;
            bytes.push(code as u8);
            self.reader.forward(2)?;
        }
        String::from_utf8(bytes).map_err(|_| Error::marked("scanner", Some(start), "expected UTF-8 URI", beginning))
    }
    fn scan_block_scalar(&mut self, folded: bool) -> Result<Token> {
        let start = self.reader.mark();
        self.reader.forward(1)?;
        let (chomp, increment) = self.scan_block_scalar_indicators(start)?;
        self.scan_ignored_line(start)?;
        let min_indent = (self.indent + 1).max(1) as usize;
        let (mut breaks, indent, mut end) = if let Some(inc) = increment {
            let indent = min_indent + inc - 1;
            let (b, e) = self.scan_block_scalar_breaks(indent)?;
            (b, indent, e)
        } else {
            let (b, max, e) = self.scan_block_scalar_indentation()?;
            (b, min_indent.max(max), e)
        };
        let mut chunks = String::default();
        let mut line_break = String::new();
        while self.reader.column == indent && self.reader.peek(0)? != 0 {
            chunks.push_str(&breaks);
            let leading_non_space = !has(" \t", self.reader.peek(0)?);
            let mut length = 0;
            while !has(NULL_OR_LINEBR, self.reader.peek(length)?) {
                length += 1;
            }
            chunks.push_str(&self.reader.prefix_forward(length)?);
            line_break = self.scan_line_break()?;
            (breaks, end) = self.scan_block_scalar_breaks(indent)?;
            if self.reader.column == indent && self.reader.peek(0)? != 0 {
                if folded && line_break == "\n" && leading_non_space && !has(" \t", self.reader.peek(0)?) {
                    if breaks.is_empty() {
                        chunks.push(' ');
                    }
                } else {
                    chunks.push_str(&line_break);
                }
            } else {
                break;
            }
        }
        if chomp != Some(false) {
            chunks.push_str(&line_break);
        }
        if chomp == Some(true) {
            chunks.push_str(&breaks);
        }
        Ok(Token {
            id: Id::Scalar,
            data: Data::Scalar(chunks, if folded { Style::Folded } else { Style::Literal }),
            span: Span { start, end },
        })
    }
    fn scan_block_scalar_indicators(&mut self, start: Mark) -> Result<(Option<bool>, Option<usize>)> {
        let mut chomp = None;
        let mut inc = None;
        let mut c = self.reader.peek(0)?;
        if c == 45 || c == 43 {
            chomp = Some(c == 43);
            self.reader.forward(1)?;
            c = self.reader.peek(0)?;
            if crate::jnum::is_digit(c) {
                let n = (c <= 0xffff)
                    .then(|| crate::jnum::digit(c, 10))
                    .flatten()
                    .ok_or_else(|| {
                        Error::plain(crate::node::YamlErrorKind::NumberFormat, "invalid indentation integer")
                    })?;
                if n == 0 {
                    return Err(self.err(Some(start), "indentation indicator must be 1-9"));
                }
                inc = Some(n as usize);
                self.reader.forward(1)?;
            }
        } else if crate::jnum::is_digit(c) {
            let n = (c <= 0xffff)
                .then(|| crate::jnum::digit(c, 10))
                .flatten()
                .ok_or_else(|| Error::plain(crate::node::YamlErrorKind::NumberFormat, "invalid indentation integer"))?;
            if n == 0 {
                return Err(self.err(Some(start), "indentation indicator must be 1-9"));
            }
            inc = Some(n as usize);
            self.reader.forward(1)?;
            c = self.reader.peek(0)?;
            if c == 45 || c == 43 {
                chomp = Some(c == 43);
                self.reader.forward(1)?;
            }
        }
        if !has(NULL_BL_LINEBR, self.reader.peek(0)?) {
            return Err(self.err(Some(start), "expected chomping or indentation indicators"));
        }
        Ok((chomp, inc))
    }
    fn scan_block_scalar_indentation(&mut self) -> Result<(String, usize, Mark)> {
        let mut chunks = String::new();
        let mut max = 0;
        let mut end = self.reader.mark();
        while has(" \r\n\u{85}\u{2028}\u{2029}", self.reader.peek(0)?) {
            if self.reader.peek(0)? != 32 {
                chunks.push_str(&self.scan_line_break()?);
                end = self.reader.mark();
            } else {
                self.reader.forward(1)?;
                max = max.max(self.reader.column);
            }
        }
        Ok((chunks, max, end))
    }
    fn scan_block_scalar_breaks(&mut self, indent: usize) -> Result<(String, Mark)> {
        let mut chunks = String::new();
        let mut end = self.reader.mark();
        while self.reader.column < indent && self.reader.peek(0)? == 32 {
            self.reader.forward(1)?;
        }
        loop {
            let lb = self.scan_line_break()?;
            if lb.is_empty() {
                break;
            }
            chunks.push_str(&lb);
            end = self.reader.mark();
            while self.reader.column < indent && self.reader.peek(0)? == 32 {
                self.reader.forward(1)?;
            }
        }
        Ok((chunks, end))
    }
    fn scan_flow_scalar(&mut self, double: bool) -> Result<Token> {
        let start = self.reader.mark();
        let quote = self.reader.peek(0)?;
        self.reader.forward(1)?;
        let mut chunks = self.scan_flow_scalar_non_spaces(double, start)?;
        while self.reader.peek(0)? != quote {
            chunks.extend(self.scan_flow_scalar_spaces(start)?.encode_utf16());
            chunks.extend(self.scan_flow_scalar_non_spaces(double, start)?);
        }
        self.reader.forward(1)?;
        Ok(Token {
            id: Id::Scalar,
            data: Data::Scalar(
                String::from_utf16_lossy(&chunks),
                if double {
                    Style::DoubleQuoted
                } else {
                    Style::SingleQuoted
                },
            ),
            span: Span {
                start,
                end: self.reader.mark(),
            },
        })
    }
    fn scan_flow_scalar_non_spaces(&mut self, double: bool, start: Mark) -> Result<Vec<u16>> {
        let mut chunks = Vec::new();
        loop {
            let mut length = 0;
            loop {
                let c = self.reader.peek(length)?;
                if has(NULL_BL_T_LINEBR, c) || has("'\"\\", c) {
                    break;
                }
                length += 1;
            }
            if length != 0 {
                chunks.extend(self.reader.prefix_forward(length)?.encode_utf16());
            }
            let c = self.reader.peek(0)?;
            if !double && c == 39 && self.reader.peek(1)? == 39 {
                chunks.push(39);
                self.reader.forward(2)?;
            } else if (double && c == 39) || (!double && has("\"\\", c)) {
                chunks.push(c as u16);
                self.reader.forward(1)?;
            } else if double && c == 92 {
                self.reader.forward(1)?;
                let c = self.reader.peek(0)?;
                let replacement = match c {
                    48 => Some(0),
                    97 => Some(7),
                    98 => Some(8),
                    116 => Some(9),
                    110 => Some(10),
                    118 => Some(11),
                    102 => Some(12),
                    114 => Some(13),
                    101 => Some(27),
                    32 => Some(32),
                    34 => Some(34),
                    92 => Some(92),
                    78 => Some(0x85),
                    95 => Some(0xa0),
                    76 => Some(0x2028),
                    80 => Some(0x2029),
                    _ => None,
                };
                if let Some(v) = replacement {
                    chunks.push(v as u16);
                    self.reader.forward(1)?;
                } else if has("xuU", c) {
                    let length = if c == 120 {
                        2
                    } else if c == 117 {
                        4
                    } else {
                        8
                    };
                    self.reader.forward(1)?;
                    let hex = self.reader.prefix(length)?.to_owned();
                    if hex.chars().any(|c| !c.is_ascii_hexdigit()) {
                        return Err(self.err(Some(start), "expected hexadecimal escape digits"));
                    }
                    let decimal = u32::from_str_radix(&hex, 16).map_err(|_| {
                        Error::plain(crate::node::YamlErrorKind::NumberFormat, "invalid escape integer")
                    })?;
                    if decimal > i32::MAX as u32 {
                        return Err(Error::plain(
                            crate::node::YamlErrorKind::NumberFormat,
                            "escape integer overflow",
                        ));
                    }
                    if decimal > 0x10ffff {
                        return Err(self.err(Some(start), "unknown escape character"));
                    }
                    // Assemble escaped pairs before replacing lone surrogates at the scalar boundary (D55).
                    if decimal <= 0xffff {
                        chunks.push(decimal as u16);
                    } else {
                        chunks.extend(
                            char::from_u32(decimal)
                                .unwrap()
                                .encode_utf16(&mut [0; 2])
                                .iter()
                                .copied(),
                        );
                    }
                    self.reader.forward(length)?;
                } else if !self.scan_line_break()?.is_empty() {
                    chunks.extend(self.scan_flow_scalar_breaks(start)?.encode_utf16());
                } else {
                    return Err(self.err(Some(start), "unknown escape character"));
                }
            } else {
                return Ok(chunks);
            }
        }
    }
    fn scan_flow_scalar_spaces(&mut self, start: Mark) -> Result<String> {
        let mut length = 0;
        while has(" \t", self.reader.peek(length)?) {
            length += 1;
        }
        let whitespaces = self.reader.prefix_forward(length)?;
        if self.reader.peek(0)? == 0 {
            return Err(self.err(Some(start), "unexpected end of stream in quoted scalar"));
        }
        let lb = self.scan_line_break()?;
        if lb.is_empty() {
            return Ok(whitespaces);
        }
        let breaks = self.scan_flow_scalar_breaks(start)?;
        let mut chunks = String::default();
        if lb != "\n" {
            chunks.push_str(&lb);
        } else if breaks.is_empty() {
            chunks.push(' ');
        }
        chunks.push_str(&breaks);
        Ok(chunks)
    }
    fn scan_flow_scalar_breaks(&mut self, start: Mark) -> Result<String> {
        let mut chunks = String::new();
        loop {
            let prefix = self.reader.prefix(3)?.to_owned();
            if (prefix == "---" || prefix == "...") && has(NULL_BL_T_LINEBR, self.reader.peek(3)?) {
                return Err(self.err(Some(start), "unexpected document separator in quoted scalar"));
            }
            while has(" \t", self.reader.peek(0)?) {
                self.reader.forward(1)?;
            }
            let lb = self.scan_line_break()?;
            if lb.is_empty() {
                return Ok(chunks);
            }
            chunks.push_str(&lb);
        }
    }
    fn scan_plain(&mut self) -> Result<Token> {
        let start = self.reader.mark();
        let mut end = start;
        let indent = self.indent + 1;
        let mut chunks = String::default();
        let mut spaces = String::new();
        loop {
            if self.reader.peek(0)? == 35 {
                break;
            }
            let mut length = 0;
            loop {
                let c = self.reader.peek(length)?;
                if has(NULL_BL_T_LINEBR, c)
                    || (c == 58
                        && (has(NULL_BL_T_LINEBR, self.reader.peek(length + 1)?)
                            || (self.flow_level != 0 && has(",[]{}", self.reader.peek(length + 1)?))))
                    || (self.flow_level != 0 && has(",?[]{}", c))
                {
                    break;
                }
                length += 1;
            }
            if length == 0 {
                break;
            }
            self.allow_simple_key = false;
            chunks.push_str(&spaces);
            chunks.push_str(&self.reader.prefix_forward(length)?);
            end = self.reader.mark();
            spaces = self.scan_plain_spaces()?;
            if spaces.is_empty()
                || self.reader.peek(0)? == 35
                || (self.flow_level == 0 && (self.reader.column as i32) < indent)
            {
                break;
            }
        }
        Ok(Token {
            id: Id::Scalar,
            data: Data::Scalar(chunks, Style::Plain),
            span: Span { start, end },
        })
    }
    fn scan_plain_spaces(&mut self) -> Result<String> {
        let mut length = 0;
        while has(" \t", self.reader.peek(length)?) {
            length += 1;
        }
        let whitespaces = self.reader.prefix_forward(length)?.to_owned();
        let lb = self.scan_line_break()?;
        if lb.is_empty() {
            return Ok(whitespaces);
        }
        self.allow_simple_key = true;
        let prefix = self.reader.prefix(3)?.to_owned();
        if prefix == "---" || (prefix == "..." && has(NULL_BL_T_LINEBR, self.reader.peek(3)?)) {
            return Ok(String::new());
        }
        let mut breaks = String::new();
        loop {
            if self.reader.peek(0)? == 32 {
                self.reader.forward(1)?;
            } else {
                let b = self.scan_line_break()?;
                if b.is_empty() {
                    break;
                }
                breaks.push_str(&b);
                let prefix = self.reader.prefix(3)?.to_owned();
                if prefix == "---" || (prefix == "..." && has(NULL_BL_T_LINEBR, self.reader.peek(3)?)) {
                    return Ok(String::new());
                }
            }
        }
        Ok(if lb != "\n" {
            lb + &breaks
        } else if breaks.is_empty() {
            " ".into()
        } else {
            breaks
        })
    }
    fn scan_line_break(&mut self) -> Result<String> {
        let c = self.reader.peek(0)?;
        if c == 13 || c == 10 || c == 0x85 {
            let n = if c == 13 && self.reader.peek(1)? == 10 { 2 } else { 1 };
            self.reader.forward(n)?;
            Ok("\n".into())
        } else if c == 0x2028 || c == 0x2029 {
            self.reader.forward(1)?;
            Ok(char::from_u32(c).unwrap().to_string())
        } else {
            Ok(String::new())
        }
    }
}
