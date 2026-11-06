#!/usr/bin/env bash
# D26: writes fixtures/dex/versions/ with real d8 output of one set of sources in each DEX version the parser accepts
# since D26, for crates/marathon-dex/tests/dex_versions.rs. d8 emits 039 at most by itself: 040 and 041 are behind the
# two system properties below, so it runs from its jar. Its help names neither, and it warns that it forces the
# container format on an API level that does not support it.
#
# Local tools: Android SDK build-tools 37.0.0 (D8 9.2.4-dev), platform android-36 and Android Studio's JBR. Network on
# first run for the junit jar, which javac needs and the APKs do not contain.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
export JAVA_HOME=${JAVA_HOME:-"/Applications/Android Studio.app/Contents/jbr/Contents/Home"}
SDK=${ANDROID_HOME:-$HOME/Library/Android/sdk}
D8JAR=$SDK/build-tools/37.0.0/lib/d8.jar
ANDROID_JAR=$SDK/platforms/android-36/android.jar
BUILD=${BUILD:-$HERE/../build/dex-versions}
OUT=$ROOT/fixtures/dex/versions
JUNIT=$BUILD/junit-4.13.2.jar
JUNIT_SHA256=8e495b634469d64fb8acfa3495a065cbacc8a0fff55ce1e31007be4c16dc57d3

rm -rf "${BUILD:?}/classes" "${BUILD:?}/dex"
mkdir -p "$BUILD/classes" "$BUILD/dex" "$OUT"
[[ -f "$JUNIT" ]] || curl -fsSL -o "$JUNIT" https://repo1.maven.org/maven2/junit/junit/4.13.2/junit-4.13.2.jar
echo "$JUNIT_SHA256  $JUNIT" | shasum -a 256 -c - >/dev/null
"$JAVA_HOME/bin/javac" --release 11 -cp "$JUNIT" -d "$BUILD/classes" \
    "$HERE/probe-src/probe/ProbeAnnotations.java" "$HERE/probe-src/probe/ProbeTest.java" \
    "$HERE/probe-src/probe/InheritanceProbe.java"
(cd "$BUILD/classes" && "$JAVA_HOME/bin/jar" cf "$BUILD/probe.jar" .)

# build <apk name> <java system property or -> <d8 arguments>
build() {
    local name=$1 property=$2
    shift 2
    local dir=$BUILD/dex/$name
    mkdir -p "$dir"
    local java=("$JAVA_HOME/bin/java")
    [[ $property == - ]] || java+=("-D$property=1")
    "${java[@]}" -cp "$D8JAR" com.android.tools.r8.D8 --no-desugaring --lib "$ANDROID_JAR" --output "$dir" "$@" "$BUILD/probe.jar"
    rm -f "${OUT:?}/${name:?}.apk"
    (cd "$dir" && touch -t 198001010000 classes.dex && zip -q -X "$OUT/$name.apk" classes.dex)
}
build dex039 - --min-api 30
build dex040 com.android.tools.r8.dexVersion40ForApiLevel30 --min-api 30
build dex041 com.android.tools.r8.dexContainerExperiment --min-api 36
# Main-dex rules split the classes, so that the container holds two images. They need a min API below 22.
printf -- '-keep class probe.ProbeTest { *; }\n' > "$BUILD/main-dex-rules.txt"
build dex041-two-images com.android.tools.r8.dexContainerExperiment --min-api 20 --main-dex-rules "$BUILD/main-dex-rules.txt"
shasum -a 256 "$OUT"/*.apk
