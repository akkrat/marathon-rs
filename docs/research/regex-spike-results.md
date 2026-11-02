> **Superseded (2026-10-23).** The user decided not to emulate Java regex semantics. User-supplied regexes now use the Rust `regex` crate (PLAN.md D6). This spike, and the unmerged WP 1.4 branch `wp/1.4-java-regex` built on it, are kept only as research: the measurements and the `java_regex`/`fancy-regex` bug reports (docs/found-bugs.md U17–U20) remain valid.

# Regex spike results (WP 0.3c)

**Decision: backend (b), our Java translator over `fancy-regex =0.19.2`, provisionally retained.**

Round 2 implements the binding review decisions in `docs/wp/0.3c-findings.md`.
User approval on 2026-10-23 extends D6: runtime budget exhaustion fails the run with
an error naming the pattern and config field. E1 is the approved scalar-boundary
exception across every operation. Neither approval implies a wall-clock deadline.

## Corpus and scores

The original 408-row and round-1 598-row fixtures are unchanged.
`fixtures/oracle/regex-round2-corpus.jsonl` contains **1,097 rows**: those 598,
491 grouped review/regression cases and eight separately scored runtime scan cases.
The input resource is `tools/oracle/module/src/main/resources/regex-round2.jsonl`;
large timing inputs are constructed by `RegexCorpus.kt`. Every one of the review
appendix's **838 divergent operations** (567 new, 210 extra, 61 final) is included.
A coverage check found no missing witnesses and no discrepancy between regenerated
Kotlin expectations and the review's independent JDK results. New neighboring rows
cover all quantifier modes, Java group study at zero counts, negative captures,
consuming supplementary lookbehind, named-character folding, syntax following D6,
empty nested groups, E1 across wrappers/operations, and depths 61/62/63/64/65.
All expectations come from the **0.2a Kotlin Regex oracle on JDK 21.0.10**, never from
the implementer or reviewer output.

macOS round-2 scores (Linux remains the orchestrator's pre-merge check):

| Family | java_regex | fancy-regex | pcre2 |
|---|---|---|---|
| ascii-classes | 13/13 | 13/13 | 13/13 |
| unicode-classes | 27/30 +3 fail | 29/30 +1 D6 | 29/30 +1 D6 |
| flags | 175/178 +3 fail | 178/178 | 178/178 |
| flag-scope | 18/18 | 18/18 | 18/18 |
| anchors | 30/30 | 30/30 | 30/30 |
| dot | 9/9 | 9/9 | 9/9 |
| quoting | 10/10 | 10/10 | 10/10 |
| advanced | 34/41 +4 fail +3 E1 | 33/41 +3 E1 +5 D6 | 33/41 +3 E1 +5 D6 |
| hardcoded | 24/24 | 24/24 | 24/24 |
| split | 4/4 | 4/4 | 4/4 |
| user-realistic | 9/9 | 9/9 | 9/9 |
| adversarial | 11/12 +1 proven | 11/12 +1 proven | 5/12 +7 limit |
| invalid | 28/30 +2 fail | 28/30 +2 D6 | 28/30 +2 D6 |
| round1 | 123/190 +67 fail | 165/190 +2 E1 +23 D6 | 151/190 +2 limit +12 fail +2 E1 +23 D6 |
| round2 | 376/491 +92 fail +20 E1 +3 compile-limit | 278/491 +13 E1 +5 compile-limit +195 D6 | 278/491 +2 limit +2 fail +11 E1 +3 compile-limit +195 D6 |
| runtime-scans | 2/8 +6 fail | 2/8 +2 proven (4 hang) | 2/8 +6 proven |
| **all** | **893/1097 +177 fail +1 proven +23 E1 +3 compile-limit** | **841/1097 +3 proven +18 E1 +5 compile-limit +226 D6 (4 hang)** | **821/1097 +11 limit +14 fail +6 proven +16 E1 +3 compile-limit +226 D6** |

Counts are disjoint. `fail` is an actual difference. `limit` means a backend Aborted
where Java answered. `E1` means an exact match with the independently JDK-generated
`scalar_expected`, differing from ordinary JDK behavior; those rows are **not passes**.
The oracle driver advances by scalars, skips interior surrogate boundaries and pins
saved `\G` state for split/replace. The comparator preserves isolated surrogate
strings as tagged UTF-16 units without editing oracle bytes. A supplementary input
alone can never excuse a difference; tests also forbid E1 for full matches.

`compile-limit` separately records the authorized safety bound when Java compiles a
deeper pattern. It is a `PatternSyntaxException`, not D6 or a syntax-agreement pass.
The chosen backend last accepts 62 nested capturing groups and first rejects 63;
its emitted wrappers use part of fancy's depth budget. Parser/set depth also stays
bounded at 64. The three deeper candidate depth limits remain visible too.

`proven` records a mathematical answer after the JDK character-read watchdog supplied
no result: `(.*a){12}` cannot fully match inputs ending in `b`/`!`; neither scan probe
can find its required literal `b` in all-`a` input. These are not measured JDK parity.
The chosen backend has **841 matching normalized outcomes, 3 proved answers, 18 E1
rows, 5 authorized compile limits, 226 D6 rejections, and 4 watchdog terminations**.
There is no unexplained semantic disagreement or Java-answer/Rust-Aborted case.
The four terminations are precisely R2-sol-11's huge delegated scans; see timings below.
A separate re-run of all 10,829 broad review probes found 5,810 identical outcomes,
1,695 equivalent syntax errors, 3,312 D6 rejections and 12 differences each verified
against the corpus's scalar JDK expectation. No other difference remained.

Full comparison exits **1** because of candidate failures/limits and the four chosen
runtime-scan watchdog terminations. A chosen-backend run excluding `runtime-scans`
exits **0**; running all its rows exits **1**, intentionally exposing R2-sol-11.
Every row is isolated with a 10 s process watchdog; this is a harness safeguard,
not part of JavaRegex. `java_regex` still aborts its `round1-189` worker with native
stack overflow; the parent records the failure and continues.
Full row-level output is committed in
[`spikes/java-regex/results/round2.txt`](../../spikes/java-regex/results/round2.txt).

Reproduce from the repository root:

```sh
tools/oracle/regex-corpus fixtures/oracle/regex-round2-corpus.jsonl
cargo run --release --manifest-path spikes/java-regex/Cargo.toml -- fixtures/oracle/regex-round2-corpus.jsonl --verbose
cargo run --release --manifest-path spikes/java-regex/Cargo.toml -- fixtures/oracle/regex-round2-corpus.jsonl --backend fancy-regex --skip-family runtime-scans
cargo test --release --manifest-path spikes/java-regex/Cargo.toml --all-targets
```

## Why not (a) `java_regex` 0.1.0

The enlarged corpus finds **177 differing rows**, including malformed-pattern acceptance,
missing properties and aliases, host Unicode skew, COMMENTS grammar, class intersections
and matcher continuation in split/replace. The full results identify each row. Some
continuation failures belong to this candidate adapter, which only exposes fresh matcher
searches; they remain losses rather than silently inheriting the chosen engine's fix.

More decisively, its fixed 5,000,000-step / depth-500 budget returns plain false on
exhaustion: `(a|b)*` on 600 chars and `(?:a+b)+` on 1,200 chars wrongly return false
where Java returns true (`examples/limits.rs`). Its public API cannot distinguish the
error, so callers cannot handle it correctly. Named-group lookup for replacement is
also unavailable to the candidate adapter.

Original local audit at round 1: about 5,400 source lines (`lib` 1924, `engine` 1299,
`parser` 923, `gen` 599, `types` 256, `unicode` 420), zero `unsafe`, `no_std`, licence
MIT OR Apache-2.0, data-driven `jsonl_tests` and self-consistency `proptest_invariants`.
Its dependency tables target a newer Unicode generation, consistent with its OpenJDK 25
claim; U+1C89, U+1C8A and the U+A7CB/U+0264 fold demonstrate skew from JDK 21. The
original maintenance snapshot was one author, created 2026-02-21, last push 2026-09-05,
2 stars and no forks. Mutated-corpus fuzzing in round 1 (`examples/fuzz.rs`, 15 s per
backend) saw no panic; the later 100,000-group subprocess exposed the translator's
missing depth guard, fixed in this rework.

## Why not (c) `pcre2` 0.2.11

The shared translator now rejects all variable-length lookbehind consistently under D6,
so this is no longer an extra PCRE2 compile loss. PCRE2 still has **14 differing rows**
and **11 budget-limited rows**. Twelve divergences remain in round-1 continuation
cases, with two more in round 2. E1 differences have their own counts (16), and do not
inflate passes or excuse continuation errors. The limits include seven original
adversarial cases plus two round-1 and two round-2 cases.
PCRE2 limits are 1,000,000 match steps / 100,000 depth, with JIT off.

`pcre2-sys 0.2.10` vendors PCRE2 10.46 and `.cargo/config.toml` sets
`PCRE2_SYS_STATIC=1`; it needs a C compiler but no system PCRE2. The original spike
built and reproduced its then-current scores on the Linux box (gcc 15.2.0, no
pkg-config). This round was measured locally only; the chosen engine remains pure Rust.

## Unicode and grammar parity

`tools/oracle/regex-unicode/generate.sh` derives the committed compact gzip table from
the JDK itself. It includes general categories, binary/POSIX properties, all scripts
and ISO aliases, blocks and the exact accepted name map, every Java identifier class,
character names for literal resolution and syntax validation, and simple upper/lower mappings. The generator
requires JDK 21. Regeneration is byte-identical. The build expands the data, and the
translator emits explicit scalar ranges; no host property or case-fold table is used.
Surrogate-containing properties such as `InHighSurrogates` remain valid and match no
scalar input. Lone surrogate *escapes* are D6; paired `\u` escapes remain accepted.

COMMENTS grammar includes flag tokens, group names, quantifiers/modifiers, class ranges,
intersections and numeric escapes. In `(?x)[ ^a]`, the whitespace before `^` prevents
negation, matching the JDK's raw initial-negation check. `[a&&]` and `[&&a]` preserve
`a`. A repeated flag hyphen is a syntax error. Bare/unknown `\N`, `\b{x}` and
`\B{g}` are syntax errors; `\b{2}` is valid. `(?i)\1` without a group compiles
as never-match. D6 spans retain the original source through quoting and COMMENTS.

Positive fixed-length lookbehind with BMP scalar content and captures is supported,
including outside backreferences. Java's `TreeInfo.maxValid`/determinism validation
runs first, so `(?<=(?:a|b){2})b` and `(?<=(a?){0})b` are syntax errors even though
mathematical lengths alone suggest a bound. Ques/Branch and Curly/GroupCurly/Loop
selection, greedy character-loop overflow, zero counts and possessive groups matter.
A reference *inside* lookbehind remains invalid even when the numbered group is absent.
Every Java-valid variable-length lookbehind is D6. Captures inside negative lookahead
or lookbehind are D6 because Java can leak them from failed positions; noncapturing
groups remain available. `a.(?<=..)` on `a😀` is a consuming full-match witness for
the supplementary lookbehind restriction, not E1.

BMP ranges containing surrogate code units also require the R3 restriction:
`(?<=[\u0000-\uFFFF]{2})` finds at UTF-16 `[3,3]` in `𐐀b` by looking back from the
low half plus `b`. Scalar-only matching cannot do this. This conservative application
of R3 is documented explicitly; ordinary BMP scalar ranges, including property
intersections restricted to `[a-z]`, remain accepted. The added `round2-476/477`
probes expose the otherwise hidden mismatch. No UTF-16 position model is added.

Atomic-tail dollars and `\Z` preserve branch choice. The negative-dollar optimization
keeps the not-between-CR-and-LF condition while delegating regular expensive prefixes.
Direct `\R?`/`\R{0,1}` keep Java Ques behavior; grouped optional `(?:\R)?` may backtrack
from CRLF to CR; counted `(?:\R){1}` cannot. Lazy and possessive neighbors have oracle
rows. Recursively empty groups repeat successfully. Zero-count captures retain group
numbering and null/nonparticipating values rather than disappearing in the backend.

## E1 and the frozen use-site re-audit

Approved exception **E1**: **JavaRegex never places a match boundary inside a surrogate
pair. Java can, after an empty match advances by one UTF-16 unit. This applies to every
operation: find, findAll, split, replace and continuation.**

The bare-`\B` midpoint shortcut and `Found.utf16_range` are removed. Bare/wrapped
boundaries now agree with one another and every operation uses scalar positions.
The chosen engine's E1 rows are `advanced-022/025/027`, `round1-052/054`, and
`round2-051/052/062/063/122..129/134`. The counts above record them as E1, never passes.

Re-audit at `e47352f2`: `git grep` across **all Kotlin main sources** for `Regex`,
`toRegex`, `Pattern.compile/matches` and every `.split(`, followed by each declaration
and its callers. All user regex paths are full-match-only:

| Source declarations and consumers | Operation | Empty scan next to supplementary input? |
|---|---|---|
| `Configuration.kt:39-40,165-166` → `Scheduler.kt:133,137` (`includeSerialRegexes`, `excludeSerialRegexes`) | matches | no scan/advancement |
| `FilteringConfiguration.kt` → six name/package/method filters and `AnnotationFilter.kt:14` | matches (including `regex::matches`) | no scan/advancement |
| `AnnotationDataFilter.kt:30,40,42`, `nameRegex`/`valueRegex` | matches on names, strings or toString values | no scan/advancement |
| `TestParserConfiguration.kt:16` → `NmTestParser.kt:108`, `testClassRegexes` | matches | no scan/advancement; default negative lookahead remains supported |

The complete hard-coded pattern audit follows. Paths abbreviate only the common
package prefixes listed in `regex.md` §1.2; line numbers refer to the frozen source.

| Site | Active pattern or required content | Operation | Empty boundary possible? |
|---|---|---|---|
| `StringExtensions.kt:23,26` | `[^a-zA-Z0-9\.\#]` | replace | no: one code point, even if supplementary |
| `FilterValuesFileReader.kt:18` | `\s+#.*$` | replace | no: whitespace plus literal `#` |
| `GraphiteDataSource.kt:26` | `\s` | replace | no: one whitespace character |
| `WriteFile.kt:17,29`, PLAIN_PATH | `/[A-Za-z0-9_./@+-]+` | matches | no scan |
| `RsyncFileBridge.kt:97` | `(?s)\n.*\z` | replace | no: mandatory newline |
| `AppleTestBundle.kt:68` | `[- ]` | replace | no: one character |
| `SessionResultsPathFinder.kt:5`, `DiagnosticLogsPathFinder.kt:5` → `ArtifactFinderListener.kt:14` | `(^\s*&#124;\s+)/.+\.xcresult\s*$`, log counterpart | find | no: mandatory slash, `.+` and extension (the prefix itself may be empty) |
| `TestRunProgressParser.kt:22,28,36,44` → `:49-55,64,77,127,162` | started/finished/failing/timeout patterns | matches then find and read captures | no: required literal test-case text or file/error text, despite some `.*` captures |
| `SimulatorService.kt:41,154`, UDID_REGEX | `[A-Z0-9\-]*` | matches | can match empty, but no scanning/advancement |
| `TestAnnotationParser.kt:57,84,112` | `\p{Graph}+\([^\)]*\)` | findAll | no: Graph+ plus both parentheses |
| `LogcatMessageParser.kt:23,77-104` | DATE_TIME, PID, TID, PRIORITY, TAG composed into HEADER | matcher.matches, then groups | TAG is nullable, but only inside a full matched header; no scan |
| `TestResultsListener.kt:93` | `.+\[\d+]` | matches | no scan |
| `NmTestParser.kt:123-142` | private Regex filter/find/contains helpers | unused | no callers anywhere in the frozen tree; no pattern arguments supplied |
| `Nm.kt:51,59` | remote `grep`/`cut` commands | worker shell commands | not JavaRegex; active symbol scans do not call the unused helpers |
| all other `.split(` sites, including test-name decomposition, paths, versions and annotation parameters | literal strings/chars | Kotlin literal splitting | no regex split sites |

Serializer/equality/stringification uses do not execute patterns. The vendor-test
factory only supplies the same serial settings. This audit replaces the incorrect
claim that `Nm` passes hard-coded symbol regexes to those unused helpers. There is no
active empty regex search next to supplementary content, so E1 is unobservable from
marathon's external interface at the frozen revision.

## D6 messages and witnesses

WP 1.4 wraps each `Rejected { construct, reason }` with this exact config message:

```text
ConfigurationException: unsupported Java regex construct '<construct>' in '<field-path>': <reason>. Pattern was: '<pattern>'. See D6 (PLAN.md §3.11).
```

The table gives actual spellings from the corpus, not diagnostic placeholders.

| Matched source spelling (examples) | Exact reason | Corpus witnesses |
|---|---|---|
| `\X` | extended grapheme cluster matching is not supported by the Rust regex backend | `unicode-classes-025` |
| `\1` | case-insensitive backreferences fold differently in the Rust regex backend | `advanced-037`; named spellings are also preserved, tested in `round1.rs` |
| `\b{g}` | grapheme cluster boundaries are not supported by the Rust regex backend | `advanced-039`; COMMENTS spelling `\b {g }` in `round1-177` |
| `\uD800`, `\x{d800}`, `\x{D800}`, `\uDC00` | a lone surrogate cannot occur in Rust strings | `advanced-040`, `round1-085`, `round1-144/145/147` |
| `(?<=a+)`, `(?<=(a{1,3}))`, `(?<=a+b*)` | variable-length lookbehind has different matching semantics in the Rust regex backend | `advanced-014/015`, `invalid-015/028`, `round1-003..014/096/125..126` |
| `(?<!(a))`, `(?!(a)$)` | captures in negative lookaround have different lifetimes in the Rust regex backend; use noncapturing groups | `round2-001/002/072/073/091..094` |
| `(?<=..)`, `(?<=\w{2})`, `(?<=[\u0000-\uFFFF]{2})` | lookbehind over supplementary code points or surrogate code units has different matching semantics in the Rust regex backend | consuming `round2-135/136`, `round2-464..471/476/477` |

The 226 chosen-backend rejections are 144 variable lookbehind, 44 negative captures,
27 supplementary/surrogate lookbehind, 7 lone surrogate escapes/names, 2 grapheme
boundary, 1 grapheme cluster and 1 case-insensitive backreference. Valid named-character
escapes are supported, including COMMENTS and case-fold paths; their D6 family is removed.
All D6 is deferred until complete syntax validation, including a malformed suffix.

`regex.md` §3.2–3.3 is reconciled with this list. The identifier properties and `(?d)` /
`(?u)` are supported; `(?c)` is the JDK inline no-op. Missing numbered references are
never-match, while undefined named references remain syntax errors. Invalid constructs
are syntax errors rather than D6 and are compared by the Java exception class.

## Runtime budgets and the approved D6 extension

`JavaRegex` never maps exhaustion to false. **Every fancy-regex runtime resource error**,
including backtrack count and maximum VM stack, is `Err(RunError::Aborted)` through
matches/find/split/replace. The extension **approved 2026-10-23** is: **a user regex
that exhausts the engine's budget at run time fails the run with an error naming
the pattern and config field**. WP 1.4 must carry both into the error.

The old “false is fail-closed” claim was wrong. For
`excludeSerialRegexes.none { matches(...) }`, false **includes** a device; an inclusion
rule would exclude it. Neither direction supplies a safe uniform fallback.

The budget is raised from 1,000,000 to **100,000,000** backtracks. A11,
`(?:(?=)(a?){22}a{22}|a{22})` on 22 `a`s, initially completed in about 135–147 ms
with the raised budget. Removing a vacuous leading, capture-free empty lookahead now
lets full matching delegate in under 1 ms; find still exercises the capture-preserving
VM and the increased budget (255 ms in the release regression test). A10's `(?=)(a)*` on 1,100,000 `a`s also delegates and
returns true in under 1 ms. S12's capture-free equivalent, `(?!(?:a|aa)+$).*` on 40 `a`s plus `!`, returns true in under
1 ms: its exposed tail dollar in a regular negative lookahead is delegated with
consuming final-terminator branches and an explicit CRLF guard. Atomic/assertion-dependent
bodies and captures enclosing the dollar do not use that rewrite. Input-ending and
newline negatives are pinned by the corpus.

Remaining measured aborts: `(?:(a)|b)*(?=)` and the additional VM regression `(a)*(?=)`
on 1,100,000 `a`s exceed the VM stack on full match and return Aborted. The former
also gives JDK StackOverflowError in corpus `round1-188`; the latter is a direct Rust
resource-path test. Find/split can delegate and answer; tests permit that answer and
require any resource error to be Aborted. The original A10 prefix case is now optimized,
but resource-variant unit tests still cover both backtrack and stack errors. No budget can
promise completion for all Java-fast patterns. This change removes the two reported
false positives, while retaining finite limits. No timing-only result is called parity.

**The 100M cap is a step cap, not a wall-clock deadline.** A delegated regular scan can
read input-dependent amounts per VM step without consuming many backtracks. The runner's
10 s process watchdog is absent from JavaRegex; the API has no deadline mechanism.
R2-sol-11 measured release `(?=a*\z)b` find on 100,000 `a`s at **1.16 s**, with
1,000,000 and 10,000,000 still running when killed at **12 s**; the negative form
`(?!(?:a*\z))b` on 1,000,000 also exceeded 12 s without Aborted.

Round-2 implementation measurements (`examples/scans.rs`, release; match time after
compilation, individual large processes killed externally at 12 s):

| Input | Positive lookahead | Negative lookahead |
|---|---:|---:|
| 1,024 `a`s | 0.219 ms, null | 0.165 ms, null |
| 100,000 `a`s | 1.184 s, null | 1.180 s, null |
| 1,000,000 `a`s | >12 s, killed, no Aborted | >12 s, killed, no Aborted |
| 10,000,000 `a`s | >12 s, killed, no Aborted | >10 s, corpus watchdog, no Aborted |

The corpus has an individual row per size and pattern. Its JDK read watchdog aborts
on the three largest sizes. Rust's 100K null results are counted as proofs; the four
larger chosen-engine runs count as **hang**, not pass, proof or Aborted. All three
backends were scored on these rows as well. The review also measured explicit Aborted
at 3.78 s for the 30-character A11 variant, 2.97 s for a trailing-empty-assertion
30-character full match, and 1.90 s for `(a|aa)+(?=b)` on 100 `a`s.

**WP 1.4:** marathon's regex inputs are short serials, test names and class names.
Measure worst cases at realistic lengths such as **1 KiB**, including catastrophic
VM paths as well as delegated scans. The two 1 KiB measurements above are examples,
not an input-independent worst-case guarantee. If a wall-time guarantee is required,
define a cancellable/bounded execution contract separately. Per the binding decision,
this round adds documentation and measurements, **no deadline mechanism**.

## API for WP 1.4

The concrete prototype is `spikes/java-regex/src/backends/fancy.rs::JavaRegex`:

- `compile(pattern) -> Result<JavaRegex, CompileError>`; WP 1.4 adds config-field context.
  Syntax carries Java's `PatternSyntaxException` class, with Rust wording allowed.
- `pattern() -> &str`, `Display` and `Serialize` expose the **original source**, never
  the translated pattern. This preserves `RegexSerializer` and round-tripping.
- `matches -> Result<bool, RunError>`, `find -> Result<Option<Found>, RunError>`,
  `split(input, limit) -> Result<Vec<String>, RunError>`. Shared `find_all` and `replace`
  also return Results. Exhaustion stays distinct from an ordinary negative answer.
- Kotlin findAll uses a fresh matcher for each search. Split and replace retain Matcher
  continuation: after an empty match, the search start advances but `\G` stays at the
  previous end. The chosen backend enables assertion overrides only on find patterns
  containing `\G`, preserving linear delegation for other searches and full matches.
- Parser and set-tree depth are bounded before emission/drop. A subprocess compiles
  100,000 nested groups/classes and a wide optional-branch sequence without aborting.
  Backend recursion exhaustion maps to the same syntax class; 62/63 are boundary-tested.
- All positions are scalar boundaries under E1. No midpoint-only adapter path or UTF-16
  string/position model exists. The oracle comparator keeps E1 separate from agreement.

## Incidental bugs

Third-party findings for `docs/found-bugs.md` (columns: where, behaviour, seen in,
handling, status). None is reported upstream yet.

| Where | Behaviour | Seen in | Our handling | Status |
|---|---|---|---|---|
| `java_regex` 0.1.0, `\p{Space}` / `\p{IsWhite_Space}` | misses vertical tab (`\x0b`), so `matches` is `false` where Java is `true` | WP 0.3c corpus `unicode-classes-016`, `-030` | Not our backend (decision is (b)); listed for the audit | open |
| `java_regex` 0.1.0, `\p{IsAlphabetic}` | rejected as unknown property; Java accepts it as an `Alphabetic` alias | `unicode-classes-009` | Same as above | open |
| `java_regex` 0.1.0, `(?U)(?i)` + `K` | Kelvin-sign fold missing under Unicode case-insensitivity | `flags-013`, `-174`, `-175` | Same as above | open |
| `java_regex` 0.1.0, budget exhaustion | fixed 5M-step / depth-500 budget surfaces as plain "no match" with no signal (`(a|b)*` on 600 chars, `(?:a+b)+` on 1200 chars answer `false`, Java `true`) | `spikes/java-regex/examples/limits.rs` | Decisive against (a): WP 1.4 could not map this to "aborted" | open |
| `java_regex` 0.1.0, `(?c)` | rejected as "Invalid inline flag"; the JDK accepts it as a no-op | `advanced-041` | Same as above | open |
| `java_regex` 0.1.0, `\x{110000}` and `\i` | accepted; Java throws `PatternSyntaxException` | `invalid-024`, `invalid-027` | Same as above | open |
| `java_regex` 0.1.0, Unicode boundaries | `(?U)\b` on standalone U+0301 misses the leading boundary; `(?U)\w` rejects U+200C | Sol S5/S6 extra probes; `round1-051`, `round1-158` | Not chosen; regression rows retained | open |
| `java_regex` 0.1.0, flag clearing | `(?u)(?-U)(?i)k` matches the Kelvin sign, although clearing U also clears Unicode case folding on the JDK | `round1-150` | Not chosen | open |
| `java_regex` 0.1.0, script aliases | Missing `sc=Deva` and `sc=Adlm` (and script spellings such as `IsDeseret`) | `round1-080/081/152/153` | JDK alias table in the chosen translator | open |
| `java_regex` 0.1.0, COMMENTS grammar | Group/flag whitespace, repetition modifiers, range syntax and numeric escapes diverge from Java | S7/A4; `round1-026/027`, `round1-096..100`, `round1-168..185` | Not chosen; generated Kotlin witnesses | open |
| `java_regex` 0.1.0, intersections | `[a&&]` is rejected and `[&&a]` fails to match `a` | `round1-018/019` | Not chosen | open |
| `java_regex` 0.1.0, invalid syntax accepted | Accepts `[\Q\E]`, bare `\N`, repeated flag hyphens and `\p{InBasic-Latin}` | `round1-030/088/138/139/148/151` | Not chosen; preserve syntax-error oracle rows | open |
| Our original regex translator and adapter | Unbounded recursive compilation could abort the process; atom/anchor rewrites, optional `\R`, COMMENTS/intersections, host Unicode and split/replace continuation gave wrong answers | S1–S10, A1–A9 | Bounded parser/set trees, JDK tables and corrected translation/matcher state; subprocess and corpus regressions | patched-locally |
| Our original regex runner and design note | Credited limit-to-false as agreement and described it as fail-closed; maximum VM-stack errors were classified as Other | S11/S12, A10/A11 | Result-based Aborted for every runtime resource error; disjoint scoreboard counts and nonzero failure exit; approved D6 runtime extension (2026-10-23) | patched-locally |
| `java_regex` 0.1.0, repeated capture VM | `(?=)(a)*` on 1,100,000 `a`s aborts the process with native stack overflow, even with the original runner's 64 MiB stack; JDK returns true | `round1-189`, added from A10 | Per-row process isolation records a failed worker; chosen backend removes the vacuous leading assertion and delegates | open |
| Our round-1 regex translator | Negative assertion captures, supplementary lookbehind, group study, CRLF dollar rewrite, direct/counted linebreak and recursively empty repeats diverged | R2-sol-1..6/8/10; 838 independent review divergences | Corrected semantics/Java syntax validation, explicit D6 subset, dead capture branch, recursion mapping and exact boundary tests | patched-locally |
| Our round-1 bare-boundary midpoint adapter and comparator | Bare \B had a special find path while wrappers, replace, split and continuation used scalars; only three exception IDs were scored | R2-sol-9 | Removed midpoint path, adopted approved E1 consistently, JDK-generated scalar comparison and lossless surrogate expectations | patched-locally |
| `fancy-regex` 0.19.2, zero-count captures | `(?:()){0}` delegated captures omit the unparticipating group, returning only group zero | New round-2 zero-count regression, extending R2-sol-2/6 | Emit an impossible capture-preserving branch plus an empty alternative; group numbering and null values remain observable | worked-around |
| `fancy-regex` 0.19.2, delegated scan cost | 100M backtracks gives no wall-time bound: input-dependent delegated scans exceed 12 s without Aborted | R2-sol-11; `examples/scans.rs`; `runtime-scans-003/004/007/008` | Documented; process watchdog only in harness, realistic-size measurements assigned to WP 1.4; no new deadline | open |
