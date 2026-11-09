//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! and serialization/yaml/SerializeModule.kt. Synchronous binding runs off the core executor.

use marathon_error::{ConfigKind, FailureKind};
use std::cell::RefCell;
use std::path::PathBuf;

use crate::binding::{
    BindingCause, BindingContext, ConfigurationCause, ConfigurationError, PathPart, Profile, TokenError,
    ValidationStage,
};
use jcompat::{
    JavaRegex,
    jtime::{Duration, Instant},
};
use snakeyaml::{
    node::{Node, NodeKind, Stream, YamlError},
    resolve::{JInt, java_trim},
    tokens::{self, Tok, Token},
};

pub const MISMATCHED: FailureKind = FailureKind::Config(ConfigKind::MismatchedInput);
pub const INVALID_FORMAT: FailureKind = FailureKind::Config(ConfigKind::InvalidFormat);
pub const JSON_MAPPING: FailureKind = FailureKind::Config(ConfigKind::JsonMapping);
pub const MISSING_KOTLIN: FailureKind = FailureKind::Config(ConfigKind::MissingParameter);
pub const UNKNOWN_PROPERTY: FailureKind = FailureKind::Config(ConfigKind::UnrecognizedProperty);
pub const INVALID_TYPE: FailureKind = FailureKind::Config(ConfigKind::InvalidTypeId);
pub type Result<T> = std::result::Result<T, Box<ConfigurationError>>;

/// Schema functions receive a view; they can call any other schema function on a child.
/// Defaults belong to schemas, never to the YAML reader.
pub struct Binder<'a> {
    pub context: &'a BindingContext,
    error: Option<&'a YamlError>,
    /// The first unknown property of each object still being bound. Jackson's
    /// property-based creator buffers unknown properties and reports them only
    /// after `creator.build`, so later field errors and missing creator
    /// parameters win.
    deferred: RefCell<Vec<(*const Node, Box<ConfigurationError>)>>,
}
impl<'a> Binder<'a> {
    pub fn new(context: &'a BindingContext, stream: &'a Stream) -> Self {
        Self {
            context,
            error: stream.error.as_ref(),
            deferred: RefCell::new(Vec::new()),
        }
    }
    pub fn view(&'a self, node: &'a Node) -> Value<'a> {
        Value {
            binder: self,
            node,
            path: Vec::new(),
            hidden_type: None,
            wrapper_end: false,
            parent: None,
        }
    }
    /// None is a leading null document. Callers preserve its separate crash stage.
    pub fn first<T>(&'a self, stream: &'a Stream, bind: impl FnOnce(Value<'a>) -> Result<T>) -> Result<Option<T>> {
        match stream.documents.first() {
            Some(doc) => {
                let value = self.view(&doc.root);
                if value.is_null()? {
                    Ok(None)
                } else {
                    let bound = bind(value);
                    // Every object binder settles its own node; anything left
                    // here is still reported rather than silently accepted.
                    let leftover = self.deferred.take().into_iter().next();
                    match (bound, leftover) {
                        (Ok(_), Some((_, error))) => Err(error),
                        (bound, _) => bound.map(Some),
                    }
                }
            }
            None => Err(self.stream_error(Vec::new(), None, None)),
        }
    }
    fn stream_error(
        &self,
        path: Vec<PathPart>,
        span: Option<snakeyaml::node::Span>,
        wrap: Option<FailureKind>,
    ) -> Box<ConfigurationError> {
        let token = self.error.map(tokens::TokenError::from).unwrap_or(tokens::TokenError {
            stage: "binding",
            kind: tokens::TokenErrorKind::MismatchedInput,
            message: "no content to bind".into(),
            context: None,
            problem: None,
            location: None,
        });
        token_error(token, path, span, wrap)
    }
    fn defer(&self, object: &Node, error: Box<ConfigurationError>) {
        let mut deferred = self.deferred.borrow_mut();
        if !deferred.iter().any(|(node, _)| std::ptr::eq(*node, object)) {
            deferred.push((object, error));
        }
    }
    /// `creator.build` then `handleUnknownProperties`: a failed bind discards
    /// the buffered unknown property, a successful one reports it.
    fn settle<T>(&self, object: &Node, bound: Result<T>) -> Result<T> {
        let mut deferred = self.deferred.borrow_mut();
        let unknown = deferred
            .iter()
            .position(|(node, _)| std::ptr::eq(*node, object))
            .map(|i| deferred.remove(i).1);
        match (bound, unknown) {
            (Ok(_), Some(error)) => Err(error),
            (bound, _) => bound,
        }
    }
}
/// Share the config-boundary projection with standalone tokenizer consumers such as the corpus runner.
pub fn token_kind(kind: tokens::TokenErrorKind) -> FailureKind {
    match kind {
        tokens::TokenErrorKind::Fixture => FailureKind::Injected(marathon_error::InjectedKind::ParserFixture),
        tokens::TokenErrorKind::MarkedYaml => FailureKind::Config(ConfigKind::YamlMarked),
        tokens::TokenErrorKind::YamlParse => FailureKind::Config(ConfigKind::YamlParse),
        tokens::TokenErrorKind::JsonParse => FailureKind::Config(ConfigKind::JsonParse),
        tokens::TokenErrorKind::StreamConstraints => FailureKind::Config(ConfigKind::StreamConstraints),
        tokens::TokenErrorKind::JsonEof => FailureKind::Config(ConfigKind::JsonEof),
        tokens::TokenErrorKind::MismatchedInput => MISMATCHED,
    }
}
fn token_error(
    e: tokens::TokenError,
    path: Vec<PathPart>,
    span: Option<snakeyaml::node::Span>,
    wrap: Option<FailureKind>,
) -> Box<ConfigurationError> {
    let mut causes: Vec<ConfigurationCause> = wrap
        .clone()
        .filter(|c| *c != token_kind(e.kind))
        .map(|_| ConfigurationCause {
            kind: token_kind(e.kind),
            message: Some(e.message.to_owned()),
        })
        .into_iter()
        .collect();
    // YAMLParser wraps the original SnakeYAML scanner/parser exception. The
    // wrapper and its cause are distinct even when their diagnostics coincide.
    // JacksonYAMLParseException likewise wraps the reader's ReaderException or
    // the scanner's plain YAMLException (code point limit).
    let original = match (e.kind, e.stage) {
        (tokens::MARKED_YAML_EXCEPTION, "scanner") => Some(FailureKind::Config(ConfigKind::Scanner)),
        (tokens::MARKED_YAML_EXCEPTION, _) => Some(FailureKind::Config(ConfigKind::Parser)),
        (tokens::JACKSON_YAML_PARSE_EXCEPTION, "reader") => Some(FailureKind::Config(ConfigKind::Reader)),
        (tokens::JACKSON_YAML_PARSE_EXCEPTION, _) => Some(FailureKind::Config(ConfigKind::Yaml)),
        _ => None,
    };
    if let Some(class) = original {
        causes.push(ConfigurationCause {
            kind: class,
            message: Some(e.message.to_owned()),
        });
    }
    Box::new(ConfigurationError {
        stage: ValidationStage::Parse,
        kind: wrap.unwrap_or_else(|| token_kind(e.kind)),
        message: Some(e.message.to_owned()),
        causes,
        path,
        span,
        cause: BindingCause::Stream(TokenError {
            stage: e.stage.into(),
            kind: token_kind(e.kind),
            message: e.message,
            context_mark: e.context,
            problem_mark: e.problem,
            jackson_location: e.location,
        }),
    })
}

fn wrap_stream(mut error: Box<ConfigurationError>) -> Box<ConfigurationError> {
    if matches!(error.cause, BindingCause::Stream(_)) && error.kind != JSON_MAPPING {
        error.causes.insert(
            0,
            ConfigurationCause {
                kind: error.kind.clone(),
                message: error.message.to_owned(),
            },
        );
        error.kind = JSON_MAPPING;
    }
    error
}

#[derive(Clone)]
pub struct Value<'a> {
    pub(crate) binder: &'a Binder<'a>,
    pub node: &'a Node,
    pub(crate) path: Vec<PathPart>,
    pub(crate) hidden_type: Option<usize>,
    pub(crate) wrapper_end: bool,
    /// The object or array this value is a member of.
    pub(crate) parent: Option<&'a Node>,
}
impl<'a> Value<'a> {
    pub fn context(&self) -> &'a BindingContext {
        self.binder.context
    }
    pub fn error(&self, class: FailureKind, message: impl Into<String>) -> Box<ConfigurationError> {
        let message = message.into();
        Box::new(ConfigurationError {
            stage: ValidationStage::Parse,
            kind: class,
            message: Some(message.to_owned()),
            causes: Vec::new(),
            path: self.path.clone(),
            span: Some(self.node.opening),
            cause: BindingCause::Coercion(message),
        })
    }
    fn child(&self, node: &'a Node, part: PathPart) -> Self {
        let mut path = self.path.clone();
        path.push(part);
        Self {
            binder: self.binder,
            node,
            path,
            hidden_type: None,
            wrapper_end: false,
            parent: Some(self.node),
        }
    }
    pub(crate) fn token(&self) -> Result<Token> {
        let toks = tokens::subtree(self.node, self.binder.error);
        toks.tokens
            .into_iter()
            .next()
            .map(|mut token| {
                if self.wrapper_end {
                    token.tok = Tok::EndArray;
                }
                token
            })
            .ok_or_else(|| {
                toks.error
                    .map(|e| token_error(e, self.path.clone(), Some(self.node.opening), None))
                    .unwrap_or_else(|| self.error(MISMATCHED, "expected a value"))
            })
    }
    pub fn is_null(&self) -> Result<bool> {
        if matches!(self.node.kind, NodeKind::Map { .. } | NodeKind::Seq { .. }) {
            return Ok(false);
        }
        Ok(matches!(self.token()?.tok, Tok::Null(_)))
    }
    /// Force token consumption, as readTree/skipChildren do, including ignored fields.
    pub fn validate_tree(&self) -> Result<()> {
        if let Some(e) = tokens::subtree(self.node, self.binder.error).error {
            return Err(token_error(e, self.path.clone(), Some(self.node.opening), None));
        }
        Ok(())
    }
    pub fn read_tree(&self) -> Result<crate::tree::Tree<'a>> {
        self.validate_tree()?;
        crate::tree::Tree::new(self.clone())
    }
    pub fn bind<T>(&self, bind: impl FnOnce(Self) -> Result<T>) -> Result<T> {
        bind(self.clone())
    }
    /// Direct child lookup. On materialized trees duplicate names already collapsed.
    pub fn get(&self, name: &str) -> Result<Option<Self>> {
        if let NodeKind::Map { entries, .. } = &self.node.kind {
            for (index, (key, node)) in entries.iter().enumerate() {
                if self.hidden_type == Some(index) {
                    continue;
                }
                let key = self.key(key)?;
                if key == name {
                    return Ok(Some(self.child(node, PathPart::Key(key))));
                }
            }
        }
        Ok(None)
    }
    /// ObjectNode.findValue checks direct children first, then searches child
    /// containers in insertion order. ArrayNode searches elements in order.
    pub fn find_value(&self, name: &str) -> Result<Option<Self>> {
        let mut pending = vec![self.clone()];
        while let Some(value) = pending.pop() {
            if let Some(found) = value.get(name)? {
                return Ok(Some(found));
            }
            match &value.node.kind {
                NodeKind::Map { entries, .. } => {
                    for (index, (key, node)) in entries.iter().enumerate().rev() {
                        if value.hidden_type == Some(index) {
                            continue;
                        }
                        pending.push(value.child(node, PathPart::Key(value.key(key)?)));
                    }
                }
                NodeKind::Seq { items, .. } => {
                    for (index, node) in items.iter().enumerate().rev() {
                        pending.push(value.child(node, PathPart::Index(index)));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }
    fn key(&self, key: &Node) -> Result<String> {
        match tokens::check_key(key) {
            Ok(s) => Ok(s.to_string()),
            Err(tokens::KeyProblem::NotScalar(e)) => Err(token_error(*e, self.path.clone(), Some(key.opening), None)),
            Err(tokens::KeyProblem::Cut) => Err(self.binder.stream_error(self.path.clone(), Some(key.opening), None)),
        }
    }
    pub fn object(&self) -> Result<Object<'a>> {
        match &self.node.kind {
            NodeKind::Map { entries, complete } => Ok(Object {
                value: self.clone(),
                entries,
                complete: *complete,
                pos: 0,
            }),
            NodeKind::Cut => Err(self
                .binder
                .stream_error(self.path.clone(), Some(self.node.opening), None)),
            _ => Err(self.error(MISMATCHED, "expected an object")),
        }
    }
    /// An unknown property of a Kotlin data class. Its value is still read, as
    /// the creator's buffer copies it; the error waits for [`Binder::settle`].
    pub fn unknown(&self, ignore: bool) -> Result<()> {
        self.validate_tree()?;
        if ignore {
            return Ok(());
        }
        let error = self.error(UNKNOWN_PROPERTY, "unknown property");
        match self.parent {
            Some(object) => {
                self.binder.defer(object, error);
                Ok(())
            }
            None => Err(error),
        }
    }
    /// A Kotlin `object` has no creator, so Jackson rejects the property at once.
    pub(crate) fn unknown_now(&self) -> Result<()> {
        Err(self.error(UNKNOWN_PROPERTY, "unknown property"))
    }
    /// Canonical nullable-composite adapter: schema functions wrap their object body
    /// here and return `Result<Option<T>>`, just like leaves and collections.
    /// Pass that schema directly to `list`/`map` to retain null elements
    /// (Kotlin StrictNullChecks=false), or assign it to a `Field` for null policy.
    pub fn nullable<T>(&self, bind: impl FnOnce(Self) -> Result<T>) -> Result<Option<T>> {
        if self.wrapper_end || self.is_null()? {
            Ok(None)
        } else {
            self.binder.settle(self.node, bind(self.clone())).map(Some)
        }
    }
    pub fn required<T>(&self, value: Option<T>) -> Result<T> {
        value.ok_or_else(|| self.error(MISSING_KOTLIN, "non-null parameter is null"))
    }
    fn scalar<T>(&self, bind: impl FnOnce(&Token) -> std::result::Result<T, FailureKind>) -> Result<T> {
        bind(&self.token()?).map_err(|class| self.error(class, "cannot coerce scalar to field type"))
    }
    pub fn int(&self) -> Result<i32> {
        self.scalar(crate::scalar::int_value)
    }
    pub fn long(&self) -> Result<i64> {
        self.scalar(crate::scalar::long_value)
    }
    pub fn double(&self) -> Result<f64> {
        self.scalar(crate::scalar::double_value)
    }
    pub fn boolean(&self) -> Result<bool> {
        self.scalar(crate::scalar::bool_value)
    }
    /// Jackson boxed-number/boolean semantics for a Kotlin nullable scalar
    /// (`Int?` etc.): an explicit null, an empty/blank string and the quoted
    /// string `"null"` all deserialize to null instead of the primitive zero
    /// that `int`/`long`/`double`/`boolean` produce. Every other spelling
    /// binds exactly as the primitive coercion does.
    fn boxed_is_null(&self) -> Result<bool> {
        if self.is_null()? {
            return Ok(true);
        }
        Ok(match &self.token()?.tok {
            Tok::Str(s) => {
                let text = s.to_owned();
                let trimmed = java_trim(&text);
                trimmed.is_empty() || trimmed == "null"
            }
            _ => false,
        })
    }
    pub fn int_boxed(&self) -> Result<Option<i32>> {
        if self.boxed_is_null()? {
            Ok(None)
        } else {
            self.int().map(Some)
        }
    }
    pub fn long_boxed(&self) -> Result<Option<i64>> {
        if self.boxed_is_null()? {
            Ok(None)
        } else {
            self.long().map(Some)
        }
    }
    pub fn double_boxed(&self) -> Result<Option<f64>> {
        if self.boxed_is_null()? {
            Ok(None)
        } else {
            self.double().map(Some)
        }
    }
    pub fn boolean_boxed(&self) -> Result<Option<bool>> {
        if self.boxed_is_null()? {
            Ok(None)
        } else {
            self.boolean().map(Some)
        }
    }
    pub fn string(&self) -> Result<Option<String>> {
        let token = self.token()?;
        let value = crate::scalar::string_value(&token).map_err(|class| self.error(class, "expected scalar string"))?;
        Ok(value.map(|s| {
            if matches!(token.tok, Tok::Embedded(_)) {
                s
            } else {
                token.java_text.unwrap_or(s)
            }
        }))
    }
    pub fn enumeration(&self, names: &[&str]) -> Result<Option<usize>> {
        let token = self.token()?;
        let ordinal = match &token.tok {
            Tok::Null(_) => return Ok(None),
            Tok::Int {
                value: Ok(JInt::Int(i)),
                ..
            } => Some(i64::from(*i)),
            Tok::Int {
                value: Ok(JInt::Long(i)),
                ..
            } if i32::try_from(*i).is_ok() => Some(*i),
            Tok::Int { .. } => return Err(self.error(JSON_MAPPING, "enum ordinal outside integer range")),
            Tok::Str(s) => {
                let text = s.to_owned();
                let s = java_trim(&text);
                if let Some(index) = names.iter().position(|name| *name == s) {
                    return Ok(Some(index));
                }
                if s == "0"
                    || (s.as_bytes().first().is_some_and(|b| (b'1'..=b'9').contains(b))
                        && s.bytes().all(|b| b.is_ascii_digit()))
                {
                    s.parse::<i64>().ok()
                } else {
                    None
                }
            }
            _ => return Err(self.error(MISMATCHED, "expected enum name or ordinal")),
        };
        ordinal
            .and_then(|i| usize::try_from(i).ok())
            .filter(|i| *i < names.len())
            .map(Some)
            .ok_or_else(|| self.error(INVALID_FORMAT, "unknown enum value"))
    }
    /// For enums with @JsonCreator, such as SerialStrategy's case folding.
    pub fn enum_creator<T>(&self, creator: impl FnOnce(&str) -> Option<T>) -> Result<Option<T>> {
        self.enum_creator_named("", creator)
    }
    pub fn enum_creator_named<T>(
        &self,
        enum_class: &str,
        creator: impl FnOnce(&str) -> Option<T>,
    ) -> Result<Option<T>> {
        self.string()?
            .map(|s| {
                let text = s.to_owned();
                creator(&text).ok_or_else(|| {
                    let mut error = self.error(
                        FailureKind::Config(ConfigKind::ValueInstantiation),
                        "enum creator rejected value",
                    );
                    error.causes.push(ConfigurationCause {
                        kind: FailureKind::IllegalArgument,
                        message: if enum_class.is_empty() {
                            error.message.to_owned()
                        } else {
                            Some(format!("No enum constant {enum_class}.{}", text.to_uppercase()))
                        },
                    });
                    error
                })
            })
            .transpose()
    }
    fn time_enabled(&self) -> Result<()> {
        if self.context().profile == Profile::Marathonfile {
            Ok(())
        } else {
            Err(self.error(
                FailureKind::Config(ConfigKind::InvalidDefinition),
                "JavaTimeModule is not registered in this profile",
            ))
        }
    }
    fn check_time_number(&self) -> Result<()> {
        if let Tok::Float { cleaned, .. } = self.token()?.tok
            && let Some(decimal) = jcompat::jnum::Decimal::parse(&cleaned)
            && decimal.checked_seconds_and_nanos().is_none()
        {
            return Err(self.error(
                JSON_MAPPING,
                "D9: epoch/duration seconds are outside the signed 64-bit range",
            ));
        }
        Ok(())
    }
    pub fn instant(&self) -> Result<Option<Instant>> {
        if self.is_null()? {
            return Ok(None);
        }
        self.time_enabled()?;
        self.check_time_number()?;
        self.scalar(crate::scalar::instant_value)
            .map_err(|error| self.time_error_cause(error, true))?
            .map(|s| jcompat::jtime::parse_instant(&s).ok_or_else(|| self.error(INVALID_FORMAT, "invalid instant")))
            .transpose()
    }
    pub fn duration(&self) -> Result<Option<Duration>> {
        if self.is_null()? {
            return Ok(None);
        }
        self.time_enabled()?;
        self.check_time_number()?;
        self.scalar(crate::scalar::duration_value)
            .map_err(|error| self.time_error_cause(error, false))?
            .map(|s| jcompat::jtime::parse_duration(&s).ok_or_else(|| self.error(INVALID_FORMAT, "invalid duration")))
            .transpose()
    }
    fn time_error_cause(&self, mut error: Box<ConfigurationError>, instant: bool) -> Box<ConfigurationError> {
        if error.kind == INVALID_FORMAT
            && let Ok(Token {
                tok: Tok::Str(text), ..
            }) = self.token()
        {
            let causes = if instant {
                let text = text.to_owned();
                jcompat::jtime::parse_instant_detailed(snakeyaml::resolve::java_trim(&text))
                    .err()
                    .map(|e| e.chain)
                    .unwrap_or_default()
            } else {
                vec![(
                    FailureKind::DateTimeParse,
                    String::from("Text cannot be parsed to a Duration"),
                )]
            };
            for (class, message) in causes {
                error.causes.push(ConfigurationCause {
                    kind: class,
                    message: Some(message),
                });
            }
        }
        error
    }
    pub fn file(&self) -> Result<Option<PathBuf>> {
        let t = self.token()?;
        if matches!(t.tok, Tok::Null(_)) {
            return Ok(None);
        }
        if matches!(t.tok, Tok::StartObject | Tok::StartArray) {
            if self.context().profile != Profile::Marathonfile {
                return Err(self.error(JSON_MAPPING, "expected path text"));
            }
            // FileDeserializer passes valueAsString to File(String), which
            // throws on a container's null text. Builder outputDir escapes
            // directly; Jackson wraps constructor properties at nested paths.
            let top = matches!(self.path.as_slice(), [PathPart::Key(key)] if key == &"outputDir".to_owned());
            let mut error = self.error(
                if top { FailureKind::NullReference } else { JSON_MAPPING },
                "expected path text",
            );
            if top {
                error.message = None;
            } else {
                error.causes.push(ConfigurationCause {
                    kind: FailureKind::NullReference,
                    message: None,
                });
            }
            return Err(error);
        }
        // FileDeserializer reads valueAsString, including the original !!binary scalar.
        let text = if self.context().profile == Profile::Marathonfile {
            t.java_text
                .ok_or_else(|| self.error(JSON_MAPPING, "expected path text"))?
        } else {
            self.required(self.string()?)?
        };
        let text = text.to_owned();
        if self.context().profile != Profile::Marathonfile {
            let text = java_trim(&text);
            return Ok((!text.is_empty()).then(|| jcompat::file_manager::file_path(text)));
        }
        if text.contains('\0') {
            return Err(self.error(JSON_MAPPING, "path contains NUL"));
        }
        if text.starts_with('/') {
            return Ok(Some(jcompat::file_manager::absolute_file_path(&text, "/")));
        }
        let base = &self.context().marathonfile_dir;
        let path =
            crate::paths::canonical_file(&base.join(text)).map_err(|e| self.error(JSON_MAPPING, e.to_string()))?;
        let text = path
            .to_str()
            .ok_or_else(|| self.error(JSON_MAPPING, "canonical path must be UTF-8"))?;
        Ok(Some(PathBuf::from(text)))
    }
    pub fn regex(&self) -> Result<Option<JavaRegex>> {
        let token = self.token()?;
        if matches!(token.tok, Tok::Null(_)) {
            return Ok(None);
        }
        // D9 (exotic syntax): the Kotlin object form `{pattern, options}`
        // is not supported; only string patterns are. Name the field so
        // the rejection is actionable.
        if matches!(self.node.kind, NodeKind::Map { .. }) {
            let field = self
                .path
                .iter()
                .map(|p| match p {
                    PathPart::Key(k) => k.to_owned(),
                    PathPart::Index(i) => format!("[{i}]"),
                })
                .collect::<Vec<_>>()
                .join(".");
            let mut error = self.error(
                JSON_MAPPING,
                format!(
                    "{field}: only string patterns are supported for a Regex; \
                     the object form {{pattern, options}} is not supported (D9)"
                ),
            );
            error.causes.push(ConfigurationCause {
                kind: FailureKind::NullReference,
                message: error.message.to_owned(),
            });
            return Err(error);
        }
        let Tok::Str(s) = token.tok else {
            // Kotlin's RegexDeserializer reads a tree and names its JsonNodeType.
            let kind = match token.tok {
                Tok::Int { .. } | Tok::Float { .. } => "NUMBER",
                Tok::True(_) | Tok::False(_) => "BOOLEAN",
                Tok::Embedded(_) => "BINARY",
                Tok::StartArray => "ARRAY",
                Tok::StartObject => "OBJECT",
                _ => "unknown",
            };
            let mut error = self.error(
                JSON_MAPPING,
                format!("Expected a string or an object to deserialize a Regex, but type was {kind}"),
            );
            let class = if matches!(self.node.kind, NodeKind::Map { .. }) {
                FailureKind::NullReference
            } else {
                FailureKind::IllegalState
            };
            error.causes.push(ConfigurationCause {
                kind: class,
                message: error.message.to_owned(),
            });
            return Err(error);
        };
        let pattern = s.to_owned();
        let field = self
            .path
            .iter()
            .map(|p| match p {
                PathPart::Key(k) => k.to_owned(),
                PathPart::Index(i) => format!("[{i}]"),
            })
            .collect::<Vec<_>>()
            .join(".");
        JavaRegex::compile(&pattern, &field).map(Some).map_err(|e| {
            let mut error = self.error(FailureKind::Config(ConfigKind::JsonMapping), e.to_string());
            error.causes.push(ConfigurationCause {
                kind: FailureKind::PatternSyntax,
                message: error.message.to_owned(),
            });
            error
        })
    }
    pub fn list<T>(&self, mut bind: impl FnMut(Self) -> Result<T>) -> Result<Option<Vec<T>>> {
        if self.is_null()? {
            return Ok(None);
        }
        match &self.node.kind {
            NodeKind::Seq { items, complete } => {
                let mut values = Vec::new();
                for (i, node) in items.iter().enumerate() {
                    values.push(bind(self.child(node, PathPart::Index(i)))?);
                }
                if !complete {
                    let mut path = self.path.clone();
                    path.push(PathPart::Index(items.len()));
                    return Err(self.binder.stream_error(path, Some(self.node.span), Some(JSON_MAPPING)));
                }
                Ok(Some(values))
            }
            _ => Err(self.error(
                if matches!(self.token()?.tok, Tok::Str(ref s) if java_trim(&s.to_owned()).is_empty()) {
                    INVALID_FORMAT
                } else {
                    MISMATCHED
                },
                "expected an array",
            )),
        }
    }
    /// Ordered map with replacement in the first occurrence's position, like LinkedHashMap.
    pub fn map<T>(&self, mut bind: impl FnMut(Self) -> Result<T>) -> Result<Option<Vec<(String, T)>>> {
        if self.is_null()? {
            return Ok(None);
        }
        if !matches!(self.node.kind, NodeKind::Map { .. }) {
            return Err(self.error(
                if matches!(self.token()?.tok, Tok::Str(ref s) if java_trim(&s.to_owned()).is_empty()) {
                    INVALID_FORMAT
                } else {
                    MISMATCHED
                },
                "expected a map",
            ));
        }
        let mut object = self.object()?;
        let mut out: Vec<(String, T)> = Vec::new();
        while let Some((name, value)) = object.next_field().map_err(wrap_stream)? {
            let value = bind(value).map_err(wrap_stream)?;
            if let Some(entry) = out.iter_mut().find(|(key, _)| key == &name) {
                entry.1 = value;
            } else {
                out.push((name, value));
            }
        }
        Ok(Some(out))
    }
    fn polymorphic_array<T>(
        &self,
        items: &'a [Node],
        complete: bool,
        dispatch: &mut impl FnMut(&str, Self) -> Option<Result<T>>,
    ) -> Result<Option<T>> {
        let Some(first) = items.first() else {
            return Err(if complete {
                self.error(MISMATCHED, "expected a wrapper-array type id")
            } else {
                self.binder.stream_error(self.path.clone(), Some(self.node.span), None)
            });
        };
        let id = self.child(first, PathPart::Index(0));
        let id = id
            .string()?
            .ok_or_else(|| id.error(MISMATCHED, "expected a wrapper-array type id"))?;
        let id = id.to_owned();
        // Jackson resolves the id before advancing to the body. END_ARRAY is
        // a null subtype value; an unfinished stream fails on that advance.
        let mut body = self.child(items.get(1).unwrap_or(first), PathPart::Index(1));
        body.wrapper_end = items.len() == 1;
        let body_node = body.node;
        let bound = dispatch(&id, body).ok_or_else(|| self.error(INVALID_TYPE, "unknown type id"))?;
        let bound = self.binder.settle(body_node, bound);
        if items.len() == 1 {
            return if complete {
                Ok(None)
            } else {
                Err(self.binder.stream_error(self.path.clone(), Some(self.node.span), None))
            };
        }
        let bound = bound?;
        if items.len() != 2 || !complete {
            return Err(if complete {
                self.error(MISMATCHED, "expected the end of a wrapper array")
            } else {
                self.binder.stream_error(self.path.clone(), Some(self.node.span), None)
            });
        }
        Ok(Some(bound))
    }

    /// Late type properties are buffered. Only the occurrence used for dispatch
    /// is hidden; subsequent `type` fields are ordinary subtype properties.
    pub fn polymorphic<T>(&self, mut dispatch: impl FnMut(&str, Self) -> Option<Result<T>>) -> Result<Option<T>> {
        if self.node.tag.is_none()
            && let NodeKind::Seq { items, complete } = &self.node.kind
        {
            return self.polymorphic_array(items, *complete, &mut dispatch);
        }
        self.nullable(|value| {
            if let Some(tag) = &value.node.tag {
                let id = tag.trim_start_matches('!');
                return dispatch(id, value.clone())
                    .unwrap_or_else(|| Err(value.error(INVALID_TYPE, "unknown native type id")));
            }
            if !matches!(value.node.kind, NodeKind::Map { .. } | NodeKind::Cut) {
                return Err(value.error(INVALID_TYPE, "expected a polymorphic object"));
            }
            let mut object = value.object()?;
            while let Some((name, child)) = object.next_field()? {
                if name == "type" {
                    let id = match child.token()?.tok {
                        Tok::Str(s) => Some(s.to_owned()),
                        Tok::Int { text, .. } | Tok::Float { text, .. } => Some(text),
                        Tok::True(_) => Some("true".into()),
                        Tok::False(_) => Some("false".into()),
                        _ => None,
                    };
                    if let Some(id) = id {
                        let mut subtype = value.clone();
                        subtype.hidden_type = Some(object.pos - 1);
                        return dispatch(&id, subtype)
                            .unwrap_or_else(|| Err(child.error(INVALID_TYPE, "unknown type id")));
                    }
                    // Jackson buffers null/container type entries until a usable id
                    // appears; those entries remain visible to that subtype.
                }
                child.validate_tree()?;
            }
            Err(value.error(INVALID_TYPE, "missing type id"))
        })
    }
}

pub struct Object<'a> {
    value: Value<'a>,
    entries: &'a [(Node, Node)],
    complete: bool,
    pos: usize,
}
impl<'a> Object<'a> {
    pub fn next_field(&mut self) -> Result<Option<(String, Value<'a>)>> {
        while let Some((key, node)) = self.entries.get(self.pos) {
            self.pos += 1;
            let name = self.value.key(key)?;
            let value = self.value.child(node, PathPart::Key(name.clone()));
            if self.value.hidden_type == Some(self.pos - 1) {
                value.validate_tree()?;
                continue;
            }
            return Ok(Some((name, value)));
        }
        if !self.complete {
            return Err(self
                .value
                .binder
                .stream_error(self.value.path.clone(), Some(self.value.node.span), None));
        }
        Ok(None)
    }
}

/// A creator parameter: absent uses its schema default; explicit null replaces it.
/// Required references are checked after duplicate assignments (D9).
pub struct Field<'a, T> {
    value: Option<T>,
    source: Value<'a>,
    missing: bool,
    null_value: Option<fn() -> T>,
}
impl<'a, T> Field<'a, T> {
    fn new(parent: &Value<'a>, default: Option<T>) -> Self {
        Self {
            value: default,
            source: parent.clone(),
            missing: true,
            null_value: None,
        }
    }
    /// Kotlin non-null reference: missing retains the default (None means required),
    /// explicit null is rejected by `required`, even when a default exists.
    pub fn reference(parent: &Value<'a>, default: Option<T>) -> Self {
        Self::new(parent, default)
    }
    /// Kotlin nullable parameter: missing retains the default; explicit null
    /// replaces it with None. Finish with `optional`, and use `Value::nullable`
    /// around primitive/composite binders so null is not coerced to zero.
    pub fn nullable(parent: &Value<'a>, default: Option<T>) -> Self {
        Self::new(parent, default)
    }
    pub fn set(&mut self, source: Value<'a>, value: Option<T>) {
        self.value = value.or_else(|| self.null_value.map(|zero| zero()));
        self.source = source;
        self.missing = false;
    }
    pub fn optional(self) -> Option<T> {
        self.value
    }
    pub fn required(self, name: &str) -> Result<T> {
        self.value.ok_or_else(|| {
            let mut e = self.source.error(MISSING_KOTLIN, "missing or null non-null parameter");
            if self.missing {
                e.path.push(PathPart::Key(name.into()));
            }
            e
        })
    }
}

impl<'a, T: Default> Field<'a, T> {
    /// Kotlin primitive: missing retains the schema default; explicit null uses
    /// the primitive zero (`T::default`), independent of that schema default.
    /// Finish with `required`. For a primitive without a Kotlin default, pass zero.
    pub fn primitive(parent: &Value<'a>, default: T) -> Self {
        let mut field = Self::new(parent, Some(default));
        field.null_value = Some(T::default);
        field
    }
}
