# Regex spike (WP 0.3c / 1.4): research note

Source repo: `$MARATHON_KT`, the fork's branch,
frozen at `e47352f2` (confirmed via `git rev-parse` during this research).
Reference JDK for oracle work: Android Studio's OpenJDK 21.0.10;
The fork's CI uses Temurin 21.0.x (PLAN.md §11.3). `java.util.regex` behaviour is
identical between the two for everything in this note.

Relevant plan sections: §3.11 D6 (unexpressible user constructs are rejected
at config load, never silently reinterpreted), §4.2 (chosen backend:
translator over `fancy-regex` with an explicit backtrack limit; PCRE2 as the
evaluated second backend), §4.3 (the `JavaRegex` jcompat piece), §6.3
(Java-regex corpus in the oracle corpora).

Crate versions checked 2026-10-12 via the crates.io API:
`fancy-regex` 0.19.2, `pcre2` 0.2.11, `regex` 1.13.1, `java_regex` 0.1.0.

## 1. Inventory of regex use sites

Kotlin `Regex` is `java.util.regex` (JDK 21). `Regex.matches(s)` is a **full**
match (`Matcher.matches`); `String.contains(Regex)` / `Regex.containsMatchIn`
is an unanchored **find**; `Regex.find` / `findAll` return the first / all
non-overlapping finds with groups. There is no custom `Regex` deserializer:
`SerializeModule` registers only a `RegexSerializer` (writes
`value.pattern`), so user patterns bind through the Jackson Kotlin module's
single-`String` constructor, i.e. `Regex(pattern)` with default flags.
A syntactically invalid user pattern therefore fails config load inside
Jackson as it does today; the Rust side reproduces the failure as a parse
error (wording is an accepted difference per §3.10, cause included).

### 1.1 User-supplied patterns (all go through `JavaRegex`, all D6 candidates)

| # | Site | Config key | Match mode |
|---|---|---|---|
| U1 | `configuration/.../config/Configuration.kt:39-40,165-166`, used in `core/.../execution/Scheduler.kt:133,137` | `includeSerialRegexes` (`any { it.matches(serial) }`), `excludeSerialRegexes` (`none { it.matches(serial) }`) | full |
| U2 | `configuration/.../config/FilteringConfiguration.kt` (8 filter configs: simple-class-name, simple-test-name, fully-qualified-class-name, fully-qualified-test-name, method, package, annotation, test), used in `core/.../execution/filter/{SimpleClassname,SimpleTestname,FullyQualifiedClassname,FullyQualifiedTestname,TestMethod,TestPackage}Filter.kt`, `AnnotationFilter` | `regex` (exactly one of `regex`/`values`/`file` required) | full (`matches`) |
| U3 | `FilteringConfiguration.kt:193-194` (`AnnotationDataFilterConfiguration`), used in `core/.../execution/filter/AnnotationDataFilter.kt:30,40,42` | `nameRegex` (full match on property name), `valueRegex` (full match on each value / `toString()`) | full |
| U4 | `configuration/.../config/vendor/apple/TestParserConfiguration.kt:16`, used in `vendor-apple/base/.../apple/NmTestParser.kt:108` (`testClassRegexes.all { it.matches(test.clazz) }`) | `testClassRegexes` (default `[Regex("^((?!Abstract).)*Test[s]*$")]` — note: a *default* user-visible pattern using negative lookahead; must keep working) | full |

No user-supplied pattern is used with `split` or `replace` (those call sites
are all hard-coded, §1.2). All user patterns are full-match-only. Hard-coded searches read groups; the
shared API still pins find/findAll, split, replace and continuation in the corpus.
The round-2 use-site re-audit in `regex-spike-results.md` establishes why E1 is
unobservable through the frozen Kotlin marathon.

### 1.2 Hard-coded patterns (ported as unit-tested constants, not user input)

These never see D6 rejection; each becomes a fixed `JavaRegex` (or plain
string operation) with a test pinning its behaviour. `find` vs `matches`
below is load-bearing: `TestRunProgressParser` gates on `matches` then
extracts with `find` on the *same* pattern, so the port must keep both modes
on one compiled object.

| Site | Pattern (Kotlin source) | Mode |
|---|---|---|
| `core/.../extension/StringExtensions.kt:26` (`escapeRegex`) | `[^a-zA-Z0-9\\.\\#]` in `replace(regex, "-")` | replace |
| `core/.../execution/filter/FilterValuesFileReader.kt:18` | `\\s+#.*$` in `replace` (comment strip; `$` = end-of-line-input, single line) | replace |
| `core/.../analytics/metrics/remote/graphite/GraphiteDataSource.kt:26` | `"""` + `\s` + `"""` `.toRegex()` in `replace` (strip all whitespace from query) | replace |
| `vendor-apple/base/.../apple/cmd/WriteFile.kt:17,29` (`PLAIN_PATH`) | `/[A-Za-z0-9_./@+-]+`, `!PLAIN_PATH.matches(dst)` | full |
| `vendor-apple/base/.../cmd/remote/rsync/RsyncFileBridge.kt:97` | `(?s)\n.*\z` in `replace` (cut rsync stdout at first newline; uses `(?s)` + `\z`) | replace |
| `vendor-apple/base/.../listener/ArtifactFinderListener.kt:14` + `logparser/parser/{SessionResults,DiagnosticLogs}PathFinder.kt:5` | `(^\s*\|\s+)/.+\.xcresult\s*$`, `(^\s*\|\s+)/.+\.log\s*$`, `pattern.find(line)?.groupValues?.firstOrNull()` | find |
| `vendor-apple/base/.../logparser/parser/TestRunProgressParser.kt:22,28,36,44,49-55,64,77,127,162` | started/finished/timeout patterns, and `FAILING_TEST_MATCHER` (`(/.+&#124;<unknown>:\d+):\serror:\s[\+\-]\[(.*)\s(.*)\]\s:(\s.*)` at runtime; Kotlin double backslashes encode regex whitespace escapes) | full then find |
| `vendor-apple/base/.../bin/xcrun/simctl/service/SimulatorService.kt:154,41` (`UDID_REGEX`) | `[A-Z0-9\\-]*`, `stdout.findLast { it.trim().matches(UDID_REGEX) }` (note `*`: matches empty string) | full |
| `vendor-apple/base/.../apple/model/AppleTestBundle.kt:68` | `[- ]` in `replace(..., "_")` | replace |
| `vendor-apple/base/.../apple/NmTestParser.kt:123-142` (`filter(contentsRegex)`, `firstMatchOrNull`, `contains`) | unused private helpers; there are no callers or supplied patterns at `e47352f2`. Active `Nm` scans run remote shell `grep`/`cut` commands | no live regex use |
| `vendor-android/.../adam/event/TestAnnotationParser.kt:112,57,84` (`METRIC_ANNOTATION_V2_REGEX`) | `\\p{Graph}+\\([^\\)]*\\)` with `findAll` | findAll |
| `vendor-android/.../adam/log/LogcatMessageParser.kt:77-104,23` | `java.util.regex.Pattern` pieces (`\d\d-\d\d ...`, `\d+`, `\w+`, `[VDIWEAF]`, `.*?`) composed into `HEADER` (`^\[ (...) +(...): *(...) (...)/(...) +\]$`), `matcher.matches()` | full (via `Pattern`, not Kotlin `Regex`) |
| `vendor-android/.../executor/listeners/TestResultsListener.kt:93` | `.+\[\d+]` full match on parameterized method names | full |

Not regex (excluded deliberately): `DeviceFailureParser` (plain
`line.contains`), logback `PatternLayout` imports (logging config, WP 1.6),
`DateTimeFormatter.ofPattern` (time formatting, WP 1.2).

Takeaway for the spike: only four user-facing pattern sources (U1–U4), all
full-match; hard-coded patterns cover replace, full, find and findAll. No live
Kotlin regex split site was found (all split arguments are literal strings/chars). The corpus (§4) must cover both.

## 2. Gap table: JDK 21 `java.util.regex` vs `fancy-regex` 0.19.2 vs `pcre2` 0.2.11

Conventions: ✅ expressible, 🔁 rewritable by the translator (§3),
❌ D6 rejection candidate. `fancy-regex` syntax is the `regex-syntax` 0.8
grammar plus its own extensions (verified against the
[0.19.2 docs](https://docs.rs/fancy-regex/latest/fancy_regex/));
`pcre2` is PCRE2 10.x semantics via the bundled C library (needs a C
compiler at build time and ships `unsafe` of its own — not our code, so the
workspace `unsafe_code = "deny"` lint still holds, but it rules out pure-Rust
builds). Cells marked *spike* need JVM-oracle confirmation in WP 0.3c; the
rest follows from the documented grammars.

### 2.1 Syntax

| Construct | Java 21 | fancy-regex 0.19.2 | pcre2 |
|---|---|---|---|
| Possessive `X?+ X*+ X++ X{n,m}+` | ✅ | ✅ (possessive modifier documented) | ✅ |
| Atomic `(?>X)` | ✅ | ✅ (documented with example) | ✅ |
| `\G` last-match anchor | ✅ | ✅ (documented) | ✅ (`\G` = first-match pos) |
| `\Q..\E` quoting | ✅ | ❌ (`regex-syntax` has no `\Q`; translator must quote) | ✅ |
| POSIX `[\p{Alpha} / [:alpha:]]` (ASCII!) | ✅ | 🔁 (`[[:alpha:]]` natively = ASCII in `regex-syntax`; `\p{Alpha}` must be rewritten to the ASCII ranges) | ✅ (`[:alpha:]` ASCII unless UCP) |
| `\p{javaLowerCase}` etc. (4 Java-only classes) | ✅ | 🔁 (rewrite: lower→`\p{Ll}`, upper→`\p{Lu}`, whitespace→explicit list, mirrored→`\p{Mirrored}` *spike*) | 🔁 (no `java*` classes; same rewrite) |
| `\h \v` horiz/vert whitespace | ✅ | 🔁 (rewrite to explicit char lists, *spike* for exact sets) | ✅ native |
| `\R` linebreak matcher | ✅ (`\r\n\|[\n-\r\x85\u2028\u2029]`) | ✅ (documented; same set, *spike*) | ✅ (same set under BSR_UNICODE default) |
| `\X` ext. grapheme cluster | ✅ | 🔁 (no `\X`; rewrite is impractical → ❌ in practice) | ✅ native |
| Named groups `(?<n>X)` / backref `\k<n>` | ✅ | ✅ (`(?<n>)`, `(?P<n>)`, `\k<n>`) | ✅ (also `(?'n')`, `\g`) |
| Numbered backrefs `\1`, groups in find | ✅ | ✅ | ✅ |
| Lookahead/behind (incl. `((?!Abstract).)` in U4 default) | ✅ | ✅ (variable-length behind behind `variable-lookbehinds`, on by default) | ✅ (fixed/alternation-bounded behind) |
| Inline flags `(?i)(?m)(?s)(?x)` + scoped/negated | ✅ | ✅ (same letters; different `u`/`U`, see §2.2) | ✅ (`(?i)(?m)(?s)(?x)`) |
| `(?d)` UNIX_LINES | ✅ | ❌ (no equivalent; `(?R)` is CRLF-only mode, not the same) | ❌ |
| `(?u)` UNICODE_CASE | ✅ | ❌-as-spelled (its `u` = full Unicode mode, §2.2) | ❌-as-spelled |
| `(?U)` UNICODE_CHARACTER_CLASS | ✅ | ✅-by-default (Unicode classes/word boundary on by default; scoped `(?u:X)` = same) | ✅ via `(*UCP)` verb (not `(?U)` spelling) |
| CANON_EQ (no inline spelling) | ✅ via compile flag only | ❌ | ❌ |
| Class ops `[a-z&&[^bc]]` | ✅ | ✅ (`&&` intersection; *spike* for nested `&&` edge cases) | ❌ (`&&` is literal; rewrite rarely possible) |
| Conditionals `(?(1)…)` / backtrack verbs / subroutines `\g<n>` | ❌ (Java rejects) | ✅ extra (harmless: Java patterns never contain them) | ✅ extra |
| `.` (DOTALL off) | ✅ exc. `\n\r\u0085\u2028\u2029` (+`\r\n` pair) | 🔁 (native `.` = exc. `\n` only → rewrite, §3) | 🔁 (native `.` = exc. `\n` only → rewrite or newline option) |

### 2.2 Semantics

| Behaviour | Java 21 default | fancy-regex 0.19.2 | pcre2 (options in brackets) |
|---|---|---|---|
| `\d \s \w` | ASCII (`[0-9]`, `[ \t\n\x0B\f\r]`, `[a-zA-Z_0-9]`) | Unicode (`\pN`, `\p{White_Space}`, `\p{Alphabetic}+…`); ASCII via `(?-u:…)` | ASCII; Unicode [with UCP] |
| `\b \B` | follows `\w` (ASCII) | follows `\w` (Unicode; ASCII under `(?-u:)`) | follows `\w` (ASCII; Unicode [with UCP]) |
| `(?i)` folding | ASCII-only (4 groups incl. `ſ`/`K` quirks) unless `(?u)` | Unicode simple folding always (with default `unicode` feature) | ASCII; Unicode [with UCP/UTF] |
| `(?u)` region (U-CASE, ASCII classes) | folding↔Unicode, classes stay ASCII | ❌ no such combination (`(?u:)` flips classes too) | ❌ (`(*UCP)` flips classes too) |
| `.` / `^` / `$` line recognition | terminators `\n \r \r\n \x85 \u2028 \u2029` | `\n` only (`(?R)` adds `\r\n` only) | `\n` only [default]; configurable [BSR/newline options] |
| `$` no-MULTILINE | at end, or before **one final terminator** (any of the set) | end of haystack only (*spike*; `$` ≠ `\Z` here — fancy `\Z` is "before trailing `\n`s", also wrong) | at end or before trailing `\n` [default] |
| `\Z` / `\z` / `\A` | `\Z`=end-or-before-final-terminator, `\z`=absolute end | `\Z` documented (trailing-`\n` flavour); `\z` *spike* | ✅ same 3 anchors (trailing-`\n` flavour) |
| `^` MULTILINE at very end of input | never matches at the very end (pinned by the JDK corpus) | differs (*spike*) | differs (*spike*) |
| Case-fold specials (`İ ı Σ ς`) under Unicode folding | simple folding + multi-char rules per UnicodeData | same Unicode version-dependent tables (version skew possible) | same caveat |
| Input model | UTF-16 code units (lone surrogates matchable) | Unicode scalar values (`&str`) | UTF-8 code points [with UTF] |
| `matches()` vs `find()` empty-match advance | `find` advances by one UTF-16 code unit after an empty match | same | same |

### 2.3 Catastrophic backtracking

| Backend | Behaviour on e.g. `(a+)+$` vs `"a"*28 + "b"` |
|---|---|
| Java 21 | unbounded recursive backtracking: seconds-to-hang, then `StackOverflowError` (an `Error`, **not** an `Exception` — Kotlin `catch (e: Exception)` in `NmTestParser.extract` does not catch it; a hostile `testClassRegexes` entry can crash the JVM run) |
| fancy-regex | `RegexBuilder::backtrack_limit` (explicit cap per PLAN §4.2) → catchable `Err(BacktrackLimitExceeded)` at match time. Semantics diverge by construction: Java has no limit. The limit error is input-dependent, so it cannot be a config-load rejection; WP 1.4 must define the runtime mapping (approved D6 extension: fail the run with an error naming the pattern and field; never map exhaustion to a non-match) |
| pcre2 | `match_limit` / `depth_limit` → catchable match error; same input-dependence caveat. JIT optionally changes thresholds |

Implication: neither backend reproduces "hang then SO". That is a deliberate,
defensible runtime change (a distinct budget error avoids a crash), covered by the
D6 runtime extension approved 2026-10-23 (§3.4); it is not
a config-load rejection. The corpus (§4) includes backtracking-heavy cases
with a timeout harness so the JVM side records time-limits rather than hangs.

### 2.4 Third option the spike must score: `java_regex` 0.1.0

[MathieuDutSik/java_regex](https://github.com/MathieuDutSik/java_regex)
claims byte-for-byte `java.util.regex` compatibility (OpenJDK 25), `no_std` +
zero `unsafe`, a `Matcher`-mirroring API (`matches`, `looking_at`, `find`,
`split`, `replace`, `$N`/`${name}` templates), eight documented OpenJDK
quirks reproduced, and JVM differential fuzzing. Its one documented
difference (UTF-16 vs codepoint offsets) is unobservable through marathon's
call sites (none read offsets). Risks: pre-1.0, single-author, unaudited
dependency for a GPL-2.0 binary (license check required), and it contradicts
PLAN §4.2's prescribed backend — adopting it needs orchestrator approval.
The spike should benchmark all three backends against the §4 corpus; if
`java_regex` passes the corpus, it eliminates the translator (§3) and the D6
list shrinks to near-empty. This note's translator design (§3) is written so
it stays valid if fancy-regex wins.

## 3. Translator design (fancy-regex backend)

`JavaRegex::compile(pattern)` runs three stages at config load: **scan →
reject-or-rewrite → `fancy_regex::RegexBuilder`**. Rewriting is purely
syntactic; any doubt rejects (D6: never silently reinterpret).

### 3.1 Normalisation (implemented in the 0.3c spike)

The parser resolves flags at each grammar position, restoring the enclosing state on
leaving a group. It emits explicit code-point classes and assertions; backend flags
never determine Java character semantics.

1. Default `\d\s\w` are ASCII; `(?U)` selects the JDK Unicode classes and also sets
   UNICODE_CASE. `(?u)` independently selects Unicode folding, leaving the classes
   ASCII. Negated and scoped flags follow JDK `Pattern.addFlag/subFlag`.
2. Literals, ranges, properties, scripts, blocks, word classes and boundary mark tests
   use **JDK 21 / Unicode 15.0** tables generated by `tools/oracle/regex-unicode/`.
   This includes every script alias, the exact JDK block-name grammar, all `java*`
   classes (including identifiers), and simple case folding. No host Unicode property
   or case table reaches the emitted pattern.
3. Dot, caret and dollar implement all Java line terminators, CRLF pairing, UNIX_LINES
   and MULTILINE explicitly. A full-match wrapper enforces `\A…\z`. A terminal dollar
   can be omitted only when it cannot affect an atomic choice. For an exposed terminal
   dollar in a regular negative lookahead, consuming final-terminator alternatives
   followed by `\z` keep expensive loops in the linear delegate. A separate LF branch
   checks for final CRLF before the prefix, including searches starting between CR/LF.
   Atomic or assertion-dependent bodies do not qualify; captured negative bodies are D6.
4. `\Q…\E` is removed using JDK quoting rules before parsing, retaining original-source
   offsets for D6 diagnostics. COMMENTS mode skips ASCII whitespace and comments at
   JDK grammar positions, including modifiers, group names, repetition and escapes;
   escaped whitespace remains literal. In particular, whitespace before an initial
   class `^` prevents it from being the negation marker, as in Java.
5. Possessive quantifiers and atomic groups stay atomic. Direct optional `\R` uses
   Java's Ques semantics (CRLF choice is atomic); optional noncapturing/capturing groups
   use Branch semantics and may backtrack to CR; counted groups such as `{1}` use
   deterministic GroupCurly semantics. `{0,1}` is Ques too, regardless of spelling.
   Lazy and possessive variants retain their own behavior. Recursively empty bodies
   can be omitted only without observable captures; zero-count captures retain their
   numbering and nonparticipation through a dead branch. Empty intersections preserve
   the other operand.
6. OpenJDK 21 `TreeInfo` validation (Ques, Branch, Curly/GroupCurly, Loop, character
   greedy loops, integer overflow, determinism and maxValid) runs before D6. A repeated
   nondeterministic group is invalid even with `{0}` or `{1}`; backreferences inside
   lookbehind remain syntax errors. Only fixed-length, BMP scalar lookbehind is
   accepted; supplementary-capable sets or sets containing surrogate code units are D6.
   Positive lookbehind may capture; capturing groups anywhere in negative lookaround
   are D6. User code can use `(?:…)`. Undefined numbered references compile as never-match
   (including `(?i)`), but still invalidate a lookbehind maximum just as in Java.
7. Named characters `\N{name}` resolve through the committed JDK table, then use the
   ordinary literal/class and case-fold paths. Surrogate-valued names follow the lone
   surrogate policy. All D6 candidates are recorded while parsing and reported only
   after complete Java syntax validation, so a later unclosed group remains syntax.
8. Parser nesting and character-set tree depth are bounded at 64 before recursive
   emission and drop. Backend `RecursionExceeded` is also the authorized
   `PatternSyntaxException: Stack overflow during pattern compilation`; simple nested
   captures last compile at 62 and first fail at 63 because emitted wrappers count too.
   Subprocess tests cover 100,000 groups, classes and a wide optional-branch sequence;
   successfully parsed trees also have bounded emitter depth.

### 3.2 D6 rejection list (config-load errors)

Java-valid constructs rejected by the chosen backend are listed below. The actual
`<construct>` always uses the matched source spelling, including case, spacing and
`\p` versus `\P`, wherever relevant. Quoted literals are not scanned as constructs.

| Construct family | Exact reason |
|---|---|
| `\X` | extended grapheme cluster matching is not supported by the Rust regex backend |
| Existing-group backreferences under CASE_INSENSITIVE, e.g. `\1` or `\k<word>` | case-insensitive backreferences fold differently in the Rust regex backend |
| Grapheme boundary `\b{g}` | grapheme cluster boundaries are not supported by the Rust regex backend |
| Lone surrogate escapes, e.g. `\uD800` or `\x{d800}` | a lone surrogate cannot occur in Rust strings |
| Variable-length lookbehind, e.g. `(?<=(a{1,3}))` | variable-length lookbehind has different matching semantics in the Rust regex backend |
| Lookbehind capable of matching supplementary code points or surrogate code units, e.g. `(?<=..)` | lookbehind over supplementary code points or surrogate code units has different matching semantics in the Rust regex backend |
| Capturing groups in negative lookahead/behind, e.g. `(?!(a)$)` | captures in negative lookaround have different lifetimes in the Rust regex backend; use noncapturing groups |

Malformed `\N`, unknown character names, `\b{x}`, `\B{g}`, repeated flag hyphens,
and ordinary invalid grammar are **PatternSyntaxException**, not D6. `\b{2}` is a
valid repeated boundary assertion. Paired `\uD800\uDC00` escapes are one scalar and
stay accepted. Unicode identifier properties, `(?d)` and `(?u)` are implemented.

### 3.3 Rejection message format

D6 demands "a clear message naming the construct". Exact format (field path
uses the Marathonfile key path convention from WP 2.1):

```text
ConfigurationException: unsupported Java regex construct '<construct>' in '<field-path>': <reason>. Pattern was: '<pattern>'. See D6 (PLAN.md §3.11).
```

Examples:

```text
ConfigurationException: unsupported Java regex construct '(?<=a+)' in 'filtering.method.regex': variable-length lookbehind has different matching semantics in the Rust regex backend. Pattern was: '(?<=a+)b'. See D6 (PLAN.md §3.11).
ConfigurationException: unsupported Java regex construct '\X' in 'vendor.ios.testParser.testClassRegexes[0]': extended grapheme cluster matching is not supported by the Rust regex backend. Pattern was: '\X+'. See D6 (PLAN.md §3.11).
```

`<construct>` is the matched source spelling (e.g. `(?u:`, `\X`, `\p{…}` if
a future gap), `<reason>` one line. The message goes through the normal
`ConfigurationException` wrapper so stage attribution (§3.2 "staged
validation") is unchanged. Compile errors that Java also rejects keep
Java's error class (`PatternSyntaxException: <java message>`) with Rust
wording as the accepted §3.10 difference.

### 3.4 Runtime mapping (D6 extension approved 2026-10-23)

`matches`, `find`, `find_all`, `split` and `replace` return `Result<…, RunError>`.
Every fancy-regex runtime resource error, including backtrack-limit and VM-stack
exhaustion, maps to the distinct `RunError::Aborted`. It is never converted to false,
null or an empty result. WP 1.4 callers must fail the run with an error naming the
original pattern and config field.

The approved D6 extension is: **a regex that exhausts the engine's budget at run time
fails the run with an error naming the pattern and field**. Returning false is unsafe
for exclusion rules: `excludeSerialRegexes.none { matches(...) }` would then include
the device. An inclusion rule would instead exclude it; neither is a valid fallback.

The spike uses a 100,000,000-backtrack budget plus fancy-regex's bounded VM stack.
The A11 benign exponential case completes in under 1 ms for full matching after a
vacuous leading assertion is removed; capture-preserving find uses the larger budget.
The S12 negative-lookahead case is delegated and completes in under 1 ms. Larger or structurally different
patterns can still exhaust the budget. See `regex-spike-results.md` for measurements,
remaining aborts and the exact corpus accounting. A step cap is not a wall-clock
deadline: delegated scans may perform input-dependent work per VM step. WP 1.4 must
measure worst cases at realistic sizes (serials, test/class names, e.g. 1 KiB); a wall-time
guarantee, if required later, needs a separate execution contract. No deadline is added here.

## 4. Corpus generator plan (JVM side, WP 0.3c)

A small Kotlin program in `tools/oracle` (new source set, never shipped)
emits one JSONL file; each line is self-contained so the Rust differential
test needs no JVM:

```json
{"id": "flags-i-003", "pattern": "(?i)Straße", "op": "matches",
 "inputs": ["STRASSE", "straße", "Straße"],
 "expected": [true, true, true],
 "find_expected": null, "groups": null, "compile_error": null}
```

Record shape: `id`, `pattern`, `op` (`matches` | `find` | `findAll` |
`replace` | `split`), `inputs`, per-op expected outputs, `groups` for
find-cases (list of group lists, `null` = no match), `compile_error` =
`PatternSyntaxException` message when compilation fails (wording compared
loosely: class + first line only).

### 4.1 Pattern × input dimensions (each a named family with an id prefix)

1. `ascii-classes`: `\d \s \w \b \D \S \W \B` × ASCII + non-ASCII inputs
   (Cyrillic, Arabic-Indic digits `\u0661`, NBSP, `ſ U+017F`, Kelvin `K U+212A`).
2. `unicode-classes`: `\p{L} \p{Nd} \p{Graph} \p{InGreek} \p{IsLatin}
   \p{javaLowerCase} \p{javaWhitespace} \p{Alpha} [[:alpha:]]` × mixed inputs.
3. `flags`: each of `i d m s u x U` global, `-`negated, and scoped
   (`(?i:x)`, `(?-i:x)`), plus interactions (`(?iu)`, `(?U)(?i)`); expected
   outputs pin the ASCII-vs-Unicode folding split (§2.2).
4. `anchors`: `^ $ \A \z \Z \G \b` × inputs with/without trailing
   `\n \r \r\n \x85 \u2028 \u2029`, empty input, multiline on/off.
5. `dot`: `.` and `(?s:.)` × every terminator above.
6. `quoting`: `\Q..\E`, unterminated `\Q`, `\E` outside quote, metachars.
7. `advanced`: possessive, atomic, backrefs (incl. unmatched-group and
   `\k<name>`), lookarounds (incl. the U4 default `^((?!Abstract).)*Test[s]*$`
   × class names), `&&` classes, `{n,m}` bounds.
8. `hardcoded`: every pattern in §1.2 verbatim × its fixture lines (xcodebuild
   log lines from `fixtures/`, logcat `-v long` headers, rsync stdout,
   `nm` symbol lines, annotation strings).
9. `user-realistic`: serial regexes, package/class/method filters, annotation
   name/value patterns from production Marathonfiles (sanitised).
10. `adversarial`: `(a+)+$`, `(a|a?)+$`, `(\w+)*!` × growing inputs; JVM side
    runs each under a watchdog thread and records `timed_out_ms` instead of
    hanging (documents the Java column of §2.3 without blocking generation).
11. `invalid`: 20+ malformed patterns (unclosed class/group, bad escape,
    bad repetition, stray `\E`? — valid in Java —, undefined group name)
    with the exact `PatternSyntaxException` text for the wording-difference
    baseline.

### 4.2 Generation and consumption

- Generator runs on the reference JDK (Android Studio 21.0.10); the emitted
  JSONL is committed under `fixtures/regex-corpus.jsonl` with a
  `PROVENANCE.md` entry (JDK version, generator source, date).
- Rust test (WP 1.4): for each line, compile via `JavaRegex` (expecting
  either success or the §3.2/§3.3 rejection / Java-error mirror), run `op`
  over `inputs`, compare exactly (booleans, group strings; `compile_error`
  loosely). Any mismatch fails the test with the case `id`.
- Fancy-vs-PCRE2 comparison (the spike's actual decision): a throwaway
  harness runs the same JSONL through the raw translator+fancy and through
  `pcre2` (+`java_regex` if trialled) and reports per-family pass rates —
  that report is the spike output that fixes the supported subset and D6
  list. Success criterion: 100% on families 1–9 + 11 for the chosen backend;
  family 10 must stay bounded and report any exhaustion distinctly, never silently convert it to a negative match.

## Sources

- Kotlin use sites: `git grep` over `configuration/src/main`,
  `core/src/main`, `vendor` at `e47352f2` (full hit list in §1; per-file
  lines cited inline).
- Java study/node selection: [OpenJDK 21.0.10 Pattern.java](https://github.com/openjdk/jdk21u/blob/jdk-21.0.10-ga/src/java.base/share/classes/java/util/regex/Pattern.java),
  especially `group0`, `TreeInfo` and `Node.study` for Ques/Branch/Curly/GroupCurly/Loop.
- `java.util.regex.Pattern` behaviour: OpenJDK 21 `Pattern` javadoc
  (constructs, flags, line terminators, `$`-before-final-terminator) +
  the `QUIRKS.md`/`SPEC.md` of `MathieuDutSik/java_regex` for
  underspecified behaviour (multiline `^` at end of input, capture leaks).
- `fancy-regex` 0.19.2: [crate docs](https://docs.rs/fancy-regex/latest/fancy_regex/)
  (Syntax section: `\G \Z \R \O \N`, atomic groups, possessive modifier,
  `oniguruma_mode`, variable lookbehinds, `RegexBuilder::backtrack_limit`,
  `BacktrackLimitExceeded`); `regex-syntax` 0.8 grammar for the remainder
  (no `\Q..\E`, no `\h\v\X`, `u`-flag = Unicode mode).
- PCRE2: `pcre2api`/`pcre2compat` man pages (UCP-gated `\d\s\w`/caseless,
  `(*UCP)` verb, BSR/newline options, `match_limit`); `pcre2` crate 0.2.11
  via crates.io.
- `java_regex` 0.1.0: crate README + SPEC/QUIRKS/DIFFERENCES docs
  (byte-for-byte claim, UTF-16-offset caveat, bench numbers); crates.io
  for version.
- PLAN.md §§3.10, 3.11 (D6), 4.2, 4.3, 6.3 as cited above.


