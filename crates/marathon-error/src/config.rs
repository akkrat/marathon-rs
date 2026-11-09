//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConfigKind {
    Rejected,
    JsonParse,
    StreamConstraints,
    JsonEOF,
    JsonMapping,
    InvalidDefinition,
    InvalidFormat,
    InvalidTypeId,
    MismatchedInput,
    UnrecognizedProperty,
    ValueInstantiation,
    JacksonYAMLParse,
    MarkedYAML,
    MissingKotlinParameter,
    YAML,
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
            Self::JsonEOF => "com.fasterxml.jackson.core.io.JsonEOFException",
            Self::JsonMapping => "com.fasterxml.jackson.databind.JsonMappingException",
            Self::InvalidDefinition => "com.fasterxml.jackson.databind.exc.InvalidDefinitionException",
            Self::InvalidFormat => "com.fasterxml.jackson.databind.exc.InvalidFormatException",
            Self::InvalidTypeId => "com.fasterxml.jackson.databind.exc.InvalidTypeIdException",
            Self::MismatchedInput => "com.fasterxml.jackson.databind.exc.MismatchedInputException",
            Self::UnrecognizedProperty => "com.fasterxml.jackson.databind.exc.UnrecognizedPropertyException",
            Self::ValueInstantiation => "com.fasterxml.jackson.databind.exc.ValueInstantiationException",
            Self::JacksonYAMLParse => "com.fasterxml.jackson.dataformat.yaml.JacksonYAMLParseException",
            Self::MarkedYAML => "com.fasterxml.jackson.dataformat.yaml.snakeyaml.error.MarkedYAMLException",
            Self::MissingKotlinParameter => "com.fasterxml.jackson.module.kotlin.MissingKotlinParameterException",
            Self::YAML => "org.yaml.snakeyaml.error.YAMLException",
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
            Self::Rejected | Self::YAML | Self::Parser | Self::Reader | Self::Scanner
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
