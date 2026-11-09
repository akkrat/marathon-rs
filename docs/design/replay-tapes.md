# Replay tapes and event scripts (version 1)

> WP 8.5 / D58: native rendering no longer allocates object or metadata identity suffixes. The identity tape format below is historical; checked-in oracle tapes remain unchanged.

WP 0.2b defines the inputs shared with the future Rust seams (WP 3.1). These are oracle inputs, not Marathonfile extensions.
No goldens are produced here. The reference is commit `e47352f2`.

## Activation and builds

`tools/oracle/build.sh` builds both distributions. `build.sh reference` builds only `build/marathon-src`; `build.sh patched`
builds only `build/marathon-patched`. The reference receives `0001` (oracle module registration) only. The patched clone
also receives `0002-oracle-hooks.patch`. Both trees are verified against the pinned commit plus their selected patches before
Gradle runs. Never edit the original Kotlin checkout.

All hooks delegate the replaced expression when `-Doracle.hooks.tape=/absolute/tape.json` is absent. In particular, clock,
random, UUID, channel, `runBlocking`, logging and vendor-event behavior remain live. A tape is loaded once per JVM.

Optional properties:

- `oracle.hooks.trace`: JSONL path, one record per hooked call: `seq`, `kind`, `label`, `value`.
- `oracle.hooks.report`: JSON path written at normal JVM shutdown: `active`, per-label `calls`, `unused`, `failures`.

The scenario transcript also embeds that report. Missing/exhausted draws or IDs fail instead of falling back to randomness.
A scheduler-derived clock without a replay loop fails instead of falling back to elapsed host time. Unused explicit entries
are reported; validation rejects them. A pattern generator has no finite tail to consume. The trace is diagnostic: live
transport calls can interleave, even when report output is stable.

## Time, draw, and ID tape

```json
{
  "format": "marathon-replay/tape@1",
  "clocks": {
    "wall": {"mode": "scheduler", "start": "2020-01-01T00:00:00Z"},
    "monotonic": {"mode": "scheduler", "startNanos": 0},
    "scheduler": {"mode": "virtual"}
  },
  "reads": {"core.TemporalTestResult.startTime": [1577836800010, 1577836800020]},
  "draws": {
    "core.RandomOrderSortingStrategy.key": [17, -42],
    "android.AdamDeviceProvider.borrow": ["127.0.0.1:5039:emulator-5554"]
  },
  "ids": {
    "core.TestBatch.id": {"values": ["00000000-0000-0000-0000-000000000001"]},
    "core.AllureReporter.uuid": {"pattern": "10000000-0000-0000-0000-%012d", "start": 1}
  }
}
```

This example shows independent features; a particular scenario supplies only the explicit entries it will consume.
`scenario-tape.json` is the common tape used by the checked-in scenario inputs; `random-order` uses `random-order-tape.json`.

All integers are signed 64-bit JSON integers, except random integer draws, which must fit signed 32-bit. Strings, fractions,
booleans, and overflowing numbers are not coerced to integers. Unknown keys and modes are rejected. Labels are case-sensitive.

Wall and monotonic clocks have four modes:

| Mode | Value |
|---|---|
| `real` (default when omitted) | Original host clock; useful for partial instrumentation, not deterministic replay |
| `frozen` | Wall `start` (ISO instant, millisecond precision) or monotonic `startNanos` |
| `scheduler` | Start plus explicit scheduler milliseconds, scaled by 1,000,000 for monotonic nanoseconds |
| `step` | Start plus the number of previous reads **at that label** times `stepMillis` / `stepNanos` |

`step` requires its nonnegative step field. It is a compact explicit read tape, not passage of host time. A `reads[label]`
array overrides the selected clock mode at that site, one entry per call. Wall entries are epoch milliseconds; monotonic
entries are nanoseconds. This permits independent clocks, discontinuous wall time, and distinct readings at different sites.
Scheduler-derived clocks require virtual scheduler mode. Arithmetic uses JVM signed `Long` semantics.

Scheduler time starts at zero. Each scenario event's `at` is an explicit time-tape entry. `advanceTimeBy` runs tasks strictly
before the target and `runCurrent` drains tasks at the target, including timeouts and readiness launches, **before** delivery
of the external event at that same timestamp. Nothing advances to the next pending timer automatically. Equal-time external
events are delivered in array order, with quiescence between them. Wall and monotonic clocks advance with scheduler time only
when their mode requests it; frozen clocks and per-site read tapes remain independent.

A numeric draw supplies `Random.nextInt()`. A string draw selects an element by its stable key (provider device serial), not
collection index. There is a separate cursor per label. No generator algorithm or cross-site draw order is shared with Rust.

IDs likewise have a per-site cursor. Specify either `values`, or one Java integer format conversion (`%d`, `%012d`, `%08x`, etc.)
with an optional counter `start` (default 1). UUID sites use UUID-shaped strings in the supplied validation tapes. Temporary
file/directory sites use the ID as the random filename component, under `java.io.tmpdir`. Existing files are not overwritten.

`config.Configuration.identities` and `core.RawJsonReporter.identities` inspect actual values before formatting and replace
default JVM object/array identity suffixes with ID allocations. Literal strings are never interpreted as identities.
Custom data-class renderers are preserved (including credential redaction); ordinary generated data-class renderers recurse
into constructor values. Logback messages are already formatted strings and retain their literal text. Repeated references retain the same allocated suffix. These are visible
nondeterminism too: array values in raw metadata remain identity strings, not decoded arrays (known bug C19).

## Core event script

```json
{
  "format": "marathon-replay/scenario@1",
  "name": "one scripted batch",
  "source": "oracle replay seam",
  "entry": "run",
  "tests": [{"pkg": "example", "clazz": "Tests", "method": "works", "metaProperties": []}],
  "devices": [{"serial": "device-1", "behaviour": "scripted"}],
  "events": [
    {"at": 1000, "event": "connect", "device": "device-1"},
    {"at": 2000, "event": "prepare-done", "device": "device-1"},
    {"at": 3000, "event": "batch-result", "device": "device-1", "results": [
      {"test": "example.Tests#works", "status": "PASSED", "start": 0, "end": 1}
    ]},
    {"at": 4000, "event": "end"}
  ],
  "expect": {"completed": true, "value": true}
}
```

Run with the patched oracle binary, `JAVA_OPTS` containing the tape property:

```sh
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
export JAVA_OPTS="-Doracle.hooks.tape=$PWD/tools/oracle/scenario-tape.json"
tools/oracle/build/marathon-patched/oracle-build/install/oracle/bin/oracle \
  scenario tools/oracle/scenarios/scripted.json tools/oracle/build/my-replay
```

Use a fresh `java.io.tmpdir` too when repeating a tape containing temporary-file IDs. The work directory must be empty.
A run writes `output/` and `transcript.json`; exit 0 means expectations passed, 1 means a scenario/driver failure, and 2 means
bad command-line usage. Validation separately captures stdout, stderr, and exit code.

`entry` is `runAsync` by default, matching the original scenario tests. `run` additionally exercises the hooked `runBlocking`
bridge and needs the active patched build. A test dispatcher owns the core context in both reference and patched scenario
runs. `timer: "counter"` reproduces the source tests' mocked Timer: successive `currentTimeMillis()` values 0,1,2,… and zero
for the other members. Otherwise the factory uses its normal SystemTimer, hooked only in the patched build.

`command` defaults to `{"type":"run"}`; parse accepts `type`, `outputFileName`, and `includeFlakyTests`.
`configuration` accepts the source factory's retry quotas/timeouts, `ignoreFailures`, `strictMode`, serial regex lists, and
`retryStrategy`, `poolingStrategy`, `shardingStrategy`, `sortingStrategy`, `batchingStrategy`, `flakinessStrategy`. Strategy
objects have the ordinary Marathonfile shapes. Unknown configuration members fail.

Tests are named `pkg.clazz#method`; metadata entries have `name` and a `values` object. Device defaults match StubDevice:
OS `25`, model/manufacturer/ABI `test`, healthy, connected, no features, prepare/test delays both 5000 ms. Optional fields are
`prepareTimeMillis`, `testTimeMillis`, `crashWithTestBatchException`, `crashWithDeviceLostException`, `reportResultsBeforeCrash`,
and `results` (test ID → ordered status array). `behaviour: "stub"` (default) runs the StubDevice algorithm, including its
per-device timestamp counter and crash behavior. The copied implementation records device calls without changing the Device
instance used in results. `behaviour: "scripted"` suspends until an explicit completion arrives.

| Event | Additional fields / effect |
|---|---|
| `connect`, `disconnect` | `device`; provider event on the registered channel |
| `prepare-done` | Completes the pending prepare |
| `prepare-failed` | Optional `message`; fails prepare and lets the core's retry delay run |
| `batch-result` | `results` with `test`, `status`, optional `start`, `end`, `stacktrace`; must name exactly the active batch's tests |
| `batch-failed` | `exception`: `TestBatchExecutionException`, `DeviceLostException`, or `RuntimeException`; optional `message`, `reportEmptyResultsFirst` |
| `end` | Last event; stops driving time and evaluates expectations |

Before the first event and after every time advance/delivery, `runCurrent()` drains the dispatcher's runnable queue. The
probe then checks every registered actor mailbox and provider-event channel. A nonempty open channel with no runnable task
fails quiescence. Closed-and-drained channels are excluded: coroutines 1.8.1 reports `isEmpty == false` on them. Cancellation
continues to discard actor messages exactly as Kotlin does. Detached jobs are not reparented or repaired by this harness.

`expect` supports `completed`, Boolean `value`, exception class `exception`, `rawJson` (relative path, the source tests'
JSONAssert LENIENT comparison), and `files` (relative output path → expected trimmed text). Validation also compares the
entire report tree and transcript bytes, so LENIENT fixture matching is not the repeatability check.

`transcript.json` records format `marathon-replay/transcript@1`, scenario name, hook activation, an ordered log with scheduler
`t`, completion outcome/time, each expectation result, the hook report, and driver errors. Batch IDs and exact assignments
remain visible. The committed JSON files are scenario **inputs**, not oracle goldens.

## Live Android barriers

A tape may additionally contain `barriers`, an ordered array of `label:key` tokens. Each token admits one suspend block; the
next token is released after that block completes. Calls arriving early suspend; missing, extra, and duplicate pending events
fail. Cancellation before admission leaves the event unconsumed. This controls selected live transport boundaries without
substituting device results:

- `android.AdamDeviceProvider.connected:<adb serial>` wraps device setup and delivery of the provider connection;
- `core.DeviceActor.prepare:<marathon serial>` wraps vendor prepare;
- `core.DeviceActor.execute:<marathon serial>` wraps vendor execute.

Without `barriers`, these wrappers simply call the original block. Stub scenarios use the virtual dispatcher and external
event script instead. The CLI validation runs real fake-adb socket I/O with frozen wall/monotonic clocks and **real scheduler
mode**, using the above barriers. It does not claim to virtualize Vert.x, SSH, process timeouts, or arbitrary live workloads.
The validation generates the barrier keys from the selected ADB port. See `nondeterminism.md` for the remaining boundaries.

### Strict script validation

Scenario, test, device, event, metadata, command and result records are JSON objects; tests/devices/events and completion
results are arrays. Test IDs and device serials are unique. Metadata values are strings. Every non-`end` event requires a
known `device`. Event fields are exactly `at`, `event`, `device`, plus the fields listed in the event table; `end` permits
only `at` and `event`. There is exactly one `end`, at the final array position. All events must be consumed by the driver,
including after core completion; an unconsumed suffix fails the run.

Times (`at`, device delays and completion start/end) and timeout configuration values are nonnegative signed 64-bit JSON
integers. Retry quota fits signed 32-bit. Fractions, numeric strings and overflow are rejected. Event times are nondecreasing;
explicit completion `end` cannot precede explicit `start`. Boolean/string fields require their JSON types. Strategy objects
retain the configuration deserializer's schema. Optional `expect` must be an object containing only `completed` and `value`
(Booleans), `exception` and `rawJson` (strings), and `files` (an object whose values are strings). Explicit nulls, arrays,
coercible scalars and unknown expectation keys are rejected before execution. See `ScenarioSpecTest` for malformed-input regressions.

The committed `random-order-tape.json` adds a finite random-sort draw and billing clock read. Its matching scenario puts two
copies of the same test in the queue via the ordinary flakiness strategy. PriorityQueue therefore consumes a sort draw, but
both copies have the same key: the untaped reference has identical ordering for every random key. This proves active draw
and read consumption in (b)/(c) without discarding any assignment or result-order differences.

Marathon barrier device keys are `host:port:serial` (for example `127.0.0.1:5039:emulator-5554`); `port` is the validation
fake-adb server's selected listening port, not a fixed default ADB port. Provider-connected keys use the bare ADB serial.
Unexpected, exhausted and duplicate pending barriers append failures to the hook report even if application code catches
the exception. Validation rejects any such report.

### WP 3.5 P2 amendment (2026-10-25)

The approved WP 3.5 simplification removes the overwritten
`core.TemporalTestResult.startTime` wall read inside
`TestRunResultsAccumulator.testStarted(test, startTime)`. Each such callback
consumes one fewer labelled wall value than the patched Kotlin replay. The
no-argument start overload still reads Timer (`core.di.Clock`) first; explicit
start times do not. Standalone `TemporalTestResult::from_clock` retains the
`core.TemporalTestResult.startTime` label and propagates exhaustion. No other
clock read is removed or reordered, including default end before missing start
and one Timer read per zero endTime or synthesized missing result.
