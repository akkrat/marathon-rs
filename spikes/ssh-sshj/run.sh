#!/usr/bin/env bash
# Runs one harness case: run.sh <case> [key=value ...]. See src/Harness.kt for the cases and the keys.
set -euo pipefail
cd "$(dirname "$0")"
JAVA_HOME=${JAVA_HOME:-"/Applications/Android Studio.app/Contents/jbr/Contents/Home"}
cp="build/classes:$(printf '%s:' build/lib/*.jar)"
exec "$JAVA_HOME/bin/java" -Dlogback.configurationFile=logback-spike.xml -cp "$cp" HarnessKt "$@"
