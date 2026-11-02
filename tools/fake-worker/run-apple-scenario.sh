#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
exec python3 "$ROOT/tools/fake-worker/apple_scenario.py" "$@"
