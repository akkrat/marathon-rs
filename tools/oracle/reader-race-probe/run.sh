#!/usr/bin/env bash
# U145: Kotlin's produceLinesManually from the read-only source checkout's already-built CLI distribution.
set -euo pipefail
cd "$(dirname "$0")/../../.."
java_home=${JAVA_HOME:-/Applications/Android Studio.app/Contents/jbr/Contents/Home}
kotlin_repo=${MARATHON_KT:-$HOME/src/marathon}
"$java_home/bin/java" -cp "$kotlin_repo/cli/build/install/marathon/lib/*" tools/oracle/reader-race-probe/ReaderRaceProbe.java
