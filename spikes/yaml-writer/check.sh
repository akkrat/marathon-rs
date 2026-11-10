#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo fmt --manifest-path spikes/yaml-writer/Cargo.toml --check
cargo clippy --manifest-path spikes/yaml-writer/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path spikes/yaml-writer/Cargo.toml -- --nocapture
