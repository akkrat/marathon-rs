//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/log/Log.kt,
//! vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/log/LogCatHeader.kt,
//! vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/log/LogCatMessage.kt and
//! vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/log/LogcatMessageParser.kt
//!
//! Only `LogLevel` is taken from `Log.kt`; the hex-dump `mSpaceLine` prep is
//! dead (P5). The Kotlin parser takes an `AndroidDevice` it never uses
//! (`getPackageName` always returns `"?"`); this port takes no device.
//! There are no Kotlin tests for these files: the tests below are new and
//! derived from the source (WP 6.3 findings).

use jcompat::{jnum, regex_patterns::LOGCAT_HEADER};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LogLevel {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
    Assert,
}
impl LogLevel {
    pub fn priority(self) -> i32 {
        match self {
            Self::Verbose => 2,
            Self::Debug => 3,
            Self::Info => 4,
            Self::Warn => 5,
            Self::Error => 6,
            Self::Assert => 7,
        }
    }
    pub fn string_value(self) -> &'static str {
        match self {
            Self::Verbose => "verbose",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Assert => "assert",
        }
    }
    pub fn priority_letter(self) -> u16 {
        match self {
            Self::Verbose => u16::from(b'V'),
            Self::Debug => u16::from(b'D'),
            Self::Info => u16::from(b'I'),
            Self::Warn => u16::from(b'W'),
            Self::Error => u16::from(b'E'),
            Self::Assert => u16::from(b'A'),
        }
    }
    fn get_by_letter(letter: u16) -> Option<Self> {
        [
            Self::Verbose,
            Self::Debug,
            Self::Info,
            Self::Warn,
            Self::Error,
            Self::Assert,
        ]
        .into_iter()
        .find(|level| level.priority_letter() == letter)
    }
    /// Only the first character is used; empty input finds nothing.
    pub fn get_by_letter_string(letter: &str) -> Option<Self> {
        letter.encode_utf16().next().and_then(Self::get_by_letter)
    }
}

/// Equality is over all six fields, as in Kotlin. The Kotlin `hashCode`
/// arithmetic is unobservable and not reproduced (P5); the derived `Hash`
/// is only consistent with `Eq`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LogCatHeader {
    pub log_level: LogLevel,
    pub pid: i32,
    pub tid: i32,
    pub app_name: String,
    pub tag: String,
    pub timestamp: String,
}
impl LogCatHeader {
    /// Kotlin `toString()`: `"<timestamp>: <letter>/<tag>(<pid>)"`.
    pub fn to_kotlin_string(&self) -> String {
        let mut out = self.timestamp.clone();
        out.push_str(": ");
        out.push(char::from_u32(u32::from(self.log_level.priority_letter())).unwrap());
        out.push('/');
        out.push_str(&self.tag);
        out.push_str(&format!("({})", self.pid));
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogCatMessage {
    header: LogCatHeader,
    pub message: String,
}
impl LogCatMessage {
    pub fn new(header: LogCatHeader, message: String) -> Self {
        Self { header, message }
    }
    pub fn header(&self) -> &LogCatHeader {
        &self.header
    }
    pub fn log_level(&self) -> LogLevel {
        self.header.log_level
    }
    pub fn pid(&self) -> i32 {
        self.header.pid
    }
    pub fn tid(&self) -> i32 {
        self.header.tid
    }
    pub fn app_name(&self) -> &str {
        &self.header.app_name
    }
    pub fn tag(&self) -> &str {
        &self.header.tag
    }
    pub fn timestamp(&self) -> &str {
        &self.header.timestamp
    }
    /// Kotlin `toString()`: `"$header: $message"`.
    pub fn to_kotlin_string(&self) -> String {
        let mut out = self.header.to_kotlin_string();
        out.push_str(": ");
        out.push_str(&self.message);
        out
    }
}

/// Stateful `logcat -v long` line parser; `previous_header` persists across
/// calls, which `LogcatManager` (WP 6.5) relies on when feeding chunks.
#[derive(Clone, Debug, Default)]
pub struct LogCatMessageParser {
    pub previous_header: Option<LogCatHeader>,
}
impl LogCatMessageParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses one header line, or returns None when it does not match the
    /// whole-line header grammar (which has no year — see U26).
    pub fn process_log_header(&mut self, line: &str) -> Option<LogCatHeader> {
        let text = line;
        let captures = LOGCAT_HEADER.find(text)?;
        let whole = captures.get(0)?;
        // Java Matcher.matches(): the match must cover the whole line.
        if whole.start() != 0 || whole.end() != text.len() {
            return None;
        }
        let group = |index: usize| captures.get(index).map(|m| m.as_str());
        let header = LogCatHeader {
            log_level: Self::parse_priority(group(4).unwrap_or_default()),
            pid: Self::parse_process_id(group(2).unwrap_or_default()),
            tid: Self::parse_thread_id(group(3).unwrap_or_default()),
            app_name: Self::get_package_name(),
            tag: String::from(group(5).unwrap_or_default()),
            timestamp: String::from(group(1).unwrap_or_default()),
        };
        self.previous_header = Some(header.clone());
        Some(header)
    }

    /// Empty lines are skipped, so a multi-line message becomes one
    /// `LogCatMessage` per line under the same header; a line before any
    /// header is dropped (the KDoc's exception claim is stale).
    pub fn process_log_lines(&mut self, lines: &[String]) -> Vec<LogCatMessage> {
        let mut messages = Vec::with_capacity(lines.len());
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if self.process_log_header(line).is_none() {
                let Some(header) = &self.previous_header else { continue };
                messages.push(LogCatMessage::new(header.clone(), line.clone()));
            }
        }
        messages
    }

    pub fn parse_process_id(string: &str) -> i32 {
        jnum::parse_int(string).unwrap_or(-1)
    }

    /// `Integer.decode`: `0x`/`#` hex and leading-`0` octal are honoured.
    pub fn parse_thread_id(string: &str) -> i32 {
        jnum::decode_int(string).unwrap_or(-1)
    }

    /// Unknown letters fall back to WARN, except exactly `"F"`, which is ASSERT.
    pub fn parse_priority(string: &str) -> LogLevel {
        if let Some(level) = LogLevel::get_by_letter_string(&String::from(string)) {
            return level;
        }
        if string != "F" {
            LogLevel::Warn
        } else {
            LogLevel::Assert
        }
    }

    /// Kotlin `getPackageName` is a stub: every header's app name is `"?"`,
    /// which is observable in the bridged device log lines (WP 6.5).
    pub fn get_package_name() -> String {
        "?".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcompat::jstring;

    // No Kotlin tests exist for the logcat parser; these are new tests
    // derived from the Kotlin source (WP 6.3 findings), not ports.

    fn js(s: &str) -> String {
        s.into()
    }

    fn lines_of(text: &str) -> Vec<String> {
        jstring::lines(text)
    }

    const HEADER: &str = "[ 08-18 16:39:11.760  2977: 2988 D/PhoneInterfaceManager ]";

    #[test]
    fn header_line_parses_all_fields() {
        let mut parser = LogCatMessageParser::new();
        let header = parser.process_log_header(&js(HEADER)).expect("header");
        assert_eq!(header.timestamp, js("08-18 16:39:11.760"));
        assert_eq!(header.pid, 2977);
        assert_eq!(header.tid, 2988);
        assert_eq!(header.log_level, LogLevel::Debug);
        assert_eq!(header.tag, js("PhoneInterfaceManager"));
        assert_eq!(header.app_name, js("?"));
        assert_eq!(parser.previous_header, Some(header));
    }

    #[test]
    fn header_with_year_is_rejected() {
        // U26: the fake-adb logcat.txt headers carry a year; the grammar has
        // none, so every such line is rejected as a header.
        let mut parser = LogCatMessageParser::new();
        assert_eq!(
            parser.process_log_header(&js("[ 2026-10-12 12:00:00.000  1234: 1234 I/TestRunner ]")),
            None
        );
        assert_eq!(parser.process_log_header(&js("not a header")), None);
        assert_eq!(parser.previous_header, None);
    }

    #[test]
    fn fake_adb_logcat_stream_yields_no_messages() {
        const FIXTURE: &str = include_str!("../../../fixtures/fake-adb/android-two-devices/script/logcat.txt");
        let mut parser = LogCatMessageParser::new();
        assert_eq!(parser.process_log_lines(&lines_of(FIXTURE)), Vec::new());
    }

    #[test]
    fn corrected_logcat_long_stream_parses_line_per_message() {
        const FIXTURE: &str = include_str!("../../../fixtures/adam/logcat-long.txt");
        let mut parser = LogCatMessageParser::new();
        let messages = parser.process_log_lines(&lines_of(FIXTURE));
        let summary: Vec<(String, LogLevel, String)> = messages
            .iter()
            .map(|m| (m.tag().to_owned(), m.log_level(), m.message.to_owned()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "PhoneInterfaceManager".into(),
                    LogLevel::Debug,
                    "iccPhonebookInterfaceManager.execute".into()
                ),
                (
                    "TestRunner".into(),
                    LogLevel::Info,
                    "started: testText(com.example.MainActivityTest)".into()
                ),
                ("AndroidRuntime".into(), LogLevel::Error, "FATAL EXCEPTION: main".into()),
                // The second line of the error is its own message, never joined.
                (
                    "AndroidRuntime".into(),
                    LogLevel::Error,
                    "java.lang.AssertionError: expected:<1> but was:<2>".into()
                ),
                (
                    "AndroidRuntime".into(),
                    LogLevel::Warn,
                    "Ignoring duplicate failure".into()
                ),
                (
                    "TestRunner".into(),
                    LogLevel::Info,
                    "finished: testText(com.example.MainActivityTest)".into()
                ),
            ]
        );
    }

    #[test]
    fn blank_lines_vanish_and_lines_before_any_header_drop() {
        let mut parser = LogCatMessageParser::new();
        let messages = parser.process_log_lines(&lines_of(&format!("orphan\n{HEADER}\nfirst\n\nsecond")));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message, js("first"));
        assert_eq!(messages[1].message, js("second"));
        assert_eq!(messages[0].header(), messages[1].header());
    }

    #[test]
    fn previous_header_persists_across_calls() {
        let mut parser = LogCatMessageParser::new();
        assert_eq!(parser.process_log_lines(&lines_of(HEADER)), Vec::new());
        let messages = parser.process_log_lines(&lines_of("later chunk"));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tag(), &js("PhoneInterfaceManager"));
    }

    #[test]
    fn oversized_pid_in_header_becomes_minus_one() {
        let mut parser = LogCatMessageParser::new();
        let header = parser
            .process_log_header(&js("[ 08-18 16:39:11.760  9999999999: 2988 D/Tag ]"))
            .expect("header");
        assert_eq!(header.pid, -1);
    }

    #[test]
    fn fatal_priority_letter_in_header_maps_to_assert() {
        let mut parser = LogCatMessageParser::new();
        let header = parser
            .process_log_header(&js("[ 08-18 16:39:11.760  2977: 2988 F/Tag ]"))
            .expect("header");
        assert_eq!(header.log_level, LogLevel::Assert);
    }

    #[test]
    fn parse_process_id_falls_back_to_minus_one() {
        assert_eq!(LogCatMessageParser::parse_process_id("2977"), 2977);
        assert_eq!(LogCatMessageParser::parse_process_id("9999999999"), -1);
        assert_eq!(LogCatMessageParser::parse_process_id("x"), -1);
    }

    #[test]
    fn parse_thread_id_uses_integer_decode() {
        // Reference JBR oracle rows for Integer.decode (WP 6.3 findings).
        assert_eq!(LogCatMessageParser::parse_thread_id("2988"), 2988);
        assert_eq!(LogCatMessageParser::parse_thread_id("0x1A"), 26);
        assert_eq!(LogCatMessageParser::parse_thread_id("010"), 8);
        assert_eq!(LogCatMessageParser::parse_thread_id("08"), -1);
        assert_eq!(LogCatMessageParser::parse_thread_id("abc"), -1);
        assert_eq!(LogCatMessageParser::parse_thread_id("9999999999"), -1);
    }

    #[test]
    fn parse_priority_unknown_letters_fall_back_to_warn() {
        assert_eq!(LogCatMessageParser::parse_priority("E"), LogLevel::Error);
        assert_eq!(LogCatMessageParser::parse_priority("V"), LogLevel::Verbose);
        assert_eq!(LogCatMessageParser::parse_priority("F"), LogLevel::Assert);
        assert_eq!(LogCatMessageParser::parse_priority("X"), LogLevel::Warn);
        assert_eq!(LogCatMessageParser::parse_priority(""), LogLevel::Warn);
        // The F special case compares the whole string, so "Fx" is WARN.
        assert_eq!(LogCatMessageParser::parse_priority("Fx"), LogLevel::Warn);
    }

    #[test]
    fn log_level_values_and_letter_lookup() {
        assert_eq!(
            [
                LogLevel::Verbose,
                LogLevel::Debug,
                LogLevel::Info,
                LogLevel::Warn,
                LogLevel::Error,
                LogLevel::Assert
            ]
            .map(|level| (level.priority(), level.string_value(), level.priority_letter())),
            [
                (2, "verbose", u16::from(b'V')),
                (3, "debug", u16::from(b'D')),
                (4, "info", u16::from(b'I')),
                (5, "warn", u16::from(b'W')),
                (6, "error", u16::from(b'E')),
                (7, "assert", u16::from(b'A')),
            ]
        );
        assert_eq!(LogLevel::get_by_letter_string(&js("Error")), Some(LogLevel::Error));
        assert_eq!(LogLevel::get_by_letter_string(&js("")), None);
        assert_eq!(LogLevel::get_by_letter_string(&js("F")), None);
    }

    #[test]
    fn header_and_message_to_string_forms() {
        let header = LogCatHeader {
            log_level: LogLevel::Debug,
            pid: 2977,
            tid: 2988,
            app_name: js("?"),
            tag: js("PhoneInterfaceManager"),
            timestamp: js("08-18 16:39:11.760"),
        };
        assert_eq!(
            header.to_kotlin_string(),
            js("08-18 16:39:11.760: D/PhoneInterfaceManager(2977)")
        );
        let message = LogCatMessage::new(header, js("hello"));
        assert_eq!(
            message.to_kotlin_string(),
            js("08-18 16:39:11.760: D/PhoneInterfaceManager(2977): hello")
        );
    }
}
