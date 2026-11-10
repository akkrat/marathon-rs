# Differential harness

The [Phase 8 contract and workflow](../../docs/design/differential.md) describe
the native goldens, normalization and provenance. JVM captures are historical;
do not generate new JVM records.

`./run check` validates historical evidence and checks native cases against
`fixtures/contract/native-v1`. `./run native-vendors --output runs/new-native`
runs the 16 bounded vendor cases. `./run all --profile PROFILE --output DIR`
runs the native gate and selected captures. The gate does not write goldens.
The JVM `capture` command is historical and requires `--historical`.

To re-bless intentionally, remove the existing `fixtures/contract/native-v1`
generation and run `./run bless`; review the generated views and provenance
with `./run diff-golden --against HEAD` (or another revision).
For the configuration corpus, use `MARATHON_BLESS_CONFIG=1 cargo test -p
marathon-cli --test config_contract`, then review `fixtures/contract/config/`.
Changes to the contract require a D-row in PLAN.md §3.11. Before running the
harness directly, build its YAML parser helper with `cargo build -q -p
snakeyaml --example yaml_tree`; the gate builds it automatically.

Native YAML comparison canonicalizes `tag:yaml.org,2002:binary` scalars as
lists of decoded bytes. Base64 whitespace, 76-character wrapping and block
chomping do not change the comparison; a changed byte or removal of the binary
tag does. Untagged strings retain their text and string type. Invalid base64 is
rejected. This changes comparison only; writer output and stored goldens stay
unchanged.

Console comparison (WP 8.8): the native contract view of stdout/stderr keeps only
the per-test progress lines and the end-of-run summary block (`contract_console`),
byte-exact. In `vendor_console.py`, a complete native diagnostic prefix
(`HH:mm:ss.SSS LEVEL marathon_<crate>::<module>: `) has only its time token
normalized; level, target, message, fields and continuation lines stay exact,
compact (timeless) lines are untouched, and the rule never applies to raw
progress or summary lines. Historical JVM prefix handling is unchanged.

Native comparison parses the JSON values of line-start `window.<name> = <json>`
assignments inside script elements in `reports/html/**/*.html`, including
`var dataset = <json>` in
`reports/html/timeline/index.html`, using the same ordered-data rules as JSON
files. Numeric spellings and string escapes may differ; changed values, key or
array order, null/omission, and every byte outside the JSON values still differ.
Only top-level assignments outside JavaScript comments, quoted strings and
template literals (including interpolations) qualify; nested code stays text.
Stored golden views and historical JVM comparisons remain unchanged.

`android-instrumentation-eof` is native-only: its new scripted instrumentation
stream starts `MainActivityTest#testText` and ends without a result. Marathon's
own failure reaches JUnit, Allure, per-test JSON and the HTML test page. It is
registered with `native_only: true`, with no historical JVM golden or verification
record; historical capture rejects it. The other 17 adapters retain their JVM
evidence and validation. Its stream lives under `tools/differential/scenarios/`;
existing copied fixtures are unchanged.
