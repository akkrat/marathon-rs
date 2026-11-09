//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleDeviceTestRunner.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FatalKind {
    WindowSystem,
    Io,
    Assertion,
    BootstrapMethod,
    ClassCircularity,
    ClassFormat,
    General,
    Initialization,
    IllegalAccess,
    IncompatibleClassChange,
    Instantiation,
    Internal,
    Linkage,
    NoClassDefFound,
    NoSuchField,
    NoSuchMethod,
    OutOfMemory,
    StackOverflow,
    ThreadDeath,
    Unknown,
    UnsatisfiedLink,
    UnsupportedClassVersion,
    Verify,
    VirtualMachine,
    AnnotationFormat,
    CoderMalfunction,
    ServiceConfiguration,
    Zip,
    ParserFactory,
    StreamFactory,
    TransformerFactory,
    NotImplemented,
    ReflectionUnsupported,
}

impl FatalKind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::WindowSystem => "fatal window system failure",
            Self::Io => "fatal I/O error",
            Self::Assertion => "fatal assertion failure",
            Self::BootstrapMethod => "fatal bootstrap method failure",
            Self::ClassCircularity => "fatal circular type dependency",
            Self::ClassFormat => "fatal invalid class format",
            Self::General => "fatal failure",
            Self::Initialization => "fatal initialization failure",
            Self::IllegalAccess => "fatal illegal member access",
            Self::IncompatibleClassChange => "fatal incompatible type change",
            Self::Instantiation => "fatal instance creation failure",
            Self::Internal => "fatal internal failure",
            Self::Linkage => "fatal linking failure",
            Self::NoClassDefFound => "fatal missing type definition",
            Self::NoSuchField => "fatal missing field",
            Self::NoSuchMethod => "fatal missing method",
            Self::OutOfMemory => "fatal out of memory",
            Self::StackOverflow => "fatal stack overflow",
            Self::ThreadDeath => "fatal thread termination",
            Self::Unknown => "fatal unknown failure",
            Self::UnsatisfiedLink => "fatal unresolved native link",
            Self::UnsupportedClassVersion => "fatal unsupported class version",
            Self::Verify => "fatal verification failure",
            Self::VirtualMachine => "fatal virtual machine failure",
            Self::AnnotationFormat => "fatal invalid annotation format",
            Self::CoderMalfunction => "fatal text codec malfunction",
            Self::ServiceConfiguration => "fatal service configuration failure",
            Self::Zip => "fatal archive failure",
            Self::ParserFactory => "fatal XML parser factory failure",
            Self::StreamFactory => "fatal XML stream factory failure",
            Self::TransformerFactory => "fatal XML transformer factory failure",
            Self::NotImplemented => "fatal unimplemented operation",
            Self::ReflectionUnsupported => "fatal unsupported reflection",
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::WindowSystem => "fatal.window-system",
            Self::Io => "fatal.io",
            Self::Assertion => "fatal.assertion",
            Self::BootstrapMethod => "fatal.bootstrap-method",
            Self::ClassCircularity => "fatal.class-circularity",
            Self::ClassFormat => "fatal.class-format",
            Self::General => "fatal.general",
            Self::Initialization => "fatal.initialization",
            Self::IllegalAccess => "fatal.illegal-access",
            Self::IncompatibleClassChange => "fatal.incompatible-class-change",
            Self::Instantiation => "fatal.instantiation",
            Self::Internal => "fatal.internal",
            Self::Linkage => "fatal.linkage",
            Self::NoClassDefFound => "fatal.no-class-def-found",
            Self::NoSuchField => "fatal.no-such-field",
            Self::NoSuchMethod => "fatal.no-such-method",
            Self::OutOfMemory => "fatal.out-of-memory",
            Self::StackOverflow => "fatal.stack-overflow",
            Self::ThreadDeath => "fatal.thread-death",
            Self::Unknown => "fatal.unknown",
            Self::UnsatisfiedLink => "fatal.unsatisfied-link",
            Self::UnsupportedClassVersion => "fatal.unsupported-class-version",
            Self::Verify => "fatal.verify",
            Self::VirtualMachine => "fatal.virtual-machine",
            Self::AnnotationFormat => "fatal.annotation-format",
            Self::CoderMalfunction => "fatal.coder-malfunction",
            Self::ServiceConfiguration => "fatal.service-configuration",
            Self::Zip => "fatal.zip",
            Self::ParserFactory => "fatal.parser-factory",
            Self::StreamFactory => "fatal.stream-factory",
            Self::TransformerFactory => "fatal.transformer-factory",
            Self::NotImplemented => "fatal.not-implemented",
            Self::ReflectionUnsupported => "fatal.reflection-unsupported",
        }
    }
}

impl std::fmt::Display for FatalKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}
