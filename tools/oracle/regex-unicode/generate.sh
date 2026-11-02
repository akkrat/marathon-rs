#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
JDK=${JAVA_HOME:-"/Applications/Android Studio.app/Contents/jbr/Contents/Home"}
OUT=${1:-"$HERE/../../../spikes/java-regex/src/translate/jdk21-unicode.tsv.gz"}
"$JDK/bin/java" --add-opens java.base/java.util.regex=ALL-UNNAMED --add-opens java.base/java.lang=ALL-UNNAMED "$HERE/RegexUnicode.java" "$OUT"
