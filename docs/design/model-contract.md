# Model and construction contract

WP 0.6; Kotlin `e47352f2`. Unless otherwise qualified, source paths below
are under `core/src/main/kotlin/com/malinskiy/marathon/`. No serde derive or
Rust Debug rendering is an external serializer. The merged jcompat writers
supply explicit Gson/Jackson/Kotlin writers; WP 3.9 selects the correct one.

## Equality, identity and serialization

| Type / source | Value equality and hashing | Reference identity | Serialized/output form |
|---|---|---|---|
| Test (`test/Test.kt:5–23`) | Only pkg, clazz, method; metadata ignored. Rust Eq/Hash implement this equivalence; Rust hash bytes are not Java Objects.hash integers | Separate test allocations can be equal; do not deduplicate copies merely because they are equal | All four constructor fields, in order: pkg, clazz, method, metaProperties. Kotlin data-class toString includes metadata even though equals ignores it |
| MetaProperty (`test/MetaProperty.kt:3`) | name and map contents; map entry order does not affect equals | No special identity for the property itself | name then values; insertion order of map entries retained |
| MetaValue (`MetaProperty.values: Any?`; Android DEX discovery) | Boxed type matters: Int differs from Long; NaNs compare equal, signed zeros differ. Lists ordered, maps key/value-based; arrays and ordinary objects identity-based; typed Decoded wrappers follow the rules below | Arc allocation retained for arrays/objects; cloning an Arc preserves aliasing | Full typed values for Gson. RawJsonReporter calls toString on each value; JVM arrays leak identity strings (C19), never reinterpret arbitrary literal strings as identities |
| TestBatch (`test/TestBatch.kt:5`) | Ordered tests plus id; Test equality still ignores metadata | `BatchRef = Arc<TestBatch>`; use Arc::ptr_eq for QueueActor reject (:179), never id or structural equals | tests then id. Allocate id explicitly through `core.TestBatch.id`, never in Clone or a hidden Default |
| Device (`device/Device.kt`) | Frozen implementations inherit Object equality; DeviceRef Eq is pointer identity | Replacement with same serial gets a fresh Arc; retain old handles for late notifications | Serialize DeviceInfo snapshots where Kotlin toDeviceInfo is called, not the live trait object |
| DeviceInfo (`device/DeviceInfo.kt:5–15`) | All seven constructor fields; feature lists preserve order | Does not substitute for Device identity | operatingSystem, serialNumber, model, manufacturer, networkState, deviceFeatures, healthy. Omit safeSerialNumber$delegate everywhere (D3). Computed safeSerialNumber is not a Gson field |
| OperatingSystem (`device/OperatingSystem.kt:3–6`) | version string | None | version only; major is a computed nullable integer, not a field |
| DevicePoolId (`device/DevicePoolId.kt:3`) | name string | None; this is a value key | object containing name, not a bare string unless the particular serializer selects name |
| DevicePool (`device/DevicePool.kt:3–6`) | name and ordered devices with their Device identity equality | Updating collection does not create new Device identities | name and devices; not the primary report snapshot format |
| TestShard (`execution/TestShard.kt:5`) | Ordered tests and flakyTests, preserving duplicates | None | tests then flakyTests |
| TestStatus (`execution/TestStatus.kt:3–8`) | Enum identity | None | FAILURE, PASSED, IGNORED, INCOMPLETE, ASSUMPTION_FAILURE; preserve ordinal order |
| TestResult (`execution/TestResult.kt:6–37`) | Eight constructor fields, including testBatchId and attachments. Test's metadata remains excluded transitively | None | test, device, testBatchId, status, startTime, endTime, stacktrace, attachments, then backing field isTimeInfoAvailable (C17). Null stacktrace omitted by Gson. No duration/isIgnored/isSuccess fields |
| TestBatchResults (`execution/TestBatchResults.kt:5–10`) | Device identity and ordered finished/failed/uncompleted collections | Retains original Device allocation, not an equivalent new wrapper | Constructor fields when serialized; scheduling carries it internally |
| Attachment (`execution/Attachment.kt:5–26`) | File, type, nullable name | None | file (absolute path through FileSerializer), type enum name, optional name. empty is computed from filesystem and is not serialized |
| TestBundle (`execution/bundle/TestBundle.kt`; Apple/Android/StubTestBundle classes) | All three are ordinary classes, not data classes; BundleRef Eq uses identity | Same path/id does not make separately allocated bundles equal; identifiers must return stable handles | Vendor-specific fields belong to vendor serializers; id is not a universal structural key |
| DeviceEvent (`device/DeviceProvider.kt:6–9`) | Kotlin event classes use identity; Rust deliberately exposes no Eq | Carried DeviceRef retains device identity; event wrapper identity is not used by core | Internal channel message, not a report record |
| Failure / VendorError (exception package) | Rust structural comparison is diagnostic only, not Kotlin Throwable.equals | No Throwable object identity is used for device classification; enum kind and cause chain are authoritative | Explicit class/message/cause formatter, never Rust Debug |
| ExecutionCommand (`config/ExecutionCommands.kt`) | Parse outputFileName/includeFlakyTests; Run singleton | No resource identity | CLI command, not Marathonfile schema |
| String / &str | Unicode text, Rust scalar-value ordering (D55) | None | Lone surrogates are replaced at input boundaries; contract-sensitive lengths still count UTF-16 units |
| JavaPath (`jcompat::file_manager`) | Stored Java File path spelling, not canonical/inode identity | None | Gson writes absolute path; config dump preserves noncanonical `.` components. File construction normalization belongs to 2.1, not native PathBuf normalization |
| Instant / Duration (`jcompat::jtime`) | Signed seconds + normalized nanos | None | Merged Java parsing/formatting; never std::time::Duration for negative durations |
| Configuration (`configuration/.../Configuration.kt`) | Kotlin compares serial regex source strings but hashes Regex identity (F7); approved pattern-based Rust Eq/Hash implemented in `schema.rs` | No emulated Regex identity API | P1 binding, P3 YAML, config-dump and toMap are distinct views, not one derive |

`MetaMap` enforces unique keys with private entries and preserves insertion
order. Inserting a duplicate key replaces its value in place, matching Kotlin
Map/LinkedHashMap. MetaProperty.values and MetaValue::Map both use it; callers
cannot construct duplicate-key states that break Eq symmetry/reflexivity.
Only Node maps retain duplicate keys; WP 2.1 owns that representation.
There is no generic object variant: WP 6.4's producer audit found nothing that
constructs one, so `MetaValue::Object`/`MetaObject` were removed (6.4 P7).
Custom annotation/value classes must be represented by their actual scalar,
list/map or typed vendor adapter.

Feature collections at the frozen vendor call sites are lists. These model
vectors preserve that list equality and order; no unordered set coercion is
allowed. Source-specific Set operations (batch deduplication and parked-device
sets) retain their insertion order in the actor/strategy implementations.

Use jcompat's **numeric** formatters where exposed; Rust
Hash only supplies equivalent-key behaviour for internal lookups. Never depend
on randomized Rust HashMap iteration for an output or Java container traversal.
Configuration derives lawful PartialEq/Eq/Hash in `schema.rs`; its regex
fields use F7's approved pattern-based equality and hashing. Keep original
regex pattern spelling for output. Do not add JVM identity
hashing or violate Rust's Hash/Eq invariant.

TestResult.isTimeInfoAvailable is computed as startTime != 0 && endTime != 0
by the serializer from the frozen constructor values, and excluded from Eq.
Its Kotlin toString omits batch ID/attachments and truncates stacktrace to
24 UTF-16 units. Test's name/class-name formatting uses Kotlin isNotBlank and
its separators (`Test.kt:26–51`), not Rust's default whitespace/Debug rules.
Computed getters/formatters are implementation work in the consuming WPs:
OperatingSystem.major (3.2), test names (1.7), TestStatus.plus and result status
predicates (3.5), attachment empty checks (3.9a), serializers (1.5/3.9).
Attachment MIME types remain image/gif, image/jpeg, image/png, image/webp,
video/mp4, text/plain and application/octet-stream; labels are screen, log,
logcat, xcodebuild-log and profiling. AppType strings are cli and gradle-plugin.

### DEX annotation wrappers (R1-wonder-5 / R2-wonder-1)

`vendor/vendor-android/.../DexTestParser.kt:47–65` unwraps only the annotation's
**top-level** DecodedValue. DecodedArrayValue becomes its DecodedValue[];
elements are not recursively unboxed. WP 6.4 constructs `MetaValue::Array`
with JVM class `[Lcom.linkedin.dex.parser.DecodedValue;` and every element a
`MetaValue::Decoded(DecodedWrapper)`. Nested arrays retain their wrapper.
The frozen typed representation is:

```rust
MetaValue::Decoded(DecodedWrapper)
enum DecodedWrapper {
    DecodedByte(i8), DecodedShort(i16), DecodedInt(i32), DecodedLong(i64),
    DecodedFloat(f32), DecodedDouble(f64), DecodedChar(u16), DecodedBoolean(bool),
    DecodedString(String), DecodedType(String), DecodedEnum(String),
    DecodedNull,
    DecodedArrayValue(Arc<[DecodedWrapper]>),
}
```

Wrapper kind participates in equality: String, Type and Enum remain distinct
although all three payloads are JVM Strings. Scalar wrappers compare their
payloads; Float/Double use Java compare semantics (all NaNs equal, signed zeros
distinct). DecodedNull represents the one JVM singleton, so all instances of
that variant are equal; it differs from MetaValue::Null. Array wrappers compare
**backing array identity**, not wrapper allocation or contents: two wrappers
around the same Arc are equal; fresh equal-content arrays are unequal. Cloning
preserves that Arc. Outer MetaArray equality remains allocation identity. No
Java hash integer is exposed or required, and Rust Eq stays lawful.

`execution/filter/AnnotationDataFilter.kt:35–40` renders each element using its
wrapper's Kotlin toString, then performs substringAfter("(value=") followed
by substringBefore(")"). Missing delimiters leave the input unchanged. The
first occurrence wins; this is not structured extraction or recursive matching
(C28). `DecodedWrapper::kotlin_value` supplies this rendering through the merged
Kotlin writer. D58 supersedes identity rendering: `DecodedNull` is `null`,
including inside wrapper arrays, and `MetaArray` renders as `[elements]` using
the same rendering for each element as outside an array. Outer annotation arrays
still match per element after the existing delimiter extraction; unboxed null
still does not match. Character scalars retain UTF-16 units and numbers use Java formatting.

The pinned `com.linkedin.dextestparser:parser:2.3.4` JAR (SHA-1
`9279e0742f97d099cd0b07df66ca1045479ff156`) was inspected with `javap -p -c`
and a JVM probe against Gson **2.11.0**, the version in the reference build.
The nested JVM classes are exactly `com.linkedin.dex.parser.DecodedValue$`
followed by the variant names above. Scalar classes have one `value` field;
DecodedArrayValue has `values`; DecodedNull has only static INSTANCE. Its base
class has only static Companion. Gson excludes those static fields.

| Element wrapper | Kotlin toString | Filter input | Gson JSON |
|---|---|---|---|
| DecodedInt(7) | `DecodedInt(value=7)` | `7` | `{"value":7}` |
| DecodedString("a)b") | `DecodedString(value=a)b)` | `a` | `{"value":"a)b"}` |
| DecodedNull | `null` (D58) | `null` | `{}` |
| DecodedArrayValue([Int(7), Int(8)]) | `DecodedArrayValue(values=[DecodedInt(value=7), DecodedInt(value=8)])` | `7` | `{"values":[{"value":7},{"value":8}]}` |
| DecodedArrayValue([]) | `DecodedArrayValue(values=[])` | `DecodedArrayValue(values=[]` | `{"values":[]}` |

All other scalar wrappers use `Decoded<Kind>(value=<Java scalar text>)`.
Byte/Short/Int/Long/Float/Double project to a Gson number, Boolean to a JSON
boolean, and Char/String/Type/Enum to a JSON string (Char is one UTF-16 unit).
Nonfinite Float/Double values retain Java toString for filtering, but default
Gson rejects their JSON serialization. Null wrappers serialize as `{}`, never
JSON null, even when nested. Nested array wrappers project recursively to an
object with the sole `values` array field, including for empty arrays. There
is no class discriminator, enum tag, identity field or Rust variant name in
JSON. The typed serde report views (WP 8.4 JSON, WP 8.9 YAML) preserve these
exact wrapper fields; they must not be flattened or serialized through a
blanket derive of the discovery enum. YAML also retains undotted non-finite
values as strings and uses explicit float tags to preserve finite decimal data.
Top-level DecodedNull is still unboxed to MetaValue::Null (and default Gson
omits a null map entry); a top-level decoded array serializes as a bare JSON
array **of wrapper objects**, e.g. `[{"value":7},{}, {"values":[]}]` (spacing
is illustrative). RawJsonReporter instead calls the outer array's toString,
retaining its JVM array identity string (C19). Element rendering does not
change that raw-report behaviour.

The typed enum covers every pinned DEX wrapper. The generic MetaValue::Object
variant this freeze reserved was removed by WP 6.4 once its producer
reachability audit found no producer (docs/wp/6.4-findings.md, P7). Discovery
and end-to-end writer/filter parity remain with WPs 6.4, 3.9 and 3.3
respectively.

## Crate and construction boundaries

`jcompat` is the canonical implementation, not an interface placeholder:

- Node and model text uses `String` (D55); contract-sensitive lengths count UTF-16 units.
  `file_manager::JavaPath` is the
  path representation throughout config, attachments and output services.
- `jtime::Instant` / `Duration` use validated constructors (`instant_of`,
  `duration_of`) and their existing parsers/formatters; do not redeclare public
  seconds/nanos structs. `jnum` and `formatting` own numeric and date output.
  Default formatting entry points always use en-US (D8). No run/binding
  context carries a machine/default locale.
- `JavaRegex::compile(pattern, field)` takes UTF-8 and reports rejected syntax
  at config load (D6). `matches(&str)` is a full match returning bool;
  `find`/`find_all` use UTF-8 captures, while `split`/`replace` retain their
  merged operation errors. `pattern()` supplies source text for comparisons
  and output. There is no Java-regex backend, UTF-16 regex overload, runtime
  abort outcome or budget. Model text is UTF-8; input boundaries replace lone surrogates
  as described in D55.
- `StringSubstitutor` receives the binder's explicit environment lookup.
  Substitution errors keep their source-defined exception boundary.
- `writers::KotlinValue` retains Kotlin text rendering. JSON and YAML use typed
  serde views (WP 8.4/8.9), with ordered metadata adapters. `MetaValue` remains
  discovery data with boxed types and object identity; it is not another
  serialization value tree. Output views retain the WP 3.9 field selection;
  jnum/jtime text and replay identity hooks remain where their contracts require them.
- `logging::{Logger, MarathonLogging}` own logger handles, registry levels and
  layouts. `Device::logger` returns that handle. Domain LogProducer/LogWriter
  handle test log files and listeners, not a second console logging facade.
- `output::{FileType, FolderType}` re-export the merged file-manager enums.
  `FileTarget::request` bridges model fields and an explicitly formatted test
  name to `Utf16FileRequest`; OutputFiles implementations plan via FileManager
  and materialize off core. `fragment_hash` owns shard hashing.

D60 supersedes WP 1.1's reference heap and comparator contract with
`OrderedQueue<Test, SortKey>` and a synchronous `FnMut(&Test) -> SortKey`.
Each insertion extracts one key, ties are FIFO, and iteration is in priority
order. Removal uses Test equality and removes every matching copy, independently
of its priority or metadata. WP 3.2 P2 (2026-10-24) supersedes 0.6 D4 for queue
construction: WP 3.7 prefetches each pool's declared history keys off core
through WP 3.4a's async MetricsProvider, absorbing fetch failures with defaults.
The synchronous MetricsLookup view answers from memory, so key extraction and
queue operations stay on core. D34 pins backend history for the queue's lifetime;
run-local observed-duration fallback remains live for newly inserted entries,
while existing entries retain their stored keys. Per-test warn-once logs follow
insertion-time lookup order. History queries/TimingLog move to queue construction,
including for queues that Kotlin never compares (PLAN §3.11 D13, limited to the
residual cases listed there).
The prefetch goes through the run-wide memoising MetricsProvider, deduplicated by
`MetricsKey` with single-flight across concurrent pool builds: one query and
TimingLog line per key per run, as in Kotlin (whose coverage report at
`Marathon.kt:180–181` already queries the ExecutionTime keys before any pool
exists). See PLAN §3.11 D13 for the two residual cases.

Outside queue construction, retain the source first-touch boundary. The
concrete queue has no async/fallible key API.

`marathon-config` depends only on jcompat. Configuration lists every top-level
field with its signed Kotlin width. Named private section slots are deliberate:
2.2 fills strategy/filter/analytics representations; 2.3 fills vendor and P2
device schema. No empty successful configs, generic YAML catch-all maps,
Default values, serializers or validation side effects are implemented here.
Opaque slots reserve type names/ownership, not their eventual field layout.

WP 2.1 owns Node/Mark/Span and token shapes, following yaml-spike-results.md;
0.6 removes its duplicate declarations. SourceMark/SourceSpan are private-field
opaque placeholders for the future location types in structured errors, not a
second implementation. The Node contract retains duplicates, raw string
spelling, scalar style, anchors/tags, spans and incomplete collection prefixes.
Stream error roles remain distinct. P1/P2 BindingContext carries the Marathonfile
path (diagnostics use its absolute path), Marathonfile directory, process directory, ordered environment, injectable
now and the CLI analyticsTracking/bugsnagReporting booleans.
ConfigurationLoader::parse includes **all stage-(a) post-processing**: bundle
extraction, path re-resolution and tracking-flag AND merge
(ConfigurationFactory.kt:192–201); the loader calls the vendor hooks internally.
Consumers must not call post_process again after parse. Those CLI booleans can
disable a true config value, but cannot enable a false one. Parse-time work
can perform extraction; logical/vendor validation remains a separate hook, and macOS's skipped
validate call is preserved. Exceptions from substitution remain their original
class outside the ConfigurationException wrapper; callers must not wrap every
ConfigurationError with that class. The diagnostic stage is still Parse.

`marathon-core` owns vendor traits and models. Device methods take owned config,
context, pool and batch handles so a vendor future can be Send + 'static when
wrapped with its Arc<Device>. VendorRuntime explicitly dispatches it to the
worker runtime; Send alone does not imply off-core execution. Typed results
cross through Deferred<T>. Vendor dependencies contain that runtime and the
same clock/tracker/file/barrier services as the run, plus its logging registry.

`marathon-cli::VendorFactories` matches the parsed VendorConfiguration and
calls the selected factory. No independent kind parameter can disagree with
the config. Factories retain their vendor-specific typed resources; core has
no downcasts or service lookup. They return provider plus optional parser,
log configurator and bundle identifier. WP 3.10 resolves missing services
using source-compatible errors before constructing RunDependencies.
WP 2.5's CLI caller gates Stub/Empty with exactly
`No vendor config present in <marathonfile path>` (absolute path;
ApplicationView.kt:79). Empty already fails parse with
`No vendor configuration specified` (ConfigurationFactory.kt:190). Stub/Empty
slots remain for explicit non-CLI/test construction.
Report/event pipeline handles must be constructed from the same per-run
pipeline; nothing uses a global singleton. Parse output remains a separate
service so parse does not trigger report shutdown.

## Compile-only consumer index

`crates/marathon-core/tests/contracts.rs` is compiled by cargo test/clippy,
but its functions are never run. The individual functions label their owning
WPs: configuration_family (2.x), strategies_and_filters (1.1/1.4, 3.2–3.4),
runtime_contract/dispatch_vendor (3.1/3.6), scheduling_contract (3.5–3.8),
vendor_execution/parsing_contract (3.10, 4.x–6.x), reports_and_listeners
(3.9, 4.7/4.9, 5.3, 6.6), transport_contract/bundle_contract (4.1–4.6,
6.1–6.4), replay_contract (7.1–7.2) and finish_contract (3.10, 7.3–7.6).
CLI's `tests/construction.rs` compiles the actual explicit dispatch boundary.
These do not count as ported Kotlin behaviour tests. The five real parity
ports are in `marathon-core/tests/parity.rs`.

Tracker callbacks and log configuration return vendor futures because the
Kotlin implementations can perform blocking I/O. The core awaits their bridge
completion; registration-order callbacks remain sequential. Vendor factories
also return Send futures, permitting constructors that inspect files to run
off core. Constructing that future does not poll it. Backend setup performed
while resolving Koin's run dependencies must still happen before run; only
history queries are deferred by the parse path's includeFlakyTests choice.
