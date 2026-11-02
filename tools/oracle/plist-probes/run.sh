#!/usr/bin/env bash
# WP 4.5a JVM probes: compiles PlistProbes.java against the Kotlin marathon distribution jars
# (dd-plist 1.28 and the compiled plist models of e47352f2) and prints the probe output.
# Usage: tools/oracle/plist-probes/run.sh [output-file]
set -euo pipefail

JH=${JAVA_HOME:-/Applications/Android Studio.app/Contents/jbr/Contents/Home}
MARATHON_KT=${MARATHON_KT:-$HOME/src/marathon}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
LIB="$MARATHON_KT/cli/build/install/marathon/lib"
FIXTURES="$ROOT/fixtures/vendor-apple-base/fixtures/xctestrun"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
"$JH/bin/javac" -cp "$LIB/*" -d "$WORK" "$HERE/PlistProbes.java"
if [[ $# -gt 0 ]]; then
    "$JH/bin/java" -cp "$WORK:$LIB/*" PlistProbes "$FIXTURES" > "$1" 2>"$WORK/stderr.txt" || { cat "$WORK/stderr.txt" >&2; exit 1; }
    echo "wrote $1"
else
    "$JH/bin/java" -cp "$WORK:$LIB/*" PlistProbes "$FIXTURES"
fi
