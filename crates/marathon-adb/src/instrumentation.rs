//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/testrunner/transform/InstrumentationResponseTransformer.kt`
//! (which declares package `com.malinskiy.adam.request.transform`), over the
//! `ProgressiveResponseTransformer<List<TestEvent>?>` shape of
//! `.../request/transform/ProgressiveResponseTransformer.kt`.
//! adam portions: Copyright (C) 2021 Anton Malinskiy; Apache-2.0.
//!
//! `process` returns the throw-equivalent error where Kotlin lets a Java
//! exception escape (a non-numeric `numtests`, substring failures on
//! malformed atoms); it never panics on input.

use crate::testrunner::{MetricMap, Status, TestEvent, TestIdentifier, TokenType};
use jcompat::{jnum, jstring};

/// Where Kotlin's `process` throws out of the transformer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstrumentationError {
    /// `parameters["numtests"]?.toInt()` — `NumberFormatException`, not `toIntOrNull`.
    InvalidNumTests(String),
    /// `StringIndexOutOfBoundsException`: a status/result code line with no
    /// value character at all, or a parameter piece with no `=` (adam's
    /// `toMap` indexes `substring(0, -1)` then; see WP 6.3 findings).
    MalformedAtom(String),
}
impl std::fmt::Display for InstrumentationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidNumTests(v) => write!(f, "invalid numtests value: {}", display(v)),
            Self::MalformedAtom(v) => write!(f, "malformed instrumentation atom: {}", display(v)),
        }
    }
}
impl std::error::Error for InstrumentationError {}

/// A non-fatal parser warning, drained via
/// [`InstrumentationResponseTransformer::take_warnings`]. The transformer is
/// a pure parser with no logger sink; 6.6a formats these (see `Display`) and
/// routes them to the marathon log at WARN.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstrumentationWarning {
    /// P2: an unknown `INSTRUMENTATION_STATUS_CODE` (raw, trimmed code text
    /// as it appeared, which need not be numeric); the atom was ignored.
    UnknownStatusCode(String),
}

impl InstrumentationWarning {
    /// The raw status-code text of an unknown-status-code warning.
    pub fn code(&self) -> &str {
        match self {
            Self::UnknownStatusCode(code) => code,
        }
    }
}

impl std::fmt::Display for InstrumentationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStatusCode(code) => {
                write!(
                    f,
                    "unknown INSTRUMENTATION_STATUS_CODE: {}, atom ignored",
                    display(code)
                )
            }
        }
    }
}

fn display(text: &str) -> String {
    String::from_utf8_lossy(text.as_bytes()).into_owned()
}

const KNOWN_KEYS: [&str; 9] = [
    "test", "class", "stack", "numtests", "Error", "shortMsg", "stream", "current", "id",
];

/// Stateful chunk parser for `am instrument -r` output. Feed shell chunks to
/// `process`, then call `transform` once when the stream ends.
#[derive(Clone, Debug, Default)]
pub struct InstrumentationResponseTransformer {
    buffer: String,
    start_reported: bool,
    finish_reported: bool,
    tests_expected: i32,
    tests_executed: i32,
    test_metrics: MetricMap,
    warnings: Vec<InstrumentationWarning>,
    /// An `INSTRUMENTATION_CODE` atom was parsed. Not `finish_reported`, which `transform` sets too.
    session_ended: bool,
    /// `am` said it could not start the instrumentation, see [`Self::ended_without_result`].
    start_refused: bool,
    test_started: bool,
}

impl InstrumentationResponseTransformer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Kotlin `buffer` is public; nothing in marathon reads it.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Drain the warnings accumulated since the last call (P2 unknown status
    /// codes). Warnings survive an `Err` return from `process` for atoms
    /// parsed before the failing one.
    pub fn take_warnings(&mut self) -> Vec<InstrumentationWarning> {
        std::mem::take(&mut self.warnings)
    }

    pub fn process(&mut self, bytes: &[u8]) -> Result<Option<Vec<TestEvent>>, InstrumentationError> {
        // Java String(bytes, UTF_8) also decodes each chunk independently,
        // so a multi-byte character split across chunks is lost in both.
        // The malformed-input replacement itself differs, though: Java's
        // decoder and from_utf8_lossy can emit a different number of U+FFFD
        // replacement characters for the same invalid byte sequence.
        // Matching Java's count is an approved non-goal (WP 6.3 findings,
        // R1-sol-3); per-chunk lossy decoding stays.
        self.buffer.push_str(&String::from_utf8_lossy(bytes));
        // Before the atoms below take the text away. Over the buffer, as a chunk can end inside the word.
        self.start_refused |= self.buffer.contains("INSTRUMENTATION_FAILED");

        let mut result: Option<Vec<TestEvent>> = None;
        while let Some(atom) = self.find_atom() {
            if let Some(events) = self.parse(&atom)? {
                result.get_or_insert_with(Vec::new).extend(events);
            }
        }
        Ok(result)
    }

    fn find_atom(&mut self) -> Option<Vec<String>> {
        let token_position = [
            self.buffer.find(TokenType::InstrumentationStatusCode.name()),
            self.buffer.find(TokenType::InstrumentationCode.name()),
        ]
        .into_iter()
        .flatten()
        .min()?;
        let next_line_break = self.buffer[token_position..].find(['\n', '\r'])? + token_position;
        let atom = jstring::lines(&self.buffer[..next_line_break]);
        self.buffer.drain(..next_line_break + 1);
        Some(atom)
    }

    pub fn transform(&mut self) -> Option<Vec<TestEvent>> {
        if self.finish_reported {
            return None;
        }
        // P3 (approved deviation): Kotlin leaves finishReported unset in both
        // failure branches, so a second transform() re-emits (and re-prepends
        // "No test results\n"). Here a repeated finish reports nothing.
        self.finish_reported = true;
        if !self.start_reported {
            let text = format!("No test results\n{}", self.buffer);
            Some(vec![TestEvent::TestRunFailed {
                error: jstring::trim(&text).to_owned(),
            }])
        } else if self.tests_expected > self.tests_executed {
            Some(vec![TestEvent::TestRunFailed {
                error: format!(
                    "Test run failed to complete. Expected {} tests, executed {}",
                    self.tests_expected, self.tests_executed
                ),
            }])
        } else {
            Some(vec![TestEvent::TestRunEnded {
                elapsed_time_millis: 0,
                metrics: MetricMap::new(),
            }])
        }
    }

    /// D25: whether the output stopped before the instrumentation's end, which `transform` cannot say: it calls such a
    /// stream failed only if it lacks tests, and fine otherwise (N38). `am instrument -r` ends every instrumentation
    /// that ran, crashed or not, with `INSTRUMENTATION_CODE`, last of all, so a stream without one was cut off.
    ///
    /// Not cut off, and still answered by `transform`:
    /// - `am` refusing to start the instrumentation, which prints `INSTRUMENTATION_FAILED` and no code. Real devices
    ///   mix it with a status atom holding an `Error`, in either order. No test has started then;
    /// - output that never was the protocol, which `No test results` then quotes. An unfinished last line that could
    ///   still become the protocol's first word doesn't count: `I` or `INSTRUMENTATION` was cut. Other text followed by
    ///   a cut can't be told from this on a v1 shell.
    ///
    /// A final code line without its line break still counts, as a recording can end that way (adam's `log_13`), but
    /// only whole: `INSTRUMENTATION_CODE: <int>`. The word alone, or with no number yet, is a cut inside that line, and a
    /// runner that omits `numtests` would otherwise pass its partial list.
    pub fn ended_without_result(&self) -> bool {
        let last_line = self.buffer.rsplit(['\n', '\r']).next().unwrap_or_default();
        if self.session_ended || (self.start_refused && !self.test_started) || whole_code_line(last_line) {
            return false;
        }
        let blank = self.buffer.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
        let unfinished = self.buffer.rsplit('\n').next().unwrap_or_default();
        let first_word = !unfinished.is_empty() && "INSTRUMENTATION_".starts_with(unfinished);
        self.start_reported || blank || first_word || self.buffer.contains("INSTRUMENTATION_")
    }

    fn parse(&mut self, atom: &[String]) -> Result<Option<Vec<TestEvent>>, InstrumentationError> {
        let last = &atom[atom.len() - 1];
        if last.starts_with(TokenType::InstrumentationStatusCode.name()) {
            self.parse_status_code(last, &atom[..atom.len() - 1])
        } else if last.starts_with(TokenType::InstrumentationCode.name()) {
            self.parse_instrumentation_code(last, atom)
        } else {
            // INSTRUMENTATION_FAILED and anything else: dropped, no events.
            Ok(None)
        }
    }

    fn parse_status_code(
        &mut self,
        last: &str,
        atom: &[String],
    ) -> Result<Option<Vec<TestEvent>>, InstrumentationError> {
        let value_units = last
            .get(TokenType::InstrumentationStatusCode.name().len() + 1..)
            .ok_or_else(|| InstrumentationError::MalformedAtom(last.to_string()))?;
        let code = to_int_or_null(value_units);
        let status = Status::value_of(code);
        let parameters = to_map(atom, "INSTRUMENTATION_STATUS: ")?;
        self.start_refused |= parameters.get_str("Error").is_some() && test_id(&parameters).is_none();

        let mut events: Vec<TestEvent> = Vec::new();
        if !self.start_reported {
            if let Some(numtests) = parameters.get_str("numtests") {
                let tests = to_int_exact(numtests)
                    .ok_or_else(|| InstrumentationError::InvalidNumTests(numtests.to_string()))?;
                events.push(TestEvent::TestRunStartedEvent { test_count: tests });
                self.tests_expected = tests;
            }
            self.start_reported = true;
        }

        match status {
            Status::Success => {
                if let Some(id) = test_id(&parameters) {
                    events.push(TestEvent::TestEnded {
                        id,
                        metrics: std::mem::take(&mut self.test_metrics),
                    });
                }
                self.tests_executed += 1;
            }
            Status::Start => {
                if let Some(id) = test_id(&parameters) {
                    self.test_started = true;
                    events.push(TestEvent::TestStarted { id });
                }
            }
            Status::InProgress => {
                for (key, value) in parameters.entries() {
                    if !is_known_key(key) {
                        self.test_metrics.insert(key.clone(), value.clone());
                    }
                }
            }
            Status::Error | Status::Failure => {
                if let Some(stack) = parameters.get_str("stack") {
                    let stack = stack.to_owned();
                    match test_id(&parameters) {
                        Some(id) => {
                            events.push(TestEvent::TestFailed {
                                id: id.clone(),
                                stack_trace: stack.to_owned(),
                            });
                            events.push(TestEvent::TestEnded {
                                id,
                                metrics: std::mem::take(&mut self.test_metrics),
                            });
                        }
                        None => events.push(TestEvent::TestRunFailing {
                            error: parameters
                                .get_str("stream")
                                .map(str::to_owned)
                                .unwrap_or_else(|| String::from("Unexpected failure during execution")),
                            stack_trace: stack.to_owned(),
                        }),
                    }
                }
                self.tests_executed += 1;
            }
            Status::Ignored => {
                if let Some(id) = test_id(&parameters) {
                    events.push(TestEvent::TestIgnored { id: id.clone() });
                    events.push(TestEvent::TestEnded {
                        id,
                        metrics: std::mem::take(&mut self.test_metrics),
                    });
                }
                self.tests_executed += 1;
            }
            Status::AssumptionFailure => {
                if let (Some(id), Some(stack)) = (test_id(&parameters), parameters.get_str("stack")) {
                    let stack = stack.to_owned();
                    events.push(TestEvent::TestAssumptionFailed {
                        id: id.clone(),
                        stack_trace: stack.to_owned(),
                    });
                    events.push(TestEvent::TestEnded {
                        id,
                        metrics: std::mem::take(&mut self.test_metrics),
                    });
                }
                self.tests_executed += 1;
            }
            Status::Unknown => {
                // P2 (approved deviation): adam has Status.UNKNOWN -> TODO(),
                // which kills the run. Ignore the atom instead, counting
                // nothing, and queue a warning for the caller to log at WARN
                // (6.6a owns logging; see take_warnings).
                self.warnings.push(InstrumentationWarning::UnknownStatusCode(
                    jstring::trim(value_units).to_owned(),
                ));
            }
        }

        Ok((!events.is_empty()).then_some(events))
    }

    fn parse_instrumentation_code(
        &mut self,
        last: &str,
        atom: &[String],
    ) -> Result<Option<Vec<TestEvent>>, InstrumentationError> {
        let value_units = last
            .get(TokenType::InstrumentationCode.name().len() + 1..)
            .ok_or_else(|| InstrumentationError::MalformedAtom(last.to_string()))?;
        let code = to_int_or_null(value_units);
        self.session_ended = true;
        // Session result code -1 is Status.ERROR by value and means success.
        if Status::value_of(code) == Status::Error {
            let lines: Vec<String> = atom.iter().filter(|line| *line != last).cloned().collect();
            let parameters = to_map(&lines, "INSTRUMENTATION_RESULT:")?;
            let mut time = 0i64;
            if let Some(stream) = parameters.get_str("stream") {
                for line in jstring::lines(stream) {
                    if line.starts_with("Time: ") {
                        time = parse_time(&line[6..]);
                    }
                }
            }
            let metrics: MetricMap = parameters
                .entries()
                .iter()
                .filter(|(key, _)| !is_known_key(key))
                .cloned()
                .collect();
            self.finish_reported = true;
            Ok(Some(vec![TestEvent::TestRunEnded {
                elapsed_time_millis: time,
                metrics,
            }]))
        } else {
            self.finish_reported = true;
            let mut short_message: Option<String> = None;
            let mut time = 0i64;
            for line in atom {
                if line.starts_with("INSTRUMENTATION_RESULT: shortMsg=") {
                    short_message = Some(line[33..].to_owned());
                } else if line.starts_with("Time: ") {
                    time = parse_time(&line[6..]);
                }
            }
            Ok(Some(vec![
                TestEvent::TestRunFailed {
                    error: short_message.unwrap_or_else(|| {
                        format!(
                            "Unexpected INSTRUMENTATION_CODE: {}",
                            code.map_or_else(|| "null".to_owned(), |c| c.to_string())
                        )
                    }),
                },
                TestEvent::TestRunEnded {
                    elapsed_time_millis: time,
                    metrics: MetricMap::new(),
                },
            ]))
        }
    }
}

fn test_id(parameters: &MetricMap) -> Option<TestIdentifier> {
    Some(TestIdentifier {
        class_name: parameters.get_str("class")?.to_string(),
        test_name: parameters.get_str("test")?.to_string(),
    })
}

fn is_known_key(key: &str) -> bool {
    KNOWN_KEYS.contains(&key)
}

/// Kotlin's double-to-long cast truncates, saturates, and maps NaN to zero.
fn parse_time(text: &str) -> i64 {
    jnum::parse_double(text).map_or(0, |seconds| (seconds * 1000.0) as i64)
}

fn to_int_exact(text: &str) -> Option<i32> {
    jnum::parse_int(text)
}

fn to_int_or_null(text: &str) -> Option<i32> {
    jnum::parse_int(jstring::trim(text))
}

fn whole_code_line(line: &str) -> bool {
    let token = TokenType::InstrumentationCode.name();
    line.strip_prefix(token)
        .and_then(|s| s.strip_prefix(':'))
        .and_then(to_int_or_null)
        .is_some()
}

/// adam's `List<String>.toMap`: non-empty lines rejoined, split on the
/// delimiter, each piece split at its first `=`; pieces with nothing after
/// the `=` are dropped. A piece with no `=` at all makes Kotlin compute
/// `substring(0, -1)` and throw (empty pieces are dropped first).
fn to_map(lines: &[String], delimiter: &str) -> Result<MetricMap, InstrumentationError> {
    let joined = lines
        .iter()
        .filter(|line| !line.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    let mut map = MetricMap::new();
    for piece in joined.split(delimiter) {
        let trimmed = jstring::trim(piece);
        let Some((key, value)) = trimmed.split_once('=') else {
            if trimmed.is_empty() {
                continue;
            }
            return Err(InstrumentationError::MalformedAtom(trimmed.to_owned()));
        };
        if !value.is_empty() {
            map.insert(key.to_owned(), value.to_owned());
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcompat::writers::KotlinValue;

    // Port of adam 0.5.8 InstrumentationResponseTransformerTest: the Kotlin
    // tests compare event data-class toString() lines against .expected
    // files, so events are rendered with jcompat's KotlinValue (PLAN §6.2).

    fn text(value: &str) -> KotlinValue {
        KotlinValue::Text(value.to_string())
    }
    fn number(value: &str) -> KotlinValue {
        KotlinValue::Text(value.into())
    }
    fn data_class(name: &str, fields: Vec<(&str, KotlinValue)>) -> KotlinValue {
        KotlinValue::DataClass {
            name: name.into(),
            fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        }
    }
    fn id_value(id: &TestIdentifier) -> KotlinValue {
        data_class(
            "TestIdentifier",
            vec![("className", text(&id.class_name)), ("testName", text(&id.test_name))],
        )
    }
    fn metrics_value(metrics: &MetricMap) -> KotlinValue {
        KotlinValue::Map(metrics.entries().iter().map(|(k, v)| (text(k), text(v))).collect())
    }
    fn event_value(event: &TestEvent) -> KotlinValue {
        match event {
            TestEvent::TestRunStartedEvent { test_count } => data_class(
                "TestRunStartedEvent",
                vec![("testCount", number(&test_count.to_string()))],
            ),
            TestEvent::TestStarted { id } => data_class("TestStarted", vec![("id", id_value(id))]),
            TestEvent::TestFailed { id, stack_trace } => data_class(
                "TestFailed",
                vec![("id", id_value(id)), ("stackTrace", text(stack_trace))],
            ),
            TestEvent::TestAssumptionFailed { id, stack_trace } => data_class(
                "TestAssumptionFailed",
                vec![("id", id_value(id)), ("stackTrace", text(stack_trace))],
            ),
            TestEvent::TestIgnored { id } => data_class("TestIgnored", vec![("id", id_value(id))]),
            TestEvent::TestEnded { id, metrics } => data_class(
                "TestEnded",
                vec![("id", id_value(id)), ("metrics", metrics_value(metrics))],
            ),
            TestEvent::TestRunFailed { error } => data_class("TestRunFailed", vec![("error", text(error))]),
            TestEvent::TestRunFailing { error, stack_trace } => data_class(
                "TestRunFailing",
                vec![("error", text(error)), ("stackTrace", text(stack_trace))],
            ),
            TestEvent::TestRunStopped { elapsed_time_millis } => data_class(
                "TestRunStopped",
                vec![("elapsedTimeMillis", number(&elapsed_time_millis.to_string()))],
            ),
            TestEvent::TestRunEnded {
                elapsed_time_millis,
                metrics,
            } => data_class(
                "TestRunEnded",
                vec![
                    ("elapsedTimeMillis", number(&elapsed_time_millis.to_string())),
                    ("metrics", metrics_value(metrics)),
                ],
            ),
        }
    }

    fn rendered(events: &[TestEvent]) -> String {
        events
            .iter()
            .map(|e| event_value(e).render())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn feed_line_by_line(
        transformer: &mut InstrumentationResponseTransformer,
        input: &str,
        ending: &str,
    ) -> Vec<TestEvent> {
        let mut events = Vec::new();
        for line in input.lines() {
            let bytes = format!("{line}{ending}").into_bytes();
            if let Some(e) = transformer.process(&bytes).expect("process") {
                events.extend(e);
            }
        }
        if let Some(e) = transformer.transform() {
            events.extend(e);
        }
        events
    }

    fn feed_single_block(transformer: &mut InstrumentationResponseTransformer, input: &str) -> Vec<TestEvent> {
        let mut events = Vec::new();
        if let Some(e) = transformer.process(input.as_bytes()).expect("process") {
            events.extend(e);
        }
        if let Some(e) = transformer.transform() {
            events.extend(e);
        }
        events
    }

    /// Kotlin readText().trimEnd(): these fixture files end in ASCII newlines.
    fn expected(text: &str) -> &str {
        text.trim_end()
    }

    const LOG_1_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_1.input");
    const LOG_1_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_1.expected");
    const LOG_2_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_2.input");
    const LOG_2_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_2.expected");
    const LOG_3_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_3.input");
    const LOG_3_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_3.expected");
    const LOG_4_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_4.input");
    const LOG_4_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_4.expected");
    const LOG_5_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_5.input");
    const LOG_5_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_5.expected");
    const LOG_6_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_6.input");
    const LOG_6_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_6.expected");
    const LOG_7_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_7.input");
    const LOG_7_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_7.expected");
    const LOG_8_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_8.input");
    const LOG_8_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_8.expected");
    const LOG_9_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_9.input");
    const LOG_9_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_9.expected");
    const LOG_10_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_10.input");
    const LOG_10_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_10.expected");
    const LOG_11_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_11.input");
    const LOG_11_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_11.expected");
    const LOG_12_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_12.input");
    const LOG_12_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_12.expected");
    const LOG_13_INPUT: &str = include_str!("../../../fixtures/adam/instrumentation/log_13.input");
    const LOG_13_EXPECTED: &str = include_str!("../../../fixtures/adam/instrumentation/log_13.expected");

    #[test]
    fn test_single_failure() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_line_by_line(&mut t, LOG_3_INPUT, "\n");
        assert_eq!(rendered(&events), expected(LOG_3_EXPECTED));
    }

    #[test]
    fn test_complex1() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_line_by_line(&mut t, LOG_2_INPUT, "\n");
        assert_eq!(rendered(&events), expected(LOG_2_EXPECTED));
    }

    #[test]
    fn test_complex2() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_line_by_line(&mut t, LOG_1_INPUT, "\n");
        assert_eq!(rendered(&events), expected(LOG_1_EXPECTED));
    }

    #[test]
    fn test_no_results_reported() {
        let mut t = InstrumentationResponseTransformer::new();
        assert_eq!(
            t.transform(),
            Some(vec![TestEvent::TestRunFailed {
                error: "No test results".into()
            }])
        );
    }

    #[test]
    fn test_incomplete_tests() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_line_by_line(&mut t, LOG_4_INPUT, "\n");
        assert_eq!(rendered(&events), expected(LOG_4_EXPECTED));
    }

    #[test]
    fn test_incomplete_tests2() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_line_by_line(&mut t, LOG_5_INPUT, "\n");
        assert_eq!(rendered(&events), expected(LOG_5_EXPECTED));
    }

    #[test]
    fn test_other_failure() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_line_by_line(&mut t, LOG_6_INPUT, "\n");
        assert_eq!(rendered(&events), expected(LOG_6_EXPECTED));
    }

    #[test]
    fn test_buffer_framing() {
        let mut t = InstrumentationResponseTransformer::new();
        let mut events = Vec::new();
        for line in LOG_6_INPUT.lines() {
            let char_count = line.chars().count();
            let cut_char = 7 * char_count / 8;
            let cut = line.char_indices().nth(cut_char).map_or(line.len(), |(i, _)| i);
            for part in [&line[..cut], &format!("{}\n", &line[cut..])] {
                if let Some(e) = t.process(part.as_bytes()).expect("process") {
                    events.extend(e);
                }
            }
        }
        if let Some(e) = t.transform() {
            events.extend(e);
        }
        assert_eq!(rendered(&events), expected(LOG_6_EXPECTED));
    }

    #[test]
    fn test_windows_line_ending() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_line_by_line(&mut t, LOG_3_INPUT, "\r\n");
        assert_eq!(rendered(&events), expected(LOG_3_EXPECTED));
    }

    #[test]
    fn test_single_block() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, LOG_7_INPUT);
        assert_eq!(rendered(&events), expected(LOG_7_EXPECTED));
    }

    #[test]
    fn test_failure_handling() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, LOG_8_INPUT);
        assert_eq!(rendered(&events), expected(LOG_8_EXPECTED));
    }

    #[test]
    fn test_metrics() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, LOG_9_INPUT);
        assert_eq!(rendered(&events), expected(LOG_9_EXPECTED));
    }

    #[test]
    fn test_run_metrics() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, LOG_10_INPUT);
        assert_eq!(rendered(&events), expected(LOG_10_EXPECTED));
    }

    #[test]
    fn test_single_test_metric() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, LOG_11_INPUT);
        assert_eq!(rendered(&events), expected(LOG_11_EXPECTED));
    }

    #[test]
    fn test_single_ignored_test() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, LOG_12_INPUT);
        assert_eq!(rendered(&events), expected(LOG_12_EXPECTED));
    }

    #[test]
    fn missing_listener_test() {
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, LOG_13_INPUT);
        assert_eq!(rendered(&events), expected(LOG_13_EXPECTED));
    }

    // New tests (not adam ports): rules the brief pins beyond the fixtures.

    #[test]
    fn byte_at_a_time_feeding_matches_block_feeding() {
        // Same stream the Kotlin line harness feeds: readLines() drops the
        // terminators and each line is re-fed with '\n' appended, including
        // after the final line (log_1.input has no trailing newline, and an
        // atom only frames at a line break).
        let stream: String = LOG_1_INPUT.lines().map(|line| format!("{line}\n")).collect();
        let mut t = InstrumentationResponseTransformer::new();
        let mut events = Vec::new();
        for byte in stream.as_bytes() {
            if let Some(e) = t.process(std::slice::from_ref(byte)).expect("process") {
                events.extend(e);
            }
        }
        if let Some(e) = t.transform() {
            events.extend(e);
        }
        assert_eq!(rendered(&events), expected(LOG_1_EXPECTED));
    }

    #[test]
    fn unknown_status_code_is_ignored_and_counts_nothing() {
        // P2: adam crashes with TODO() here; the port drops the atom.
        let input = "INSTRUMENTATION_STATUS: numtests=1\n\
                     INSTRUMENTATION_STATUS: test=a\n\
                     INSTRUMENTATION_STATUS: class=C\n\
                     INSTRUMENTATION_STATUS_CODE: 7\n\
                     INSTRUMENTATION_STATUS: test=a\n\
                     INSTRUMENTATION_STATUS: class=C\n\
                     INSTRUMENTATION_STATUS_CODE: 0\n";
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, input);
        assert_eq!(
            events,
            vec![
                TestEvent::TestRunStartedEvent { test_count: 1 },
                TestEvent::TestEnded {
                    id: TestIdentifier {
                        class_name: "C".into(),
                        test_name: "a".into()
                    },
                    metrics: MetricMap::new(),
                },
                TestEvent::TestRunEnded {
                    elapsed_time_millis: 0,
                    metrics: MetricMap::new()
                },
            ]
        );
    }

    #[test]
    fn unknown_status_code_warning_is_drained_through_the_public_api() {
        // R1 (review round 1): no stderr side channel; 6.6a drains warnings
        // and routes Display's exact text to the marathon log at WARN.
        let mut t = InstrumentationResponseTransformer::new();
        let started = t
            .process(b"INSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n")
            .expect("process");
        assert_eq!(started, Some(vec![TestEvent::TestRunStartedEvent { test_count: 1 }]));
        assert_eq!(t.take_warnings(), Vec::new());

        // A lone unknown atom keeps the null (None) event semantics, even
        // when the raw code is not numeric.
        assert_eq!(
            t.process(b"INSTRUMENTATION_STATUS_CODE: bogus\n").expect("process"),
            None
        );
        let warnings = t.take_warnings();
        assert_eq!(
            warnings,
            vec![InstrumentationWarning::UnknownStatusCode("bogus".into())]
        );
        assert_eq!(warnings[0].code(), &String::from("bogus"));
        assert_eq!(
            warnings[0].to_string(),
            "unknown INSTRUMENTATION_STATUS_CODE: bogus, atom ignored"
        );
        assert_eq!(t.take_warnings(), Vec::new(), "draining empties the queue");

        // Several unknown atoms in one chunk queue in order.
        assert_eq!(
            t.process(b"INSTRUMENTATION_STATUS_CODE: 7\nINSTRUMENTATION_STATUS_CODE: 8\n")
                .expect("process"),
            None
        );
        assert_eq!(
            t.take_warnings(),
            vec![
                InstrumentationWarning::UnknownStatusCode("7".into()),
                InstrumentationWarning::UnknownStatusCode("8".into()),
            ]
        );
    }

    #[test]
    fn non_numeric_numtests_is_an_error_not_a_panic() {
        // Kotlin reads numtests with toInt(): the throw escapes process().
        let mut t = InstrumentationResponseTransformer::new();
        let input = b"INSTRUMENTATION_STATUS: numtests=many\nINSTRUMENTATION_STATUS_CODE: 1\n";
        assert_eq!(
            t.process(input),
            Err(InstrumentationError::InvalidNumTests("many".into()))
        );
    }

    #[test]
    fn numtests_with_a_leading_space_is_an_error_not_a_panic() {
        // R1-sol-2: Kotlin `toInt()` does not trim, so `numtests= 1` throws
        // NumberFormatException; the trimmed `toIntOrNull` path accepted it.
        let mut t = InstrumentationResponseTransformer::new();
        let input = b"INSTRUMENTATION_STATUS: numtests= 1\nINSTRUMENTATION_STATUS_CODE: 1\n";
        assert_eq!(
            t.process(input),
            Err(InstrumentationError::InvalidNumTests(" 1".into()))
        );
    }

    #[test]
    fn parameter_piece_without_equals_is_an_error_not_a_panic() {
        // Garbage before the first INSTRUMENTATION_STATUS piece reaches
        // adam's substring(0, -1) (WP 6.3 findings, incidental adam bug).
        let mut t = InstrumentationResponseTransformer::new();
        let input = b"garbage line\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n";
        assert_eq!(
            t.process(input),
            Err(InstrumentationError::MalformedAtom("garbage line".into()))
        );
    }

    #[test]
    fn repeated_transform_after_failure_reports_nothing() {
        // P3: Kotlin re-emits (re-prepending "No test results\n") on a second call.
        let mut t = InstrumentationResponseTransformer::new();
        assert!(t.transform().is_some());
        assert_eq!(t.transform(), None);
    }

    #[test]
    fn time_is_seconds_times_1000_truncated() {
        // Reference JBR: (long)(parseDouble(t) * 1000) — 0.0004s becomes 0ms.
        let input = "INSTRUMENTATION_RESULT: stream=\nTime: 0.0004\nINSTRUMENTATION_CODE: -1\n";
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, input);
        assert_eq!(
            events,
            vec![TestEvent::TestRunEnded {
                elapsed_time_millis: 0,
                metrics: MetricMap::new()
            }]
        );
    }

    /// D25: every recording this repository has, by whether its stream reached the instrumentation's end. `log_1` and
    /// `log_4` are adam's fixtures of output cut off, without a code; `started.txt` is the first lines of a stream, for
    /// a test that hangs. Nothing else lacks the code, including crashes and non-zero session codes.
    #[test]
    fn recordings_end_with_their_code_except_the_ones_cut_off_d25() {
        const FAKE: [(&str, &str, bool); 5] = [
            (
                "execution",
                include_str!("../../../fixtures/fake-adb/android-runner-cli/script/execution.txt"),
                true,
            ),
            (
                "discovery",
                include_str!("../../../fixtures/fake-adb/android-runner-cli/script/discovery.txt"),
                true,
            ),
            (
                "crash",
                include_str!("../../../fixtures/fake-adb/android-runner-cli/script/crash.txt"),
                true,
            ),
            (
                "started",
                include_str!("../../../fixtures/fake-adb/android-runner-cli/script/started.txt"),
                false,
            ),
            (
                "two-devices",
                include_str!("../../../fixtures/fake-adb/android-two-devices/script/instrumentation.txt"),
                true,
            ),
        ];
        let adam = [
            ("log_1", LOG_1_INPUT, false),
            ("log_2", LOG_2_INPUT, true),
            ("log_3", LOG_3_INPUT, true),
            ("log_4", LOG_4_INPUT, false),
            ("log_5", LOG_5_INPUT, true),
            ("log_6", LOG_6_INPUT, true),
            ("log_7", LOG_7_INPUT, true),
            ("log_8", LOG_8_INPUT, true),
            ("log_9", LOG_9_INPUT, true),
            ("log_10", LOG_10_INPUT, true),
            ("log_11", LOG_11_INPUT, true),
            ("log_12", LOG_12_INPUT, true),
            ("log_13", LOG_13_INPUT, true),
        ];
        for (name, input, complete) in FAKE.into_iter().chain(adam) {
            let mut t = InstrumentationResponseTransformer::new();
            t.process(input.as_bytes()).expect("process");
            assert_eq!(t.ended_without_result(), !complete, "{name}");
            // The transformer's own answer stays what adam's tests pin.
            assert!(t.transform().is_some() || complete, "{name}");
        }
    }

    /// The ends of `am instrument -w -r` seen on API 33 and 36 emulators, in `docs/wp/x18-findings.md`.
    #[test]
    fn real_refusals_and_crashes_are_not_a_stream_cut_off_d25() {
        const REFUSED: &str = "INSTRUMENTATION_STATUS: Error=Unable to find instrumentation info for: ComponentInfo{com.nope/com.nope.Runner}\nINSTRUMENTATION_STATUS: id=ActivityManagerService\nINSTRUMENTATION_STATUS_CODE: -1\n";
        const ON_ERROR: &str = "onError: commandError=true message=INSTRUMENTATION_FAILED: com.nope/com.nope.Runner\n";
        let started = "INSTRUMENTATION_STATUS: class=C\nINSTRUMENTATION_STATUS: test=a\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n";
        for (name, output) in [
            ("refused, status first", format!("{REFUSED}{ON_ERROR}")),
            ("refused, onError first", format!("{ON_ERROR}{REFUSED}")),
            ("refused, target missing", ON_ERROR.to_owned()),
            (
                "crash",
                format!("{started}INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n"),
            ),
            (
                "code without its line break",
                format!("{started}INSTRUMENTATION_CODE: -1"),
            ),
            ("not the protocol", "/system/bin/sh: am: not found\n".to_owned()),
            ("not the protocol, no line break", "something-something".to_owned()),
        ] {
            let mut t = InstrumentationResponseTransformer::new();
            t.process(output.as_bytes()).expect("process");
            assert!(!t.ended_without_result(), "{name}");
        }
        // Split inside the word, which the first chunk's atom would otherwise take along.
        let mut t = InstrumentationResponseTransformer::new();
        t.process(b"onError: commandError=true message=INSTRUMENTATION_FA")
            .expect("process");
        t.process(b"ILED: x\n").expect("process");
        assert!(!t.ended_without_result());
    }

    #[test]
    fn streams_cut_off_are_told_apart_from_other_output_d25() {
        let started = "INSTRUMENTATION_STATUS: class=C\nINSTRUMENTATION_STATUS: test=a\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n";
        let cut = |output: &str| {
            let mut t = InstrumentationResponseTransformer::new();
            t.process(output.as_bytes()).expect("process");
            t.ended_without_result()
        };
        // No output: a dropped connection, or `am` killed before it printed anything.
        assert!(cut(""));
        // Cut inside the first word, or before it.
        for output in [
            "I",
            "INSTRU",
            "INSTRUMENTATION",
            "\n",
            "\r\n",
            "a warning\nINSTRUMENTATION",
        ] {
            assert!(cut(output), "{output:?}");
        }
        // Cut inside the first atom, before anything was parsed.
        assert!(cut("INSTRUMENTATION_STATUS: numtests=2\nINSTRU"));
        // A test in progress.
        assert!(cut(started));
        // Every test reported: `transform` would call that a success.
        let finished = format!(
            "{started}INSTRUMENTATION_STATUS: class=C\nINSTRUMENTATION_STATUS: test=a\nINSTRUMENTATION_STATUS_CODE: 0\n"
        );
        assert!(cut(&finished));
        // A test printing the word that `am` uses for a refusal does not hide the stream's end.
        assert!(cut(&format!(
            "{started}INSTRUMENTATION_STATUS: stream=INSTRUMENTATION_FAILED: mine\nINSTRUMENTATION_STATUS_CODE: 2\n"
        )));
    }

    /// x37: only a whole code line ends the stream when its line break is missing. Every test reported and no
    /// `numtests` gives `transform` no count to miss, so this is all that tells a cut list from a whole one.
    #[test]
    fn a_stream_cut_inside_its_code_line_is_cut_off_d25() {
        let finished = "INSTRUMENTATION_STATUS: class=C\nINSTRUMENTATION_STATUS: test=a\nINSTRUMENTATION_STATUS_CODE: 1\n\
                        INSTRUMENTATION_STATUS: class=C\nINSTRUMENTATION_STATUS: test=a\nINSTRUMENTATION_STATUS_CODE: 0\n";
        let cut = |end: &str| {
            let mut t = InstrumentationResponseTransformer::new();
            t.process(format!("{finished}{end}").as_bytes()).expect("process");
            t.ended_without_result()
        };
        for end in [
            "INSTRUMENTATION_CODE",
            "INSTRUMENTATION_CODE:",
            "INSTRUMENTATION_CODE: ",
            "INSTRUMENTATION_CODE: -",
            "INSTRUMENTATION_RESULT: stream=\nINSTRUMENTATION_CODE",
            "text INSTRUMENTATION_CODE: -1",
        ] {
            assert!(cut(end), "{end:?}");
        }
        for end in [
            "INSTRUMENTATION_CODE: -1",
            "INSTRUMENTATION_CODE: 0",
            "INSTRUMENTATION_CODE: -1 ",
        ] {
            assert!(!cut(end), "{end:?}");
        }
        // Split across chunks, the code still counts once whole.
        let mut t = InstrumentationResponseTransformer::new();
        t.process(format!("{finished}INSTRUMENTATION_CODE").as_bytes())
            .expect("process");
        t.process(b": -1").expect("process");
        assert!(!t.ended_without_result());
    }

    #[test]
    fn fake_adb_instrumentation_stream_produces_the_kotlin_run_events() {
        // The WP 0.5 fake-adb stream: one pass, one failure, Time: 1.5.
        const FIXTURE: &str = include_str!("../../../fixtures/fake-adb/android-two-devices/script/instrumentation.txt");
        let mut t = InstrumentationResponseTransformer::new();
        let events = feed_single_block(&mut t, FIXTURE);
        let stack = "java.lang.AssertionError: expected:<1> but was:<2>\n\
                     \tat org.junit.Assert.fail(Assert.java:89)\n\
                     \tat org.junit.Assert.failNotEquals(Assert.java:835)\n\
                     \tat org.junit.Assert.assertEquals(Assert.java:647)\n\
                     \tat com.example.MainActivityAllureTest.testFailure(MainActivityAllureTest.kt:30)";
        let pass_id = TestIdentifier {
            class_name: "com.example.MainActivityTest".into(),
            test_name: "testText".into(),
        };
        let fail_id = TestIdentifier {
            class_name: "com.example.MainActivityAllureTest".into(),
            test_name: "testFailure".into(),
        };
        assert_eq!(
            events,
            vec![
                TestEvent::TestRunStartedEvent { test_count: 2 },
                TestEvent::TestStarted { id: pass_id.clone() },
                TestEvent::TestEnded {
                    id: pass_id,
                    metrics: MetricMap::new()
                },
                TestEvent::TestStarted { id: fail_id.clone() },
                TestEvent::TestFailed {
                    id: fail_id.clone(),
                    stack_trace: stack.into()
                },
                TestEvent::TestEnded {
                    id: fail_id,
                    metrics: MetricMap::new()
                },
                TestEvent::TestRunEnded {
                    elapsed_time_millis: 1500,
                    metrics: MetricMap::new()
                },
            ]
        );
    }
}
