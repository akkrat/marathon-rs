#!/usr/bin/env bash
# Builds the sshj harness against the Kotlin marathon's own classes (SshjCommandExecutorFactory, SshjCommandExecutor,
# PatchedSSHClient, PerformanceDefaultConfig) and sshj 0.39.0. Nothing is built inside the Kotlin repository: its
# installed distribution's jars are copied into build/lib and the harness is compiled with the Kotlin 1.9.24 compiler
# from the Gradle cache.
set -euo pipefail
cd "$(dirname "$0")"

MARATHON_KT=${MARATHON_KT:-$HOME/src/marathon}
JAVA_HOME=${JAVA_HOME:-"/Applications/Android Studio.app/Contents/jbr/Contents/Home"}
GRADLE_CACHE=${GRADLE_CACHE:-$HOME/.gradle/caches/modules-2/files-2.1}
DIST_LIB=$MARATHON_KT/cli/build/install/marathon/lib

if [[ ! -d $DIST_LIB ]]; then
    echo "error: $DIST_LIB not found; the Kotlin marathon distribution must have been installed" >&2
    exit 1
fi

rm -rf build
mkdir -p build/lib build/classes
cp "$DIST_LIB"/*.jar build/lib/

find_jar() { find "$GRADLE_CACHE/$1" -name "$2" -not -name '*-sources.jar' | head -n 1; }
compiler_cp=""
for spec in \
    "org.jetbrains.kotlin/kotlin-compiler-embeddable/1.9.24 kotlin-compiler-embeddable-1.9.24.jar" \
    "org.jetbrains.kotlin/kotlin-stdlib/1.9.24 kotlin-stdlib-1.9.24.jar" \
    "org.jetbrains.kotlin/kotlin-script-runtime/1.9.24 kotlin-script-runtime-1.9.24.jar" \
    "org.jetbrains.kotlin/kotlin-reflect/1.9.24 kotlin-reflect-1.9.24.jar" \
    "org.jetbrains.kotlin/kotlin-daemon-embeddable/1.9.24 kotlin-daemon-embeddable-1.9.24.jar" \
    "org.jetbrains.intellij.deps/trove4j trove4j-*.jar" \
    "org.jetbrains/annotations/13.0 annotations-13.0.jar"; do
    read -r dir pattern <<<"$spec"
    jar=$(find_jar "$dir" "$pattern")
    if [[ -z $jar ]]; then
        echo "error: $pattern not found under $GRADLE_CACHE/$dir" >&2
        exit 1
    fi
    compiler_cp+="$jar:"
done

lib_cp=$(printf '%s:' build/lib/*.jar)
# The harness calls internal members (SshjCommandExecutorFactory.connect/createClient, shellCommand), as the Kotlin tests do
base_jar=$(ls build/lib/base-*.jar)
"$JAVA_HOME/bin/java" -cp "$compiler_cp" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler \
    -no-stdlib -no-reflect -jvm-target 17 -nowarn -Xfriend-paths="$base_jar" \
    -cp "$lib_cp" -d build/classes src/*.kt
echo "built build/classes"
