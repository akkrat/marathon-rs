# Legacy xcresult oracle (WP 4.6)

Run from the worktree (the optional first argument selects the read-only Kotlin
checkout; the optional second argument selects the corpus output, and
`models.json` is written next to it; the default is the committed
`fixtures/oracle/xcresult/` pair):

```sh
tools/oracle/xcresult/run.py $MARATHON_KT
```

Needs the reference JBR (defaults to Android Studio's JBR, override `JAVA_HOME`)
and cached Gradle artifacts. Measured JBR: 21.0.10; Kotlin compiler/stdlib/reflect
1.9.24, KotlinPoet 1.16.0, Gson 2.10.1, Jackson core/databind/annotations/Kotlin
2.17.2. No network, Xcode, remote workers or credentials are used.

The script compiles the original `XcresulttoolSourceCreator.kt` and the five
schema-model files from `e47352f2`, and runs them on our byte-identical schema.
One synthetic `OracleNullableLists` object is appended to a disposable schema;
it tests optional String/object arrays, absent from schema 3.39. It is generated
by the same Kotlin generator and is never part of the 57 production models.
The original `AppleJsonTypeAdapterFactory.kt` is compiled unchanged. The exact
`AppleJsonMapper` setup block is extracted from the original wrapper to avoid
building its unrelated command/runtime dependencies. All compilation and model
output go to a temporary directory removed on completion; the reference stays
read-only. Nothing from another worktree is used.

`corpus.jsonl` has a version header and 160 measured cases. Each row records an
ID, target class, literal JSON input and either its projected value or exception
class/cause chain, message and Jackson field path. `Date` projects to epoch
milliseconds. Reflection projects the **actual generated fields**, retaining
constructor names and inherited fields (Jackson getter serialization would
rename `isConcreteDevice` to `concreteDevice`). A separate plain ObjectMapper
writes ASCII escapes so unpaired UTF-16 surrogates survive the evidence file.
Nonfinite floating point results are strings (`NaN`, `Infinity`, `-Infinity`).
All JSON fixture values are projected recursively, not just their failure counts.
`models.json` records every generated class's primary-constructor name, Kotlin
type and `KParameter.isOptional` default flag via Kotlin reflection.

The inputs cover every scalar kind, defaults/missing/null values, optional
scalars, required objects/dates, wrappers and wrong wrapper shapes, numeric
boundaries/coercions, null/malformed/duplicate list entries, nullable-list
precedence, recursive Reference/TypeDefinition, base-type binding, unknown
fields, syntax errors/truncation, the 1000/1001 nesting-depth boundary, top-level null, duplicate object fields and
trailing tokens. The five unchanged JSON fixtures and WP 0.4b's SCRIPTED root/ID
stdout are also measured. Scripted JSON is not a real `.xcresult` bundle.

Approved P1/P2/P3 differences are annotated on affected inputs. P1's malformed
scalar wrappers become structured errors in Rust; at model fields Kotlin already
wraps the null dereference in JsonMappingException. P2 rejects full-width integer
strings and hexadecimal doubles; P3 rejects RFC-1123, date-only strings and
lenient out-of-range calendar dates. Other cases are compared directly by the
Rust test, including all nested projected fixture values.

Ordinary builds and the gate only consume committed evidence, never this runner.
Regeneration is deterministic on the pinned JBR and dependencies.
