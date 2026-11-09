//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConfigKind {
    Rejected,
    JsonParse,
    StreamConstraints,
    JsonEof,
    JsonMapping,
    InvalidDefinition,
    InvalidFormat,
    InvalidTypeId,
    MismatchedInput,
    UnrecognizedProperty,
    ValueInstantiation,
    YamlParse,
    YamlMarked,
    MissingParameter,
    Yaml,
    Parser,
    Reader,
    Scanner,
}

impl ConfigKind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::Rejected => "configuration rejected",
            Self::JsonParse => "JSON parsing failure",
            Self::StreamConstraints => "input stream limit exceeded",
            Self::JsonEof => "unexpected end of JSON input",
            Self::JsonMapping => "configuration mapping failure",
            Self::InvalidDefinition => "invalid configuration definition",
            Self::InvalidFormat => "invalid configuration value format",
            Self::InvalidTypeId => "invalid configuration type identifier",
            Self::MismatchedInput => "configuration input type mismatch",
            Self::UnrecognizedProperty => "unrecognized configuration property",
            Self::ValueInstantiation => "configuration value construction failure",
            Self::YamlParse => "YAML decoding failure",
            Self::YamlMarked => "YAML error",
            Self::MissingParameter => "missing required configuration parameter",
            Self::Yaml => "YAML failure",
            Self::Parser => "YAML parsing failure",
            Self::Reader => "YAML reading failure",
            Self::Scanner => "YAML scanning failure",
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::Rejected => "config.rejected",
            Self::JsonParse => "config.json-parse",
            Self::StreamConstraints => "config.stream-constraints",
            Self::JsonEof => "config.json-eof",
            Self::JsonMapping => "config.json-mapping",
            Self::InvalidDefinition => "config.invalid-definition",
            Self::InvalidFormat => "config.invalid-format",
            Self::InvalidTypeId => "config.invalid-type-id",
            Self::MismatchedInput => "config.mismatched-input",
            Self::UnrecognizedProperty => "config.unrecognized-property",
            Self::ValueInstantiation => "config.value-instantiation",
            Self::YamlParse => "config.yaml-parse",
            Self::YamlMarked => "config.yaml-marked",
            Self::MissingParameter => "config.missing-parameter",
            Self::Yaml => "config.yaml",
            Self::Parser => "config.parser",
            Self::Reader => "config.reader",
            Self::Scanner => "config.scanner",
        }
    }
}

impl ConfigKind {
    pub fn is_json(self) -> bool {
        !matches!(
            self,
            Self::Rejected | Self::Yaml | Self::Parser | Self::Reader | Self::Scanner
        )
    }
    pub fn is_mapping(self) -> bool {
        matches!(
            self,
            Self::JsonMapping
                | Self::InvalidDefinition
                | Self::InvalidFormat
                | Self::InvalidTypeId
                | Self::MismatchedInput
                | Self::UnrecognizedProperty
                | Self::ValueInstantiation
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValidationKind {
    Configuration,
    FileNotFound,
    Zip,
    Io,
    NoSuchFile,
}
impl ValidationKind {
    pub const fn failure_kind(self) -> crate::FailureKind {
        match self {
            Self::Configuration => crate::FailureKind::Config(ConfigKind::Rejected),
            Self::FileNotFound => crate::FailureKind::FileNotFound,
            Self::Zip => crate::FailureKind::Zip,
            Self::Io => crate::FailureKind::Io,
            Self::NoSuchFile => crate::FailureKind::NoSuchFile,
        }
    }
    pub const fn description(self) -> &'static str {
        match self {
            Self::Configuration => ConfigKind::Rejected.description(),
            Self::FileNotFound => "file not found",
            Self::Zip => "archive failure",
            Self::Io => "I/O error",
            Self::NoSuchFile => "no such file or directory",
        }
    }
}

impl std::fmt::Display for ConfigKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}

impl std::fmt::Display for ValidationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.failure_kind().fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mapping_and_json_catches_keep_distinct_equivalence_classes() {
        assert!(ConfigKind::MissingParameter.is_json());
        assert!(!ConfigKind::MissingParameter.is_mapping());
        assert!(ConfigKind::JsonMapping.is_mapping());
        assert!(ConfigKind::InvalidFormat.is_mapping());
        assert!(!ConfigKind::JsonParse.is_mapping());
        for kind in [
            ConfigKind::Yaml,
            ConfigKind::Reader,
            ConfigKind::Scanner,
            ConfigKind::Parser,
            ConfigKind::Rejected,
        ] {
            assert!(!kind.is_json());
        }
        for kind in [
            ValidationKind::Configuration,
            ValidationKind::FileNotFound,
            ValidationKind::Zip,
            ValidationKind::Io,
            ValidationKind::NoSuchFile,
        ] {
            assert_eq!(kind.description(), kind.failure_kind().description());
        }
    }
}
