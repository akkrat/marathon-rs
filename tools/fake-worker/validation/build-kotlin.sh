#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../.."
export JAVA_HOME=${JAVA_HOME:-"/Applications/Android Studio.app/Contents/jbr/Contents/Home"}
build=$PWD/tools/fake-worker/build
lib=${FW_KOTLIN_LIB:-$PWD/tools/oracle/build/marathon-src/cli/build/install/marathon/lib}
cache=${GRADLE_CACHE:-$HOME/.gradle/caches/modules-2/files-2.1}
mkdir -p "$build/classes" "$build/lib" "$build/compiler"
cp "$lib"/*.jar "$build/lib/"
for spec in \
  "org.jetbrains.kotlin/kotlin-compiler-embeddable/1.9.24 kotlin-compiler-embeddable-1.9.24.jar" \
  "org.jetbrains.kotlin/kotlin-stdlib/1.9.24 kotlin-stdlib-1.9.24.jar" \
  "org.jetbrains.kotlin/kotlin-script-runtime/1.9.24 kotlin-script-runtime-1.9.24.jar" \
  "org.jetbrains.kotlin/kotlin-reflect/1.9.24 kotlin-reflect-1.9.24.jar" \
  "org.jetbrains.kotlin/kotlin-daemon-embeddable/1.9.24 kotlin-daemon-embeddable-1.9.24.jar" \
  "org.jetbrains.intellij.deps/trove4j trove4j-*.jar" \
  "org.jetbrains/annotations/13.0 annotations-13.0.jar"; do
  read -r folder pattern <<< "$spec"
  jar=$(find "$cache/$folder" -name "$pattern" -not -name '*-sources.jar' | head -1)
  test -n "$jar"
  cp "$jar" "$build/compiler/"
done
compiler=$(printf '%s:' "$build"/compiler/*.jar)
libs=$(printf '%s:' "$build"/lib/*.jar)
"$JAVA_HOME/bin/java" -cp "$compiler" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -no-stdlib -no-reflect -jvm-target 17 -nowarn -cp "$libs" -d "$build/classes" tools/fake-worker/validation/Boundary.kt
