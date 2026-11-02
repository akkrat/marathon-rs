//! Port of core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt,
//! core/src/main/kotlin/com/malinskiy/marathon/log/{MarathonLogConfigurator,TimingLog,KLoggerDebug}.kt,
//! cli/src/main/resources/logback.xml, vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/configuration/AndroidLogConfigurator.kt,
//! and vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/{AppleLogConfigurator,XcodebuildStderrLog}.kt.
//!
//! Formatting and filtering return owned lines. The runtime forwards these to its I/O task;
//! neither clock reads nor stream writes occur here. Supply local wall time (including the
//! timezone offset) at the `library.logback.wall` replay seam.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Level {
    All,
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Off,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Off => "OFF",
        }
    }

    fn colour(self) -> &'static str {
        match self {
            Self::Error => "1;31",
            Self::Warn => "31",
            Self::Info => "34",
            _ => "39",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Layout {
    #[default]
    Startup,
    Runtime,
    Compact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Vendor {
    Android,
    Apple { compact_output: bool },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum JansiMode {
    #[default]
    Default,
    Force,
    Strip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTarget {
    Terminal,
    Pipe,
    File,
}

impl JansiMode {
    pub fn preserves_ansi(self, target: OutputTarget) -> bool {
        match self {
            Self::Default => target == OutputTarget::Terminal,
            Self::Force => true,
            Self::Strip => false,
        }
    }
}

/// The three patterns used by marathon, rather than an arbitrary logback pattern parser.
impl Layout {
    pub fn pattern(self) -> &'static str {
        match self {
            Self::Startup => "%d{HH:mm:ss.SSS} [%thread] %-5level %logger{36} - %msg%n",
            Self::Runtime => "%highlight(%.-1level %d{HH:mm:ss.SSS} [%thread] <%logger{40}> %msg%n)",
            Self::Compact => "%highlight(%.-1level [%thread] <%logger{48}> %msg%n)",
        }
    }

    /// Queue this when stopping the appender. Jansi resets terminals even in strip mode.
    pub fn finish(self, target: OutputTarget) -> &'static str {
        if self != Self::Startup && target == OutputTarget::Terminal {
            "\x1b[0m"
        } else {
            ""
        }
    }

    /// `throwable` is the rendered trace, including its final newline. JVM-internal frames
    /// may differ under PLAN §3.10; worker traces remain untouched message text.
    pub fn format(self, event: &LogEvent<'_>, mode: JansiMode, target: OutputTarget) -> String {
        let time = time_of_day(event.local_time_millis);
        let mut line = match self {
            Self::Startup => format!(
                "{time} [{}] {:<5} {} - {}\n",
                event.thread,
                event.level.as_str(),
                abbreviate(event.logger, 36),
                event.message,
            ),
            Self::Runtime | Self::Compact => {
                let date = if self == Self::Runtime {
                    format!("{time} ")
                } else {
                    String::new()
                };
                let length = if self == Self::Runtime { 40 } else { 48 };
                format!(
                    "\x1b[{}m{} {date}[{}] <{}> {}\n\x1b[0;39m",
                    event.level.colour(),
                    &event.level.as_str()[..1],
                    event.thread,
                    abbreviate(event.logger, length),
                    event.message,
                )
            }
        };
        line.push_str(event.throwable);
        // Startup uses the raw stdout appender; only the vendor appender enables Jansi.
        if self != Self::Startup && !mode.preserves_ansi(target) {
            strip_ansi(&line)
        } else {
            line
        }
    }
}

pub struct LogEvent<'a> {
    pub level: Level,
    pub logger: &'a str,
    pub thread: &'a str,
    pub local_time_millis: i64,
    pub message: &'a str,
    pub throwable: &'a str,
}

fn time_of_day(millis: i64) -> String {
    let millis = millis.rem_euclid(86_400_000);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        millis / 3_600_000,
        millis / 60_000 % 60,
        millis / 1000 % 60,
        millis % 1000
    )
}

/// Logback abbreviates whole package segments from the left and never shortens the
/// final segment. `length` is a target, not a maximum, and Java counts UTF-16 units.
pub fn abbreviate(name: &str, length: usize) -> String {
    if length == 0 {
        return name.rsplit('.').next().unwrap_or(name).to_owned();
    }
    let units: Vec<u16> = name.encode_utf16().collect();
    if units.len() < length {
        return name.to_owned();
    }
    let Some(last_dot) = units.iter().rposition(|&u| u == u16::from(b'.')) else {
        return name.to_owned();
    };
    let to_trim = units.len().saturating_sub(length).min(last_dot);
    let mut trimmed = 0;
    let mut first = true;
    let mut out = Vec::new();
    let mut i = 0;
    while i < last_dot {
        let unit = units[i];
        if unit == u16::from(b'.') {
            if trimmed >= to_trim {
                break;
            }
            out.push(unit);
            first = true;
        } else if first {
            out.push(unit);
            first = false;
        } else {
            trimmed += 1;
        }
        i += 1;
    }
    out.extend_from_slice(&units[i..]);
    // Java's UTF-8 encoder replaces an unpaired surrogate left by abbreviation with '?'.
    char::decode_utf16(out).map(|ch| ch.unwrap_or('?')).collect()
}

/// SLF4J substitution receives Java-rendered argument values (including `null`).
/// Interpolated Kotlin messages can be supplied directly to `LogEvent`.
pub fn format_message(pattern: &str, arguments: &[&str]) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    for arg in arguments {
        loop {
            let Some(index) = rest.find("{}") else {
                out.push_str(rest);
                return out;
            };
            if index > 0 && rest.as_bytes()[index - 1] == b'\\' {
                if index > 1 && rest.as_bytes()[index - 2] == b'\\' {
                    out.push_str(&rest[..index - 1]);
                    out.push_str(arg);
                    rest = &rest[index + 2..];
                    break;
                }
                out.push_str(&rest[..index - 1]);
                out.push_str("{}");
                rest = &rest[index + 2..];
            } else {
                out.push_str(&rest[..index]);
                out.push_str(arg);
                rest = &rest[index + 2..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Handle levels stay in the registry so a subsequent lookup also affects old handles.
#[derive(Clone, Debug)]
pub struct Logger {
    name: String,
}

impl Logger {
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug)]
pub struct MarathonLogging {
    pub debug: bool,
    layout: Layout,
    levels: HashMap<String, Level>,
    timings: Option<Logger>,
    xcodebuild: Option<Logger>,
    warning_printed: bool,
}

impl Default for MarathonLogging {
    fn default() -> Self {
        Self {
            debug: true,
            layout: Layout::Startup,
            levels: HashMap::from([
                ("ROOT".into(), Level::Debug),
                ("io.netty".into(), Level::Error),
                ("io.vertx".into(), Level::Error),
            ]),
            timings: None,
            xcodebuild: None,
            warning_printed: false,
        }
    }
}

impl MarathonLogging {
    /// The caller sends this diagnostic to raw stdout when wrapping a non-logback backend.
    pub fn fallback_warning(&mut self, backend_simple_name: &str) -> Option<String> {
        if !self.debug || self.warning_printed {
            return None;
        }
        self.warning_printed = true;
        Some(format!(
            "Can't change log level during runtime for {backend_simple_name}. Please configure your logger separately. Wrapping the log and redirecting everything into warn for now\n"
        ))
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn logger(&mut self, name: impl Into<String>, level: Option<Level>) -> Logger {
        let name = name.into();
        let name = if name.eq_ignore_ascii_case("ROOT") {
            "ROOT".into()
        } else {
            name
        };
        self.levels.insert(
            name.clone(),
            level.unwrap_or(if self.debug { Level::Debug } else { Level::Error }),
        );
        Logger { name }
    }

    /// Plain slf4j `LoggerFactory.getLogger(name)`, bypassing MarathonLogging: no level is set, the
    /// effective level comes from the hierarchy (e.g. sshj's `LoggerFactory.DEFAULT` loggers).
    pub fn slf4j_logger(&self, name: impl Into<String>) -> Logger {
        Logger { name: name.into() }
    }

    /// Whether `logger(name, _)` has already set a level for `name`; such
    /// handles must be reused rather than re-requested, which resets the level.
    pub fn is_registered(&self, name: &str) -> bool {
        if name.eq_ignore_ascii_case("ROOT") {
            return true;
        }
        self.levels.contains_key(name)
    }

    pub fn effective_level(&self, name: &str) -> Level {
        let mut parent = if name.eq_ignore_ascii_case("ROOT") {
            "ROOT"
        } else {
            name
        };
        loop {
            if let Some(level) = self.levels.get(parent) {
                return *level;
            }
            match parent.rsplit_once(['.', '$']) {
                Some((prefix, _)) => parent = prefix,
                None => return self.levels["ROOT"],
            }
        }
    }

    pub fn enabled(&self, logger: &Logger, level: Level) -> bool {
        let threshold = self.effective_level(&logger.name);
        threshold != Level::Off && level >= threshold
    }

    /// C18: appender replacement does not reset levels on previously requested loggers.
    pub fn configure(&mut self, debug: bool, vendor: Vendor) {
        self.debug = debug;
        self.layout = match vendor {
            Vendor::Android | Vendor::Apple { compact_output: false } => Layout::Runtime,
            Vendor::Apple { compact_output: true } => Layout::Compact,
        };
        if matches!(vendor, Vendor::Apple { .. }) {
            for name in [
                "TimelineSummaryProvider",
                "net.schmizz.sshj.transport.kex.Curve25519SHA256",
                "net.schmizz.sshj.transport.random.BouncyCastleRandom",
                "net.schmizz.sshj.DefaultConfig",
                "net.schmizz.sshj.common.KeyType",
                "net.schmizz.sshj.common.ECDSAVariationsAdapter",
            ] {
                self.levels.insert(name.into(), Level::Error);
            }
        }
    }

    pub fn timings(&mut self) -> Logger {
        if let Some(logger) = &self.timings {
            return logger.clone();
        }
        let logger = self.logger("Timings", Some(Level::Info));
        self.timings = Some(logger.clone());
        logger
    }

    pub fn xcodebuild(&mut self) -> Logger {
        if let Some(logger) = &self.xcodebuild {
            return logger.clone();
        }
        let logger = self.logger("Xcodebuild", Some(Level::Warn));
        self.xcodebuild = Some(logger.clone());
        logger
    }

    pub fn timing_message(
        &mut self,
        elapsed_millis: impl FnOnce() -> Option<i64>,
        message: impl FnOnce() -> String,
    ) -> Option<String> {
        let logger = self.timings();
        self.enabled(&logger, Level::Info).then(|| {
            let message = message();
            match elapsed_millis() {
                Some(elapsed) => format!("{message} (+{elapsed} ms since start)"),
                None => message,
            }
        })
    }

    pub fn xcodebuild_message(&mut self, device: &str, line: &str) -> Option<String> {
        let logger = self.xcodebuild();
        self.enabled(&logger, Level::Warn)
            .then(|| format!("{device}: stderr={line}"))
    }

    /// The closure is evaluated only if the current name-based level permits the event.
    pub fn log(
        &self,
        logger: &Logger,
        level: Level,
        thread: &str,
        local_time_millis: i64,
        output: (JansiMode, OutputTarget),
        message: impl FnOnce() -> String,
    ) -> Option<String> {
        self.enabled(logger, level).then(|| {
            self.layout.format(
                &LogEvent {
                    level,
                    logger: &logger.name,
                    thread,
                    local_time_millis,
                    message: &message(),
                    throwable: "",
                },
                output.0,
                output.1,
            )
        })
    }
}

/// Stateful vendor stream transformation. Keep this in the runtime's output task;
/// each event is highlighted before the same Jansi parser receives its bytes.
pub struct ConsoleOutput {
    pub mode: JansiMode,
    pub target: OutputTarget,
    stripper: JansiStripper,
}

impl ConsoleOutput {
    pub fn new(mode: JansiMode, target: OutputTarget) -> Self {
        Self {
            mode,
            target,
            stripper: JansiStripper::default(),
        }
    }

    pub fn format(&mut self, layout: Layout, event: &LogEvent<'_>) -> Vec<u8> {
        let raw = layout.format(event, JansiMode::Force, self.target);
        if layout != Layout::Startup {
            self.stripper
                .transform(raw.as_bytes(), self.mode.preserves_ansi(self.target))
        } else {
            raw.into_bytes()
        }
    }

    pub fn failure(&self) -> Option<JansiFailure> {
        self.stripper.failure()
    }

    pub fn finish(&mut self, layout: Layout) -> &'static [u8] {
        // Numeric parsing also fails on the uninstall reset; no reset bytes reach the terminal.
        if self.failure() == Some(JansiFailure::NumberOverflow) {
            return &[];
        }
        self.stripper = JansiStripper::default();
        layout.finish(self.target).as_bytes()
    }
}

/// Library exceptions are observable stream state, never Rust panics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JansiFailure {
    /// The numeric parser remains broken on subsequent writes and terminal uninstall.
    NumberOverflow,
    /// The current write is aborted, but Jansi resets before rethrowing.
    InvalidOption,
}

/// Jansi's bounded escape buffer, shared across writes to the vendor appender.
#[derive(Default)]
pub struct JansiStripper {
    pending: Vec<u8>,
    state: u8,
    number_start: usize,
    failure: Option<JansiFailure>,
}

impl JansiStripper {
    fn command(&self, command: u8) -> bool {
        if command == b'G' {
            let first = self.pending[2..self.pending.len() - 1]
                .split(|&b| b == b';')
                .next()
                .unwrap_or_default();
            return !first.is_empty()
                && first.iter().all(u8::is_ascii_digit)
                && std::str::from_utf8(first)
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                    .is_some();
        }
        if command != b'm' {
            return command.is_ascii_alphabetic();
        }
        let text = &self.pending[2..self.pending.len() - 1];
        let Some(text) = std::str::from_utf8(text).ok() else {
            return false;
        };
        let mut values = Vec::new();
        for item in text.split(';').filter(|s| !s.is_empty()) {
            let Ok(value) = item.parse::<i32>() else {
                return false;
            };
            values.push(value);
        }
        let mut values = values.into_iter().peekable();
        while let Some(value) = values.next() {
            if matches!(value, 38 | 48) && values.peek().is_some() {
                let count = match values.next() {
                    Some(2) => 3,
                    Some(5) => 1,
                    _ => return false,
                };
                for _ in 0..count {
                    if !matches!(values.next(), Some(0..=255)) {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn reset(&mut self, discard: bool, output: &mut Vec<u8>) {
        if !discard {
            output.extend_from_slice(&self.pending);
        }
        self.pending.clear();
        self.state = 0;
    }

    pub fn failure(&self) -> Option<JansiFailure> {
        self.failure
    }

    fn process_command(&mut self, byte: u8, preserve: bool, output: &mut Vec<u8>) {
        if !preserve
            && matches!(byte, b'A'..=b'F' | b'H' | b'f' | b'J'..=b'M' | b'S' | b'T')
            && matches!(self.pending.get(2), Some(b'?' | b'=' | b'"'))
        {
            self.reset(true, output);
            self.failure = Some(JansiFailure::InvalidOption);
        } else {
            if !preserve && matches!(byte, b'C' | b'E') {
                let first = self.pending[2..self.pending.len() - 1]
                    .split(|&b| b == b';')
                    .next()
                    .unwrap_or_default();
                let count = std::str::from_utf8(first)
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1);
                // Jansi's strip processor translates forward/next-line movement into bytes.
                output.extend(std::iter::repeat_n(if byte == b'C' { b' ' } else { b'\n' }, count));
            }
            self.reset(!preserve && self.command(byte), output);
        }
    }

    /// Incomplete sequences remain buffered, as they do in AnsiOutputStream.
    pub fn write(&mut self, input: &[u8]) -> Vec<u8> {
        self.transform(input, false)
    }

    fn transform(&mut self, input: &[u8], preserve: bool) -> Vec<u8> {
        if self.failure == Some(JansiFailure::NumberOverflow) {
            return Vec::new();
        }
        self.failure = None;
        let mut output = Vec::new();
        for &byte in input {
            if self.state == 0 {
                if byte == 27 {
                    self.pending.push(byte);
                    self.state = 1;
                } else {
                    output.push(byte);
                }
                continue;
            }
            self.pending.push(byte);
            match self.state {
                1 => match byte {
                    b'[' => self.state = 2,
                    b']' => self.state = 5,
                    b'(' | b')' => self.state = 9,
                    _ => self.reset(false, &mut output),
                },
                2 => match byte {
                    b'0'..=b'9' => {
                        self.number_start = self.pending.len() - 1;
                        self.state = 4;
                    }
                    b';' | b'?' | b'=' => (),
                    b'"' => self.state = 3,
                    _ => self.process_command(byte, preserve, &mut output),
                },
                3 => {
                    // Jansi 2.4.1 consumes repeated quotes, then processes the first nonquote as a command.
                    if byte == b';' {
                        self.state = 2;
                    } else if byte != b'"' {
                        self.process_command(byte, preserve, &mut output);
                    }
                }
                4 => {
                    if !byte.is_ascii_digit() {
                        let number = std::str::from_utf8(&self.pending[self.number_start..self.pending.len() - 1])
                            .ok()
                            .and_then(|s| s.parse::<i32>().ok());
                        if number.is_none() {
                            self.failure = Some(JansiFailure::NumberOverflow);
                        } else if byte == b';' {
                            self.state = 2;
                        } else {
                            self.process_command(byte, preserve, &mut output);
                        }
                    }
                }
                5 => {
                    if byte.is_ascii_digit() {
                        self.state = 6;
                    } else {
                        self.reset(false, &mut output);
                    }
                }
                6 => {
                    if byte == b';' {
                        self.state = 7;
                    } else if !byte.is_ascii_digit() {
                        self.reset(false, &mut output);
                    }
                }
                7 => {
                    if byte == 7 {
                        self.reset(!preserve, &mut output);
                    } else if byte == 27 {
                        self.state = 8;
                    }
                }
                8 => {
                    if byte == b'\\' {
                        self.reset(!preserve, &mut output);
                    } else {
                        self.state = 7;
                    }
                }
                9 => self.reset(!preserve, &mut output),
                _ => unreachable!(),
            }
            if self.failure.is_some() {
                break;
            }
            // Jansi flushes an overlong escape verbatim after exactly 100 bytes.
            if self.pending.len() >= 100 {
                self.reset(false, &mut output);
            }
        }
        output
    }
}

/// Strip a complete buffer. Use `JansiStripper` when a sequence can span writes.
pub fn strip_ansi(input: &str) -> String {
    String::from_utf8_lossy(&JansiStripper::default().write(input.as_bytes())).into_owned()
}

/// Non-logback fallback: low-level calls become WARN, but level-enabled properties
/// still belong to the underlying backend. The caller forwards the returned call.
pub struct KLoggerDebug;

impl KLoggerDebug {
    pub fn level(level: Level) -> Level {
        match level {
            Level::Trace | Level::Debug | Level::Info => Level::Warn,
            _ => level,
        }
    }

    pub fn message(level: Level, pattern: &str, arguments: &[&str], varargs: bool) -> (Level, String) {
        let output = if varargs && matches!(level, Level::Trace | Level::Debug | Level::Info) {
            // The Kotlin overrides omit the spread operator: the entire array is one argument.
            format_message(pattern, &[&format!("[{}]", arguments.join(", "))])
        } else {
            format_message(pattern, arguments)
        };
        (Self::level(level), output)
    }
}
