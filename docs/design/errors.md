# Typed failures (WP 8.6a)

Milestones 1–5 replace string classifications throughout the workspace. Rendered bytes remain unchanged;
the narrower 8.6a brief applies even where PLAN §3.12 permits wording changes. No D-row or
fixture update is needed. The approved milestone-1 design is implemented.

## Types and crate graph

`marathon-error` is a dependency-free leaf crate. It owns `FailureKind`, re-exported from
`marathon_core::error`. The vocabulary includes common alternatives, domain subkinds and `External(String)`.
`Failure`, its cloneable cause chain, `OperationError`, `Cancellation` (including timeout
scope identity), and `VendorError` remain in core. No trait objects or downcasts enter them.

ADB, DEX and config cannot depend on core. They depend directly on the leaf crate,
as core does now; vendor integrations already depending on core may use its re-export.
All shared kind definitions and their mappings have one owner. `FailureKind` wraps domain enums also defined in `marathon-error`, each in its own module:

| Type | Meaning / consumers |
| --- | --- |
| `AdbKind` | Request rejection, validation, sync/image protocols, transport; ADB and Android |
| `DexKind` | DEX decoding; DEX and Android (common I/O kinds are reused) |
| `ConfigKind`, `ValidationKind` | Binding, coercion, YAML/JSON decoding, validation and configuration rejection; config and core |
| `SshKind` | Connection, authentication, channel, key decoding and transport; Apple, iOS, macOS |
| `AnalyticsKind` | Influx v1/v2, Flux, Graphite and response decoding; analytics and core |
| `AndroidKind`, `AppleKind` | Vendor-specific failures outside ADB/SSH |
| `ReportKind` | Reporter/encoding failures, without moving reporter implementations |
| `FatalKind` | Existing JVM Error catch distinctions, including ThreadDeath |
| `InjectedKind` | Closed replay/test failure markers; no arbitrary string construction |

Producer error structs keep their domain data (for example SSH disconnect reasons,
suppressed errors and the no-common-algorithm marker); projection into `Failure` moves
the shared kind directly. No conversion renders a class and parses it back. Common I/O,
argument, state, cancellation and runner classifications stay top-level alternatives.
`FatalKind` and SSH/DEX membership methods replace the old string hierarchy helpers.

## Typed construction and rendering

`Failure` owns `pub kind: FailureKind`, its nullable message, cloneable cause chain and
opaque frame text. `Failure::from_kind` stores the kind directly. `to_java_string` obtains
the rendered name from `kind.legacy_class()`; `stack_trace` delegates to `to_java_string`.
There is no string constructor, string equality implementation on kinds, or independently
stored class that can disagree with the kind. `Failure` has a custom `Debug` view using the
same mapping: an SSH clock-error fallback embeds it in a message, so its old field labels
and recursive representation must remain byte-identical until the wording WP.

The field swap includes required consumer edits across the workspace in one buildable
change. Domain producers project their shared kind directly; they never render and parse
it back. `artifact_io.rs` was the first converted funnel; its I/O classification, NIO reason
selection, file-open/clock failures and worker-completion failures all use kinds.

## Legacy rendering and external text

Each kind type has one `legacy_class` mapping. Wrapper variants delegate to their domain
mapping; unrelated distinctions never share a rendering accidentally. In particular,
`Replay` renders `marathon.ReplayError`, while `ReplayClock` renders `ReplayError`.
Nullable/empty messages, cause order, frame text and newline handling stay unchanged.

`External(String)` makes `legacy_class` return `&str` rather than `&'static str`. Use it
only if an adapter receives a separate class name from a runner and projects that name
verbatim into a `Failure`; annotate every such construction with its external source.
Its text deliberately participates in no selector: no equality-based selection,
`wraps_as_analytics_io`, `is_fatal` or deduplication class. It carries verbatim external
text only; never classify or normalise that text into an internal catch or key.
No production construction site currently needs it. Instrumentation stack/shortMsg, logcat,
XCTest/xcresult and command output already held in messages or traces remain opaque there;
do not split those strings to manufacture an `External` kind. The new external-text test
models a runner-provided class and proves embedded class names remain text.

## Recorded-input boundary

Convert recorded class names once in the replay/scenario input adapter, before a typed
failure enters execution. Use a fallible, explicit whitelist for that recording format;
scenario short names such as `DeviceLostException` are format tokens, not class suffixes
to guess. Unknown names take the existing invalid-input path (for example the unsupported
`batch-failed` exception error), never `External`, a default kind, or an internal panic.
`marathon-test-support/src/recorded_errors.rs` owns the whitelist and scenario-token parser;
its exhaustive enum round-trip test covers every recordable legacy kind, including the
`InjectedKind::ReplayStep` string `replay`, while excluding opaque `External` text;
`StubDevice::deliver` parses batch failures before queuing typed execution events. Recorded
Influx errors retain their recorded category while the body parser supplies their independent
retry disposition; a missing message remains retryable. Keep recorded expectation strings as
expected rendered bytes. Copied tapes and fixtures
remain untouched. The existing clock tape reader need not gain an exception parser when
its format contains no exception values.

## Two converted selectors

Both examples are implemented in `crates/marathon-core/src/artifact_io.rs`:

```rust
// io_error, before: the selected string flowed into the string-based constructor.
ErrorKind::NotFound => "java.nio.file.NoSuchFileException",
// after: the selected kind flows into Failure::from_kind.
ErrorKind::NotFound => FailureKind::NoSuchFile,

// nio_error, before:
if class == "java.nio.file.FileSystemException" { /* append native reason */ }
// after:
if kind == FailureKind::FileSystem { /* append native reason */ }
```

The first retains every I/O category and its fallback; the second retains exactly which
errors append the native reason. Invalid input still delegates to `io_error`, and the
ENOTDIR parent-path handling is unchanged. Tests pin class/message bytes for both.

## Inventory and expected size

The brief's original survey counted 123 distinct class strings in 462 literals across
116 files. A broader lexical inventory of starting commit `7bb782d9` finds **167 distinct
qualified error-class strings, 613 literals, 134 files** under `crates/*/src/**/*.rs`.
This counts ordinary complete string literals (comments excluded), including in-source
tests and hierarchy tables, with Java/Kotlin/vendor prefixes and an Exception, Error,
Throwable or ThreadDeath ending. It excludes arbitrary package/logger/type names and
unqualified names such as `ReplayError`; it is not the same scope as the original survey.

The following per-crate reference counts are initial upper estimates of required kind
alternatives, shared through the leaf crate, not counts of separate new enums to duplicate:

| Consumer | Distinct class references / expected alternatives |
| --- | ---: |
| core | 30, plus the unqualified clock replay kind |
| config | 24 |
| ADB | 19 |
| DEX | 12 |
| analytics | 33 |
| Android | 39 |
| Apple (including SSH) | 63 |
| iOS | 16 |
| macOS | 18 |
| CLI | 10 |
| test support | 15 |
| jcompat | 37 references to migrate/remove, chiefly fatal catch membership |
| snakeyaml | 4 parser kinds to project at the config boundary |

These reference counts overlap. The final definitions in the leaf crate are:

| Module | Enum | Alternatives |
| --- | --- | ---: |
| `lib.rs` | `FailureKind` | 70 (59 direct, 10 domain wrappers, 1 external) |
| `adb.rs` | `AdbKind` | 11 |
| `dex.rs` | `DexKind` | 1 |
| `config.rs` | `ConfigKind` / `ValidationKind` | 18 / 5 |
| `ssh.rs` | `SshKind` | 13 |
| `analytics.rs` | `AnalyticsKind` / `InfluxV1Kind` / `RetryDisposition` | 16 / 8 / 2 |
| `android.rs` | `AndroidKind` | 4 |
| `apple.rs` | `AppleKind` | 7 |
| `report.rs` | `ReportKind` | 5 |
| `fatal.rs` | `FatalKind` | 33 |
| `injected.rs` | `InjectedKind` | 34 |

Independent SnakeYAML owns seven `TokenErrorKind` alternatives (six parser categories and
one existing test marker); config projects them directly at its boundary. SnakeYAML has no
legacy string table; the corpus runner also reuses `binder::token_kind` and the shared
kinds' live mappings. Validation kinds
project into the shared vocabulary and delegate their rendering, avoiding duplicate mappings.

## Analytics response and transport boundaries

The transport uses Hyper and `hyper-timeout`. Its source-chain walk selects kinds from
`io::ErrorKind`, retaining the existing deepest recognized source rule:

| Typed source | Kind |
| --- | --- |
| `ConnectionRefused` | `Connect` |
| `ConnectionReset`, `BrokenPipe` | `Socket` |
| `TimedOut` (including socket-idle errors) | `SocketTimeout` |
| No recognized transport source | `Io` |

No locally built failure message selects retry behaviour. HTTP status and Retry-After
rules retain their existing consecutive-response semantics. `wraps_as_analytics_io` is an
explicit set matching the old `java.io.`/`java.net.` wrapper condition, distinct from the
SSH and APK I/O catch sets.

The v1 server body is an external protocol. `analytics::parse_v1_error_body` runs once at
the HTTP response boundary (or recorded-input boundary), returning classification plus a
separate retry disposition. In the following table, the first matching row selects the
category; any matching `Never` row disables retry, regardless of body order:

| Server phrase | Category | Retry |
| --- | --- | --- |
| `database not found` | `DatabaseNotFound` | Never |
| `points beyond retention policy` | `PointsBeyondRetentionPolicy` | Never |
| `field type conflict` | `FieldTypeConflict` | Never |
| `unable to parse` | `UnableToParse` | Never |
| `hinted handoff queue not empty` | `HintedHandOffQueueNotEmpty` | Never |
| `cache-max-memory-size exceeded` | `CacheMaxMemorySizeExceeded` | Retry |
| `user is required to write to database` | `AuthorizationFailed` | Never |
| `user is not authorized to write to database` | `AuthorizationFailed` | Never |
| `authorization failed` | `AuthorizationFailed` | Never |
| `username required` | `AuthorizationFailed` | Never |

A generic body selects `General` and `Retry`. Thus cache-memory followed by username-required
keeps the cache classification and disables retry. `BatchWriter::retryable` only reads the
typed disposition; cancellation remains non-retryable and transport failures remain retryable.
On the `BatchWriter::send` producer path, the old phrase predicate could only ever match
server bodies: `influx::request` passes successful `Http::request` replies to
`checked(reply, false)`, the only producer copying response text into failures. Transport
operations feed `network_error`, wrapped as `InfluxIo` by `influx::request`; URL/URI,
request-builder and redirect/protocol failures are generated locally by `Http::request`.
These transport and local failures never carry the nine phrases. Non-v1 failures are
therefore retryable whatever their message says, including a synthetic `InfluxIo` or
`IllegalState` containing a v1 phrase. A regression contrasts these synthetic failures
with the identical phrase received through `checked` as a server body.

`http.rs` tests pin all phrases, every ordered pair in both body orders, generic bodies,
the cache-memory mixture and typed I/O sources with unrelated messages. Its loopback test
pins read/write idle timeouts. `influx.rs` pins typed retry decisions independent of message
text; existing `transport_faults.rs` and client oracle suites pin real faults, retry counts,
causes and output bytes.

## Catch sets, keys and completion

`is_fatal` replaces the exact 33-member JVM Error whitelist, including ThreadDeath, with
negative tests for Error-looking injected names. The shared SSH parent graph preserves its
old deliberately narrow equivalence classes. Domain tests pin the three connect cases,
the Android retry I/O set, the five APK I/O cases, JSON-vs-mapping wrapping and device loss.

Analytics deduplication uses `(kind.dedup_key(), message)`: the key ignores v1 retry metadata
because the old `(class, message)` key did. A backend-level regression proves identical
class/message failures log once even when retry disposition differs, while a different
class or message remains separate.

The complete selector ledger is `docs/wp/8.6a-selectors.md`; test changes and acceptance grep
are recorded beside it. No scope omissions, stop conditions, D-rows, fixture updates or parity
row changes remain. External text, KotlinValue and YAML writing remain outside this WP; 8.6b
can replace each domain's mapping independently, including the preserved Debug rendering.


# Plain failure wording proposal (WP 8.6b, milestone 1)

**Design stop: proposed, not implemented or approved.** The 8.6a sections above describe
current behavior. This section proposes D59; no rendering, class mapping, config-dump
schema or existing assertion has changed at this milestone.

## Proposed rendering contract

`Display` for each kind supplies the description below. `Display` for `Failure` prints
`<description>: <message>` when the message is present (including an empty message), and
only `<description>` when it is absent. `stack_trace` retains its cause order, `Caused by: `
prefixes, frame text and newline rules. User-visible embedded errors use `Display`,
including the SSH clock-error fallback in `sshj/executor.rs`; developer `Debug` may expose
typed fields but will no longer own a legacy class-name mapping. Audit other formatting
sites by the error value's type, not by blindly replacing every debug formatter.

The CLI's existing uncaught envelope (`Exception in thread "main" `) remains
unchanged: it is outside the kind description, and the brief limits this change
to class names and embedded failure rendering. Removing that envelope would be
an additional wording decision, not part of the proposed D59 replacements.

Every description is a lower-case noun phrase. Acronyms are lower-case as well. The
common `Io` description is `i/o error`; `error` here describes the problem, rather than
a Java `Error` suffix or catch hierarchy. Fatal descriptions preserve their fatal
identity even though those kinds should arise only from recorded/injected failures.

**Merges: none.** All 208 concrete recordable kinds have distinct descriptions.
Domain wrappers delegate rather than introduce another description. Validation aliases
below deliberately share the description of the same projected kind; they are not
merges of distinct `FailureKind` values. Influx retry metadata does not affect wording.
`External(String)` remains verbatim, including any external class name. No external
instrumentation stack/shortMsg, runner output, logcat, XCTest or xcresult text is parsed
or rewritten to make it fit this table.

## Wording table

### Common and runner kinds

| Kind | Proposed description |
| --- | --- |
| `FailureKind::Security` | security restriction |
| `FailureKind::ReplayClock` | clock replay failure |
| `FailureKind::DeviceLost` | device lost |
| `FailureKind::DeviceSetup` | device setup failure |
| `FailureKind::IncompatibleDevice` | incompatible device |
| `FailureKind::NoDevices` | no devices available |
| `FailureKind::NoTests` | no tests found |
| `FailureKind::BatchExecution` | test batch execution failure |
| `FailureKind::TestParsing` | test discovery failure |
| `FailureKind::Transfer` | file transfer failure |
| `FailureKind::Eof` | unexpected end of input |
| `FailureKind::FileNotFound` | file not found |
| `FailureKind::Io` | i/o error |
| `FailureKind::InterruptedIo` | interrupted i/o |
| `FailureKind::UncheckedIo` | unchecked i/o error |
| `FailureKind::Arithmetic` | arithmetic failure |
| `FailureKind::ArrayIndexOutOfBounds` | array index out of bounds |
| `FailureKind::ClassCast` | incompatible value type |
| `FailureKind::Generic` | failure |
| `FailureKind::IllegalArgument` | invalid argument |
| `FailureKind::IllegalState` | invalid state |
| `FailureKind::IndexOutOfBounds` | index out of bounds |
| `FailureKind::NegativeArraySize` | negative array size |
| `FailureKind::NullReference` | missing value |
| `FailureKind::NumberFormat` | invalid number |
| `FailureKind::Runtime` | runtime failure |
| `FailureKind::StringIndexOutOfBounds` | string index out of bounds |
| `FailureKind::UnsupportedOperation` | unsupported operation |
| `FailureKind::Bind` | socket bind failure |
| `FailureKind::Connect` | connection failure |
| `FailureKind::NoRouteToHost` | no route to host |
| `FailureKind::PortUnreachable` | unreachable port |
| `FailureKind::Protocol` | protocol failure |
| `FailureKind::Socket` | socket failure |
| `FailureKind::SocketTimeout` | socket timed out |
| `FailureKind::UnknownHost` | unknown host |
| `FailureKind::AccessDenied` | access denied |
| `FailureKind::FileAlreadyExists` | file already exists |
| `FailureKind::FileSystem` | filesystem failure |
| `FailureKind::InvalidPath` | invalid path |
| `FailureKind::NoSuchFile` | missing filesystem entry |
| `FailureKind::GeneralSecurity` | security operation failure |
| `FailureKind::DateTime` | invalid date or time |
| `FailureKind::DateTimeParse` | date or time parsing failure |
| `FailureKind::NoSuchElement` | missing element |
| `FailureKind::Cancelled` | cancelled |
| `FailureKind::Timeout` | timed out |
| `FailureKind::PatternSyntax` | invalid regular expression |
| `FailureKind::Zip` | archive failure |
| `FailureKind::UninitializedProperty` | uninitialized property |
| `FailureKind::CopyDestinationExists` | copy destination already exists |
| `FailureKind::CopySourceMissing` | copy source missing |
| `FailureKind::JobCancelled` | job cancelled |
| `FailureKind::TimeoutCancelled` | operation cancelled after timeout |
| `FailureKind::ChannelReceiveClosed` | receive channel closed |
| `FailureKind::ChannelSendClosed` | send channel closed |
| `FailureKind::Replay` | replay failure |
| `FailureKind::DependencyCreation` | dependency creation failure |
| `FailureKind::MissingDefinition` | missing dependency definition |

### ADB

| Kind | Proposed description |
| --- | --- |
| `AdbKind::PullFailed` | adb pull failure |
| `AdbKind::PushFailed` | adb push failure |
| `AdbKind::RequestRejected` | adb request rejected |
| `AdbKind::RequestValidation` | invalid adb request |
| `AdbKind::UnsupportedForwardingSpec` | unsupported adb forwarding specification |
| `AdbKind::UnsupportedImageProtocol` | unsupported adb image protocol |
| `AdbKind::UnsupportedSyncProtocol` | unsupported adb sync protocol |
| `AdbKind::ConnectRefused` | adb connection refused |
| `AdbKind::ConnectTimeout` | adb connection timed out |
| `AdbKind::ClosedChannel` | adb channel closed |
| `AdbKind::TransportClosed` | adb transport closed |

### DEX

| Kind | Proposed description |
| --- | --- |
| `DexKind::Decode` | dex decoding failure |

### Configuration

| Kind | Proposed description |
| --- | --- |
| `ConfigKind::Rejected` | configuration rejected |
| `ConfigKind::JsonParse` | json parsing failure |
| `ConfigKind::StreamConstraints` | input stream limit exceeded |
| `ConfigKind::JsonEof` | unexpected end of json input |
| `ConfigKind::JsonMapping` | configuration mapping failure |
| `ConfigKind::InvalidDefinition` | invalid configuration definition |
| `ConfigKind::InvalidFormat` | invalid configuration value format |
| `ConfigKind::InvalidTypeId` | invalid configuration type identifier |
| `ConfigKind::MismatchedInput` | configuration input type mismatch |
| `ConfigKind::UnrecognizedProperty` | unrecognized configuration property |
| `ConfigKind::ValueInstantiation` | configuration value construction failure |
| `ConfigKind::YamlParse` | yaml decoding failure |
| `ConfigKind::YamlMarked` | yaml location failure |
| `ConfigKind::MissingParameter` | missing required configuration parameter |
| `ConfigKind::Yaml` | yaml failure |
| `ConfigKind::Parser` | yaml parsing failure |
| `ConfigKind::Reader` | yaml reading failure |
| `ConfigKind::Scanner` | yaml scanning failure |

### SSH

| Kind | Proposed description |
| --- | --- |
| `SshKind::KeyDecryptionFailed` | ssh key decryption failure |
| `SshKind::Base64Decoding` | ssh base64 decoding failure |
| `SshKind::Buffer` | ssh buffer failure |
| `SshKind::CircularBuffer` | ssh circular buffer failure |
| `SshKind::General` | ssh failure |
| `SshKind::Connection` | ssh connection failure |
| `SshKind::OpenChannel` | ssh channel opening failure |
| `SshKind::Transport` | ssh transport failure |
| `SshKind::Authentication` | ssh authentication failed |
| `SshKind::Encryption` | ssh key encryption failure |
| `SshKind::Pem` | ssh pem decoding failure |
| `SshKind::Password` | ssh key password failure |
| `SshKind::Pkcs` | ssh pkcs decoding failure |

### Analytics

| Kind | Proposed description |
| --- | --- |
| `AnalyticsKind::BadRequest` | influxdb bad request |
| `AnalyticsKind::Forbidden` | influxdb access forbidden |
| `AnalyticsKind::InfluxV2` | influxdb v2 failure |
| `AnalyticsKind::InternalServerError` | influxdb internal server failure |
| `AnalyticsKind::NotFound` | influxdb resource not found |
| `AnalyticsKind::ServiceUnavailable` | influxdb service unavailable |
| `AnalyticsKind::Unauthorized` | influxdb authentication required |
| `AnalyticsKind::UnprocessableEntity` | unprocessable influxdb request |
| `AnalyticsKind::FluxCsvParser` | flux csv parsing failure |
| `AnalyticsKind::FluxQuery` | flux query failure |
| `AnalyticsKind::Graphite` | graphite failure |
| `AnalyticsKind::InfluxV2Provider` | influxdb v2 provider failure |
| `AnalyticsKind::JsonEncoding` | json encoding failure |
| `AnalyticsKind::InfluxIo` | influxdb i/o error |
| `AnalyticsKind::InfluxMapping` | influxdb mapping failure |

### InfluxDB v1

| Kind | Proposed description |
| --- | --- |
| `InfluxV1Kind::General` | influxdb v1 failure |
| `InfluxV1Kind::AuthorizationFailed` | influxdb v1 authorization failed |
| `InfluxV1Kind::CacheMaxMemorySizeExceeded` | influxdb v1 cache memory limit exceeded |
| `InfluxV1Kind::DatabaseNotFound` | influxdb v1 database not found |
| `InfluxV1Kind::FieldTypeConflict` | influxdb v1 field type conflict |
| `InfluxV1Kind::HintedHandOffQueueNotEmpty` | influxdb v1 pending handoff queue |
| `InfluxV1Kind::PointsBeyondRetentionPolicy` | influxdb v1 points outside retention policy |
| `InfluxV1Kind::UnableToParse` | influxdb v1 input parsing failure |

### Android

| Kind | Proposed description |
| --- | --- |
| `AndroidKind::TestAnnotationProducerNotFound` | android test annotation producer not found |
| `AndroidKind::CommandRejected` | android command rejected |
| `AndroidKind::DeviceNotSupported` | unsupported android device |
| `AndroidKind::Install` | android installation failure |

### Apple

| Kind | Proposed description |
| --- | --- |
| `AppleKind::NotAdmitted` | apple device not admitted |
| `AppleKind::NotBooted` | apple device not booted |
| `AppleKind::TransferStalled` | apple file transfer stalled |
| `AppleKind::SetupDropped` | apple device setup dropped |
| `AppleKind::Boot` | apple device boot failure |
| `AppleKind::Command` | apple command failure |
| `AppleKind::DeviceFailure` | apple device failure |

### Reports

| Kind | Proposed description |
| --- | --- |
| `ReportKind::AllureResultsWrite` | allure results write failure |
| `ReportKind::ImageIo` | image i/o error |
| `ReportKind::ImageMetadata` | invalid image metadata |
| `ReportKind::Transformer` | xml transformation failure |
| `ReportKind::Sax` | xml parsing failure |

### Fatal kinds

| Kind | Proposed description |
| --- | --- |
| `FatalKind::WindowSystem` | fatal window system failure |
| `FatalKind::Io` | fatal i/o error |
| `FatalKind::Assertion` | fatal assertion failure |
| `FatalKind::BootstrapMethod` | fatal bootstrap method failure |
| `FatalKind::ClassCircularity` | fatal circular type dependency |
| `FatalKind::ClassFormat` | fatal invalid class format |
| `FatalKind::General` | fatal failure |
| `FatalKind::Initialization` | fatal initialization failure |
| `FatalKind::IllegalAccess` | fatal illegal member access |
| `FatalKind::IncompatibleClassChange` | fatal incompatible type change |
| `FatalKind::Instantiation` | fatal instance creation failure |
| `FatalKind::Internal` | fatal internal failure |
| `FatalKind::Linkage` | fatal linking failure |
| `FatalKind::NoClassDefFound` | fatal missing type definition |
| `FatalKind::NoSuchField` | fatal missing field |
| `FatalKind::NoSuchMethod` | fatal missing method |
| `FatalKind::OutOfMemory` | fatal out of memory |
| `FatalKind::StackOverflow` | fatal stack overflow |
| `FatalKind::ThreadDeath` | fatal thread termination |
| `FatalKind::Unknown` | fatal unknown failure |
| `FatalKind::UnsatisfiedLink` | fatal unresolved native link |
| `FatalKind::UnsupportedClassVersion` | fatal unsupported class version |
| `FatalKind::Verify` | fatal verification failure |
| `FatalKind::VirtualMachine` | fatal virtual machine failure |
| `FatalKind::AnnotationFormat` | fatal invalid annotation format |
| `FatalKind::CoderMalfunction` | fatal text codec malfunction |
| `FatalKind::ServiceConfiguration` | fatal service configuration failure |
| `FatalKind::Zip` | fatal archive failure |
| `FatalKind::ParserFactory` | fatal xml parser factory failure |
| `FatalKind::StreamFactory` | fatal xml stream factory failure |
| `FatalKind::TransformerFactory` | fatal xml transformer factory failure |
| `FatalKind::NotImplemented` | fatal unimplemented operation |
| `FatalKind::ReflectionUnsupported` | fatal unsupported reflection |

### Injected and replay markers

| Kind | Proposed description |
| --- | --- |
| `InjectedKind::ReplayStep` | injected replay step failure |
| `InjectedKind::NotBootedProbe` | injected not booted probe failure |
| `InjectedKind::RunTimeout` | injected run timeout |
| `InjectedKind::TransferException` | injected transfer failure |
| `InjectedKind::CleanupError` | injected cleanup failure |
| `InjectedKind::ExecutionError` | injected execution failure |
| `InjectedKind::After` | injected after hook failure |
| `InjectedKind::Callback` | injected callback failure |
| `InjectedKind::Test` | injected test marker |
| `InjectedKind::ParserFixture` | injected parser fixture failure |
| `InjectedKind::NotAdmitted` | injected admission failure |
| `InjectedKind::SinkFailure` | injected sink failure |
| `InjectedKind::TestFailure` | injected test failure |
| `InjectedKind::TestTask` | injected test task failure |
| `InjectedKind::TimeoutCancellation` | injected timeout cancellation |
| `InjectedKind::TransferStalled` | injected stalled transfer |
| `InjectedKind::Cancelled` | injected cancellation |
| `InjectedKind::Lost` | injected device loss |
| `InjectedKind::Probe` | injected probe failure |
| `InjectedKind::ProbeAssertionError` | injected probe assertion failure |
| `InjectedKind::ProbeExceptionNamedError` | injected error named probe failure |
| `InjectedKind::QualificationConfig` | injected qualification configuration failure |
| `InjectedKind::TestBackend` | injected backend failure |
| `InjectedKind::TestBarrier` | injected barrier failure |
| `InjectedKind::TestBlocking` | injected blocking operation failure |
| `InjectedKind::TestCarbon` | injected carbon failure |
| `InjectedKind::TestClock` | injected clock failure |
| `InjectedKind::TestClose` | injected close failure |
| `InjectedKind::TestError` | injected generic failure |
| `InjectedKind::TestFactory` | injected factory failure |
| `InjectedKind::TestPeer` | injected peer failure |
| `InjectedKind::TestQuery` | injected query failure |
| `InjectedKind::TestWatchdog` | injected watchdog failure |
| `InjectedKind::Timeout` | injected timeout |

### Delegation and validation aliases

| Kind | Description source |
| --- | --- |
| `FailureKind::Config(k)` | `ConfigKind::k` |
| `FailureKind::Ssh(k)` | `SshKind::k` |
| `FailureKind::Analytics(k)` | `AnalyticsKind::k` |
| `FailureKind::Dex(k)` | `DexKind::k` |
| `FailureKind::Adb(k)` | `AdbKind::k` |
| `FailureKind::Android(k)` | `AndroidKind::k` |
| `FailureKind::Apple(k)` | `AppleKind::k` |
| `FailureKind::Report(k)` | `ReportKind::k` |
| `FailureKind::Fatal(k)` | `FatalKind::k` |
| `FailureKind::Injected(k)` | `InjectedKind::k` |
| `FailureKind::External(class)` | verbatim `class` |
| `AnalyticsKind::InfluxV1 { problem, retry }` | `InfluxV1Kind::problem`, independent of `retry` |
| `ValidationKind::Configuration` | configuration rejected (`ConfigKind::Rejected`) |
| `ValidationKind::FileNotFound` | file not found (`FailureKind::FileNotFound`) |
| `ValidationKind::Zip` | archive failure (`FailureKind::Zip`) |
| `ValidationKind::Io` | i/o error (`FailureKind::Io`) |
| `ValidationKind::NoSuchFile` | missing filesystem entry (`FailureKind::NoSuchFile`) |

`RetryDisposition::{Retry, Never}` are policy metadata, not failures. SnakeYAML's
`TokenErrorKind` stays independent: config's existing `binder::token_kind` projection
selects the table's shared kind; it must not acquire a second wording table.

## Baseline coverage before wording changes

`android-instrumentation-eof` uses the ordinary one-device Android CLI capture,
APK discovery, installation, runner, listeners and reporters. A new native script
starts `com.example.MainActivityTest#testText`, then closes the instrumentation
stream with exit 0 without sending a result. `runner::stream_ended_early` creates
`FailureKind::Eof`; this is a Marathon-generated failure, not external failure text.
The run fails with CLI exit 1; the fake exits 0, teardown is joined and hooks are consumed.
The uncompleted retry quota is zero, so the started test becomes a final failure.

The baseline value is:

```text
Instrumentation failed: java.io.EOFException: the instrumentation stream ended before a result, with exit code 0
```

It appears in these files (`{safe_serial}` is the declared device role):

| Result path under `reports/` | Value location |
| --- | --- |
| `tests/omni/marathon_junit_report.xml` | testcase failure text |
| `allure-results/10000000-0000-0000-0000-000000000001-result.json` | `statusDetails.message` and `statusDetails.trace` |
| `test_result/omni/{safe_serial}/com.example.MainActivityTest#testText.json` | `stacktrace` |
| `html/pools/omni/{safe_serial}/com.example.MainActivityTest.testText.html` | embedded `window.test` stack trace and the rendered log text |

`test_result/raw.json` contains status/timing/metadata but no failure text. The
runner also logs this failure; native console comparison intentionally retains
only the pinned progress/summary lines. Result files therefore provide the gate's
wording witness. The new contract pins the full manifest, parsed report values,
transport and final fake state with the same rules as the existing Android cases.
Its golden is `fixtures/contract/native-v1/android-instrumentation-eof.json.gz`.
It has explicit native-only registration, not fabricated historical JVM evidence.
The documented `bless` command captures all adapters; the other 17 views were unchanged.

No extra Apple failure or config-error whole-CLI adapter is added at this stop:
the Android case covers all four required reporters, and the existing config
oracle/staging suites already cover parse/logical failures and ordered causes.

## Oracle comparison proposal

Copied fixtures, recorded scenario tokens, tapes and historical JVM captures stay
unchanged. Keep `marathon-test-support/src/recorded_errors.rs` as the single owner
of legacy recorded names. Replace its dependence on production `legacy_class`
with an explicit whitelist-owned list of `(recorded name, kind)` values. Its tests
must prove both directions of coverage: every name parses to its expected kind,
and an exhaustive match over every recordable concrete kind has a whitelist name.
This is input-format coverage, not a new production name renderer. Unknown names
remain errors, with no suffix guessing, fallback kind or `External` escape hatch.
Influx v1 keeps recorded category and independently reconstructed retry metadata.

For in-memory oracle suites, parse the expected recorded class through that
whitelist and compare the actual kind directly, then the same message, position,
path, stage, retry disposition and ordered causes that the suite checks today.
Retain each suite's existing normalization and explicitly permitted message
exceptions, including Jackson-raised cause wording (PLAN §3.10). Add test-only
access to the whitelist where needed; no production dependency on test support.
Use typed catch membership to identify those Jackson cases after parsing, rather
than a package-name test on new wording.

For binary-level `config_dump_goldens` and CLI log/trace oracle comparisons, adapt
only the expected error structures/known generated failure slots. Parse the
recorded class prefix once (the delimiter is the first `: `, or the complete
class token if no message is present); preserve null versus empty messages,
multiline messages, ordered causes and frame text. Compare the actual kind or
stable config identifier to the parsed expected kind and separately assert the
exact table-driven rendering. Generated nested failure text needs the same
site-specific projection, for example a device-lost message that embeds its
cause. Never rewrite arbitrary message substrings: an external `shortMsg`, stack
or HTTP body containing `java.io.IOException` must stay byte-for-byte text.
Fixtures containing opaque debug formatting require a narrow structural adapter
at their known recorded failure field, not a blanket regex over JSON or logs.

The Python config differential driver (`tools/oracle/config-differential-lib/suite.py`)
also validates the error schema and compares cause strings. Give it a test-only Rust
bridge built from test support that accepts recorded class/message inputs and emits
stable identifiers, descriptions and the existing typed Jackson-cause category.
Python must consume this bridge, not duplicate the whitelist or infer a kind from
the new description. Keep historical `class`-schema evidence validation separate
from native `kind`-schema validation, and preserve the driver's exact configuration
and final-filesystem checks. Add adversaries for changed kind, messages, cause
count/order and external class-looking text. The existing Python comparison
self-tests use synthetic classes; replace them with whitelisted witnesses where
they exercise the new typed boundary, without accepting unknown real recordings.

The corpus runners (`marathon-config/examples/corpus.rs` and config test support),
ADB/DEX oracles, analytics response/transport tests, Apple/iOS oracles, core
run-flow and device-actor logs, and CLI config dumps all use this boundary.
No local duplicate Java-name table may be introduced to keep a test compiling.
Pure selector assertions should compare kinds directly; rendering tests assert
literal proposed descriptions, not derive both actual and expected from `Display`.
Add regression coverage for unchanged external class text, absent/empty messages,
causes/frames and the SSH clock fallback's use of `Display` instead of `Debug`.

Historical whole-CLI differential verification remains unchanged: its stored
native/JVM pairs keep their original text. Fresh native captures use native goldens,
with the new Android failure case supplying the D59 result-file witness. Do not
normalize class names away from native-golden comparison.

## Config-dump proposal

Replace only the top-level error classification key `class` with `kind`. Its value
is an explicit stable lower-case identifier, independent of description wording:
common variants use kebab-case (`file-not-found`, `io`, `no-such-file`); domain
variants add a domain prefix (`config.rejected`, `config.json-mapping`,
`config.missing-parameter`, `config.yaml-parse`). Freeze these identifiers in tests;
do not derive them from `Debug`, Rust type paths or a Java name. The renderer
accepts typed kinds, not caller-supplied class strings. Example:

```json
{
  "stage": "parse",
  "error": {
    "kind": "config.rejected",
    "message": "Invalid configuration",
    "causes": ["configuration mapping failure: invalid value"]
  },
  "config": null,
  "fsEffects": []
}
```

Keep `message` nullable. Keep `causes` as an array of strings, outermost first,
replacing just each class prefix with its proposed description. Preserve this
format's existing `: null` for an absent cause message, distinct from `Failure`'s
message-less `Display`; an empty cause message still ends in `: `. No new fields,
structured causes or unrelated key/array ordering changes are proposed. `stage`,
canonical configuration, filesystem effects, formatting and exit status stay the same.

The JVM config-dump fixtures continue to contain `class`. The comparison parses
that value through the whitelist, compares the native `kind` with its explicit
identifier, and compares cause descriptions with the parsed expected kinds plus
the same messages as today. This is an intentional D59 schema change, documented
in `config-dump-format.md` when implemented; it does not pretend the old/new error
JSON schemas are identical.

**Corpus golden effect: none expected.** `config_contract.rs` stores accepted
canonical configurations and rejection outcomes (`exit`, `accepted`, and `stage`
where present). It discards `error`, its class, messages and causes, including for
schema rejection cases. It also binds Marathondevices separately. Inspect the
computed corpus after implementation; do not re-bless unchanged data. If another
necessary change makes corpus data differ, use only
`MARATHON_BLESS_CONFIG=1 cargo test -p marathon-cli --test config_contract`, review
and explain every difference. Never regenerate historical JVM config dumps.

## Expected tests and golden changes after approval

No existing test assertion is weakened at this design stop. The only changed test
file so far is `tools/differential/test_evidence.py`, adding
`test_native_only_scenario_rejects_a_historical_jvm_capture`; existing tests keep
their assertions. `differential_vendors` discovers the added case from the registry
and runs it through the existing full-report comparison.

After approval, the following files currently consume legacy rendering in tests
or in oracle support and are expected to change. The list includes classification
assertions that will compare typed kinds instead of rendering a class. Each path
is relative to the directory in the first column; `.rs` applies to every name.

| Directory | Expected test / test-support files |
| --- | --- |
| `crates/jcompat/tests` | `string_substitutor.rs` |
| `crates/marathon-error/src` | `lib.rs`, `config.rs`; new exhaustive description, uniqueness and delegation coverage for all domain modules |
| `crates/marathon-test-support/src` | `recorded_errors.rs` (replace legacy round trips with exhaustive whitelist coverage) |
| `crates/marathon-test-support/tests` | `fake_simulator_worker.rs` |
| `crates/marathon-adb/src/client` | `oracle_tests.rs` |
| `crates/marathon-adb/tests` | `instrument_request.rs` |
| `crates/marathon-analytics/src` | `http.rs`, `influx.rs`, `transport_faults.rs` (in-source tests) |
| `crates/marathon-analytics/tests` | `clients.rs` |
| `crates/marathon-android/tests` | `android_artifacts.rs`, `android_runner.rs`, `device_test.rs`, `dex_test_parser_test.rs`, `installer_test.rs`, `models_test.rs`, `reconnect_test.rs`, `remote_parser.rs` |
| `crates/marathon-apple/src` | `artifact_processor/tests.rs`, `xcresult/tests.rs`, `plist/mod.rs` (in-source test) |
| `crates/marathon-apple/tests` | `apple_bin_oracle.rs`, `apple_device.rs`, `apple_device_test_runner.rs`, `apple_logparser.rs`, `apple_test_bundle.rs`, `batch_failure_policy.rs`, `code_coverage_targets.rs`, `nm_test_parser.rs`, `reader.rs`, `remote_file_manager.rs`, `skip_copying_derived_data.rs`, `ssh_connect.rs`, `ssh_key_formats.rs`, `ssh_known_hosts.rs`, `ssh_real_sshd.rs`, `ssh_session.rs`, `test_root_factory.rs`, `xcodebuild_session.rs`, `xctest_parser.rs`; SSH clock-fallback regression in `ssh_session.rs` |
| `crates/marathon-cli/tests` | `production_runner.rs`, `config_dump_goldens.rs`, `cli_matrix.rs`, `cli_contract.rs`, `run_outcome.rs`, `xctest_parser_supply.rs`, `support/mod.rs` |
| `tools/oracle/config-differential-lib` | `suite.py` (native error schema and typed expected-error bridge; keep historical self-validation unchanged) |
| `crates/marathon-config/examples` | `corpus.rs` |
| `crates/marathon-config/tests` | `binding.rs`, `binding_review.rs`, `bundle.rs`, `config_dump_staging.rs`, `configuration_factory.rs`, `devices.rs`, `dynamic.rs`, `schema.rs`, `validation.rs`, `validation_corpus.rs`, `vendor.rs`, `vendor_corpus.rs`, `support/bind.rs`, `support/load.rs`, `support/schema_bind.rs` |
| `crates/marathon-core/src` | `error.rs`, `artifact_io.rs` (in-source tests) |
| `crates/marathon-core/tests` | `allure_reporter.rs`, `analytics_remote.rs`, `artifact_services.rs`, `device_actor_oracle.rs`, `event_pipeline.rs`, `filter_edges.rs`, `html_reporter.rs`, `junit_reporter.rs`, `marathon_scenarios.rs`, `pool_scheduler.rs`, `progress_oracle.rs`, `reporter_support/mod.rs`, `run_flow_oracle.rs`, `run_flow_witnesses.rs`, `runtime_review.rs`, `runtime_review2.rs`, `screenshot.rs`, `support/device_actor.rs`, `support/pool.rs`, `timeline_reporter.rs` |
| `crates/marathon-dex/tests` | `dex_versions.rs`, `differential.rs`, `unit.rs` |
| `crates/marathon-ios/tests` | `planning.rs`, `recovery_support/mod.rs`, `simulator_batch.rs`, `simulator_batch_failure.rs`, `simulator_oracle.rs`, `simulator_prepare.rs`, `simulator_provider_edges.rs`, `simulator_setup.rs`, `simulator_support/artifacts.rs`, `simulator_support/mod.rs`, `simulator_world.rs`, `world_context.rs` |
| `crates/marathon-macos/tests` | `batch.rs`, `permissions.rs`, `provider.rs` |

This is an expected-impact inventory, not a claim that every assertion in each
file must change. Check direct fixture consumers too: a fixture-supplied Java
name need not appear literally at the assertion site. Record the final exact
changed-file list and every assertion change beyond wording in
`docs/wp/8.6b-test-changes.md`. Keep tests of selectors, causes, retry behavior,
statuses, ordering and external text. No test deletion or parity-row change is
planned; any necessary deletion needs `n/a` with `8.6b: D59` and review.

`progress_oracle::progress_line_contract_for_each_status`,
`reporters::stdout_summary_contract` and
`started_ci_passes_fraction_and_prints_after_ci` keep their progress/summary
assertions. Their production format strings carry no generated exception class.
`progress_oracle.rs` has a separate invalid-number fallback that currently pins
an exception rendering; adapt that path without changing valid progress lines.

| Golden / evidence set | Expected change |
| --- | --- |
| `fixtures/contract/native-v1/android-instrumentation-eof.json.gz` | Four report files: `java.io.EOFException` becomes `unexpected end of input`. Allure has two occurrences; the HTML test page has embedded JSON and rendered text; JUnit and per-test JSON have one each: six value occurrences in four files. The message and every unrelated value stay unchanged. |
| Other 17 `fixtures/contract/native-v1/*.json.gz` views | No expected value changes; confirm with the documented diff tool. |
| `fixtures/contract/native-v1/provenance.json` | Source/driver provenance refresh from the documented bless; report separately from product values. |
| `fixtures/contract/config/corpus.json` | No expected data change because rejection text is excluded; verify, do not bless needlessly. |
| `fixtures/oracle/**`, `fixtures/differential/**`, copied fake/tape inputs | No edits or re-recording. Parse recorded expectations at their boundary instead. |

After wording implementation, re-bless only with the documented tool and run
`tools/differential/run diff-golden --against e34dadc1`.
Write `docs/wp/8.6b-golden-diff.md` with files, before/after values, counts and
separate provenance changes. Anything beyond approved class-prefix/description
changes in native result values is a failure to investigate. Add D59 to PLAN
§3.11 only with the implementation, including the config-dump `class` to `kind`
change and plain cause prefixes. Update this design and the config-dump format
then; the Phase 8 external-text exclusion and exit codes stay in force.

## Risks and review points

- Fine distinctions such as `FileNotFound` versus `NoSuchFile`, common versus
  fatal i/o, replay clock versus replay, and injected versus vendor markers
  remain separate in both wording and selectors. The table intentionally uses
  longer qualifiers to avoid accidental semantic merges.
- The whitelist must remain exhaustive after production mappings disappear.
  Analytics retry metadata must not be lost when a recorded name is decoded.
- Oracle adapters can mask a regression if they transform arbitrary text or
  derive all expected wording from the same renderer. Use typed comparisons,
  literal rendering tests and the unnormalized native failure golden together.
- Config-dump's hidden error schema changes deliberately. Stable identifiers
  must remain separate from future editorial changes to descriptions; the
  existing null-cause string convention remains explicit.
- The baseline contains one internal kind, not every vendor kind end to end.
  Exhaustive kind rendering tests and existing domain oracles cover the rest.
- Instrumentation EOF occurs over a real loopback stream. Keep the current
  bounded capture and full manifest comparison, and verify it again in the
  gate rather than relying only on the bless run.
