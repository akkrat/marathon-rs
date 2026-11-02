#!/usr/bin/env bash
# Build dex-test-parser 2.3.4 test-app androidTest APK without Gradle/AGP.
# This is the recipe that produced fixtures/dex/test-app-debug-androidTest.apk
# on 2026-10-24 (see fixtures/PROVENANCE.md). Rerunning it reproduces
# classes.dex and AndroidManifest.xml byte for byte; the APK itself differs
# only in zip timestamps.
#
# Inputs, all fetched by URL and checked against the SHA-256 below on first
# run: the dex-test-parser tag 2.3.4 source tarball (test-app/), junit 4.13.2,
# hamcrest-core 1.3 and kotlin-stdlib 1.9.24 from Maven Central,
# support-annotations 28.0.0 and the support test runner 1.0.2 AAR (its
# classes.jar) from Google's Maven. Local tools: Android SDK platform
# android-36 + build-tools 36.1.0, kotlinc-jvm 2.3.10 (Android Studio),
# JBR 21.0.10. The result was validated with the pinned parser JAR:
# DexParser.findTestMethods returns the 23 test methods DexParserShould
# expects (fixtures/oracle/dex/differential.jsonl).
#
# Output: $B/apk/test-app-debug-androidTest.apk (B defaults to the gitignored
# tools/oracle/build/test-app).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
B=${B:-$HERE/../build/test-app}
DEPS=$B/deps
SDK=${ANDROID_HOME:-$HOME/Library/Android/sdk}
JBR=${JAVA_HOME:-"/Applications/Android Studio.app/Contents/jbr/Contents/Home"}
KOTLINC=${KOTLINC:-"/Applications/Android Studio.app/Contents/plugins/Kotlin/kotlinc/bin/kotlinc"}
ANDROID_JAR=$SDK/platforms/android-36/android.jar
export JAVA_HOME="$JBR"
mkdir -p "$DEPS"

fetch() { # url sha256 [file name]
    local name=${3:-$(basename "$1")}
    if [[ ! -f "$DEPS/$name" ]]; then curl -fsSL -o "$DEPS/$name" "$1"; fi
    echo "$2  $DEPS/$name" | shasum -a 256 -c - >/dev/null
}
MAVEN=https://repo1.maven.org/maven2
GOOGLE=https://dl.google.com/dl/android/maven2
fetch https://github.com/linkedin/dex-test-parser/archive/refs/tags/2.3.4.tar.gz \
    5023bddd8fadaa647b5fd329a9f6bdc0eebf91a0152686c3ba048bd33b78dd63 dex-test-parser-2.3.4.tar.gz
fetch $MAVEN/junit/junit/4.13.2/junit-4.13.2.jar \
    8e495b634469d64fb8acfa3495a065cbacc8a0fff55ce1e31007be4c16dc57d3
fetch $MAVEN/org/hamcrest/hamcrest-core/1.3/hamcrest-core-1.3.jar \
    66fdef91e9739348df7a096aa384a5685f4e875584cce89386a7a47251c4d8e9
fetch $MAVEN/org/jetbrains/kotlin/kotlin-stdlib/1.9.24/kotlin-stdlib-1.9.24.jar \
    858b902696da9cf585ab9d98ffc1c2712269828354dfe9107e3711b084a36468
fetch $GOOGLE/com/android/support/support-annotations/28.0.0/support-annotations-28.0.0.jar \
    5d5b9414f02d3fa0ee7526b8d5ddae0da67c8ecc8c4d63ffa6cf91488a93b927
fetch $GOOGLE/com/android/support/test/runner/1.0.2/runner-1.0.2.aar \
    f04b9ae342975ba1cb3e4a06e13426e3e6b8a73faa45acba604493d83c9a4f00
unzip -p "$DEPS/runner-1.0.2.aar" classes.jar > "$DEPS/runner-classes.jar"
echo "94d07f13ffd048bfaecfbf16b9a712bd4135dec71c24b4ffd166acd65402eadd  $DEPS/runner-classes.jar" | shasum -a 256 -c - >/dev/null

rm -rf "$B/src" && mkdir -p "$B/src"
tar -xzf "$DEPS/dex-test-parser-2.3.4.tar.gz" -C "$B/src" --strip-components 1
SRC=$B/src/test-app/src
JUNIT=$DEPS/junit-4.13.2.jar
HAMCREST=$DEPS/hamcrest-core-1.3.jar
KSTDLIB=$DEPS/kotlin-stdlib-1.9.24.jar
OPTIONAL=$SDK/platforms/android-36/optional
CP=$ANDROID_JAR:$OPTIONAL/android.test.base.jar:$OPTIONAL/android.test.runner.jar:$JUNIT:$HAMCREST
CP=$CP:$DEPS/support-annotations-28.0.0.jar:$DEPS/runner-classes.jar:$KSTDLIB

rm -rf "$B/classes" "$B/dex" "$B/apk" && mkdir -p "$B/classes" "$B/dex" "$B/apk"
find "$SRC/androidTest" -name '*.kt' > "$B/kt.txt"
find "$SRC/androidTest" -name '*.java' > "$B/java.txt"
# 1. Kotlin (Java sources passed for symbol resolution; kotlinc emits no Java classes)
"$KOTLINC" -cp "$CP" -d "$B/classes" $(cat "$B/kt.txt" "$B/java.txt") 2>/dev/null
# 2. Java
"$JBR/bin/javac" -source 8 -target 8 -cp "$CP:$B/classes" -d "$B/classes" @"$B/java.txt" 2>&1 \
    | grep -v "bootstrap\|deprecat" || true
# 3. Dex: test classes + kotlin-stdlib + junit (as AGP would include androidTest deps)
(cd "$B/classes" && "$JBR/bin/jar" cf "$B/test-classes.jar" .)
"$SDK/build-tools/36.1.0/d8" --min-api 15 --lib "$ANDROID_JAR" --output "$B/dex" "$B/test-classes.jar" \
    "$KSTDLIB" "$JUNIT" "$HAMCREST" "$DEPS/runner-classes.jar" 2>&1 | tail -3
# 4. Package with binary manifest
"$SDK/build-tools/36.1.0/aapt2" link -o "$B/apk/base.apk" -I "$ANDROID_JAR" --manifest "$SRC/main/AndroidManifest.xml" \
    --min-sdk-version 15 --target-sdk-version 26 2>&1 | tail -2
cp "$B/apk/base.apk" "$B/apk/test-app-debug-androidTest.apk" && cp "$B/dex/classes.dex" "$B/apk/"
(cd "$B/apk" && zip -q test-app-debug-androidTest.apk classes.dex)
unzip -l "$B/apk/test-app-debug-androidTest.apk"
