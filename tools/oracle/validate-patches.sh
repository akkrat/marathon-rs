#!/usr/bin/env bash
# Default: both builds, all Kotlin unit tests, 10 exact replays, reference comparisons.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
export JAVA_HOME=${JAVA_HOME:-"/Applications/Android Studio.app/Contents/jbr/Contents/Home"}
cd "$ROOT"
exec python3 tools/oracle/validate_patches.py "$@"
