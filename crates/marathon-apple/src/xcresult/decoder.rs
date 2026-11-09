//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcresulttool/AppleJsonTypeAdapterFactory.kt

use marathon_error::{ConfigKind, FailureKind};
use std::collections::BTreeMap;

use jcompat::jackson::{self, JavaDate, Scalar, ScalarError};
use marathon_core::error::Failure;
use serde_json::value::RawValue;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeErrorKind {
    JsonSyntax,
    Mapping,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodePathSegment {
    Field(String),
    Index(usize),
}

/// Structured equivalent of the JSON-processing failures caught by WP 4.4.
/// Approved P1 turns unsafe scalar-wrapper dereferences into mapping failures;
/// there are no unchecked adapter panics for the caller to catch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XcresultDecodeError {
    pub kind: DecodeErrorKind,
    pub path: Vec<DecodePathSegment>,
    pub failure: Failure,
}

impl std::fmt::Display for XcresultDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.failure.to_string().to_owned())
    }
}
impl std::error::Error for XcresultDecodeError {}

impl From<XcresultDecodeError> for Failure {
    fn from(error: XcresultDecodeError) -> Self {
        error.failure
    }
}

fn failure(kind: FailureKind, message: &str) -> Failure {
    Failure::from_kind(kind, Some(message.into()))
}

#[derive(Clone, Debug, Default)]
pub(super) struct Path(Vec<DecodePathSegment>);
impl Path {
    fn field(&self, name: &str) -> Self {
        let mut path = self.clone();
        path.0.push(DecodePathSegment::Field(name.to_owned()));
        path
    }
    fn index(&self, index: usize) -> Self {
        let mut path = self.clone();
        path.0.push(DecodePathSegment::Index(index));
        path
    }
    fn error(&self, class: FailureKind, message: &str) -> XcresultDecodeError {
        XcresultDecodeError {
            kind: DecodeErrorKind::Mapping,
            path: self.0.clone(),
            failure: failure(class, message),
        }
    }
    fn shape(&self) -> XcresultDecodeError {
        self.error(
            FailureKind::Config(ConfigKind::MismatchedInput),
            "Unexpected xcresult JSON value shape",
        )
    }
    fn scalar(&self, error: ScalarError) -> XcresultDecodeError {
        match error {
            ScalarError::Shape => self.shape(),
            ScalarError::Invalid => self.error(
                FailureKind::Config(ConfigKind::InvalidFormat),
                "Invalid xcresult scalar value",
            ),
        }
    }
}

const MAX_NESTING_DEPTH: usize = 1000;

fn syntax(error: serde_json::Error) -> XcresultDecodeError {
    XcresultDecodeError {
        kind: DecodeErrorKind::JsonSyntax,
        path: vec![],
        failure: failure(
            if error.is_eof() {
                FailureKind::Config(ConfigKind::JsonEof)
            } else {
                FailureKind::Config(ConfigKind::JsonParse)
            },
            &error.to_string(),
        ),
    }
}

fn nesting_error(depth: usize) -> XcresultDecodeError {
    XcresultDecodeError {
        kind: DecodeErrorKind::JsonSyntax,
        path: vec![],
        failure: failure(
            FailureKind::Config(ConfigKind::StreamConstraints),
            &format!(
                "Document nesting depth ({depth}) exceeds the maximum allowed ({MAX_NESTING_DEPTH}, from `StreamReadConstraints.getMaxNestingDepth()`)"
            ),
        ),
    }
}

// RawValue validates syntax while retaining UTF-16 escapes, integer widths and
// float token spelling. A serde Value would reject Java's unpaired surrogates.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum JsonNode {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<JsonNode>),
    Object(BTreeMap<String, JsonNode>),
}
enum Open {
    Array(Vec<JsonNode>),
    Object(BTreeMap<String, JsonNode>, Option<String>),
}
impl JsonNode {
    /// Scans an already validated value with an explicit stack, so neither input
    /// depth nor the decoder's own recursion (bounded by Jackson's limit) can
    /// overflow the thread stack.
    pub(super) fn parse(raw: &RawValue) -> Result<Self, XcresultDecodeError> {
        let text = raw.get();
        let bytes = text.as_bytes();
        let malformed = || syntax(serde_json::from_str::<()>("]").unwrap_err());
        let mut open = Vec::new();
        let mut pos = 0;
        loop {
            while bytes.get(pos).is_some_and(|byte| b" \t\n\r,:".contains(byte)) {
                pos += 1;
            }
            let start = pos;
            let value = match bytes.get(pos).ok_or_else(malformed)? {
                b'[' | b'{' => {
                    if open.len() == MAX_NESTING_DEPTH {
                        return Err(nesting_error(open.len() + 1));
                    }
                    open.push(if bytes[pos] == b'[' {
                        Open::Array(vec![])
                    } else {
                        Open::Object(BTreeMap::new(), None)
                    });
                    pos += 1;
                    continue;
                }
                b']' | b'}' => {
                    pos += 1;
                    match open.pop().ok_or_else(malformed)? {
                        Open::Array(values) => Self::Array(values),
                        Open::Object(fields, _) => Self::Object(fields),
                    }
                }
                b'"' => {
                    pos += 1;
                    while *bytes.get(pos).ok_or_else(malformed)? != b'"' {
                        pos += if bytes[pos] == b'\\' { 2 } else { 1 };
                    }
                    pos += 1;
                    let token = &text[start..pos];
                    if let Some(Open::Object(_, key @ None)) = open.last_mut() {
                        *key = Some(serde_json::from_str(token).map_err(syntax)?);
                        continue;
                    }
                    Self::String(jackson::json_string(token).unwrap_or_default())
                }
                _ => {
                    while bytes.get(pos).is_some_and(|byte| !b" \t\n\r,]}".contains(byte)) {
                        pos += 1;
                    }
                    match &text[start..pos] {
                        "null" => Self::Null,
                        "true" => Self::Bool(true),
                        "false" => Self::Bool(false),
                        number => Self::Number(number.to_owned()),
                    }
                }
            };
            match open.last_mut() {
                None => return Ok(value),
                Some(Open::Array(values)) => values.push(value),
                Some(Open::Object(fields, key)) => {
                    fields.insert(key.take().ok_or_else(malformed)?, value);
                }
            }
        }
    }
    pub(super) fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(fields) => fields.get(key),
            _ => None,
        }
    }
    fn scalar(&self) -> Scalar<'_> {
        match self {
            Self::Null => Scalar::Null,
            Self::Bool(value) => Scalar::Bool(*value),
            Self::Number(text) => Scalar::Number(text),
            Self::String(text) => Scalar::String(text),
            _ => Scalar::Other,
        }
    }
    #[cfg(test)]
    pub(super) fn object<const N: usize>(fields: [(&str, Self); N]) -> Self {
        Self::Object(fields.into_iter().map(|(key, value)| (key.to_owned(), value)).collect())
    }
}

/// Sealed bound implemented by all generated object models.
#[allow(private_bounds)]
pub trait LegacyDecode: Decode {}
impl<T: Decode> LegacyDecode for T {}

pub(super) trait Decode: Sized {
    const STRING_COLLECTION: bool = false;
    fn decode(node: &JsonNode, path: &Path) -> Result<Option<Self>, XcresultDecodeError>;
}

/// Decode the first JSON value, as ObjectMapper.readValue does with trailing-token
/// validation disabled. JSON null succeeds as None; blank output is WP 4.4's policy.
pub fn decode_legacy<T: LegacyDecode>(json: &str) -> Result<Option<T>, XcresultDecodeError> {
    let json = jackson::normalize_json_strings(json);
    let raw = serde_json::Deserializer::from_str(&json)
        .into_iter::<Box<RawValue>>()
        .next()
        .unwrap_or_else(|| serde_json::from_str::<Box<RawValue>>(""))
        .map_err(syntax)?;
    let node = JsonNode::parse(&raw)?;
    T::decode(&node, &Path::default())
}

pub(super) struct Fields<'a> {
    node: &'a JsonNode,
    path: &'a Path,
}
impl<'a> Fields<'a> {
    pub(super) fn new(node: &'a JsonNode, path: &'a Path) -> Result<Self, XcresultDecodeError> {
        if !matches!(node, JsonNode::Object(_)) {
            return Err(path.shape());
        }
        Ok(Self { node, path })
    }
    pub(super) fn optional<T: Decode>(&self, name: &str) -> Result<Option<T>, XcresultDecodeError> {
        match self.node.get(name) {
            None | Some(JsonNode::Null) => Ok(None),
            Some(node) => T::decode(node, &self.path.field(name)),
        }
    }
    pub(super) fn defaulted<T: Decode + Default>(&self, name: &str) -> Result<T, XcresultDecodeError> {
        Ok(self.optional(name)?.unwrap_or_default())
    }
    pub(super) fn required<T: Decode>(&self, name: &str) -> Result<T, XcresultDecodeError> {
        self.optional(name)?.ok_or_else(|| {
            self.path.field(name).error(
                FailureKind::Config(ConfigKind::MissingParameter),
                "Missing or null required xcresult constructor parameter",
            )
        })
    }
    pub(super) fn list<T: Decode>(&self, name: &str) -> Result<Vec<Option<T>>, XcresultDecodeError> {
        match self.node.get(name) {
            None | Some(JsonNode::Null) => Ok(vec![]),
            Some(node) => decode_list(node, &self.path.field(name)),
        }
    }
    #[allow(dead_code)] // The pinned schema has no nullable arrays; synthetic generation probes do.
    pub(super) fn optional_list<T: Decode>(&self, name: &str) -> Result<Option<Vec<Option<T>>>, XcresultDecodeError> {
        match self.node.get(name) {
            None | Some(JsonNode::Null) if T::STRING_COLLECTION => Ok(Some(vec![])),
            None | Some(JsonNode::Null) => Ok(None),
            Some(node) => decode_list(node, &self.path.field(name)).map(Some),
        }
    }
}

fn decode_list<T: Decode>(node: &JsonNode, path: &Path) -> Result<Vec<Option<T>>, XcresultDecodeError> {
    let values = if T::STRING_COLLECTION {
        // Jackson's StringCollectionDeserializer is excluded by Apple's adapter predicate.
        match node {
            JsonNode::Array(values) => values,
            _ => return Err(path.shape()),
        }
    } else {
        match node.get("_values") {
            Some(JsonNode::Array(values)) => values,
            _ => return Ok(vec![]),
        }
    };
    values
        .iter()
        .enumerate()
        .map(|(index, node)| T::decode(node, &path.index(index)))
        .collect()
}

fn wrapped<'a>(node: &'a JsonNode, path: &Path) -> Result<&'a JsonNode, XcresultDecodeError> {
    node.get("_value").ok_or_else(|| {
        path.error(
            FailureKind::Config(ConfigKind::JsonMapping),
            "Missing xcresult scalar _value wrapper (WP 4.6 P1)",
        )
    })
}

impl Decode for String {
    const STRING_COLLECTION: bool = true;
    fn decode(node: &JsonNode, path: &Path) -> Result<Option<Self>, XcresultDecodeError> {
        if node.is_null() {
            return Ok(None);
        }
        jackson::string(wrapped(node, path)?.scalar())
            .map(Some)
            .map_err(|error| path.scalar(error))
    }
}
macro_rules! scalar {
    ($ty:ty, $decode:ident) => {
        impl Decode for $ty {
            fn decode(node: &JsonNode, path: &Path) -> Result<Option<Self>, XcresultDecodeError> {
                if node.is_null() {
                    return Ok(None);
                }
                jackson::$decode(wrapped(node, path)?.scalar()).map_err(|error| path.scalar(error))
            }
        }
    };
}
scalar!(i32, int);
scalar!(f64, double);
scalar!(bool, boolean);
scalar!(JavaDate, date);

impl<T: Decode> Decode for Box<T> {
    fn decode(node: &JsonNode, path: &Path) -> Result<Option<Self>, XcresultDecodeError> {
        T::decode(node, path).map(|value| value.map(Box::new))
    }
}
