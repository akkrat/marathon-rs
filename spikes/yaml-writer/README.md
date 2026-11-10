# WP 8.9 milestone-1 experiment

This package is excluded from the workspace. It borrows the existing projections
to evaluate emitters; it does not replace production serialization.

With the normal Rust toolchain on PATH and MARATHON_KT set:

```sh
cargo run --manifest-path spikes/yaml-writer/Cargo.toml
cargo test --manifest-path spikes/yaml-writer/Cargo.toml -- --nocapture
cargo fmt --manifest-path spikes/yaml-writer/Cargo.toml --check
cargo clippy --manifest-path spikes/yaml-writer/Cargo.toml --all-targets -- -D warnings
python3 spikes/yaml-writer/run-native.py
scripts/check.sh
```

`run-native.py` temporarily changes the two emission functions to return the
prototype's `Result`, propagates the parse command error, and makes config-dump
serialize/reload every parsed configuration before comparing it with the corpus
golden. It runs local, parse and whole-vendor native goldens and the configuration
corpus golden, then restores the exact original bytes of all production files,
including Cargo.lock, in a `finally` block. Do not run other builds or edit those
files during the experiment. An unsuccessful experiment still restores the files.
A force-killed Python process cannot run its cleanup; check the working diff
before proceeding if the process is killed.

The adapter preserves null omission, order, binary and the old undotted special
float strings. Actual serde output models, production error signatures, property
tests and removal of the old writer belong to milestone 2.
