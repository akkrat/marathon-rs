# config-dump output format

`config-dump` prints what one Marathonfile does to the configuration stages of a run (PLAN.md §3.2, §6.3). The JVM tool
(`tools/oracle/config-dump`, WP 0.2a) defines the format; the hidden Rust `marathon config-dump` must print the same document
for the same input.

```sh
tools/oracle/config-dump <Marathonfile> [--env-file F] [--now <ISO instant>]
```

## What runs

1. **Stage (a), parse**, as `ApplicationView.execute` runs it:
   - the `isFile` check (`ConfigurationException: No config <absolute path> present`);
   - `ConfigurationFactory(marathonfileDir = <Marathonfile>.canonicalFile.parentFile, analyticsTracking = true,
     bugsnagReporting = true).parse(<Marathonfile>)`: environment substitution, Jackson binding, the Android SDK lookup, the
     Apple path resolution and `bundle.validate()` (which extracts `.ipa`/`.zip` archives), `xcresult.pull`.
2. **Stage (b), logical**: `LogicalConfigurationValidator().validate(config)`, the first thing `Marathon.runAsync` does, with
   the vendor `validate()` calls it makes.

Stage (c), provider time, is not run.

## Options

- `--env-file F`: the process environment is exactly the `KEY=VALUE` lines of `F` (blank lines and `#` lines are skipped; the
  value is everything after the first `=`, no quoting). The JVM tool re-executes itself with that environment, so
  `${VAR}` substitution and the `ANDROID_HOME`/`ANDROID_SDK_ROOT` lookup see nothing else. Without the option the caller's
  environment is used.
- `--now <instant>`: the reference time of the relative `timeLimit` values (`InstantTimeProvider`). Without it the real clock
  is used, as in the CLI. The JVM tool passes `ConfigurationFactory` a mapper built like its default one but with a fixed
  time provider, and checks on each run that the two mappers have the same modules and features.

## Document

One JSON object, pretty-printed with two-space indentation and a final newline:

```json
{
  "stage": "parse" | "logical" | "ok",
  "error": null | {"class": "<exception class>", "message": "<message or null>", "causes": ["<class>: <message>", ...]},
  "config": null | <canonical configuration>,
  "fsEffects": [<effect>, ...]
}
```

- `stage` is the stage that rejected the Marathonfile, or `ok`.
- `error` is the exception that stage threw, with its cause chain, outermost first. Messages are Kotlin's; the Rust side may
  word them differently where PLAN.md §3.10 allows it, but the class and stage must match.
- `config` is the canonical configuration when stage (a) succeeded, **also when stage (b) rejected it**, and `null` otherwise.
- `fsEffects` lists what changed under the Marathonfile's directory between before stage (a) and after stage (b):

  ```json
  {"op": "created" | "deleted" | "modified", "path": "<relative path, / separated>", "kind": "file" | "dir" | "symlink",
   "size": <bytes> | null, "sha256": "<hex>" | null}
  ```

  sorted by `path`; `size`/`sha256` describe the file after the change (before it, for `deleted`); for a symlink `sha256`
  holds its target. Symlinks are not followed. A directory that is deleted and recreated with the same content (the bundle
  extraction does this) is no effect.

## Canonical configuration

A deterministic rendering of the parsed `Configuration` object graph, built from the JVM fields of each object rather than
from Jackson's view of it, so that derived values and fields hidden by `@JsonIgnore` appear:

| Value | Rendering |
|---|---|
| object | JSON object of its fields (the class's and its superclasses'; a subclass field shadows a superclass field of the same name), keys sorted as described under "Key order". Static fields, synthetic fields and lambdas (`AppleTestBundleConfiguration.tempDirFor`) are left out. The key is the Kotlin property name (`devicesFile`, not the YAML key `devices`) |
| polymorphic object | as above plus `"type": "<name>"`, the first name under which a supertype's `@JsonSubTypes` lists the class (`iOS` for the iOS configuration, whichever alias the file used). Kotlin `object`s render as `{"type": ...}` |
| `lateinit`/nullable field without a value | `null` |
| `by lazy` property | under the property name: the value when initialised, otherwise `{"lazy": "uninitialized"}` |
| String, Boolean | as is |
| Int, Long, Short, Byte, BigInteger | JSON integer |
| Double, Float | JSON number spelled by `Double.toString`/`Float.toString` (`80.0`, `1.0E-5`); NaN and infinities as strings |
| enum | its `name` |
| `Duration`, `Instant` | ISO-8601 `toString()` (`PT30S`, `1970-01-12T12:46:40Z`) |
| `File`, `Path` | `absolutePath`: absolute, but not normalised (`/dir/./marathon` stays as is; marathon itself canonicalises where it does) |
| `Regex`, `Pattern` | the pattern string |
| Map | JSON object with `toString()` keys, sorted as described under "Key order" |
| Collection, array | JSON array in iteration order |
| `ByteArray` | base64 |

Derived values appear as the fields hold them: the Apple timeouts (`xcresulttool = max(shell, 120 s)`, `bootWait = max(boot,
180 s)`), `xcresult.pullingPolicy` after `pull`, the resolved `devicesFile` (`<dir>/Marathondevices` by default), the
canonicalised SSH key and known-hosts paths, the Android SDK taken from the environment, `analyticsTracking`/
`bugsnagReporting` after merging with the CLI flags, and the bundle's `app`, `testApp` and `xctest` found or extracted by
`validate()`.

## Key order

Object and map keys are sorted by Java's `String.compareTo`, which compares **UTF-16 code units**, not code points (the JVM
tool sorts with a `TreeMap<String, …>`). The two orders differ only when a key holds a character outside the Basic
Multilingual Plane: its surrogates (U+D800–U+DFFF) sort before U+E000–U+FFFF, so `😀` (U+1F600) comes before `ｚ` (U+FF5A).
The Rust side must sort by UTF-16 code units too, e.g. by comparing `str::encode_utf16()` sequences.
`fixtures/oracle/config-dump/oracle_map_keys.json` pins this.

## Goldens

`fixtures/oracle/config-dump/` holds the dumps of every `fixtures/configuration/**/sample_*.yaml` and of the oracle's own
inputs in `fixtures/oracle/config-dump-inputs/`; `tools/oracle/config-dump-goldens.sh` regenerates them (see
`fixtures/oracle/README.md` for the environment and the fixed "now").

A golden is the config-dump document plus one extra top-level key, `golden`, which is metadata and not part of the
config-dump output:

```json
"golden": {"input": "<Marathonfile, relative to the fixture directory>", "env": {...}, "now": "<instant>",
           "host": {"os": "<uname -s>", "release": "<uname -r>", "arch": "<uname -m>"},
           "normalisations": ["<rule applied>", ...]}
```

### Path normalisation

Absolute paths in a dump depend on where it ran, so goldens and the outputs compared with them are normalised, in this
order, in every string of the document:

1. **The sandbox.** The absolute path of the directory the Marathonfile was copied into, in both its given and its canonical
   spelling, becomes `<FIXTURE_CONFIG>`.
2. **Host aliases.** Canonicalisation resolves host-specific links outside the sandbox. On macOS, `/home` is an automounter
   link into the data volume, so `File.canonicalFile` turns `/home/testuser/.ssh/id_rsa` into
   `/System/Volumes/Data/home/testuser/.ssh/id_rsa`. The prefix `/System/Volumes/Data/` becomes `/`. Linux needs no rule.

A comparison applies the same rules to the Rust output before comparing; `golden.normalisations` lists the rules that
changed the golden. Paths that canonicalisation resolves through other host links (for example a symlinked home directory)
are not normalised: such inputs are not used in goldens.
