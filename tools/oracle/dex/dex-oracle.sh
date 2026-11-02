#!/usr/bin/env bash
# WP 6.4 JVM oracle: runs the pinned dex-test-parser 2.3.4 JAR (Maven SHA-1
# verified) and the reference ApkParser.kt/InstrumentationInfo.kt (compiled
# unchanged from MARATHON_KT at e47352f2) over the fixture APKs, and writes
# fixtures/oracle/dex/{differential,probes}.jsonl. Also (re)builds
# fixtures/dex/probe.apk from probe-src/ with javac + d8, and the manifest /
# dex mutants for the probe rows (copied to fixtures/dex/mutants at the end),
# and fixtures/oracle/dex/string-bytes.txt (StringBytesProbe).
#
# Needs: reference JDK (JAVA_HOME), kotlinc (Android Studio's by default),
# Android SDK build-tools (d8/aapt2), network on first run for the pinned jars.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
MARATHON_KT=${MARATHON_KT:-$HOME/src/marathon}
export JAVA_HOME=${JAVA_HOME:-"/Applications/Android Studio.app/Contents/jbr/Contents/Home"}
KOTLINC=${KOTLINC:-"/Applications/Android Studio.app/Contents/plugins/Kotlin/kotlinc/bin/kotlinc"}
SDK=${ANDROID_HOME:-$HOME/Library/Android/sdk}
BT=$SDK/build-tools/36.1.0
ANDROID_JAR=$SDK/platforms/android-36/android.jar
BUILD=$HERE/../build/dex-oracle
DEPS=$BUILD/deps
mkdir -p "$DEPS" "$BUILD/classes" "$BUILD/mutants" "$ROOT/fixtures/oracle/dex"

fetch() { # path sha1
    local name=$(basename "$1")
    if [[ ! -f "$DEPS/$name" ]]; then curl -fsSL -o "$DEPS/$name" "https://repo1.maven.org/maven2/$1"; fi
    echo "$2  $DEPS/$name" | shasum -a 1 -c - >/dev/null
}
fetch com/linkedin/dextestparser/parser/2.3.4/parser-2.3.4.jar 9279e0742f97d099cd0b07df66ca1045479ff156
fetch com/shazam/axmlparser/1.0/axmlparser-1.0.jar 45f4debaca3fc1cb2c70228a3316f1f2424303d1
fetch org/jetbrains/kotlin/kotlin-stdlib/1.9.24/kotlin-stdlib-1.9.24.jar 9928532f12c66ad816a625b3f9984f8368ca6d2b
fetch org/jetbrains/annotations/13.0/annotations-13.0.jar 919f0dfe192fb4e063e7dacadee7f8bb9a2672a9
fetch com/google/code/gson/gson/2.11.0/gson-2.11.0.jar 527175ca6d81050b53bdd4c457a6d6e017626b0e
fetch com/google/guava/guava/32.0.1-jre/guava-32.0.1-jre.jar 6e5d51a72d142f2d40a57dfb897188b36a95b489
fetch com/github/ajalt/clikt/clikt-jvm/3.1.0/clikt-jvm-3.1.0.jar 32e2bada213627d1af91c05764b328517c7b6511
fetch junit/junit/4.13.2/junit-4.13.2.jar 8ac9e16d933b6fb43bc7f576336b8f4d7eb5ba12
CP=$(ls "$DEPS"/*.jar | paste -sd: -)

ANDROID_KT="$MARATHON_KT/vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android"
"$KOTLINC" -cp "$CP" -d "$BUILD/classes" "$ANDROID_KT/ApkParser.kt" "$ANDROID_KT/InstrumentationInfo.kt" 2>/dev/null
"$JAVA_HOME/bin/javac" -cp "$CP:$BUILD/classes" -d "$BUILD/classes" "$HERE/DexOracle.java" "$HERE/StringBytesProbe.java"
"$JAVA_HOME/bin/java" -cp "$BUILD/classes" StringBytesProbe "$ROOT/fixtures/oracle/dex/string-bytes.txt"

# probe.apk: negative values, supplementary string, nested annotation.
mkdir -p "$BUILD/probe/classes" "$BUILD/probe/dex"
"$JAVA_HOME/bin/javac" -cp "$DEPS/junit-4.13.2.jar" -d "$BUILD/probe/classes" \
    "$HERE/probe-src/probe/ProbeAnnotations.java" "$HERE/probe-src/probe/ProbeTest.java"
(cd "$BUILD/probe/classes" && "$JAVA_HOME/bin/jar" cf "$BUILD/probe/probe.jar" .)
"$BT/d8" --min-api 15 --output "$BUILD/probe/dex" "$BUILD/probe/probe.jar" >/dev/null 2>&1
(cd "$BUILD/probe/dex" && touch -t 198001010000 classes.dex && zip -q -X "$ROOT/fixtures/dex/probe.apk" classes.dex)

# probe-inheritance.apk: array identity through superclass chains.
mkdir -p "$BUILD/probe-inheritance/classes" "$BUILD/probe-inheritance/dex"
"$JAVA_HOME/bin/javac" -cp "$DEPS/junit-4.13.2.jar" -d "$BUILD/probe-inheritance/classes" \
    "$HERE/probe-src/probe/InheritanceProbe.java"
(cd "$BUILD/probe-inheritance/classes" && "$JAVA_HOME/bin/jar" cf "$BUILD/probe-inheritance/probe-inheritance.jar" .)
"$BT/d8" --min-api 15 --output "$BUILD/probe-inheritance/dex" "$BUILD/probe-inheritance/probe-inheritance.jar" >/dev/null 2>&1
rm -f "$ROOT/fixtures/dex/probe-inheritance.apk"
(cd "$BUILD/probe-inheritance/dex" && touch -t 198001010000 classes.dex && zip -q -X "$ROOT/fixtures/dex/probe-inheritance.apk" classes.dex)

# probe-utf8.apk: Utf8ProbeTest with malformed string bytes patched in place.
mkdir -p "$BUILD/probe-utf8/classes" "$BUILD/probe-utf8/dex"
"$JAVA_HOME/bin/javac" -cp "$DEPS/junit-4.13.2.jar" -d "$BUILD/probe-utf8/classes" \
    "$HERE/probe-src/probe/Utf8ProbeTest.java"
(cd "$BUILD/probe-utf8/classes" && "$JAVA_HOME/bin/jar" cf "$BUILD/probe-utf8/probe-utf8.jar" .)
"$BT/d8" --min-api 15 --output "$BUILD/probe-utf8/dex" "$BUILD/probe-utf8/probe-utf8.jar" >/dev/null 2>&1
rm -f "$BUILD/probe-utf8/base.apk"
(cd "$BUILD/probe-utf8/dex" && touch -t 198001010000 classes.dex && zip -q -X "$BUILD/probe-utf8/base.apk" classes.dex)

# mutants (generated in the build dir, then copied to fixtures/dex/mutants)
python3 -I "$HERE/mutate_dex.py" "$BUILD/probe-utf8/base.apk" "$BUILD/mutants/probe-utf8.apk" utf8
python3 -I "$HERE/zip_descriptor.py" "$ROOT/fixtures/dex/probe.apk" "$BUILD/mutants/probe-stored-descriptor.apk" stored
python3 -I "$HERE/zip_descriptor.py" "$ROOT/fixtures/dex/probe.apk" "$BUILD/mutants/probe-deflated-descriptor.apk" deflated
python3 -I "$HERE/mutate_dex.py" "$ROOT/fixtures/dex/probe.apk" "$BUILD/mutants/probe-dex040.apk" magic040
python3 -I "$HERE/mutate_dex.py" "$ROOT/fixtures/dex/probe.apk" "$BUILD/mutants/probe-superclass-self.apk" superclass-self
python3 -I "$HERE/mutate_dex.py" "$ROOT/fixtures/dex/probe.apk" "$BUILD/mutants/probe-superclass-noindex.apk" superclass-noindex

manifest_apk() { # name, instrumentation attrs (may be empty), package attr (may be empty)
    local dir="$BUILD/mutants/$1-src" ; mkdir -p "$dir"
    cat > "$dir/AndroidManifest.xml" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android" $3>
    <application/>
    <instrumentation $2 />
</manifest>
EOF
    "$BT/aapt2" link -o "$BUILD/mutants/$1.apk" -I "$ANDROID_JAR" --manifest "$dir/AndroidManifest.xml" \
        --min-sdk-version 15 --target-sdk-version 26 >/dev/null 2>&1
}
PKG='package="com.example.test"'
manifest_apk manifest-relative-dot 'android:name=".MyRunner" android:targetPackage="com.example"' "$PKG"
manifest_apk manifest-relative-bare 'android:name="MyRunner" android:targetPackage="com.example"' "$PKG"
printf 'this is not a zip file at all' > "$BUILD/mutants/notzip.apk"
manifest_apk manifest-missing-target 'android:name="com.example.Runner"' "$PKG"
# aapt2 will not build these; build_axml.py writes the binary XML directly.
python3 -I "$HERE/build_axml.py" "$BUILD/mutants/manifest-missing-runner.apk" missing-runner
python3 -I "$HERE/build_axml.py" "$BUILD/mutants/manifest-missing-package.apk" missing-package
python3 -I "$HERE/build_axml.py" "$BUILD/mutants/manifest-missing-name-and-package.apk" missing-name-and-package
python3 -I "$HERE/build_axml.py" "$BUILD/mutants/manifest-unpooled-package.apk" unpooled-package
for mode in surrogate-high-package surrogate-low-runner surrogate-pair-runner; do
    python3 -I "$HERE/build_axml.py" "$BUILD/mutants/manifest-$mode.apk" "$mode"
done

run() { "$JAVA_HOME/bin/java" -cp "$CP:$BUILD/classes" DexOracle "$@"; }
F="$ROOT/fixtures"
rm -f "$F/oracle/dex/differential.jsonl" "$F/oracle/dex/probes.jsonl"
run "$F/oracle/dex/differential.jsonl" \
    "$F/vendor-android/android_test_1.apk" \
    "$F/vendor-android/apk/app-debug-androidTest.apk" \
    "$F/vendor-android/apk/app-debug.apk" \
    "$F/vendor-android/apk/standalone-hdpi.apk" \
    "$F/vendor-android/apk/base-en.apk" \
    "$F/dex/abstract-class-in-second-dex.apk" \
    "$F/dex/abstract-class-in-second-dex.apk:customAnnotations=" \
    "$F/dex/test-app-debug-androidTest.apk:customAnnotations=" \
    "$F/dex/test-app-debug-androidTest.apk"
run "$F/oracle/dex/probes.jsonl" \
    "$F/dex/probe.apk" \
    "$F/dex/probe-inheritance.apk" \
    "$BUILD/mutants/probe-dex040.apk" \
    "$BUILD/mutants/probe-superclass-self.apk" \
    "$BUILD/mutants/probe-superclass-noindex.apk" \
    "$BUILD/mutants/manifest-relative-dot.apk" \
    "$BUILD/mutants/manifest-relative-bare.apk" \
    "$BUILD/mutants/manifest-missing-runner.apk" \
    "$BUILD/mutants/manifest-missing-target.apk" \
    "$BUILD/mutants/manifest-missing-package.apk" \
    "$BUILD/mutants/manifest-missing-name-and-package.apk" \
    "$BUILD/mutants/manifest-unpooled-package.apk" \
    "$BUILD/mutants/notzip.apk" \
    "$BUILD/mutants/probe-utf8.apk" \
    "$BUILD/mutants/probe-stored-descriptor.apk" \
    "$BUILD/mutants/probe-deflated-descriptor.apk" \
    "$BUILD/mutants/manifest-surrogate-high-package.apk" \
    "$BUILD/mutants/manifest-surrogate-low-runner.apk" \
    "$BUILD/mutants/manifest-surrogate-pair-runner.apk"
# relative path, so the recorded FileNotFoundException message is portable
(cd "$BUILD/mutants" && run "$F/oracle/dex/probes.jsonl" "no-such-file.apk")
# The Rust tests run against the committed mutant copies.
mkdir -p "$ROOT/fixtures/dex/mutants"
cp "$BUILD/mutants/"*.apk "$ROOT/fixtures/dex/mutants/"
echo "wrote fixtures/oracle/dex/differential.jsonl and probes.jsonl"
