//! Port of the failure vocabulary used by core/src/main/kotlin/com/malinskiy/marathon/exceptions/DeviceLostException.kt.
//! Approved D59 descriptions, independent of the production formatting table.
use marathon_error::*;

#[test]
fn every_concrete_kind_displays_its_approved_description() {
    for (kind, expected, identifier) in [
        (FailureKind::Security, "security restriction", "security"),
        (FailureKind::ReplayClock, "clock replay failure", "replay-clock"),
        (FailureKind::DeviceLost, "device lost", "device-lost"),
        (FailureKind::DeviceSetup, "device setup failure", "device-setup"),
        (
            FailureKind::IncompatibleDevice,
            "incompatible device",
            "incompatible-device",
        ),
        (FailureKind::NoDevices, "no devices available", "no-devices"),
        (FailureKind::NoTests, "no tests found", "no-tests"),
        (
            FailureKind::BatchExecution,
            "test batch execution failure",
            "batch-execution",
        ),
        (FailureKind::TestParsing, "test discovery failure", "test-parsing"),
        (FailureKind::Transfer, "file transfer failure", "transfer"),
        (FailureKind::Eof, "unexpected end of input", "eof"),
        (FailureKind::FileNotFound, "file not found", "file-not-found"),
        (FailureKind::Io, "I/O error", "io"),
        (FailureKind::InterruptedIo, "interrupted I/O", "interrupted-io"),
        (FailureKind::UncheckedIo, "unchecked I/O error", "unchecked-io"),
        (FailureKind::Arithmetic, "arithmetic failure", "arithmetic"),
        (
            FailureKind::ArrayIndexOutOfBounds,
            "array index out of bounds",
            "array-index-out-of-bounds",
        ),
        (FailureKind::ClassCast, "incompatible value type", "class-cast"),
        (FailureKind::Generic, "failure", "generic"),
        (FailureKind::IllegalArgument, "invalid argument", "illegal-argument"),
        (FailureKind::IllegalState, "invalid state", "illegal-state"),
        (
            FailureKind::IndexOutOfBounds,
            "index out of bounds",
            "index-out-of-bounds",
        ),
        (
            FailureKind::NegativeArraySize,
            "negative array size",
            "negative-array-size",
        ),
        (FailureKind::NullReference, "missing value", "null-reference"),
        (FailureKind::NumberFormat, "invalid number", "number-format"),
        (FailureKind::Runtime, "runtime failure", "runtime"),
        (
            FailureKind::StringIndexOutOfBounds,
            "string index out of bounds",
            "string-index-out-of-bounds",
        ),
        (
            FailureKind::UnsupportedOperation,
            "unsupported operation",
            "unsupported-operation",
        ),
        (FailureKind::Bind, "socket bind failure", "bind"),
        (FailureKind::Connect, "connection failure", "connect"),
        (FailureKind::NoRouteToHost, "no route to host", "no-route-to-host"),
        (FailureKind::PortUnreachable, "unreachable port", "port-unreachable"),
        (FailureKind::Protocol, "protocol failure", "protocol"),
        (FailureKind::Socket, "socket failure", "socket"),
        (FailureKind::SocketTimeout, "socket timed out", "socket-timeout"),
        (FailureKind::UnknownHost, "unknown host", "unknown-host"),
        (FailureKind::AccessDenied, "access denied", "access-denied"),
        (
            FailureKind::FileAlreadyExists,
            "file already exists",
            "file-already-exists",
        ),
        (FailureKind::FileSystem, "filesystem failure", "file-system"),
        (FailureKind::InvalidPath, "invalid path", "invalid-path"),
        (FailureKind::NoSuchFile, "no such file or directory", "no-such-file"),
        (
            FailureKind::GeneralSecurity,
            "security operation failure",
            "general-security",
        ),
        (FailureKind::DateTime, "invalid date or time", "date-time"),
        (
            FailureKind::DateTimeParse,
            "date or time parsing failure",
            "date-time-parse",
        ),
        (FailureKind::NoSuchElement, "missing element", "no-such-element"),
        (FailureKind::Cancelled, "cancelled", "cancelled"),
        (FailureKind::Timeout, "timed out", "timeout"),
        (
            FailureKind::PatternSyntax,
            "invalid regular expression",
            "pattern-syntax",
        ),
        (FailureKind::Zip, "archive failure", "zip"),
        (
            FailureKind::UninitializedProperty,
            "uninitialized property",
            "uninitialized-property",
        ),
        (
            FailureKind::CopyDestinationExists,
            "copy destination already exists",
            "copy-destination-exists",
        ),
        (
            FailureKind::CopySourceMissing,
            "copy source missing",
            "copy-source-missing",
        ),
        (FailureKind::JobCancelled, "job cancelled", "job-cancelled"),
        (
            FailureKind::TimeoutCancelled,
            "operation cancelled after timeout",
            "timeout-cancelled",
        ),
        (
            FailureKind::ChannelReceiveClosed,
            "receive channel closed",
            "channel-receive-closed",
        ),
        (
            FailureKind::ChannelSendClosed,
            "send channel closed",
            "channel-send-closed",
        ),
        (FailureKind::Replay, "replay failure", "replay"),
        (
            FailureKind::DependencyCreation,
            "dependency creation failure",
            "dependency-creation",
        ),
        (
            FailureKind::MissingDefinition,
            "missing dependency definition",
            "missing-definition",
        ),
        (
            FailureKind::Adb(AdbKind::PullFailed),
            "ADB pull failure",
            "adb.pull-failed",
        ),
        (
            FailureKind::Adb(AdbKind::PushFailed),
            "ADB push failure",
            "adb.push-failed",
        ),
        (
            FailureKind::Adb(AdbKind::RequestRejected),
            "ADB request rejected",
            "adb.request-rejected",
        ),
        (
            FailureKind::Adb(AdbKind::RequestValidation),
            "invalid ADB request",
            "adb.request-validation",
        ),
        (
            FailureKind::Adb(AdbKind::UnsupportedForwardingSpec),
            "unsupported ADB forwarding specification",
            "adb.unsupported-forwarding-spec",
        ),
        (
            FailureKind::Adb(AdbKind::UnsupportedImageProtocol),
            "unsupported ADB image protocol",
            "adb.unsupported-image-protocol",
        ),
        (
            FailureKind::Adb(AdbKind::UnsupportedSyncProtocol),
            "unsupported ADB sync protocol",
            "adb.unsupported-sync-protocol",
        ),
        (
            FailureKind::Adb(AdbKind::ConnectRefused),
            "ADB connection refused",
            "adb.connect-refused",
        ),
        (
            FailureKind::Adb(AdbKind::ConnectTimeout),
            "ADB connection timed out",
            "adb.connect-timeout",
        ),
        (
            FailureKind::Adb(AdbKind::ClosedChannel),
            "ADB channel closed",
            "adb.closed-channel",
        ),
        (
            FailureKind::Adb(AdbKind::TransportClosed),
            "ADB transport closed",
            "adb.transport-closed",
        ),
        (FailureKind::Dex(DexKind::Decode), "DEX decoding failure", "dex.decode"),
        (
            FailureKind::Config(ConfigKind::Rejected),
            "configuration rejected",
            "config.rejected",
        ),
        (
            FailureKind::Config(ConfigKind::JsonParse),
            "JSON parsing failure",
            "config.json-parse",
        ),
        (
            FailureKind::Config(ConfigKind::StreamConstraints),
            "input stream limit exceeded",
            "config.stream-constraints",
        ),
        (
            FailureKind::Config(ConfigKind::JsonEof),
            "unexpected end of JSON input",
            "config.json-eof",
        ),
        (
            FailureKind::Config(ConfigKind::JsonMapping),
            "configuration mapping failure",
            "config.json-mapping",
        ),
        (
            FailureKind::Config(ConfigKind::InvalidDefinition),
            "invalid configuration definition",
            "config.invalid-definition",
        ),
        (
            FailureKind::Config(ConfigKind::InvalidFormat),
            "invalid configuration value format",
            "config.invalid-format",
        ),
        (
            FailureKind::Config(ConfigKind::InvalidTypeId),
            "invalid configuration type identifier",
            "config.invalid-type-id",
        ),
        (
            FailureKind::Config(ConfigKind::MismatchedInput),
            "configuration input type mismatch",
            "config.mismatched-input",
        ),
        (
            FailureKind::Config(ConfigKind::UnrecognizedProperty),
            "unrecognized configuration property",
            "config.unrecognized-property",
        ),
        (
            FailureKind::Config(ConfigKind::ValueInstantiation),
            "configuration value construction failure",
            "config.value-instantiation",
        ),
        (
            FailureKind::Config(ConfigKind::YamlParse),
            "YAML decoding failure",
            "config.yaml-parse",
        ),
        (
            FailureKind::Config(ConfigKind::YamlMarked),
            "YAML error",
            "config.yaml-marked",
        ),
        (
            FailureKind::Config(ConfigKind::MissingParameter),
            "missing required configuration parameter",
            "config.missing-parameter",
        ),
        (FailureKind::Config(ConfigKind::Yaml), "YAML failure", "config.yaml"),
        (
            FailureKind::Config(ConfigKind::Parser),
            "YAML parsing failure",
            "config.parser",
        ),
        (
            FailureKind::Config(ConfigKind::Reader),
            "YAML reading failure",
            "config.reader",
        ),
        (
            FailureKind::Config(ConfigKind::Scanner),
            "YAML scanning failure",
            "config.scanner",
        ),
        (
            FailureKind::Ssh(SshKind::KeyDecryptionFailed),
            "SSH key decryption failure",
            "ssh.key-decryption-failed",
        ),
        (
            FailureKind::Ssh(SshKind::Base64Decoding),
            "SSH base64 decoding failure",
            "ssh.base64-decoding",
        ),
        (FailureKind::Ssh(SshKind::Buffer), "SSH buffer failure", "ssh.buffer"),
        (
            FailureKind::Ssh(SshKind::CircularBuffer),
            "SSH circular buffer failure",
            "ssh.circular-buffer",
        ),
        (FailureKind::Ssh(SshKind::General), "SSH failure", "ssh.general"),
        (
            FailureKind::Ssh(SshKind::Connection),
            "SSH connection failure",
            "ssh.connection",
        ),
        (
            FailureKind::Ssh(SshKind::OpenChannel),
            "SSH channel opening failure",
            "ssh.open-channel",
        ),
        (
            FailureKind::Ssh(SshKind::Transport),
            "SSH transport failure",
            "ssh.transport",
        ),
        (
            FailureKind::Ssh(SshKind::Authentication),
            "SSH authentication failed",
            "ssh.authentication",
        ),
        (
            FailureKind::Ssh(SshKind::Encryption),
            "SSH key encryption failure",
            "ssh.encryption",
        ),
        (FailureKind::Ssh(SshKind::Pem), "SSH PEM decoding failure", "ssh.pem"),
        (
            FailureKind::Ssh(SshKind::Password),
            "SSH key password failure",
            "ssh.password",
        ),
        (FailureKind::Ssh(SshKind::Pkcs), "SSH PKCS decoding failure", "ssh.pkcs"),
        (
            FailureKind::Analytics(AnalyticsKind::BadRequest),
            "InfluxDB bad request",
            "analytics.bad-request",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::Forbidden),
            "InfluxDB access forbidden",
            "analytics.forbidden",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV2),
            "InfluxDB v2 failure",
            "analytics.influx-v2",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InternalServerError),
            "InfluxDB internal server failure",
            "analytics.internal-server-error",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::NotFound),
            "InfluxDB resource not found",
            "analytics.not-found",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::ServiceUnavailable),
            "InfluxDB service unavailable",
            "analytics.service-unavailable",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::Unauthorized),
            "InfluxDB authentication required",
            "analytics.unauthorized",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::UnprocessableEntity),
            "unprocessable InfluxDB request",
            "analytics.unprocessable-entity",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::FluxCsvParser),
            "Flux CSV parsing failure",
            "analytics.flux-csv-parser",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::FluxQuery),
            "Flux query failure",
            "analytics.flux-query",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::Graphite),
            "Graphite failure",
            "analytics.graphite",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV2Provider),
            "InfluxDB v2 provider failure",
            "analytics.influx-v2-provider",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::JsonEncoding),
            "JSON encoding failure",
            "analytics.json-encoding",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxIo),
            "InfluxDB I/O error",
            "analytics.influx-io",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxMapping),
            "InfluxDB mapping failure",
            "analytics.influx-mapping",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::General,
                retry: RetryDisposition::Retry,
            }),
            "InfluxDB v1 failure",
            "analytics.influx-v1.general",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::AuthorizationFailed,
                retry: RetryDisposition::Retry,
            }),
            "InfluxDB v1 authorization failed",
            "analytics.influx-v1.authorization-failed",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::CacheMaxMemorySizeExceeded,
                retry: RetryDisposition::Retry,
            }),
            "InfluxDB v1 cache memory limit exceeded",
            "analytics.influx-v1.cache-max-memory-size-exceeded",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::DatabaseNotFound,
                retry: RetryDisposition::Retry,
            }),
            "InfluxDB v1 database not found",
            "analytics.influx-v1.database-not-found",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::FieldTypeConflict,
                retry: RetryDisposition::Retry,
            }),
            "InfluxDB v1 field type conflict",
            "analytics.influx-v1.field-type-conflict",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::HintedHandOffQueueNotEmpty,
                retry: RetryDisposition::Retry,
            }),
            "InfluxDB v1 pending handoff queue",
            "analytics.influx-v1.hinted-hand-off-queue-not-empty",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::PointsBeyondRetentionPolicy,
                retry: RetryDisposition::Retry,
            }),
            "InfluxDB v1 points outside retention policy",
            "analytics.influx-v1.points-beyond-retention-policy",
        ),
        (
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::UnableToParse,
                retry: RetryDisposition::Retry,
            }),
            "InfluxDB v1 input parsing failure",
            "analytics.influx-v1.unable-to-parse",
        ),
        (
            FailureKind::Android(AndroidKind::TestAnnotationProducerNotFound),
            "Android test annotation producer not found",
            "android.test-annotation-producer-not-found",
        ),
        (
            FailureKind::Android(AndroidKind::CommandRejected),
            "Android command rejected",
            "android.command-rejected",
        ),
        (
            FailureKind::Android(AndroidKind::DeviceNotSupported),
            "unsupported Android device",
            "android.device-not-supported",
        ),
        (
            FailureKind::Android(AndroidKind::Install),
            "Android installation failure",
            "android.install",
        ),
        (
            FailureKind::Apple(AppleKind::NotAdmitted),
            "Apple device not admitted",
            "apple.not-admitted",
        ),
        (
            FailureKind::Apple(AppleKind::NotBooted),
            "Apple device not booted",
            "apple.not-booted",
        ),
        (
            FailureKind::Apple(AppleKind::TransferStalled),
            "Apple file transfer stalled",
            "apple.transfer-stalled",
        ),
        (
            FailureKind::Apple(AppleKind::SetupDropped),
            "Apple device setup dropped",
            "apple.setup-dropped",
        ),
        (
            FailureKind::Apple(AppleKind::Boot),
            "Apple device boot failure",
            "apple.boot",
        ),
        (
            FailureKind::Apple(AppleKind::Command),
            "Apple command failure",
            "apple.command",
        ),
        (
            FailureKind::Apple(AppleKind::DeviceFailure),
            "Apple device failure",
            "apple.device-failure",
        ),
        (
            FailureKind::Report(ReportKind::AllureResultsWrite),
            "Allure results write failure",
            "report.allure-results-write",
        ),
        (
            FailureKind::Report(ReportKind::ImageIo),
            "image I/O error",
            "report.image-io",
        ),
        (
            FailureKind::Report(ReportKind::ImageMetadata),
            "invalid image metadata",
            "report.image-metadata",
        ),
        (
            FailureKind::Report(ReportKind::Transformer),
            "XML transformation failure",
            "report.transformer",
        ),
        (
            FailureKind::Report(ReportKind::Sax),
            "XML parsing failure",
            "report.sax",
        ),
        (
            FailureKind::Fatal(FatalKind::WindowSystem),
            "fatal window system failure",
            "fatal.window-system",
        ),
        (FailureKind::Fatal(FatalKind::Io), "fatal I/O error", "fatal.io"),
        (
            FailureKind::Fatal(FatalKind::Assertion),
            "fatal assertion failure",
            "fatal.assertion",
        ),
        (
            FailureKind::Fatal(FatalKind::BootstrapMethod),
            "fatal bootstrap method failure",
            "fatal.bootstrap-method",
        ),
        (
            FailureKind::Fatal(FatalKind::ClassCircularity),
            "fatal circular type dependency",
            "fatal.class-circularity",
        ),
        (
            FailureKind::Fatal(FatalKind::ClassFormat),
            "fatal invalid class format",
            "fatal.class-format",
        ),
        (FailureKind::Fatal(FatalKind::General), "fatal failure", "fatal.general"),
        (
            FailureKind::Fatal(FatalKind::Initialization),
            "fatal initialization failure",
            "fatal.initialization",
        ),
        (
            FailureKind::Fatal(FatalKind::IllegalAccess),
            "fatal illegal member access",
            "fatal.illegal-access",
        ),
        (
            FailureKind::Fatal(FatalKind::IncompatibleClassChange),
            "fatal incompatible type change",
            "fatal.incompatible-class-change",
        ),
        (
            FailureKind::Fatal(FatalKind::Instantiation),
            "fatal instance creation failure",
            "fatal.instantiation",
        ),
        (
            FailureKind::Fatal(FatalKind::Internal),
            "fatal internal failure",
            "fatal.internal",
        ),
        (
            FailureKind::Fatal(FatalKind::Linkage),
            "fatal linking failure",
            "fatal.linkage",
        ),
        (
            FailureKind::Fatal(FatalKind::NoClassDefFound),
            "fatal missing type definition",
            "fatal.no-class-def-found",
        ),
        (
            FailureKind::Fatal(FatalKind::NoSuchField),
            "fatal missing field",
            "fatal.no-such-field",
        ),
        (
            FailureKind::Fatal(FatalKind::NoSuchMethod),
            "fatal missing method",
            "fatal.no-such-method",
        ),
        (
            FailureKind::Fatal(FatalKind::OutOfMemory),
            "fatal out of memory",
            "fatal.out-of-memory",
        ),
        (
            FailureKind::Fatal(FatalKind::StackOverflow),
            "fatal stack overflow",
            "fatal.stack-overflow",
        ),
        (
            FailureKind::Fatal(FatalKind::ThreadDeath),
            "fatal thread termination",
            "fatal.thread-death",
        ),
        (
            FailureKind::Fatal(FatalKind::Unknown),
            "fatal unknown failure",
            "fatal.unknown",
        ),
        (
            FailureKind::Fatal(FatalKind::UnsatisfiedLink),
            "fatal unresolved native link",
            "fatal.unsatisfied-link",
        ),
        (
            FailureKind::Fatal(FatalKind::UnsupportedClassVersion),
            "fatal unsupported class version",
            "fatal.unsupported-class-version",
        ),
        (
            FailureKind::Fatal(FatalKind::Verify),
            "fatal verification failure",
            "fatal.verify",
        ),
        (
            FailureKind::Fatal(FatalKind::VirtualMachine),
            "fatal virtual machine failure",
            "fatal.virtual-machine",
        ),
        (
            FailureKind::Fatal(FatalKind::AnnotationFormat),
            "fatal invalid annotation format",
            "fatal.annotation-format",
        ),
        (
            FailureKind::Fatal(FatalKind::CoderMalfunction),
            "fatal text codec malfunction",
            "fatal.coder-malfunction",
        ),
        (
            FailureKind::Fatal(FatalKind::ServiceConfiguration),
            "fatal service configuration failure",
            "fatal.service-configuration",
        ),
        (FailureKind::Fatal(FatalKind::Zip), "fatal archive failure", "fatal.zip"),
        (
            FailureKind::Fatal(FatalKind::ParserFactory),
            "fatal XML parser factory failure",
            "fatal.parser-factory",
        ),
        (
            FailureKind::Fatal(FatalKind::StreamFactory),
            "fatal XML stream factory failure",
            "fatal.stream-factory",
        ),
        (
            FailureKind::Fatal(FatalKind::TransformerFactory),
            "fatal XML transformer factory failure",
            "fatal.transformer-factory",
        ),
        (
            FailureKind::Fatal(FatalKind::NotImplemented),
            "fatal unimplemented operation",
            "fatal.not-implemented",
        ),
        (
            FailureKind::Fatal(FatalKind::ReflectionUnsupported),
            "fatal unsupported reflection",
            "fatal.reflection-unsupported",
        ),
        (
            FailureKind::Injected(InjectedKind::ReplayStep),
            "injected replay step failure",
            "injected.replay-step",
        ),
        (
            FailureKind::Injected(InjectedKind::NotBootedProbe),
            "injected not booted probe failure",
            "injected.not-booted-probe",
        ),
        (
            FailureKind::Injected(InjectedKind::RunTimeout),
            "injected run timeout",
            "injected.run-timeout",
        ),
        (
            FailureKind::Injected(InjectedKind::TransferException),
            "injected transfer failure",
            "injected.transfer-exception",
        ),
        (
            FailureKind::Injected(InjectedKind::CleanupError),
            "injected cleanup failure",
            "injected.cleanup-error",
        ),
        (
            FailureKind::Injected(InjectedKind::ExecutionError),
            "injected execution failure",
            "injected.execution-error",
        ),
        (
            FailureKind::Injected(InjectedKind::After),
            "injected after hook failure",
            "injected.after",
        ),
        (
            FailureKind::Injected(InjectedKind::Callback),
            "injected callback failure",
            "injected.callback",
        ),
        (
            FailureKind::Injected(InjectedKind::Test),
            "injected test marker",
            "injected.test",
        ),
        (
            FailureKind::Injected(InjectedKind::ParserFixture),
            "injected parser fixture failure",
            "injected.parser-fixture",
        ),
        (
            FailureKind::Injected(InjectedKind::NotAdmitted),
            "injected admission failure",
            "injected.not-admitted",
        ),
        (
            FailureKind::Injected(InjectedKind::SinkFailure),
            "injected sink failure",
            "injected.sink-failure",
        ),
        (
            FailureKind::Injected(InjectedKind::TestFailure),
            "injected test failure",
            "injected.test-failure",
        ),
        (
            FailureKind::Injected(InjectedKind::TestTask),
            "injected test task failure",
            "injected.test-task",
        ),
        (
            FailureKind::Injected(InjectedKind::TimeoutCancellation),
            "injected timeout cancellation",
            "injected.timeout-cancellation",
        ),
        (
            FailureKind::Injected(InjectedKind::TransferStalled),
            "injected stalled transfer",
            "injected.transfer-stalled",
        ),
        (
            FailureKind::Injected(InjectedKind::Cancelled),
            "injected cancellation",
            "injected.cancelled",
        ),
        (
            FailureKind::Injected(InjectedKind::Lost),
            "injected device loss",
            "injected.lost",
        ),
        (
            FailureKind::Injected(InjectedKind::Probe),
            "injected probe failure",
            "injected.probe",
        ),
        (
            FailureKind::Injected(InjectedKind::ProbeAssertionError),
            "injected probe assertion failure",
            "injected.probe-assertion-error",
        ),
        (
            FailureKind::Injected(InjectedKind::ProbeExceptionNamedError),
            "injected error named probe failure",
            "injected.probe-exception-named-error",
        ),
        (
            FailureKind::Injected(InjectedKind::QualificationConfig),
            "injected qualification configuration failure",
            "injected.qualification-config",
        ),
        (
            FailureKind::Injected(InjectedKind::TestBackend),
            "injected backend failure",
            "injected.test-backend",
        ),
        (
            FailureKind::Injected(InjectedKind::TestBarrier),
            "injected barrier failure",
            "injected.test-barrier",
        ),
        (
            FailureKind::Injected(InjectedKind::TestBlocking),
            "injected blocking operation failure",
            "injected.test-blocking",
        ),
        (
            FailureKind::Injected(InjectedKind::TestCarbon),
            "injected carbon failure",
            "injected.test-carbon",
        ),
        (
            FailureKind::Injected(InjectedKind::TestClock),
            "injected clock failure",
            "injected.test-clock",
        ),
        (
            FailureKind::Injected(InjectedKind::TestClose),
            "injected close failure",
            "injected.test-close",
        ),
        (
            FailureKind::Injected(InjectedKind::TestError),
            "injected generic failure",
            "injected.test-error",
        ),
        (
            FailureKind::Injected(InjectedKind::TestFactory),
            "injected factory failure",
            "injected.test-factory",
        ),
        (
            FailureKind::Injected(InjectedKind::TestPeer),
            "injected peer failure",
            "injected.test-peer",
        ),
        (
            FailureKind::Injected(InjectedKind::TestQuery),
            "injected query failure",
            "injected.test-query",
        ),
        (
            FailureKind::Injected(InjectedKind::TestWatchdog),
            "injected watchdog failure",
            "injected.test-watchdog",
        ),
        (
            FailureKind::Injected(InjectedKind::Timeout),
            "injected timeout",
            "injected.timeout",
        ),
    ] {
        assert_eq!(kind.to_string(), expected, "{kind:?}");
        assert_eq!(kind.identifier(), identifier, "{kind:?}");
    }
    assert_eq!(
        FailureKind::External("runner.CustomException".into()).to_string(),
        "runner.CustomException"
    );
}

#[test]
fn validation_aliases_display_the_projected_kind() {
    use marathon_error::config::ValidationKind;
    for kind in [
        ValidationKind::Configuration,
        ValidationKind::FileNotFound,
        ValidationKind::Zip,
        ValidationKind::Io,
        ValidationKind::NoSuchFile,
    ] {
        assert_eq!(kind.to_string(), kind.failure_kind().to_string());
    }
}
