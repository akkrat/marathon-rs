//! Port of SnakeYAML 2.2 org/yaml/snakeyaml/reader/StreamReader.java.
// Copyright (c) 2008, SnakeYAML. Licensed under Apache-2.0; see LICENSE.
use super::{Error, LINEBR, Result, has};

use crate::node::{Mark, YamlErrorKind, is_printable};

pub struct StreamReader {
    input: Vec<u16>,
    read: usize,
    window: Vec<u32>,
    pointer: usize,
    eof: bool,
    pub index: usize,
    pub document_index: usize,
    pub line: usize,
    pub column: usize,
}
impl StreamReader {
    pub fn new(text: &str) -> Self {
        Self {
            input: text.encode_utf16().collect(),
            read: 0,
            window: Vec::new(),
            pointer: 0,
            eof: false,
            index: 0,
            document_index: 0,
            line: 0,
            column: 0,
        }
    }
    pub fn mark(&self) -> Mark {
        Mark {
            index: self.index,
            line: self.line + 1,
            column: self.column + 1,
        }
    }
    pub fn forward(&mut self, length: usize) -> Result<()> {
        for _ in 0..length {
            if !self.ensure_enough_data(0)? {
                break;
            }
            let c = self.window[self.pointer];
            self.pointer += 1;
            self.index += 1;
            self.document_index += 1;
            if has(LINEBR, c) || (c == 13 && self.ensure_enough_data(0)? && self.window[self.pointer] != 10) {
                self.line += 1;
                self.column = 0;
            } else if c != 0xfeff {
                self.column += 1;
            }
        }
        Ok(())
    }
    pub fn peek(&mut self, n: usize) -> Result<u32> {
        Ok(if self.ensure_enough_data(n)? {
            self.window[self.pointer + n]
        } else {
            0
        })
    }
    pub fn prefix(&mut self, length: usize) -> Result<String> {
        if length > 0 {
            self.ensure_enough_data(length)?;
        }
        let mut out = String::default();
        for &c in &self.window[self.pointer..(self.pointer + length).min(self.window.len())] {
            out.push(char::from_u32(c).unwrap_or('\u{fffd}'));
        }
        Ok(out)
    }
    pub fn prefix_forward(&mut self, length: usize) -> Result<String> {
        let prefix = self.prefix(length)?;
        self.pointer += length;
        self.index += length;
        self.document_index += length;
        self.column += length;
        Ok(prefix)
    }
    fn ensure_enough_data(&mut self, size: usize) -> Result<bool> {
        if !self.eof && self.pointer + size >= self.window.len() {
            self.update()?;
        }
        Ok(self.pointer + size < self.window.len())
    }
    fn update(&mut self) -> Result<()> {
        let mut end = (self.read + 1024).min(self.input.len());
        if end == self.read {
            self.eof = true;
            return Ok(());
        }
        if (0xd800..=0xdbff).contains(&self.input[end - 1]) {
            if end == self.input.len() {
                self.eof = true;
            } else {
                end += 1;
            }
        }
        self.window.drain(..self.pointer);
        self.pointer = 0;
        while self.read < end {
            let c = u32::from(self.input[self.read]);
            self.read += 1;
            let c = if (0xd800..=0xdbff).contains(&c)
                && self.read < end
                && (0xdc00..=0xdfff).contains(&self.input[self.read])
            {
                let lo = u32::from(self.input[self.read]);
                self.read += 1;
                0x10000 + ((c - 0xd800) << 10) + (lo - 0xdc00)
            } else {
                c
            };
            self.window.push(c);
            if !char::from_u32(c).is_some_and(is_printable) {
                return Err(Error::plain(
                    YamlErrorKind::SpecialCharacter,
                    "special characters are not allowed",
                ));
            }
        }
        Ok(())
    }
}
