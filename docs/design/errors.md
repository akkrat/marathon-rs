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
one existing test marker); config projects them directly at its boundary. Validation kinds
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
