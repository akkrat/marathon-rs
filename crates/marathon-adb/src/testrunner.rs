//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/testrunner/TestEvent.kt`,
//! `.../testrunner/TestIdentifier.kt`, `.../testrunner/model/Status.kt` and
//! `.../testrunner/model/TokenType.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use jcompat::writers::JavaString;

/// Kotlin `linkedMapOf<String, String>()`: insertion order is kept and
/// re-inserting a key replaces its value without moving it. Equality is
/// Kotlin map equality (same pairs, any order).
#[derive(Clone, Debug, Default)]
pub struct MetricMap {
    entries: Vec<(JavaString, JavaString)>,
}
impl MetricMap {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, key: JavaString, value: JavaString) -> Option<JavaString> {
        if let Some((_, current)) = self.entries.iter_mut().find(|(existing, _)| *existing == key) {
            return Some(std::mem::replace(current, value));
        }
        self.entries.push((key, value));
        None
    }
    pub fn get(&self, key: &JavaString) -> Option<&JavaString> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    /// Lookup by an ASCII key without allocating a `JavaString`.
    pub fn get_str(&self, key: &str) -> Option<&JavaString> {
        self.entries
            .iter()
            .find(|(k, _)| k.0.len() == key.len() && k.0.iter().zip(key.bytes()).all(|(&u, b)| u == u16::from(b)))
            .map(|(_, v)| v)
    }
    pub fn contains_key_str(&self, key: &str) -> bool {
        self.get_str(key).is_some()
    }
    pub fn entries(&self) -> &[(JavaString, JavaString)] {
        &self.entries
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
impl PartialEq for MetricMap {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len() && self.entries.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}
impl Eq for MetricMap {}
impl FromIterator<(JavaString, JavaString)> for MetricMap {
    fn from_iter<T: IntoIterator<Item = (JavaString, JavaString)>>(iter: T) -> Self {
        let mut map = Self::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TestIdentifier {
    pub class_name: JavaString,
    pub test_name: JavaString,
}

/// The sealed `TestEvent` hierarchy, variants in the Kotlin declaration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestEvent {
    TestRunStartedEvent {
        test_count: i32,
    },
    TestStarted {
        id: TestIdentifier,
    },
    TestFailed {
        id: TestIdentifier,
        stack_trace: JavaString,
    },
    TestAssumptionFailed {
        id: TestIdentifier,
        stack_trace: JavaString,
    },
    TestIgnored {
        id: TestIdentifier,
    },
    TestEnded {
        id: TestIdentifier,
        metrics: MetricMap,
    },
    TestRunFailed {
        error: JavaString,
    },
    TestRunFailing {
        error: JavaString,
        stack_trace: JavaString,
    },
    TestRunStopped {
        elapsed_time_millis: i64,
    },
    TestRunEnded {
        elapsed_time_millis: i64,
        metrics: MetricMap,
    },
}

/// Declared next to `TestEvent` in adam but not a subclass of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestLogcat {
    pub id: TestIdentifier,
    pub log: JavaString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Success,
    Start,
    InProgress,
    Error,
    Failure,
    Ignored,
    AssumptionFailure,
    Unknown,
}
impl Status {
    pub fn value(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::Start => 1,
            Self::InProgress => 2,
            Self::Error => -1,
            Self::Failure => -2,
            Self::Ignored => -3,
            Self::AssumptionFailure => -4,
            Self::Unknown => 6666,
        }
    }
    pub fn is_terminal(self) -> bool {
        self.value() < 0
    }
    /// `Status.valueOf(Int?)`: anything unrecognised, including null, is UNKNOWN.
    pub fn value_of(value: Option<i32>) -> Self {
        match value {
            Some(0) => Self::Success,
            Some(1) => Self::Start,
            Some(2) => Self::InProgress,
            Some(-1) => Self::Error,
            Some(-2) => Self::Failure,
            Some(-3) => Self::Ignored,
            Some(-4) => Self::AssumptionFailure,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenType {
    InstrumentationStatus,
    InstrumentationStatusCode,
    InstrumentationResult,
    InstrumentationCode,
    InstrumentationFailed,
}
impl TokenType {
    /// The Kotlin enum `name`, which is the wire spelling.
    pub fn name(self) -> &'static str {
        match self {
            Self::InstrumentationStatus => "INSTRUMENTATION_STATUS",
            Self::InstrumentationStatusCode => "INSTRUMENTATION_STATUS_CODE",
            Self::InstrumentationResult => "INSTRUMENTATION_RESULT",
            Self::InstrumentationCode => "INSTRUMENTATION_CODE",
            Self::InstrumentationFailed => "INSTRUMENTATION_FAILED",
        }
    }
}
