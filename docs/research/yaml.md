# YAML spike research note (WP 0.3b / §4.2, feeds WP 2.1)

How Jackson 2.17.2 + SnakeYAML 2.2 turn Marathonfile text into bound config,
what the Rust replacement must reproduce, and the corpus the spike must run.

## 0. Method and versions

All "Jackson result" claims below marked **[probe]** were executed, not inferred:
a scratch Java probe (`/tmp/yamlspike/Probe{,2,3}.java`, outside any repo) on
the reference JDK 21.0.10 (Android Studio JBR) with the exact jars from the
Gradle cache:

| Artifact | Version | Evidence |
|---|---|---|
| `jackson-dataformat-yaml` | 2.17.2 | Gradle cache |
| `org.yaml:snakeyaml` | **2.2** | `jackson-dataformat-yaml-2.17.2.pom` declares `<version>2.2</version>` |
| `jackson-databind/core/annotations`, `jackson-module-kotlin` | 2.17.2 | Gradle cache |
| `jackson-datatype-jsr310` | 2.17.2 | Gradle cache |

"saphyr result" claims marked **[probe]** were executed against saphyr-parser
**0.2.1** (monorepo `saphyr-rs/saphyr`, commit `1583482`), via a scratch Cargo
project (`/tmp/spike-rs`) that path-depends on a copy of `parser/` with its two
leaf dependencies stubbed (`arraydeque` → `VecDeque`, `thiserror` →
hand-written `Display`; behaviour-neutral for the corpus). crates.io is
unreachable from this environment (403), so the spike will need vendored crates.
Claims marked **[code]** were read from source but not executed; each names its file.

**Correction to PLAN §3.2 before the spike starts:** "an empty scalar becomes
`""`" is true only for *quoted* empty scalars (`''`, `""` → `VALUE_STRING`).
A *plain* empty scalar (`key:` with nothing after it) resolves to `VALUE_NULL`
**[probe T33]**, because `EMPTY_STRING_AS_NULL` defaults to true *and*
SnakeYAML's `EMPTY` resolver maps `""` to `Tag.NULL`. The binding layer must
reproduce both.

## 1. SnakeYAML events → Jackson tokens

Source: `YAMLParser.java` (`jackson-dataformats-text`, tag
`jackson-dataformats-text-2.17.2`), `Resolver.java` / `LoaderOptions.java`
(SnakeYAML 2.2, `-sources` jar from Maven Central).

### 1.1 Wiring: which SnakeYAML machinery is even on the path

Marathon builds `ObjectMapper(YAMLFactory().disable(USE_NATIVE_TYPE_ID))` with
no `LoaderOptions` ([code]
`configuration/.../serialization/ConfigurationFactory.kt:30-33`). `YAMLFactory`
stores `_loaderOptions = null`, and `YAMLParser` then uses `new
LoaderOptions()` ([code] `YAMLParser.java:195-204`). So **all SnakeYAML 2.2
defaults apply**, but only the ones enforced at Scanner/Parser level matter,
because `YAMLParser` drives `new ParserImpl(new StreamReader(...))` directly
and **bypasses `Composer`/`BaseConstructor` entirely**:

| LoaderOptions 2.2 default | Value | Applies to Marathon? |
|---|---|---|
| `codePointLimit` | 3 MiB (3145728) | **Yes** — enforced in `ScannerImpl.fetchMoreTokens` → `JacksonYAMLParseException: The incoming YAML document exceeds the limit` **[probe T65]** |
| `nestingDepthLimit` | 50 | **No** — enforced only in `Composer`, which Jackson never runs **[code]**. Depth is capped instead by jackson-core `StreamReadConstraints` (max 1000) **[probe deep1100]** |
| `maxAliasesForCollections` | 50 | **No** — `Composer`-only **[code]**. Aliases never expand on the Jackson path anyway (see 1.4), so billion-laughs is impossible **[probe T64]** |
| `allowDuplicateKeys` | true | N/A at event level; duplicates surface as repeated `FIELD_NAME`s and databind keeps the **last** silently (see 1.5) **[probe T51/B33]** |
| `allowRecursiveKeys` | false | N/A (constructor-level) |
| `processComments` | false | Comments never surface **[probe T54]** |

### 1.2 Scalar resolution (`_decodeScalar` + `Resolver`)

For an untagged scalar Jackson asks SnakeYAML's `Resolver` (YAML **1.1**
regexes, verbatim below from 2.2 source), then decodes itself:

- `BOOL`: `^(?:yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF)$`
  — note: **no single `y/n`**; only the listed capitalizations (lower,
  First-upper, UPPER).
- `INT`: `^(?:[-+]?0b_*[0-1][0-1_]*|[-+]?0_*[0-7][0-7_]*|[-+]?(?:0|[1-9][0-9_]*)|[-+]?0x_*[0-9a-fA-F][0-9a-fA-F_]*|[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+)$`
  — note: **no `0o` octal**, `08` does **not** match (second alternative needs
  `[0-7]` after `0`, third needs a lone `0` or leading `[1-9]`).
- `FLOAT`: base-10 with `.` and/or exponent, `.inf`/`.nan` (sign only on inf),
  **base-60** (`1:20.5`).
- `NULL`: `^(?:~|null|Null|NULL| )$`; `EMPTY`: `^$`; `TIMESTAMP`: ISOish dates;
  `MERGE`: `^<<$`; `VALUE`: `^=$`.

Token outcomes **[probe]** (input shown as `v: <scalar>`):

| Scalar(s) | Token | Why |
|---|---|---|
| `yes Yes YES`, `no`, `on On ON`, `off`, `true True TRUE`, `false` | `VALUE_TRUE/FALSE` | resolver BOOL, `_matchYAMLBoolean` |
| `y`, `n` | `VALUE_STRING` | resolver has no single-letter BOOL; Jackson's len-1 branch is unreachable for implicit scalars |
| `42 -42 +42 0 -0`, `500_000 1_2 -5_0 42_` | `VALUE_NUMBER_INT` | resolver INT; Jackson strips `_` (`_cleanYamlInt`), so even trailing `42_` works |
| `_42` | `VALUE_STRING` | resolver rejects |
| `017` | `VALUE_NUMBER_INT` (octal 15) | resolver `0_*[0-7]`; Jackson re-decodes as octal |
| `08` | `VALUE_STRING` | resolver rejects; but binds to `8` via String→int coercion (see 2) |
| `0o17` | `VALUE_STRING` | **not** in resolver INT regex; bind to int **fails** |
| `0x1F -0x1F`, `0b101` | `VALUE_NUMBER_INT` | resolver + Jackson hex/binary decode |
| `1:20` | `VALUE_STRING` | resolver says INT (base-60) but Jackson has no sexagesimal decoder → falls back to String |
| `1.5 .5 1e3 5.` | `VALUE_NUMBER_FLOAT` | |
| `.inf -.Inf .nan` | `VALUE_NUMBER_FLOAT` | **but** binding a `double` fails with `Malformed numeric value '.inf'` (Jackson can't re-parse its own token) |
| `1:20.5` | `VALUE_NUMBER_FLOAT` | same bind failure as `.inf` |
| `.` `+` `_` | `VALUE_STRING` | |
| `~ null Null NULL`, empty plain (`v:`) | `VALUE_NULL` | |
| `2001-12-15`, `2001-12-15T02:59:43Z` | `VALUE_STRING` | resolver TIMESTAMP → Jackson `else` branch → String; `JavaTimeModule` parses ISO at bind time |
| `=` | `VALUE_STRING` | resolver VALUE → `else` branch |

### 1.3 Quoted vs plain, explicit tags

- Any **quoted** scalar resolves with `implicit=false` → `Tag.STR` →
  `VALUE_STRING`, verbatim: `'yes'`, `"42"`, `"null"`, `"1.5"` are all strings
  **[probe]**. Coercion to numbers/bools then happens (or fails) in databind (see 2).
- Quoted empty (`''`, `""`) → `VALUE_STRING("")` even with
  `EMPTY_STRING_AS_NULL=true` (the feature only diverts *before* tag
  resolution, and quoted empties resolve to STR, not NULL) **[probe T34/35]**.
- Explicit tags (`scalar.getTag()` non-null): `tag:yaml.org,2002:` prefix is
  stripped and only `binary|bool|int|float|null|str` are honoured; anything else
  (including `timestamp` and **all custom/verbatim tags**) → `VALUE_STRING`
  **[probe T41–T48, T69]**. Details: `!!bool yes` → TRUE (yes/no/on/off *and*
  single `y` accepted under an explicit bool tag **[probe U01]** — wider than
  implicit); `!!int yes` → STRING (decode failure degrades, no error);
  `!!binary ...` → `VALUE_EMBEDDED_OBJECT` (base64+MIME variant);
  `!!null ""` → STRING (the `len > 0` guard in `_decodeScalar`) **[code]**.
- Verbatim `%TAG`/edge tags need no emulation beyond "→ string".

### 1.4 Anchors, aliases, merge keys

- `&x` on a node is metadata only; the node token is unaffected **[probe T49]**.
- `*x` → `VALUE_STRING("x")` — **the anchor name as a string**. "for now,
  nothing to do: in future, maybe try to expose as ObjectIds?" (`YAMLParser.java`)
  **[probe T49/B32/V12]**. A **dangling** `*nope` also yields
  `VALUE_STRING("nope")` with **no error** **[probe T60]** — the event parser
  never validates anchor existence.
- `<<` is an ordinary `FIELD_NAME("<<")`; SnakeYAML performs **no** merge on
  the event path and Jackson does none either **[probe T50]**. A mapping
  `{<<: *b, y: 2}` binds `<<`→`"b"`, `y`→`2`.
- Consequence: the billion-laughs shape is inert — aliases are strings, never
  expanded. No alias cap needed on our side.

### 1.5 Duplicate keys, multiple/empty documents, misc syntax

- Duplicate keys: both `FIELD_NAME`s are emitted; databind keeps the **last**
  with no error, for POJOs and maps alike **[probe T51/B33]**.
  (`FAIL_ON_READING_DUP_TREE_KEY` defaults false, so `readTree` in the six
  hand-written deserializers is last-wins too **[code]**.)
- Multiple documents: `DocumentStart` is skipped (`continue`); the token stream
  concatenates all documents **[probe T52]**. `readValue` binds the **first**
  document; trailing documents are ignored (`FAIL_ON_TRAILING_TOKENS` off)
  **[probe B34]**.
- A leading **empty explicit document** (`---\n---\na: 1`) yields
  `VALUE_NULL` first, so `readValue` **returns null** (Java `null`, not an
  exception) **[probe V01/V02]**. In marathon this `null` escapes the
  `JsonProcessingException` catch in `ConfigurationFactory.parse` and NPEs on
  `configuration.copy` → the "crashed" path, not a `ConfigurationException`.
  The spike must pin this; WP 2.1 must reproduce the crash-vs-error staging.
- Empty input / comment-only input: **no tokens at all** → `readValue` throws
  `MismatchedInputException: No content to map due to end-of-input` (inside the
  `ConfigurationException` wrapper) **[probe V03/V04]**.
- `? a` / `: 1` explicit keys: scalar keys are plain `FIELD_NAME`s; a
  **non-scalar** mapping key in object context is a hard
  `"Expected a field name (Scalar value in YAML)"` error **[code]**.
- Flow `[]`/`{}` are normal arrays/objects **[probe]**; `...` is skipped
  **[code]**; literal `|`/folded `>`/escaped quotes produce plain
  `VALUE_STRING`s (style is invisible to Jackson) **[code]**.
- Rejected at scan/read time: tab indentation (`MarkedYAMLException`)
  **[probe T56]**, control characters (`JacksonYAMLParseException: special
  characters are not allowed`) **[probe T57]**. A leading BOM is consumed
  silently **[probe T58]**.

### 1.6 P3 output shape (for the emitter side, WP 1.5/3.9b)

With marathon's mapper settings (`NON_NULL`, `USE_NATIVE_TYPE_ID` off),
serializing `{name: "hello", count: 42, word: "yes", colon: "a:b", nothing:
null, flag: true}` yields **[probe Probe3]**:

```yaml
---
name: "hello"
count: 42
word: "yes"
colon: "a:b"
flag: true
```

i.e. `---` start, **all** strings double-quoted, nulls dropped, numbers/bools
bare. (Matches PLAN §3.2 P3.)

## 2. Databind coercion under marathon's settings

`ConfigurationFactory.kt` sets: `NullToEmptyCollection=false`,
`NullIsSameAsDefault=false`, `StrictNullChecks=false`, `SingletonSupport=true`
(Kotlin module); `FAIL_ON_UNKNOWN_PROPERTIES` unset (default **true**),
`FAIL_ON_NULL_FOR_PRIMITIVES` unset (default **false**),
`FAIL_ON_NUMBERS_FOR_ENUMS` unset (default **false**),
`ALLOW_COERCION_OF_SCALARS` unset (default **true**) — all four defaults
**[probe]**-printed from the running mapper. Target-type rules below use a
`{int i, long l, double d, boolean b, String s, enum{RED,GREEN} e, List<int>
list}` bean **[probe B/V/C/W series]**:

| Input | Result |
|---|---|
| `i: "42"` (quoted number → int) | `42` |
| `s: 42` / `s: yes` / `s: 1.5` / `s: 017` / `s: 0x1F` (any scalar → String) | raw spelling: `"42" "yes" "1.5" "017" "0x1F"` |
| `i: 1.9` / `i: 1e3` (float → int) | `1` / `1000` (truncation) |
| `i: "08"` | `8` — token is STRING, databind re-parses leniently |
| `i: 0o17`, `i: 1:20` | `InvalidFormatException` (no valid `int`) |
| `i: 42_`, `i: 1_2` | `42`, `12` (token-level underscore stripping) |
| `i: true` (bool → int) | `MismatchedInputException` — **asymmetric**: float→int ok, bool→int rejected |
| `e: 0` / `e: 1` (int → enum ordinal) | `RED` / `GREEN`; `e: 5` → `InvalidFormatException` (index range) |
| `e: "1"` (String → enum) | `GREEN` — ordinal works from strings too |
| `e: RED` / `e: red` | ok / `InvalidFormatException` — **case-sensitive** |
| `i: ""` (empty string → int) | `0`, no error |
| `i: null`, `b: null` (explicit null → primitive) | zero value (`0`/`false`) — `FAIL_ON_NULL_FOR_PRIMITIVES` off |
| `s/e/list: null` (Java bean) | `null` |
| `zzz: 1` (unknown key) | `UnrecognizedPropertyException` — except inside the six hand-written deserializers, which use `readTree` and ignore unknown keys (notably `readOnly` is silently dropped for influxdb/graphite) |
| `b: 1` (number → boolean) | `true` |
| `b: "1"`, `b: "0"`, `b: "yes"` (String → boolean) | `InvalidFormatException` — only `true/True/TRUE/false/False/FALSE` accepted |
| `b: yes`, `b: True`, `b: on` (plain 1.1 words) | `true` (they arrive as `VALUE_TRUE`) |
| `b: y` | `InvalidFormatException` on `"y"` (arrives as STRING) |
| `l: 9223372036854775807` / `+1` over | ok / `JsonMappingException` out of range |
| `d: .inf` / `.nan` / `1:20.5` | `JsonMappingException: Malformed numeric value` |
| `d: 5.` | `5.0` |
| `list: 5` (scalar → collection) | `MismatchedInputException` |
| `list: null` | `null` (`NullToEmptyCollection=false`) |
| `t: "2024-01-01T00:00:00Z"` / `t: 1700000000` (Instant) | ok / epoch seconds |
| `u: "PT30S"` / `u: 30` / `u: PT30S` (Duration) | all `PT30S` — numbers mean seconds |
| `s: ~`, `s:` (null token → String), `s: ''` | `null`, `null`, `""` |

Kotlin deltas (from the module settings in `ConfigurationFactory.kt`, **not**
directly probed — Java beans have no nullability; the spike must confirm each
against a Kotlin data-class oracle):
explicit `null` for a Kotlin primitive (`Int/Long/Double/Boolean`) binds the
zero value **even when the parameter has a default** (`NullIsSameAsDefault=false`
means null never falls back to the default); for a non-null reference it is
rejected; for a nullable or a collection/map it binds `null` (never empty).

Per-field `timeLimit` ([code] the three strategy deserializers): the value is
tried as `Instant`, then as `Duration` added to the injectable "now"; only
`InvalidFormatException` is caught (a shape error such as an array propagates
out). Final failure: `fixed-size` → `null`, `execution-time` →
`ConfigurationException("bbb")`, `probability` →
`ConfigurationException("Unable to deserialize <node> into Instant")`.
`success-rate` has no custom deserializer: absolute Instant (or epoch number) only.
`findValue` is recursive depth-first in document order, so a nested key can
shadow an outer one depending on field order — corpus rows 55–56 pin this.

`File` fields ([code] `FileDeserializer.kt`): `Paths.get(text)`; absolute stays,
relative resolves against the Marathonfile dir via `canonicalFile` (Apple
re-resolution: `canonicalFile` again for `devices`/`knownHostsPath`/SSH key).
P2 (Marathondevices) uses a mapper **without** `FileDeserializer`, so relative
`key` paths resolve against the process CWD instead.

## 3. saphyr-parser 0.2.1, and yaml-rust2 as the alternative

### 3.1 saphyr-parser event API (what WP 2.1 builds on)

- `Parser::new_from_str(&str)` / `new(src)` / `new_from_iter`; pull with
  `next_event() -> Option<Result<(Event, Span), ScanError>>`, or push with the
  `EventReceiver` / `SpannedEventReceiver` traits (`load()` drives a receiver).
- `Event::{Nothing, StreamStart, StreamEnd, DocumentStart(bool explicit),
  DocumentEnd, Alias(usize), Scalar(Cow<str>, ScalarStyle, usize /*anchor*/,
  Option<Cow<Tag>>), SequenceStart(usize, Option<Tag>), SequenceEnd,
  MappingStart(usize, Option<Tag>), MappingEnd}`. `Tag{handle, suffix}` with
  `is_yaml_core_schema()` (`handle == "tag:yaml.org,2002:"`) and `Display`
  (`!suffix` for local tags).
- `ScalarStyle::{Plain, SingleQuoted, DoubleQuoted, Literal, Folded}` — every
  style observed **[probe T33/34/68/71/72/73]**.
- `Span{start, end}` of `Marker`s with `index()` (byte offset), `line()`,
  `col()` (1-based) — enough to name file + key path + cause per §3.10.
- Anchors are **numeric ids from 1** (`0` = none), assigned in definition order;
  redefinition mints a **new** id and later aliases resolve to the latest
  **[probe T70]**; the table is **cleared per document** (source
  `parser.rs:509-510`). Events carry ids, never names.
- `no_std` + `alloc` compatible; license MIT OR Apache-2.0; MSRV 1.85;
  dependencies only `arraydeque` + `thiserror`. Claims "fully YAML 1.2
  compliant" and ships the `yaml-test-suite` harness (`parser/tests/`).

### 3.2 YAML 1.2 vs 1.1: what saphyr does and does not do

The parser performs **no implicit typing at all** — `yes`, `42`, `~` all arrive
as `Scalar` text **[probe]**. All of §1.2 therefore moves into our `Node`
resolution function (see §4), which is good news: there is no 1.2-vs-1.1 fight
inside the library, only surface we must emulate ourselves. Accepted/rejected
**[probe]** deltas that the `Node` layer must close:

| Case | SnakeYAML/Jackson | saphyr 0.2.1 | Fix in our layer |
|---|---|---|---|
| BOM | stripped | kept (`"\uFEFFv"` key!) | strip one leading BOM before parsing |
| `*nope` (dangling alias) | `VALUE_STRING("nope")`, no error | hard `ScanError: unknown anchor`, stream aborts | §4 alias-patch loop |
| `\x01` control char | rejected | accepted as content | reject non-printables ourselves (message ours per §3.10) |
| 3.2 MB scalar | rejected at 3145728 code points | accepted | enforce the same count |
| 1100-deep nesting | rejected over depth 1000 | accepted | enforce depth 1000 in binding |
| empty explicit doc `---` | `VALUE_NULL` token | `Scalar("", Plain)` | map to Null (same rule as `v:`) |
| `<<`, dup keys, multi-doc, `...`, tabs, `=` | §§1.4–1.5 | identical event shapes (`Alias(id)`, repeated keys, `DocumentStart(explicit)`, `DocumentEnd`) | binding rules only |
| `!<foo>` verbatim tag | → string | `Tag{handle:"!!", suffix:"foo"}` | → string |
| `%TAG` directives | resolved by SnakeYAML | tracked (`keep_tags`) | only `tag:yaml.org,2002:` + `!` matter; rest → string |

Saphyr parses a strict superset of what Jackson accepts (minus BOM handling),
so every divergence is closable in our code — none requires rejecting input
Jackson accepts, except by our own explicit checks.

### 3.3 yaml-rust2 (alternative, not recommended)

`yaml-rust2` 0.13.0 (MIT OR Apache-2.0, deps `hashlink`, `arraydeque`,
optional `encoding_rs`) fuses parsing and typing: `Yaml::from_str` resolves
`true/True/TRUE` (+`false` family) to bool, `""|~|null` to null, `0x`/`0o`
prefixes and `i64`/`f64` parses — i.e. YAML-1.2-core-ish, **not** 1.1: `yes`,
`on`, `Null`, `NULL` stay strings, `017` becomes integer 17, `0b101` stays a
string, tag mismatch yields `BadValue` ([code] `yaml-rust2/src/yaml.rs`). It
also owns duplicate-key and alias-expansion policy inside its loader, giving us
less control than saphyr's raw events. Choosing it would mean fighting its
built-in (wrong-for-us) resolution instead of writing our own. Keep saphyr.

## 4. Proposed Rust `Node` design

```rust
struct Span { start: BytePos, end: BytePos } // from saphyr Marker index/line/col
enum Style { Plain, SingleQuoted, DoubleQuoted, Literal, Folded }
struct Tag { handle: String, suffix: String }
enum Node {
    Scalar { text: String, style: Style, tag: Option<Tag>,
             anchor: Option<u64>, span: Span },
    Seq { items: Vec<Node>, anchor: Option<u64>, tag: Option<Tag>, span: Span },
    Map { entries: Vec<(Node, Node)>, anchor: Option<u64>, // Vec, not a map:
          tag: Option<Tag>, span: Span },                   // duplicates+order kept
    Alias { id: u64, span: Span },                          // resolved at bind time
}
struct Document { explicit: bool, root: Option<Node> } // None = empty doc
```

Pipeline: strip one leading BOM → saphyr events → `Vec<Document>` → resolve +
bind first document only (`None` docs / zero docs reproduce §1.5 exactly).

- **Resolution** `resolve(text, style, tag) -> ScalarToken` reimplements
  `Resolver` regexes + `_decodeScalar` + `_decodeNumberScalar` verbatim, including
  the traps: no single-letter bools, no `0o`, `08`→string, sexagesimal→string,
  trailing-underscore ints, `.inf/.nan` as float-tokens-that-fail-binding,
  `!!bool y`→true, `!!int yes`→string, `!!null ""`→string, plain empty→null,
  everything timestamp/custom-tagged→string.
- **Aliases need one vendored diff.** Saphyr's `scanner` module is private, so
  the `id→name` table cannot be rebuilt outside the crate, and `Alias(usize)`
  events carry no name. Proposal: **vendor `saphyr-parser`** (MIT/Apache-2.0
  permits it) with a minimal diff at the alias branch of `parse_node`
  (`parser.rs`, `Some(id) => Ok((Event::Alias(*id), span))`, where the anchor
  `name` is already in scope): carry the name on the event, e.g.
  `Alias { id, name }`. At bind time an alias behaves as **the anchor's name
  string** (Jackson semantics — defined, redefined, or forward-referenced),
  *not* the target node; an `Alias` in mapping-key position is the Jackson
  `"Expected a field name"` error (spike: locate the corresponding saphyr
  state path for key position). Consider upstreaming the diff.
- **Dangling/forward aliases** (the remaining hard gap): saphyr aborts with
  `unknown anchor` where Jackson yields the name string. Proposed
  **alias-patch loop** on top of the vendored event: on that error, read the
  alias name from the source at the error byte offset (`Marker::index`),
  replace that single `*name` occurrence with its double-quoted form, re-parse;
  each iteration fixes ≥1 occurrence, so it terminates after at most
  (aliases + 1) passes over kilobyte-sized input. (Patching is always
  semantics-preserving: Jackson maps *every* value-position alias to its name
  string, defined or not.)
- **Limits**: code-point count 3145728 over the input, nesting depth 1000 in
  binding, both with our own messages (§3.10 allows wording differences).
  Reject non-printable scalars ourselves. No alias-count limit (matches Jackson).
- **Spans** on every node give file + key-path + cause for errors (§3.10) and
  power the mutation corpus (§6.3) and `config-dump` key paths.

## 5. Spike corpus (~60 rows)

Run each row against the Kotlin oracle (P1 profile) and the Rust `Node` +
binding sketch; both must agree on accept/reject *and* on the bound value for
the stated target type. `S/I/B/E/L/D/T/U` = string/int/bool/enum/list/double/
instant/duration field; "token" = token-level check.

| # | Snippet (as `k: …` unless shown) | Jackson result | Notes |
|---|---|---|---|
| C01 | `yes`, `Yes`, `YES` | token TRUE | full bool-word matrix incl. `no/on/off` cases |
| C02 | `true`, `True`, `TRUE`, `false` | token TRUE/FALSE | |
| C03 | `y`, `n` | token STRING | single letters never bool |
| C04 | `'yes'`, `"42"`, `"null"`, `"1.5"` | token STRING | quoted ⇒ always string; coercion at bind |
| C05 | `42`, `-42`, `+42`, `0`, `-0` | token INT | |
| C06 | `500_000`, `1_2`, `-5_0`, `42_` | token INT | underscores stripped, even trailing (`1_2`→12, `42_`→42 probed) |
| C07 | `_42` | token STRING | |
| C08 | `017` | token INT 15 | C-style octal |
| C09 | `08` | token STRING; `I`←`8` | binds via String→int re-parse |
| C10 | `0o17` | token STRING; `I`←error | SnakeYAML 2.2 has no `0o` |
| C11 | `0x1F`, `-0x1F`, `0b101` | token INT | |
| C12 | `1:20` | token STRING; `I`←error | sexagesimal recognized by resolver, undecodable by Jackson |
| C13 | `1.5`, `.5`, `1e3`, `5.` | token FLOAT | |
| C14 | `.inf`, `-.Inf`, `.nan` | token FLOAT; `D`←**error** `Malformed numeric value` | tokenizes but never binds |
| C15 | `1:20.5` | token FLOAT; `D`←error | same trap as C14 |
| C16 | `.`, `+`, `_` | token STRING | |
| C17 | `~`, `null`, `Null`, `NULL` | token NULL | |
| C18 | `k:` (plain empty) | token NULL | **not** `""` — corrects PLAN §3.2 reading |
| C19 | `k: ''`, `k: ""` | token STRING `""` | only quoted empties are strings |
| C20 | `2001-12-15`, `2001-12-15T02:59:43Z` | token STRING | `T`←ISO ok; `T`←epoch-seconds number ok |
| C21 | `!!str yes` | token STRING | |
| C22 | `!!int 42` / `!!int yes` | INT / STRING (no error) | decode failure degrades to string |
| C23 | `!!bool yes`, `!!bool y` | token TRUE | wider than implicit |
| C24 | `!!float 1` | token FLOAT | |
| C25 | `!!binary QUJDRA==` | EMBEDDED_OBJECT | base64 |
| C26 | `!!timestamp 2001-12-15`, `!custom foo`, `=` | token STRING | unknown/verbatim tags ⇒ string |
| C27 | `a: &x 5` + `b: *x`, target `S`/`L` | `b`→`"x"`; `Map`→`{a=5,b="x"}` | aliases are name strings |
| C28 | `b: *dangling` | token STRING, no error | saphyr errors — patch loop |
| C29 | `b: *later` + `a: &later 5` (forward ref) | `"later"`, no error | saphyr errors — patch loop |
| C30 | `a: &x 1` + `b: &x 2` + `c: *x` | `"x"` (name; single string anyway) | id churn invisible through strings |
| C31 | `<<: *b` merge mapping | ordinary key, **no merge** | |
| C32 | `*k: 1` (alias as key) | error `Expected a field name` | Node builder must error |
| C33 | `a: 1` + `a: 2` | last wins, no error | POJO, map and `readTree` alike |
| C34 | `a: 1 --- b: 2` | first doc bound, rest ignored | |
| C35 | `---` + `---` + `a: 1`, full `readValue` | **returns null** → NPE/crash path, not config error | stage this in WP 2.1 |
| C36 | empty input; comment-only input | `MismatchedInputException: No content` | inside `ConfigurationException` |
| C37 | tab-indented `b: 1` | scan error | both reject |
| C38 | `v: \x01` | Jackson rejects | saphyr accepts — our check must reject |
| C39 | BOM + `v: 1` | ok, BOM stripped | saphyr keeps BOM — strip first |
| C40 | 60 scalar aliases | ok | no cap on Jackson path |
| C41 | 60-deep maps | ok | Composer 50-limit bypassed |
| C42 | 1100-deep maps | `StreamConstraintsException` (max 1000) | enforce 1000 ourselves |
| C43 | 3.2 MB scalar | rejected at 3145728 code points | enforce same count |
| C44 | `[1, 2]`, `{a: b}`, `\|`/`>` blocks, `"a\nA"`, `'it''s'`, `...`, `? a` `: 1`, CRLF | all fine | styles invisible to Jackson; saphyr styles recorded |
| C45 | `I: "42"` | `42` | quoted number |
| C46 | `S: 42` / `yes` / `1.5` / `017` / `0x1F` | raw spellings | number→string keeps text |
| C47 | `S: ~`, `S:` (empty) / `S: ''` | `null`, `null` / `""` | |
| C48 | `I: 1.9`, `I: 1e3` | `1`, `1000` | truncation |
| C49 | `I: true` | `MismatchedInputException` | bool→int rejected (asymmetric vs C48) |
| C50 | `E: 0` / `1` / `5`; `E: "1"`; `E: RED` / `red` | RED/GREEN/error; GREEN; ok/error | ordinals incl. from strings; case-sensitive |
| C51 | `I: ""`, `I: null`, `B: null`, `L: null` | `0`, `0`, `false`, `null` | |
| C52 | Kotlin `Int`/`String?`/`String`/`List` ← null | zero / `null` / **reject** / `null` | oracle-pending (module semantics) |
| C53 | `zzz: 1` unknown key | `UnrecognizedPropertyException` | except the six lenient deserializers |
| C54 | `B: 1` / `"1"` / `"0"` / `"yes"` / `yes` / `True` / `on` / `y` | true / err / err / err / true / true / true / err | |
| C55 | `L: 9223372036854775807` / `+1` | ok / out-of-range error | |
| C56 | `L: 5` (scalar → collection) | `MismatchedInputException` | |
| C57 | `U: "PT30S"` / `30` / `PT30S` | all `PT30S` | numbers mean seconds |
| C58 | strategy `timeLimit` failures | fixed-size→`null`; execution-time→`"bbb"`; probability→`Unable to deserialize…`; array value propagates (not `InvalidFormatException`) | |
| C59 | `findValue` shadowing (`{nested:{timeLimit:Y}, timeLimit:X}`) | order-dependent winner | pin actual |
| C60 | P1 emit sample (§1.6) | `---`, double-quoted strings, nulls dropped | WP 1.5/3.9b input |

Plus, outside the YAML layer but in the same oracle run: the env-substitution
truth table (PLAN §3.2) and the Kotlin-nullability rows (C52) against real
data classes — both needed before WP 2.1 binds real types.

## 6. Risks for the spike

1. **Alias-name exposure requires a vendored diff** (§4). If upstream won't
   take it and vendoring is rejected on review, the fallback is our own
   SnakeYAML-compatible scanner (PLAN §4.2 already names it).
2. **First-doc-empty returns null, not an error** (C35) — easy to "fix" by
   accident; the crash-vs-error staging is observable behaviour.
3. **`.inf`/sexagesimal bind failures** (C14/C15) — a natural Rust float parser
   accepts what Jackson rejects; the binding layer must fail closed here.
4. **Oracle gaps**: C52 (Kotlin nullability), exact `findValue` order (C59),
   UTF-16-BOM input (untested on either side) still need JVM runs in the spike.
5. **crates.io unreachable (403)** from this environment — vendored saphyr (and
   its leaf deps `arraydeque`, `thiserror`) must be committed or mirrored
   before 0.3b can build.
