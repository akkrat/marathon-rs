# Differential harness

See [the contract](../../docs/design/differential.md) for capture, comparison,
provenance and normalization rules. `./run check` validates all pinned JVM
records and runs the registered native contracts. `./run native-vendors
--output runs/new-native` runs the 15 bounded whole-vendor cases without a JDK.

`./run smoke --profile macos-arm64 --output runs/new-pairs` runs two fresh JVM
captures and one native capture of every registered adapter. `linux-x86_64`
selects that host; `harness-first` explicitly reduces selection to actual-local
transport and injected parse. `./run all` also executes the offline/native gate.
Full host profiles reject a host mismatch, missing adapter, tool or recording.

`./run capture ios-smoke --output fixtures/differential/vendors/ios-smoke/jvm-v3`
is an explicit, immutable new JVM generation. Adapter, contract and selected
generation paths live in `registry.json`; capture never changes that selection.
Refresh only intentionally with `python3 -I tools/differential/make_registry.py`.
The gate uses `--check` and never rewrites fixtures or the ledger.
