#!/usr/bin/env bash
# The gate for every WP and for main (PLAN.md §8.3). Checks that need the Kotlin repository are skipped,
# with a warning, when it is absent (for example on CI).
set -euo pipefail

export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cd "$(dirname "$0")/.."

MARATHON_KT=${MARATHON_KT:-$HOME/src/marathon}
# Generators inspect the release tree, even when the user's read-only checkout stays at the baseline.
release_source=$(cat docs/release-source.sha)
if [[ -d "$MARATHON_KT" ]]; then
    source_head=$(git -C "$MARATHON_KT" rev-parse HEAD)
    if [[ "$source_head" != "$release_source" ]]; then
        if ! git -C "$MARATHON_KT" cat-file -e "$release_source^{commit}"; then
            echo "error: release source $release_source is unavailable; set MARATHON_KT to a release checkout" >&2
            exit 1
        fi
        release_tree=$(mktemp -d "${TMPDIR:-/tmp}/mrs-release-source.XXXXXX")
        trap 'rm -rf "$release_tree"' EXIT
        git -C "$MARATHON_KT" archive "$release_source" | tar -x -C "$release_tree"
        MARATHON_KT=$release_tree
    fi
fi

step() { printf '\n==> %s\n' "$*"; }

step "optional local checks"
local_check=${MARATHON_LOCAL_CHECK:-$HOME/.config/marathon-rs/local-check.sh}
if [[ -f "$local_check" ]]; then
    bash "$local_check"
else
    echo "none ($local_check not present)"
fi

step "oracle tables (fixtures/oracle)"
# The oracle itself needs the Kotlin repository and a JDK, so only the committed tables are checked: every .jsonl line is a
# JSON object and every .json file is a JSON document.
python3 -I - <<'PY'
import json, pathlib, sys
root = pathlib.Path("fixtures/oracle")
bad = 0
files = sorted(root.rglob("*.jsonl")) + sorted(root.rglob("*.json"))
for f in files:
    try:
        if f.suffix == ".jsonl":
            # split("\n"), not splitlines(): rows hold raw U+2028, U+0085 and the like inside strings.
            text = f.read_text(encoding="utf-8")
            if not text.endswith("\n"):
                raise ValueError("no newline at the end")
            for n, line in enumerate(text[:-1].split("\n"), 1):
                if not isinstance(json.loads(line), dict):
                    raise ValueError(f"line {n} is not a JSON object")
        else:
            json.loads(f.read_text(encoding="utf-8"))
    except ValueError as e:
        print(f"{f}: {e}")
        bad += 1
print(f"{len(files)} files checked")
sys.exit(1 if bad else 0)
PY

step "Apple scenario evidence (offline)"
python3 -I tools/fake-worker/test_apple_evidence.py

step "differential evidence and comparator (offline; native cases run in cargo test below)"
python3 -I tools/differential/run validate
python3 -I tools/differential/test_evidence.py

step "real end-to-end preparation and ownership guards (offline)"
python3 -I tools/real-e2e/run validate
python3 -I tools/real-e2e/test_real_e2e.py
python3 -I tools/real-e2e/test_outcomes.py

step "xcresult schema generation (offline)"
python3 -I scripts/generate_xcresult.py --check
python3 -I scripts/test_generate_xcresult.py

step "release tooling (offline)"
python3 -I scripts/release/test_release.py

step "cargo fmt"
cargo fmt --all --check

step "cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo test"
cargo test --workspace

step "vendored russh client tests (vendor/russh/README.marathon.md)"
CARGO_TARGET_DIR="$PWD/target/vendor-tests" cargo test --manifest-path vendor/russh/Cargo.toml --lib client:: -q

step "plan graph"
python3 -I scripts/check_plan_dag.py

if [[ -d "$MARATHON_KT" ]]; then
    step "source inventory"
    python3 -I scripts/source_inventory.py --check "$MARATHON_KT"
    step "parity matrix"
    python3 -I scripts/parity_matrix.py --check "$MARATHON_KT"
    step "fixtures"
    python3 -I scripts/sync_fixtures.py "$MARATHON_KT"
else
    step "WARNING: Kotlin repository not found at $MARATHON_KT (set MARATHON_KT);" \
        "skipping the source inventory, parity matrix and fixtures checks"
fi

step "all checks passed"
