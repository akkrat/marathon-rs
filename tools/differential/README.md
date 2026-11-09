# Differential harness

The [Phase 8 contract and workflow](../../docs/design/differential.md) describe
the native goldens, normalization and provenance. JVM captures are historical;
do not generate new JVM records.

`./run check` validates historical evidence and checks native cases against
`fixtures/contract/native-v1`. `./run native-vendors --output runs/new-native`
runs the 15 bounded vendor cases. `./run all --profile PROFILE --output DIR`
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
