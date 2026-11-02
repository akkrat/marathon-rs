#!/usr/bin/env bash
# Builds the Kotlin reference distribution and the oracle tools (PLAN.md §6.3, docs/wp/0.2a.md).
#
# The Kotlin repository is never built in place: it is cloned into tools/oracle/build/marathon-src (gitignored), checked out
# at the frozen commit and built in two variants: reference (0001 only), patched (all patches).
# Running the script again reuses both clones and skips patches already applied. Usage: build.sh [all|reference|patched].
#
# Environment:
#   MARATHON_KT         the Kotlin repository to clone (default ~/src/marathon)
#   JAVA_HOME           the reference JDK (default: Android Studio's JBR, OpenJDK 21.0.10)
#   ORACLE_GRADLE_FLAGS extra Gradle flags, for example --offline
set -euo pipefail

ORACLE_DIR=$(cd "$(dirname "$0")" && pwd)
MARATHON_KT=${MARATHON_KT:-$HOME/src/marathon}
export JAVA_HOME=${JAVA_HOME:-"/Applications/Android Studio.app/Contents/jbr/Contents/Home"}
COMMIT=${ORACLE_SOURCE_SHA:-e47352f25cbc50333900d5d040a5544ff1224501}
case "$COMMIT" in
    e47352f25cbc50333900d5d040a5544ff1224501) BUILD="$ORACLE_DIR/build" ;;
    c95b212882c7ba7b1ad91b7981916ed175f1530e) BUILD="$ORACLE_DIR/build-$COMMIT" ;;
    *) echo "error: unsupported ORACLE_SOURCE_SHA=$COMMIT" >&2; exit 2 ;;
esac
VARIANT=${1:-all}
case "$VARIANT" in
    all) "$0" reference; "$0" patched; exit ;;
    reference) SRC="$BUILD/marathon-src"; PATCHES=("$ORACLE_DIR/patches/0001-oracle-module.patch") ;;
    patched) SRC="$BUILD/marathon-patched"; PATCHES=("$ORACLE_DIR"/patches/*.patch) ;;
    *) echo "usage: $0 [all|reference|patched]" >&2; exit 2 ;;
esac

step() { printf '\n==> %s\n' "$*"; }

# The receipt of this variant is invalidated before anything that can fail (tool check, clone, checkout, patches, tree
# check, Gradle): a rejected or failed rebuild must never leave the previous build's receipt usable.
rm -f "$BUILD/receipt-$VARIANT.json"

if [[ ! -x "$JAVA_HOME/bin/java" ]]; then
    echo "error: no JDK at JAVA_HOME=$JAVA_HOME" >&2
    exit 1
fi
"$JAVA_HOME/bin/java" -version

mkdir -p "$BUILD"
if [[ ! -d "$SRC/.git" ]]; then
    step "cloning $MARATHON_KT"
    git clone --quiet --no-hardlinks "$MARATHON_KT" "$SRC"
fi

if [[ $(git -C "$SRC" rev-parse HEAD) != "$COMMIT" ]]; then
    if ! git -C "$SRC" cat-file -e "$COMMIT^{commit}" 2>/dev/null; then
        git -C "$SRC" fetch --quiet "$MARATHON_KT"
    fi
    if [[ -n $(git -C "$SRC" status --porcelain --untracked-files=no) ]]; then
        echo "error: $SRC has local changes and is not at $COMMIT; delete it and run again" >&2
        exit 1
    fi
    step "checking out $COMMIT"
    git -C "$SRC" checkout --quiet --detach "$COMMIT"
fi

for patch in "${PATCHES[@]}"; do
    if git -C "$SRC" apply --reverse --check "$patch" 2>/dev/null; then
        echo "already applied: $(basename "$patch")"
    else
        step "applying $(basename "$patch")"
        git -C "$SRC" apply "$patch"
    fi
done

# Refuse to build anything but the pinned tree plus the oracle patches: compare the tree of the working directory with the
# pinned commit's tree patched in a scratch index. Every file counts, ignored or not (Gradle compiles ignored files under a
# source directory too), except the build outputs in BUILD_OUTPUTS.
BUILD_OUTPUTS=(
    .gradle build oracle-build oracle-hooks-build buildSrc/.gradle buildSrc/build
    analytics/usage/build cli/build configuration/build core/build report/execution-timeline/build report/html-report/build
    vendor/vendor-android/build vendor/vendor-apple/base/build vendor/vendor-apple/ios/build vendor/vendor-apple/macos/build
    vendor/vendor-test/build
)
excludes=()
for d in "${BUILD_OUTPUTS[@]}"; do excludes+=(":(exclude,top)$d"); done
step "verifying the clone against $COMMIT + patches"
expected_index="$BUILD/expected.index"
actual_index="$BUILD/actual.index"
rm -f "$expected_index" "$actual_index"
GIT_INDEX_FILE="$expected_index" git -C "$SRC" read-tree "$COMMIT"
for patch in "${PATCHES[@]}"; do
    GIT_INDEX_FILE="$expected_index" git -C "$SRC" apply --cached "$patch"
done
expected_tree=$(GIT_INDEX_FILE="$expected_index" git -C "$SRC" write-tree)
GIT_INDEX_FILE="$actual_index" git -C "$SRC" read-tree "$COMMIT"
GIT_INDEX_FILE="$actual_index" git -C "$SRC" add --all --force -- . "${excludes[@]}"
actual_tree=$(GIT_INDEX_FILE="$actual_index" git -C "$SRC" write-tree)
rm -f "$expected_index" "$actual_index"
if [[ $(git -C "$SRC" rev-parse HEAD) != "$COMMIT" || $actual_tree != "$expected_tree" ]]; then
    echo "error: $SRC differs from $COMMIT plus tools/oracle/patches:" >&2
    git -C "$SRC" diff --stat "$expected_tree" "$actual_tree" >&2
    echo "delete $SRC and run again" >&2
    exit 1
fi

step "gradle :cli:installDist :oracle:installDist"
# shellcheck disable=SC2086 # ORACLE_GRADLE_FLAGS is a list of flags
(cd "$SRC" && ./gradlew --console=plain ${ORACLE_GRADLE_FLAGS:-} :cli:installDist :oracle:installDist)

step "smoke test: marathon version"
"$SRC/cli/build/install/marathon/bin/marathon" version

python3 -I "$ORACLE_DIR/build-receipt.py" --variant "$VARIANT" --source "$COMMIT" --src "$SRC" --verified-tree "$expected_tree" \
    --java "$("$JAVA_HOME/bin/java" -version 2>&1 | head -1)" --out "$BUILD/receipt-$VARIANT.json" "${PATCHES[@]}"

echo
echo "reference distribution: $SRC/cli/build/install/marathon/bin/marathon"
echo "oracle tools:           $SRC/oracle-build/install/oracle/bin/oracle"
