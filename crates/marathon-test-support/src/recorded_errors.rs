//! Port of core/src/test/kotlin/com/malinskiy/marathon/scenario/SuccessScenariosTest.kt
//! (recorded exception values at the replay input boundary).

use marathon_error::{
    AdbKind, AnalyticsKind, AndroidKind, AppleKind, ConfigKind, DexKind, FailureKind, FatalKind, InfluxV1Kind,
    InjectedKind, ReportKind, RetryDisposition, SshKind,
};

pub fn failure_kind(class: &str) -> Result<FailureKind, String> {
    match class {
        "java.lang.SecurityException" => Ok(FailureKind::Security),
        "After" => Ok(FailureKind::Injected(InjectedKind::After)),
        "C" => Ok(FailureKind::Injected(InjectedKind::ParserFixture)),
        "Callback" => Ok(FailureKind::Injected(InjectedKind::Callback)),
        "CleanupError" => Ok(FailureKind::Injected(InjectedKind::CleanupError)),
        "ExecutionError" => Ok(FailureKind::Injected(InjectedKind::ExecutionError)),
        "NotAdmitted" => Ok(FailureKind::Injected(InjectedKind::NotAdmitted)),
        "ReplayError" => Ok(FailureKind::ReplayClock),
        "RunTimeout" => Ok(FailureKind::Injected(InjectedKind::RunTimeout)),
        "SinkFailure" => Ok(FailureKind::Injected(InjectedKind::SinkFailure)),
        "TestFailure" => Ok(FailureKind::Injected(InjectedKind::TestFailure)),
        "TestTask" => Ok(FailureKind::Injected(InjectedKind::TestTask)),
        "TimeoutCancellationException" => Ok(FailureKind::Injected(InjectedKind::TimeoutCancellationException)),
        "TransferException" => Ok(FailureKind::Injected(InjectedKind::TransferException)),
        "TransferStalled" => Ok(FailureKind::Injected(InjectedKind::TransferStalled)),
        "cancelled" => Ok(FailureKind::Injected(InjectedKind::Cancelled)),
        "com.fasterxml.jackson.core.JsonParseException" => Ok(FailureKind::Config(ConfigKind::JsonParse)),
        "com.fasterxml.jackson.core.exc.StreamConstraintsException" => {
            Ok(FailureKind::Config(ConfigKind::StreamConstraints))
        }
        "com.fasterxml.jackson.core.io.JsonEOFException" => Ok(FailureKind::Config(ConfigKind::JsonEOF)),
        "com.fasterxml.jackson.databind.JsonMappingException" => Ok(FailureKind::Config(ConfigKind::JsonMapping)),
        "com.fasterxml.jackson.databind.exc.InvalidDefinitionException" => {
            Ok(FailureKind::Config(ConfigKind::InvalidDefinition))
        }
        "com.fasterxml.jackson.databind.exc.InvalidFormatException" => {
            Ok(FailureKind::Config(ConfigKind::InvalidFormat))
        }
        "com.fasterxml.jackson.databind.exc.InvalidTypeIdException" => {
            Ok(FailureKind::Config(ConfigKind::InvalidTypeId))
        }
        "com.fasterxml.jackson.databind.exc.MismatchedInputException" => {
            Ok(FailureKind::Config(ConfigKind::MismatchedInput))
        }
        "com.fasterxml.jackson.databind.exc.UnrecognizedPropertyException" => {
            Ok(FailureKind::Config(ConfigKind::UnrecognizedProperty))
        }
        "com.fasterxml.jackson.databind.exc.ValueInstantiationException" => {
            Ok(FailureKind::Config(ConfigKind::ValueInstantiation))
        }
        "com.fasterxml.jackson.dataformat.yaml.JacksonYAMLParseException" => {
            Ok(FailureKind::Config(ConfigKind::JacksonYAMLParse))
        }
        "com.fasterxml.jackson.dataformat.yaml.snakeyaml.error.MarkedYAMLException" => {
            Ok(FailureKind::Config(ConfigKind::MarkedYAML))
        }
        "com.fasterxml.jackson.module.kotlin.MissingKotlinParameterException" => {
            Ok(FailureKind::Config(ConfigKind::MissingKotlinParameter))
        }
        "com.hierynomus.sshj.common.KeyDecryptionFailedException" => Ok(FailureKind::Ssh(SshKind::KeyDecryptionFailed)),
        "com.influxdb.exceptions.BadRequestException" => Ok(FailureKind::Analytics(AnalyticsKind::BadRequest)),
        "com.influxdb.exceptions.ForbiddenException" => Ok(FailureKind::Analytics(AnalyticsKind::Forbidden)),
        "com.influxdb.exceptions.InfluxException" => Ok(FailureKind::Analytics(AnalyticsKind::InfluxV2)),
        "com.influxdb.exceptions.InternalServerErrorException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::InternalServerError))
        }
        "com.influxdb.exceptions.NotFoundException" => Ok(FailureKind::Analytics(AnalyticsKind::NotFound)),
        "com.influxdb.exceptions.ServiceUnavailableException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::ServiceUnavailable))
        }
        "com.influxdb.exceptions.UnauthorizedException" => Ok(FailureKind::Analytics(AnalyticsKind::Unauthorized)),
        "com.influxdb.exceptions.UnprocessableEntityException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::UnprocessableEntity))
        }
        "com.influxdb.query.exceptions.FluxCsvParserException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::FluxCsvParser))
        }
        "com.influxdb.query.exceptions.FluxQueryException" => Ok(FailureKind::Analytics(AnalyticsKind::FluxQuery)),
        "com.linkedin.dex.spec.DexException" => Ok(FailureKind::Dex(DexKind::Decode)),
        "com.malinskiy.adam.exception.PullFailedException" => Ok(FailureKind::Adb(AdbKind::PullFailed)),
        "com.malinskiy.adam.exception.PushFailedException" => Ok(FailureKind::Adb(AdbKind::PushFailed)),
        "com.malinskiy.adam.exception.RequestRejectedException" => Ok(FailureKind::Adb(AdbKind::RequestRejected)),
        "com.malinskiy.adam.exception.RequestValidationException" => Ok(FailureKind::Adb(AdbKind::RequestValidation)),
        "com.malinskiy.adam.exception.UnsupportedForwardingSpecException" => {
            Ok(FailureKind::Adb(AdbKind::UnsupportedForwardingSpec))
        }
        "com.malinskiy.adam.exception.UnsupportedImageProtocolException" => {
            Ok(FailureKind::Adb(AdbKind::UnsupportedImageProtocol))
        }
        "com.malinskiy.adam.exception.UnsupportedSyncProtocolException" => {
            Ok(FailureKind::Adb(AdbKind::UnsupportedSyncProtocol))
        }
        "com.malinskiy.marathon.analytics.external.graphite.GraphiteException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::Graphite))
        }
        "com.malinskiy.marathon.analytics.external.influx2.Influxdb2Exception" => {
            Ok(FailureKind::Analytics(AnalyticsKind::Influxdb2))
        }
        "com.malinskiy.marathon.android.adam.TestAnnotationProducerNotFoundException" => {
            Ok(FailureKind::Android(AndroidKind::TestAnnotationProducerNotFound))
        }
        "com.malinskiy.marathon.android.exception.CommandRejectedException" => {
            Ok(FailureKind::Android(AndroidKind::CommandRejected))
        }
        "com.malinskiy.marathon.android.exception.DeviceNotSupportedException" => {
            Ok(FailureKind::Android(AndroidKind::DeviceNotSupported))
        }
        "com.malinskiy.marathon.android.exception.InstallException" => Ok(FailureKind::Android(AndroidKind::Install)),
        "com.malinskiy.marathon.apple.cmd.CommandException" => Ok(FailureKind::Apple(AppleKind::Command)),
        "com.malinskiy.marathon.apple.cmd.remote.rsync.TransferStalled" => {
            Ok(FailureKind::Apple(AppleKind::TransferStalled))
        }
        "com.malinskiy.marathon.apple.ios.BootFailure" => Ok(FailureKind::Apple(AppleKind::Boot)),
        "com.malinskiy.marathon.apple.ios.NotAdmitted" => Ok(FailureKind::Apple(AppleKind::NotAdmitted)),
        "com.malinskiy.marathon.apple.ios.NotBooted" => Ok(FailureKind::Apple(AppleKind::NotBooted)),
        "com.malinskiy.marathon.apple.ios.SetupDropped" => Ok(FailureKind::Apple(AppleKind::SetupDropped)),
        "com.malinskiy.marathon.apple.ios.device.SetupDropped" => Ok(FailureKind::Apple(AppleKind::SetupDropped)),
        "com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException" => {
            Ok(FailureKind::Apple(AppleKind::DeviceFailure))
        }
        "com.malinskiy.marathon.config.exceptions.ConfigurationException" => {
            Ok(FailureKind::Config(ConfigKind::Rejected))
        }
        "com.malinskiy.marathon.exceptions.DeviceLostException" => Ok(FailureKind::DeviceLost),
        "com.malinskiy.marathon.exceptions.DeviceSetupException" => Ok(FailureKind::DeviceSetup),
        "com.malinskiy.marathon.exceptions.IncompatibleDeviceException" => Ok(FailureKind::IncompatibleDevice),
        "com.malinskiy.marathon.exceptions.NoDevicesException" => Ok(FailureKind::NoDevices),
        "com.malinskiy.marathon.exceptions.NoTestCasesFoundException" => Ok(FailureKind::NoTests),
        "com.malinskiy.marathon.exceptions.TestBatchExecutionException" => Ok(FailureKind::BatchExecution),
        "com.malinskiy.marathon.exceptions.TestParsingException" => Ok(FailureKind::TestParsing),
        "com.malinskiy.marathon.exceptions.TransferException" => Ok(FailureKind::Transfer),
        "com.squareup.moshi.JsonEncodingException" => Ok(FailureKind::Analytics(AnalyticsKind::JsonEncoding)),
        "io.netty.channel.AbstractChannel$AnnotatedConnectException" => Ok(FailureKind::Adb(AdbKind::ConnectRefused)),
        "io.netty.channel.ConnectTimeoutException" => Ok(FailureKind::Adb(AdbKind::ConnectTimeout)),
        "io.netty.channel.StacklessClosedChannelException" => Ok(FailureKind::Adb(AdbKind::ClosedChannel)),
        "io.qameta.allure.AllureResultsWriteException" => Ok(FailureKind::Report(ReportKind::AllureResultsWrite)),
        "io.vertx.core.impl.NoStackTraceThrowable" => Ok(FailureKind::Adb(AdbKind::TransportClosed)),
        "java.awt.AWTError" => Ok(FailureKind::Fatal(FatalKind::WindowSystem)),
        "java.io.EOFException" => Ok(FailureKind::Eof),
        "java.io.FileNotFoundException" => Ok(FailureKind::FileNotFound),
        "java.io.IOError" => Ok(FailureKind::Fatal(FatalKind::IO)),
        "java.io.IOException" => Ok(FailureKind::Io),
        "java.io.InterruptedIOException" => Ok(FailureKind::InterruptedIO),
        "java.io.UncheckedIOException" => Ok(FailureKind::UncheckedIO),
        "java.lang.ArithmeticException" => Ok(FailureKind::Arithmetic),
        "java.lang.ArrayIndexOutOfBoundsException" => Ok(FailureKind::ArrayIndexOutOfBounds),
        "java.lang.AssertionError" => Ok(FailureKind::Fatal(FatalKind::Assertion)),
        "java.lang.BootstrapMethodError" => Ok(FailureKind::Fatal(FatalKind::BootstrapMethod)),
        "java.lang.ClassCastException" => Ok(FailureKind::ClassCast),
        "java.lang.ClassCircularityError" => Ok(FailureKind::Fatal(FatalKind::ClassCircularity)),
        "java.lang.ClassFormatError" => Ok(FailureKind::Fatal(FatalKind::ClassFormat)),
        "java.lang.Error" => Ok(FailureKind::Fatal(FatalKind::General)),
        "java.lang.Exception" => Ok(FailureKind::Generic),
        "java.lang.ExceptionInInitializerError" => Ok(FailureKind::Fatal(FatalKind::ExceptionInInitializer)),
        "java.lang.IllegalAccessError" => Ok(FailureKind::Fatal(FatalKind::IllegalAccess)),
        "java.lang.IllegalArgumentException" => Ok(FailureKind::IllegalArgument),
        "java.lang.IllegalStateException" => Ok(FailureKind::IllegalState),
        "java.lang.IncompatibleClassChangeError" => Ok(FailureKind::Fatal(FatalKind::IncompatibleClassChange)),
        "java.lang.IndexOutOfBoundsException" => Ok(FailureKind::IndexOutOfBounds),
        "java.lang.InstantiationError" => Ok(FailureKind::Fatal(FatalKind::Instantiation)),
        "java.lang.InternalError" => Ok(FailureKind::Fatal(FatalKind::Internal)),
        "java.lang.LinkageError" => Ok(FailureKind::Fatal(FatalKind::Linkage)),
        "java.lang.NegativeArraySizeException" => Ok(FailureKind::NegativeArraySize),
        "java.lang.NoClassDefFoundError" => Ok(FailureKind::Fatal(FatalKind::NoClassDefFound)),
        "java.lang.NoSuchFieldError" => Ok(FailureKind::Fatal(FatalKind::NoSuchField)),
        "java.lang.NoSuchMethodError" => Ok(FailureKind::Fatal(FatalKind::NoSuchMethod)),
        "java.lang.NullPointerException" => Ok(FailureKind::NullReference),
        "java.lang.NumberFormatException" => Ok(FailureKind::NumberFormat),
        "java.lang.OutOfMemoryError" => Ok(FailureKind::Fatal(FatalKind::OutOfMemory)),
        "java.lang.RuntimeException" => Ok(FailureKind::Runtime),
        "java.lang.StackOverflowError" => Ok(FailureKind::Fatal(FatalKind::StackOverflow)),
        "java.lang.StringIndexOutOfBoundsException" => Ok(FailureKind::StringIndexOutOfBounds),
        "java.lang.ThreadDeath" => Ok(FailureKind::Fatal(FatalKind::ThreadDeath)),
        "java.lang.UnknownError" => Ok(FailureKind::Fatal(FatalKind::Unknown)),
        "java.lang.UnsatisfiedLinkError" => Ok(FailureKind::Fatal(FatalKind::UnsatisfiedLink)),
        "java.lang.UnsupportedClassVersionError" => Ok(FailureKind::Fatal(FatalKind::UnsupportedClassVersion)),
        "java.lang.UnsupportedOperationException" => Ok(FailureKind::UnsupportedOperation),
        "java.lang.VerifyError" => Ok(FailureKind::Fatal(FatalKind::Verify)),
        "java.lang.VirtualMachineError" => Ok(FailureKind::Fatal(FatalKind::VirtualMachine)),
        "java.lang.annotation.AnnotationFormatError" => Ok(FailureKind::Fatal(FatalKind::AnnotationFormat)),
        "java.net.BindException" => Ok(FailureKind::Bind),
        "java.net.ConnectException" => Ok(FailureKind::Connect),
        "java.net.NoRouteToHostException" => Ok(FailureKind::NoRouteToHost),
        "java.net.PortUnreachableException" => Ok(FailureKind::PortUnreachable),
        "java.net.ProtocolException" => Ok(FailureKind::Protocol),
        "java.net.SocketException" => Ok(FailureKind::Socket),
        "java.net.SocketTimeoutException" => Ok(FailureKind::SocketTimeout),
        "java.net.UnknownHostException" => Ok(FailureKind::UnknownHost),
        "java.nio.charset.CoderMalfunctionError" => Ok(FailureKind::Fatal(FatalKind::CoderMalfunction)),
        "java.nio.file.AccessDeniedException" => Ok(FailureKind::AccessDenied),
        "java.nio.file.FileAlreadyExistsException" => Ok(FailureKind::FileAlreadyExists),
        "java.nio.file.FileSystemException" => Ok(FailureKind::FileSystem),
        "java.nio.file.InvalidPathException" => Ok(FailureKind::InvalidPath),
        "java.nio.file.NoSuchFileException" => Ok(FailureKind::NoSuchFile),
        "java.security.GeneralSecurityException" => Ok(FailureKind::GeneralSecurity),
        "java.time.DateTimeException" => Ok(FailureKind::DateTime),
        "java.time.format.DateTimeParseException" => Ok(FailureKind::DateTimeParse),
        "java.util.NoSuchElementException" => Ok(FailureKind::NoSuchElement),
        "java.util.ServiceConfigurationError" => Ok(FailureKind::Fatal(FatalKind::ServiceConfiguration)),
        "java.util.concurrent.CancellationException" => Ok(FailureKind::Cancelled),
        "java.util.concurrent.TimeoutException" => Ok(FailureKind::Timeout),
        "java.util.regex.PatternSyntaxException" => Ok(FailureKind::PatternSyntax),
        "java.util.zip.ZipError" => Ok(FailureKind::Fatal(FatalKind::Zip)),
        "java.util.zip.ZipException" => Ok(FailureKind::Zip),
        "javax.imageio.IIOException" => Ok(FailureKind::Report(ReportKind::ImageIo)),
        "javax.imageio.metadata.IIOInvalidTreeException" => Ok(FailureKind::Report(ReportKind::ImageMetadata)),
        "javax.xml.parsers.FactoryConfigurationError" => Ok(FailureKind::Fatal(FatalKind::ParserFactory)),
        "javax.xml.stream.FactoryConfigurationError" => Ok(FailureKind::Fatal(FatalKind::StreamFactory)),
        "javax.xml.transform.TransformerException" => Ok(FailureKind::Report(ReportKind::Transformer)),
        "javax.xml.transform.TransformerFactoryConfigurationError" => {
            Ok(FailureKind::Fatal(FatalKind::TransformerFactory))
        }
        "kotlin.NotImplementedError" => Ok(FailureKind::Fatal(FatalKind::NotImplemented)),
        "kotlin.UninitializedPropertyAccessException" => Ok(FailureKind::UninitializedProperty),
        "kotlin.io.FileAlreadyExistsException" => Ok(FailureKind::CopyDestinationExists),
        "kotlin.io.NoSuchFileException" => Ok(FailureKind::CopySourceMissing),
        "kotlin.jvm.KotlinReflectionNotSupportedError" => {
            Ok(FailureKind::Fatal(FatalKind::KotlinReflectionNotSupported))
        }
        "kotlinx.coroutines.JobCancellationException" => Ok(FailureKind::JobCancelled),
        "kotlinx.coroutines.TimeoutCancellationException" => Ok(FailureKind::TimeoutCancelled),
        "kotlinx.coroutines.channels.ClosedReceiveChannelException" => Ok(FailureKind::ChannelReceiveClosed),
        "kotlinx.coroutines.channels.ClosedSendChannelException" => Ok(FailureKind::ChannelSendClosed),
        "lost" => Ok(FailureKind::Injected(InjectedKind::Lost)),
        "marathon.ReplayError" => Ok(FailureKind::Replay),
        "net.schmizz.sshj.common.Base64DecodingException" => Ok(FailureKind::Ssh(SshKind::Base64Decoding)),
        "net.schmizz.sshj.common.Buffer$BufferException" => Ok(FailureKind::Ssh(SshKind::Buffer)),
        "net.schmizz.sshj.common.CircularBuffer$CircularBufferException" => {
            Ok(FailureKind::Ssh(SshKind::CircularBuffer))
        }
        "net.schmizz.sshj.common.SSHException" => Ok(FailureKind::Ssh(SshKind::General)),
        "net.schmizz.sshj.connection.ConnectionException" => Ok(FailureKind::Ssh(SshKind::Connection)),
        "net.schmizz.sshj.connection.channel.OpenFailException" => Ok(FailureKind::Ssh(SshKind::OpenChannel)),
        "net.schmizz.sshj.transport.TransportException" => Ok(FailureKind::Ssh(SshKind::Transport)),
        "net.schmizz.sshj.userauth.UserAuthException" => Ok(FailureKind::Ssh(SshKind::Authentication)),
        "oracle.DeviceActorProbe$NotBootedProbeException" => Ok(FailureKind::Injected(
            InjectedKind::OracleDeviceActorProbeNotBootedProbeException,
        )),
        "org.bouncycastle.openssl.EncryptionException" => Ok(FailureKind::Ssh(SshKind::Encryption)),
        "org.bouncycastle.openssl.PEMException" => Ok(FailureKind::Ssh(SshKind::PEM)),
        "org.bouncycastle.openssl.PasswordException" => Ok(FailureKind::Ssh(SshKind::Password)),
        "org.bouncycastle.pkcs.PKCSException" => Ok(FailureKind::Ssh(SshKind::PKCS)),
        "org.influxdb.InfluxDBException" => Ok(FailureKind::Analytics(AnalyticsKind::InfluxV1 {
            problem: InfluxV1Kind::General,
            retry: RetryDisposition::Retry,
        })),
        "org.influxdb.InfluxDBException$AuthorizationFailedException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::AuthorizationFailed,
                retry: RetryDisposition::Never,
            }))
        }
        "org.influxdb.InfluxDBException$CacheMaxMemorySizeExceededException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::CacheMaxMemorySizeExceeded,
                retry: RetryDisposition::Retry,
            }))
        }
        "org.influxdb.InfluxDBException$DatabaseNotFoundException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::DatabaseNotFound,
                retry: RetryDisposition::Never,
            }))
        }
        "org.influxdb.InfluxDBException$FieldTypeConflictException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::FieldTypeConflict,
                retry: RetryDisposition::Never,
            }))
        }
        "org.influxdb.InfluxDBException$HintedHandOffQueueNotEmptyException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::HintedHandOffQueueNotEmpty,
                retry: RetryDisposition::Never,
            }))
        }
        "org.influxdb.InfluxDBException$PointsBeyondRetentionPolicyException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::PointsBeyondRetentionPolicy,
                retry: RetryDisposition::Never,
            }))
        }
        "org.influxdb.InfluxDBException$UnableToParseException" => {
            Ok(FailureKind::Analytics(AnalyticsKind::InfluxV1 {
                problem: InfluxV1Kind::UnableToParse,
                retry: RetryDisposition::Never,
            }))
        }
        "org.influxdb.InfluxDBIOException" => Ok(FailureKind::Analytics(AnalyticsKind::InfluxIo)),
        "org.influxdb.InfluxDBMapperException" => Ok(FailureKind::Analytics(AnalyticsKind::InfluxMapping)),
        "org.koin.core.error.InstanceCreationException" => Ok(FailureKind::DependencyCreation),
        "org.koin.core.error.NoBeanDefFoundException" => Ok(FailureKind::MissingDefinition),
        "org.xml.sax.SAXException" => Ok(FailureKind::Report(ReportKind::SAX)),
        "org.yaml.snakeyaml.error.YAMLException" => Ok(FailureKind::Config(ConfigKind::YAML)),
        "org.yaml.snakeyaml.parser.ParserException" => Ok(FailureKind::Config(ConfigKind::Parser)),
        "org.yaml.snakeyaml.reader.ReaderException" => Ok(FailureKind::Config(ConfigKind::Reader)),
        "org.yaml.snakeyaml.scanner.ScannerException" => Ok(FailureKind::Config(ConfigKind::Scanner)),
        "probe" => Ok(FailureKind::Injected(InjectedKind::Probe)),
        "probe.AssertionError" => Ok(FailureKind::Injected(InjectedKind::ProbeAssertionError)),
        "probe.ExceptionNamedError" => Ok(FailureKind::Injected(InjectedKind::ProbeExceptionNamedError)),
        "qualification.Config" => Ok(FailureKind::Injected(InjectedKind::QualificationConfig)),
        "test" => Ok(FailureKind::Injected(InjectedKind::Test)),
        "test.Backend" => Ok(FailureKind::Injected(InjectedKind::TestBackend)),
        "test.Barrier" => Ok(FailureKind::Injected(InjectedKind::TestBarrier)),
        "test.Blocking" => Ok(FailureKind::Injected(InjectedKind::TestBlocking)),
        "test.Carbon" => Ok(FailureKind::Injected(InjectedKind::TestCarbon)),
        "test.Clock" => Ok(FailureKind::Injected(InjectedKind::TestClock)),
        "test.Close" => Ok(FailureKind::Injected(InjectedKind::TestClose)),
        "test.Error" => Ok(FailureKind::Injected(InjectedKind::TestError)),
        "test.Factory" => Ok(FailureKind::Injected(InjectedKind::TestFactory)),
        "test.Peer" => Ok(FailureKind::Injected(InjectedKind::TestPeer)),
        "test.Query" => Ok(FailureKind::Injected(InjectedKind::TestQuery)),
        "test.Watchdog" => Ok(FailureKind::Injected(InjectedKind::TestWatchdog)),
        "timeout" => Ok(FailureKind::Injected(InjectedKind::Timeout)),
        other => Err(format!("unsupported recorded exception {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_security_failure_is_typed_and_unknown_classes_are_rejected() {
        assert_eq!(failure_kind("java.lang.SecurityException"), Ok(FailureKind::Security));
        assert_eq!(
            failure_kind("java.lang.FutureException"),
            Err("unsupported recorded exception java.lang.FutureException".into())
        );
    }
}
