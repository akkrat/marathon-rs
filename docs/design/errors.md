# Typed failures (WP 8.6a)

Milestone 1 proves the representation and one core funnel. Rendered bytes remain unchanged;
the narrower 8.6a brief applies even where PLAN §3.12 permits wording changes. No D-row or
fixture update is needed. Milestones 2–5 await design review.

## Types and crate graph

`marathon-error` is a dependency-free leaf crate. It owns `FailureKind`, re-exported from
`marathon_core::error`. The first version has 27 named alternatives plus `External(String)`.
`Failure`, its cloneable cause chain, `OperationError`, `Cancellation` (including timeout
scope identity), and `VendorError` remain in core. No trait objects or downcasts enter them.

ADB, DEX and config cannot depend on core. They will depend directly on the leaf crate,
as core does now; vendor integrations already depending on core may use its re-export.
All shared kind definitions and their mappings have one owner. In later milestones,
`FailureKind` will wrap domain enums also defined in `marathon-error`:

| Type | Meaning / consumers |
| --- | --- |
| `AdbKind` | Request rejection, validation, sync/image protocols, transport; ADB and Android |
| `DexKind` | DEX decoding; DEX and Android (common I/O kinds are reused) |
| `ConfigKind` | Binding, coercion, YAML/JSON decoding and configuration rejection; config and core |
| `SshKind` | Connection, authentication, channel, key decoding and transport; Apple, iOS, macOS |
| `AnalyticsKind` | Influx v1/v2, Flux, Graphite and response decoding; analytics and core |
| `AndroidKind`, `AppleKind` | Vendor-specific failures outside ADB/SSH |
| `ReportKind` | Reporter/encoding failures, without moving reporter implementations |
| `FatalKind` | Existing JVM Error catch distinctions, including ThreadDeath |

Producer error structs keep their domain data (for example SSH disconnect reasons,
suppressed errors and the no-common-algorithm marker); projection into `Failure` moves
the shared kind directly. No conversion renders a class and parses it back. Common I/O,
argument, state, cancellation and runner classifications stay top-level alternatives.
`FatalKind` and SSH/DEX membership methods replace the old string hierarchy helpers.

## Milestone-1 bridge and its removal

As approved for this design stop, `Failure.class: String` temporarily remains public so
unconverted consumers still build. Its field comment explicitly says milestone 2 removes
it. `Failure::from_kind(kind, message)` obtains that string exclusively from
`kind.legacy_class()`. There is no independently supplied class to disagree with the kind;
tests check each converted I/O path against both the mapping and literal baseline text.
The milestone-1 object stores the compatibility string, not a second copy of the kind.

`artifact_io.rs` is the converted funnel: I/O classification, NIO reason selection,
file-open and clock failures, and worker-completion failures use kinds. Its decisions
never inspect the compatibility string. Other funnels and selectors remain for their
scheduled milestones; the final grep acceptance is not claimed at this stop.

Milestone 2 changes the field to `pub kind: FailureKind` and makes `from_kind` store it.
It updates `to_java_string` to call `kind.legacy_class()`; `stack_trace` continues through
`to_java_string`. Required consumer edits replace struct-literal fields and rendered-class
reads across the workspace in the same buildable change. Domain alternatives must exist
before those constructors migrate; no generic legacy/unknown-string alternative is added.
Each domain's later milestone completes its selectors and producer-specific error types.
There will be no permanent string constructor or string equality implementation on kinds.

## Legacy rendering and external text

Each kind type has one `legacy_class` mapping. Wrapper variants delegate to their domain
mapping; unrelated distinctions never share a rendering accidentally. In particular,
`Replay` renders `marathon.ReplayError`, while `ReplayClock` renders `ReplayError`.
Nullable/empty messages, cause order, frame text and newline handling stay unchanged.

`External(String)` makes `legacy_class` return `&str` rather than `&'static str`. Use it
only if an adapter receives a separate class name from a runner and projects that name
verbatim into a `Failure`; annotate every such construction with its external source.
No production site has needed it in milestone 1. Instrumentation stack/shortMsg, logcat,
XCTest/xcresult and command output already held in messages or traces remain opaque there;
do not split those strings to manufacture an `External` kind. The new external-text test
models a runner-provided class and proves embedded class names remain text.

## Recorded-input boundary

Convert recorded class names once in the replay/scenario input adapter, before a typed
failure enters execution. Use a fallible, explicit whitelist for that recording format;
scenario short names such as `DeviceLostException` are format tokens, not class suffixes
to guess. Unknown names take the existing invalid-input path (for example the unsupported
`batch-failed` exception error), never `External`, a default kind, or an internal panic.
Keep recorded expectation strings as expected rendered bytes. Copied tapes and fixtures
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

These counts overlap. Hierarchy-only names may disappear; test injection names and
separate retry classifications may add alternatives. Milestone 1 implements 28 alternatives
in the leaf crate; the final shared vocabulary is expected to be roughly 130–170 named
alternatives across the top-level and domain enums, subject to the boundary audit.

## Risks for the remaining milestones

- The analytics transport is currently Hyper plus `hyper-timeout`, not reqwest. Use its
  typed transport/I/O sources, retaining refused/reset/broken-pipe/socket-idle distinctions
  and cancellation scope identity. Status/Retry-After retries are already typed; preserve
  their consecutive-response rules. Existing transport-fault and oracle tests remain exact.
- Influx v1 `http::v1_error_class` classifies server body text, first matching phrase wins.
  `BatchWriter::retryable` separately rejects any of nine phrases, even a later match;
  cache-max-memory-size is classified but is retryable. A single chosen server-error enum
  cannot preserve mixtures containing that phrase and a later non-retryable phrase.
  Keep response classification and retry disposition separately typed at the response
  boundary. Pin each phrase, order/mixtures, generic responses and transport failures.
- HTTP status alone cannot reproduce all v1 body categories. The server exposes textual
  error descriptions; confirm the protocol classification boundary during milestone 2.
  Also audit arbitrary transport error messages against today's substring-based retry
  decision. If equivalent typed information cannot preserve a decision, stop and report
  rather than changing retries or introducing a D-row silently.
- Preserve SSH inheritance and message fallback, config wrapping stages and cause order,
  fatal-vs-exception catches and deduplication keys. `FailureKind` derives `Hash` for the
  latter, but deduplication is not converted at this stop.
