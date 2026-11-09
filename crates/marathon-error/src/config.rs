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
    pub const fn legacy_class(&self) -> &'static str {
        match self {
            Self::Rejected => "com.malinskiy.marathon.config.exceptions.ConfigurationException",
            Self::JsonParse => "com.fasterxml.jackson.core.JsonParseException",
            Self::StreamConstraints => "com.fasterxml.jackson.core.exc.StreamConstraintsException",
            Self::JsonEof => "com.fasterxml.jackson.core.io.JsonEOFException",
            Self::JsonMapping => "com.fasterxml.jackson.databind.JsonMappingException",
            Self::InvalidDefinition => "com.fasterxml.jackson.databind.exc.InvalidDefinitionException",
            Self::InvalidFormat => "com.fasterxml.jackson.databind.exc.InvalidFormatException",
            Self::InvalidTypeId => "com.fasterxml.jackson.databind.exc.InvalidTypeIdException",
            Self::MismatchedInput => "com.fasterxml.jackson.databind.exc.MismatchedInputException",
            Self::UnrecognizedProperty => "com.fasterxml.jackson.databind.exc.UnrecognizedPropertyException",
            Self::ValueInstantiation => "com.fasterxml.jackson.databind.exc.ValueInstantiationException",
            Self::YamlParse => "com.fasterxml.jackson.dataformat.yaml.JacksonYAMLParseException",
            Self::YamlMarked => "com.fasterxml.jackson.dataformat.yaml.snakeyaml.error.MarkedYAMLException",
            Self::MissingParameter => "com.fasterxml.jackson.module.kotlin.MissingKotlinParameterException",
            Self::Yaml => "org.yaml.snakeyaml.error.YAMLException",
            Self::Parser => "org.yaml.snakeyaml.parser.ParserException",
            Self::Reader => "org.yaml.snakeyaml.reader.ReaderException",
            Self::Scanner => "org.yaml.snakeyaml.scanner.ScannerException",
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
    pub const fn legacy_class(self) -> &'static str {
        match self {
            Self::Configuration => ConfigKind::Rejected.legacy_class(),
            Self::FileNotFound => crate::FailureKind::FileNotFound.legacy_class(),
            Self::Zip => crate::FailureKind::Zip.legacy_class(),
            Self::Io => crate::FailureKind::Io.legacy_class(),
            Self::NoSuchFile => crate::FailureKind::NoSuchFile.legacy_class(),
        }
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
            assert_eq!(kind.legacy_class(), kind.failure_kind().legacy_class());
        }
    }
}
