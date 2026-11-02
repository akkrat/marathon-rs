#!/usr/bin/env bash
# WP 4.1: JDK 21 and the pinned Kotlin 1.9.24 stdlib; source repo stays read-only.
set -euo pipefail
cd "$(dirname "$0")/../../.."
java_home=${JAVA_HOME:-/Applications/Android Studio.app/Contents/jbr/Contents/Home}
stdlib=${KOTLIN_STDLIB:-$HOME/.gradle/caches/modules-2/files-2.1/org.jetbrains.kotlin/kotlin-stdlib/1.9.24/9928532f12c66ad816a625b3f9984f8368ca6d2b/kotlin-stdlib-1.9.24.jar}
mkdir -p target
probe_root=$(mktemp -d target/wp-4.1-probe.XXXXXX)
trap 'rm -rf "$probe_root"' EXIT
"$java_home/bin/java" -cp "$stdlib" tools/oracle/local-transport-probes/LocalTransportProbe.java "$probe_root"
if [[ ${1:-} == --kotlin ]]; then
    kotlin_repo=${MARATHON_KT:-$HOME/src/marathon}
    "$java_home/bin/java" -cp "$kotlin_repo/cli/build/install/marathon/lib/*" tools/oracle/local-transport-probes/KotlinCommandProbe.java
fi
