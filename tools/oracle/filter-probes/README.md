# WP 3.3 filter probes

Run against the local reference distribution, never the read-only source checkout:

```sh
tools/oracle/build.sh reference
tools/oracle/filter-probes/run
# Optional independent regeneration; compare all five tables byte-for-byte.
tools/oracle/filter-probes/run /tmp/mrs-3.3-filter-repeat
```

`FilterProbes.kt` compiles with cached Kotlin 1.9.24 against the installed reference
JARs at `e47352f2`; Android Studio JBR 21.0.10 is the default. The runner and its
Allure child processes use absolute classpaths. Children remove both plan variables,
then set only the recorded environment, and use a temporary working directory.
The reference checkout is never built or modified in place. No worker is contacted.

All expected results, exception chains and logger records come from the JVM.
Temporary directories become `<tmp>`; the DecodedNull singleton's actual identity
suffix becomes `<identity>`. Frames and timestamps are omitted. Input annotation
labels and output annotation labels distinguish equal tests with different metadata.
Composition records every child call and its complete input sequence, even after an
intersection is empty. `files` records missing-file cache initialization, mutation,
retry and the exact ERROR message. `allure` records actual environment selection,
Jackson version dispatch and malformed-input exceptions/warnings. Its invalid NUL
path is invoked through the actual private `tryGetPath` because an OS environment
cannot carry NUL. The private DEX-to-MetaProperty converter is invoked reflectively
for the producer rows; selection still uses the real Kotlin filter and wrappers.

| Table | Rows | Scope |
|---|---:|---|
| `leaves.jsonl` | 43 | Seven single-value leaves, regex/value branch precedence, both operations, disabled, fragmentation including zero/negative counts and empty input; D6 lookaround witness |
| `composition.jsonl` | 13 | All operations with 0–3 children, duplicate metadata representatives, input/call order, nested factory construction |
| `annotation.jsonl` | 34 | All DEX wrapper families, first-delimiter truncation, null/nested/empty arrays, scalar/list/map rendering; 14 actual producer-to-filter rows and an eager-render trace |
| `files.jsonl` | 13 | ASCII comment whitespace, BOM, CR/LF/CRLF, malformed UTF-8, Unicode terminators, cache/mutation/retry/disabled/explicit values |
| `allure.jsonl` | 41 | Environment precedence, relative paths, absent/directory/invalid paths, ordinary and exotic JSON, cross-field matches, empty plans, wrapper C7, first annotation, lazy supplier counts/retry and empty compositions |

The Rust tests replay the ordinary cases and explicitly assert approved D6/P2
outcomes. P2 is scoped in `docs/wp/3.3.md`: missing/null test collections, null entries,
scalar ID/selector/version coercion and root null become WARN plus absence. Malformed
JSON also warns and becomes absent as Kotlin does; the Rust diagnostic uses
`java.io.IOException` rather than Jackson-specific parser classes/messages. Unknown
version strings (including `"1"`) and absent version remain supported Unknown plans.

Pinned dependency SHA-256:

- allure-test-filter 2.29.0: `a18abb955670fa06ccfd3f1e6cf203e4ced83bef575272164fc328876b9aa275`
- dex-test-parser 2.3.4: `55de1bf8663fe38707473e66952fb8aca20cfa3f44ffd1a7d434141d98262d91`
- Gson 2.11.0: `57928d6e5a6edeb2abd3770a8f95ba44dce45f3b23b7a9dc2b309c581552a78b`
