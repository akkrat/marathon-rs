//! Port of core/src/test/kotlin/com/malinskiy/marathon/scenario/SuccessScenariosTest.kt
//! (recorded exception values at the replay input boundary).

use marathon_error::{
    AdbKind, AnalyticsKind, AndroidKind, AppleKind, ConfigKind, DexKind, FailureKind, FatalKind, InfluxV1Kind,
    InjectedKind, ReportKind, RetryDisposition, SshKind,
};

/// Exact names in the recorded-input format, never a production renderer.
pub const RECORDED_KINDS: &[(&str, FailureKind)] = &[
    ("java.lang.SecurityException", FailureKind::Security),
    ("ReplayError", FailureKind::ReplayClock),
    (
        "com.malinskiy.marathon.exceptions.DeviceLostException",
        FailureKind::DeviceLost,
    ),
    (
        "com.malinskiy.marathon.exceptions.DeviceSetupException",
        FailureKind::DeviceSetup,
    ),
    (
        "com.malinskiy.marathon.exceptions.IncompatibleDeviceException",
        FailureKind::IncompatibleDevice,
    ),
    (
        "com.malinskiy.marathon.exceptions.NoDevicesException",
        FailureKind::NoDevices,
    ),
    (
        "com.malinskiy.marathon.exceptions.NoTestCasesFoundException",
        FailureKind::NoTests,
    ),
    (
        "com.malinskiy.marathon.exceptions.TestBatchExecutionException",
        FailureKind::BatchExecution,
    ),
    (
        "com.malinskiy.marathon.exceptions.TestParsingException",
        FailureKind::TestParsing,
    ),
    (
        "com.malinskiy.marathon.exceptions.TransferException",
        FailureKind::Transfer,
    ),
    ("java.io.EOFException", FailureKind::Eof),
    ("java.io.FileNotFoundException", FailureKind::FileNotFound),
    ("java.io.IOException", FailureKind::Io),
    ("java.io.InterruptedIOException", FailureKind::InterruptedIo),
    ("java.io.UncheckedIOException", FailureKind::UncheckedIo),
    ("java.lang.ArithmeticException", FailureKind::Arithmetic),
    (
        "java.lang.ArrayIndexOutOfBoundsException",
        FailureKind::ArrayIndexOutOfBounds,
    ),
    ("java.lang.ClassCastException", FailureKind::ClassCast),
    ("java.lang.Exception", FailureKind::Generic),
    ("java.lang.IllegalArgumentException", FailureKind::IllegalArgument),
    ("java.lang.IllegalStateException", FailureKind::IllegalState),
    ("java.lang.IndexOutOfBoundsException", FailureKind::IndexOutOfBounds),
    ("java.lang.NegativeArraySizeException", FailureKind::NegativeArraySize),
    ("java.lang.NullPointerException", FailureKind::NullReference),
    ("java.lang.NumberFormatException", FailureKind::NumberFormat),
    ("java.lang.RuntimeException", FailureKind::Runtime),
    (
        "java.lang.StringIndexOutOfBoundsException",
        FailureKind::StringIndexOutOfBounds,
    ),
    (
        "java.lang.UnsupportedOperationException",
        FailureKind::UnsupportedOperation,
    ),
    ("java.net.BindException", FailureKind::Bind),
    ("java.net.ConnectException", FailureKind::Connect),
    ("java.net.NoRouteToHostException", FailureKind::NoRouteToHost),
    ("java.net.PortUnreachableException", FailureKind::PortUnreachable),
    ("java.net.ProtocolException", FailureKind::Protocol),
    ("java.net.SocketException", FailureKind::Socket),
    ("java.net.SocketTimeoutException", FailureKind::SocketTimeout),
    ("java.net.UnknownHostException", FailureKind::UnknownHost),
    ("java.nio.file.AccessDeniedException", FailureKind::AccessDenied),
    (
        "java.nio.file.FileAlreadyExistsException",
        FailureKind::FileAlreadyExists,
    ),
    ("java.nio.file.FileSystemException", FailureKind::FileSystem),
    ("java.nio.file.InvalidPathException", FailureKind::InvalidPath),
    ("java.nio.file.NoSuchFileException", FailureKind::NoSuchFile),
    ("java.security.GeneralSecurityException", FailureKind::GeneralSecurity),
    ("java.time.DateTimeException", FailureKind::DateTime),
    ("java.time.format.DateTimeParseException", FailureKind::DateTimeParse),
    ("java.util.NoSuchElementException", FailureKind::NoSuchElement),
    ("java.util.concurrent.CancellationException", FailureKind::Cancelled),
    ("java.util.concurrent.TimeoutException", FailureKind::Timeout),
    ("java.util.regex.PatternSyntaxException", FailureKind::PatternSyntax),
    ("java.util.zip.ZipException", FailureKind::Zip),
    (
        "kotlin.UninitializedPropertyAccessException",
        FailureKind::UninitializedProperty,
    ),
    (
        "kotlin.io.FileAlreadyExistsException",
        FailureKind::CopyDestinationExists,
    ),
    ("kotlin.io.NoSuchFileException", FailureKind::CopySourceMissing),
    ("kotlinx.coroutines.JobCancellationException", FailureKind::JobCancelled),
    (
        "kotlinx.coroutines.TimeoutCancellationException",
        FailureKind::TimeoutCancelled,
    ),
    (
        "kotlinx.coroutines.channels.ClosedReceiveChannelException",
        FailureKind::ChannelReceiveClosed,
    ),
    (
        "kotlinx.coroutines.channels.ClosedSendChannelException",
        FailureKind::ChannelSendClosed,
    ),
    ("marathon.ReplayError", FailureKind::Replay),
    (
        "org.koin.core.error.InstanceCreationException",
        FailureKind::DependencyCreation,
    ),
    (
        "org.koin.core.error.NoBeanDefFoundException",
        FailureKind::MissingDefinition,
    ),
    (
        "com.malinskiy.adam.exception.PullFailedException",
        FailureKind::Adb(AdbKind::PullFailed),
    ),
    (
        "com.malinskiy.adam.exception.PushFailedException",
        FailureKind::Adb(AdbKind::PushFailed),
    ),
    (
        "com.malinskiy.adam.exception.RequestRejectedException",
        FailureKind::Adb(AdbKind::RequestRejected),
    ),
    (
        "com.malinskiy.adam.exception.RequestValidationException",
        FailureKind::Adb(AdbKind::RequestValidation),
    ),
    (
        "com.malinskiy.adam.exception.UnsupportedForwardingSpecException",
        FailureKind::Adb(AdbKind::UnsupportedForwardingSpec),
    ),
    (
        "com.malinskiy.adam.exception.UnsupportedImageProtocolException",
        FailureKind::Adb(AdbKind::UnsupportedImageProtocol),
    ),
    (
        "com.malinskiy.adam.exception.UnsupportedSyncProtocolException",
        FailureKind::Adb(AdbKind::UnsupportedSyncProtocol),
    ),
    (
        "io.netty.channel.AbstractChannel$AnnotatedConnectException",
        FailureKind::Adb(AdbKind::ConnectRefused),
    ),
    (
        "io.netty.channel.ConnectTimeoutException",
        FailureKind::Adb(AdbKind::ConnectTimeout),
    ),
    (
        "io.netty.channel.StacklessClosedChannelException",
        FailureKind::Adb(AdbKind::ClosedChannel),
    ),
    (
        "io.vertx.core.impl.NoStackTraceThrowable",
        FailureKind::Adb(AdbKind::TransportClosed),
    ),
    ("com.linkedin.dex.spec.DexException", FailureKind::Dex(DexKind::Decode)),
    (
        "com.malinskiy.marathon.config.exceptions.ConfigurationException",
        FailureKind::Config(ConfigKind::Rejected),
    ),
    (
        "com.fasterxml.jackson.core.JsonParseException",
        FailureKind::Config(ConfigKind::JsonParse),
    ),
    (
        "com.fasterxml.jackson.core.exc.StreamConstraintsException",
        FailureKind::Config(ConfigKind::StreamConstraints),
    ),
    (
        "com.fasterxml.jackson.core.io.JsonEOFException",
        FailureKind::Config(ConfigKind::JsonEof),
    ),
    (
        "com.fasterxml.jackson.databind.JsonMappingException",
        FailureKind::Config(ConfigKind::JsonMapping),
    ),
    (
        "com.fasterxml.jackson.databind.exc.InvalidDefinitionException",
        FailureKind::Config(ConfigKind::InvalidDefinition),
    ),
    (
        "com.fasterxml.jackson.databind.exc.InvalidFormatException",
        FailureKind::Config(ConfigKind::InvalidFormat),
    ),
    (
        "com.fasterxml.jackson.databind.exc.InvalidTypeIdException",
        FailureKind::Config(ConfigKind::InvalidTypeId),
    ),
    (
        "com.fasterxml.jackson.databind.exc.MismatchedInputException",
        FailureKind::Config(ConfigKind::MismatchedInput),
    ),
    (
        "com.fasterxml.jackson.databind.exc.UnrecognizedPropertyException",
        FailureKind::Config(ConfigKind::UnrecognizedProperty),
    ),
    (
        "com.fasterxml.jackson.databind.exc.ValueInstantiationException",
        FailureKind::Config(ConfigKind::ValueInstantiation),
    ),
    (
        "com.fasterxml.jackson.dataformat.yaml.JacksonYAMLParseException",
        FailureKind::Config(ConfigKind::YamlParse),
    ),
    (
        "com.fasterxml.jackson.dataformat.yaml.snakeyaml.error.MarkedYAMLException",
        FailureKind::Config(ConfigKind::YamlMarked),
    ),
    (
        "com.fasterxml.jackson.module.kotlin.MissingKotlinParameterException",
        FailureKind::Config(ConfigKind::MissingParameter),
    ),
    (
        "org.yaml.snakeyaml.error.YAMLException",
        FailureKind::Config(ConfigKind::Yaml),
    ),
    (
        "org.yaml.snakeyaml.parser.ParserException",
        FailureKind::Config(ConfigKind::Parser),
    ),
    (
        "org.yaml.snakeyaml.reader.ReaderException",
        FailureKind::Config(ConfigKind::Reader),
    ),
    (
        "org.yaml.snakeyaml.scanner.ScannerException",
        FailureKind::Config(ConfigKind::Scanner),
    ),
    (
        "com.hierynomus.sshj.common.KeyDecryptionFailedException",
        FailureKind::Ssh(SshKind::KeyDecryptionFailed),
    ),
    (
        "net.schmizz.sshj.common.Base64DecodingException",
        FailureKind::Ssh(SshKind::Base64Decoding),
    ),
    (
        "net.schmizz.sshj.common.Buffer$BufferException",
        FailureKind::Ssh(SshKind::Buffer),
    ),
    (
        "net.schmizz.sshj.common.CircularBuffer$CircularBufferException",
        FailureKind::Ssh(SshKind::CircularBuffer),
    ),
    (
        "net.schmizz.sshj.common.SSHException",
        FailureKind::Ssh(SshKind::General),
    ),
    (
        "net.schmizz.sshj.connection.ConnectionException",
        FailureKind::Ssh(SshKind::Connection),
    ),
    (
        "net.schmizz.sshj.connection.channel.OpenFailException",
        FailureKind::Ssh(SshKind::OpenChannel),
    ),
    (
        "net.schmizz.sshj.transport.TransportException",
        FailureKind::Ssh(SshKind::Transport),
    ),
    (
        "net.schmizz.sshj.userauth.UserAuthException",
        FailureKind::Ssh(SshKind::Authentication),
    ),
    (
        "org.bouncycastle.openssl.EncryptionException",
        FailureKind::Ssh(SshKind::Encryption),
    ),
    ("org.bouncycastle.openssl.PEMException", FailureKind::Ssh(SshKind::Pem)),
    (
        "org.bouncycastle.openssl.PasswordException",
        FailureKind::Ssh(SshKind::Password),
    ),
    ("org.bouncycastle.pkcs.PKCSException", FailureKind::Ssh(SshKind::Pkcs)),
    (
        "com.influxdb.exceptions.BadRequestException",
        FailureKind::Analytics(AnalyticsKind::BadRequest),
    ),
    (
        "com.influxdb.exceptions.ForbiddenException",
        FailureKind::Analytics(AnalyticsKind::Forbidden),
    ),
    (
        "com.influxdb.exceptions.InfluxException",
        FailureKind::Analytics(AnalyticsKind::InfluxV2),
    ),
    (
        "com.influxdb.exceptions.InternalServerErrorException",
        FailureKind::Analytics(AnalyticsKind::InternalServerError),
    ),
    (
        "com.influxdb.exceptions.NotFoundException",
        FailureKind::Analytics(AnalyticsKind::NotFound),
    ),
    (
        "com.influxdb.exceptions.ServiceUnavailableException",
        FailureKind::Analytics(AnalyticsKind::ServiceUnavailable),
    ),
    (
        "com.influxdb.exceptions.UnauthorizedException",
        FailureKind::Analytics(AnalyticsKind::Unauthorized),
    ),
    (
        "com.influxdb.exceptions.UnprocessableEntityException",
        FailureKind::Analytics(AnalyticsKind::UnprocessableEntity),
    ),
    (
        "com.influxdb.query.exceptions.FluxCsvParserException",
        FailureKind::Analytics(AnalyticsKind::FluxCsvParser),
    ),
    (
        "com.influxdb.query.exceptions.FluxQueryException",
        FailureKind::Analytics(AnalyticsKind::FluxQuery),
    ),
    (
        "com.malinskiy.marathon.analytics.external.graphite.GraphiteException",
        FailureKind::Analytics(AnalyticsKind::Graphite),
    ),
    (
        "com.malinskiy.marathon.analytics.external.influx2.Influxdb2Exception",
        FailureKind::Analytics(AnalyticsKind::InfluxV2Provider),
    ),
    (
        "com.squareup.moshi.JsonEncodingException",
        FailureKind::Analytics(AnalyticsKind::JsonEncoding),
    ),
    (
        "org.influxdb.InfluxDBIOException",
        FailureKind::Analytics(AnalyticsKind::InfluxIo),
    ),
    (
        "org.influxdb.InfluxDBMapperException",
        FailureKind::Analytics(AnalyticsKind::InfluxMapping),
    ),
    (
        "org.influxdb.InfluxDBException",
        FailureKind::Analytics(AnalyticsKind::InfluxV1 {
            problem: InfluxV1Kind::General,
            retry: RetryDisposition::Retry,
        }),
    ),
    (
        "org.influxdb.InfluxDBException$AuthorizationFailedException",
        FailureKind::Analytics(AnalyticsKind::InfluxV1 {
            problem: InfluxV1Kind::AuthorizationFailed,
            retry: RetryDisposition::Retry,
        }),
    ),
    (
        "org.influxdb.InfluxDBException$CacheMaxMemorySizeExceededException",
        FailureKind::Analytics(AnalyticsKind::InfluxV1 {
            problem: InfluxV1Kind::CacheMaxMemorySizeExceeded,
            retry: RetryDisposition::Retry,
        }),
    ),
    (
        "org.influxdb.InfluxDBException$DatabaseNotFoundException",
        FailureKind::Analytics(AnalyticsKind::InfluxV1 {
            problem: InfluxV1Kind::DatabaseNotFound,
            retry: RetryDisposition::Retry,
        }),
    ),
    (
        "org.influxdb.InfluxDBException$FieldTypeConflictException",
        FailureKind::Analytics(AnalyticsKind::InfluxV1 {
            problem: InfluxV1Kind::FieldTypeConflict,
            retry: RetryDisposition::Retry,
        }),
    ),
    (
        "org.influxdb.InfluxDBException$HintedHandOffQueueNotEmptyException",
        FailureKind::Analytics(AnalyticsKind::InfluxV1 {
            problem: InfluxV1Kind::HintedHandOffQueueNotEmpty,
            retry: RetryDisposition::Retry,
        }),
    ),
    (
        "org.influxdb.InfluxDBException$PointsBeyondRetentionPolicyException",
        FailureKind::Analytics(AnalyticsKind::InfluxV1 {
            problem: InfluxV1Kind::PointsBeyondRetentionPolicy,
            retry: RetryDisposition::Retry,
        }),
    ),
    (
        "org.influxdb.InfluxDBException$UnableToParseException",
        FailureKind::Analytics(AnalyticsKind::InfluxV1 {
            problem: InfluxV1Kind::UnableToParse,
            retry: RetryDisposition::Retry,
        }),
    ),
    (
        "com.malinskiy.marathon.android.adam.TestAnnotationProducerNotFoundException",
        FailureKind::Android(AndroidKind::TestAnnotationProducerNotFound),
    ),
    (
        "com.malinskiy.marathon.android.exception.CommandRejectedException",
        FailureKind::Android(AndroidKind::CommandRejected),
    ),
    (
        "com.malinskiy.marathon.android.exception.DeviceNotSupportedException",
        FailureKind::Android(AndroidKind::DeviceNotSupported),
    ),
    (
        "com.malinskiy.marathon.android.exception.InstallException",
        FailureKind::Android(AndroidKind::Install),
    ),
    (
        "com.malinskiy.marathon.apple.ios.NotAdmitted",
        FailureKind::Apple(AppleKind::NotAdmitted),
    ),
    (
        "com.malinskiy.marathon.apple.ios.NotBooted",
        FailureKind::Apple(AppleKind::NotBooted),
    ),
    (
        "com.malinskiy.marathon.apple.cmd.remote.rsync.TransferStalled",
        FailureKind::Apple(AppleKind::TransferStalled),
    ),
    (
        "com.malinskiy.marathon.apple.ios.device.SetupDropped",
        FailureKind::Apple(AppleKind::SetupDropped),
    ),
    (
        "com.malinskiy.marathon.apple.ios.BootFailure",
        FailureKind::Apple(AppleKind::Boot),
    ),
    (
        "com.malinskiy.marathon.apple.cmd.CommandException",
        FailureKind::Apple(AppleKind::Command),
    ),
    (
        "com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException",
        FailureKind::Apple(AppleKind::DeviceFailure),
    ),
    (
        "io.qameta.allure.AllureResultsWriteException",
        FailureKind::Report(ReportKind::AllureResultsWrite),
    ),
    ("javax.imageio.IIOException", FailureKind::Report(ReportKind::ImageIo)),
    (
        "javax.imageio.metadata.IIOInvalidTreeException",
        FailureKind::Report(ReportKind::ImageMetadata),
    ),
    (
        "javax.xml.transform.TransformerException",
        FailureKind::Report(ReportKind::Transformer),
    ),
    ("org.xml.sax.SAXException", FailureKind::Report(ReportKind::Sax)),
    ("java.awt.AWTError", FailureKind::Fatal(FatalKind::WindowSystem)),
    ("java.io.IOError", FailureKind::Fatal(FatalKind::Io)),
    ("java.lang.AssertionError", FailureKind::Fatal(FatalKind::Assertion)),
    (
        "java.lang.BootstrapMethodError",
        FailureKind::Fatal(FatalKind::BootstrapMethod),
    ),
    (
        "java.lang.ClassCircularityError",
        FailureKind::Fatal(FatalKind::ClassCircularity),
    ),
    ("java.lang.ClassFormatError", FailureKind::Fatal(FatalKind::ClassFormat)),
    ("java.lang.Error", FailureKind::Fatal(FatalKind::General)),
    (
        "java.lang.ExceptionInInitializerError",
        FailureKind::Fatal(FatalKind::Initialization),
    ),
    (
        "java.lang.IllegalAccessError",
        FailureKind::Fatal(FatalKind::IllegalAccess),
    ),
    (
        "java.lang.IncompatibleClassChangeError",
        FailureKind::Fatal(FatalKind::IncompatibleClassChange),
    ),
    (
        "java.lang.InstantiationError",
        FailureKind::Fatal(FatalKind::Instantiation),
    ),
    ("java.lang.InternalError", FailureKind::Fatal(FatalKind::Internal)),
    ("java.lang.LinkageError", FailureKind::Fatal(FatalKind::Linkage)),
    (
        "java.lang.NoClassDefFoundError",
        FailureKind::Fatal(FatalKind::NoClassDefFound),
    ),
    ("java.lang.NoSuchFieldError", FailureKind::Fatal(FatalKind::NoSuchField)),
    (
        "java.lang.NoSuchMethodError",
        FailureKind::Fatal(FatalKind::NoSuchMethod),
    ),
    ("java.lang.OutOfMemoryError", FailureKind::Fatal(FatalKind::OutOfMemory)),
    (
        "java.lang.StackOverflowError",
        FailureKind::Fatal(FatalKind::StackOverflow),
    ),
    ("java.lang.ThreadDeath", FailureKind::Fatal(FatalKind::ThreadDeath)),
    ("java.lang.UnknownError", FailureKind::Fatal(FatalKind::Unknown)),
    (
        "java.lang.UnsatisfiedLinkError",
        FailureKind::Fatal(FatalKind::UnsatisfiedLink),
    ),
    (
        "java.lang.UnsupportedClassVersionError",
        FailureKind::Fatal(FatalKind::UnsupportedClassVersion),
    ),
    ("java.lang.VerifyError", FailureKind::Fatal(FatalKind::Verify)),
    (
        "java.lang.VirtualMachineError",
        FailureKind::Fatal(FatalKind::VirtualMachine),
    ),
    (
        "java.lang.annotation.AnnotationFormatError",
        FailureKind::Fatal(FatalKind::AnnotationFormat),
    ),
    (
        "java.nio.charset.CoderMalfunctionError",
        FailureKind::Fatal(FatalKind::CoderMalfunction),
    ),
    (
        "java.util.ServiceConfigurationError",
        FailureKind::Fatal(FatalKind::ServiceConfiguration),
    ),
    ("java.util.zip.ZipError", FailureKind::Fatal(FatalKind::Zip)),
    (
        "javax.xml.parsers.FactoryConfigurationError",
        FailureKind::Fatal(FatalKind::ParserFactory),
    ),
    (
        "javax.xml.stream.FactoryConfigurationError",
        FailureKind::Fatal(FatalKind::StreamFactory),
    ),
    (
        "javax.xml.transform.TransformerFactoryConfigurationError",
        FailureKind::Fatal(FatalKind::TransformerFactory),
    ),
    (
        "kotlin.NotImplementedError",
        FailureKind::Fatal(FatalKind::NotImplemented),
    ),
    (
        "kotlin.jvm.KotlinReflectionNotSupportedError",
        FailureKind::Fatal(FatalKind::ReflectionUnsupported),
    ),
    ("replay", FailureKind::Injected(InjectedKind::ReplayStep)),
    (
        "oracle.DeviceActorProbe$NotBootedProbeException",
        FailureKind::Injected(InjectedKind::NotBootedProbe),
    ),
    ("RunTimeout", FailureKind::Injected(InjectedKind::RunTimeout)),
    (
        "TransferException",
        FailureKind::Injected(InjectedKind::TransferException),
    ),
    ("CleanupError", FailureKind::Injected(InjectedKind::CleanupError)),
    ("ExecutionError", FailureKind::Injected(InjectedKind::ExecutionError)),
    ("After", FailureKind::Injected(InjectedKind::After)),
    ("Callback", FailureKind::Injected(InjectedKind::Callback)),
    ("test", FailureKind::Injected(InjectedKind::Test)),
    ("C", FailureKind::Injected(InjectedKind::ParserFixture)),
    ("NotAdmitted", FailureKind::Injected(InjectedKind::NotAdmitted)),
    ("SinkFailure", FailureKind::Injected(InjectedKind::SinkFailure)),
    ("TestFailure", FailureKind::Injected(InjectedKind::TestFailure)),
    ("TestTask", FailureKind::Injected(InjectedKind::TestTask)),
    (
        "TimeoutCancellationException",
        FailureKind::Injected(InjectedKind::TimeoutCancellation),
    ),
    ("TransferStalled", FailureKind::Injected(InjectedKind::TransferStalled)),
    ("cancelled", FailureKind::Injected(InjectedKind::Cancelled)),
    ("lost", FailureKind::Injected(InjectedKind::Lost)),
    ("probe", FailureKind::Injected(InjectedKind::Probe)),
    (
        "probe.AssertionError",
        FailureKind::Injected(InjectedKind::ProbeAssertionError),
    ),
    (
        "probe.ExceptionNamedError",
        FailureKind::Injected(InjectedKind::ProbeExceptionNamedError),
    ),
    (
        "qualification.Config",
        FailureKind::Injected(InjectedKind::QualificationConfig),
    ),
    ("test.Backend", FailureKind::Injected(InjectedKind::TestBackend)),
    ("test.Barrier", FailureKind::Injected(InjectedKind::TestBarrier)),
    ("test.Blocking", FailureKind::Injected(InjectedKind::TestBlocking)),
    ("test.Carbon", FailureKind::Injected(InjectedKind::TestCarbon)),
    ("test.Clock", FailureKind::Injected(InjectedKind::TestClock)),
    ("test.Close", FailureKind::Injected(InjectedKind::TestClose)),
    ("test.Error", FailureKind::Injected(InjectedKind::TestError)),
    ("test.Factory", FailureKind::Injected(InjectedKind::TestFactory)),
    ("test.Peer", FailureKind::Injected(InjectedKind::TestPeer)),
    ("test.Query", FailureKind::Injected(InjectedKind::TestQuery)),
    ("test.Watchdog", FailureKind::Injected(InjectedKind::TestWatchdog)),
    ("timeout", FailureKind::Injected(InjectedKind::Timeout)),
];

pub fn failure_kind(class: &str, message: Option<&str>) -> Result<FailureKind, String> {
    let kind = RECORDED_KINDS
        .iter()
        .find(|(name, _)| *name == class)
        .map(|(_, kind)| kind.clone())
        .ok_or_else(|| format!("unsupported recorded exception {class}"))?;
    Ok(match kind {
        FailureKind::Analytics(AnalyticsKind::InfluxV1 { problem, .. }) => {
            let (_, retry) = marathon_error::analytics::parse_v1_error_body(message.unwrap_or_default());
            FailureKind::Analytics(AnalyticsKind::InfluxV1 { problem, retry })
        }
        other => other,
    })
}

/// The scenario format uses short exception tokens, not arbitrary class suffixes.
pub fn batch_failure(class: &str, message: &str) -> marathon_core::error::VendorError {
    use marathon_core::error::{Failure, VendorError};
    let detail = |kind| Failure::from_kind(kind, Some(message.into()));
    match class {
        "TestBatchExecutionException" => VendorError::BatchExecution(detail(FailureKind::BatchExecution)),
        "DeviceLostException" => {
            let cause = detail(FailureKind::Runtime);
            VendorError::DeviceLost(Failure {
                kind: FailureKind::DeviceLost,
                message: Some(cause.to_string()),
                cause: Some(Box::new(cause)),
                stacktrace: None,
            })
        }
        "RuntimeException" => VendorError::Other(detail(FailureKind::Runtime)),
        other => VendorError::Other(Failure::from_kind(
            FailureKind::IllegalArgument,
            Some(format!("batch-failed: exception {other} is not supported")),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_security_failure_is_typed_and_unknown_classes_are_rejected() {
        assert_eq!(
            failure_kind("java.lang.SecurityException", None),
            Ok(FailureKind::Security)
        );
        assert_eq!(
            failure_kind("java.lang.FutureException", None),
            Err("unsupported recorded exception java.lang.FutureException".into())
        );
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use marathon_core::error::VendorError;
    #[test]
    fn replay_batch_tokens_keep_classification_causes_and_unknown_value_errors() {
        assert!(matches!(
            batch_failure("TestBatchExecutionException", "x"),
            VendorError::BatchExecution(_)
        ));
        let lost = batch_failure("DeviceLostException", "x");
        assert!(matches!(lost, VendorError::DeviceLost(_)));
        assert_eq!(lost.failure().message.as_deref(), Some("runtime failure: x"));
        assert_eq!(
            lost.failure().cause.as_deref().map(|c| &c.kind),
            Some(&FailureKind::Runtime)
        );
        assert!(matches!(batch_failure("RuntimeException", "x"), VendorError::Other(_)));
        for value in ["unknown", "java.lang.RuntimeException"] {
            let error = batch_failure(value, "x");
            assert_eq!(error.failure().kind, FailureKind::IllegalArgument);
            assert_eq!(
                error.failure().message,
                Some(format!("batch-failed: exception {value} is not supported"))
            );
        }
    }
    #[test]
    fn recorded_v1_kind_keeps_its_class_and_reconstructs_the_separate_disposition() {
        let class = "org.influxdb.InfluxDBException$CacheMaxMemorySizeExceededException";
        let kind = failure_kind(class, Some("cache-max-memory-size exceeded; username required")).unwrap();
        assert_eq!(
            kind,
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::CacheMaxMemorySizeExceeded,
                retry: RetryDisposition::Never,
            })
        );
        assert!(matches!(
            kind,
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                retry: RetryDisposition::Never,
                ..
            })
        ));
        let kind = failure_kind("org.influxdb.InfluxDBException$DatabaseNotFoundException", None).unwrap();
        assert!(matches!(
            kind,
            FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::DatabaseNotFound,
                retry: RetryDisposition::Retry
            })
        ));
    }
}

#[cfg(test)]
mod canonical_tests {
    use super::*;
    #[test]
    fn recorded_classes_round_trip_without_accepting_guessed_aliases() {
        let class = "com.malinskiy.marathon.apple.ios.device.SetupDropped";
        assert_eq!(
            failure_kind(class, None).unwrap(),
            FailureKind::Apple(AppleKind::SetupDropped)
        );
        assert!(failure_kind("com.malinskiy.marathon.apple.ios.SetupDropped", None).is_err());
    }
}

#[cfg(test)]
mod whitelist_tests {
    use super::*;
    use marathon_error::config::ValidationKind;

    fn covers_kind(kind: FailureKind) {
        assert!(
            RECORDED_KINDS
                .iter()
                .any(|(_, recorded)| recorded.dedup_key() == kind.dedup_key()),
            "{kind:?}"
        );
    }

    #[test]
    fn every_whitelisted_name_parses_and_identifiers_and_descriptions_are_unique() {
        let mut names = std::collections::HashSet::new();
        let mut descriptions = std::collections::HashSet::new();
        let mut identifiers = std::collections::HashSet::new();
        for (name, expected) in RECORDED_KINDS {
            let kind = failure_kind(name, None).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(kind.dedup_key(), expected.dedup_key());
            assert!(names.insert(name));
            assert!(descriptions.insert(expected.to_string()));
            assert!(identifiers.insert(expected.identifier()));
        }
        assert_eq!(names.len(), 208);
    }

    // Exhaustive matches require new enum variants to be covered, rather than silently
    // omitted from a hand-written list. Payload alternatives are covered separately.
    macro_rules! check_variants {
        ($ty:ident, $wrap:expr, [$($variant:ident),+ $(,)?] $(, $($payload:pat_param)|+)? ) => {{
            let check = |kind: $ty| match kind {
                $($ty::$variant => covers_kind(($wrap)($ty::$variant)),)+
                $($($payload)|+ => {},)?
            };
            $(check($ty::$variant);)+
        }};
    }

    #[test]
    fn every_recordable_kind_has_a_whitelisted_name() {
        check_variants!(
            FailureKind,
            |kind| kind,
            [
                Security,
                ReplayClock,
                DeviceLost,
                DeviceSetup,
                IncompatibleDevice,
                NoDevices,
                NoTests,
                BatchExecution,
                TestParsing,
                Transfer,
                Eof,
                FileNotFound,
                Io,
                InterruptedIo,
                UncheckedIo,
                Arithmetic,
                ArrayIndexOutOfBounds,
                ClassCast,
                Generic,
                IllegalArgument,
                IllegalState,
                IndexOutOfBounds,
                NegativeArraySize,
                NullReference,
                NumberFormat,
                Runtime,
                StringIndexOutOfBounds,
                UnsupportedOperation,
                Bind,
                Connect,
                NoRouteToHost,
                PortUnreachable,
                Protocol,
                Socket,
                SocketTimeout,
                UnknownHost,
                AccessDenied,
                FileAlreadyExists,
                FileSystem,
                InvalidPath,
                NoSuchFile,
                GeneralSecurity,
                DateTime,
                DateTimeParse,
                NoSuchElement,
                Cancelled,
                Timeout,
                PatternSyntax,
                Zip,
                UninitializedProperty,
                CopyDestinationExists,
                CopySourceMissing,
                JobCancelled,
                TimeoutCancelled,
                ChannelReceiveClosed,
                ChannelSendClosed,
                Replay,
                DependencyCreation,
                MissingDefinition,
            ],
            FailureKind::Config(_)
                | FailureKind::Ssh(_)
                | FailureKind::Analytics(_)
                | FailureKind::Dex(_)
                | FailureKind::Adb(_)
                | FailureKind::Android(_)
                | FailureKind::Apple(_)
                | FailureKind::Report(_)
                | FailureKind::Fatal(_)
                | FailureKind::Injected(_)
                | FailureKind::External(_)
        );
        check_variants!(
            AdbKind,
            FailureKind::Adb,
            [
                PullFailed,
                PushFailed,
                RequestRejected,
                RequestValidation,
                UnsupportedForwardingSpec,
                UnsupportedImageProtocol,
                UnsupportedSyncProtocol,
                ConnectRefused,
                ConnectTimeout,
                ClosedChannel,
                TransportClosed,
            ]
        );
        check_variants!(DexKind, FailureKind::Dex, [Decode,]);
        check_variants!(
            ConfigKind,
            FailureKind::Config,
            [
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
            ]
        );
        check_variants!(
            ValidationKind,
            ValidationKind::failure_kind,
            [Configuration, FileNotFound, Zip, Io, NoSuchFile,]
        );
        check_variants!(
            SshKind,
            FailureKind::Ssh,
            [
                KeyDecryptionFailed,
                Base64Decoding,
                Buffer,
                CircularBuffer,
                General,
                Connection,
                OpenChannel,
                Transport,
                Authentication,
                Encryption,
                Pem,
                Password,
                Pkcs,
            ]
        );
        check_variants!(
            AnalyticsKind,
            FailureKind::Analytics,
            [
                BadRequest,
                Forbidden,
                InfluxV2,
                InternalServerError,
                NotFound,
                ServiceUnavailable,
                Unauthorized,
                UnprocessableEntity,
                FluxCsvParser,
                FluxQuery,
                Graphite,
                InfluxV2Provider,
                JsonEncoding,
                InfluxIo,
                InfluxMapping,
            ],
            AnalyticsKind::InfluxV1 { .. }
        );
        check_variants!(
            InfluxV1Kind,
            |problem| FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem,
                retry: RetryDisposition::Retry
            }),
            [
                General,
                AuthorizationFailed,
                CacheMaxMemorySizeExceeded,
                DatabaseNotFound,
                FieldTypeConflict,
                HintedHandOffQueueNotEmpty,
                PointsBeyondRetentionPolicy,
                UnableToParse,
            ]
        );
        check_variants!(
            AndroidKind,
            FailureKind::Android,
            [
                TestAnnotationProducerNotFound,
                CommandRejected,
                DeviceNotSupported,
                Install,
            ]
        );
        check_variants!(
            AppleKind,
            FailureKind::Apple,
            [
                NotAdmitted,
                NotBooted,
                TransferStalled,
                SetupDropped,
                Boot,
                Command,
                DeviceFailure,
            ]
        );
        check_variants!(
            ReportKind,
            FailureKind::Report,
            [AllureResultsWrite, ImageIo, ImageMetadata, Transformer, Sax,]
        );
        check_variants!(
            FatalKind,
            FailureKind::Fatal,
            [
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
            ]
        );
        check_variants!(
            InjectedKind,
            FailureKind::Injected,
            [
                ReplayStep,
                NotBootedProbe,
                RunTimeout,
                TransferException,
                CleanupError,
                ExecutionError,
                After,
                Callback,
                Test,
                ParserFixture,
                NotAdmitted,
                SinkFailure,
                TestFailure,
                TestTask,
                TimeoutCancellation,
                TransferStalled,
                Cancelled,
                Lost,
                Probe,
                ProbeAssertionError,
                ProbeExceptionNamedError,
                QualificationConfig,
                TestBackend,
                TestBarrier,
                TestBlocking,
                TestCarbon,
                TestClock,
                TestClose,
                TestError,
                TestFactory,
                TestPeer,
                TestQuery,
                TestWatchdog,
                Timeout,
            ]
        );
    }
}

/// A recorded classification in a test expectation, checked at its input boundary.
pub fn kind(class: &str) -> FailureKind {
    failure_kind(class, None).unwrap_or_else(|error| panic!("{error}"))
}

/// Parse only a generated failure's leading class token; its message is opaque.
pub fn rendered(input: &str) -> Result<String, String> {
    for (name, _) in RECORDED_KINDS {
        if let Some(rest) = input.strip_prefix(name) {
            let message = if rest.is_empty() {
                None
            } else if let Some(message) = rest.strip_prefix(": ") {
                Some(message)
            } else {
                continue;
            };
            let kind = failure_kind(name, message)?;
            return Ok(match message {
                Some(message) => format!("{kind}: {message}"),
                None => kind.to_string(),
            });
        }
    }
    Err(format!("unsupported recorded failure {input}"))
}

#[cfg(test)]
mod wording_boundary_tests {
    use super::*;

    #[test]
    fn generated_class_is_mapped_but_external_class_inside_its_message_is_not() {
        assert_eq!(
            rendered("java.io.IOException: external java.lang.IllegalStateException: remote"),
            Ok("I/O error: external java.lang.IllegalStateException: remote".into())
        );
        assert_eq!(rendered("java.io.IOException"), Ok("I/O error".into()));
        assert_eq!(rendered("java.io.IOException: "), Ok("I/O error: ".into()));
        assert!(rendered("external java.io.IOException: remote").is_err());
        assert!(rendered("java.io.IOExceptionSuffix: remote").is_err());
    }
    #[test]
    fn generated_error_object_keeps_external_class_text_inside_the_message() {
        let message = "external java.lang.IllegalStateException: remote";
        let mut value = serde_json::json!({"class":"java.io.IOException", "message":message, "cause":null});
        error_object(&mut value);
        assert_eq!(
            value,
            serde_json::json!({"class":"io", "message":message, "cause":null})
        );
        let mut changed = value.clone();
        changed["message"] = "external invalid state: remote".into();
        assert_ne!(value, changed);
    }
    #[test]
    fn config_cause_without_a_message_uses_null_but_an_empty_message_stays_empty() {
        let mut error = serde_json::json!({
            "class":"java.io.IOException", "message":null,
            "causes":["java.io.IOException", "java.io.IOException: null", "java.io.IOException: ",
                "com.fasterxml.jackson.core.JsonParseException",
                "java.io.IOException: external java.lang.IllegalStateException: remote"],
        });
        config_error(&mut error);
        assert_eq!(
            error,
            serde_json::json!({
                "kind":"io", "message":null,
                "causes":["I/O error: null", "I/O error: null", "I/O error: ",
                    "JSON parsing failure: null", "I/O error: external java.lang.IllegalStateException: remote"],
            })
        );
        assert_eq!(rendered("java.io.IOException"), Ok("I/O error".into()));
    }

    #[test]
    fn class_looking_continuation_lines_in_messages_are_not_cause_headers() {
        let message = "external\nCaused by: java.lang.IllegalStateException: remote";
        let value = serde_json::json!({"class":"java.io.IOException", "message":message, "cause":null});
        let (recorded, plain) = throwable_strings(&value);
        assert_eq!(recorded, format!("java.io.IOException: {message}\n"));
        assert_eq!(plain, format!("I/O error: {message}\n"));
        assert_eq!(rendered(&recorded), Ok(plain));
    }
}

/// Project a known generated throwable object, never an arbitrary JSON object.
pub fn error_object(error: &mut serde_json::Value) {
    let kind = kind(
        error["class"]
            .as_str()
            .unwrap_or_else(|| panic!("missing recorded error class: {error}")),
    );
    if let Some(cause) = error.get("cause").filter(|c| c.is_object()) {
        // These producers use their cause's rendered text as the DeviceLost message.
        let class = cause["class"].as_str().unwrap();
        let old = match cause["message"].as_str() {
            Some(message) => format!("{class}: {message}"),
            None => class.to_owned(),
        };
        if kind == FailureKind::DeviceLost && error["message"].as_str() == Some(&old) {
            error["message"] = rendered(&old).unwrap_or_else(|e| panic!("{e}")).into();
        }
    }
    error["class"] = kind.identifier().into();
    if let Some(cause) = error.get_mut("cause").filter(|c| c.is_object()) {
        error_object(cause);
    }
}

/// The caller supplies only the logger's generated throwable-chain field.
pub fn error_chain(chain: &mut serde_json::Value) {
    let entries = chain.as_array_mut().expect("recorded throwable chain");
    for index in 0..entries.len() {
        let kind = kind(entries[index]["class"].as_str().expect("recorded class"));
        if kind == FailureKind::DeviceLost && index + 1 < entries.len() {
            let cause = &entries[index + 1];
            let class = cause["class"].as_str().expect("recorded cause class");
            let old = match cause["message"].as_str() {
                Some(message) => format!("{class}: {message}"),
                None => class.to_owned(),
            };
            if entries[index]["message"].as_str() == Some(&old) {
                entries[index]["message"] = rendered(&old).unwrap_or_else(|e| panic!("{e}")).into();
            }
        }
        entries[index]["class"] = kind.identifier().into();
    }
}

/// Rebuild generated headers from the oracle's structured throwable fields.
/// Messages are copied whole, including any class-looking continuation lines.
pub fn throwable_strings(error: &serde_json::Value) -> (String, String) {
    if error.is_null() {
        return (String::new(), String::new());
    }
    let class = error["class"].as_str().expect("recorded throwable class");
    let kind = kind(class);
    let suffix = error["message"]
        .as_str()
        .map(|message| format!(": {message}"))
        .unwrap_or_default();
    let mut recorded = format!("{class}{suffix}\n");
    let mut plain = format!("{kind}{suffix}\n");
    if let Some(cause) = error.get("cause").filter(|c| !c.is_null()) {
        let (old_cause, new_cause) = throwable_strings(cause);
        recorded.push_str("Caused by: ");
        recorded.push_str(&old_cause);
        plain.push_str("Caused by: ");
        plain.push_str(&new_cause);
    }
    (recorded, plain)
}

/// Config-dump's error field, with its separate nullable-cause message convention.
pub fn config_error(error: &mut serde_json::Value) {
    if error.is_null() {
        return;
    }
    let outer_kind = kind(error["class"].as_str().expect("recorded config error class"));
    let causes: Vec<_> = error["causes"]
        .as_array()
        .expect("recorded config causes")
        .iter()
        .map(|cause| {
            let cause = cause.as_str().expect("recorded config cause");
            if cause.contains(": ") {
                rendered(cause).unwrap_or_else(|e| panic!("{e}"))
            } else {
                // Treat a bare recorded cause as an absent message in this format only.
                format!("{}: null", kind(cause))
            }
        })
        .collect();
    *error = serde_json::json!({"kind":outer_kind.identifier(), "message":error["message"], "causes":causes});
}

/// Exact short tokens emitted by the SSH oracle's separate cause-chain format.
pub fn ssh_kind(class: &str) -> FailureKind {
    match class {
        "ConnectionException" => FailureKind::Ssh(SshKind::Connection),
        "TransportException" => FailureKind::Ssh(SshKind::Transport),
        "IllegalStateException" => FailureKind::IllegalState,
        other => panic!("unsupported recorded SSH exception {other}"),
    }
}

/// Exact exception tokens in the core scenario oracle's outcome/call fields.
pub fn scenario_kind(class: &str) -> FailureKind {
    match class {
        "DeviceLostException" => FailureKind::DeviceLost,
        "ConfigurationException" => FailureKind::Config(ConfigKind::Rejected),
        "TestBatchExecutionException" => FailureKind::BatchExecution,
        other => panic!("unsupported recorded scenario exception {other}"),
    }
}

/// The runtime oracle's outcome slot uses an unspaced colon and two short tokens.
pub fn runtime_outcome(value: &str) -> String {
    let Some((class, message)) = value.split_once(':') else {
        return value.into();
    };
    let kind = match class {
        "IllegalStateException" => FailureKind::IllegalState,
        "CancellationException" => FailureKind::Cancelled,
        "got" => return value.into(),
        other => panic!("unsupported recorded runtime outcome {other}"),
    };
    format!("{kind}:{message}")
}
